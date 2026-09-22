"""Bounded cleanup for this CI run's children, including reparented PTY sessions.

Darwin only; no ps display parsing, process-name matching, sudo or group-wide kill.
The selected fixtures inherit a unique environment marker. This is containment
for those audited children, not a sandbox for arbitrary or hostile programs.
ABI references: Apple xnu bsd/sys/proc_info.h, bsd/sys/sysctl.h and
libsyscall/wrappers/libproc/libproc.h; argument layout: adv_cmds/ps/print.c.
"""

import ctypes as c
import errno
import json
import os
from pathlib import Path
import secrets
import signal
import sys
import time


MARKER = "AMS_MODULAR_CI_RUN"


class ProcInfo(c.Structure):
    _fields_ = (
        [(name, c.c_uint32) for name in (
            "flags", "status", "xstatus", "pid", "ppid", "uid", "gid",
            "ruid", "rgid", "svuid", "svgid", "reserved",
        )]
        + [("comm", c.c_char * 16), ("name", c.c_char * 32)]
        + [(name, c.c_uint32) for name in (
            "nfiles", "pgid", "jobc", "tdev", "tpgid",
        )]
        + [("nice", c.c_int32), ("start_sec", c.c_uint64), ("start_usec", c.c_uint64)]
    )

    def identity(self):
        return (self.pid, self.uid, self.start_sec, self.start_usec)


def write_json(path, value):
    path = Path(path)
    temporary = path.with_suffix(".new")
    temporary.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


class DarwinProcesses:
    def __init__(self):
        if sys.platform != "darwin" or c.sizeof(ProcInfo) != 136:
            raise RuntimeError("this runner requires the 64-bit Darwin proc_bsdinfo ABI")
        self.lib = c.CDLL("/usr/lib/libproc.dylib", use_errno=True)
        self.lib.proc_listpids.argtypes = [c.c_uint32, c.c_uint32, c.c_void_p, c.c_int]
        self.lib.proc_listpids.restype = c.c_int
        self.lib.proc_pidinfo.argtypes = [c.c_int, c.c_int, c.c_uint64, c.c_void_p, c.c_int]
        self.lib.proc_pidinfo.restype = c.c_int
        self.system = c.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
        self.system.sysctl.argtypes = [
            c.POINTER(c.c_int), c.c_uint, c.c_void_p,
            c.POINTER(c.c_size_t), c.c_void_p, c.c_size_t,
        ]
        self.system.sysctl.restype = c.c_int
        argmax = c.c_int()
        self._sysctl([1, 8], argmax)  # CTL_KERN, KERN_ARGMAX
        self.argmax = argmax.value
        if not 4096 <= self.argmax <= 16 * 1024 * 1024:
            raise RuntimeError("unexpected Darwin KERN_ARGMAX")
        me = self.info(os.getpid())
        if me is None or me.uid != os.getuid() or me.ppid != os.getppid() or not me.start_sec:
            raise RuntimeError("Darwin process identity preflight failed")
        # Verify the raw argument/environment parser against an unchanged launch value.
        if self.environment(os.getpid()).get(b"PATH") != os.fsencode(os.environ["PATH"]):
            raise RuntimeError("Darwin environment preflight failed")

    def _sysctl(self, numbers, buffer):
        mib = (c.c_int * len(numbers))(*numbers)
        length = c.c_size_t(c.sizeof(buffer))
        if self.system.sysctl(mib, len(numbers), c.byref(buffer), c.byref(length), None, 0):
            error = c.get_errno()
            raise OSError(error, os.strerror(error))
        return length.value

    def info(self, pid):
        value = ProcInfo()
        c.set_errno(0)
        length = self.lib.proc_pidinfo(pid, 3, 0, c.byref(value), c.sizeof(value))
        if length == c.sizeof(value) and value.pid == pid:
            return value
        error = c.get_errno()
        if error == errno.ESRCH:
            return None
        # Zero/short reads without ESRCH are not proof that a process vanished.
        raise OSError(error or errno.EIO, f"proc_pidinfo incomplete for PID {pid}")

    def snapshot(self):
        # Over-allocate to cover arrivals between the sizing and fill calls.
        # A full buffer is a failed scan, never an empty/complete inventory.
        needed = self.lib.proc_listpids(4, os.getuid(), None, 0)  # PROC_UID_ONLY
        if needed <= 0 or needed > 4 * 65536:
            raise RuntimeError("invalid Darwin process inventory size")
        slots = min(65536, needed // c.sizeof(c.c_int) + 4096)
        buffer = (c.c_int * slots)()
        count = self.lib.proc_listpids(4, os.getuid(), buffer, c.sizeof(buffer))
        if count <= 0 or count >= c.sizeof(buffer) or count % c.sizeof(c.c_int):
            raise RuntimeError("incomplete Darwin process inventory")
        found = {}
        for pid in buffer[:count // c.sizeof(c.c_int)]:
            if pid <= 0:
                continue
            info = self.info(pid)
            if info is not None and info.uid == os.getuid():
                found[pid] = info
        return found

    def environment(self, pid):
        buffer = c.create_string_buffer(self.argmax)
        length = self._sysctl([1, 49, pid], buffer)  # CTL_KERN, KERN_PROCARGS2
        raw = buffer.raw[:length]
        if len(raw) < c.sizeof(c.c_int):
            raise ValueError("short Darwin process arguments")
        nargs = int.from_bytes(raw[:4], sys.byteorder, signed=True)
        if not 1 <= nargs <= len(raw):
            raise ValueError("invalid Darwin argc")
        cursor = raw.index(b"\0", 4) + 1  # executable path
        while cursor < len(raw) and raw[cursor] == 0:
            cursor += 1
        for _ in range(nargs):
            cursor = raw.index(b"\0", cursor) + 1
        result = {}
        while cursor < len(raw) and raw[cursor] != 0:
            end = raw.index(b"\0", cursor)
            entry = raw[cursor:end]
            key, separator, value = entry.partition(b"=")
            if not separator or not key:
                raise ValueError("invalid Darwin environment entry")
            result[key] = value
            cursor = end + 1
        if cursor >= len(raw):
            raise ValueError("unterminated Darwin environment")
        return result


class ProcessGuard:
    def __init__(self, journal, restore=False):
        self.api = DarwinProcesses()
        self.journal = Path(journal)
        self.owned = set()
        self.uncertain = []
        self.signals = []
        if restore:
            state = json.loads(self.journal.read_text(encoding="utf-8"))
            if state["uid"] != os.getuid():
                raise RuntimeError("cleanup journal belongs to another user")
            self.token = state["token"]
            self.baseline = {tuple(item) for item in state["baseline"]}
            self.owned = {tuple(item) for item in state["owned"]}
        else:
            self.token = secrets.token_hex(24)
            self.baseline = {p.identity() for p in self.api.snapshot().values()}
        self.save()

    def save(self):
        # Private control file, excluded from uploaded artifacts. Raw environments
        # and command arguments are never logged or persisted.
        write_json(self.journal, {
            "uid": os.getuid(), "token": self.token,
            "baseline": sorted(self.baseline), "owned": sorted(self.owned),
        })

    def register(self, pid):
        value = self.api.info(pid)
        if value is not None:
            if value.uid != os.getuid():
                raise RuntimeError("child changed UID")
            self.owned.add(value.identity())
            self.save()

    def qualify_shell_tools(self, environment):
        """Check exact native-tool visibility before allowing any PTY tests.

        Darwin START_SUSPENDED stops each image before it runs user code. This
        makes even stty observable without racing its short normal lifetime.
        No command is resumed; each exact child is SIGKILLed and reaped.
        """
        system = self.api.system
        pointer = c.POINTER(c.c_void_p)
        for name in ("posix_spawnattr_init", "posix_spawnattr_destroy"):
            getattr(system, name).argtypes = [pointer]
            getattr(system, name).restype = c.c_int
        system.posix_spawnattr_setflags.argtypes = [pointer, c.c_short]
        system.posix_spawnattr_setflags.restype = c.c_int
        system.posix_spawn.argtypes = [
            c.POINTER(c.c_int), c.c_char_p, pointer, pointer,
            c.POINTER(c.c_char_p), c.POINTER(c.c_char_p),
        ]
        system.posix_spawn.restype = c.c_int
        entries = [os.fsencode(key + "=" + value) for key, value in environment.items()]
        envp = (c.c_char_p * (len(entries) + 1))(*entries, None)
        observed = []
        for executable in ("/bin/sh", "/bin/cat", "/bin/sleep", "/bin/stty"):
            attr = c.c_void_p()
            pid = c.c_int()
            error = system.posix_spawnattr_init(c.byref(attr))
            if error:
                raise OSError(error, "posix_spawnattr_init")
            try:
                # START_SUSPENDED | CLOEXEC_DEFAULT, from Darwin sys/spawn.h.
                error = system.posix_spawnattr_setflags(c.byref(attr), 0x0080 | 0x4000)
                if error:
                    raise OSError(error, "posix_spawnattr_setflags")
                argv = (c.c_char_p * 2)(os.fsencode(executable), None)
                error = system.posix_spawn(c.byref(pid), os.fsencode(executable), None,
                                           c.byref(attr), argv, envp)
                if error:
                    raise OSError(error, "posix_spawn visibility probe")
                self.register(pid.value)
                actual = self.api.environment(pid.value)
                for key in (MARKER, "HOME", "SHELL", "PATH"):
                    if actual.get(key.encode()) != os.fsencode(environment[key]):
                        raise RuntimeError(f"cannot verify inherited {key} for {executable}")
                observed.append(executable)
            finally:
                system.posix_spawnattr_destroy(c.byref(attr))
                if pid.value:
                    # This direct child has never been waited on, so its PID
                    # cannot be reused. Even an ABI/registration failure must
                    # not strand a stopped child. Orphans elsewhere require the
                    # full UID/start identity check in signal_owned().
                    try:
                        os.kill(pid.value, signal.SIGKILL)
                    except ProcessLookupError:
                        pass

                    def reap():
                        try:
                            os.waitpid(pid.value, os.WNOHANG)
                        except ChildProcessError:
                            pass

                    cleanup = self.clean(reap)
                    if not cleanup["clean"]:
                        raise RuntimeError("visibility probe cleanup could not be verified")
        return observed

    def scan(self):
        current = self.api.snapshot()
        alive = {p.identity() for p in current.values()}
        self.owned.intersection_update(alive)
        uncertain = []
        # Read new same-UID identities to find children that already setsid()'d
        # or reparented. Audited shells and tools inherit this launch marker.
        for pid, info in current.items():
            identity = info.identity()
            if pid == os.getpid() or identity in self.baseline or identity in self.owned:
                continue
            try:
                environment = self.api.environment(pid)
                # Darwin may return argv successfully while withholding env for
                # a restricted binary. Empty/partial env is not a negative match.
                if not environment.get(b"PATH") or not environment.get(b"HOME"):
                    raise ValueError("process environment is empty or incomplete")
                marked = environment.get(MARKER.encode()) == self.token.encode()
            except (OSError, ValueError):
                marked = False
                uncertain.append(identity)
            latest = self.api.info(pid)
            if latest is None or latest.identity() != identity:
                continue
            if marked:
                self.owned.add(identity)
        # An observed child remains owned even if it later clears its marker.
        changed = True
        while changed:
            changed = False
            for info in current.values():
                parent = current.get(info.ppid)
                identity = info.identity()
                if parent is not None and parent.identity() in self.owned and identity not in self.owned:
                    self.owned.add(identity)
                    changed = True
        self.uncertain = [item for item in uncertain if item not in self.owned and item in alive]
        self.save()
        return current

    def signal_owned(self, sig):
        for identity in sorted(self.owned):
            pid, uid, _, _ = identity
            if pid <= 1 or pid == os.getpid() or uid != os.getuid():
                raise RuntimeError("unsafe process identity in cleanup journal")
            current = self.api.info(pid)
            if current is None or current.identity() != identity or current.status == 5:  # SZOMB
                continue
            try:
                os.kill(pid, sig)
                self.signals.append({"identity": identity, "signal": sig.name})
            except ProcessLookupError:
                pass

    def clean(self, reap=None):
        started = time.monotonic()
        scan_errors = []
        consecutive_empty = 0
        while time.monotonic() - started < 12:
            if reap is not None:
                reap()  # Popen.poll reaps our immediate child, including a zombie.
            try:
                self.scan()
                if not self.owned and not self.uncertain:
                    consecutive_empty += 1
                    if consecutive_empty == 2:
                        return {"clean": True, "signals": self.signals, "scan_errors": scan_errors}
                else:
                    consecutive_empty = 0
                    sig = signal.SIGTERM if time.monotonic() - started < 4 else signal.SIGKILL
                    self.signal_owned(sig)
            except (OSError, ValueError, RuntimeError) as error:
                scan_errors.append(str(error))
                consecutive_empty = 0
            time.sleep(0.2)
        return {
            "clean": False, "owned": sorted(self.owned), "unidentified": self.uncertain,
            "signals": self.signals, "scan_errors": scan_errors,
        }

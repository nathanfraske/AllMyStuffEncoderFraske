#!/usr/bin/env python3
"""Manager-only gateway PTY wrapper for an explicitly supplied AMST command.

This file does not discover peers or construct AMST flags. Use the verified
same-account gateway command and an explicitly selected fleet endpoint. It
changes only its owned PTY size, captures every byte, and reaps its own child.
"""

import argparse
import errno
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import pty
import select
import signal
import struct
import sys
import termios
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="new gateway evidence directory")
    parser.add_argument("--timeout", type=float, default=1800)
    parser.add_argument("--windows-run", action="store_true",
                        help="require one physical PowerShell line in the final --run argument")
    parser.add_argument("command", nargs=argparse.REMAINDER,
                        help="-- followed by the manager's exact verified AMST argv")
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command or not math.isfinite(args.timeout) or not 0 < args.timeout <= 7200:
        parser.error("supply explicit argv and a finite positive timeout at most 7200s")
    if args.windows_run and (
            len(command) < 2 or command[-2] != "--run" or not command[-1]
            or "\n" in command[-1] or "\r" in command[-1]):
        parser.error("Windows --run must end with one nonempty physical command line")
    home = os.environ.get("MYOWNMESH_HOME")
    if not home or not Path(home).is_absolute():
        parser.error("manager must supply explicit same-account gateway MYOWNMESH_HOME")
    if not all(hasattr(os, name) for name in ("waitid", "WNOWAIT", "P_PID", "WEXITED")):
        parser.error("gateway requires waitid/WNOWAIT to retain owned leader custody until cleanup")
    output = args.output.absolute()
    output.mkdir(mode=0o700, exist_ok=False)
    started = time.monotonic()
    # Do not inherit auto-reap semantics: the unreaped owned leader reserves its
    # PID/PGID until all possible signaling and PTY cleanup have ended.
    signal.signal(signal.SIGCHLD, signal.SIG_DFL)
    child, master = pty.fork()
    if child == 0:
        try:
            # Set the actual child console size BEFORE AMST's raw-mode attach.
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
            os.execvp(command[0], command)
        except BaseException as error:
            os.write(2, ("owned AMST exec failed: " + type(error).__name__ + "\n").encode())
            os._exit(127)
    eof = False
    timed_out = False
    drain_timed_out = False
    exited_at = None
    stopped_at = None
    forced = False
    failure = None
    sha = hashlib.sha256()
    count = 0
    try:
        with (output / "pty.stdout").open("xb", buffering=0) as capture:
            while not eof or exited_at is None:
                now = time.monotonic()
                if stopped_at is None and now - started >= args.timeout:
                    timed_out = True
                    stopped_at = now
                    try:
                        os.killpg(child, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                if exited_at is not None and not eof and stopped_at is None and now - exited_at >= 10:
                    drain_timed_out = True
                    stopped_at = now
                    try:
                        os.killpg(child, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                if stopped_at is not None and now - stopped_at >= 5 and not forced:
                    forced = True
                    try:
                        os.killpg(child, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                if not eof and select.select([master], [], [], 0.1)[0]:
                    try:
                        block = os.read(master, 65536)
                    except OSError as error:
                        if error.errno != errno.EIO:
                            raise
                        block = b""  # Linux PTY peer closure, after buffered bytes drain.
                    if not block:
                        eof = True
                    else:
                        capture.write(block)
                        sha.update(block)
                        count += len(block)
                        sys.stdout.buffer.write(block)
                        sys.stdout.buffer.flush()
                if exited_at is None:
                    observed = os.waitid(os.P_PID, child, os.WEXITED | os.WNOHANG | os.WNOWAIT)
                    if observed is not None and observed.si_pid == child:
                        exited_at = time.monotonic()
                # A descendant that retained this owned PTY must not hang the
                # gateway forever after its foreground AMST child has exited.
                if stopped_at is not None and now - stopped_at >= 15 and not eof:
                    break
                if eof and exited_at is None:
                    time.sleep(0.05)
    except (Exception, KeyboardInterrupt) as error:
        failure = {"type": type(error).__name__, "message": str(error)}
    finally:
        if exited_at is None or not eof:
            try:
                os.killpg(child, signal.SIGKILL)
            except ProcessLookupError:
                pass
            forced = True
        os.close(master)
        # Single final reap, after the last possible signal/PTY close. No
        # numeric process-group signal is allowed after releasing this PID.
        _, status = os.waitpid(child, 0)
        child_exit = os.waitstatus_to_exitcode(status)
        exit_code = 124 if timed_out else (child_exit if child_exit >= 0 else 128 - child_exit)
        if (not eof or drain_timed_out) and exit_code == 0:
            exit_code = 125
        if failure is not None and exit_code == 0:
            exit_code = 1
        receipt = {
            "gateway_uname": platform.uname()._asdict(), "gateway_uid": os.getuid(),
            "gateway_mesh_home": home, "command": command, "child_pid": child,
            "rows": 40, "columns": 120, "pty_eof": eof,
            "stderr": "merged into PTY by forkpty; not a second native stream",
            "bytes": count, "sha256": sha.hexdigest(), "timed_out": timed_out,
            "drain_timed_out": drain_timed_out, "failure": failure,
            "forced_owned_cleanup": forced, "child_exit_code": child_exit,
            "exit_code": exit_code, "elapsed_seconds": time.monotonic() - started,
            "scope": "gateway only; nested target OS/toolchain/commit must appear in command output",
        }
        (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())

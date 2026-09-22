#!/usr/bin/env python3
"""Run the reviewed cumulative Mac suites; only the CI manager executes this.

No package installation, dependency updates, app startup or hardware discovery.
Test binaries are built with Cargo's locked graph, enumerated against the fixed
name inventory, and executed serially in a private environment. Logs survive
failures. The always step takes over cleanup after stopping the original runner.
"""

import argparse
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time

from macos_processes import DarwinProcesses, MARKER, ProcessGuard, write_json


HERE = Path(__file__).resolve().parent
LOCKS = ("Cargo.lock", "node/Cargo.lock", "gui/src-tauri/Cargo.lock", "gui/mobile/Cargo.lock")
CANCELLED = False


def cancel(_signal, _frame):
    global CANCELLED
    CANCELLED = True


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def lock_identities(workspace):
    return {name: digest(workspace / name) for name in LOCKS}


def request_stop(control):
    # A persistent one-way latch, including when the original supervisor did
    # not receive the workflow's cancellation signal. Never remove it.
    (control / "stop.requested").touch(exist_ok=True)


def acquire_cleanup_lease(control, lease, evidence):
    """Quiesce the original supervisor before touching its journal or files."""
    started = time.monotonic()
    api = None
    identity = None
    sent = set()
    evidence.update({"signals": [], "lease_acquired": False})
    while time.monotonic() - started < 45:
        try:
            fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
            evidence["lease_acquired"] = True
            evidence["elapsed_seconds"] = round(time.monotonic() - started, 3)
            return
        except OSError as error:
            if error.errno not in (errno.EACCES, errno.EAGAIN):
                raise
        receipt = control / "supervisor.json"
        if identity is None and receipt.is_file():
            identity = tuple(json.loads(receipt.read_text(encoding="utf-8"))["identity"])
            if (len(identity) != 4 or not all(type(value) is int for value in identity)
                    or identity[0] <= 1 or identity[0] == os.getpid()
                    or identity[1] != os.getuid() or identity[2] <= 0
                    or not 0 <= identity[3] < 1_000_000):
                raise RuntimeError("unsafe supervisor identity in cleanup receipt")
            api = DarwinProcesses()
            evidence["identity"] = identity
        elapsed = time.monotonic() - started
        sig = signal.SIGKILL if elapsed >= 30 else signal.SIGTERM if elapsed >= 2 else None
        if identity is not None and sig is not None and sig not in sent:
            current = api.info(identity[0])
            if current is not None and current.identity() == identity and current.status != 5:
                try:
                    os.kill(identity[0], sig)
                    evidence["signals"].append({"identity": identity, "signal": sig.name})
                except ProcessLookupError:
                    pass
                sent.add(sig)
        time.sleep(0.2)
    raise TimeoutError("original supervisor did not release its lifetime lease within 45 seconds")


def private_environment(root, original):
    # Keep only hosted toolchain discovery settings. Credentials, application
    # dials, production IPC addresses and the runner's ordinary HOME are omitted.
    environment = {key: original[key] for key in (
        "PATH", "RUSTUP_TOOLCHAIN", "DEVELOPER_DIR", "SDKROOT",
        "MACOSX_DEPLOYMENT_TARGET", "PKG_CONFIG_PATH",
    ) if key in original}
    environment.update({
        "HOME": str(root / "h"), "USERPROFILE": str(root / "h"),
        "XDG_CONFIG_HOME": str(root / "config"), "XDG_DATA_HOME": str(root / "data"),
        "XDG_STATE_HOME": str(root / "state"), "XDG_CACHE_HOME": str(root / "cache"),
        "XDG_RUNTIME_DIR": str(root / "run"),
        "MYOWNMESH_HOME": str(root / "mesh"),
        "TMPDIR": str(root / "t"), "TMP": str(root / "t"), "TEMP": str(root / "t"),
        "CARGO_HOME": str(root / "cargo"), "CARGO_TARGET_DIR": str(root / "target"),
        "RUSTUP_HOME": original.get("RUSTUP_HOME", str(Path(original["HOME"]) / ".rustup")),
        "RUSTUP_AUTO_INSTALL": "0", "CARGO_TERM_COLOR": "never", "CARGO_BUILD_JOBS": "2",
        "CARGO_INCREMENTAL": "0", "CARGO_PROFILE_DEV_DEBUG": "0", "CARGO_PROFILE_TEST_DEBUG": "0",
        "CARGO_NET_RETRY": "0", "CARGO_HTTP_TIMEOUT": "60", "CMAKE_POLICY_VERSION_MINIMUM": "3.5",
        "SHELL": "/bin/sh", "ENV": "", "BASH_ENV": "", "TERM": "xterm-256color",
        "LC_ALL": "C", "LANG": "C", "RUST_BACKTRACE": "1", "RUST_TEST_THREADS": "1",
        "ALLMYSTUFF_H264_DECODER": "software", "CI": "true",
    })
    for name in ("h", "t", "config", "data", "state", "cache", "run", "mesh", "cargo", "target", "cwd"):
        (root / name).mkdir(mode=0o700)
    return environment


class Runner:
    def __init__(self, args):
        self.workspace = Path(args.workspace).resolve(strict=True)
        self.artifacts = Path(args.artifacts).resolve()
        self.control = Path(args.control).resolve()
        self.artifacts.mkdir(mode=0o700, parents=True, exist_ok=False)
        self.control.mkdir(mode=0o700, parents=True, exist_ok=False)
        self.report = {"commands": [], "suites": [], "failures": []}
        self.deadline = time.monotonic() + 70 * 60
        self.guard = None
        self.root = None
        self.environment = None
        self.clean = True
        self.args = args
        self.save()

    def save(self):
        write_json(self.artifacts / "summary.json", self.report)

    def failure(self, context, error):
        self.report["failures"].append({"context": context, "error": str(error)})
        print(f"FAIL {context}: {error}", flush=True)
        self.save()

    def stopping(self):
        return CANCELLED or (self.control / "stop.requested").exists() or time.monotonic() >= self.deadline

    def run(self, label, command, timeout, cwd=None):
        if self.stopping():
            raise RuntimeError("run stopped, cancelled or total 70-minute deadline reached")
        if not self.clean:
            raise RuntimeError("previous child cleanup was not verified")
        index = len(self.report["commands"])
        stem = f"{index:02d}-{label}"
        stdout = self.artifacts / (stem + ".stdout.log")
        stderr = self.artifacts / (stem + ".stderr.log")
        record = {"label": label, "argv": command, "stdout": stdout.name, "stderr": stderr.name,
                  "started_unix_ns": time.time_ns()}
        self.report["commands"].append(record)
        self.save()
        print(f"RUN {label}", flush=True)
        started = time.monotonic()
        child = None
        try:
            with stdout.open("wb") as out, stderr.open("wb") as err:
                child = subprocess.Popen(command, cwd=cwd or self.workspace,
                                         env=self.environment, stdin=subprocess.DEVNULL,
                                         stdout=out, stderr=err, start_new_session=True)
                self.guard.register(child.pid)
                record["pid"] = child.pid
                self.save()
                limit = min(self.deadline, started + timeout)
                while child.poll() is None:
                    self.guard.scan()
                    if self.stopping() or time.monotonic() >= limit:
                        raise TimeoutError("command cancelled or deadline reached")
                    if stdout.stat().st_size + stderr.stat().st_size > 128 * 1024 * 1024:
                        raise RuntimeError("command output exceeded 128 MiB bound")
                    time.sleep(0.2)
                record["exit_code"] = child.returncode
                if child.returncode < 0:
                    request_stop(self.control)
                    record["stop_reason"] = "child terminated by signal; no later command may launch"
        except (OSError, RuntimeError, ValueError) as error:
            record["error"] = str(error)
            # Popen retains direct, unreaped-child ownership even if inspection
            # failed before registration. Its kill method checks/reaps safely.
            if child is not None and child.poll() is None:
                child.kill()
        finally:
            cleanup = self.guard.clean(child.poll if child else None)
            self.clean = cleanup["clean"]
            record["cleanup"] = cleanup
            record["elapsed_seconds"] = round(time.monotonic() - started, 3)
            record["finished_unix_ns"] = time.time_ns()
            record["stdout_bytes"] = stdout.stat().st_size if stdout.exists() else 0
            record["stderr_bytes"] = stderr.stat().st_size if stderr.exists() else 0
            record["ok"] = record.get("exit_code") == 0 and "error" not in record and self.clean
            self.save()
        print(f"{'PASS' if record['ok'] else 'FAIL'} {label} ({record['elapsed_seconds']}s)", flush=True)
        return record

    def checked(self, label, command, timeout=60):
        record = self.run(label, command, timeout)
        if not record["ok"]:
            raise RuntimeError(f"{label} failed; retained logs: {record['stdout']}, {record['stderr']}")
        return (self.artifacts / record["stdout"]).read_text(encoding="utf-8", errors="replace").strip()

    def prepare(self):
        if self.stopping():
            raise RuntimeError("cleanup or cancellation was requested before preparation")
        if sys.version_info < (3, 11):
            raise RuntimeError("the preinstalled Python must be 3.11 or newer")
        if sys.platform != "darwin" or platform.machine() != self.args.arch:
            raise RuntimeError("runner OS/architecture does not match the explicit matrix")
        self.root = Path(tempfile.mkdtemp(prefix="ams-mac-", dir="/tmp")).resolve(strict=True)
        os.chmod(self.root, 0o700)
        info = self.root.stat()
        write_json(self.control / "root.json", {
            "root": str(self.root), "device": info.st_dev, "inode": info.st_ino,
            "artifacts": str(self.artifacts), "workspace": str(self.workspace),
        })
        self.environment = private_environment(self.root, os.environ)
        # Darwin sockaddr_un.sun_path has 104 bytes including its terminator.
        # tempfile's Rust fixture suffix is shorter than this conservative bound.
        socket_example = self.root / "t" / ("ams-ipc-fixture-" + "x" * 16) / "fixture.sock"
        if len(os.fsencode(socket_example)) >= 104:
            raise RuntimeError("private IPC path exceeds the Darwin pathname socket budget")
        self.guard = ProcessGuard(self.control / "processes.json")
        supervisor = self.guard.api.info(os.getpid())
        if supervisor is None or supervisor.uid != os.getuid():
            raise RuntimeError("cannot record the original supervisor identity")
        write_json(self.control / "supervisor.json", {"identity": supervisor.identity()})
        self.report["supervisor_identity"] = supervisor.identity()
        self.environment[MARKER] = self.guard.token
        os.chdir(self.root / "cwd")
        self.report["isolation"] = {
            "root": str(self.root), "home": self.environment["HOME"],
            "tmpdir": self.environment["TMPDIR"], "socket_path_budget_bytes": len(os.fsencode(socket_example)),
            "environment_keys": sorted(self.environment), "process_identity": "PID/UID/start seconds+microseconds",
        }
        self.report["visibility_preflight"] = self.guard.qualify_shell_tools(self.environment)
        self.report["lockfiles_before"] = lock_identities(self.workspace)
        tools = {
            "commit": ["git", "rev-parse", "HEAD"], "uname": ["/usr/bin/uname", "-a"],
            "macos": ["/usr/bin/sw_vers"], "rustc": ["rustc", "-vV"],
            "cargo": ["cargo", "--version", "--verbose"],
            "toolchain": ["rustup", "show", "active-toolchain"],
            "clang": ["clang", "--version"], "cmake": ["cmake", "--version"],
            "sdk": ["/usr/bin/xcrun", "--show-sdk-path"],
            "xcode": ["/usr/bin/xcodebuild", "-version"],
        }
        self.report["provenance"] = {
            "architecture": platform.machine(), "python": sys.version,
            "runner_label": self.args.label,
            **{key: os.environ.get(key) for key in (
                "ImageOS", "ImageVersion", "ImageRelease", "RUNNER_ARCH",
                "GITHUB_SHA", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "GITHUB_REPOSITORY",
            )},
        }
        for label, command in tools.items():
            self.report["provenance"][label] = self.checked("preflight-" + label, command)
        expected_host = {"arm64": "aarch64-apple-darwin", "x86_64": "x86_64-apple-darwin"}[self.args.arch]
        if f"host: {expected_host}" not in self.report["provenance"]["rustc"]:
            raise RuntimeError("Rust toolchain is not native for this runner architecture")
        if self.report["provenance"]["commit"] != os.environ["GITHUB_SHA"]:
            raise RuntimeError("checkout does not match the workflow commit")
        self.report["provenance"]["nasm_path"] = shutil.which("nasm", path=self.environment["PATH"])
        self.save()

    def test_suite(self, suite, executable):
        receipt = {"id": suite["id"], "expected_count": len(suite["tests"]),
                   "binary": str(executable), "binary_sha256": digest(executable)}
        self.report["suites"].append(receipt)
        arguments = [suite["filter"]] if suite["filter"] else []
        listed = self.run(suite["id"] + "-list", [str(executable), *arguments, "--list", "--format", "terse"],
                          60, cwd=self.root / "cwd")
        text = (self.artifacts / listed["stdout"]).read_text(encoding="utf-8", errors="replace")
        actual = sorted(re.findall(r"^(.+): test$", text, flags=re.MULTILINE))
        receipt["listed_names"] = actual
        if not listed["ok"] or actual != sorted(suite["tests"]):
            raise RuntimeError(f"{suite['id']} test-name inventory differs; no tests were run")
        tested = self.run(suite["id"], [str(executable), *arguments, "--test-threads=1", "--nocapture"],
                          suite["timeout_seconds"], cwd=self.root / "cwd")
        text = (self.artifacts / tested["stdout"]).read_text(encoding="utf-8", errors="replace")
        counts = re.findall(r"^test result: ok\. (\d+) passed; 0 failed; 0 ignored; 0 measured; \d+ filtered out;", text, re.MULTILINE)
        receipt["passed"] = tested["ok"] and counts == [str(len(actual))] and digest(executable) == receipt["binary_sha256"]
        self.save()
        if not receipt["passed"]:
            raise RuntimeError(f"{suite['id']} failed or did not report the exact non-ignored test count")

    def group(self, group):
        command = ["cargo", "test", "--locked", "--no-run", "--message-format=json-render-diagnostics", *group["cargo_args"]]
        built = self.run("build-" + group["id"], command, 1800)
        if not built["ok"]:
            self.failure(group["id"], "locked test build failed; this group's suites were not executed")
            return
        artifacts = {}
        with (self.artifacts / built["stdout"]).open(encoding="utf-8") as stream:
            for line in stream:
                try:
                    value = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if value.get("reason") == "compiler-artifact" and value.get("executable") and value["profile"]["test"]:
                    source = Path(value["target"]["src_path"]).resolve()
                    executable = Path(value["executable"]).resolve(strict=True)
                    if not executable.is_relative_to(self.root / "target"):
                        raise RuntimeError("Cargo produced a test executable outside the private target")
                    artifacts[source] = executable
        for suite in group["suites"]:
            try:
                source = (self.workspace / suite["source"]).resolve(strict=True)
                if source not in artifacts:
                    raise RuntimeError(f"test executable missing for {suite['source']}")
                self.test_suite(suite, artifacts[source])
            except (OSError, RuntimeError, ValueError) as error:
                self.failure(suite["id"], error)
                if not self.clean or self.stopping():
                    raise

    def execute(self):
        try:
            self.prepare()
            config = json.loads((HERE / "modular-macos-suites.json").read_text(encoding="utf-8"))
            shutil.copyfile(HERE / "modular-macos-suites.json", self.artifacts / "selected-suites.json")
            self.report["expected_executions"] = sum(len(suite["tests"]) for group in config["groups"] for suite in group["suites"])
            self.report["audited_base"] = config["audited_base"]
            self.save()
            for group in config["groups"]:
                self.group(group)
            for check in config["checks"]:
                result = self.run(check["id"], ["cargo", "check", "--locked", *check["cargo_args"]], 1800)
                if not result["ok"]:
                    self.failure(check["id"], "Mac host compile failed; see retained command logs")
            self.checked("tracked-tree", ["git", "diff", "--exit-code", "HEAD", "--"], 60)
        except (OSError, RuntimeError, ValueError, KeyError) as error:
            self.failure("runner", error)
        finally:
            if self.guard is not None:
                cleanup = self.guard.clean()
                self.report["final_process_cleanup"] = cleanup
                if not cleanup["clean"]:
                    self.failure("cleanup", "unverified survivors; private directories retained")
            if "lockfiles_before" in self.report:
                self.report["lockfiles_after"] = lock_identities(self.workspace)
                if self.report["lockfiles_before"] != self.report["lockfiles_after"]:
                    self.failure("locks", "a committed lockfile changed")
            self.report["passed_executions"] = sum(suite["expected_count"] for suite in self.report["suites"] if suite.get("passed"))
            if self.report["passed_executions"] != self.report.get("expected_executions"):
                self.failure("coverage", "not all expected executions passed")
            self.report["passed"] = not self.report["failures"] and not self.stopping()
            self.save()
            summary = os.environ.get("GITHUB_STEP_SUMMARY")
            if summary:
                with open(summary, "a", encoding="utf-8") as stream:
                    stream.write(f"\nMac {self.args.label}: {self.report['passed_executions']}/{self.report.get('expected_executions', 239)} selected executions passed. "
                                 f"Overall: {'PASS' if self.report['passed'] else 'FAIL'}. Full commands, counts, identities and logs are in the artifact.\n")
        return 0 if self.report["passed"] else 1


def cleanup(args):
    control = Path(args.control).resolve()
    artifacts = Path(args.artifacts).resolve()
    control.mkdir(mode=0o700, parents=True, exist_ok=True)
    artifacts.mkdir(mode=0o700, parents=True, exist_ok=True)
    result = {"clean": False, "supervisor_handoff": {}}
    try:
        request_stop(control)
        with (control / "supervisor.lock").open("a+b") as lease:
            # The original supervisor holds this through its final journal and
            # summary writes. Holding it here excludes future child launches.
            acquire_cleanup_lease(control, lease, result["supervisor_handoff"])
            root_receipt = control / "root.json"
            if not root_receipt.exists():
                result.update({"clean": True, "reason": "private-root receipt absent; no child launch could begin"})
            else:
                receipt = json.loads(root_receipt.read_text(encoding="utf-8"))
                journal = control / "processes.json"
                if not journal.exists():
                    # No child can be launched before this journal exists.
                    result.update({"clean": True, "reason": "process guard was never initialized"})
                else:
                    guard = ProcessGuard(journal, restore=True)
                    result.update(guard.clean())
                # Preserve process evidence even if directory removal fails.
                write_json(artifacts / "always-cleanup.json", result)
                root = Path(receipt["root"])
                if result["clean"] and root.exists():
                    actual = root.resolve(strict=True)
                    info = actual.stat()
                    if (root.is_symlink() or actual != root or actual.parent != Path("/tmp").resolve()
                            or not actual.name.startswith("ams-mac-") or info.st_uid != os.getuid()
                            or stat.S_IMODE(info.st_mode) != 0o700
                            or (info.st_dev, info.st_ino) != (receipt["device"], receipt["inode"])):
                        raise RuntimeError("refusing cleanup: private root identity changed")
                    os.chdir(control)
                    shutil.rmtree(actual)
                    result["private_root_removed"] = not actual.exists()
    except (OSError, RuntimeError, ValueError, KeyError) as error:
        result.update({"clean": False, "error": str(error)})
        raise
    finally:
        write_json(artifacts / "always-cleanup.json", result)
        print(json.dumps(result, indent=2), flush=True)
    return 0 if result["clean"] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", default=os.environ.get("GITHUB_WORKSPACE"))
    parser.add_argument("--artifacts", required=True)
    parser.add_argument("--control", required=True)
    parser.add_argument("--arch", choices=("x86_64", "arm64"))
    parser.add_argument("--label")
    parser.add_argument("--cleanup-only", action="store_true")
    args = parser.parse_args()
    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, cancel)
    os.umask(0o077)
    if args.cleanup_only:
        return cleanup(args)
    runner = Runner(args)
    with (runner.control / "supervisor.lock").open("a+b") as lease:
        fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return runner.execute()


if __name__ == "__main__":
    raise SystemExit(main())

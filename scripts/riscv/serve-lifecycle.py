#!/usr/bin/env python3
"""Test real Serve IPC/lifecycle only inside the reviewed namespace launcher.

The daemon fixture is native /usr/bin/false. This is deliberately degraded
startup coverage, not successful Mesh transport or RISC-V daemon supervision.
"""
import argparse
import errno
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import socket
import stat
import struct
import subprocess
import sys
import time


HOME_KEYS = ("HOME", "ALLMYSTUFF_USER_HOME", "MYOWNMESH_HOME", "ALLMYSTUFF_HOME",
             "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME",
             "XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP")


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def isolation_check(path, result_dir):
    record = json.loads(path.read_text(encoding="utf-8"))
    require(sys.platform == "linux" and record["schema"] == 1, "Expected Linux isolation record v1")
    for name in ("mnt", "net", "pid", "ipc", "user"):
        entry = record["namespaces"][name]
        actual = os.readlink(f"/proc/self/ns/{name}")
        require(actual == entry["inner"] and actual != entry["outer"], f"Outside reviewed {name} namespace")
    work = Path(record["work_root"]).resolve(strict=True)
    require(work == Path("/tmp/ams"), "Unexpected disposable work root")
    results = Path(record["result_root"]).resolve(strict=True)
    require(result_dir.is_relative_to(results) and result_dir != results, "Result directory must be a new child of result_root")
    require(not result_dir.is_relative_to(work), "Results must outlive disposable state")
    for name in HOME_KEYS:
        require(name in os.environ, f"Missing isolated environment: {name}")
        require(Path(os.environ[name]).resolve(strict=True).is_relative_to(work), f"Unconfined environment: {name}")
    require(os.environ.get("ALLMYSTUFF_AUTOUPDATE") == "0", "Auto-update must be disabled by the launcher")
    return record, work


def receive_exact(connection, length):
    data = bytearray()
    while len(data) < length:
        chunk = connection.recv(length - len(data))
        require(chunk, "Truncated node IPC response")
        data.extend(chunk)
    return bytes(data)


def request(endpoint, command):
    body = b"\x00" + json.dumps({"cmd": command, "args": {}}, separators=(",", ":")).encode("utf-8")
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(5)
        connection.connect(str(endpoint))
        connection.sendall(struct.pack(">I", len(body)) + body)
        length = struct.unpack(">I", receive_exact(connection, 4))[0]
        # These three small JSON responses do not need the media-sized limit.
        require(1 <= length <= 65536, f"Unexpected node IPC length: {length}")
        response = receive_exact(connection, length)
    require(response[0] == 0, "Expected JSON response tag 0")
    value = json.loads(response[1:])
    require(isinstance(value, dict) and value.get("ok") is True and value.get("error") is None,
            f"Failed {command}: {value}")
    require(isinstance(value.get("result"), dict), f"Invalid {command} result")
    return value["result"]


def process_group_members(group):
    members = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            fields = (entry / "stat").read_text().rsplit(")", 1)[1].split()
            if int(fields[2]) == group:
                members.append({"pid": int(entry.name), "state": fields[0], "start_ticks": fields[19]})
        except (FileNotFoundError, ProcessLookupError):
            continue
    return members


def interrupt(signum, _frame):
    raise InterruptedError(f"Interrupted by signal {signum}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serve", required=True, type=Path)
    parser.add_argument("--artifact-record", required=True, type=Path, help="build.py's successful serve artifacts.json")
    parser.add_argument("--isolation-record", required=True, type=Path)
    parser.add_argument("--qemu", type=Path, default=Path("/usr/bin/qemu-riscv64"))
    parser.add_argument("--expected-version", required=True)
    parser.add_argument("--result-dir", required=True, type=Path)
    parser.add_argument("--phase-timeout", type=int, default=120)
    args = parser.parse_args()
    require(1 <= args.phase_timeout <= 600, "Phase deadline must be between 1 and 600 seconds")
    result_dir = args.result_dir.resolve()
    isolation, work = isolation_check(args.isolation_record, result_dir)
    artifact_record = json.loads(args.artifact_record.read_text(encoding="utf-8"))
    require(artifact_record["kind"] == "serve" and artifact_record["exit_code"] == 0
            and artifact_record.get("executed") is False,
            "Expected successful Serve compile/link evidence")
    require(re.fullmatch(r"[0-9a-f]{40}", artifact_record["head"]), "Missing build commit")
    artifacts = [item for item in artifact_record["artifacts"]
                 if item["target"]["name"] == "allmystuff-serve" and item["profile"]["test"] is False]
    require(len(artifacts) == 1, "Expected exactly one ordinary Serve artifact")
    serve = args.serve.resolve(strict=True)
    require(digest(serve) == artifacts[0]["sha256"], "Serve ELF differs from build evidence")
    with serve.open("rb") as source:
        header = source.read(64)
    require(header[:6] == b"\x7fELF\x02\x01" and struct.unpack_from("<H", header, 18)[0] == 243,
            "Expected little-endian RISC-V ELF64")
    qemu = args.qemu.resolve(strict=True)
    failed_daemon = Path("/usr/bin/false").resolve(strict=True)
    with failed_daemon.open("rb") as source:
        false_header = source.read(64)
    require(false_header[:6] == b"\x7fELF\x02\x01" and struct.unpack_from("<H", false_header, 18)[0] == 62,
            "Expected the reviewed native amd64 false helper")
    os.umask(0o077)
    result_dir.mkdir(mode=0o700)
    state = work / "serve-lifecycle"
    state.mkdir(mode=0o700)
    for name in ("home", "home/.myownmesh", "home/.allmystuff", "config", "data", "cache", "state", "run", "tmp", "bin"):
        (state / name).mkdir(mode=0o700)
    env = {"PATH": str(state / "bin"), "LANG": "C.UTF-8", "TZ": "UTC", "ALLMYSTUFF_AUTOUPDATE": "0"}
    homes = {"HOME": "home", "ALLMYSTUFF_USER_HOME": "home", "MYOWNMESH_HOME": "home/.myownmesh",
             "ALLMYSTUFF_HOME": "home/.allmystuff", "XDG_CONFIG_HOME": "config", "XDG_DATA_HOME": "data",
             "XDG_CACHE_HOME": "cache", "XDG_STATE_HOME": "state", "XDG_RUNTIME_DIR": "run",
             "TMPDIR": "tmp", "TMP": "tmp", "TEMP": "tmp"}
    env.update({name: str(state / relative) for name, relative in homes.items()})
    endpoint = state / "home/.myownmesh/allmystuff-node.sock"
    require(len(os.fsencode(endpoint)) < 108, "Fixture socket path is too long")
    elf_report = subprocess.check_output(["/usr/bin/readelf", "-l", "-d", str(serve)], env=env, text=True, timeout=10)
    (result_dir / "readelf.txt").write_text(elf_report, encoding="utf-8")
    require(not re.search(r"\bINTERP\b|\(NEEDED\)", elf_report), "Guest loader/shared libraries require separate review")
    command = [str(qemu), "-cpu", "rv64", str(serve), "--state-home", str(state / "home"), "--mesh-bin", str(failed_daemon)]
    result = {"passed": False, "source_head": artifact_record["head"], "serve_sha256": digest(serve),
              "artifact_record_sha256": digest(args.artifact_record), "fixture_sha256": digest(Path(__file__)),
              "isolation": isolation, "qemu_sha256": digest(qemu), "false_sha256": digest(failed_daemon),
              "command": command, "environment": env, "events": [], "cleanup": [],
              "scope": "Degraded daemon; real local Serve IPC and lifecycle; no successful Mesh session"}
    processes = []
    started = time.monotonic()

    def event(name, **values):
        result["events"].append({"event": name, "seconds": time.monotonic() - started, **values})

    def launch(name):
        with (result_dir / f"{name}.stdout.log").open("xb") as out, (result_dir / f"{name}.stderr.log").open("xb") as err:
            process = subprocess.Popen(command, cwd=state, env=env, stdout=out, stderr=err, start_new_session=True)
        processes.append(process)
        event("started", label=name, pid=process.pid)
        return process

    def ready(process, label):
        deadline = time.monotonic() + args.phase_timeout
        last = None
        while time.monotonic() < deadline:
            require(process.poll() is None, f"{label} exited before IPC readiness: {process.returncode}")
            try:
                version = request(endpoint, "node_version")
                owner = request(endpoint, "runtime_owner")
                link = request(endpoint, "link_status")
                require(version.get("version") == args.expected_version, f"Unexpected version: {version}")
                require(owner == {"owner": "all-my-stuff-installed", "yieldable": False}, f"Unexpected owner: {owner}")
                require("error" in link and (link["error"] is None or isinstance(link["error"], str)), f"Invalid link status: {link}")
                if link.get("status") != "disconnected":
                    last = link
                    time.sleep(0.1)
                    continue
                mode = endpoint.stat()
                require(stat.S_ISSOCK(mode.st_mode) and stat.S_IMODE(mode.st_mode) == 0o600,
                        "Node control endpoint must be a 0600 socket")
                require(mode.st_uid == os.geteuid(), "Socket is not owned by the fixture user")
                event("ipc_ready", label=label, version=version, owner=owner, link=link, socket_mode="0600")
                return
            except (FileNotFoundError, ConnectionRefusedError, TimeoutError) as error:
                last = str(error)
            time.sleep(0.1)
        raise TimeoutError(f"{label} did not answer ready IPC: {last}")

    def stop(process, label):
        require(process.poll() is None, f"{label} exited before SIGTERM")
        process.send_signal(signal.SIGTERM)
        code = process.wait(timeout=args.phase_timeout)
        require(code == 0, f"{label} SIGTERM exit was {code}")
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            connection.settimeout(5)
            try:
                connection.connect(str(endpoint))
            except OSError as error:
                require(error.errno in (errno.ENOENT, errno.ECONNREFUSED), f"Unexpected stopped-endpoint error: {error}")
            else:
                raise RuntimeError("Stopped Serve endpoint still accepts connections")
        event("stopped", label=label, returncode=code, socket_file_remains=endpoint.exists())
        require(not process_group_members(process.pid), f"{label} left owned process-group members")

    signal.signal(signal.SIGTERM, interrupt)
    signal.signal(signal.SIGINT, interrupt)
    try:
        first = launch("first")
        ready(first, "first")
        duplicate = launch("duplicate")
        code = duplicate.wait(timeout=args.phase_timeout)
        require(code == 0, f"Duplicate-owner launch exited {code}")
        event("duplicate_exited", returncode=code)
        ready(first, "first_after_duplicate")
        stop(first, "first")
        restarted = launch("restarted")
        ready(restarted, "restarted_same_state")
        stop(restarted, "restarted")
        require(digest(serve) == result["serve_sha256"], "Serve executable changed during fixture")
        result["passed"] = True
    except Exception as error:
        result["error"] = f"{type(error).__name__}: {error}"
    finally:
        # Signal every remaining owned group before waiting on any one of them.
        # A cleanup problem must not skip the other handles or erase evidence.
        cleanup_errors = []
        for process in processes:
            if process.poll() is None or process_group_members(process.pid):
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                except OSError as error:
                    cleanup_errors.append(f"killpg {process.pid}: {error}")
                result["passed"] = False
        for process in processes:
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                cleanup_errors.append(f"Owned process {process.pid} did not reap after SIGKILL")
            remaining = process_group_members(process.pid)
            result["cleanup"].append({"pid": process.pid, "returncode": process.returncode, "remaining": remaining})
            if remaining:
                result["passed"] = False
        if cleanup_errors:
            result["cleanup_errors"] = cleanup_errors
            result["passed"] = False
        result["elapsed_seconds"] = time.monotonic() - started
        with (result_dir / "result.json").open("x", encoding="utf-8") as output:
            json.dump(result, output, indent=2)
            output.write("\n")
        print(json.dumps(result, indent=2), flush=True)
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())

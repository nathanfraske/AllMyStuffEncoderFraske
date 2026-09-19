#!/usr/bin/env python3
"""Run recorded RISC-V libtest artifacts or the reviewed Serve lifecycle fixture."""

import argparse
import hashlib
import json
import os
from pathlib import Path, PureWindowsPath
import re
import resource
import shutil
import signal
import struct
import subprocess
import sys
import time
import traceback


TARGET = "riscv64gc-unknown-linux-musl"
WORK = Path("/tmp/ams")
NAMESPACES = ("user", "mnt", "net", "pid", "ipc")


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def write_json(path, value):
    with path.open("x", encoding="utf-8") as output:
        json.dump(value, output, indent=2)
        output.write("\n")


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8"))


def namespaces():
    return {name: os.readlink("/proc/self/ns/" + name) for name in NAMESPACES}


def interrupted(signum, _frame):
    raise SystemExit(128 + signum)


def static_elf(path):
    data = path.read_bytes()
    require(len(data) >= 64 and data[:6] == b"\x7fELF\x02\x01", "Expected ELF64 little-endian")
    kind, machine = struct.unpack_from("<HH", data, 16)
    require(kind in (2, 3) and machine == 243, "Expected a RISC-V executable")
    offset = struct.unpack_from("<Q", data, 32)[0]
    size, count = struct.unpack_from("<HH", data, 54)
    require(count > 0 and size >= 56 and offset + size * count <= len(data), "Invalid program headers")
    for index in range(count):
        header = offset + size * index
        segment = struct.unpack_from("<I", data, header)[0]
        require(segment != 3, "PT_INTERP requires a separately reviewed guest runtime")
        if segment == 2:
            start = struct.unpack_from("<Q", data, header + 8)[0]
            length = struct.unpack_from("<Q", data, header + 32)[0]
            require(start + length <= len(data) and length % 16 == 0, "Invalid dynamic segment")
            for position in range(start, start + length, 16):
                tag = struct.unpack_from("<q", data, position)[0]
                if tag == 0:
                    break
                require(tag != 1, "DT_NEEDED requires a separately reviewed guest runtime")
    return {"bytes": len(data), "class": 64, "endian": "little", "machine": machine,
            "type": kind, "flags": struct.unpack_from("<I", data, 48)[0]}


def execute(command, directory, environment, log_root, label, timeout=None):
    started = time.monotonic()
    timed_out = False
    with (log_root / (label + ".stdout.log")).open("xb") as out, (log_root / (label + ".stderr.log")).open("xb") as err:
        process = subprocess.Popen(command, cwd=directory, env=environment,
                                   stdin=subprocess.DEVNULL, stdout=out, stderr=err, start_new_session=True)
        try:
            process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
        finally:
            if process.poll() is None:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait()
    result = {"command": command, "returncode": process.returncode, "timed_out": timed_out,
              "elapsed_seconds": time.monotonic() - started}
    write_json(log_root / (label + ".json"), result)
    return result


def prepare_isolation(case, request):
    current = namespaces()
    require(os.getpid() == 1, "Inner launcher must be namespace PID 1")
    require(all(current[k] != request["outer_namespaces"][k] for k in NAMESPACES),
            "A requested namespace was not created")
    mount = request["tools"]["mount"]
    for destination, mode in (("/tmp", "1777"), ("/var/tmp", "1777"), ("/run", "0755")):
        subprocess.run([mount, "-t", "tmpfs", "-o", "mode=" + mode + ",nosuid,nodev",
                        "tmpfs", destination], check=True)
    # Keep Unix sockets on Linux tmpfs even when retained logs are on a WSL drive.
    for suffix in ("home/.myownmesh", "home/.allmystuff", "config", "data", "cache", "state", "run", "tmp", "work", "bin"):
        (WORK / suffix).mkdir(mode=0o700, parents=True, exist_ok=True)
    ip = request["tools"]["ip"]
    subprocess.run([ip, "link", "set", "dev", "lo", "up"], check=True)
    links = json.loads(subprocess.check_output([ip, "-json", "link", "show"], text=True))
    require(len(links) == 1 and links[0]["ifname"] == "lo" and "UP" in links[0]["flags"],
            "Expected only an enabled loopback interface")
    mounts = {}
    for line in Path("/proc/self/mountinfo").read_text(encoding="utf-8").splitlines():
        before, after = line.split(" - ", 1)
        fields = before.split()
        require(not any(field.startswith(("shared:", "master:")) for field in fields[6:]),
                "Mount propagation is not private")
        mounts[fields[4]] = after.split()[0]
    require(all(mounts.get(path) == "tmpfs" for path in ("/tmp", "/var/tmp", "/run")),
            "Missing private temporary mounts")
    require(mounts.get("/proc") == "proc", "Missing private proc mount")
    environment = {
        "PATH": str(WORK / "bin"), "LANG": "C.UTF-8", "TZ": "UTC",
        "PYTHONDONTWRITEBYTECODE": "1", "HOME": str(WORK / "home"),
        "ALLMYSTUFF_USER_HOME": str(WORK / "home"),
        "MYOWNMESH_HOME": str(WORK / "home/.myownmesh"),
        "ALLMYSTUFF_HOME": str(WORK / "home/.allmystuff"),
        "XDG_CONFIG_HOME": str(WORK / "config"), "XDG_DATA_HOME": str(WORK / "data"),
        "XDG_CACHE_HOME": str(WORK / "cache"), "XDG_STATE_HOME": str(WORK / "state"),
        "XDG_RUNTIME_DIR": str(WORK / "run"), "TMPDIR": str(WORK / "tmp"),
        "TMP": str(WORK / "tmp"), "TEMP": str(WORK / "tmp"), "ALLMYSTUFF_AUTOUPDATE": "0",
    }
    record = {"schema": 1, "namespaces": {key: {"outer": request["outer_namespaces"][key],
              "inner": current[key]} for key in NAMESPACES}, "work_root": str(WORK),
              "result_root": str(case), "environment": environment,
              "temporary_mounts": {key: mounts[key] for key in ("/tmp", "/var/tmp", "/run", "/proc")},
              "links": links, "host_kernel": os.uname().release}
    write_json(case / "isolation.json", record)
    return environment


def inner(case):
    request = read_json(case / "request.json")
    try:
        environment = prepare_isolation(case, request)
        binary = case / "target.elf"
        require(digest(binary) == request["artifact"]["sha256"], "Copied target ELF changed")
        require(digest(Path(request["qemu"])) == request["qemu_sha256"], "QEMU executable changed")
        command = [request["qemu"], "-cpu", "rv64", str(binary)]
        if request["mode"] == "tests":
            listed = execute(command + ["--list", "--format", "terse"], WORK / "work", environment, case, "list")
            require(listed["returncode"] == 0, "Test discovery failed")
            lines = (case / "list.stdout.log").read_text(encoding="utf-8").splitlines()
            names = [line[:-6] for line in lines if line.endswith(": test")]
            require(not any(line.endswith(": benchmark") for line in lines), "Benchmark artifact needs separate accounting")
            require(len(set(names)) == len(names), "Duplicate discovered test names")
            tested = execute(command + ["--test-threads=1", "--color", "never"], WORK / "work", environment, case, "test")
            output = (case / "test.stdout.log").read_text(encoding="utf-8", errors="replace")
            summaries = re.findall(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out;", output)
            counts = [int(value) for value in summaries[0]] if len(summaries) == 1 else None
            passed = tested["returncode"] == 0 and counts == [len(names), 0, 0, 0, 0]
            result = {"passed": passed, "tests": names, "counts_passed_failed_ignored_measured_filtered": counts,
                      "test_process": tested, "scope": "Target libtest under user-mode QEMU; host kernel/sysfs view"}
        else:
            harness = Path(request["lifecycle_harness"])
            require(digest(harness) == request["lifecycle_sha256"], "Lifecycle harness changed")
            destination = case / "lifecycle"
            lifecycle = execute([sys.executable, str(harness), "--serve", str(binary),
                "--artifact-record", str(Path(request["build_record"]) / "artifacts.json"), "--qemu", request["qemu"],
                "--expected-version", request["expected_version"], "--result-dir", str(destination),
                "--isolation-record", str(case / "isolation.json")], WORK / "work", environment, case, "lifecycle")
            evidence = read_json(destination / "result.json")
            passed = (lifecycle["returncode"] == 0 and evidence["passed"] is True
                      and evidence["source_head"] == request["head"]
                      and evidence["serve_sha256"] == request["artifact"]["sha256"])
            result = {"passed": passed, "lifecycle_process": lifecycle,
                      "scope": "Degraded Serve lifecycle with a native failed-daemon fixture"}
        result["elf_unchanged"] = digest(binary) == request["artifact"]["sha256"]
        result["passed"] = result["passed"] and result["elf_unchanged"]
        write_json(case / "result.json", result)
        return 0 if result["passed"] else 1
    except Exception as error:
        write_json(case / "result.json", {"passed": False, "error": str(error)})
        traceback.print_exc()
        return 1
    # Namespace PID 1 exiting also terminates any remaining descendants.


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("tests", "lifecycle"))
    parser.add_argument("--build-record", type=Path)
    parser.add_argument("--artifact-root", type=Path, help="Linux path corresponding to the recorded Cargo target directory")
    parser.add_argument("--expected-head")
    parser.add_argument("--run-root", type=Path)
    parser.add_argument("--timeout", type=int, default=1200, help="External deadline per executable, including discovery")
    parser.add_argument("--lifecycle-sha256")
    parser.add_argument("--inside", type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    require(sys.platform == "linux", "Run centrally inside the reviewed Linux environment")
    os.umask(0o077)
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    signal.signal(signal.SIGTERM, interrupted)
    if args.inside:
        return inner(args.inside.resolve(strict=True))
    require(args.mode and args.build_record and args.artifact_root and args.run_root,
            "Supply --mode, --build-record, --artifact-root and --run-root")
    require(re.fullmatch(r"[0-9a-f]{40}", args.expected_head or ""), "Supply the exact reviewed build HEAD")
    require(1 <= args.timeout <= 7200, "Deadline must be 1..7200 seconds")
    record = args.build_record.resolve(strict=True)
    require(not any(path.is_relative_to(Path(temporary)) for path in (record, Path(__file__).resolve())
                    for temporary in ("/tmp", "/var/tmp", "/run")),
            "Build records and launcher checkout must remain visible outside private temporary mounts")
    identity = read_json(record / "identity.json")
    artifacts = read_json(record / "artifacts.json")
    metadata = read_json(record / "metadata.json")
    require(identity["head"] == artifacts["head"] == args.expected_head, "Build source identity mismatch")
    require(identity["target"] == TARGET and artifacts["exit_code"] == 0
            and identity["executed"] is False and artifacts["executed"] is False,
            "Expected a successful compile-only target build")
    require(identity["kind"] == artifacts["kind"], "Build kind mismatch")
    if args.mode == "tests":
        require(identity["kind"] in ("root-tests", "node-tests") and "--workspace" in identity["command"],
                "Expected full workspace test compilation")
        selected = [item for item in artifacts["artifacts"] if item["profile"]["test"] is True]
    else:
        require(identity["kind"] == "serve", "Expected the ordinary Serve build")
        require(re.fullmatch(r"[0-9a-f]{64}", args.lifecycle_sha256 or ""), "Supply reviewed lifecycle script SHA256")
        selected = [item for item in artifacts["artifacts"] if item["profile"]["test"] is False
                    and item["target"]["name"] == "allmystuff-serve"]
        require(len(selected) == 1, "Expected exactly one ordinary Serve executable")
    require(selected, "No matching artifacts")
    source_root = args.artifact_root.resolve(strict=True)
    recorded_root = PureWindowsPath(identity["environment_overrides"]["CARGO_TARGET_DIR"])
    tools = {name: shutil.which(name) for name in ("unshare", "mount", "ip")}
    require(all(tools.values()), "Missing native namespace prerequisites: " + str(tools))
    qemu = Path("/usr/bin/qemu-riscv64").resolve(strict=True)
    native_environment = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "PYTHONDONTWRITEBYTECODE": "1"}
    version = subprocess.check_output([str(qemu), "--version"], env=native_environment, text=True, timeout=10).strip()
    root = args.run_root.resolve()
    require(not any(root.is_relative_to(Path(path)) for path in ("/tmp", "/var/tmp", "/run")),
            "Retained output must be outside private temporary mounts")
    root.mkdir(mode=0o700)
    packages = {item["id"]: item for item in metadata["packages"]}
    cases = []
    for index, item in enumerate(selected):
        relative = PureWindowsPath(item["path"]).relative_to(recorded_root)
        require(relative.parts and relative.parts[0] == TARGET and ".." not in relative.parts,
                "Artifact path is outside the recorded target")
        source = source_root.joinpath(*relative.parts).resolve(strict=True)
        require(source.is_relative_to(source_root) and digest(source) == item["sha256"], "Artifact path/hash mismatch")
        elf = static_elf(source)
        require(elf["bytes"] == item["bytes"], "Artifact size mismatch")
        case = root / ("artifact-" + str(index).zfill(3))
        case.mkdir(mode=0o700)
        shutil.copyfile(source, case / "target.elf")
        (case / "target.elf").chmod(0o700)
        request = {"mode": args.mode, "artifact": item, "elf": elf, "head": args.expected_head,
                   "build_record": str(record), "build_inputs": {name: digest(record / name) for name in
                   ("identity.json", "artifacts.json", "metadata.json")}, "locks": identity["locks"],
                   "qemu": str(qemu), "qemu_sha256": digest(qemu), "qemu_version": version,
                   "runner_sha256": digest(Path(__file__)), "outer_namespaces": namespaces(),
                   "tools": tools, "timeout_seconds": args.timeout}
        if args.mode == "lifecycle":
            harness = Path(__file__).resolve().with_name("serve-lifecycle.py")
            require(digest(harness) == args.lifecycle_sha256, "Unexpected lifecycle script")
            request.update(lifecycle_harness=str(harness), lifecycle_sha256=args.lifecycle_sha256,
                           expected_version=packages[item["package_id"]]["version"])
        write_json(case / "request.json", request)
        cases.append(case)
    write_json(root / "selection.json", {"head": args.expected_head, "mode": args.mode,
        "selected": selected, "excluded": [item for item in artifacts["artifacts"] if item not in selected],
        "exclusion_reason": "Artifact profile/kind belongs to the other execution gate"})
    outcomes = []
    for case in cases:
        command = [tools["unshare"], "--user", "--map-root-user", "--mount", "--propagation", "private",
                   "--net", "--ipc", "--pid", "--fork", "--mount-proc", "--kill-child=KILL",
                   sys.executable, str(Path(__file__).resolve()), "--inside", str(case)]
        launched = execute(command, root, native_environment, case, "launcher", timeout=args.timeout)
        result_path = case / "result.json"
        result = read_json(result_path) if result_path.exists() else {"passed": False, "error": "No inner result"}
        result["passed"] = result["passed"] and launched["returncode"] == 0 and not launched["timed_out"]
        outcomes.append({"case": str(case), "launcher": launched, "result": result})
        for path in sorted(case.glob("*.log")):
            print(str(path) + ":", flush=True)
            sys.stdout.buffer.write(path.read_bytes())
            sys.stdout.buffer.flush()
        print(json.dumps(outcomes[-1], indent=2), flush=True)
        if not (case / "isolation.json").exists():
            break  # A failed isolation preflight must not repeat for every artifact.
    summary = {"passed": len(outcomes) == len(cases) and all(item["result"]["passed"] for item in outcomes),
               "selected_executables": len(cases), "attempted_executables": len(outcomes), "outcomes": outcomes,
               "limits": "User-mode emulation with host kernel/sysfs; doctests and real Mesh/hardware are separate"}
    write_json(root / "result.json", summary)
    print(json.dumps(summary, indent=2), flush=True)
    return 0 if summary["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

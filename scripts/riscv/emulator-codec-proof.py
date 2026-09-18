#!/usr/bin/env python3
"""Execute only the retained, hash-pinned synthetic OpenH264 test under QEMU."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import shutil
import signal
import struct
import subprocess
import sys
import time


PROOF_SHA256 = "0dc6386fa8000a87f38d9ca18380f4fa4aea3fbb34eed73c62d5cce3dee3f2d6"
PROOF_BYTES = 23792376
TEST_NAME = "software_encode_decode_roundtrip"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def interrupted(signum, _frame):
    raise SystemExit(128 + signum)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--elf", required=True, type=Path)
    parser.add_argument("--run-root", required=True, type=Path)
    parser.add_argument("--timeout", type=int, default=180)
    args = parser.parse_args()
    if sys.platform != "linux" or not 1 <= args.timeout <= 1800:
        parser.error("Use Linux and an external timeout between 1 and 1800 seconds")
    original = args.elf.resolve(strict=True)
    data = original.read_bytes()
    if len(data) != PROOF_BYTES or hashlib.sha256(data).hexdigest() != PROOF_SHA256:
        parser.error("This runner accepts only the reviewed retained codec proof")
    if (data[:6] != b"\x7fELF\x02\x01"
            or struct.unpack_from("<HH", data, 16) != (3, 243)
            or struct.unpack_from("<I", data, 48)[0] != 5):
        parser.error("Unexpected ELF identity")
    qemu = Path("/usr/bin/qemu-riscv64").resolve(strict=True)
    readelf = Path("/usr/bin/readelf").resolve(strict=True)
    os.umask(0o077)
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    run_root = args.run_root.resolve()
    # The parent must exist; existing attempts are never overwritten or removed.
    run_root.mkdir(mode=0o700)
    for name in ("home", "mesh", "app", "config", "data", "cache", "state", "runtime", "tmp", "work", "bin"):
        (run_root / name).mkdir(mode=0o700)
    binary = run_root / "codec-riscv64gc-linux-musl"
    shutil.copyfile(original, binary)
    binary.chmod(0o700)
    if digest(binary) != PROOF_SHA256:
        raise RuntimeError("Retained proof changed while copying")
    environment = {
        "PATH": str(run_root / "bin"), "LANG": "C.UTF-8", "TZ": "UTC",
        "HOME": str(run_root / "home"), "ALLMYSTUFF_USER_HOME": str(run_root / "home"),
        "MYOWNMESH_HOME": str(run_root / "mesh"), "ALLMYSTUFF_HOME": str(run_root / "app"),
        "XDG_CONFIG_HOME": str(run_root / "config"), "XDG_DATA_HOME": str(run_root / "data"),
        "XDG_CACHE_HOME": str(run_root / "cache"), "XDG_STATE_HOME": str(run_root / "state"),
        "XDG_RUNTIME_DIR": str(run_root / "runtime"), "TMPDIR": str(run_root / "tmp"),
        "TMP": str(run_root / "tmp"), "TEMP": str(run_root / "tmp"),
        "ALLMYSTUFF_AUTOUPDATE": "0",
    }
    version = subprocess.check_output([str(qemu), "--version"], env=environment,
                                      text=True, timeout=10).strip()
    elf_report = subprocess.check_output([str(readelf), "-l", "-d", str(binary)],
                                         env=environment, text=True, timeout=10)
    (run_root / "readelf.txt").write_text(elf_report, encoding="utf-8")
    if re.search(r"\bINTERP\b|\(NEEDED\)", elf_report):
        raise RuntimeError("Unexpected guest loader/shared-library requirement")
    command = [str(qemu), "-cpu", "rv64", str(binary), "--test-threads=1", "--color", "never"]
    request = {
        "original_elf": str(original), "elf_sha256": PROOF_SHA256, "elf_bytes": PROOF_BYTES,
        "elf_class": 64, "elf_endianness": "little", "elf_type": 3, "elf_machine": 243,
        "elf_flags": 5, "test": TEST_NAME, "qemu": str(qemu), "qemu_version": version,
        "qemu_sha256": digest(qemu), "runner_sha256": digest(Path(__file__)),
        "command": command, "environment": environment, "timeout_seconds": args.timeout,
        "scope": "One synthetic codec test under user-mode QEMU on the host Linux kernel",
    }
    write_json(run_root / "request.json", request)
    print(json.dumps(request, indent=2), flush=True)
    signal.signal(signal.SIGTERM, interrupted)
    started = time.monotonic()
    timed_out = False
    with (run_root / "stdout.log").open("wb") as out, (run_root / "stderr.log").open("wb") as err:
        process = subprocess.Popen(command, cwd=run_root / "work", env=environment,
                                   stdout=out, stderr=err, start_new_session=True)
        try:
            returncode = process.wait(timeout=args.timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
        finally:
            if process.poll() is None:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait()
        returncode = process.returncode
    output = (run_root / "stdout.log").read_text(encoding="utf-8", errors="replace")
    summaries = re.findall(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out;", output)
    counts = [int(value) for value in summaries[0]] if len(summaries) == 1 else None
    named_pass = bool(re.search(r"^test " + re.escape(TEST_NAME) + r" \.\.\. ok$", output, re.MULTILINE))
    passed = not timed_out and returncode == 0 and named_pass and counts == [1, 0, 0, 0, 0]
    result = {
        "passed": passed, "returncode": returncode, "timed_out": timed_out,
        "elapsed_seconds": time.monotonic() - started,
        "counts_passed_failed_ignored_measured_filtered": counts, "named_test_passed": named_pass,
        "stdout_bytes": (run_root / "stdout.log").stat().st_size,
        "stderr_bytes": (run_root / "stderr.log").stat().st_size,
        "elf_unchanged": digest(binary) == PROOF_SHA256,
    }
    if not result["elf_unchanged"]:
        result["passed"] = False
    write_json(run_root / "result.json", result)
    for name in ("stdout.log", "stderr.log"):
        print(name + ":", flush=True)
        sys.stdout.buffer.write((run_root / name).read_bytes())
        sys.stdout.buffer.flush()
    print(json.dumps(result, indent=2), flush=True)
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

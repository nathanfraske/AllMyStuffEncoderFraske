"""Windows-to-RISC-V compile/link recipe; never executes a target program."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tomllib

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
TARGET = "riscv64gc-unknown-linux-musl"
VS_CMAKE = Path(r"C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake")
DEFAULT_ZIG = Path(r"C:\Users\Admin\AppData\Local\Microsoft\WinGet\Packages\zig.zig_Microsoft.Winget.Source_8wekyb3d8bbwe\zig-x86_64-windows-0.16.0\zig.exe")
LOCKS = ("Cargo.lock", "node/Cargo.lock", "gui/src-tauri/Cargo.lock", "gui/mobile/Cargo.lock")
RECIPE = ("build.py", "zig-cc.cmd", "zig-cxx.cmd", "zig-ar.cmd", "zig-ranlib.cmd", "zig-musl-toolchain.cmake")


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def capture(command):
    return subprocess.check_output(command, cwd=REPO, text=True, encoding="utf-8").strip()


def write_json(path, value):
    with path.open("x", encoding="utf-8") as output:
        json.dump(value, output, indent=2)
        output.write("\n")


def elf_identity(path):
    with path.open("rb") as binary:
        header = binary.read(64)
    require(len(header) == 64 and header[:6] == b"\x7fELF\x02\x01", f"Not a little-endian ELF64: {path}")
    require(int.from_bytes(header[18:20], "little") == 243, f"Not RISC-V: {path}")
    require(int.from_bytes(header[16:18], "little") in (2, 3), f"Not an executable ELF: {path}")
    return {"path": str(path), "bytes": path.stat().st_size, "sha256": sha(path),
            "elf_flags": int.from_bytes(header[48:52], "little")}


def build_environment(args):
    env = os.environ.copy()
    recorded = {}

    def set_value(key, value):
        env[key] = value
        recorded[key] = value

    # Restrict all changes to this child environment. Do not edit global Cargo,
    # registry, compiler, or emulator configuration.
    for key in ("CFLAGS", "CXXFLAGS", "ARFLAGS", "TARGET_CFLAGS", "TARGET_CXXFLAGS", "TARGET_ARFLAGS",
                "RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "LIBOPUS_LIB_DIR", "OPUS_LIB_DIR"):
        env.pop(key, None)
    for suffix in (TARGET, TARGET.replace("-", "_")):
        for name, wrapper in (("CC", "zig-cc.cmd"), ("CXX", "zig-cxx.cmd"),
                              ("AR", "zig-ar.cmd"), ("RANLIB", "zig-ranlib.cmd")):
            set_value(f"{name}_{suffix}", str(HERE / wrapper))
        set_value(f"CXXSTDLIB_{suffix}", "c++")
        for name in ("CFLAGS", "CXXFLAGS", "ARFLAGS"):
            set_value(f"{name}_{suffix}", "")
        set_value(f"CMAKE_{suffix}", str(args.cmake))
        set_value(f"CMAKE_GENERATOR_{suffix}", "Ninja")
        set_value(f"CMAKE_TOOLCHAIN_FILE_{suffix}", (HERE / "zig-musl-toolchain.cmake").as_posix())
    settings = {
        "ALLMYSTUFF_RISCV_ZIG": str(args.zig),
        "ALLMYSTUFF_RISCV_NINJA": str(args.ninja),
        "CARGO_NET_OFFLINE": "true",
        "CARGO_BUILD_JOBS": "2",
        "CARGO_INCREMENTAL": "0",
        "CARGO_PROFILE_DEV_DEBUG": "0",
        "CARGO_PROFILE_TEST_DEBUG": "0",
        "CARGO_TARGET_DIR": str(REPO / "target"),
        "CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_MUSL_LINKER": str(HERE / "zig-cxx.cmd"),
        "CARGO_ENCODED_RUSTFLAGS": "-C\x1flinker-flavor=gcc\x1f-C\x1flink-self-contained=no",
        "CMAKE_POLICY_VERSION_MINIMUM": "3.5",
        "OPUS_STATIC": "1",
        "OPUS_NO_PKG": "1",
        "LIBSQLITE3_SYS_USE_PKG_CONFIG": "0",
        "ZIG_GLOBAL_CACHE_DIR": str(REPO / "target/riscv-zig-cache/global"),
        "ZIG_LOCAL_CACHE_DIR": str(REPO / "target/riscv-zig-cache/local"),
        "TMP": str(REPO / "target/riscv-tmp"),
        "TEMP": str(REPO / "target/riscv-tmp"),
    }
    for key, value in settings.items():
        set_value(key, value)
    for key in ("ZIG_GLOBAL_CACHE_DIR", "ZIG_LOCAL_CACHE_DIR", "TMP"):
        Path(env[key]).mkdir(parents=True, exist_ok=True)
    return env, recorded


def verify_graph(graph, kind):
    packages = graph["packages"]
    nodes = {node["id"]: node for node in graph["resolve"]["nodes"]}
    if kind == "root-tests":
        require(not any(package["name"] == "openh264-sys2" for package in packages),
                "Root workspace unexpectedly pulls native OpenH264")
        return
    versions = {"openh264": "0.9.3", "openh264-sys2": "0.9.6", "opus": "0.3.1",
                "audiopus_sys": "0.2.2", "cmake": "0.1.58", "libsqlite3-sys": "0.30.1", "ring": "0.17.14"}
    for name, version in versions.items():
        selected = [package for package in packages if package["name"] == name]
        require(len(selected) == 1 and selected[0]["version"] == version, f"Unexpected {name} version")
    sys2 = next(package for package in packages if package["name"] == "openh264-sys2")
    require(Path(sys2["manifest_path"]).resolve() == REPO / "vendor/openh264-sys2-0.9.6/Cargo.toml",
            "Expected reviewed vendored OpenH264 source")
    require(sys2["source"] is None and "source" in nodes[sys2["id"]]["features"], "OpenH264 source feature missing")
    node = next(package for package in packages if package["name"] == "allmystuff-node")
    features = nodes[node["id"]]["features"]
    require(not set(features) & {"host", "audio-io", "hwenc"}, f"Unexpected node feature set: {features}")
    require(sys2["id"] not in graph["workspace_members"], "Vendor became a workspace member")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kind", choices=("serve", "root-tests", "node-tests"), default="serve")
    parser.add_argument("--attempt", required=True, help="New audit label; existing attempts are never overwritten")
    parser.add_argument("--expected-head", required=True, help="Exact reviewed integration commit")
    parser.add_argument("--zig", type=Path, default=DEFAULT_ZIG)
    parser.add_argument("--cmake", type=Path, default=VS_CMAKE / "CMake/bin/cmake.exe")
    parser.add_argument("--ninja", type=Path, default=VS_CMAKE / "Ninja/ninja.exe")
    args = parser.parse_args()
    require(os.name == "nt", "This recipe currently targets the reviewed Windows host tools")
    require(re.fullmatch(r"[A-Za-z0-9_-]{1,64}", args.attempt), "Invalid attempt label")
    require(re.fullmatch(r"[0-9a-f]{40}", args.expected_head), "Expected a full Git commit")
    require(capture(["git", "rev-parse", "HEAD"]) == args.expected_head, "Wrong integration commit")
    subprocess.run(["git", "diff", "--quiet", "HEAD", "--"], cwd=REPO, check=True)
    for name in ("zig", "cmake", "ninja"):
        path = getattr(args, name).resolve()
        require(path.is_file(), f"Missing reviewed {name} executable: {path}")
        setattr(args, name, path)
    cargo = shutil.which("cargo")
    require(cargo is not None, "Cargo is unavailable")
    rust_version = capture(["rustc", "-vV"])
    require("release: 1.97.1" in rust_version and "host: x86_64-pc-windows-msvc" in rust_version,
            "Different Rust toolchain requires review")
    require(capture([str(args.zig), "version"]) == "0.16.0", "Different Zig version requires review")
    subprocess.run([sys.executable, str(REPO / "vendor/verify_openh264.py")], cwd=REPO, check=True)
    locks = {relative: sha(REPO / relative) for relative in LOCKS}
    require((REPO / "target").resolve().is_relative_to(REPO), "Target directory resolves outside this checkout")
    record = REPO / "target/riscv-runs" / args.attempt
    record.mkdir(parents=True, exist_ok=False)
    env, recorded_env = build_environment(args)
    manifest = "Cargo.toml" if args.kind == "root-tests" else "node/Cargo.toml"
    options = ["--manifest-path", manifest, "--locked", "--offline"]
    if args.kind != "root-tests":
        options += ["--no-default-features"]
    command = [cargo, "build" if args.kind == "serve" else "test", *options, "--target", TARGET]
    command += ["--bin", "allmystuff-serve"] if args.kind == "serve" else ["--no-run"]
    if args.kind == "root-tests":
        command += ["--workspace"]
    command += ["-j", "2", "--message-format=json-render-diagnostics"]
    identity = {"utc": datetime.now(timezone.utc).isoformat(), "head": args.expected_head,
                "kind": args.kind, "target": TARGET, "command": command, "executed": False,
                "locks": locks, "recipe": {relative: sha(HERE / relative) for relative in RECIPE},
                "rustc": rust_version, "tools": {name: {"path": str(getattr(args, name)),
                    "sha256": sha(getattr(args, name))} for name in ("zig", "cmake", "ninja")},
                "environment_overrides": recorded_env}
    write_json(record / "identity.json", identity)
    print(json.dumps({"record": str(record), "head": args.expected_head, "command": command, "executed": False}), flush=True)
    try:
        metadata = [cargo, "metadata", *options, "--format-version", "1", "--filter-platform", TARGET]
        with (record / "metadata.json").open("xb") as output:
            result = subprocess.run(metadata, cwd=REPO, env=env, stdout=output)
        if result.returncode:
            return result.returncode
        graph = json.loads((record / "metadata.json").read_text(encoding="utf-8"))
        verify_graph(graph, args.kind)
        artifacts = []
        with (record / "cargo.jsonl").open("w", encoding="utf-8") as output:
            with subprocess.Popen(command, cwd=REPO, env=env, stdout=subprocess.PIPE,
                                  text=True, encoding="utf-8", errors="replace") as process:
                for line in process.stdout:
                    output.write(line)
                    output.flush()
                    print(line, end="", flush=True)
                    try:
                        message = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    if message.get("reason") == "compiler-artifact" and message.get("executable"):
                        artifacts.append(message)
                code = process.wait()
        linked = []
        if code == 0:
            for artifact in artifacts:
                path = Path(artifact["executable"]).resolve()
                require(path.is_relative_to(REPO / "target" / TARGET), f"Unexpected non-target artifact: {path}")
                linked.append({**elf_identity(path), "package_id": artifact["package_id"],
                               "target": artifact["target"], "profile": artifact["profile"]})
            require(linked, "Cargo succeeded but returned no target executable")
        write_json(record / "artifacts.json", {"head": args.expected_head, "kind": args.kind,
                   "exit_code": code, "executed": False, "artifacts": linked})
        return code
    finally:
        require({relative: sha(REPO / relative) for relative in LOCKS} == locks, "A represented lock changed")
        require(capture(["git", "rev-parse", "HEAD"]) == args.expected_head, "Source HEAD changed during build")
        subprocess.run(["git", "diff", "--quiet", "HEAD", "--"], cwd=REPO, check=True)


if __name__ == "__main__":
    sys.exit(main())

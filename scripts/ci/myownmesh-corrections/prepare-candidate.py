#!/usr/bin/env python3
"""Prepare and identify an explicit patched candidate; never build or install it.

Use a new private exact-base checkout with autocrlf disabled. Every tracked byte
must match its index blob. Outputs are new files outside that checkout. A failed
operation leaves its source copy for inspection; do not blindly repeat it.
"""

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import platform
import re
import subprocess
import sys

BASE = "db7818e09fedd98899490347b86ac9bc9f97b59b"
BASE_TREE = "5492d1845e15dbc067acec3ec592bb9500b9e21d"
BASE_LOCK = "8f5a30ce863c43b0a8c5225f74d7446aed7656cd7032f47e70030526381930c6"
SET_SCHEMA = "myownmesh-corrections-patch-set-v1"
SOURCE_SCHEMA = "myownmesh-corrections-source-v1"
BUILD_SCHEMA = "myownmesh-corrections-build-v1"
HEX40 = re.compile(r"[0-9a-f]{40}")
HEX64 = re.compile(r"[0-9a-f]{64}")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def file_bytes(path):
    require(path.is_file() and not path.is_symlink(), "regular owned input file required")
    return path.read_bytes()


def selected_json(path, expected_sha):
    require(HEX64.fullmatch(expected_sha) is not None, "exact selected SHA256 required")
    data = file_bytes(path)
    require(sha256(data) == expected_sha, "selected input SHA256 differs")
    value = json.loads(data)
    require(isinstance(value, dict), "JSON object required")
    return value


def new_output(path, upstream):
    parent = path.parent.resolve(strict=True)
    target = parent / path.name
    require(not target.exists() and not target.is_symlink(), "new receipt output required")
    require(not target.is_relative_to(upstream), "receipt must be outside the source checkout")
    return target


def write_json(path, value):
    with path.open("x", encoding="utf-8", newline="\n") as target:
        json.dump(value, target, indent=2)
        target.write("\n")


def git(root, *args, input_data=None):
    result = subprocess.run(["git", *args], cwd=root, input=input_data, capture_output=True, timeout=60, check=False)
    require(result.returncode == 0, "local Git operation failed; inspect the private checkout without retry")
    return result.stdout


def text_git(root, *args):
    return git(root, *args).decode("utf-8").strip()


def source_state(root):
    require(text_git(root, "rev-parse", "--show-toplevel") == root.as_posix(), "source Git root differs")
    require(text_git(root, "rev-parse", "HEAD") == BASE, "source HEAD differs from exact base")
    require(text_git(root, "rev-parse", "HEAD^{tree}") == BASE_TREE, "source base tree differs")
    require(not git(root, "ls-files", "--others", "--exclude-standard", "-z"), "untracked source input refused")
    # This compares working bytes directly, not Git's autocrlf-normalized diff.
    entries = git(root, "ls-files", "--stage", "-z")
    count = 0
    for entry in entries.split(b"\0"):
        if not entry:
            continue
        header, raw_name = entry.split(b"\t", 1)
        mode, blob, stage = header.decode("ascii").split()
        require(stage == "0" and mode in ("100644", "100755"), "regular merged source index required")
        relative = PurePosixPath(raw_name.decode("utf-8"))
        require(not relative.is_absolute() and ".." not in relative.parts, "invalid tracked path")
        path = root.joinpath(*relative.parts)
        require(path.resolve(strict=True).is_relative_to(root), "tracked source escaped checkout")
        data = file_bytes(path)
        actual = hashlib.sha1(b"blob " + str(len(data)).encode("ascii") + b"\0" + data).hexdigest()
        require(actual == blob, "tracked working bytes differ from index; use exact-LF private checkout")
        count += 1
    require(count > 0, "empty source index refused")
    lock = git(root, "show", ":Cargo.lock")
    return {
        "source_tree": text_git(root, "write-tree"),
        "cargo_lock_sha256": sha256(lock),
        "index_entries_sha256": sha256(entries),
        "tracked_files": count,
        "working_bytes_match_index": True,
    }


def validate_set(value):
    require(value.get("schema") == SET_SCHEMA, "patch-set schema differs")
    require(value.get("kind") in ("diagnostic", "corrected"), "explicit patch-set kind required")
    require(value.get("base_commit") == BASE and value.get("base_tree") == BASE_TREE, "patch-set base differs")
    require(isinstance(value.get("expected_tree"), str) and HEX40.fullmatch(value["expected_tree"]), "exact expected source tree required")
    require(value.get("cargo_lock_sha256") == BASE_LOCK, "exact canonical base lock hash required")
    patches = value.get("patches")
    require(isinstance(patches, list) and patches, "nonempty ordered patch set required")
    previous, names = BASE_TREE, set()
    for patch in patches:
        require(isinstance(patch, dict), "patch entry must be an object")
        name = patch.get("path")
        require(isinstance(name, str) and re.fullmatch(r"[a-z0-9][a-z0-9_.-]*\.patch", name), "patch must be a sibling filename")
        require(name not in names, "duplicate patch refused")
        names.add(name)
        require(isinstance(patch.get("sha256"), str) and HEX64.fullmatch(patch["sha256"]), "exact patch SHA256 required")
        require(patch.get("purpose") in ("production", "regression", "diagnostic"), "explicit patch purpose required")
        require(patch.get("tree_before") == previous, "patch tree order differs")
        require(isinstance(patch.get("tree_after"), str) and HEX40.fullmatch(patch["tree_after"]), "exact result tree required")
        if value["kind"] == "corrected":
            require(patch["purpose"] != "diagnostic", "diagnostic patch cannot qualify corrected artifact")
        previous = patch["tree_after"]
    require(previous == value["expected_tree"] and previous != BASE_TREE, "final patch tree differs or is unchanged")


def validate_build_receipt(value, artifact_sha, features, kind):
    """Validate pinned corrected/diagnostic metadata without opening a daemon."""
    require(isinstance(value, dict) and value.get("schema") == BUILD_SCHEMA, "corrected build schema required")
    require(value.get("source_kind") == "patched-candidate" and "source_commit" in value and value["source_commit"] is None, "patched source must not masquerade as a clean commit")
    require(kind in ("corrected", "diagnostic") and value.get("patch_set_kind") == kind, "selected candidate kind differs")
    require(value.get("sha256") == artifact_sha and HEX64.fullmatch(artifact_sha), "artifact SHA differs")
    require(type(value.get("bytes")) is int and value["bytes"] > 0, "artifact byte length required")
    require(value.get("default_features") is False and value.get("features") == features, "artifact features differ")
    require(features in ([], ["transport-lab"]), "unsupported explicit build features")
    for key in ("source_receipt_sha256", "patch_set_sha256"):
        require(isinstance(value.get(key), str) and HEX64.fullmatch(value[key]), "source/patch-set receipt hash missing")
    validate_set({
        "schema": SET_SCHEMA, "kind": value.get("patch_set_kind"),
        "base_commit": value.get("base_commit"), "base_tree": value.get("base_tree"),
        "expected_tree": value.get("source_tree"), "cargo_lock_sha256": value.get("cargo_lock_sha256"),
        "patches": value.get("patches"),
    })
    require(value.get("source_verified_after_build") is True, "verified post-build source evidence required")
    command = value.get("build_command")
    require(isinstance(command, dict), "build command evidence required")
    expected = ["cargo", "+1.88.0", "build", "--locked", "--release", "--no-default-features"]
    if features:
        expected += ["--features", "transport-lab"]
    expected += ["-p", "myownmesh", "--bin", "myownmesh"]
    require(command.get("argv") == expected, "exact selected build command evidence required")
    require(isinstance(command.get("cwd"), str) and command["cwd"].strip(), "build source directory evidence required")
    require(isinstance(command.get("receipt_sha256"), str) and HEX64.fullmatch(command["receipt_sha256"]), "complete build-command receipt hash required")
    native = value.get("native")
    require(isinstance(native, dict), "native build evidence required")
    for name in ("system", "machine"):
        require(isinstance(native.get(name), str) and native[name].strip(), "native system/architecture evidence required")
    tools = value.get("toolchain_evidence")
    require(isinstance(tools, dict), "native toolchain evidence required")
    for name in ("rustc", "cargo"):
        evidence = tools.get(name)
        require(isinstance(evidence, dict), "both native compiler and Cargo evidence required")
        require(type(evidence.get("bytes")) is int and evidence["bytes"] > 0, "positive toolchain evidence length required")
        require(isinstance(evidence.get("sha256"), str) and HEX64.fullmatch(evidence["sha256"]), "exact toolchain evidence hash required")
    return value


def prepare(args, root, output):
    manifest = selected_json(args.patch_set, args.patch_set_sha256)
    validate_set(manifest)
    initial = source_state(root)
    require(initial["source_tree"] == BASE_TREE, "prepare requires pristine exact-base index")
    require(initial["cargo_lock_sha256"] == manifest["cargo_lock_sha256"], "base lock hash differs")
    patch_inputs = []
    patch_root = args.patch_set.resolve(strict=True).parent
    for record in manifest["patches"]:
        path = patch_root / record["path"]
        require(path.resolve(strict=True).parent == patch_root, "patch path escaped selected directory")
        data = file_bytes(path)
        require(sha256(data) == record["sha256"], "selected patch bytes differ")
        patch_inputs.append((record, data))
    applied = []
    try:
        for record, data in patch_inputs:
            require(text_git(root, "write-tree") == record["tree_before"], "pre-apply tree differs")
            git(root, "apply", "--check", "--index", "--whitespace=error-all", "-", input_data=data)
            git(root, "apply", "--index", "--whitespace=error-all", "-", input_data=data)
            require(text_git(root, "write-tree") == record["tree_after"], "applied tree differs")
            applied.append(record)
        final = source_state(root)
        require(final["source_tree"] == manifest["expected_tree"], "final source tree differs")
        require(final["cargo_lock_sha256"] == initial["cargo_lock_sha256"], "patch changed the locked dependency graph")
        receipt = {
            "schema": SOURCE_SCHEMA, "result": "prepared", "source_kind": "patched-candidate",
            "source_commit": None, "base_commit": BASE, "base_tree": BASE_TREE,
            "patch_set_kind": manifest["kind"], "patch_set_sha256": args.patch_set_sha256,
            "patches": manifest["patches"], **final,
        }
        write_json(output, receipt)
        return receipt
    except BaseException:
        if not output.exists():
            write_json(output, {"schema": SOURCE_SCHEMA, "result": "failed", "base_commit": BASE,
                                "patch_set_sha256": args.patch_set_sha256, "applied_patches": applied,
                                "next_action": "inspect this exact private source copy and failure; do not blindly prepare again"})
        raise


def verified_source(args, root):
    receipt = selected_json(args.source_receipt, args.source_receipt_sha256)
    require(receipt.get("schema") == SOURCE_SCHEMA and receipt.get("result") == "prepared", "prepared source receipt required")
    require(receipt.get("source_kind") == "patched-candidate" and "source_commit" in receipt and receipt["source_commit"] is None, "patched source identity required")
    manifest = {
        "schema": SET_SCHEMA, "kind": receipt.get("patch_set_kind"),
        "base_commit": receipt.get("base_commit"), "base_tree": receipt.get("base_tree"),
        "expected_tree": receipt.get("source_tree"), "cargo_lock_sha256": receipt.get("cargo_lock_sha256"),
        "patches": receipt.get("patches"),
    }
    validate_set(manifest)
    require(isinstance(receipt.get("patch_set_sha256"), str) and HEX64.fullmatch(receipt["patch_set_sha256"]), "source patch-set hash missing")
    state = source_state(root)
    for key in state:
        require(receipt.get(key) == state[key], "source changed after preparation")
    return receipt


def record_build(args, root, output):
    source = verified_source(args, root)
    features = ["transport-lab"] if args.feature_set == "transport-lab" else []
    command_bytes = file_bytes(args.build_command)
    command = json.loads(command_bytes)
    require(isinstance(command, dict), "build command receipt object required")
    require(command.get("result") == "passed" and command.get("exit_code") == 0, "successful complete build command required")
    expected = ["cargo", "+1.88.0", "build", "--locked", "--release", "--no-default-features"]
    if features:
        expected += ["--features", "transport-lab"]
    expected += ["-p", "myownmesh", "--bin", "myownmesh"]
    require(command.get("argv") == expected, "exact selected Cargo build arguments required")
    require(Path(command.get("cwd", "")).resolve(strict=True) == root, "build command source directory differs")
    for stream in ("stdout", "stderr"):
        evidence = command.get(stream)
        require(isinstance(evidence, dict) and evidence.get("eof") is True, "complete build stream evidence required")
        require(isinstance(evidence.get("bytes"), int) and evidence["bytes"] >= 0, "build stream length missing")
        require(isinstance(evidence.get("sha256"), str) and HEX64.fullmatch(evidence["sha256"]), "build stream hash missing")
    artifact = file_bytes(args.artifact)
    require(artifact, "empty artifact refused")
    tools = {}
    for name, path in (("rustc", args.rustc_evidence), ("cargo", args.cargo_evidence)):
        value = file_bytes(path)
        require(value, "native toolchain evidence must be nonempty")
        tools[name] = {"bytes": len(value), "sha256": sha256(value)}
    receipt = {
        "schema": BUILD_SCHEMA, "source_kind": "patched-candidate", "source_commit": None,
        "base_commit": BASE, "base_tree": BASE_TREE, "source_tree": source["source_tree"],
        "patch_set_kind": source["patch_set_kind"], "patch_set_sha256": source["patch_set_sha256"],
        "patches": source["patches"], "source_receipt_sha256": args.source_receipt_sha256,
        "cargo_lock_sha256": source["cargo_lock_sha256"], "sha256": sha256(artifact), "bytes": len(artifact),
        "default_features": False, "features": features,
        "build_command": {"argv": expected, "cwd": str(root), "receipt_sha256": sha256(command_bytes)},
        "native": platform.uname()._asdict(), "toolchain_evidence": tools,
        "source_verified_after_build": True,
    }
    write_json(output, receipt)
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="operation", required=True)
    for operation in ("prepare", "verify", "record-build"):
        sub = commands.add_parser(operation)
        sub.add_argument("--upstream", type=Path, required=True)
        if operation == "prepare":
            sub.add_argument("--patch-set", type=Path, required=True)
            sub.add_argument("--patch-set-sha256", required=True)
        else:
            sub.add_argument("--source-receipt", type=Path, required=True)
            sub.add_argument("--source-receipt-sha256", required=True)
        if operation != "verify":
            sub.add_argument("--output", type=Path, required=True)
        if operation == "record-build":
            sub.add_argument("--artifact", type=Path, required=True)
            sub.add_argument("--build-command", type=Path, required=True)
            sub.add_argument("--feature-set", choices=("stock", "transport-lab"), required=True)
            sub.add_argument("--rustc-evidence", type=Path, required=True)
            sub.add_argument("--cargo-evidence", type=Path, required=True)
    args = parser.parse_args()
    root = args.upstream.resolve(strict=True)
    output = new_output(args.output, root) if args.operation != "verify" else None
    if args.operation == "prepare":
        result = prepare(args, root, output)
    elif args.operation == "record-build":
        result = record_build(args, root, output)
    else:
        source = verified_source(args, root)
        result = {"result": "verified", "source_tree": source["source_tree"],
                  "source_receipt_sha256": args.source_receipt_sha256}
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    try:
        main()
    except (Exception, KeyboardInterrupt) as error:
        print(json.dumps({"result": "failed", "error": type(error).__name__, "message": str(error)}), file=sys.stderr)
        raise SystemExit(1)

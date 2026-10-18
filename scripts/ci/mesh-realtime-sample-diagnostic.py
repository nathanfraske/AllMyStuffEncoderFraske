#!/usr/bin/env python3
"""Isolated sample-write diagnostic: one strict pair on Intel Mac or Windows.

Draft bindings deliberately fail closed until B1's actual diagnostic is frozen.
This runner never changes the ordinary corrected qualification entry point.
Command lifetimes are bounded by the registered workflow, without retries.
"""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import secrets
import shutil
import subprocess
import sys
import tempfile
import time

BASE = "db7818e09fedd98899490347b86ac9bc9f97b59b"
BASE_LOCK = "8f5a30ce863c43b0a8c5225f74d7446aed7656cd7032f47e70030526381930c6"
PREFIX_PATH = "scripts/ci/myownmesh-corrections/inbound-pump-custodian-provenance.json"
PREFIX_SHA256 = "21cd5d2084f3e6bc0b04fbf3052bafa904e3e451003cb0fe44288f83d87ac50c"
PREFIX_TREE = "87c0c6485787349c74f17f890c3dcf4b3f4c6e2d"
HELPER_PATH = "scripts/ci/myownmesh-corrections/prepare-candidate.py"
HELPER_SHA256 = "f698d79e564feb88294156b16f131ac0e10a5bb20ccd640c7550af7301ba2a20"
CONTROLLER_PATH = "scripts/ci/mesh-control-paired.py"
CONTROLLER_SHA256 = "5beed8e216bd9f59f320fd03c16c6895ce27f728905a7d02ad05be23522ce1ed"
POLICY_PATH = "scripts/ci/mesh-control-paired-policy.json"
POLICY_SHA256 = "e6729ca6d0887ec0b4ef2cd91da404ffb477b251e00a5a8c1381254c1a9a9fe6"

# Replace only from the actual independently frozen B1 packet, then review.
DIAGNOSTIC_MANIFEST = "scripts/ci/myownmesh-corrections/sample-write-provenance.json"
DIAGNOSTIC_MANIFEST_SHA256 = "89b106f03265bf0a1eb95f2ee35b414c2e6229b9ed9d93c22c5c16c171321011"
DIAGNOSTIC_TREE = "c61d0fbf3a07c528ca728a9c2a329e863fe12c67"
DIAGNOSTIC_PATCH = "sample-write-diagnostic.patch"
DIAGNOSTIC_PATCH_SHA256 = "95984ae5ee37bb6779a222e550946a642b44175d9ab927de8fef9baed5fb6b8d"
DIAGNOSTIC_SCOPE = "private funded sample-write decision observation only; scheduling may change; no strict AV, cause, repair or production qualification"

LOCKS = ["Cargo.lock", "node/Cargo.lock", "gui/src-tauri/Cargo.lock",
         "gui/mobile/Cargo.lock", "scripts/ci/mesh-realtime-minimal/Cargo.lock"]
LISTS = {
    "transport": {"paired_endpoint_ready", "isolated_realtime_pair"},
    "consumer": {"paired_endpoint_ready", "prepare_realtime_pair_vectors",
                 "isolated_realtime_consumer_pair"},
}
PROFILE = {"MYOWNMESH_LOG": "warn,mesh_first_unit=debug", "MYOWNMESH_LOG_FORMAT": "json"}
FAILURE_FLAG = {"ALLMYSTUFF_MESH_REALTIME_FAILURE_DIAGNOSTICS": "1"}


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def digest(path):
    sha = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            sha.update(block)
    return sha.hexdigest()


def identity(path):
    return {"path": str(path), "sha256": digest(path), "bytes": path.stat().st_size}


def write_json(path, value):
    with path.open("x", encoding="utf-8", newline="\n") as target:
        json.dump(value, target, indent=2)
        target.write("\n")


class Commands:
    def __init__(self, output):
        self.output, self.records = output, []

    def run(self, label, argv, cwd, env=None):
        stdout, stderr = [self.output / (label + suffix) for suffix in (".stdout", ".stderr")]
        record = {"label": label, "argv": [str(x) for x in argv], "cwd": str(cwd), "result": "pending"}
        if env is not None:
            record["selected_environment"] = {key: env[key] for key in
                ("CARGO_TARGET_DIR", "CMAKE_POLICY_VERSION_MINIMUM", "CARGO_TERM_COLOR",
                 "AMS_MESH_PAIRED_VECTOR_OUTPUT", "ALLMYSTUFF_MESH_REALTIME_FAILURE_DIAGNOSTICS")
                if key in env}
        self.records.append(record)
        write_json(self.output / (label + ".command.json"), record)
        print(json.dumps(record), flush=True)
        started = time.monotonic()
        with stdout.open("xb") as out, stderr.open("xb") as err:
            result = subprocess.run(record["argv"], cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                    stdout=out, stderr=err,
                                    creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
        record.update(exit_code=result.returncode, elapsed_seconds=time.monotonic() - started,
                      result="passed" if result.returncode == 0 else "failed",
                      stdout={**identity(stdout), "eof": True}, stderr={**identity(stderr), "eof": True})
        write_json(self.output / (label + ".complete.json"), record)
        require(result.returncode == 0, label + " failed; complete streams retained")
        return stdout


def patch_records(data):
    """Keep the exact JSON bytes of each patch object, as well as its value."""
    text = data.decode("utf-8")
    marker = '"patches": ['
    require(text.count(marker) == 1, "canonical patch-array spelling required")
    offset, decoder, records = text.index(marker) + len(marker), json.JSONDecoder(), []
    while True:
        while offset < len(text) and text[offset].isspace():
            offset += 1
        if text[offset] == "]":
            return records
        _, end = decoder.raw_decode(text, offset)
        records.append(text[offset:end].encode("utf-8"))
        offset = end
        while offset < len(text) and text[offset].isspace():
            offset += 1
        if text[offset] == ",":
            offset += 1
        else:
            require(text[offset] == "]", "invalid patch-array separator")


def frozen_bindings(args):
    require(re.fullmatch(r"[0-9a-f]{40}", args.expected_app), "exact future published application commit required")
    for value in (DIAGNOSTIC_MANIFEST_SHA256, DIAGNOSTIC_PATCH_SHA256):
        require(re.fullmatch(r"[0-9a-f]{64}", value), "actual B1 manifest and patch hashes remain unbound")
    require(re.fullmatch(r"[0-9a-f]{40}", DIAGNOSTIC_TREE), "actual B1 diagnostic tree remains unbound")
    require(DIAGNOSTIC_MANIFEST.startswith("scripts/ci/myownmesh-corrections/")
            and DIAGNOSTIC_MANIFEST.endswith(".json") and ".." not in Path(DIAGNOSTIC_MANIFEST).parts,
            "actual sibling diagnostic manifest path remains unbound")
    require(re.fullmatch(r"[a-z0-9][a-z0-9_.-]*\.patch", DIAGNOSTIC_PATCH),
            "actual B1 diagnostic patch filename remains unbound")
    require(DIAGNOSTIC_SCOPE and "PENDING" not in DIAGNOSTIC_SCOPE, "actual diagnostic scope remains unbound")
    require(args.patch_set == DIAGNOSTIC_MANIFEST and args.patch_set_sha256 == DIAGNOSTIC_MANIFEST_SHA256,
            "workflow must select the exact independently reviewed diagnostic manifest")


def copy_helper(root, output, commands, app_head):
    canonical = commands.run("canonical-helper", ["git", "--no-optional-locks", "show",
                             app_head + ":" + HELPER_PATH], root)
    require(digest(canonical) == HELPER_SHA256 and digest(root / HELPER_PATH) == HELPER_SHA256,
            "canonical GitLF helper and checkout must match the reviewed helper")
    directory = output / "helper"
    directory.mkdir()
    copied = directory / "prepare-candidate.py"
    with copied.open("xb") as target:
        target.write(canonical.read_bytes())
    require(digest(copied) == HELPER_SHA256, "fresh helper copy differs")
    spec = importlib.util.spec_from_file_location("mesh_sample_diagnostic_candidate", copied)
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    return copied, helper


def selected_manifest(root, args, helper):
    prefix_data = (root / PREFIX_PATH).read_bytes()
    require(hashlib.sha256(prefix_data).hexdigest() == PREFIX_SHA256, "corrected9 canonical prefix differs")
    prefix = json.loads(prefix_data)
    path = (root / DIAGNOSTIC_MANIFEST).resolve(strict=True)
    require(path.parent == (root / PREFIX_PATH).parent, "diagnostic manifest must share the canonical patch directory")
    selected = helper.selected_json(path, args.patch_set_sha256)
    helper.validate_set(selected)
    require(prefix["kind"] == "corrected" and len(prefix["patches"]) == 9
            and prefix["expected_tree"] == PREFIX_TREE, "corrected9 prefix identity differs")
    require(selected["kind"] == "diagnostic" and len(selected["patches"]) == 10
            and selected["expected_tree"] == DIAGNOSTIC_TREE
            and selected.get("qualification_scope") == DIAGNOSTIC_SCOPE,
            "only the frozen diagnostic10 source may enter this runner")
    require(selected["patches"][:9] == prefix["patches"]
            and patch_records(path.read_bytes())[:9] == patch_records(prefix_data),
            "the first nine canonical patch records must remain byte-exact")
    for key in ("schema", "base_commit", "base_tree", "cargo_lock_sha256"):
        require(selected[key] == prefix[key], "diagnostic prefix base identity differs")
    last = selected["patches"][9]
    require(last["purpose"] == "diagnostic" and last["path"] == DIAGNOSTIC_PATCH
            and last["sha256"] == DIAGNOSTIC_PATCH_SHA256 and last["tree_before"] == PREFIX_TREE
            and last["tree_after"] == DIAGNOSTIC_TREE, "only the frozen diagnostic patch may extend corrected9")
    protected = [root / PREFIX_PATH, path]
    for item in selected["patches"]:
        patch = path.parent / item["path"]
        require(patch.resolve(strict=True).parent == path.parent and digest(patch) == item["sha256"],
                "canonical sibling patch bytes differ")
        protected.append(patch)
    return path, selected, protected


def test_executable(cargo_json, target, target_root):
    matches = []
    for line in cargo_json.read_text(encoding="utf-8").splitlines():
        try:
            item = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (item.get("reason") == "compiler-artifact" and item.get("target", {}).get("name") == target
                and "test" in item.get("target", {}).get("kind", [])
                and item.get("profile", {}).get("test") is True and item.get("executable")):
            matches.append(Path(item["executable"]).resolve(strict=True))
    require(len(matches) == 1 and matches[0].is_file() and target_root in matches[0].parents,
            "one actual CargoJSON test executable in the private target is required")
    return matches[0]


def probe_inventory(commands, name, executable, root, env):
    listed = commands.run(name + "-probe-list", [executable, "--list"], root, env)
    names = re.findall(r"(?m)^(\S+): test\s*$", listed.read_text(encoding="utf-8"))
    require(len(names) == len(LISTS[name]) and set(names) == LISTS[name], "actual probe list differs")
    return {"executable": identity(executable), "test_names": sorted(names), "list": identity(listed)}


def remove_private_build(root, parent, owner):
    require(not root.is_symlink() and root.resolve() == root and root.parent == parent
            and root.name.startswith("am4sdb-"), "private build cleanup target escaped custody")
    require(json.loads((root / "custody.json").read_text(encoding="utf-8"))
            == {"owner": owner, "root": str(root)}, "private build cleanup marker differs")
    shutil.rmtree(root)
    require(not root.exists(), "private build state remains")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--expected-app", required=True)
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--probe-kind", choices=("transport", "consumer"), required=True)
    parser.add_argument("--patch-set", required=True)
    parser.add_argument("--patch-set-sha256", required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    upstream = args.upstream.resolve(strict=True)
    output = args.artifacts.absolute()
    output.mkdir(mode=0o700, exist_ok=False)
    commands, started = Commands(output), time.monotonic()
    receipt = {"result": "pending", "candidate_kind": "diagnostic", "candidate_qualification": "diagnostic_only",
               "candidate_base_commit": BASE, "expected_app": args.expected_app, "probe_kind": args.probe_kind,
               "native_uname": platform.uname()._asdict(), "python": sys.version,
               "python_executable": sys.executable, "diagnostic_log_profile": PROFILE,
               "app_failure_diagnostics": FAILURE_FLAG, "commands": commands.records}
    locks_before, protected_before, binaries_before = None, None, None
    helper_path, source_receipt, source_sha, pair_passed = None, None, None, False
    build, private, build_parent, owner = None, None, None, None
    try:
        frozen_bindings(args)
        require(platform.machine().lower() in ("amd64", "x86_64") and
                (platform.system(), args.probe_kind) in (("Darwin", "transport"), ("Windows", "consumer")),
                "only Intel Mac transport and Windows consumer are selected")
        app_head = commands.run("app-head", ["git", "--no-optional-locks", "rev-parse", "HEAD"], root).read_text().strip()
        native_head = commands.run("candidate-head", ["git", "--no-optional-locks", "rev-parse", "HEAD"], upstream).read_text().strip()
        require(app_head == args.expected_app and native_head == BASE, "actual checkout identity differs")
        commands.run("app-clean-before", ["git", "--no-optional-locks", "diff", "--exit-code", "HEAD", "--"], root)
        commands.run("candidate-clean-before", ["git", "--no-optional-locks", "diff", "--exit-code", "HEAD", "--"], upstream)
        require(digest(root / CONTROLLER_PATH) == CONTROLLER_SHA256 and digest(root / POLICY_PATH) == POLICY_SHA256,
                "the original strict controller or explicit policy differs")
        helper_path, helper = copy_helper(root, output, commands, app_head)
        manifest_path, manifest, protected = selected_manifest(root, args, helper)
        protected += [helper_path, root / HELPER_PATH, root / CONTROLLER_PATH, root / POLICY_PATH,
                      Path(__file__).resolve(), root / ".github/workflows/mesh04-sample-diagnostic.yml"]
        protected_before = {str(path): identity(path) for path in protected}
        locks_before = {path: digest(root / path) for path in LOCKS}
        require(digest(upstream / "Cargo.lock") == BASE_LOCK, "native pristine lock differs")
        receipt.update(actual_app_commit=app_head, patch_set_sha256=args.patch_set_sha256, patch_set=manifest,
                       qualification_scope=DIAGNOSTIC_SCOPE, locks_before=locks_before,
                       protected_inputs_before=protected_before)
        source_receipt = output / "candidate-source.json"
        commands.run("diagnostic-candidate-prepare", [sys.executable, helper_path, "prepare", "--upstream", upstream,
                     "--patch-set", manifest_path, "--patch-set-sha256", args.patch_set_sha256,
                     "--output", source_receipt], root)
        source_sha = digest(source_receipt)
        source = helper.selected_json(source_receipt, source_sha)
        require(source["result"] == "prepared" and source["source_commit"] is None
                and source["patch_set_kind"] == "diagnostic" and source["source_tree"] == DIAGNOSTIC_TREE
                and source["patch_set_sha256"] == args.patch_set_sha256,
                "official source receipt differs from the selected diagnostic")
        receipt.update(candidate_source_receipt=identity(source_receipt), candidate_source=source)
        for label, argv in [
            ("app-rustc", ["rustc", "+stable", "-vV"]),
            ("app-cargo", ["cargo", "+stable", "--version"]),
            ("candidate-rustc", ["rustc", "+1.88.0", "-vV"]),
            ("candidate-cargo", ["cargo", "+1.88.0", "--version"]),
            ("cmake", ["cmake", "--version"]),
        ]:
            commands.run(label, argv, root)
        build_parent = Path(tempfile.gettempdir()).resolve(strict=True)
        build = Path(tempfile.mkdtemp(prefix="am4sdb-", dir=build_parent)).resolve()
        private = Path(tempfile.mkdtemp(prefix="am4sdp-", dir=None if os.name == "nt" else "/tmp")).resolve()
        owner = secrets.token_hex(16)
        write_json(build / "custody.json", {"owner": owner, "root": str(build)})
        receipt.update(build_root=str(build), private_parent=str(private))
        env = dict(os.environ, CMAKE_POLICY_VERSION_MINIMUM="3.5", CARGO_TERM_COLOR="never")
        node_env, app_env, native_env = [dict(env, CARGO_TARGET_DIR=str(build / name))
                                        for name in ("node", "app", "daemon")]
        receipt["build_environment"] = {"CMAKE_POLICY_VERSION_MINIMUM": "3.5", "CARGO_TERM_COLOR": "never",
                                        "node_target": node_env["CARGO_TARGET_DIR"],
                                        "app_target": app_env["CARGO_TARGET_DIR"] if args.probe_kind == "transport" else None,
                                        "daemon_target": native_env["CARGO_TARGET_DIR"]}
        node_json = commands.run("node-probe-build", ["cargo", "+stable", "test", "--manifest-path", "node/Cargo.toml",
                                 "--no-default-features", "--features", "mesh-realtime", "--test", "mesh_realtime_paired",
                                 "--no-run", "--message-format=json", "--locked"], root, node_env)
        node_probe = test_executable(node_json, "mesh_realtime_paired", build / "node")
        inventories = {"consumer": probe_inventory(commands, "consumer", node_probe, root, node_env)}
        selected_probe = node_probe
        if args.probe_kind == "transport":
            app_json = commands.run("transport-probe-build", ["cargo", "+stable", "test", "-p", "allmystuff-mesh-control",
                                    "--features", "realtime", "--test", "realtime_paired", "--no-run",
                                    "--message-format=json", "--locked"], root, app_env)
            selected_probe = test_executable(app_json, "realtime_paired", build / "app")
            inventories["transport"] = probe_inventory(commands, "transport", selected_probe, root, app_env)
        receipt["probe_inventories"] = inventories
        commands.run("diagnostic-source-before-build", [sys.executable, helper_path, "verify", "--upstream", upstream,
                     "--source-receipt", source_receipt, "--source-receipt-sha256", source_sha], root)
        commands.run("diagnostic-candidate-build", ["cargo", "+1.88.0", "build", "--locked", "--release",
                     "--no-default-features", "-p", "myownmesh", "--bin", "myownmesh"], upstream, native_env)
        daemon = build / "daemon/release" / ("myownmesh.exe" if os.name == "nt" else "myownmesh")
        build_receipt = output / "daemon-build.json"
        commands.run("diagnostic-candidate-record-build", [sys.executable, helper_path, "record-build", "--upstream", upstream,
                     "--source-receipt", source_receipt, "--source-receipt-sha256", source_sha,
                     "--build-command", output / "diagnostic-candidate-build.complete.json", "--artifact", daemon,
                     "--feature-set", "stock", "--rustc-evidence", output / "candidate-rustc.stdout",
                     "--cargo-evidence", output / "candidate-cargo.stdout", "--output", build_receipt], root)
        build_sha = digest(build_receipt)
        recorded = helper.selected_json(build_receipt, build_sha)
        helper.validate_build_receipt(recorded, digest(daemon), [], "diagnostic")
        require(recorded["bytes"] == daemon.stat().st_size and recorded["source_receipt_sha256"] == source_sha
                and recorded["patch_set_sha256"] == args.patch_set_sha256 and recorded["source_tree"] == DIAGNOSTIC_TREE,
                "official build receipt differs from actual source and binary")
        receipt.update(candidate_build_receipt=identity(build_receipt), candidate_build=recorded)
        vectors = output / "vectors"
        commands.run("valid-codec-vectors", [node_probe, "--ignored", "--exact", "prepare_realtime_pair_vectors",
                     "--nocapture", "--test-threads=1"], root, dict(node_env, AMS_MESH_PAIRED_VECTOR_OUTPUT=str(vectors)))
        receipt["vectors_manifest"] = identity(vectors / "vectors.json")
        binaries_before = {str(path): identity(path) for path in {node_probe, selected_probe, daemon}}
        receipt["binaries_before"] = binaries_before
        pair_output = output / (args.probe_kind + "-pair")
        pair_argv = [sys.executable, root / CONTROLLER_PATH, "--daemon", daemon, "--daemon-sha256", digest(daemon),
                     "--daemon-build-receipt", build_receipt, "--candidate-kind", "diagnostic",
                     "--daemon-build-receipt-sha256", build_sha, "--probe", selected_probe,
                     "--probe-sha256", digest(selected_probe), "--probe-kind", args.probe_kind,
                     "--app-commit", app_head, "--policy", root / POLICY_PATH, "--vectors", vectors / "vectors.json",
                     "--private-parent", private, "--output", pair_output, "--signaling-port", "18475", "--mode", "direct"]
        receipt["pair_binding_basis"] = {"actual_app_commit": app_head, "actual_source_receipt": identity(source_receipt),
                                         "actual_build_receipt": identity(build_receipt), "actual_daemon": identity(daemon),
                                         "actual_probe": identity(selected_probe), "argv": [str(x) for x in pair_argv]}
        pair_receipt = pair_output / "receipt.json"
        # Exactly one controller call: no retry or second probe-kind dispatch.
        # Retain the original receipt on failure too, without another request.
        try:
            commands.run(args.probe_kind + "-pair", pair_argv, root, dict(env, **FAILURE_FLAG))
        finally:
            if pair_receipt.is_file():
                receipt["pair_receipt"] = identity(pair_receipt)
        paired = json.loads(pair_receipt.read_text(encoding="utf-8"))
        require(paired["result"] == "passed" and not paired["cleanup_errors"]
                and paired["cleanup"] == "owned_root_removed_after_all_children_reaped",
                "strict pair or complete owned cleanup failed")
        require(paired["candidate_kind"] == "diagnostic" and paired["candidate_qualification"] == "diagnostic_only"
                and paired["diagnostic_log_profile"] == PROFILE, "controller diagnostic profile differs")
        require(os.name == "nt" or paired["normal_shutdown"] is True, "normal Unix child shutdown is required")
        pair_passed = True
        receipt["result"] = "passed"
    except (Exception, KeyboardInterrupt) as error:
        receipt.update(result="failed", failure={"type": type(error).__name__, "message": str(error)})
    finally:
        try:
            if locks_before is not None:
                after = {path: digest(root / path) for path in LOCKS}
                receipt["locks_after"] = after
                require(after == locks_before and digest(upstream / "Cargo.lock") == BASE_LOCK,
                        "a locked command changed source lock bytes")
                commands.run("app-clean-after", ["git", "--no-optional-locks", "diff", "--exit-code", "HEAD", "--"], root)
                head_after = commands.run("app-head-after", ["git", "--no-optional-locks", "rev-parse", "HEAD"], root).read_text().strip()
                require(head_after == args.expected_app, "application checkout changed")
            if source_sha is not None:
                commands.run("diagnostic-source-after", [sys.executable, helper_path, "verify", "--upstream", upstream,
                             "--source-receipt", source_receipt, "--source-receipt-sha256", source_sha], root)
            if protected_before is not None:
                after = {path: identity(Path(path)) for path in protected_before}
                receipt["protected_inputs_after"] = after
                require(after == protected_before, "protected diagnostic inputs changed")
            if binaries_before is not None:
                after = {path: identity(Path(path)) for path in binaries_before}
                receipt["binaries_after"] = after
                require(after == binaries_before, "actual probes or daemon changed")
            if receipt["result"] == "passed" and pair_passed:
                private.rmdir()
                remove_private_build(build, build_parent, owner)
                receipt["driver_cleanup"] = "empty_pair_parent_and_owned_build_state_removed"
            else:
                receipt["driver_cleanup"] = "state_preserved_after_failed_or_incomplete_gate"
        except (Exception, KeyboardInterrupt) as error:
            receipt.update(result="failed", final_identity_failure=type(error).__name__,
                           driver_cleanup="inspect_exact_state_after_failed_final_verification")
        receipt["elapsed_seconds"] = time.monotonic() - started
        write_json(output / "receipt.json", receipt)
        print(json.dumps({"result": receipt["result"], "receipt": str(output / "receipt.json")}), flush=True)
    return 0 if receipt["result"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Focused hosted native gates; actual process/endpoint custody is B2's harness.

No installed applications, services, default endpoints or dependency updates.
The registered workflow bounds the build step. A cancelled/incomplete driver
has no passing receipt; paired children have their own finite owned cleanup.
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

CANDIDATE = "db7818e09fedd98899490347b86ac9bc9f97b59b"
CORRECTED_PATCH_SET = "4cb553e829e61dab2858357b1a2c031a2cf5995d0e49eaa9770517c345a497e6"
CORRECTED_TREE = "bdbf1b96c7809107bd3a151b2ccf9bcad9ace73a"
CORRECTED_SCOPE = "owner-cleanup and startup-first-bind source; strict native AV gates pending; sender-reuse unqualified"
POLICY = "e6729ca6d0887ec0b4ef2cd91da404ffb477b251e00a5a8c1381254c1a9a9fe6"
LOCKS = ["Cargo.lock", "node/Cargo.lock", "gui/src-tauri/Cargo.lock",
         "gui/mobile/Cargo.lock", "scripts/ci/mesh-realtime-minimal/Cargo.lock"]
CONTRACT_TESTS = frozenset([
    "h264_and_opus_keep_literal_open_pipe_and_unit_fields",
    "inbound_keeps_pipe_context_binary_label_marker_and_wrapping_rtp_timestamp",
    "local_send_size_refusal_preserves_the_healthy_frame_boundary",
    "retained_inbound_unit_holds_the_shared_frame_budget_until_drop",
    "invalid_inbound_lengths_and_marker_end_the_pipe_without_resynchronizing",
    "oversized_inbound_prefix_refuses_before_waiting_for_its_body",
    "cancelled_partial_receive_closes_the_socket_and_cannot_reuse_a_prefix",
    "partial_receive_timeout_is_terminal_and_releases_its_reservation",
    "outbound_terminal_observer_sees_remote_eof_without_another_send",
    "dropped_flow_keeps_its_exact_cleanup_owner_and_capacity_charge",
    "inbound_capacity_returns_after_explicit_pipe_close",
    "utf8_label_limit_counts_wire_bytes_without_truncation",
    "known_flow_is_closed_once_when_outbound_pipe_setup_refuses",
    "cancelled_open_tracks_late_capability_until_one_acknowledged_close",
    "unknown_open_capability_is_uncertain_and_fences_label_retry",
    "refusal_codes_survive_without_exposing_client_or_known_flow_secrets",
    "owner_close_attempts_every_known_flow_and_reports_unacknowledged_retirement",
    "inbound_direction_cannot_send_or_acquire_an_outbound_pipe",
    "another_client_cannot_adopt_an_owned_event_session",
    "event_replacement_fences_old_io_and_cleanup_keeps_the_new_owner_alive",
    "cancelled_close_is_uncertain_without_retrying_the_consumed_capability",
    "impossible_body_budget_and_deadline_policies_refuse_before_transport",
])
NODE_TESTS = frozenset("mesh_realtime::tests::" + name for name in [
    "realtime_context_requires_two_accepted_explicit_routes",
    "unrelated_peer_and_crossed_direction_cannot_share_an_av_binding",
    "teardown_and_same_id_replacement_end_the_bound_application_epoch",
    "opus_rtp_family_is_independent_of_mono_software_pcm",
])


def digest(path):
    sha = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            sha.update(block)
    return sha.hexdigest()


def write_json(path, value):
    with path.open("x", encoding="utf-8") as target:
        json.dump(value, target, indent=2)
        target.write("\n")


def correction_helper(root):
    path = root / "scripts/ci/myownmesh-corrections/prepare-candidate.py"
    spec = importlib.util.spec_from_file_location("mesh_corrected_candidate", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return path, module


class Commands:
    def __init__(self, artifacts):
        self.artifacts = artifacts
        self.records = []

    def run(self, label, argv, cwd, env=None):
        stdout = self.artifacts / (label + ".stdout")
        stderr = self.artifacts / (label + ".stderr")
        record = {"label": label, "argv": [str(x) for x in argv],
                  "cwd": str(cwd), "result": "pending"}
        self.records.append(record)
        write_json(self.artifacts / (label + ".command.json"), record)
        print(json.dumps(record), flush=True)
        started = time.monotonic()
        # No shell interpolation. Build lifetime is bounded by the workflow
        # step/job; a killed build never becomes a successful terminal receipt.
        with stdout.open("xb") as out, stderr.open("xb") as err:
            result = subprocess.run(record["argv"], cwd=cwd, env=env,
                                    stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                                    creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
        record.update(exit_code=result.returncode, elapsed_seconds=time.monotonic() - started,
                      result="passed" if result.returncode == 0 else "failed",
                      stdout={"bytes": stdout.stat().st_size, "sha256": digest(stdout), "eof": True},
                      stderr={"bytes": stderr.stat().st_size, "sha256": digest(stderr), "eof": True})
        write_json(self.artifacts / (label + ".complete.json"), record)
        if result.returncode != 0:
            raise RuntimeError(label + " failed; complete streams retained")
        return stdout


def test_executable(json_path, target):
    matches = []
    for line in json_path.read_text(encoding="utf-8").splitlines():
        try:
            item = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (item.get("reason") == "compiler-artifact"
                and item.get("target", {}).get("name") == target
                and item.get("profile", {}).get("test") is True
                and item.get("executable")):
            matches.append(Path(item["executable"]))
    if len(matches) != 1 or not matches[0].is_file():
        raise RuntimeError("expected one exact Cargo test executable for " + target)
    return matches[0].resolve()


def selected_test_result(path, expected_names):
    text = path.read_text(encoding="utf-8")
    summaries = re.findall(r"(?m)^test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;", text)
    names = re.findall(r"(?m)^test (\S+) \.\.\. ok\s*$", text)
    if len(summaries) != 1:
        raise RuntimeError("expected one complete selected Rust test result")
    passed, failed, ignored = map(int, summaries[0])
    if (passed != len(expected_names) or failed or ignored or len(names) != passed
            or len(set(names)) != passed or set(names) != expected_names):
        raise RuntimeError("selected Rust test count/names/refusal evidence is incomplete")
    return {"passed": passed, "failed": failed, "ignored": ignored, "names": names}


def verify_minimal_graph(path):
    metadata = json.loads(path.read_text(encoding="utf-8"))
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    packages = {package["id"]: package for package in metadata["packages"]}
    reached, pending = set(), [metadata["resolve"]["root"]]
    while pending:
        identity = pending.pop()
        if identity in reached:
            continue
        reached.add(identity)
        for dependency in nodes[identity]["deps"]:
            if any(kind["kind"] in (None, "build") for kind in dependency["dep_kinds"]):
                pending.append(dependency["pkg"])
    names = {packages[identity]["name"] for identity in reached}
    forbidden = {"allmystuff-node", "allmystuff-audio", "allmystuff-video",
                 "allmystuff-pixels", "allmystuff-inventory", "allmystuff-terminal",
                 "openh264", "openh264-sys2", "opus", "audiopus_sys", "xcap", "cpal",
                 "tauri", "wry", "enigo", "clipboard-rs"}
    if names & forbidden:
        raise RuntimeError("minimal normal/build graph acquired application or codec tooling")
    control = [identity for identity in reached
               if packages[identity]["name"] == "allmystuff-mesh-control"]
    if len(control) != 1 or "realtime" not in nodes[control[0]]["features"]:
        raise RuntimeError("minimal consumer did not select the actual realtime adapter")
    return {"normal_build_packages": sorted(names), "realtime_feature_selected": True}


def remove_private_build(root, parent, owner):
    # Resolve and check the final absolute target before recursive deletion,
    # including Windows. The artifact directory and checkout are never targets.
    if (root.is_symlink() or root.resolve() != root or root.parent != parent
            or not root.name.startswith("am3b-")):
        raise RuntimeError("private build path custody changed")
    if json.loads((root / "custody.json").read_text()) != {"owner": owner, "root": str(root)}:
        raise RuntimeError("private build marker changed")
    shutil.rmtree(root)
    if root.exists():
        raise RuntimeError("private build state remains")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--expected-app", required=True)
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--candidate-kind", choices=("stock", "corrected"), default="stock")
    parser.add_argument("--patch-set", type=Path)
    parser.add_argument("--patch-set-sha256")
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40}", args.expected_app):
        parser.error("the exact task-ref application SHA is required")
    if args.candidate_kind == "corrected":
        if (args.patch_set is None or args.patch_set_sha256 is None
                or not re.fullmatch(r"[0-9a-f]{64}", args.patch_set_sha256)):
            parser.error("corrected mode requires the exact patch set and its SHA256")
    elif args.patch_set is not None or args.patch_set_sha256 is not None:
        parser.error("patch-set inputs require explicit corrected mode")
    root = Path(__file__).resolve().parents[2]
    upstream = args.upstream.resolve(strict=True)
    output = args.artifacts.absolute()
    output.mkdir(mode=0o700, exist_ok=False)
    commands = Commands(output)
    receipt = {"result": "pending", "expected_app": args.expected_app,
               "candidate": CANDIDATE, "policy_sha256": POLICY,
               "native_uname": platform.uname()._asdict(), "python": sys.version,
               "python_executable": sys.executable, "commands": commands.records}
    before, upstream_lock = None, None
    build, private, build_parent, owner = None, None, None, None
    helper_path, helper, source_receipt, source_sha = None, None, None, None
    started = time.monotonic()
    try:
        if args.candidate_kind == "corrected":
            helper_path, helper = correction_helper(root)
            patch_set = helper.selected_json(args.patch_set, args.patch_set_sha256)
            helper.validate_set(patch_set)
            if patch_set["kind"] != "corrected":
                raise RuntimeError("diagnostic patch sets cannot enter corrected qualification")
            if (args.patch_set_sha256 != CORRECTED_PATCH_SET
                    or patch_set["expected_tree"] != CORRECTED_TREE
                    or patch_set.get("qualification_scope") != CORRECTED_SCOPE):
                raise RuntimeError("corrected matrix requires the exact reviewed owner-cleanup and startup patch set")
            receipt.update(candidate=None, candidate_kind="corrected", candidate_base_commit=CANDIDATE,
                           candidate_qualification_scope=CORRECTED_SCOPE,
                           patch_set_sha256=args.patch_set_sha256, patch_set=patch_set)
        machine = platform.machine().lower()
        if machine not in ("amd64", "x86_64") or platform.system() not in ("Windows", "Linux", "Darwin"):
            raise RuntimeError("the focused native matrix requires x86_64 Windows/Linux/Intel macOS")
        if platform.system() == "Linux" and "microsoft" in platform.release().lower():
            raise RuntimeError("WSL is not the requested native Linux gate")
        app_head = commands.run("app-head", ["git", "rev-parse", "HEAD"], root).read_text().strip()
        candidate_head = commands.run("candidate-head", ["git", "rev-parse", "HEAD"], upstream).read_text().strip()
        if app_head != args.expected_app or candidate_head != CANDIDATE:
            raise RuntimeError("checkout identity mismatch")
        commands.run("app-source-clean-before", ["git", "diff", "--exit-code", "HEAD", "--"], root)
        commands.run("candidate-source-clean-before", ["git", "diff", "--exit-code", "HEAD", "--"], upstream)
        policy_bytes = (root / "scripts/ci/mesh-control-paired-policy.json").read_bytes()
        receipt["policy_raw_sha256"] = hashlib.sha256(policy_bytes).hexdigest()
        receipt["policy_lf_sha256"] = hashlib.sha256(policy_bytes.replace(b"\r\n", b"\n")).hexdigest()
        if receipt["policy_lf_sha256"] != POLICY:
            raise RuntimeError("fixture policy differs from the explicit owner selection")
        before = {path: digest(root / path) for path in LOCKS}
        upstream_lock = digest(upstream / "Cargo.lock")
        receipt["locks_before"] = before
        receipt["candidate_lock_before"] = upstream_lock
        if args.candidate_kind == "corrected":
            source_receipt = output / "candidate-source.json"
            commands.run("corrected-candidate-prepare", [sys.executable, str(helper_path), "prepare",
                         "--upstream", str(upstream), "--patch-set", str(args.patch_set.resolve(strict=True)),
                         "--patch-set-sha256", args.patch_set_sha256, "--output", str(source_receipt)], root)
            source_sha = digest(source_receipt)
            receipt.update(candidate_source_receipt_sha256=source_sha,
                           candidate_source=json.loads(source_receipt.read_text(encoding="utf-8")))
        for label, argv in [
            ("app-rustc", ["rustc", "+stable", "-vV"]),
            ("app-cargo", ["cargo", "+stable", "--version"]),
            ("candidate-rustc", ["rustc", "+1.88.0", "-vV"]),
            ("candidate-cargo", ["cargo", "+1.88.0", "--version"]),
            ("cmake", ["cmake", "--version"]),
        ]:
            commands.run(label, argv, root)
        # Task-only build targets. No shared installed application state.
        build_parent = Path(tempfile.gettempdir()).resolve(strict=True)
        build = Path(tempfile.mkdtemp(prefix="am3b-", dir=build_parent)).resolve()
        private = Path(tempfile.mkdtemp(prefix="am3p-", dir=None if os.name == "nt" else "/tmp")).resolve()
        owner = secrets.token_hex(16)
        write_json(build / "custody.json", {"owner": owner, "root": str(build)})
        receipt.update(build_root=str(build), private_parent=str(private))
        env = dict(os.environ, CMAKE_POLICY_VERSION_MINIMUM="3.5", CARGO_TERM_COLOR="never")
        app_env = dict(env, CARGO_TARGET_DIR=str(build / "app"))
        node_env = dict(env, CARGO_TARGET_DIR=str(build / "node"))
        daemon_env = dict(env, CARGO_TARGET_DIR=str(build / "daemon"))
        receipt["build_environment"] = {"CMAKE_POLICY_VERSION_MINIMUM": "3.5", "CARGO_TERM_COLOR": "never",
                                        "app_target": app_env["CARGO_TARGET_DIR"],
                                        "node_target": node_env["CARGO_TARGET_DIR"],
                                        "daemon_target": daemon_env["CARGO_TARGET_DIR"]}
        minimal = "scripts/ci/mesh-realtime-minimal/Cargo.toml"
        for label, extra in [("minimal-off", []), ("minimal-realtime", ["--features", "realtime"])]:
            commands.run(label, ["cargo", "+stable", "check", "--manifest-path", minimal,
                                "--no-default-features", "--locked", *extra], root, app_env)
        graph = commands.run("minimal-graph", ["cargo", "+stable", "metadata", "--manifest-path", minimal,
                             "--no-default-features", "--features", "realtime", "--locked", "--format-version", "1"], root, app_env)
        receipt["minimal_graph"] = verify_minimal_graph(graph)
        contract_log = commands.run("realtime-contract", ["cargo", "+stable", "test", "-p", "allmystuff-mesh-control",
                     "--features", "realtime", "--test", "realtime_contract", "--locked", "--", "--test-threads=1"], root, app_env)
        policy_log = commands.run("node-route-policy", ["cargo", "+stable", "test", "--manifest-path", "node/Cargo.toml",
                     "--no-default-features", "--features", "mesh-realtime", "--lib", "--locked",
                     "mesh_realtime::tests::", "--", "--test-threads=1"], root, node_env)
        receipt["selected_tests"] = {"realtime_contract": selected_test_result(contract_log, CONTRACT_TESTS),
                                     "node_route_policy": selected_test_result(policy_log, NODE_TESTS)}
        bare_json = commands.run("transport-probe-build", ["cargo", "+stable", "test", "-p", "allmystuff-mesh-control",
                                 "--features", "realtime", "--test", "realtime_paired", "--no-run",
                                 "--message-format=json", "--locked"], root, app_env)
        node_json = commands.run("consumer-probe-build", ["cargo", "+stable", "test", "--manifest-path", "node/Cargo.toml",
                                 "--no-default-features", "--features", "mesh-realtime", "--test", "mesh_realtime_paired",
                                 "--no-run", "--message-format=json", "--locked"], root, node_env)
        bare_probe = test_executable(bare_json, "realtime_paired")
        node_probe = test_executable(node_json, "mesh_realtime_paired")
        if args.candidate_kind == "corrected":
            commands.run("corrected-candidate-source-before-build", [sys.executable, str(helper_path), "verify",
                         "--upstream", str(upstream), "--source-receipt", str(source_receipt),
                         "--source-receipt-sha256", source_sha], root)
        build_label = "stock-candidate-build" if args.candidate_kind == "stock" else "corrected-candidate-build"
        commands.run(build_label, ["cargo", "+1.88.0", "build", "--locked", "--release",
                     "--no-default-features", "-p", "myownmesh", "--bin", "myownmesh"], upstream, daemon_env)
        daemon = build / "daemon/release" / ("myownmesh.exe" if os.name == "nt" else "myownmesh")
        build_receipt = output / "daemon-build.json"
        pair_identity_args = []
        if args.candidate_kind == "stock":
            write_json(build_receipt, {"source_commit": CANDIDATE, "sha256": digest(daemon),
                                      "features": [], "default_features": False})
        else:
            commands.run("corrected-candidate-record-build", [sys.executable, str(helper_path), "record-build",
                         "--upstream", str(upstream), "--source-receipt", str(source_receipt),
                         "--source-receipt-sha256", source_sha,
                         "--build-command", str(output / (build_label + ".complete.json")),
                         "--artifact", str(daemon), "--feature-set", "stock",
                         "--rustc-evidence", str(output / "candidate-rustc.stdout"),
                         "--cargo-evidence", str(output / "candidate-cargo.stdout"),
                         "--output", str(build_receipt)], root)
            build_sha = digest(build_receipt)
            recorded = helper.selected_json(build_receipt, build_sha)
            helper.validate_build_receipt(recorded, digest(daemon), [], "corrected")
            if (recorded["bytes"] != daemon.stat().st_size
                    or recorded["source_receipt_sha256"] != source_sha
                    or recorded["patch_set_sha256"] != args.patch_set_sha256
                    or recorded["source_tree"] != patch_set["expected_tree"]):
                raise RuntimeError("corrected build receipt differs from the selected source or artifact")
            receipt.update(candidate_build_receipt_sha256=build_sha, candidate_build=recorded)
            pair_identity_args = ["--candidate-kind", "corrected", "--daemon-build-receipt-sha256", build_sha]
        vectors = output / "vectors"
        vector_env = dict(node_env, AMS_MESH_PAIRED_VECTOR_OUTPUT=str(vectors))
        commands.run("valid-codec-vectors", [str(node_probe), "--ignored", "--exact", "prepare_realtime_pair_vectors",
                     "--nocapture", "--test-threads=1"], root, vector_env)
        receipt["payload_manifest_sha256"] = digest(vectors / "vectors.json")
        for kind, probe in [("transport", bare_probe), ("consumer", node_probe)]:
            commands.run(kind + "-pair", [sys.executable, str(root / "scripts/ci/mesh-control-paired.py"),
                         "--daemon", str(daemon), "--daemon-sha256", digest(daemon),
                         "--daemon-build-receipt", str(build_receipt), *pair_identity_args,
                         "--probe", str(probe), "--probe-sha256", digest(probe), "--probe-kind", kind,
                         "--app-commit", app_head, "--policy", str(root / "scripts/ci/mesh-control-paired-policy.json"),
                         "--vectors", str(vectors / "vectors.json"), "--private-parent", str(private),
                         "--output", str(output / (kind + "-pair")), "--signaling-port", "18475", "--mode", "direct"], root, env)
            paired = json.loads((output / (kind + "-pair/receipt.json")).read_text())
            if (paired["result"] != "passed" or paired["cleanup_errors"]
                    or paired["cleanup"] != "owned_root_removed_after_all_children_reaped"):
                raise RuntimeError("paired cleanup did not establish complete owned shutdown")
            if os.name != "nt" and not paired["normal_shutdown"]:
                raise RuntimeError("normal Unix daemon shutdown was not established")
        receipt["result"] = "passed"
    except (Exception, KeyboardInterrupt) as error:
        receipt.update(result="failed", failure={"type": type(error).__name__, "message": str(error)})
    finally:
        try:
            if before is not None:
                after = {path: digest(root / path) for path in LOCKS}
                receipt["locks_after"] = after
                if after != before or digest(upstream / "Cargo.lock") != upstream_lock:
                    raise RuntimeError("a locked command mutated a reviewed lockfile")
                commands.run("app-source-clean-after", ["git", "diff", "--exit-code", "HEAD", "--"], root)
                if args.candidate_kind == "stock":
                    commands.run("candidate-source-clean-after", ["git", "diff", "--exit-code", "HEAD", "--"], upstream)
                elif source_sha is not None:
                    commands.run("corrected-candidate-source-after", [sys.executable, str(helper_path), "verify",
                                 "--upstream", str(upstream), "--source-receipt", str(source_receipt),
                                 "--source-receipt-sha256", source_sha], root)
            if receipt["result"] == "passed":
                # Both controllers have proved their roots removed and all
                # children reaped. This parent must now be exactly empty.
                private.rmdir()
                remove_private_build(build, build_parent, owner)
                receipt["driver_cleanup"] = "empty_pair_parent_and_owned_build_state_removed"
            else:
                receipt["driver_cleanup"] = "state_preserved_after_failed_or_incomplete_gate"
        except Exception as error:
            receipt.update(result="failed", final_identity_failure=type(error).__name__)
        receipt["elapsed_seconds"] = time.monotonic() - started
        write_json(output / "receipt.json", receipt)
        print(json.dumps({"result": receipt["result"], "receipt": str(output / "receipt.json")}), flush=True)
    return 0 if receipt["result"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())

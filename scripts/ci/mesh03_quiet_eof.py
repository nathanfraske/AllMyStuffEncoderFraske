#!/usr/bin/env python3
"""Manager-only raw-wire quiet Events EOF diagnostic for MyOwnMesh db7818e.

Use only explicitly supplied checksum-pinned binaries and newly owned state.
No endpoint discovery, real flow, installed service, upstream mutation or retry.
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
import subprocess
import sys
import tempfile
import time


SOURCE = "db7818e09fedd98899490347b86ac9bc9f97b59b"
BASE_TREE = "5492d1845e15dbc067acec3ec592bb9500b9e21d"
BASE_LOCK_LF_SHA256 = "8f5a30ce863c43b0a8c5225f74d7446aed7656cd7032f47e70030526381930c6"
TEST = "isolated_quiet_events_eof"
REVOKED_TEST = "isolated_owner_revocation"
HELPER_LF_SHA256 = "82dece01deba45027125cc9fdff1d3f64f0358d93a1d5486e7bd075ee110829d"


def helpers():
    path = Path(__file__).resolve().with_name("mesh-control-isolated.py")
    source = path.read_bytes().replace(b"\r\n", b"\n")
    if path.is_symlink() or hashlib.sha256(source).hexdigest() != HELPER_LF_SHA256:
        raise ValueError("private helper differs from the reviewed source")
    spec = importlib.util.spec_from_file_location("mesh_control_private_helpers", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module, path


def corrected_build(args):
    """Pin the complete patched build identity; never relabel it as stock db."""
    path = args.candidate_build_receipt
    if path.is_symlink() or not path.is_file():
        raise ValueError("corrected build receipt must be a regular file")
    raw = path.read_bytes()
    if hashlib.sha256(raw).hexdigest() != args.candidate_build_receipt_sha256:
        raise ValueError("corrected build receipt SHA256 differs")
    value = json.loads(raw)
    if (value.get("schema") != "myownmesh-corrections-build-v1"
            or value.get("source_kind") != "patched-candidate"
            or "source_commit" not in value or value["source_commit"] is not None
            or value.get("patch_set_kind") != "corrected"
            or value.get("base_commit") != SOURCE or value.get("base_tree") != BASE_TREE
            or value.get("cargo_lock_sha256") != BASE_LOCK_LF_SHA256
            or value.get("default_features") is not False
            or value.get("features") not in ([], ["transport-lab"])
            or value.get("sha256") != args.daemon_sha256.lower()
            or type(value.get("bytes")) is not int
            or value["bytes"] != args.daemon.stat().st_size):
        raise ValueError("build receipt does not identify the selected corrected daemon")
    for key, width in (("source_tree", 40), ("patch_set_sha256", 64),
                       ("source_receipt_sha256", 64), ("cargo_lock_sha256", 64)):
        if not re.fullmatch(r"[0-9a-f]{%d}" % width, str(value.get(key, ""))):
            raise ValueError("corrected build source identity is incomplete")
    patches = value.get("patches")
    if not isinstance(patches, list) or not patches:
        raise ValueError("corrected build must retain its ordered patch identities")
    tree = BASE_TREE
    for patch in patches:
        if (not isinstance(patch, dict)
                or patch.get("purpose") not in ("production", "regression", "diagnostic")
                or not isinstance(patch.get("path"), str)
                or Path(patch["path"]).is_absolute()
                or ".." in Path(patch["path"]).parts
                or not re.fullmatch(r"[0-9a-f]{64}", str(patch.get("sha256", "")))
                or patch.get("tree_before") != tree
                or not re.fullmatch(r"[0-9a-f]{40}", str(patch.get("tree_after", "")))):
            raise ValueError("corrected build ordered patch chain is invalid")
        tree = patch["tree_after"]
    if tree != value["source_tree"] or not any(p["purpose"] == "production" for p in patches):
        raise ValueError("corrected build final source tree differs")
    for key in ("build_command", "native", "toolchain_evidence"):
        if key not in value:
            raise ValueError("corrected build execution provenance is incomplete")
    return {"path": str(path.resolve()), "sha256": args.candidate_build_receipt_sha256,
            "receipt": value}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--daemon", type=Path, required=True)
    parser.add_argument("--daemon-sha256", required=True)
    parser.add_argument("--probe", type=Path, required=True)
    parser.add_argument("--probe-sha256", required=True)
    parser.add_argument("--app-commit", required=True)
    parser.add_argument("--private-parent", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--resource-grant", required=True)
    parser.add_argument("--event-capacity", type=int, required=True)
    parser.add_argument("--quiet-ms", type=int, required=True)
    parser.add_argument("--expect", choices=("retained", "revoked"), required=True)
    parser.add_argument("--candidate-build-receipt", type=Path)
    parser.add_argument("--candidate-build-receipt-sha256")
    parser.add_argument("--baseline-negative-control", action="store_true",
                        help="run the strict new probe on stock db; defect detection exits nonzero")
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40}", args.app_commit):
        parser.error("app commit must be the exact reviewed application SHA")
    if not 100 <= args.quiet_ms <= 10_000:
        parser.error("quiet observation must be between 100 and 10000 milliseconds")
    if not 0 < args.event_capacity <= (1 << 64) - 1:
        parser.error("event capacity must be an explicit positive u64")
    supplied_receipt = args.candidate_build_receipt is not None
    supplied_receipt_sha = args.candidate_build_receipt_sha256 is not None
    if supplied_receipt != supplied_receipt_sha:
        parser.error("corrected build receipt and its SHA256 must be supplied together")
    if (args.baseline_negative_control and (args.expect != "revoked" or supplied_receipt)
            or args.expect == "retained" and supplied_receipt):
        parser.error("stock diagnostic and corrected candidate identities must stay separate")
    if args.expect == "revoked" and not args.baseline_negative_control and not supplied_receipt:
        parser.error("revoked qualification requires a pinned corrected build receipt")
    if supplied_receipt_sha and not re.fullmatch(r"[0-9a-f]{64}", args.candidate_build_receipt_sha256):
        parser.error("corrected build receipt SHA256 must be lowercase hexadecimal")
    build = corrected_build(args) if supplied_receipt else None
    selected_test = REVOKED_TEST if args.expect == "revoked" else TEST
    prefix = "MESH04_OWNER_REVOCATION " if args.expect == "revoked" else "MESH03_QUIET_EOF "
    common, helper_path = helpers()
    grant = common.checked_grant(args.resource_grant)
    parent = args.private_parent.resolve(strict=True)
    if not parent.is_dir():
        parser.error("private parent must be an existing protected short directory")
    output = args.output.resolve()
    if output == parent or parent in output.parents:
        parser.error("new evidence directory must be outside the private parent")
    output.mkdir(mode=0o700, exist_ok=False)
    owner = secrets.token_hex(8)
    root = Path(tempfile.mkdtemp(prefix="amc-eof-", dir=parent)).resolve()
    (root / "custody.json").write_text(json.dumps({"owner": owner, "root": str(root)}))
    state = root / "state"
    state.mkdir(mode=0o700)
    endpoint = "ams-eof-" + owner if os.name == "nt" else str(state / "control.sock")
    daemon_endpoint = "\\\\.\\pipe\\" + endpoint if os.name == "nt" else endpoint
    receipt = {
        "app_commit_input": args.app_commit,
        "candidate_source_for_reviewed_recipe": SOURCE,
        "binary_source_mapping": ("pinned corrected build receipt" if build else
                                  "requires independent exact-stock-source build receipt"),
        "source_kind": "patched-candidate" if build else "stock-baseline",
        "candidate_build": build,
        "baseline_negative_control": args.baseline_negative_control,
        "selected_test": selected_test,
        "corrected_qualification": False,
        "daemon_sha256": args.daemon_sha256.lower(),
        "probe_sha256": args.probe_sha256.lower(),
        "helper_sha256": common.digest(helper_path),
        "native_uname": platform.uname()._asdict(),
        "python": sys.version,
        "python_executable": sys.executable,
        "uid": os.getuid() if hasattr(os, "getuid") else None,
        "private_root": str(root),
        "endpoint": endpoint,
        "resource_grant": grant,
        "event_capacity": args.event_capacity,
        "quiet_ms": args.quiet_ms,
        "expected_authority_after_quiet": args.expect,
        "daemon_log_policy": "stdout/stderr discarded at child stdio; no raw capabilities retained",
        "ack_loss_flow_open": "not_executed_requires_real_flow",
        "children": [],
        "result": "pending",
    }
    children = []
    started = time.monotonic()
    exit_code = 1
    try:
        if os.name != "nt" and len(os.fsencode(endpoint)) > 100:
            raise ValueError("private socket path requires a shorter parent")
        config = common.configuration("candidate", state, daemon_endpoint, "", args.event_capacity)
        (state / "config.json").write_text(json.dumps(config, indent=2) + "\n")
        if os.name != "nt":
            (state / "config.json").chmod(0o600)
        receipt["config"] = config
        daemon_copy = root / ("daemon.exe" if os.name == "nt" else "daemon")
        probe_copy = root / ("probe.exe" if os.name == "nt" else "probe")
        common.copy_binary(args.daemon.absolute(), args.daemon_sha256, daemon_copy)
        common.copy_binary(args.probe.absolute(), args.probe_sha256, probe_copy)
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(("MYOWNMESH_", "AMS_MESH_CONTROL_", "AMS_QUIET_EOF_"))}
        env.update({"MYOWNMESH_HOME": str(state), "MYOWNMESH_AUTOUPDATE": "0",
                    "MYOWNMESH_LOG": "error", "RUST_LOG": "error",
                    "MYOWNMESH_RESOURCE_GRANT": args.resource_grant})
        # Fresh private state precedes even --version: main applies pending updates first.
        version = common.start([str(daemon_copy), "--version"], env, root, output, "version")
        children.append(version)
        common.finish(version, 10)
        reported = (output / "version.stdout").read_text().strip()
        if version["exit_code"] != 0 or reported not in ("myownmesh 1.0.0", "myownmesh v1.0.0"):
            raise ValueError("supplied binary did not report candidate version")
        receipt["reported_version"] = reported
        stdout = open(os.devnull, "wb")
        stderr = open(os.devnull, "wb")
        try:
            process = subprocess.Popen(
                [str(daemon_copy), "serve"], cwd=root, env=env,
                stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr,
                **common.child_options(),
            )
        except BaseException:
            stdout.close()
            stderr.close()
            raise
        daemon = {"process": process, "stdout": stdout, "stderr": stderr,
                  "label": "daemon", "command": [str(daemon_copy), "serve"], "pid": process.pid}
        children.append(daemon)
        probe_env = env.copy()
        probe_env.update({"AMS_QUIET_EOF_ROOT": str(root), "AMS_QUIET_EOF_OWNER": owner,
                          "AMS_QUIET_EOF_ENDPOINT": endpoint,
                          "AMS_QUIET_EOF_QUIET_MS": str(args.quiet_ms)})
        probe = common.start([str(probe_copy), "--ignored", "--exact", selected_test,
                              "--nocapture", "--test-threads=1"],
                             probe_env, root, output, "probe")
        children.append(probe)
        common.finish(probe, 100)
        if daemon["process"].poll() is not None:
            raise RuntimeError("owned daemon exited during raw-wire diagnostic")
        records = []
        probe_stdout = (output / "probe.stdout").read_text()
        probe_stderr = (output / "probe.stderr").read_text()
        for line in probe_stdout.splitlines():
            if prefix in line:
                records.append(json.loads(line.split(prefix, 1)[1]))
        if len(records) != 1:
            raise RuntimeError("raw-wire probe did not emit exactly one completion record")
        result = records[0]
        expected = "unknown_flow_capability" if args.expect == "retained" else "invalid_local_authority"
        source_key = "upstream_base" if args.expect == "revoked" else "source"
        if (result.get(source_key) != SOURCE or result.get("quiet_ms") != args.quiet_ms
                or result.get("before_close") != "unknown_flow_capability"
                or result.get("wrong_c_control") != "invalid_local_authority"
                or result.get("quiet_observer") is not True
                or result.get("flow_open_requests") != 0
                or result.get("ack_loss_flow_open") != "not_executed_requires_real_flow"
                or result.get("after_quiet") not in ("unknown_flow_capability", "invalid_local_authority")):
            raise RuntimeError("raw-wire completion record violates the reviewed diagnostic contract")
        receipt["observation"] = result
        if args.expect == "revoked":
            if (result.get("foreign_owner_refused") is not True
                    or result.get("observer_survived") is not True
                    or result.get("successor_survived") is not True
                    or result.get("native_flow_retirement") != "not_executed_no_real_flow"
                    or result.get("owner_revoked") != (result["after_quiet"] == expected)
                    or result.get("immediate_after_close") not in (
                        "unknown_flow_capability", "invalid_local_authority")
                    or type(result.get("sampled_after_close_ms")) is not int
                    or result["sampled_after_close_ms"] < args.quiet_ms):
                raise RuntimeError("corrected probe isolation or revocation record is invalid")
            if args.baseline_negative_control:
                if (probe["exit_code"] == 0 or result["after_quiet"] != "unknown_flow_capability"
                        or "test result: FAILED. 0 passed; 1 failed;" not in probe_stdout
                        or "quiet EOF must retire the exact subject authority" not in (
                            probe_stdout + probe_stderr)):
                    raise RuntimeError("stock negative control did not exhibit the exact retained-owner defect")
                receipt["result"] = "baseline_defect_reproduced_nonzero"
                # A measured negative control is evidence, never a corrected pass.
                exit_code = 1
            else:
                if probe["exit_code"] != 0 or result["after_quiet"] != expected:
                    raise RuntimeError("corrected raw-wire owner revocation failed")
                receipt["result"] = "corrected_owner_revocation_passed"
                receipt["corrected_qualification"] = True
                exit_code = 0
        else:
            if (probe["exit_code"] != 0 or result["after_quiet"] != expected
                    or result.get("gap_reproduced") is not True):
                raise RuntimeError("stock diagnostic differs from its selected retained expectation")
            receipt["result"] = "expected_observation_recorded"
            exit_code = 0
    except (Exception, KeyboardInterrupt) as error:
        receipt["result"] = "failed"
        receipt["failure"] = {"type": type(error).__name__, "message": str(error)}
    finally:
        errors = []
        for child in reversed(children):
            try:
                if "exit_code" not in child:
                    common.stop(child)
            except Exception as error:
                errors.append(type(error).__name__)
            receipt["children"].append({key: value for key, value in child.items()
                                        if key not in ("process", "stdout", "stderr")})
        receipt["children"].reverse()
        daemon_children = [child for child in children if child["label"] == "daemon"]
        receipt["normal_shutdown"] = bool(daemon_children) and all(
            child.get("stop_mode") == "sigterm" and child.get("exit_code") == 0
            for child in daemon_children
        )
        if os.name != "nt" and daemon_children and not receipt["normal_shutdown"]:
            errors.append("normal_unix_shutdown_not_observed")
        if any(child["process"].poll() is None for child in children):
            errors.append("owned_child_not_reaped")
        if errors:
            receipt["cleanup"] = "private_root_preserved_after_unreaped_or_abnormal_child"
        else:
            try:
                common.remove_owned(root, parent, owner)
                receipt["cleanup"] = "owned_private_root_removed_after_all_children_reaped"
            except Exception as error:
                errors.append(type(error).__name__)
                receipt["cleanup"] = "private_root_preserved_after_custody_or_cleanup_failure"
        if errors:
            receipt["result"] = "failed"
            receipt["corrected_qualification"] = False
            exit_code = 1
        receipt["cleanup_errors"] = errors
        receipt["elapsed_seconds"] = time.monotonic() - started
        receipt["stream_files"] = [{"name": path.name, "bytes": path.stat().st_size,
                                    "sha256": common.digest(path)}
                                   for path in sorted(output.iterdir())
                                   if path.suffix in (".stdout", ".stderr")]
        receipt_path = output / "receipt.json"
        receipt_path.write_text(json.dumps(receipt, indent=2) + "\n")
        print(json.dumps({"result": receipt["result"], "exit_code": exit_code,
                          "observation": receipt.get("observation"),
                          "normal_shutdown": receipt["normal_shutdown"],
                          "cleanup": receipt["cleanup"], "receipt": str(receipt_path)}))
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())

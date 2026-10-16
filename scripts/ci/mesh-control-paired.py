#!/usr/bin/env python3
"""Manager-run private candidate daemon pair; no installed/shared endpoints.

All resource, network, codec and timeout choices come from an explicit TEST
policy. This script never downloads, builds, installs, discovers or retries a
daemon. Complete child streams and a separate cleanup receipt are retained.
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
import sys
import tempfile
import time


def support():
    path = Path(__file__).with_name("mesh-control-isolated.py")
    spec = importlib.util.spec_from_file_location("mesh_control_owned_support", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module, path


def candidate_receipt(args, features):
    path = args.daemon_build_receipt
    if path.is_symlink() or not path.is_file():
        raise ValueError("regular explicit build receipt required")
    data = path.read_bytes()
    receipt_sha = hashlib.sha256(data).hexdigest()
    if args.daemon_build_receipt_sha256 is not None:
        if (not re.fullmatch(r"[0-9a-f]{64}", args.daemon_build_receipt_sha256)
                or receipt_sha != args.daemon_build_receipt_sha256):
            raise ValueError("selected build receipt SHA256 differs")
    build = json.loads(data)
    if args.candidate_kind == "stock":
        if (not isinstance(build, dict)
                or build.get("source_commit") != "db7818e09fedd98899490347b86ac9bc9f97b59b"
                or build.get("source_kind") not in (None, "stock")
                or build.get("sha256") != args.daemon_sha256.lower()
                or build.get("features") != features or build.get("default_features") is not False):
            raise ValueError("exact stock candidate build/feature/SHA mapping required")
    else:
        if args.daemon_build_receipt_sha256 is None:
            raise ValueError("patched candidate requires an explicitly pinned build receipt")
        helper_path = Path(__file__).with_name("myownmesh-corrections") / "prepare-candidate.py"
        spec = importlib.util.spec_from_file_location("mesh_corrected_candidate_identity", helper_path)
        helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(helper)
        helper.validate_build_receipt(build, args.daemon_sha256.lower(), features, args.candidate_kind)
        if args.daemon.stat().st_size != build["bytes"]:
            raise ValueError("selected artifact byte length differs")
    return build, receipt_sha


def positive(value, name, maximum=(1 << 64) - 1):
    if type(value) is not int or not 0 < value <= maximum:
        raise ValueError(name + " must be an explicit positive finite integer")
    return value


def exact_fields(value, names, label):
    if not isinstance(value, dict) or set(value) != set(names.split()):
        raise ValueError(label + " must contain exactly the documented fields")


def policy_file(path, owned):
    if not path.is_file() or path.is_symlink():
        raise ValueError("policy must be an explicit regular file")
    value = json.loads(path.read_text())
    exact_fields(value, "resource_grant event_capacity network profile signaling_limits ipc_frame_bytes client_policy test_timeout_secs", "TEST policy")
    owned.checked_grant(value["resource_grant"])
    positive(value["event_capacity"], "event_capacity")
    positive(value["ipc_frame_bytes"], "ipc_frame_bytes", (1 << 32) - 1)
    if value["ipc_frame_bytes"] <= 265:
        raise ValueError("TEST daemon frame ceiling must fund a full-width label and payload")
    positive(value["test_timeout_secs"], "test_timeout_secs", 600)
    client = value["client_policy"]
    exact_fields(client, "max_flows max_inbound_pipes max_frame_bytes max_retained_bytes operation_timeout_ms io_timeout_ms", "client policy")
    for key, amount in client.items():
        positive(amount, key, (1 << 32) - 1)
    if client["max_frame_bytes"] <= value["ipc_frame_bytes"]:
        raise ValueError("pressure control requires caller frame bound above daemon bound")
    if client["max_retained_bytes"] < client["max_frame_bytes"] + 4:
        raise ValueError("client retained budget must fund one maximum frame plus prefix")
    network = value["network"]
    exact_fields(network, "event_capacity connection_trace_capacity kind scheduler semantic_policy topology hub tree local_observations introduction signaling stun_servers turn_servers pinned_peers auto_approve", "private network template")
    if (network["kind"] != "open" or network["topology"] != {"kind": "full_mesh"}
            or network["auto_approve"] is not False
            or any(network[name] for name in ("stun_servers", "turn_servers", "pinned_peers"))
            or any(network[name] is not None for name in ("hub", "tree", "local_observations", "introduction"))):
        raise ValueError("pair requires explicit fresh Open/full-mesh without external peers or services")
    for key in ("event_capacity", "connection_trace_capacity"):
        positive(network[key], key)
    exact_fields(network["semantic_policy"],
                 "max_fact_encoded_bytes max_dependencies_per_fact max_authority_uses_per_fact max_authority_predecessors_per_use max_admitted_facts max_admitted_bytes max_quarantined_facts max_quarantined_bytes max_quarantined_facts_per_author max_quarantined_bytes_per_author max_retained_facts_per_author max_retained_bytes_per_author max_hot_history_facts max_dependency_edges max_ready_batch max_pending_proofs max_pending_proof_bytes max_proof_records max_proof_bytes max_proof_links max_author_usage_rows max_provisional_rows max_transaction_dirty_main_pages max_uncheckpointed_wal_frames max_freelist_pages max_fragmented_pages max_main_journal_bytes max_live_checkpoint_bytes max_database_bytes max_wal_bytes wal_checkpoint_threshold_bytes emergency_reserve_bytes",
                 "semantic policy")
    for key, amount in network["semantic_policy"].items():
        positive(amount, key)
    exact_fields(network["scheduler"],
                 "reactive_announce_min_interval_ms reoffer_min_interval_ms stale_inbound_timeout_ms probe_ttl_ms probe_resolve_timeout_ms skew_warn_ms skew_clear_ms skew_warn_ticks handshake_timeout_ms handshake_hello_retry_schedule_ms heartbeat_interval_ms heartbeat_timeout_ms wake_coalesce_ms wake_probe_delay_ms liveness_probe_min_interval_ms ice_disconnected_restart_ms state_watch_interval_ms reconnect_retry_backoff_ms data_channel_open_timeout_ms offer_build_timeout_ms ice_introspect_timeout_ms peer_send_timeout_ms restart_traffic_grace_ms relay_rescue_min_interval_ms network_change_restart_cooldown_ms reconnecting_grace_ms",
                 "scheduler policy")
    exact_fields(network["signaling"], "strategy mdns servers redundancy denylist public_fallback mdns_policy nostr_timing", "signaling policy")
    signaling = network["signaling"]
    if (signaling["strategy"] != "nostr" or signaling["mdns"] is not False
            or signaling["servers"] != [] or signaling["redundancy"] != 1
            or signaling["denylist"] != [] or signaling["public_fallback"] is not False):
        raise ValueError("pair signaling must select only its owned loopback relay")
    exact_fields(value["signaling_limits"],
                 "max_connections max_event_rate max_req_rate max_subscriptions max_filters_per_req max_message_bytes max_connections_per_ip max_presence_memberships max_handshake_bytes max_frame_bytes max_stored_events stored_retention_secs stats_heartbeat_interval_secs max_replay_per_req outbound_queue_cap strike_limit handshake_timeout_secs writer_stop_timeout_secs",
                 "signaling service limits")
    for key, amount in value["signaling_limits"].items():
        positive(amount, key)
    profile = value["profile"]
    exact_fields(profile, "codecs", "codec profile")
    if not isinstance(profile["codecs"], list) or len(profile["codecs"]) != 2:
        raise ValueError("qualification profile must explicitly select H264 and Opus")
    expected = {("video", "video/h264", 90000, 0, "annex_b"),
                ("audio", "audio/opus", 48000, 2, "whole")}
    actual = set()
    for codec in profile["codecs"]:
        exact_fields(codec, "kind payload_type mime clock_rate channels fmtp rtcp_feedback framing", "codec tuple")
        actual.add((codec["kind"], codec["mime"].lower(), codec["clock_rate"], codec["channels"], codec["framing"]))
    if actual != expected:
        raise ValueError("profile must match H264 and Opus RTP families (Opus SDP channels=2 even for mono PCM)")
    return value


def checked_vectors(path, owned, frame_bound):
    if not path.is_file() or path.is_symlink():
        raise ValueError("vector manifest must be an explicit regular file")
    manifest = json.loads(path.read_text())
    exact_fields(manifest, "version video audio software_reference", "vector manifest")
    if manifest["version"] != 1 or len(manifest["video"]) != 3 or len(manifest["audio"]) != 6:
        raise ValueError("expected exactly three valid H264 AUs and six Opus packets")
    parent = path.resolve().parent
    leaves = set()
    for kind in ("video", "audio"):
        for unit in manifest[kind]:
            exact_fields(unit, "file sha256 bytes duration_us ts_us", "encoded unit")
            if not isinstance(unit["file"], str) or Path(unit["file"]).name != unit["file"]:
                raise ValueError("vector file must be one owned leaf")
            if unit["file"] in leaves or unit["file"] == "vectors.json":
                raise ValueError("vector leaves must be unique and distinct from their manifest")
            leaves.add(unit["file"])
            if not isinstance(unit["sha256"], str) or not re.fullmatch(r"[0-9a-f]{64}", unit["sha256"]):
                raise ValueError("vector SHA256 must be exact lowercase hex")
            payload = parent / unit["file"]
            if payload.is_symlink() or not payload.is_file() or payload.resolve().parent != parent:
                raise ValueError("vector custody changed")
            positive(unit["duration_us"], "duration_us", (1 << 32) - 1)
            positive(unit["bytes"], "encoded bytes", frame_bound - 265)
            if payload.stat().st_size != unit["bytes"] or owned.digest(payload) != unit["sha256"]:
                raise ValueError("encoded vector checksum/length mismatch")
    return manifest


def relay_policy_file(path):
    if not path.is_file() or path.is_symlink():
        raise ValueError("relay policy must be an explicit regular file")
    value = json.loads(path.read_text())
    exact_fields(value, "version credential_mode service", "relay TEST supplement")
    if value["version"] != 1 or value["credential_mode"] != "fresh_per_owned_root":
        raise ValueError("relay supplement requires version1 and fresh owned credentials")
    service = value["service"]
    exact_fields(service, "enabled bind port public_ip realm max_bps_per_connection relay_port_min relay_port_max tcp_enabled tls_proxy_enabled tls_proxy_port tcp_max_connections tcp_max_connections_per_ip tcp_auth_timeout_ms tcp_idle_timeout_ms", "relay service policy")
    if (service["enabled"] is not True or service["bind"] != "127.0.0.1"
            or service["public_ip"] != "127.0.0.1" or service["tcp_enabled"] is not False
            or service["tls_proxy_enabled"] is not False or not service["realm"]):
        raise ValueError("relay fixture is loopback UDP only with explicit realm")
    for name in ("port", "relay_port_min", "relay_port_max", "tls_proxy_port"):
        positive(service[name], name, 65535)
        if service[name] < 1024:
            raise ValueError("relay ports must be explicitly unprivileged")
    if not service["relay_port_min"] <= service["relay_port_max"]:
        raise ValueError("finite relay allocation window required")
    if service["relay_port_min"] <= service["port"] <= service["relay_port_max"]:
        raise ValueError("TURN control port must be outside the relay allocation window")
    for name in ("max_bps_per_connection", "tcp_max_connections", "tcp_max_connections_per_ip", "tcp_auth_timeout_ms", "tcp_idle_timeout_ms"):
        positive(service[name], name)
    return value


def configuration(policy, state, endpoint, network_id, relay_url, host, signal_port, relay, credentials):
    network = json.loads(json.dumps(policy["network"]))
    network.update(id=network_id, network_id=network_id, label=network_id)
    network["signaling"]["servers"] = [relay_url]
    turn = {"enabled": False}
    if relay:
        turn = dict(relay["service"])
        turn["enabled"] = host
        turn["credentials"] = [credentials]
        network["turn_servers"] = [{"urls": [f"turn:127.0.0.1:{turn['port']}?transport=udp"],
                                    "username": credentials["username"], "credential": credentials["password"]}]
    return {
        "version": 2, "identity_path": str(state / ".secrets/identity.json"),
        "event_capacity": policy["event_capacity"],
        "auto_update": {"enabled": False}, "auto_cleanup": {"updates": False},
        "daemon": {"enabled": True, "control_socket": endpoint, "log_level": "warn"},
        "services": {
            "node": {"enabled": True},
            "signaling": {"enabled": host, "bind": "127.0.0.1", "port": signal_port,
                          "limits": policy["signaling_limits"]},
            "stun": {"enabled": False}, "turn": turn,
        },
        "networks": [network],
    }


def probe_result(output, label, expected_scenario):
    records = []
    for line in (output / (label + ".stdout")).read_text().splitlines():
        if "MESH_CONTROL_PAIRED " in line:
            records.append(json.loads(line.split("MESH_CONTROL_PAIRED ", 1)[1]))
    if len(records) != 1 or records[0].get("scenario") != expected_scenario:
        raise RuntimeError("owned probe did not emit one exact completed scenario receipt")
    return records[0]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--daemon", type=Path, required=True)
    parser.add_argument("--daemon-sha256", required=True)
    parser.add_argument("--daemon-build-receipt", type=Path, required=True)
    parser.add_argument("--daemon-build-receipt-sha256")
    parser.add_argument("--candidate-kind", choices=("stock", "diagnostic", "corrected"), default="stock")
    parser.add_argument("--probe", type=Path, required=True)
    parser.add_argument("--probe-sha256", required=True)
    parser.add_argument("--probe-kind", choices=("transport", "consumer"), required=True)
    parser.add_argument("--app-commit", required=True)
    parser.add_argument("--policy", type=Path, required=True)
    parser.add_argument("--vectors", type=Path, required=True)
    parser.add_argument("--private-parent", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--signaling-port", type=int, required=True)
    parser.add_argument("--mode", choices=("direct", "relay-lab"), required=True)
    parser.add_argument("--relay-policy", type=Path)
    args = parser.parse_args()
    owned, support_path = support()
    if not re.fullmatch(r"[0-9a-f]{40}", args.app_commit):
        parser.error("exact reviewed application commit required")
    if not 1024 <= args.signaling_port <= 65535:
        parser.error("choose an explicit unprivileged signaling port; collisions fail without retry")
    selected_features = ["transport-lab"] if args.mode == "relay-lab" else []
    build, build_receipt_sha = candidate_receipt(args, selected_features)
    relay = None
    if args.mode == "relay-lab":
        if args.relay_policy is None:
            parser.error("relay-lab requires its separate explicit TEST supplement")
        relay = relay_policy_file(args.relay_policy)
        ports = relay["service"]
        if args.signaling_port == ports["port"] or ports["relay_port_min"] <= args.signaling_port <= ports["relay_port_max"]:
            parser.error("signaling and owned TURN ports must be disjoint")
    elif args.relay_policy is not None:
        parser.error("direct mode must not receive a relay supplement")
    policy = policy_file(args.policy, owned)
    vectors = checked_vectors(args.vectors, owned, policy["ipc_frame_bytes"])
    parent = args.private_parent.resolve(strict=True)
    if not parent.is_dir():
        parser.error("private-parent must already exist and be protected for this account")
    if os.name != "nt" and parent.stat().st_mode & 0o077:
        parser.error("private-parent must have no group/other Unix permissions")
    requested_output = args.output.absolute()
    output = requested_output.parent.resolve(strict=True) / requested_output.name
    if output == parent or parent in output.parents:
        parser.error("choose NEW evidence outside the protected private-parent")
    output.mkdir(mode=0o700, exist_ok=False)
    owner = secrets.token_hex(8)
    credentials = {"username": "ams-" + owner, "password": secrets.token_urlsafe(24)} if relay else None
    root = Path(tempfile.mkdtemp(prefix="amc-", dir=parent)).resolve()
    (root / "custody.json").write_text(json.dumps({"owner": owner, "root": str(root)}))
    children = []
    endpoints = {}
    environments = {}
    states = {}
    network_id = "ams-mp-" + owner
    started = time.monotonic()
    receipt = {
        "app_commit_input": args.app_commit, "daemon_build_receipt": build,
        "candidate_kind": args.candidate_kind,
        "candidate_qualification": "diagnostic_only" if args.candidate_kind == "diagnostic" else args.candidate_kind,
        "diagnostic_log_profile": {"MYOWNMESH_LOG": "warn,mesh_first_unit=debug", "MYOWNMESH_LOG_FORMAT": "json"}
        if args.candidate_kind == "diagnostic" else None,
        "daemon_build_receipt_sha256": build_receipt_sha,
        "probe_sha256": args.probe_sha256, "probe_kind": args.probe_kind,
        "mode": args.mode, "native_uname": platform.uname()._asdict(),
        "relay_test_policy": relay,
        "relay_test_policy_sha256": owned.digest(args.relay_policy) if relay else None,
        "python": sys.version, "python_executable": sys.executable,
        "uid": os.getuid() if hasattr(os, "getuid") else None,
        "policy_sha256": owned.digest(args.policy), "explicit_test_policy": policy,
        "vectors_manifest_sha256": owned.digest(args.vectors),
        "support_sha256": owned.digest(support_path), "private_root": str(root),
        "result": "pending", "children": [],
    }
    exit_code = 1
    try:
        daemon_copy = root / ("daemon.exe" if os.name == "nt" else "daemon")
        probe_copy = root / ("probe.exe" if os.name == "nt" else "probe")
        owned.copy_binary(args.daemon.absolute(), args.daemon_sha256, daemon_copy)
        owned.copy_binary(args.probe.absolute(), args.probe_sha256, probe_copy)
        vectors_copy = root / "vectors"
        vectors_copy.mkdir(mode=0o700)
        (vectors_copy / "vectors.json").write_text(json.dumps(vectors))
        for kind in ("video", "audio"):
            for unit in vectors[kind]:
                source = args.vectors.resolve().parent / unit["file"]
                data = source.read_bytes()
                target = vectors_copy / unit["file"]
                target.write_bytes(data)
                if owned.digest(target) != unit["sha256"]:
                    raise ValueError("private vector copy checksum changed")
        base_env = {key: value for key, value in os.environ.items()
                    if not key.startswith(("MYOWNMESH_", "AMS_MESH_CONTROL_", "AMS_MESH_PAIRED_"))}
        for side in ("a", "b"):
            state = root / ("state-" + side)
            state.mkdir(mode=0o700)
            states[side] = state
            endpoint = "ams-mp-" + owner + "-" + side if os.name == "nt" else str(state / "s")
            if os.name != "nt" and len(os.fsencode(endpoint)) > 100:
                raise ValueError("use a shorter private parent for native Unix socket paths")
            endpoints[side] = endpoint
            daemon_endpoint = "\\\\.\\pipe\\" + endpoint if os.name == "nt" else endpoint
            config = configuration(policy, state, daemon_endpoint, network_id,
                                   f"ws://127.0.0.1:{args.signaling_port}", side == "a", args.signaling_port, relay, credentials)
            config_path = state / "config.json"
            config_path.write_text(json.dumps(config, indent=2) + "\n")
            if os.name != "nt":
                config_path.chmod(0o600)
            env = dict(base_env)
            env.update(MYOWNMESH_HOME=str(state), MYOWNMESH_AUTOUPDATE="0", RUST_LOG="warn",
                       MYOWNMESH_RESOURCE_GRANT=policy["resource_grant"],
                       MYOWNMESH_CONNECTOR_REALTIME_POLICY="enabled",
                       MYOWNMESH_REALTIME_PROFILE=json.dumps(policy["profile"]),
                       MYOWNMESH_IPC_REALTIME_FRAME_BYTES=str(policy["ipc_frame_bytes"]))
            if args.candidate_kind == "diagnostic":
                env["MYOWNMESH_LOG"] = "warn,mesh_first_unit=debug"
                env["MYOWNMESH_LOG_FORMAT"] = "json"
            if relay:
                env["MYOWNMESH_TRANSPORT_LAB_ICE_POLICY"] = "relay"
            environments[side] = env
        # A fresh private home precedes even --version; the upstream main runs
        # pending updater settlement before parsing flags.
        version = owned.start([str(daemon_copy), "--version"], environments["a"], root, output, "version")
        children.append(version)
        owned.finish(version, 10)
        if version["exit_code"] != 0 or (output / "version.stdout").read_text().strip() not in ("myownmesh 1.0.0", "myownmesh v1.0.0"):
            raise RuntimeError("checksum-pinned candidate version check failed")
        daemon_a = owned.start([str(daemon_copy), "serve"], environments["a"], root, output, "daemon-a")
        children.append(daemon_a)
        probe_env = dict(base_env)
        probe_env.update(AMS_MESH_PAIRED_ROOT=str(root), AMS_MESH_PAIRED_OWNER=owner,
                         AMS_MESH_PAIRED_ENDPOINT_A=endpoints["a"], AMS_MESH_PAIRED_ENDPOINT_B=endpoints["b"],
                         AMS_MESH_PAIRED_NETWORK=network_id, AMS_MESH_PAIRED_VECTORS=str(vectors_copy / "vectors.json"),
                         AMS_MESH_PAIRED_POLICY=json.dumps(policy),
                         AMS_MESH_PAIRED_SIGNALING_PORT=str(args.signaling_port),
                         AMS_MESH_PAIRED_MODE=args.mode,
                         AMS_MESH_PAIRED_TURN_PORT=str(relay["service"]["port"]) if relay else "")
        ready = owned.start([str(probe_copy), "--ignored", "--exact", "paired_endpoint_ready",
                             "--nocapture", "--test-threads=1"], probe_env, root, output, "ready-a")
        children.append(ready)
        owned.finish(ready, 45)
        if ready["exit_code"] != 0 or daemon_a["process"].poll() is not None:
            raise RuntimeError("owned loopback signaling/daemon startup failed; no retry")
        receipt["startup"] = probe_result(output, "ready-a", "endpoint-ready")
        if (receipt["startup"].get("mode") != args.mode
                or receipt["startup"].get("owned_turn_running") != bool(relay)):
            raise RuntimeError("private startup receipt does not match selected direct/relay mode")
        daemon_b = owned.start([str(daemon_copy), "serve"], environments["b"], root, output, "daemon-b-1")
        children.append(daemon_b)
        selector = "isolated_realtime_pair" if args.probe_kind == "transport" else "isolated_realtime_consumer_pair"
        probe = owned.start([str(probe_copy), "--ignored", "--exact", selector,
                             "--nocapture", "--test-threads=1"], probe_env, root, output, "paired")
        children.append(probe)
        marker = root / "restart-ready"
        while not marker.exists():
            if any(child["process"].poll() is not None for child in (daemon_a, daemon_b, probe)):
                raise RuntimeError("owned pair ended before exact restart barrier")
            if time.monotonic() - started > policy["test_timeout_secs"]:
                raise TimeoutError("paired absolute work deadline")
            time.sleep(0.02)
        if marker.read_text() != owner:
            raise ValueError("paired restart marker custody mismatch")
        owned.stop(daemon_b)
        if os.name != "nt" and daemon_b["exit_code"] != 0:
            raise RuntimeError("peer B did not complete normal Unix shutdown")
        if owned.digest(daemon_copy) != args.daemon_sha256.lower():
            raise ValueError("owned candidate executable changed before restart")
        daemon_b = owned.start([str(daemon_copy), "serve"], environments["b"], root, output, "daemon-b-2")
        children.append(daemon_b)
        complete_writing = root / "restart-complete-writing"
        with complete_writing.open("x") as completed:
            completed.write(owner)
            completed.flush()
        complete_writing.rename(root / "restart-complete")
        remaining = policy["test_timeout_secs"] - (time.monotonic() - started)
        if remaining <= 0:
            raise TimeoutError("no work deadline remains after owned restart")
        owned.finish(probe, remaining)
        if probe["exit_code"] != 0 or any(child["process"].poll() is not None for child in (daemon_a, daemon_b)):
            raise RuntimeError("actual paired media probe failed; full streams retained")
        receipt["probe_result"] = probe_result(output, "paired", args.probe_kind)
        result = receipt["probe_result"]
        for phase in ("initial", "after_owned_peer_restart"):
            if (result.get(phase, {}).get("video_units") != 3
                    or result.get(phase, {}).get("audio_packets") != 6):
                raise RuntimeError("paired receipt lacks exact real AV delivery counts")
            if args.probe_kind == "consumer" and result[phase].get("received_software_decode") is not True:
                raise RuntimeError("consumer receipt lacks received software decode assertions")
        for phase in ("initial_path", "after_restart_path"):
            observed = result.get(phase, {})
            if observed.get("mode") != args.mode:
                raise RuntimeError("selected-pair receipt mode mismatch")
            kind = "relay" if relay else "host"
            for side in ("a", "b"):
                if observed.get(side, {}).get("selected_pair") != {"local": kind, "remote": kind}:
                    raise RuntimeError("both native endpoints must report the actual selected ICE pair")
        receipt["result"] = "passed"
        exit_code = 0
    except (Exception, KeyboardInterrupt) as error:
        receipt["result"] = "failed"
        receipt["failure"] = {"type": type(error).__name__, "message": str(error)}
    finally:
        cleanup_errors = []
        for child in reversed(children):
            try:
                if "exit_code" not in child:
                    owned.stop(child)
            except Exception as error:
                cleanup_errors.append(type(error).__name__)
            receipt["children"].append({key: value for key, value in child.items()
                                        if key not in ("process", "stdout", "stderr")})
        receipt["children"].reverse()
        daemon_children = [child for child in children if child["label"].startswith("daemon-")]
        receipt["normal_shutdown"] = bool(daemon_children) and all(
            child.get("stop_mode") == "sigterm" and child.get("exit_code") == 0 for child in daemon_children)
        if os.name != "nt" and not receipt["normal_shutdown"]:
            cleanup_errors.append("IncompleteNormalUnixShutdown")
        if cleanup_errors or any(child["process"].poll() is None for child in children):
            receipt["cleanup"] = "private_root_preserved_after_child_failure"
        else:
            try:
                owned.remove_owned(root, parent, owner)
                receipt["cleanup"] = "owned_root_removed_after_all_children_reaped"
            except Exception as error:
                cleanup_errors.append(type(error).__name__)
                receipt["cleanup"] = "private_root_preserved_after_custody_failure"
        if cleanup_errors:
            receipt["result"] = "failed"
            exit_code = 1
        receipt["cleanup_errors"] = cleanup_errors
        receipt["elapsed_seconds"] = time.monotonic() - started
        receipt["stream_files"] = [{"name": path.name, "bytes": path.stat().st_size, "sha256": owned.digest(path)}
                                   for path in sorted(output.iterdir()) if path.suffix in (".stdout", ".stderr")]
        (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
        print(json.dumps({"result": receipt["result"], "normal_shutdown": receipt["normal_shutdown"],
                          "cleanup": receipt["cleanup"], "receipt": str(output / "receipt.json")}))
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())

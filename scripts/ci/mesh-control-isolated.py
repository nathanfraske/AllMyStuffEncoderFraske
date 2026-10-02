#!/usr/bin/env python3
"""Run one checksum-pinned daemon/probe pair in newly owned private state.

Native execution belongs to the manager. No discovery, downloads, installers,
default endpoints, shared process lookup, services, or permission changes.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import time

SOURCES = {
    "legacy": ("0.3.21", "59143cbdb094b6b99464c224a500e148fd7d2113"),
    "candidate": ("1.0.0", "db7818e09fedd98899490347b86ac9bc9f97b59b"),
}
DIMENSIONS = {
    "accounted_memory_bytes", "queued_bytes", "socket_or_handle",
    "native_transport_object", "worker_or_task", "callback_or_scheduled_work",
    "storage_bytes", "storage_object", "relay_or_provider_allocation",
    "parsing_or_cpu_work", "opaque_dependency_residual",
}


def digest(path):
    sha = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            sha.update(block)
    return sha.hexdigest()


def copy_binary(source, expected, destination):
    if not source.is_file() or source.is_symlink():
        raise ValueError("supplied executable must be a regular non-symlink file")
    if not re.fullmatch(r"[0-9a-fA-F]{64}", expected):
        raise ValueError("executable SHA256 must be exact")
    if digest(source) != expected.lower():
        raise ValueError("supplied executable checksum mismatch")
    shutil.copyfile(source, destination)
    if os.name != "nt":
        destination.chmod(0o700)
    if digest(destination) != expected.lower():
        raise ValueError("owned executable copy checksum mismatch")


def child_options():
    if os.name == "nt":
        # Foreground CLI only; never create an interactive desktop window.
        return {"creationflags": subprocess.CREATE_NO_WINDOW}
    return {"start_new_session": True}


def start(command, env, cwd, output, label):
    stdout = (output / (label + ".stdout")).open("xb", buffering=0)
    stderr = (output / (label + ".stderr")).open("xb", buffering=0)
    try:
        process = subprocess.Popen(
            command, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
            stdout=stdout, stderr=stderr, **child_options()
        )
    except BaseException:
        stdout.close()
        stderr.close()
        raise
    return {"process": process, "stdout": stdout, "stderr": stderr,
            "label": label, "command": command, "pid": process.pid}


def finish(child, timeout):
    child["exit_code"] = child["process"].wait(timeout=timeout)
    child["stdout"].close()
    child["stderr"].close()


def stop(child):
    process = child["process"]
    if process.poll() is not None:
        mode = "already_exited"
    elif os.name == "nt":
        # Reviewed `serve` awaits ctrl_c on Windows, not CTRL_BREAK. A hidden
        # child has no console in which to deliver Ctrl-C safely. Terminate only
        # this Popen-owned child and report forced rather than graceful teardown.
        mode = "forced_owned_process"
        process.terminate()
    else:
        mode = "sigterm"
        process.send_signal(signal.SIGTERM)
    try:
        finish(child, 10)
    except subprocess.TimeoutExpired:
        mode = "forced_owned_process"
        process.kill()
        finish(child, 10)
    child["stop_mode"] = mode


def checked_grant(raw):
    values = {}
    for entry in raw.split(","):
        name, separator, amount = entry.partition("=")
        name, amount = name.strip(), amount.strip()
        if (not separator or name not in DIMENSIONS or name in values
                or not re.fullmatch(r"[0-9]+", amount)):
            raise ValueError("resource grant must name each reviewed dimension once")
        value = int(amount)
        if value > (1 << 64) - 1:
            raise ValueError("resource grant must contain finite u64 amounts")
        values[name] = value
    if values.keys() != DIMENSIONS:
        raise ValueError("candidate requires all eleven owner-selected grant dimensions")
    return values


def configuration(contract, state, endpoint, network, event_capacity):
    services = {name: {"enabled": False} for name in ("node", "signaling", "stun", "turn")}
    config = {
        "version": 3 if contract == "legacy" else 2,
        "identity_path": str(state / ".secrets" / "identity.json"),
        "auto_update": {"enabled": False},
        "auto_cleanup": {"updates": False},
        "daemon": {"enabled": True, "control_socket": endpoint, "log_level": "warn"},
        "services": services,
        "networks": [],
    }
    if contract == "legacy":
        services["node"]["enabled"] = True
        services["relay"] = {"enabled": False}
        # One fresh local network. No signaling drivers, public fallback, ICE
        # servers, pinned peers or automatic peer approvals are enabled.
        config["networks"] = [{
            "id": network, "network_id": network,
            "signaling": {"strategy": "none", "mdns": False, "servers": [],
                          "public_fallback": False, "listen_only": True},
            "stun_servers": [], "turn_servers": [], "pinned_peers": [],
            "auto_approve": False,
        }]
    else:
        config["event_capacity"] = event_capacity
    return config


def remove_owned(root, parent, owner):
    # Verify final absolute target before recursive removal, including Windows.
    if root.is_symlink() or root.resolve() != root or root.parent != parent:
        raise ValueError("private root custody changed; cleanup refused")
    custody = json.loads((root / "custody.json").read_text())
    if custody != {"owner": owner, "root": str(root)} or not root.name.startswith("amc-"):
        raise ValueError("private root marker changed; cleanup refused")
    shutil.rmtree(root)
    if root.exists():
        raise ValueError("private root was not removed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--daemon", type=Path, required=True)
    parser.add_argument("--daemon-sha256", required=True)
    parser.add_argument("--probe", type=Path, required=True)
    parser.add_argument("--probe-sha256", required=True)
    parser.add_argument("--app-commit", required=True)
    parser.add_argument("--contract", choices=SOURCES, required=True)
    parser.add_argument("--private-parent", type=Path, required=True,
                        help="existing short private directory, protected for this account")
    parser.add_argument("--output", type=Path, required=True, help="new evidence directory")
    parser.add_argument("--resource-grant", help="candidate owner-selected eleven-dimensional grant")
    parser.add_argument("--event-capacity", type=int, help="candidate owner-selected finite history")
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40}", args.app_commit):
        parser.error("--app-commit must be the exact reviewed application commit")
    grant = None
    if args.contract == "candidate":
        if not args.resource_grant or args.event_capacity is None:
            parser.error("candidate requires --resource-grant and --event-capacity; no defaults")
        grant = checked_grant(args.resource_grant)
        if not 0 < args.event_capacity <= (1 << 64) - 1:
            parser.error("candidate event capacity must be a positive u64")
    elif args.resource_grant is not None or args.event_capacity is not None:
        parser.error("candidate grant/history settings cannot be silently used for legacy")
    parent = args.private_parent.resolve(strict=True)
    if not parent.is_dir():
        parser.error("--private-parent must be an existing directory")
    output = args.output.absolute()
    output.mkdir(mode=0o700, exist_ok=False)
    owner = secrets.token_hex(8)
    root = Path(tempfile.mkdtemp(prefix="amc-", dir=parent)).resolve()
    (root / "custody.json").write_text(json.dumps({"owner": owner, "root": str(root)}))
    state = root / "state"
    state.mkdir(mode=0o700)
    network = "ams-mc-" + owner
    endpoint = "ams-mc-" + owner if os.name == "nt" else str(state / "control.sock")
    daemon_endpoint = "\\\\.\\pipe\\" + endpoint if os.name == "nt" else endpoint
    version, source_commit = SOURCES[args.contract]
    receipt = {
        "app_commit_input": args.app_commit,
        "daemon_source_for_reviewed_recipe": source_commit,
        "binary_source_mapping": "requires independent build/release receipt; semver does not prove it",
        "daemon_sha256": args.daemon_sha256.lower(), "probe_sha256": args.probe_sha256.lower(),
        "contract": args.contract, "native_uname": platform.uname()._asdict(),
        "python": sys.version, "python_executable": sys.executable,
        "uid": os.getuid() if hasattr(os, "getuid") else None,
        "private_root": str(root), "endpoint": endpoint, "resource_grant": grant,
        "event_capacity": args.event_capacity, "children": [], "result": "pending",
    }
    children = []
    daemon = None
    probe = None
    started = time.monotonic()
    exit_code = 1
    try:
        if os.name != "nt" and len(os.fsencode(endpoint)) > 100:
            raise ValueError("private socket path exceeds conservative native macOS bound; use shorter parent")
        config = configuration(args.contract, state, daemon_endpoint, network, args.event_capacity)
        (state / "config.json").write_text(json.dumps(config, indent=2) + "\n")
        if os.name != "nt":
            (state / "config.json").chmod(0o600)
        receipt["config"] = config
        daemon_copy = root / ("daemon.exe" if os.name == "nt" else "daemon")
        probe_copy = root / ("probe.exe" if os.name == "nt" else "probe")
        copy_binary(args.daemon.absolute(), args.daemon_sha256, daemon_copy)
        copy_binary(args.probe.absolute(), args.probe_sha256, probe_copy)
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(("MYOWNMESH_", "AMS_MESH_CONTROL_"))}
        env.update({"MYOWNMESH_HOME": str(state), "MYOWNMESH_AUTOUPDATE": "0", "RUST_LOG": "warn"})
        if args.resource_grant is not None:
            env["MYOWNMESH_RESOURCE_GRANT"] = args.resource_grant
        # Empty owned state precedes EVERY binary invocation, including version:
        # both reviewed mains apply pending updates before parsing CLI options.
        version_child = start([str(daemon_copy), "--version"], env, root, output, "version")
        children.append(version_child)
        finish(version_child, 10)
        reported = (output / "version.stdout").read_text().strip()
        if version_child["exit_code"] != 0 or reported not in ("myownmesh " + version, "myownmesh v" + version):
            raise ValueError("supplied binary does not report the exact selected daemon version")
        receipt["reported_version"] = reported
        daemon = start([str(daemon_copy), "serve"], env, root, output, "daemon-1")
        children.append(daemon)
        probe_env = env.copy()
        probe_env.update({"AMS_MESH_CONTROL_ROOT": str(root), "AMS_MESH_CONTROL_OWNER": owner,
                          "AMS_MESH_CONTROL_CONTRACT": args.contract,
                          "AMS_MESH_CONTROL_ENDPOINT": endpoint, "AMS_MESH_CONTROL_NETWORK": network})
        probe = start([str(probe_copy), "--ignored", "--exact", "isolated_daemon_sessions",
                       "--nocapture", "--test-threads=1"], probe_env, root, output, "probe")
        children.append(probe)
        ready = root / "restart-ready"
        while not ready.exists():
            if probe["process"].poll() is not None or daemon["process"].poll() is not None:
                raise RuntimeError("owned probe/daemon ended before the restart barrier")
            if time.monotonic() - started > 150:
                raise TimeoutError("owned restart barrier deadline")
            time.sleep(0.05)
        if ready.read_text() != owner:
            raise ValueError("owned restart barrier custody mismatch")
        stop(daemon)
        if os.name != "nt" and daemon["exit_code"] != 0:
            raise RuntimeError("owned daemon did not complete normal Unix shutdown")
        if digest(daemon_copy) != args.daemon_sha256.lower():
            raise ValueError("owned daemon checksum changed before restart")
        daemon = start([str(daemon_copy), "serve"], env, root, output, "daemon-2")
        children.append(daemon)
        finish(probe, max(1, 180 - (time.monotonic() - started)))
        if probe["exit_code"] != 0:
            raise RuntimeError("actual library probe failed; retain complete streams")
        if daemon["process"].poll() is not None:
            raise RuntimeError("owned daemon ended during the lifecycle probe")
        records = []
        for line in (output / "probe.stdout").read_text().splitlines():
            if "MESH_CONTROL_ISOLATED " in line:
                records.append(json.loads(line.split("MESH_CONTROL_ISOLATED ", 1)[1]))
        if len(records) != 1 or records[0].get("sessions") != 4 or records[0].get("contract") != args.contract:
            raise RuntimeError("probe did not emit its exact completed lifecycle receipt")
        receipt["probe_result"] = records[0]
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
                    stop(child)
            except Exception as error:
                cleanup_errors.append(type(error).__name__)
            receipt["children"].append({key: value for key, value in child.items()
                                        if key not in ("process", "stdout", "stderr")})
        receipt["children"].reverse()
        receipt["elapsed_seconds"] = time.monotonic() - started
        daemon_children = [child for child in children if child["label"].startswith("daemon-")]
        receipt["normal_shutdown"] = bool(daemon_children) and all(
            child.get("stop_mode") == "sigterm" and child.get("exit_code") == 0
            for child in daemon_children
        )
        if os.name != "nt" and daemon_children and not receipt["normal_shutdown"]:
            receipt["result"] = "failed"
            exit_code = 1
        if cleanup_errors or any(child["process"].poll() is None for child in children):
            receipt["cleanup"] = "preserved_private_root_after_unreaped_child"
            receipt["result"] = "failed"
            exit_code = 1
        else:
            try:
                remove_owned(root, parent, owner)
                receipt["cleanup"] = "owned_root_removed_after_all_children_reaped"
            except Exception as error:
                cleanup_errors.append(type(error).__name__)
                receipt["cleanup"] = "private_root_preserved_after_custody_or_cleanup_failure"
                receipt["result"] = "failed"
                exit_code = 1
        receipt["cleanup_errors"] = cleanup_errors
        receipt["stream_files"] = [{"name": path.name, "bytes": path.stat().st_size, "sha256": digest(path)}
                                   for path in sorted(output.iterdir()) if path.suffix in (".stdout", ".stderr")]
        (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
        print(json.dumps({"result": receipt["result"], "exit_code": exit_code,
                          "normal_shutdown": receipt["normal_shutdown"], "cleanup": receipt["cleanup"],
                          "receipt": str(output / "receipt.json")}))
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())

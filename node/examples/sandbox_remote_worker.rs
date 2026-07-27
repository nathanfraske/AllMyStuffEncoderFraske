//! Lease-bounded remote executor for the sealed sandbox harness.
//!
//! One authenticated terminal bootstrap launches this worker. Later requests
//! arrive as sealed files through the existing AllMyStuff Files data route.
//! The worker invokes only the fixed sandbox bootstrap in the stable runtime,
//! seals its result, and exits after collection or an idle lease.

use std::collections::HashSet;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Debug)]
struct Config {
    runtime: PathBuf,
    stage_root: PathBuf,
    idle: Duration,
    ignore_request_id: String,
    manifest_sha256: String,
}

fn usage() -> &'static str {
    "usage: sandbox_remote_worker --runtime DIR --stage-root DIR \
     --idle-seconds N --ignore-request-id UUID --manifest-sha256 HEX"
}

fn next_value(
    arguments: &mut impl Iterator<Item = String>,
    option: &str,
) -> Result<String> {
    arguments
        .next()
        .ok_or_else(|| anyhow::anyhow!("{option} requires a value"))
}

fn parse() -> Result<Config> {
    let mut arguments = std::env::args().skip(1);
    let mut runtime = None;
    let mut stage_root = None;
    let mut idle_seconds = None;
    let mut ignore_request_id = None;
    let mut manifest_sha256 = None;

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--runtime" => {
                runtime = Some(PathBuf::from(next_value(&mut arguments, "--runtime")?))
            }
            "--stage-root" => {
                stage_root = Some(PathBuf::from(next_value(&mut arguments, "--stage-root")?))
            }
            "--idle-seconds" => {
                let value = next_value(&mut arguments, "--idle-seconds")?;
                let seconds = value
                    .parse::<u64>()
                    .context("--idle-seconds must be a whole number")?;
                if !(60..=3600).contains(&seconds) {
                    bail!("--idle-seconds must be between 60 and 3600");
                }
                idle_seconds = Some(seconds);
            }
            "--ignore-request-id" => {
                ignore_request_id =
                    Some(next_value(&mut arguments, "--ignore-request-id")?)
            }
            "--manifest-sha256" => {
                manifest_sha256 =
                    Some(next_value(&mut arguments, "--manifest-sha256")?)
            }
            unknown => bail!("unknown argument: {unknown}"),
        }
    }

    let manifest_sha256 =
        manifest_sha256.context("--manifest-sha256 is required")?;
    if manifest_sha256.len() != 64
        || !manifest_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("--manifest-sha256 must be 64 hexadecimal characters");
    }

    Ok(Config {
        runtime: runtime.context("--runtime is required")?,
        stage_root: stage_root.context("--stage-root is required")?,
        idle: Duration::from_secs(
            idle_seconds.context("--idle-seconds is required")?,
        ),
        ignore_request_id: ignore_request_id
            .context("--ignore-request-id is required")?,
        manifest_sha256: manifest_sha256.to_ascii_uppercase(),
    })
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn sha256(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("open result for hashing: {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read result for hashing: {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect())
}

fn write_json_atomic(path: &Path, value: &Value) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create result directory: {}", parent.display()))?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("sandbox-worker"),
        std::process::id()
    ));
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(&temporary, bytes)
        .with_context(|| format!("write temporary result: {}", temporary.display()))?;
    if path.exists() {
        fs::remove_file(path)
            .with_context(|| format!("replace result: {}", path.display()))?;
    }
    fs::rename(&temporary, path)
        .with_context(|| format!("publish result: {}", path.display()))
}

fn request_string<'a>(request: &'a Value, pointer: &str) -> &'a str {
    request
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn error_result(request: &Value, message: String) -> Value {
    json!({
        "schema": 1,
        "kind": "allmystuff-sandbox-remote-worker-error",
        "request_id": request_string(request, "/request_id"),
        "run_id": request_string(request, "/run_id"),
        "action": request_string(request, "/action"),
        "host": std::env::var("COMPUTERNAME").unwrap_or_default(),
        "target_peer_id": request_string(request, "/target/peer_id"),
        "instance_id": request_string(request, "/instance_id"),
        "source_commit": request_string(request, "/bundle/source_commit"),
        "error": message,
    })
}

fn seal_result(config: &Config, request: &Value, result_path: &Path) -> Result<()> {
    let request_id = request_string(request, "/request_id");
    let result_name = format!("sandbox-remote-result-{request_id}.json");
    let seal_path = config
        .stage_root
        .join("outbox")
        .join(format!("sandbox-remote-result-{request_id}.seal.json"));
    let size = fs::metadata(result_path)
        .with_context(|| format!("stat remote result: {}", result_path.display()))?
        .len();
    let seal = json!({
        "schema": 1,
        "kind": "allmystuff-sandbox-worker-result-seal",
        "request_id": request_id,
        "host": std::env::var("COMPUTERNAME").unwrap_or_default(),
        "target_peer_id": request_string(request, "/target/peer_id"),
        "result_remote_path": format!(
            ".allmystuff-sandbox-stage\\outbox\\{result_name}"
        ),
        "result_size": size,
        "result_sha256": sha256(result_path)?,
        "manifest_sha256": config.manifest_sha256,
    });
    write_json_atomic(&seal_path, &seal)
}

fn execute_request(config: &Config, request: &Value) -> Result<()> {
    let request_id = request_string(request, "/request_id");
    if request_id.is_empty() {
        bail!("request has no request_id");
    }
    let bootstrap = config
        .runtime
        .join("bootstrap-allmystuff-sandbox-remote.ps1");
    if !bootstrap.is_file() {
        bail!("stable bootstrap is missing: {}", bootstrap.display());
    }
    let result_path = config
        .stage_root
        .join("outbox")
        .join(format!("sandbox-remote-result-{request_id}.json"));

    let status = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&bootstrap)
        .current_dir(&config.runtime)
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("launch stable bootstrap: {}", bootstrap.display()))?;

    if !status.success() || !result_path.is_file() {
        let message = format!(
            "bootstrap exit={:?}; inspect the sandbox worker logs",
            status.code(),
        );
        write_json_atomic(&result_path, &error_result(request, message))?;
    }
    seal_result(config, request, &result_path)
}

fn ready_record(config: &Config) -> Value {
    json!({
        "schema": 1,
        "kind": "allmystuff-sandbox-remote-worker",
        "pid": std::process::id(),
        "started_unix_ms": unix_millis(),
        "idle_seconds": config.idle.as_secs(),
        "manifest_sha256": config.manifest_sha256,
        "request_path": config
            .stage_root
            .join("inbox")
            .join("sandbox-remote-request.json"),
    })
}

fn run(config: Config) -> Result<()> {
    let request_path = config
        .stage_root
        .join("inbox")
        .join("sandbox-remote-request.json");
    let ready_path = config.runtime.join("sandbox-remote-worker.json");
    write_json_atomic(&ready_path, &ready_record(&config))?;
    println!(
        "SANDBOX_REMOTE_WORKER_READY pid={} idle_seconds={}",
        std::process::id(),
        config.idle.as_secs()
    );

    let mut processed = HashSet::from([config.ignore_request_id.clone()]);
    let mut last_invalid = Vec::new();
    let mut last_activity = Instant::now();
    loop {
        if last_activity.elapsed() >= config.idle {
            println!("SANDBOX_REMOTE_WORKER_IDLE_EXIT");
            break;
        }

        if let Ok(bytes) = fs::read(&request_path) {
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(request) => {
                    last_invalid.clear();
                    let request_id = request_string(&request, "/request_id").to_string();
                    if !request_id.is_empty() && processed.insert(request_id.clone()) {
                        last_activity = Instant::now();
                        if let Err(error) = execute_request(&config, &request) {
                            let result_path = config
                                .stage_root
                                .join("outbox")
                                .join(format!("sandbox-remote-result-{request_id}.json"));
                            write_json_atomic(
                                &result_path,
                                &error_result(&request, format!("{error:#}")),
                            )?;
                            seal_result(&config, &request, &result_path)?;
                        }
                        if request
                            .get("worker_stop_after")
                            .and_then(Value::as_bool)
                            == Some(true)
                        {
                            println!(
                                "SANDBOX_REMOTE_WORKER_REQUESTED_EXIT request_id={request_id}"
                            );
                            break;
                        }
                    }
                }
                Err(error) if bytes != last_invalid => {
                    eprintln!("sandbox request is not complete JSON yet: {error}");
                    last_invalid = bytes;
                }
                Err(_) => {}
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    if let Ok(value) = fs::read(&ready_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .ok_or_else(|| anyhow::anyhow!("worker record is unavailable"))
    {
        if value.get("pid").and_then(Value::as_u64) == Some(std::process::id() as u64) {
            let _ = fs::remove_file(&ready_path);
        }
    }
    Ok(())
}

fn main() {
    let result = parse().and_then(run);
    if let Err(error) = result {
        eprintln!("{error:#}");
        eprintln!("{}", usage());
        std::process::exit(1);
    }
}

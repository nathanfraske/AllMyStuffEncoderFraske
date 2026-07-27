//! Unshipped P2 field harness: stage files and run bounded PowerShell through
//! the production AllMyStuff Files/terminal routes. Node-control is local IPC;
//! every remote payload is a FileEvent/TermEvent on an authenticated ICE lane.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use allmystuff_node::node_control::NodeClient;
use allmystuff_session::{FileEvent, TermEvent};
use anyhow::{bail, Context, Result};
use base64::Engine as _;
use serde_json::{json, Value};

const FILE_CHUNK: usize = 40 * 1024;
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;
const TERMINAL_DSR_HANDSHAKE_ERROR: &str =
    "remote PowerShell did not complete its exact DSR handshake";
const TERMINAL_PRE_SEND_ERROR: &str = "terminal setup failed before command send";

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let Some(command) = args.first().map(String::as_str) else {
        usage();
        bail!("missing command");
    };
    let peer = std::env::var("ALLMYSTUFF_P2_PEER")
        .context("ALLMYSTUFF_P2_PEER must name the exact bootstrap peer")?;
    if peer.trim().is_empty() {
        bail!("ALLMYSTUFF_P2_PEER cannot be empty");
    }
    let client = NodeClient::new().context("resolve local AllMyStuff node-control socket")?;
    let local = local_node(&client).await?;
    if inventory_preflight_enabled() {
        let paths = authenticated_ice_paths(&client, &peer).await?;
        println!("P2_AUTHENTICATED_ICE={}", serde_json::to_string(&paths)?);
    } else {
        // Hard-exclusion mode for a shared fleet: connect only the exact
        // requested application route and do not enumerate unrelated peers.
        // The route still runs over MyOwnMesh's authenticated ICE data channel.
        println!("P2_EXACT_ROUTE_NO_PEER_INVENTORY={peer}");
    }

    match command {
        "ice" if args.len() == 1 => {}
        "upload" if args.len() >= 3 => {
            let remote_dir = &args[1];
            let files = args[2..].iter().map(PathBuf::from).collect::<Vec<_>>();
            upload_files(&client, &local, &peer, remote_dir, &files).await?;
        }
        "download" if args.len() == 3 => {
            download_file(
                &client,
                &local,
                &peer,
                &args[1],
                Path::new(&args[2]),
                None,
            )
            .await?;
        }
        "download-wait" if args.len() == 4 => {
            let seconds = args[3]
                .parse::<u64>()
                .context("download-wait timeout must be whole seconds")?;
            if !(1..=3600).contains(&seconds) {
                bail!("download-wait timeout must be between 1 and 3600 seconds");
            }
            download_file(
                &client,
                &local,
                &peer,
                &args[1],
                Path::new(&args[2]),
                Some(Duration::from_secs(seconds)),
            )
            .await?;
        }
        "exec" if args.len() == 2 => {
            let script = std::fs::read(&args[1])
                .with_context(|| format!("read local script {}", args[1]))?;
            let output = exec_script(&client, &local, &peer, &script).await?;
            print!("{output}");
        }
        "exec-bootstrap" if args.len() == 1 => {
            let output = exec_bootstrap(&client, &local, &peer).await?;
            print!("{output}");
        }
        "terminal-routes" if args.len() == 1 => {
            let routes = p2_terminal_routes(&client, &local, &peer).await?;
            println!("P2_TERMINAL_ROUTES={}", serde_json::to_string(&routes)?);
        }
        "disconnect-terminal-routes" if args.len() == 1 => {
            let routes = p2_terminal_routes(&client, &local, &peer).await?;
            let mut disconnected = Vec::new();
            for route in routes {
                if route.get("state").and_then(Value::as_str) == Some("torn_down") {
                    continue;
                }
                let route_id = route
                    .get("route_id")
                    .and_then(Value::as_str)
                    .context("filtered terminal route has no route_id")?;
                client
                    .request("disconnect_route", json!({ "route_id": route_id }))
                    .await
                    .with_context(|| format!("disconnect exact P2 terminal route {route_id}"))?;
                disconnected.push(route_id.to_string());
            }
            println!(
                "P2_TERMINAL_ROUTES_DISCONNECTED={}",
                serde_json::to_string(&disconnected)?
            );
        }
        "disconnect" if args.len() >= 2 => {
            for route in &args[1..] {
                client
                    .request("disconnect_route", json!({ "route_id": route }))
                    .await
                    .with_context(|| format!("disconnect exact route {route}"))?;
                println!("P2_ROUTE_DISCONNECTED={route}");
            }
        }
        _ => {
            usage();
            bail!("invalid arguments");
        }
    }
    Ok(())
}

fn inventory_preflight_enabled() -> bool {
    !std::env::var("ALLMYSTUFF_P2_NO_INVENTORY").is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "on" | "true" | "yes"
        )
    })
}

fn usage() {
    eprintln!("set ALLMYSTUFF_P2_PEER to the exact bootstrap peer; there is no default");
    eprintln!("set ALLMYSTUFF_P2_NO_INVENTORY=1 to skip all peer-list preflight");
    eprintln!("p2_remote_transport ice");
    eprintln!("p2_remote_transport upload REMOTE_DIR LOCAL_FILE [LOCAL_FILE ...]");
    eprintln!("p2_remote_transport download REMOTE_FILE LOCAL_FILE");
    eprintln!("p2_remote_transport download-wait REMOTE_FILE LOCAL_FILE TIMEOUT_SECONDS");
    eprintln!("p2_remote_transport exec LOCAL_SCRIPT.ps1");
    eprintln!("p2_remote_transport exec-bootstrap");
    eprintln!("p2_remote_transport terminal-routes");
    eprintln!("p2_remote_transport disconnect-terminal-routes");
    eprintln!("p2_remote_transport disconnect ROUTE_ID [ROUTE_ID ...]");
}

async fn local_node(client: &NodeClient) -> Result<String> {
    let scan = client
        .request("scan_self", Value::Null)
        .await
        .context("query running local AllMyStuff node")?;
    scan.get("node_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .context("scan_self response has no node_id")
}

async fn authenticated_ice_paths(client: &NodeClient, peer: &str) -> Result<Vec<Value>> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let networks = client
            .request("mesh_networks", Value::Null)
            .await
            .context("query joined mesh networks")?;
        let mut found = Vec::new();
        for network in networks
            .get("networks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(config_id) = network.get("config_id").and_then(Value::as_str) else {
                continue;
            };
            let Ok(view) = client
                .request("mesh_peers", json!({ "network": config_id }))
                .await
            else {
                continue;
            };
            for p in view
                .get("peers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let id = p
                    .get("device_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if canonical_node(id) == canonical_node(peer)
                    && p.get("status").and_then(Value::as_str) == Some("active")
                    && p.get("authenticated").and_then(Value::as_bool) == Some(true)
                    && p.get("selected_pair").is_some_and(Value::is_object)
                {
                    found.push(json!({
                        "network": config_id,
                        "peer": id,
                        "status": p.get("status"),
                        "authenticated": p.get("authenticated"),
                        "selected_pair": p.get("selected_pair"),
                        "rtt_ms": p.get("rtt_ms"),
                        "needs_turn": p.get("needs_turn"),
                    }));
                }
            }
        }
        if !found.is_empty() {
            return Ok(found);
        }
        if Instant::now() >= deadline {
            bail!("{peer} has no authenticated ACTIVE selected ICE pair");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn upload_files(
    client: &NodeClient,
    local: &str,
    peer: &str,
    remote_dir: &str,
    files: &[PathBuf],
) -> Result<()> {
    if remote_dir.to_ascii_lowercase().contains("downloads")
        || remote_dir.to_ascii_lowercase().contains("\\temp")
        || remote_dir.to_ascii_lowercase().contains("/temp")
    {
        bail!("refusing a Downloads/Temp staging destination");
    }
    for file in files {
        if !file.is_file() {
            bail!("local upload source is not a file: {}", file.display());
        }
    }

    let nonce = nonce();
    let from = format!("{peer}:files");
    let to = format!("{local}:files-view:p2-{nonce}");
    let expected = format!("route:{from}→{to}");
    let route = client
        .request(
            "connect_route",
            json!({ "from": from, "to": to, "media": "generic", "video": [], "session": null }),
        )
        .await
        .context("offer production Files route")?
        .as_str()
        .context("connect_route returned no route id")?
        .to_string();
    if route != expected {
        bail!("unexpected Files route id {route}; expected {expected}");
    }
    if let Err(error) = wait_route_active(client, &route, Duration::from_secs(20)).await {
        let _ = client
            .request("disconnect_route", json!({ "route_id": route }))
            .await;
        return Err(error);
    }
    let token = client
        .request("file_watch", json!({ "route_id": route }))
        .await
        .context("watch Files responses")?
        .as_u64()
        .context("file_watch returned no token")?;

    let result = async {
        let mut req = 1u64;
        send_file(
            client,
            &route,
            FileEvent::Mkdir {
                req,
                path: remote_dir.into(),
            },
        )
        .await?;
        wait_file_ok(client, &route, req, Duration::from_secs(30)).await?;

        for local_path in files {
            req += 1;
            let name = local_path
                .file_name()
                .and_then(|v| v.to_str())
                .context("upload filename is not UTF-8")?;
            let remote = format!("{}\\{}", remote_dir.trim_end_matches(['\\', '/']), name);
            let bytes = std::fs::read(local_path)
                .with_context(|| format!("read {}", local_path.display()))?;
            if bytes.is_empty() {
                send_file(
                    client,
                    &route,
                    FileEvent::Write {
                        req,
                        path: remote,
                        data: vec![],
                        append: false,
                        eof: true,
                    },
                )
                .await?;
            } else {
                for (index, piece) in bytes.chunks(FILE_CHUNK).enumerate() {
                    send_file(
                        client,
                        &route,
                        FileEvent::Write {
                            req,
                            path: remote.clone(),
                            data: piece.to_vec(),
                            append: index != 0,
                            eof: (index + 1) * FILE_CHUNK >= bytes.len(),
                        },
                    )
                    .await?;
                    if index % 32 == 31 {
                        println!(
                            "P2_UPLOAD_PROGRESS={} bytes={}",
                            name,
                            ((index + 1) * FILE_CHUNK).min(bytes.len())
                        );
                    }
                }
            }
            wait_file_ok(client, &route, req, Duration::from_secs(60)).await?;
            println!("P2_UPLOAD_OK={} bytes={}", name, bytes.len());
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;

    let _ = client
        .request("file_unwatch", json!({ "route_id": route, "token": token }))
        .await;
    let _ = client
        .request("disconnect_route", json!({ "route_id": route }))
        .await;
    result
}

async fn send_file(client: &NodeClient, route: &str, event: FileEvent) -> Result<()> {
    client
        .request(
            "file_send",
            json!({ "route_id": route, "event": serde_json::to_value(event)? }),
        )
        .await
        .context("send Files-plane event")?;
    Ok(())
}

async fn download_file(
    client: &NodeClient,
    local: &str,
    peer: &str,
    remote_path: &str,
    local_path: &Path,
    availability_timeout: Option<Duration>,
) -> Result<()> {
    if local_path.exists() {
        bail!(
            "refusing to overwrite local download: {}",
            local_path.display()
        );
    }
    if let Some(parent) = local_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create local download directory {}", parent.display()))?;
    }

    let nonce = nonce();
    let from = format!("{peer}:files");
    let to = format!("{local}:files-view:p2-{nonce}");
    let expected = format!("route:{from}→{to}");
    let route = client
        .request(
            "connect_route",
            json!({ "from": from, "to": to, "media": "generic", "video": [], "session": null }),
        )
        .await
        .context("offer production Files route")?
        .as_str()
        .context("connect_route returned no route id")?
        .to_string();
    if route != expected {
        bail!("unexpected Files route id {route}; expected {expected}");
    }
    if let Err(error) = wait_route_active(client, &route, Duration::from_secs(20)).await {
        let _ = client
            .request("disconnect_route", json!({ "route_id": route }))
            .await;
        return Err(error);
    }
    let token = client
        .request("file_watch", json!({ "route_id": route }))
        .await
        .context("watch Files responses")?
        .as_u64()
        .context("file_watch returned no token")?;

    let result = async {
        let mut req = 1u64;
        send_file(
            client,
            &route,
            FileEvent::Read {
                req,
                path: remote_path.to_string(),
            },
        )
        .await?;

        let mut file = std::fs::File::create(local_path)
            .with_context(|| format!("create local download {}", local_path.display()))?;
        let mut received = 0u64;
        let availability_deadline =
            Instant::now() + availability_timeout.unwrap_or(Duration::from_secs(90));
        let mut transfer_deadline = None;
        loop {
            let batch = client
                .request_bytes("file_poll", json!({ "route_id": route }))
                .await
                .context("poll Files download")?;
            for raw in split_batch(&batch)? {
                let event: FileEvent = serde_json::from_slice(raw).context("parse FileFrame")?;
                match event {
                    FileEvent::Chunk {
                        req: event_req,
                        data,
                        total,
                        eof,
                    } if event_req == req => {
                        transfer_deadline
                            .get_or_insert_with(|| Instant::now() + Duration::from_secs(90));
                        if total > MAX_DOWNLOAD {
                            bail!("remote file is {total} bytes, above the {MAX_DOWNLOAD}-byte harness limit");
                        }
                        received = received
                            .checked_add(data.len() as u64)
                            .context("download length overflow")?;
                        if received > total {
                            bail!("remote download exceeded its declared {total}-byte length");
                        }
                        file.write_all(&data).context("write local download")?;
                        if eof {
                            if received != total {
                                bail!("remote download ended at {received} of {total} bytes");
                            }
                            file.flush().context("flush local download")?;
                            println!(
                                "P2_DOWNLOAD_OK={} bytes={} local={}",
                                remote_path,
                                received,
                                local_path.display()
                            );
                            return Ok::<(), anyhow::Error>(());
                        }
                    }
                    FileEvent::Err {
                        req: event_req,
                        reason,
                    } if event_req == req => {
                        if availability_timeout.is_some()
                            && received == 0
                            && Instant::now() < availability_deadline
                        {
                            req = req
                                .checked_add(1)
                                .context("Files wait request id overflow")?;
                            tokio::time::sleep(Duration::from_millis(250)).await;
                            send_file(
                                client,
                                &route,
                                FileEvent::Read {
                                    req,
                                    path: remote_path.to_string(),
                                },
                            )
                            .await?;
                            continue;
                        }
                        bail!("remote Files request {req} failed: {reason}")
                    }
                    _ => {}
                }
            }
            if transfer_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                bail!("timed out transferring {remote_path}");
            }
            if transfer_deadline.is_none() && Instant::now() >= availability_deadline {
                bail!("timed out waiting for {remote_path}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    .await;

    let _ = client
        .request("file_unwatch", json!({ "route_id": route, "token": token }))
        .await;
    let _ = client
        .request("disconnect_route", json!({ "route_id": route }))
        .await;
    if result.is_err() {
        let _ = std::fs::remove_file(local_path);
    }
    result
}

async fn wait_file_ok(client: &NodeClient, route: &str, req: u64, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let batch = client
            .request_bytes("file_poll", json!({ "route_id": route }))
            .await
            .context("poll Files responses")?;
        for raw in split_batch(&batch)? {
            let event: Value = serde_json::from_slice(raw).context("parse FileFrame")?;
            if event.get("req").and_then(Value::as_u64) != Some(req) {
                continue;
            }
            match event.get("kind").and_then(Value::as_str) {
                Some("ok") => return Ok(()),
                Some("err") => bail!(
                    "remote Files request {req} failed: {}",
                    event
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                ),
                _ => {}
            }
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for Files request {req}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn exec_script(
    client: &NodeClient,
    local: &str,
    peer: &str,
    script: &[u8],
) -> Result<String> {
    let encoded_script = base64::engine::general_purpose::STANDARD.encode(script);
    let statement = format!(
        "$__p2=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String(\
         '{encoded_script}'));&([ScriptBlock]::Create($__p2))"
    );
    exec_powershell_statement(client, local, peer, &statement).await
}

async fn p2_terminal_routes(
    client: &NodeClient,
    local: &str,
    peer: &str,
) -> Result<Vec<Value>> {
    let snapshot = client
        .request("session_snapshot", Value::Null)
        .await
        .context("query local session routes")?;
    let expected_from = format!("{peer}:terminal");
    let expected_to_prefix = format!("{local}:term-view:p2-");
    let mut matches = Vec::new();
    for live in snapshot
        .get("routes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(route) = live.get("route") else {
            continue;
        };
        let from = route.get("from").and_then(Value::as_str);
        let to = route.get("to").and_then(Value::as_str);
        if from != Some(expected_from.as_str())
            || !to.is_some_and(|value| value.starts_with(&expected_to_prefix))
        {
            continue;
        }
        matches.push(json!({
            "route_id": route.get("id"),
            "from": from,
            "to": to,
            "state": route_state(&snapshot, route.get("id").and_then(Value::as_str).unwrap_or_default()),
            "term_session": live.get("term_session"),
            "incarnation": live.get("incarnation"),
        }));
    }
    Ok(matches)
}

async fn exec_bootstrap(client: &NodeClient, local: &str, peer: &str) -> Result<String> {
    // This path is deliberately fixed. The deployer uploads and seal-checks
    // this exact bootstrap before asking the terminal route to invoke it.
    // Keeping the PTY command short avoids PSReadLine repainting a large
    // base64 payload one character at a time.
    let statement = concat!(
        "$__p2_path=Join-Path $env:USERPROFILE ",
        "'.allmystuff-sandbox-stage\\inbox\\",
        "bootstrap-allmystuff-sandbox-remote.ps1';",
        "if(-not(Test-Path -LiteralPath $__p2_path -PathType Leaf)){",
        "throw 'sealed remote bootstrap is missing'};",
        "& $__p2_path"
    );
    exec_powershell_statement(client, local, peer, statement).await
}

async fn exec_powershell_statement(
    client: &NodeClient,
    local: &str,
    peer: &str,
    statement: &str,
) -> Result<String> {
    match exec_powershell_statement_once(client, local, peer, statement).await {
        Err(error) if error.to_string().starts_with(TERMINAL_PRE_SEND_ERROR) => {
            println!("P2_TERMINAL_SETUP_RETRY=1 reason=pre_send_setup");
            tokio::time::sleep(Duration::from_millis(250)).await;
            exec_powershell_statement_once(client, local, peer, statement)
                .await
                .context("terminal setup retry failed")
        }
        result => result,
    }
}

async fn exec_powershell_statement_once(
    client: &NodeClient,
    local: &str,
    peer: &str,
    statement: &str,
) -> Result<String> {
    let nonce = nonce();
    let from = format!("{peer}:terminal");
    let to = format!("{local}:term-view:p2-{nonce}");
    let expected = format!("route:{from}→{to}");
    let route = client
        .request(
            "connect_route",
            json!({ "from": from, "to": to, "media": "generic", "video": [], "session": null }),
        )
        .await
        .context("offer production terminal route")?
        .as_str()
        .context("connect_route returned no route id")?
        .to_string();
    if route != expected {
        let _ = client
            .request("disconnect_route", json!({ "route_id": route }))
            .await;
        bail!("unexpected terminal route id {route}; expected {expected}");
    }
    if let Err(error) = wait_route_active(client, &route, Duration::from_secs(20)).await {
        let _ = client
            .request("disconnect_route", json!({ "route_id": route }))
            .await;
        return Err(error);
    }
    let watch = client
        .request("term_watch", json!({ "route_id": route }))
        .await;
    let token = match watch {
        Ok(value) => match value.as_u64() {
            Some(token) => token,
            None => {
                let _ = client
                    .request("disconnect_route", json!({ "route_id": route }))
                    .await;
                bail!("term_watch returned no token");
            }
        },
        Err(error) => {
            let _ = client
                .request("disconnect_route", json!({ "route_id": route }))
                .await;
            return Err(error).context("watch terminal output");
        }
    };

    let result = async {
        let ok_marker = format!("__P2_EXEC_OK_{nonce}_EXIT_0__");
        let error_marker = format!("__P2_EXEC_ERROR_{nonce}_EXIT_1__");
        let child_script = format!(
            "$ErrorActionPreference='Stop';try{{{statement};Write-Output \
             '{ok_marker}';exit 0}}catch{{Write-Output \
             ('{error_marker}'+($_|Out-String));exit 1}}"
        );
        let child_utf16le = child_script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let encoded_command = base64::engine::general_purpose::STANDARD.encode(child_utf16le);
        // Run the payload in a noninteractive child. This prevents PSReadLine's
        // history prediction from painting a previous command (and its old
        // sentinel) into the PTY output before the new command has actually run.
        let command = format!(
            "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand \
             {encoded_command}; exit\r\n"
        );
        let setup: Result<Vec<u8>> = async {
            client
                .request(
                    "term_send",
                    json!({
                        "route_id": route,
                        "event": serde_json::to_value(TermEvent::Resize {
                            cols: 160,
                            rows: 50
                        })?,
                    }),
                )
                .await
                .context("resize initial terminal")?;

            // Windows PowerShell/PSReadLine asks a real terminal for its cursor
            // position before presenting the first prompt. Answer only the
            // exact DSR query; never reflect or interpret arbitrary remote
            // output as input.
            let handshake_deadline = Instant::now() + Duration::from_secs(10);
            let mut output = Vec::new();
            let mut dsr_answers = 0usize;
            let mut last_dsr_answer = None;
            loop {
                let batch = match client
                    .request_bytes("term_poll", json!({ "route_id": route }))
                    .await
                {
                    Ok(batch) => batch,
                    Err(error)
                        if error
                            .to_string()
                            .contains("route isn't an active terminal session here") =>
                    {
                        if Instant::now() >= handshake_deadline {
                            return Err(error).context("wait for terminal session registration");
                        }
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        continue;
                    }
                    Err(error) => {
                        return Err(error).context("poll initial terminal handshake");
                    }
                };
                for raw in split_batch(&batch)? {
                    output.extend_from_slice(raw);
                }
                let dsr_queries = output
                    .windows(4)
                    .filter(|window| *window == b"\x1b[6n")
                    .count();
                while dsr_answers < dsr_queries {
                    client
                        .request(
                            "term_send",
                            json!({
                                "route_id": route,
                                "event": serde_json::to_value(TermEvent::Data {
                                    bytes: b"\x1b[1;1R".to_vec(),
                                })?,
                            }),
                        )
                        .await
                        .context("answer exact terminal DSR cursor query")?;
                    dsr_answers += 1;
                    last_dsr_answer = Some(Instant::now());
                }
                if last_dsr_answer.is_some_and(|at| at.elapsed() >= Duration::from_millis(750)) {
                    break;
                }
                if Instant::now() >= handshake_deadline {
                    bail!(
                        "{TERMINAL_DSR_HANDSHAKE_ERROR}; output: {}",
                        String::from_utf8_lossy(&output)
                    );
                }
                // `session_snapshot` is a one-shot local control connection. A
                // 10 Hz timeout loop needlessly churns Windows named-pipe
                // handles while the far side is offline. Four checks per
                // second remains responsive relative to data-plane route setup.
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Ok(output)
        }
        .await;
        let mut output = setup.context(TERMINAL_PRE_SEND_ERROR)?;

        client
            .request(
                "term_send",
                json!({
                    "route_id": route,
                    "event": serde_json::to_value(TermEvent::Data {
                        bytes: command.into_bytes()
                    })?,
                }),
            )
            .await
            .context("send terminal script over Files/AMST data plane")?;

        let deadline = Instant::now() + Duration::from_secs(75);
        loop {
            let batch = client
                .request_bytes("term_poll", json!({ "route_id": route }))
                .await
                .context("poll terminal output")?;
            for raw in split_batch(&batch)? {
                output.extend_from_slice(raw);
            }
            let text = String::from_utf8_lossy(&output);
            if text.contains(&ok_marker) {
                break Ok(text.into_owned());
            }
            if text.contains(&error_marker) {
                break Err(anyhow::anyhow!("remote script failed:\n{text}"));
            }
            if Instant::now() >= deadline {
                break Err(anyhow::anyhow!("remote script timed out; output:\n{text}"));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    .await;

    let _ = client
        .request("term_unwatch", json!({ "route_id": route, "token": token }))
        .await;
    let _ = client
        .request("disconnect_route", json!({ "route_id": route }))
        .await;
    result
}

async fn wait_route_active(client: &NodeClient, route: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let snapshot = client
            .request("session_snapshot", Value::Null)
            .await
            .context("query route state")?;
        match route_state(&snapshot, route).as_deref() {
            Some("active") => return Ok(()),
            Some("rejected") => bail!("route {route} was rejected"),
            Some("torn_down") => bail!("route {route} was torn down"),
            _ => {}
        }
        if Instant::now() >= deadline {
            bail!("route {route} did not become active within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn route_state(snapshot: &Value, route: &str) -> Option<String> {
    let live = snapshot
        .get("routes")?
        .as_array()?
        .iter()
        .find(|candidate| candidate.pointer("/route/id").and_then(Value::as_str) == Some(route))?;
    let state = live.get("state")?;
    state
        .as_str()
        .or_else(|| state.get("state").and_then(Value::as_str))
        .map(str::to_string)
}

fn split_batch(batch: &[u8]) -> Result<Vec<&[u8]>> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset < batch.len() {
        if batch.len() - offset < 4 {
            bail!("truncated poll batch at byte {offset}");
        }
        let len = u32::from_le_bytes(batch[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        if len > batch.len() - offset {
            bail!("invalid poll frame length {len}");
        }
        out.push(&batch[offset..offset + len]);
        offset += len;
    }
    Ok(out)
}

fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn canonical_node(value: &str) -> String {
    let node = value.split_once(':').map(|(node, _)| node).unwrap_or(value);
    match node.rsplit_once('-') {
        Some((bare, suffix))
            if suffix.len() == 5
                && suffix.chars().all(|c| c.is_ascii_alphanumeric())
                && bare.len() >= 32 =>
        {
            bare.to_string()
        }
        _ => node.to_string(),
    }
}

#[allow(dead_code)]
fn file_name(path: &Path) -> Result<&str> {
    path.file_name()
        .and_then(|v| v.to_str())
        .context("file name is not UTF-8")
}

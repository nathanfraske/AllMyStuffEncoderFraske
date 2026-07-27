//! Narrow local control helper for a sandbox AllMyStuff node.
//!
//! The helper refuses the production node socket. It only invokes existing
//! node-control commands and never changes a signaling, ICE, STUN, TURN, route,
//! or media wire format.

use std::time::{Duration, Instant};

use allmystuff_graph::{Grant, GrantRole, MediaKind, Person};
use allmystuff_node::node_control::NodeClient;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

const TEST_NETWORK_COMPONENTS: usize = 4;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    assert_sandbox_socket()?;
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let Some(command) = args.first().map(String::as_str) else {
        usage();
        bail!("missing command");
    };
    let client = NodeClient::new().context("resolve sandbox node-control socket")?;

    let result = match command {
        "identity" if args.len() == 1 => {
            let scan = client
                .request("scan_self", Value::Null)
                .await
                .context("scan sandbox node")?;
            let mesh = client
                .request("mesh_identity", Value::Null)
                .await
                .context("read sandbox mesh identity")?;
            json!({
                "scan": scan,
                "mesh": mesh,
            })
        }
        "specs" if args.len() == 1 => client
            .request("machine_specs", Value::Null)
            .await
            .context("read sandbox host specs")?,
        "set-label" if args.len() == 2 => client
            .request("mesh_identity_set_label", json!({ "label": args[1] }))
            .await
            .context("set sandbox identity label")?,
        "networks" if args.len() == 1 => client
            .request("mesh_networks", Value::Null)
            .await
            .context("list sandbox networks")?,
        "generate-test-network-id" if args.len() == 1 => generate_test_network_id(&client).await?,
        "join" if args.len() == 3 => join_network(&client, &args[1], &args[2]).await?,
        "leave" if args.len() == 2 => client
            .request("mesh_network_remove", json!({ "network": args[1] }))
            .await
            .context("leave sandbox test network")?,
        "peers" if args.len() == 2 => client
            .request("mesh_peers", json!({ "network": args[1] }))
            .await
            .context("list sandbox test peers")?,
        "wait-exact-peer" if args.len() == 4 => {
            let timeout = args[3]
                .parse::<u64>()
                .context("wait-exact-peer timeout must be whole seconds")?;
            if !(1..=300).contains(&timeout) {
                bail!("wait-exact-peer timeout must be between 1 and 300 seconds");
            }
            wait_exact_peer(&client, &args[1], &args[2], Duration::from_secs(timeout)).await?
        }
        "grant-screen-view" if args.len() == 2 => grant_screen_view(&client, &args[1]).await?,
        "stop-sharing-with" if args.len() == 2 => stop_sharing_with(&client, &args[1]).await?,
        _ => {
            usage();
            bail!("invalid arguments");
        }
    };

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn usage() {
    eprintln!("sandbox_node_control identity");
    eprintln!("sandbox_node_control specs");
    eprintln!("sandbox_node_control set-label LABEL");
    eprintln!("sandbox_node_control networks");
    eprintln!("sandbox_node_control generate-test-network-id");
    eprintln!("sandbox_node_control join NETWORK_ID LABEL");
    eprintln!("sandbox_node_control leave NETWORK_ID");
    eprintln!("sandbox_node_control peers NETWORK_ID");
    eprintln!("sandbox_node_control wait-exact-peer NETWORK_ID PEER_ID TIMEOUT_SECONDS");
    eprintln!("sandbox_node_control grant-screen-view PEER_ID");
    eprintln!("sandbox_node_control stop-sharing-with PEER_ID");
}

fn assert_sandbox_socket() -> Result<()> {
    let socket = std::env::var("ALLMYSTUFF_NODE_SOCKET")
        .context("ALLMYSTUFF_NODE_SOCKET must select a sandbox node")?;
    let normalized = socket.to_ascii_lowercase();
    if !normalized.contains("sandbox") {
        bail!("refusing node-control socket without the sandbox marker: {socket}");
    }
    Ok(())
}

async fn generate_test_network_id(client: &NodeClient) -> Result<Value> {
    let mut components = Vec::with_capacity(TEST_NETWORK_COMPONENTS);
    for _ in 0..TEST_NETWORK_COMPONENTS {
        let response = client
            .request("mesh_network_id_generate", Value::Null)
            .await
            .context("generate test-network component")?;
        let component = response
            .get("network_id")
            .and_then(Value::as_str)
            .context("mesh_network_id_generate returned no network_id")?;
        if component.len() != 8
            || !component
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            bail!("mesh_network_id_generate returned an unexpected component");
        }
        components.push(component.to_string());
    }
    Ok(json!({
        "network_id": components.join("-"),
        "generator_calls": TEST_NETWORK_COMPONENTS,
    }))
}

async fn join_network(client: &NodeClient, network_id: &str, label: &str) -> Result<Value> {
    if network_id.eq_ignore_ascii_case("allmystuff-local-claim-v1") {
        bail!("the built-in local-claim network is not a dedicated test network");
    }
    let networks = client
        .request("mesh_networks", Value::Null)
        .await
        .context("check existing sandbox networks")?;
    if let Some(existing) = networks
        .get("networks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|network| {
            network
                .get("network_id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.eq_ignore_ascii_case(network_id))
        })
    {
        return Ok(json!({
            "status": "already_joined",
            "network": existing,
        }));
    }

    let result = client
        .request(
            "mesh_network_add",
            json!({
                "config": {
                    "id": network_id,
                    "network_id": network_id,
                    "label": label,
                    "auto_approve": true,
                }
            }),
        )
        .await
        .context("join sandbox test network")?;
    Ok(json!({
        "status": "joined",
        "result": result,
    }))
}

async fn wait_exact_peer(
    client: &NodeClient,
    network: &str,
    expected_peer: &str,
    timeout: Duration,
) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let view = client
            .request("mesh_peers", json!({ "network": network }))
            .await
            .context("poll sandbox test peers")?;
        let peers = view
            .get("peers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let unexpected = peers.iter().filter(|peer| {
            let id = peer
                .get("device_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            canonical_node(id) != canonical_node(expected_peer)
        });
        let unexpected_ids = unexpected
            .map(|peer| {
                peer.get("device_id")
                    .and_then(Value::as_str)
                    .unwrap_or("(missing)")
                    .to_string()
            })
            .collect::<Vec<_>>();
        if !unexpected_ids.is_empty() {
            bail!(
                "test network contains unexpected peer(s): {}",
                unexpected_ids.join(", ")
            );
        }

        if let Some(peer) = peers.iter().find(|peer| {
            peer.get("device_id")
                .and_then(Value::as_str)
                .is_some_and(|id| canonical_node(id) == canonical_node(expected_peer))
        }) {
            let ready = peer.get("status").and_then(Value::as_str) == Some("active")
                && peer.get("authenticated").and_then(Value::as_bool) == Some(true)
                && peer.get("selected_pair").is_some_and(Value::is_object);
            if ready {
                return Ok(json!({
                    "status": "ready",
                    "network": network,
                    "peer": peer,
                    "peer_count": peers.len(),
                }));
            }
        }

        if Instant::now() >= deadline {
            bail!("expected peer {expected_peer} did not reach an authenticated active ICE path");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn grant_screen_view(client: &NodeClient, peer_id: &str) -> Result<Value> {
    let canonical = canonical_node(peer_id);
    let person = Person {
        id: format!("person:{canonical}").into(),
        name: canonical.to_string(),
    };
    let person_id = person.id.clone();
    let grant = Grant::scoped(
        &person_id,
        MediaKind::Display,
        GrantRole::Consume,
        None,
        "Sandbox screen view",
    );
    let grant_id = grant.id.clone();
    let result = client
        .request(
            "share_grant",
            json!({
                "person": person,
                "node": peer_id,
                "grant": grant,
            }),
        )
        .await
        .context("grant sandbox peer permission to view this screen")?;
    Ok(json!({
        "status": "granted",
        "peer": peer_id,
        "person_id": person_id,
        "grant_id": grant_id,
        "media": "display",
        "role": "consume",
        "result": result,
    }))
}

async fn stop_sharing_with(client: &NodeClient, peer_id: &str) -> Result<Value> {
    let person_id = format!("person:{}", canonical_node(peer_id));
    let result = client
        .request("share_stop", json!({ "person": person_id }))
        .await
        .context("remove sandbox screen-share grant")?;
    Ok(json!({
        "status": "stopped",
        "peer": peer_id,
        "person_id": person_id,
        "result": result,
    }))
}

fn canonical_node(id: &str) -> &str {
    let id = id.trim();
    if let Some((key, suffix)) = id.rsplit_once('-') {
        if suffix.len() == 5 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return key;
        }
    }
    id
}

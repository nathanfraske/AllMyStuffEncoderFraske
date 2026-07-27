//! Local-only field probe for the GUI route-handle wire shape.
//!
//! This uses the running node's ordinary control socket, prints the raw JSON
//! value returned to Tauri, and immediately closes the exact route generation.

use allmystuff_node::node_control::NodeClient;
use anyhow::{Context, Result};
use serde_json::{json, Value};

#[tokio::main]
async fn main() -> Result<()> {
    let source = std::env::args()
        .nth(1)
        .context("usage: route_handle_probe SOURCE SINK")?;
    let sink = std::env::args()
        .nth(2)
        .context("usage: route_handle_probe SOURCE SINK")?;
    let client = NodeClient::new()?;
    let handle = client
        .request(
            "connect_route_handle",
            json!({
                "from": source,
                "to": sink,
                "media": "display",
                "video": ["h264"],
                "session": null,
            }),
        )
        .await
        .context("connect_route_handle")?;
    println!("{}", serde_json::to_string_pretty(&handle)?);

    if let (Some(route_id), Some(generation)) = (
        handle.get("route_id").and_then(Value::as_str),
        handle.get("generation").and_then(Value::as_u64),
    ) {
        let _ = client
            .request(
                "disconnect_route",
                json!({ "route_id": route_id, "generation": generation }),
            )
            .await;
    }
    Ok(())
}

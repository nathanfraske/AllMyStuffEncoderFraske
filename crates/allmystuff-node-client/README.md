# AllMyStuff node client

This library speaks the AllMyStuff node's existing binary local IPC protocol.
It supplies framing, request/response/event types, socket addressing and clients
without depending on `allmystuff-node`, capture, codecs, scanners or a GUI.
It still uses `allmystuff-protocol` for the existing state-directory helper,
including that crate's graph/Serde/`dirs` dependencies.

```toml
[dependencies]
allmystuff-node-client = { path = "path/to/AllMyStuff/crates/allmystuff-node-client" }
anyhow = "1"
serde_json = "1"
```

Inside an existing Tokio runtime, a caller can use the node-style client:

```rust,no_run
async fn node_version() -> anyhow::Result<serde_json::Value> {
    allmystuff_node_client::NodeClient::new()?
        .request("node_version", serde_json::Value::Null)
        .await
}
```

`NodeClient` preserves the node/desktop's `anyhow` context chains and warning
messages under the existing `allmystuff_node::node_control` tracing target.
`terminal::NodeClient` preserves the terminal client's `Result<_, String>`
messages and silent event handling. Their shared implementation opens one
connection per request. A subscription waits for acknowledgement before it
spawns the event reader, keeps the socket writer alive, and awaits each channel
send. A `TAG_EVENT` containing `Restart` remains an ordinary event; `TAG_RESTART`
delivers a restart and ends the reader.

Public imports through `allmystuff_node::node_control` remain compatible.
The terminal's private `client` module retains the names used by its unchanged
callers. Runtime-owner arbitration, server binding and permissions, dispatch
and child-process supervision stay in their original callers. The terminal's
wait/retry helper also stays there. The GUI still depends on the node for those
other operations, and mobile still embeds its existing engine.

`NodeClient::connect` exposes the existing connection operation for the Windows
host's named-pipe owner inspection. Its visibility changes at the crate boundary;
the owner check and its additional error context remain in the node.

The wire remains `[u32 BE length][tag][payload]`, with the tag counted in the
length, a 256 MiB read ceiling, and the existing JSON defaults and explicit
nulls. An incomplete four-byte length prefix returns `None`; EOF after that
prefix is an error. Framing acceptance and write-side integer conversion are
unchanged. The address remains `MYOWNMESH_HOME/allmystuff-node.sock` on Unix
(falling back to the existing profile-home helper), or `allmystuff-node` as a
namespaced pipe on Windows. There are no client connection options, pooling,
automatic retries or new timeouts.

Arbitrary endpoints are available only to the crate's test build. This package
does not expose the MyOwnMesh daemon protocol as a new API or grant authority
to execute node commands. Code relocation changes defining-crate and diagnostic
source-location metadata; wire behavior, error text/chains and logging target
are preserved.

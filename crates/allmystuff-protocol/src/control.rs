//! A hand-kept mirror of the MyOwnMesh daemon's control protocol
//! (`MyOwnMesh/crates/myownmesh/src/control.rs` + `ipc/wire.rs`).
//!
//! Wire format is line-delimited JSON over a local socket — a unix-domain
//! socket at `~/.myownmesh/daemon.sock`, a namespaced pipe on Windows.
//! AllMyStuff talks to `myownmesh serve` exactly the way the MyOwnMesh GUI
//! and MyOwnLLM do, and — like them — mirrors the wire shapes here rather
//! than depending on `myownmesh-core`, so the app builds and tests without
//! the engine workspace present. A drift between this mirror and the
//! daemon surfaces as a JSON parse error on the receiving end, never
//! silent corruption.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The daemon's per-connection client handle id. Transparent over a
/// `u64` on the wire, matching `myownmesh::ipc::ClientId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClientId(pub u64);

/// Client → daemon request. One JSON object per line, dispatched on `op`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    // ---- status / identity / networks --------------------------------
    Status,
    NetworksList,
    PeersList {
        network: String,
    },
    RosterList {
        network: String,
    },
    RosterApprove {
        network: String,
        device_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    RosterRemove {
        network: String,
        device_id: String,
    },
    IdentityShow,
    IdentitySetLabel {
        label: String,
    },
    NetworkIdGenerate,
    ConfigShow,
    NetworkAdd {
        config: Value,
    },
    NetworkRemove {
        network: String,
    },
    NetworkUpdate {
        config: Value,
    },

    // ---- event stream ------------------------------------------------
    /// Upgrade this connection to a duplex event socket. After the ack,
    /// every server-initiated line is a [`ServerOut`].
    EventsSubscribe,

    // ---- typed channels (how AllMyStuff syncs with peers) ------------
    ChannelSubscribe {
        client_id: ClientId,
        network: String,
        channel: String,
    },
    ChannelUnsubscribe {
        client_id: ClientId,
        network: String,
        channel: String,
    },
    ChannelSendTo {
        network: String,
        channel: String,
        peer: String,
        payload: Value,
    },
    ChannelSendAll {
        network: String,
        channel: String,
        payload: Value,
    },

    // ---- generic RPC (request/response with one peer) ----------------
    RpcRegister {
        client_id: ClientId,
        network: String,
        method: String,
        streaming: bool,
    },
    RpcUnregister {
        client_id: ClientId,
        network: String,
        method: String,
    },
    RpcRespond {
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ok: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    RpcCall {
        network: String,
        peer: String,
        method: String,
        payload: Value,
    },

    /// Replace this node's advertised capability matrix on a network.
    CapabilitiesSet {
        network: String,
        capabilities: Value,
    },

    // ---- self-update -------------------------------------------------
    UpdateStatus,
    UpdateCheck,
    UpdateApply,
    UpdateSetPrefs {
        prefs: Value,
    },
}

/// Daemon → client reply to a one-shot [`Request`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Response {
    pub fn ok(data: Value) -> Self {
        Response {
            ok: true,
            error: None,
            data: Some(data),
        }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        Response {
            ok: false,
            error: Some(msg.into()),
            data: None,
        }
    }
}

/// Server-initiated frame on a duplex (post-`EventsSubscribe`) socket,
/// dispatched on `kind`. Mirrors `myownmesh::ipc::ServerOut`. Unknown
/// kinds are ignored by design, so the daemon can add variants without
/// breaking us.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerOut {
    /// A live mesh event (peer state / phase / diagnostics). Carried as
    /// opaque JSON — AllMyStuff forwards it to the front-end as-is, the
    /// same way the MyOwnMesh GUI does.
    Event {
        event: Value,
    },
    /// The subscriber fell behind; `skipped` events were dropped.
    Lagged {
        skipped: u64,
    },
    /// An inbound RPC from a peer for a method this client registered.
    RpcInbound {
        network: String,
        from: String,
        request_id: String,
        method: String,
        payload: Value,
        streaming: bool,
    },
    /// A frame on a typed channel this client subscribed to — the
    /// transport for AllMyStuff's presence / route / share messages.
    ChannelInbound {
        network: String,
        from: String,
        channel: String,
        payload: Value,
    },
    RpcCallStreamChunk {
        request_id: String,
        payload: Value,
    },
    RpcCallStreamEnd {
        request_id: String,
        #[serde(default)]
        error: Option<String>,
    },
    HandlerDisplaced {
        network: String,
        method: String,
        by: String,
    },
}

/// Default daemon control-socket location, recomputed locally so the
/// build stays independent of `myownmesh-core` (matching the GUI's
/// `ControlClient::new`). Honours `MYOWNMESH_HOME`.
#[cfg(unix)]
pub fn default_socket_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("MYOWNMESH_HOME")
        .map(std::path::PathBuf::from)
        .or_else(dirs_home)?;
    Some(home.join(".myownmesh").join("daemon.sock"))
}

#[cfg(unix)]
fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

/// On Windows the daemon listens on a namespaced pipe rather than a
/// filesystem path.
#[cfg(not(unix))]
pub fn default_pipe_name() -> &'static str {
    "myownmesh.sock"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_tags_on_op() {
        let j = serde_json::to_value(Request::Status).unwrap();
        assert_eq!(j["op"], "status");

        let j = serde_json::to_value(Request::PeersList {
            network: "home".into(),
        })
        .unwrap();
        assert_eq!(j["op"], "peers_list");
        assert_eq!(j["network"], "home");
    }

    #[test]
    fn client_id_is_a_bare_number() {
        let j = serde_json::to_value(ClientId(42)).unwrap();
        assert_eq!(j, serde_json::json!(42));
    }

    #[test]
    fn channel_send_round_trips() {
        let req = Request::ChannelSendAll {
            network: "home".into(),
            channel: "allmystuff/presence".into(),
            payload: serde_json::json!({"hello": true}),
        };
        let line = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&line).unwrap();
        assert!(matches!(back, Request::ChannelSendAll { .. }));
    }

    #[test]
    fn server_out_dispatches_on_kind() {
        let frame = serde_json::json!({
            "kind": "channel_inbound",
            "network": "home",
            "from": "peerid",
            "channel": "allmystuff/presence",
            "payload": {"x": 1}
        });
        let out: ServerOut = serde_json::from_value(frame).unwrap();
        match out {
            ServerOut::ChannelInbound { channel, .. } => assert_eq!(channel, "allmystuff/presence"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn unknown_response_fields_are_tolerated() {
        // The daemon's Response may carry extra keys; we only need ok/error/data.
        let r: Response =
            serde_json::from_str(r#"{"ok":true,"data":{"a":1},"extra":"ignored"}"#).unwrap();
        assert!(r.ok);
        assert_eq!(r.data.unwrap()["a"], 1);
    }
}

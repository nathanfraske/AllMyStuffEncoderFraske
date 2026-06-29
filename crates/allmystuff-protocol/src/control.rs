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

/// The daemon's per-connection client handle id. On the wire it's the
/// `Display` string `c<n>` (e.g. `"c42"`), exactly matching
/// `myownmesh::ipc::ClientId` — the daemon parses it back with
/// `FromStr`, so a bare number would be rejected. The daemon hands a
/// client its id in the `EventsSubscribe` ack (`data.client_id`); the
/// client passes it back on every channel/RPC op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientId(pub u64);

impl std::fmt::Display for ClientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "c{}", self.0)
    }
}

impl std::str::FromStr for ClientId {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let n = s
            .strip_prefix('c')
            .ok_or_else(|| format!("ClientId must start with 'c', got '{s}'"))?;
        Ok(ClientId(
            n.parse().map_err(|e| format!("ClientId parse: {e}"))?,
        ))
    }
}

impl Serialize for ClientId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ClientId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

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
        /// Also purge the network's persisted **governance state + roster** — a
        /// genuine *forget* (e.g. leaving a fleet), not just unloading the live
        /// network. Default `false` so disabling/refreshing a network keeps its
        /// signed state for a later rejoin; only a deliberate leave sets it.
        /// Matches the daemon's field; leaving it on disk is what makes a
        /// leave-then-rejoin reload a stale (forked) genesis.
        #[serde(default)]
        purge: bool,
    },
    NetworkUpdate {
        config: Value,
    },
    /// Reconnect a joined network in place — redial signaling and renegotiate
    /// ICE without leaving the room (the non-destructive twin of
    /// `NetworkRemove` + `NetworkAdd`). Peers keep their sessions and
    /// app-level state, so this is what the GUI's refresh / reconnect controls
    /// drive. `peer` omitted reconnects every peer on the network; `peer` set
    /// reconnects just that one (a per-node refresh). Mirrors the daemon's
    /// `control::Request::NetworkReconnect`.
    NetworkReconnect {
        network: String,
        #[serde(default)]
        peer: Option<String>,
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
        /// The peer's **bare pubkey** (`public_id`). The daemon's peer set is
        /// keyed by what signaling announces — a display id (`pubkey-SUFFIX`,
        /// the form `IdentityShow` and presence carry) misses it and the
        /// daemon replies "peer not found". Strip the suffix before sending.
        peer: String,
        payload: Value,
    },
    ChannelSendAll {
        network: String,
        channel: String,
        payload: Value,
    },

    // ---- video track lane (real RTP media, not the data channel) -----
    /// Write one encoded H.264 access unit (Annex-B, base64) onto the
    /// video track lane to `peer` (bare pubkey, like `ChannelSendTo`).
    /// The lane is provisioned on every connection at negotiation;
    /// `duration_us` paces the RTP clock (1/fps).
    VideoSend {
        network: String,
        peer: String,
        /// Which of the peer's video lanes to write to. Defaults to 0 so a
        /// pre-pool daemon (which ignores the field) still drives its single
        /// lane; a lane-pool daemon (myownmesh ≥ 0.2.7) routes it to that track.
        #[serde(default)]
        stream: u8,
        duration_us: u64,
        data: String,
    },
    /// Route assembled video access units from this network's peers to
    /// this client's event socket as `video_inbound` frames.
    VideoSubscribe {
        client_id: ClientId,
        network: String,
    },
    VideoUnsubscribe {
        client_id: ClientId,
        network: String,
    },

    // ---- audio track lane (real RTP media, not the data channel) -----
    /// Write one encoded Opus frame (base64) onto the audio track lane
    /// to `peer` (bare pubkey, like `ChannelSendTo`). The lane is
    /// provisioned on every connection at negotiation; `duration_us` is
    /// the frame length (20 000 for the canonical Opus frame).
    AudioSend {
        network: String,
        peer: String,
        /// Which of the peer's audio lanes to write to (defaults to 0, exactly
        /// like [`Request::VideoSend`]).
        #[serde(default)]
        stream: u8,
        duration_us: u64,
        data: String,
    },
    /// Route audio frames from this network's peers to this client's
    /// event socket as `audio_inbound` frames.
    AudioSubscribe {
        client_id: ClientId,
        network: String,
    },
    AudioUnsubscribe {
        client_id: ClientId,
        network: String,
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

    // ---- closed-network governance (forwarded to the myownmesh daemon) ----
    //
    // AllMyStuff drives the daemon's closed-network governance + the
    // per-device custody MFA that gates owner/kind changes. These mirror the
    // daemon's `op`/snake_case wire shapes (stringly-typed `to`/`role`, which
    // the daemon's NetworkKind/Role deserialise from snake_case) so the bytes
    // match without depending on `myownmesh-core` here.
    /// Snapshot the signed governance state (kind, roles, transition log,
    /// pending proposals, splits) for a network.
    GovernanceState {
        network: String,
    },
    /// Propose a kind change. `to` is `"open"` or `"closed"`.
    GovernanceProposeKindChange {
        network: String,
        to: String,
        /// Per-device custody second factor, when this device enrolled one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mfa_code: Option<String>,
    },
    /// Propose granting `target` a role: `"member"` | `"controller"` | `"owner"`.
    GovernanceProposeRoleGrant {
        network: String,
        target: String,
        role: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mfa_code: Option<String>,
    },
    /// Propose revoking `target`'s role (back to member).
    GovernanceProposeRoleRevoke {
        network: String,
        target: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mfa_code: Option<String>,
    },
    /// Propose evicting `target` from the closed network's roster entirely
    /// — the propagating removal a fleet uses to kick a lost/stolen device.
    GovernanceProposeEvict {
        network: String,
        target: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mfa_code: Option<String>,
    },
    /// Sign a pending proposal.
    GovernanceSign {
        network: String,
        proposal_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mfa_code: Option<String>,
    },
    /// Deny a pending proposal (single-shot kill switch).
    GovernanceDeny {
        network: String,
        proposal_id: String,
    },
    /// Withdraw a proposal this device floated.
    GovernanceWithdraw {
        network: String,
        proposal_id: String,
    },
    /// Spawn a proposer-initiated split. Returns the derived network id.
    GovernanceSpawnSplit {
        network: String,
        proposal_id: String,
    },
    /// Enroll a per-device TOTP custody lock for `network`. Returns the
    /// secret (base32 + `otpauth://` URI) and one-time recovery codes.
    GovernanceMfaEnroll {
        network: String,
    },
    /// Whether this device holds a custody enrollment for `network`.
    GovernanceMfaStatus {
        network: String,
    },
    /// Remove the custody lock for `network` (requires a valid code).
    GovernanceMfaDisable {
        network: String,
        code: String,
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
    fn client_id_serialises_as_the_c_string() {
        // The daemon parses client_id with FromStr ("c42"), so it must go
        // on the wire as that string — never a bare number.
        let j = serde_json::to_value(ClientId(42)).unwrap();
        assert_eq!(j, serde_json::json!("c42"));
        let back: ClientId = serde_json::from_value(serde_json::json!("c42")).unwrap();
        assert_eq!(back, ClientId(42));
        assert!(serde_json::from_value::<ClientId>(serde_json::json!(42)).is_err());
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

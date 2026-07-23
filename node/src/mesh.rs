//! The live mesh: wires the daemon's typed channels to the
//! [`allmystuff_session::Session`] state machine and the [`AudioBridge`].
//!
//! On start it subscribes to the AllMyStuff presence / control / media
//! channels on every joined network, broadcasts this node's
//! [`NodeProfile`], and pumps inbound frames:
//!
//!  * **presence** → updates the peer set (the graph fills with real peers).
//!  * **control** → drives the route handshake; the [`Effect`]s it returns
//!    send replies and start/stop audio.
//!  * **media** → audio frames fed to the playback side of active routes.
//!
//! Everything the front-end sees comes through `allmystuff://session`
//! snapshots emitted after each change.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};

use crate::UiSink;

use allmystuff_graph::{Grant, MediaKind, NodeId, Person, PersonId, Route};
use allmystuff_protocol::control::{MAX_MEDIA_FRAME_BYTES, MEDIA_KIND_AUDIO, MEDIA_KIND_VIDEO};
use allmystuff_protocol::{
    claim_code_network_id, format_claim_code, AppControl, ClientId, ControlMessage, KvmControl,
    NodeProfile, OwnedMember, OwnedRoster, OwnershipControl, Request, RoomMessage, RouteControl,
    ShareControl, SharedFileMeta, SiteControl, SiteService, TerminalSessionInfo, CHANNEL_CONTROL,
    CHANNEL_MEDIA, CHANNEL_PRESENCE, CHANNEL_ROOMS, FEATURE_MEDIA_INCARNATION,
    FEATURE_ROUTE_INCARNATION, FEATURE_ROUTE_TEARDOWN_ACK, LOCAL_CLAIM_NETWORK_ID,
    PROTOCOL_VERSION,
};
use allmystuff_session::{
    AudioFrame, ClipboardContentKind, ClipboardEvent, ClipboardFrame, ClipboardItem, Effect,
    FileEvent, FileFrame, InputAction, InputEvent, MediaPayload, RouteState, Session, SiteEvent,
    SiteFrame, TermEvent, TermFrame, VideoAssembler, VideoFrame, VideoStatusFrame,
    CLIPBOARD_CHUNK_BYTES, SITE_CHUNK_BYTES,
};

use crate::audio::{
    AudioBridge, AudioProfile, CaptureSource, OpusDecodeKind, OpusReceiver, OpusStream,
};
use crate::clipboard::{ClipboardService, LocalClip};
use crate::control_client::{ControlClient, MediaPipe, MediaTrackPipe, ProfiledInboundFrame};
use crate::files::FilesPlane;
use crate::input_inject::Injector;
use crate::media_policy::{
    EffectivePlan, MediaCapabilities, MediaMode, MediaPolicyController, PolicyEnvelope,
    PolicyPayload, PolicyRequest, AUDIO_HANDOFF_PACKETS, VIDEO_HANDOFF_FRAMES,
};
use crate::ownership::Ownership;
use crate::shares::Shares;
use crate::sites::{ClientMapping, SitesProxy};
use crate::terminal::{OutMsg, TerminalHost};
use crate::video::{VideoBridge, VideoMode, VideoPacket, VideoSource};
use crate::video_decode::{Au, DecodeBridge};
use std::time::{Duration, Instant};

type EffectivePlanEchoKey = (String, Option<String>);
type EffectivePlanEcho = (u64, EffectivePlan);

pub struct Mesh {
    client: Arc<ControlClient>,
    /// The media plane's dedicated daemon connection: frame chunks ride it
    /// back-to-back instead of paying a connect + round trip each.
    /// The legacy/general local-daemon writer. Non-A/V application traffic
    /// stays on this single pipe; only audio and video receive isolated IPC
    /// writers below.
    media_pipe: MediaPipe,
    realtime_video_pipe: MediaPipe,
    audio_pipe: MediaPipe,
    background_video_pipe: MediaPipe,
    audio_track_pipe: MediaTrackPipe,
    /// Where node events surface. The GUI wires this to Tauri's event bus
    /// (`app.emit`); the headless `allmystuff serve` binary uses a logging
    /// sink — the events are all front-end concerns, so a node with no UI
    /// simply drops them. See [`crate::UiSink`].
    sink: Arc<dyn UiSink>,
    audio: Arc<AudioBridge>,
    /// Screen + camera capture for the display/video routes this machine
    /// sources (the far end of a console session looking at us, a room
    /// member watching our camera).
    video: Arc<VideoBridge>,
    /// Native H.264 decode for inbound display routes whose console window
    /// asked for ready-to-paint frames (no WebCodecs in its webview, or its
    /// decoder stalled out).
    video_decode: Arc<DecodeBridge>,
    /// M2 — the pacer's requested-vs-actual gap ledger (one log line per
    /// minute): the honesty check on every sub-millisecond spacing the
    /// drain model asks for.
    pace_gaps: Mutex<PaceGapStats>,
    /// M3 + the chunk-train bandwidth estimator: per inbound video route,
    /// arrival dispersion of the pacer's own timed bursts → a bottleneck
    /// estimate and a one-way-delay trend, attached to every outbound
    /// [`RouteControl::VideoFeedback`] (the ICE datapath's control
    /// channel — never signaling).
    video_arrivals: Mutex<HashMap<String, ArrivalState>>,
    /// Keyboard/mouse injection for input routes that sink here — gated on
    /// the sender being our owner or a fleet member.
    injector: Injector,
    /// Mesh-native terminal sessions: PTYs this machine hosts for terminal
    /// routes sourcing here (gated like input injection), and the output
    /// buffers terminal windows drain for routes sinking here.
    terminal: TerminalHost,
    /// Sequence for viewer-side outbound terminal frames (keystrokes,
    /// resizes — one stream per app run, like `input_seq`).
    term_seq: AtomicU64,
    /// Viewer-route ids that already have a live host/loopback output pump.
    /// Exactly one pump per route: a duplicate `StartMedia` — e.g. the offer
    /// delivered on more than one shared network — must not spawn a second
    /// pump onto the same route, which would fan the shell's output out twice
    /// (the cause of doubled/tripled terminals on a multi-network peer).
    term_pumps: Mutex<std::collections::HashSet<String>>,
    /// Highest terminal-frame `seq` already taken per route, each direction:
    /// `term_rx_seq` is output the *viewer* takes from the host;
    /// `term_in_seq` is input the *host* takes from the viewer. Both sending
    /// sides number a route's frames strictly increasing, so a seq we've
    /// already seen is a duplicate delivery (the same send arriving on several
    /// shared networks) — dropped, not re-applied. Without the input one, a
    /// keystroke redelivered N times is written to the PTY N times and the
    /// shell echoes `aaaa`.
    term_rx_seq: Mutex<HashMap<String, u64>>,
    term_in_seq: Mutex<HashMap<String, u64>>,
    /// Mesh-native file sessions: filesystem ops this machine hosts for
    /// files routes sourcing here (gated like the terminal), and the
    /// response buffers files windows drain for routes sinking here.
    files: FilesPlane,
    /// Sequence for outbound file frames (requests viewer-side, response
    /// streams host-side — one stream per app run, like `term_seq`).
    file_seq: AtomicU64,
    /// Mesh-native sites: this machine's exposed-service allow-list + the
    /// live reverse-proxy connections (client mappings sinking here, host
    /// tunnels sourcing here). See [`SitesProxy`].
    sites: SitesProxy,
    /// Sequence for outbound site frames (one stream per app run, like the
    /// other media-plane sequences).
    site_seq: AtomicU64,
    /// Client mappings currently being auto-re-mapped after a reject (keyed
    /// `<pubkey>:<host_port>`), so a burst of rejects can't spawn a stampede of
    /// competing heal tasks for the same tunnel.
    site_remap_inflight: Mutex<std::collections::HashSet<String>>,
    /// Per-route rate limit for the dead-site-route NACK (last send `Instant`),
    /// so a peer draining a full pipe onto a route we no longer hold gets one
    /// Reject, not one per frame.
    site_nack_at: Mutex<HashMap<String, std::time::Instant>>,
    /// Viewer-side download sinks: a `(route, req)` whose `Chunk`s should
    /// stream straight to a local file (the Downloads folder) instead of
    /// the window's queue — registered by `file_download` *before* the
    /// Read request goes out, so the first chunk can't race it.
    downloads: Mutex<HashMap<(String, u64), DownloadSink>>,
    /// Host-side **Shared Files** registry: the files this machine has
    /// offered into rooms, keyed by the opaque token the uploader handed
    /// out. A `:shared` route can only `Fetch` by token (never browse a
    /// path), and a fetch is served only when the requester's pubkey is in
    /// the token's `allowed` set (the room's members) — so a call's shared
    /// area never becomes a way to read the disk. Bytes flow straight to
    /// the downloader; the room host only ever carries the *list*.
    shared: Mutex<HashMap<String, SharedReg>>,
    state: Mutex<State>,
    /// Serializes joined-network snapshots from fetch through commit so an
    /// older async response cannot overwrite a newer daemon session.
    network_sync_serial: tokio::sync::Mutex<()>,
    /// Serializes daemon peer snapshots. Inbound observations remain
    /// independent and are never erased by these snapshot refreshes.
    peer_refresh_serial: tokio::sync::Mutex<()>,
    /// This device's persisted ownership record — who owns it and whether
    /// it's currently offering itself for adoption (claim mode).
    ownership: Arc<Ownership>,
    /// Canonical pubkeys authorised to control this device — the fleet's
    /// **closed-network signed roster**, cached from the daemon (`RosterList`
    /// for `ownership.fleet_network_id()`). [`Mesh::sender_may_control`] trusts
    /// THIS alone: membership is established by the owner founding a genuinely
    /// closed network (founder self-election) and admitting members into the
    /// signed roster, so no unauthenticated gossip can grant control — closing
    /// the fleet-conscription takeover (AMS-01). Refreshed on ownership changes
    /// and on a periodic tick.
    fleet_authorized: Mutex<std::collections::HashSet<String>>,
    /// Canonical pubkeys of devices THIS node has sent an ownership `Claim` to
    /// and is awaiting a `Claimed` confirmation from. An inbound
    /// `OwnershipControl::Claimed` is honoured only when its authenticated
    /// sender is in this set — and the entry is consumed on use — so an
    /// *unsolicited* `Claimed` from an arbitrary peer can't drive itself into
    /// this device's fleet member list and signed roster (which
    /// [`Mesh::sender_may_control`] trusts), i.e. can't hand itself control of
    /// this machine. The outbound-claim mirror of the per-sender guards the
    /// other ownership arms already apply. In-memory only: a claim interrupted
    /// by a restart simply needs re-issuing.
    pending_claims: Mutex<std::collections::HashSet<String>>,
    /// Latest passive clock-skew sample per peer (ms; positive = the peer's
    /// wall clock reads ahead of ours) with when it landed, from the
    /// `sent_at` stamp presence adverts carry. Fed to the network verdict in
    /// [`Mesh::note_peer_clock`]; stale entries age out of the vote rather
    /// than an offline peer's old clock voting forever.
    peer_clock_skew: Mutex<HashMap<String, (i64, std::time::Instant)>>,
    /// Whether the "this device's clock is out of sync" warning is currently
    /// raised — latched so it fires once per episode (and clears once), not
    /// on every presence advert while the clock stays wrong.
    clock_skew_warned: std::sync::atomic::AtomicBool,
    /// When each outbound route offer was first seen still-unanswered by the
    /// reaper sweep ([`Mesh::spawn_offer_reaper`]). An offer has no deadline
    /// in the wire protocol and the session is clock-free, so this is where
    /// "awaiting accept" gets its timer; entries leave when the route stops
    /// being an outbound `Offered`.
    offer_first_seen: Mutex<HashMap<(String, Option<String>), std::time::Instant>>,
    /// User-owned outbound route intent, independent of the daemon-backed
    /// [`Session`]. A daemon event-socket restart destroys observed routes,
    /// lanes, and media resources, but it must not silently turn an open
    /// console off. Bring-up replays these exact endpoint/media requests with
    /// a fresh wire incarnation. Explicit disconnect and peer terminal
    /// responses remove the intent before any asynchronous cleanup begins.
    desired_routes: Mutex<HashMap<String, DesiredRoute>>,
    /// Process-local generation returned to GUI callers. It fences a delayed
    /// local close from route A after deterministic route id reuse has already
    /// installed route B, including legacy peers that have no wire
    /// incarnation.
    route_intent_generation: AtomicU64,
    /// Exact teardowns awaiting an application-level acknowledgement. The
    /// daemon's reliable-send ack is transport-level and can succeed with no
    /// peer app subscriber, so these are retried on the existing offer sweep.
    pending_teardowns: Mutex<HashMap<(String, Option<String>), PendingTeardown>>,
    /// The daemon-link status as last emitted on `allmystuff://subscription`
    /// — answered back by [`Mesh::mesh_status`], because the emit itself is
    /// one-shot and a late-subscribing GUI misses it.
    last_status: Mutex<(String, Option<String>)>,
    /// Short-lived reliable-control workers, one per exact route lifetime.
    /// Independent routes cannot head-of-line block each other. Each worker's
    /// pending mailbox has one coalescing slot per reliable protocol kind, so
    /// repeated state cannot grow an unbounded FIFO while a daemon send stalls.
    reliable_control_workers: Arc<Mutex<HashMap<ReliableControlKey, ReliableControlWorkerHandle>>>,
    reliable_control_worker_seq: AtomicU64,
    /// Cancels every in-flight reliable request as soon as a daemon session is
    /// retired. A transport acknowledgement from a replacement daemon must
    /// never make an old Offer or Teardown look delivered.
    reliable_control_epoch: watch::Sender<u64>,
    /// Epoch and event-subscriber client id installed by the current bring-up.
    /// This is separate from the route Session so worker fencing can be checked
    /// without treating a partially-reset State as current.
    active_daemon_context: Arc<Mutex<Option<DaemonContext>>>,
    /// Last non-empty fleet roster we read from the closed network's signed
    /// roster (`fleet_roster_value`). A member-side resilience cache — the
    /// symmetric twin of the owner's durable `fleet_members()` fallback: the
    /// signed roster is the source of truth, but it's momentarily unreadable
    /// while the fleet's closed network is mid-(re)join, and during that gap a
    /// co-member must not flicker to "another fleet". A non-empty read always
    /// replaces this, so an eviction propagates the instant the roster is
    /// readable again — we never resurrect a removed member.
    fleet_roster_cache: Mutex<Vec<OwnedMember>>,
    /// Durable share relationships — who I share with and the grants in each
    /// direction. Node-owned (enforcement lives here), persisted beside the
    /// ownership record, and projected into [`Mesh::snapshot`] so the GUI
    /// renders a peer as *shared* with its grants across a restart.
    shares: Arc<Shares>,
    /// Outbound audio: capture callbacks push `(peer, frame)`; a forwarder
    /// task sends them on the media channel. Bounded like video: a stalled
    /// link sheds buffers (a brief skip) instead of queueing a backlog the
    /// listener then hears seconds late.
    audio_out: mpsc::Sender<AudioOut>,
    /// The matching receiver, parked by [`Mesh::new`] and drained by the
    /// forwarder task [`Mesh::start`] spawns. It lives here rather than being
    /// spawned in `new` because the GUI builds the `Mesh` in a *synchronous*
    /// Tauri `setup` (no ambient Tokio runtime to spawn on); `start` is the
    /// first point guaranteed an async context, and on the same runtime
    /// everything else runs on.
    ///
    /// Video deliberately does not have a process-global class queue. Each
    /// capture route gets its own one-AU queue and persistent local-daemon
    /// writer in [`Self::start_video_stream`]. Besides eliminating cross-route
    /// head-of-line blocking, keeping a route on one ordered writer prevents
    /// a focus change from sending adjacent dependent AUs down two sockets
    /// that the daemon could service in the opposite order.
    audio_rx: Mutex<Option<mpsc::Receiver<AudioOut>>>,
    /// Peer-wide budgets, focus election, requested/effective policy, and the
    /// viewer's cached remote plans. It contains no transport state.
    media_policy: Mutex<MediaPolicyController>,
    /// Orders one local policy mutation with the VideoBridge changes produced
    /// from its plans. Without this outer transaction, a newer controller
    /// value can be followed by an older route restart that was still carrying
    /// its previously sampled cap.
    video_policy_apply_serial: Mutex<()>,
    /// Latest effective-plan echo per route. ICE-path control sends can wait
    /// on a slow daemon, so the inbound event pump only updates this queue and
    /// one detached worker drains it. Replacements coalesce by route.
    effective_plan_echoes: Mutex<HashMap<EffectivePlanEchoKey, EffectivePlanEcho>>,
    effective_plan_echo_running: AtomicBool,
    effective_plan_echo_epoch: AtomicU64,
    /// Last full legacy Tune fields sent for each watched route. A v1
    /// priority-only message repeats them so an older peer that ignores the
    /// extension keeps its current quality instead of resetting to Auto.
    requested_video_tunes: Mutex<HashMap<String, LegacyVideoTune>>,
    /// Sequence for outbound input events (one stream per app run).
    input_seq: AtomicU64,
    /// Highest injected input sequence per exact route lifetime. A daemon
    /// reconnect bug can leave duplicate subscriber pumps alive; both pumps
    /// then deliver the same destructive key or button event. Ordered SCTP
    /// input accepts each sequence once and resets with the route lifetime.
    input_in_seq: Mutex<HashMap<(String, Option<String>), u64>>,
    /// Sequence for outbound clipboard frames (one stream per app run, like
    /// `input_seq` — clipboard rides alongside control).
    clipboard_seq: AtomicU64,
    /// Transfer ids for outbound clipboard image/file pastes — scopes a
    /// transfer's chunks, separate from the per-frame `clipboard_seq`.
    clipboard_transfer: AtomicU64,
    /// The OS clipboard on its own thread — reads on paste, writes on
    /// receipt (see [`crate::clipboard`]).
    clipboard: ClipboardService,
    /// Inbound clipboard transfers being reassembled, keyed by (route,
    /// transfer id). Image bytes accumulate in memory; file bytes stream to
    /// a per-transfer staging dir.
    clip_inbound: Mutex<HashMap<(String, u64), ClipInbound>>,
    /// When we last sent a clipboard [`Pull`](ClipboardEvent::Pull) per route
    /// — the gate that lets the remote's reply land on *our* clipboard. Only a
    /// reply that arrives within [`CLIPBOARD_PULL_WINDOW`] of our own pull is
    /// accepted, so a misbehaving peer can't clobber our clipboard unasked.
    clip_pull_at: Mutex<HashMap<String, std::time::Instant>>,
    /// Our presence boot id — how peers detect that we (re)started and answer
    /// with their state (see `NodeProfile::boot`). Seeded once per app run, but
    /// **refreshed whenever a local network reset drops our peer caches** (see
    /// [`Mesh::prune_unjoined_peers`]): the reset discards everything we knew
    /// about each peer, so we are a fresh incarnation as far as their state is
    /// concerned, and a new boot id is exactly what makes them re-send it.
    /// Without the bump, a network refresh on one side left the *other* side
    /// (same boot id, peer still "known") silent, stranding the connection
    /// until both sides refreshed or an app restarted.
    /// Presence boot id and route sequence under one lock. They must advance
    /// atomically or a reset can mint `new_boot:N` before `new_boot:1`, making
    /// the receiver reject the real successor as older.
    route_incarnation_clock: Mutex<RouteIncarnationClock>,
    /// Daemon-session fence captured by every inbound binary media-source
    /// task. A task from an old event subscription may remain alive if its
    /// pipe does; it must not dispatch frames into the replacement Session.
    daemon_session_epoch: Arc<AtomicU64>,
    /// Reassembles chunked inbound video frames (a frame bigger than the
    /// data channel's ~64 KiB message ceiling arrives in pieces).
    video_in: Mutex<VideoAssembler>,
    /// Per-route queues of ready-to-ship packets (28-byte header +
    /// payload) for the console windows watching inbound video. The
    /// webview *pulls* these (`video_poll`, one drain per display
    /// refresh): a pull that fails costs one tick, where the previous
    /// push channel's ordered delivery meant one lost message silently
    /// froze the stream forever while the backend kept counting frames.
    video_watchers: Mutex<VideoWatchRegistry>,
    /// Process-random seed plus a monotonic increment for local watcher
    /// claims. A GUI surviving a node restart must never have its old token
    /// alias the first watcher created by the replacement process.
    video_watch_token: AtomicU64,
    /// Whether the local daemon speaks the video track lane (`video_*`
    /// ops, myownmesh ≥ 0.2.1). Probed at session start; while false the
    /// app neither offers nor picks H.264 — screen shares ride MJPEG and
    /// a single loud log says why. This is what keeps a stale daemon a
    /// slow stream instead of a black one.
    daemon_video: std::sync::atomic::AtomicBool,
    /// Subscription health is network-scoped. A successful VideoSubscribe on
    /// mesh A says nothing about mesh B, yet the old global flag negotiated
    /// H.264 to peers reachable only on B and produced a route with no inbound
    /// video sink. Successful entries are retained and failed entries are
    /// retried for the daemon session's lifetime.
    network_subscriptions: Mutex<HashMap<String, NetworkSubscriptionState>>,
    /// Serializes initial subscription passes with the background healer so
    /// two sync triggers do not issue duplicate requests for the same slot.
    subscription_serial: tokio::sync::Mutex<()>,
    /// Daemon session epoch whose retry worker is active. One worker observes
    /// the current joined set until reset advances `daemon_session_epoch`.
    subscription_retry_epoch: AtomicU64,
    /// Inbound per-route counters (frames, bytes), logged every few
    /// seconds — the receive half of the dial-in line the sender's
    /// `StreamStats` provides.
    video_in_stats: Mutex<HashMap<String, VideoInStats>>,
    /// Last emission per inbound-video diagnostic key — the rate limit
    /// behind [`Self::diag_ok`], so a dead stream explains itself once per
    /// [`WARN_EVERY`] instead of at frame rate.
    video_diag_last: Mutex<HashMap<String, std::time::Instant>>,
    /// When each inbound track lane was first seen carrying media that no
    /// route here maps to (key `deadlane:<media>:<peer>:<lane>`), cleared
    /// the moment the lane resolves. A lane-shaped NACK
    /// ([`RouteControl::DeadLane`]) is sent only once the condition has
    /// persisted a full [`WARN_EVERY`] — a stream's first samples can
    /// legally outrun the Accept/VideoLane control messages at start, and
    /// NACKing that instant would kill a healthy stream being born.
    dead_lane_since: Mutex<HashMap<String, std::time::Instant>>,
    /// When each route last asked its sender for a clean decode entry —
    /// decode errors arrive at frame rate; the asks must not.
    refresh_asks: Mutex<HashMap<String, std::time::Instant>>,
    /// Per-peer backoff state for the refresh round-trip ([`ControlMessage::
    /// ProfileRequest`]), so a held-down refresh can't hammer a peer. See
    /// [`Mesh::allow_profile_request`].
    profile_req: Mutex<HashMap<String, ProfileReqState>>,
    /// Per-route Opus decoders for inbound lane audio (stateful across
    /// frames; dropped with the route).
    audio_decoders: Mutex<HashMap<String, OpusReceiver>>,
    /// Policy-aware Opus encoders captured by the audio callbacks. Replacing
    /// one in place changes packetization/bitrate/FEC without reopening the
    /// OS capture device.
    audio_encoders: Mutex<HashMap<String, Arc<Mutex<OpusStream>>>>,
    /// Legacy outbound PCM captures still allowed only while their peer has no
    /// governed video plan. The first policy-managed video route stops them so
    /// raw PCM cannot silently exceed the peer-wide media cap.
    pcm_audio_routes: Mutex<HashMap<String, String>>,
    /// Whether the local daemon speaks the audio track lane (`audio_*`
    /// ops, myownmesh ≥ 0.2.4) — the audio twin of `daemon_video`.
    /// While false, audio rides PCM frames over the media channel.
    daemon_audio: std::sync::atomic::AtomicBool,
    /// How many media lanes the local daemon provisions per peer (from
    /// Status `media_lanes`); 1 means a pre-pool daemon.
    daemon_lanes: std::sync::atomic::AtomicU8,
    /// Whether the local daemon speaks the **binary media pipes**
    /// (`media_track_pipe` / `media_source_pipe`, from Status `media_pipes`).
    /// The version pin can't gate this — the feature predates a release — so
    /// it's a capability flag. While false, H.264/Opus ride the legacy base64
    /// `video_send`/`audio_send` ops and inbound arrives as base64 events, so
    /// an older daemon on the socket still streams (just with the base64 tax)
    /// instead of a black screen.
    daemon_media_pipes: std::sync::atomic::AtomicBool,
    /// **Host side:** the RTP video track lane pinned to each route we
    /// stream, by route id. Assigned once (lowest free in the peer's pool)
    /// when the stream starts and held until teardown, so an unrelated route
    /// coming or going never renumbers a live stream's lane. The viewer is
    /// told the binding ([`RouteControl::VideoLane`]) and demuxes by it.
    video_lane_pins: Mutex<HashMap<String, OutboundVideoLanePin>>,
    /// Process-local incarnation of each outbound video route. Route ids are
    /// intentionally stable across a rapid codec/source re-offer, so the id
    /// alone cannot tell a queued AU from the capture instance that produced
    /// it. The generation is never serialized: it only fences stale callbacks
    /// and queued work before they reach the existing media plane.
    video_route_generations: Mutex<VideoRouteGenerations>,
    /// Wire lifetime of each route whose local media resources are currently
    /// running. Stop effects must match this exact value, for every media kind,
    /// so a delayed predecessor stop cannot tear down a same-id successor.
    active_media_incarnations: Mutex<HashMap<String, Option<String>>>,
    /// Serializes every route-id keyed resource start/stop across async effect
    /// batches, direct disconnects, and same-id replacement.
    route_lifecycle_locks: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    /// A very small, process-local guard around a screen switch. The viewer
    /// tears the old display route down and offers the new one on separate
    /// local node-control requests; a delayed duplicate teardown can therefore
    /// land just after the successor activates. Route ids carry no incarnation,
    /// so without this narrow fence that close tears down the brand-new
    /// monitor and changing codec merely happens to start it again after the
    /// race. Local duplicates are watch-confirmed inside 100 ms; inbound ones
    /// wait a bounded 2.5 seconds for an existing ICE-path liveness control.
    /// Nothing here is serialized and no new message is sent over any channel.
    video_switch_guards: Mutex<VideoSwitchGuards>,
    /// **Viewer side:** the lane→route binding a streamer told us, per peer
    /// (canonical pubkey). Inbound H.264 on lane `L` from peer `P` belongs to
    /// `video_lane_binds[P][L]` — authoritative over the positional guess.
    /// Empty for a peer that doesn't announce (older build): that peer's lanes
    /// fall back to the positional sort.
    /// Keyed by `(network, canonical peer)`: lane ids are scoped to one
    /// WebRTC PeerSession, not globally to a peer that shares several meshes.
    video_lane_binds: Mutex<HashMap<(String, String), HashMap<u8, VideoLaneBinding>>>,
    /// The disabled-networks park store, when the embedding process shares
    /// one (the node binary's `network_set_enabled` seam). Consulted by
    /// [`Mesh::ensure_claim_networks`] so a deliberately switched-off local
    /// claim network *stays* off across claim-state changes instead of
    /// being silently re-joined — the network can't be left, so the park
    /// store is the only "off" it has, and it has to stick.
    disabled_networks: Mutex<Option<Arc<crate::networks_store::DisabledNetworks>>>,
    /// CEC Support state — the technician's dialed customers + Agent Name, and
    /// the customer's consent store + pending connect-requests. Empty and inert
    /// on a node that never joins the CEC ecosystem; when it does, its per-frame
    /// gate ([`Mesh::sender_may_drive`]) additively consults the consent store
    /// so a dialed technician's screen/input rides the very same engine, trusted
    /// by a live grant instead of owner/fleet. See [`crate::cec`].
    cec: crate::cec::Cec,
}

/// One captured-audio packet headed for the forwarder, in whichever
/// shape its route negotiated.
enum AudioOut {
    /// A PCM frame for `CHANNEL_MEDIA` — the floor every peer speaks.
    Channel(String, AudioFrame),
    /// One encoded Opus frame for the daemon's audio track lane.
    Lane {
        peer: String,
        route: String,
        duration_us: u64,
        data: Vec<u8>,
    },
}

#[derive(Clone, Default)]
struct LegacyVideoTune {
    max_edge: Option<u32>,
    bitrate: Option<u32>,
    fps: Option<u32>,
    game: bool,
    mode: Option<String>,
    peer_cap_bps: Option<u64>,
    priority: bool,
}

#[derive(Default)]
struct VideoWatchRegistry {
    current: HashMap<String, VideoWatcher>,
    standby: HashMap<String, Vec<VideoWatcher>>,
}

impl std::ops::Deref for VideoWatchRegistry {
    type Target = HashMap<String, VideoWatcher>;

    fn deref(&self) -> &Self::Target {
        &self.current
    }
}

impl std::ops::DerefMut for VideoWatchRegistry {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.current
    }
}

impl VideoWatchRegistry {
    /// Preserve window claims across a daemon/session reconnect while
    /// discarding every byte and decoder callback tied to the vanished media
    /// lifetime. This keeps an already-open viewer's token valid, but forces
    /// raw H.264 to wait for a clean entry and requires a real post-reconnect
    /// poll before the claim counts as liveness evidence.
    fn reset_for_reconnect(&mut self) {
        for watcher in self.current.values_mut().chain(
            self.standby
                .values_mut()
                .flat_map(|watchers| watchers.iter_mut()),
        ) {
            watcher.queue.clear();
            watcher.awaiting_key = !watcher.decode;
            watcher.last_poll = None;
            if watcher.decode {
                watcher.decode_epoch = next_native_decode_epoch();
            }
        }
    }

    fn reset_route_for_reconnect(&mut self, route_id: &str) {
        if let Some(watcher) = self.current.get_mut(route_id) {
            watcher.queue.clear();
            watcher.awaiting_key = !watcher.decode;
            watcher.last_poll = None;
            if watcher.decode {
                watcher.decode_epoch = next_native_decode_epoch();
            }
        }
        if let Some(watchers) = self.standby.get_mut(route_id) {
            for watcher in watchers {
                watcher.queue.clear();
                watcher.awaiting_key = !watcher.decode;
                watcher.last_poll = None;
                if watcher.decode {
                    watcher.decode_epoch = next_native_decode_epoch();
                }
            }
        }
    }

    fn remove(&mut self, route_id: &str) -> Option<VideoWatcher> {
        self.standby.remove(route_id);
        self.current.remove(route_id)
    }

    fn claim(&mut self, route_id: String, watcher: VideoWatcher) {
        let now = Instant::now();
        self.prune_standby(&route_id, now);
        if let Some(mut displaced) = self.current.insert(route_id.clone(), watcher) {
            displaced.queue.clear();
            // A never-polled registration has not proved that a live window
            // owns it. A recently polled claim may be restored if an obsolete
            // late registration displaced it and then immediately unwinds.
            if watcher_claim_is_recent(&displaced, now) {
                self.standby.entry(route_id).or_default().push(displaced);
            }
        }
    }

    /// Release one claim. If it owned the route, restore the most recently
    /// displaced live claim so a late obsolete registration cannot leave the
    /// intended window armed on a stale token with no backend owner.
    fn release(&mut self, route_id: &str, token: u64) -> Option<(bool, Option<bool>)> {
        if self
            .current
            .get(route_id)
            .is_some_and(|watcher| watcher.token == token)
        {
            let removed_decode = self.current.remove(route_id)?.decode;
            let now = Instant::now();
            self.prune_standby(route_id, now);
            let restored = self.standby.get_mut(route_id).and_then(Vec::pop);
            if self.standby.get(route_id).is_some_and(Vec::is_empty) {
                self.standby.remove(route_id);
            }
            let restored_decode = restored.as_ref().map(|watcher| watcher.decode);
            if let Some(mut watcher) = restored {
                watcher.queue.clear();
                watcher.awaiting_key = !watcher.decode;
                self.current.insert(route_id.to_string(), watcher);
            }
            return Some((removed_decode, restored_decode));
        }

        if let Some(standby) = self.standby.get_mut(route_id) {
            standby.retain(|watcher| watcher.token != token);
            if standby.is_empty() {
                self.standby.remove(route_id);
            }
        }
        None
    }

    fn prune_standby(&mut self, route_id: &str, now: Instant) {
        if let Some(standby) = self.standby.get_mut(route_id) {
            standby.retain(|watcher| watcher_claim_is_recent(watcher, now));
            if standby.is_empty() {
                self.standby.remove(route_id);
            }
        }
    }
}

fn watcher_claim_is_recent(watcher: &VideoWatcher, now: Instant) -> bool {
    watcher
        .last_poll
        .is_some_and(|last| now.saturating_duration_since(last) <= VIDEO_LOCAL_POLL_OBSERVE)
}

/// One console window's claim on a route's inbound packets: the queue it
/// drains plus the token that claim was made with — `video_unwatch`
/// removes the queue only when the token still matches, so a stale
/// unwatch (a torn-down watcher racing the next one over async IPC)
/// can't delete its successor's queue.
struct VideoWatcher {
    token: u64,
    /// Whether this window asked the backend to decode H.264 for it
    /// (raw RGBA frames out) instead of passing access units through.
    decode: bool,
    /// Stable across native-to-native watch handoff while one decoder worker
    /// remains live; renewed after any pass-through or teardown boundary.
    decode_epoch: u64,
    queue: std::collections::VecDeque<ViewerPacket>,
    /// Raw H.264 only: once a reference chain is dropped, dependent AUs are
    /// refused until a key unit arrives.
    awaiting_key: bool,
    /// Updated by the window's 16 ms safety poll even when no frame arrived.
    /// A post-disconnect-request poll is stronger liveness evidence than mere
    /// watcher presence because `video_unwatch` is fire-and-forget.
    last_poll: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatcherEnqueue {
    Accepted,
    Dropped,
    NeedsRefresh,
}

fn next_native_decode_epoch() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// One packet waiting at the local backend-to-viewer boundary. Profiling
/// metadata is process-local only: it is stripped while `video_poll` builds
/// the existing byte-identical length-prefixed batch.
struct ViewerPacket {
    bytes: Vec<u8>,
    profile_id: u64,
    frame_ts_us: Option<u64>,
    enqueued_at: Option<Instant>,
}

/// A fallback base64 video event pinned to the exact route generation that
/// owned its lane when the daemon reader admitted it. The consumer verifies
/// both facts again before decode, so a queued predecessor AU can never cross
/// a lane rebind into its successor.
struct QueuedVideoEvent {
    value: Value,
    route_id: String,
    generation: u64,
}

fn queued_video_binding_matches(
    current_route: Option<&str>,
    current_generation: Option<u64>,
    expected_route: &str,
    expected_generation: u64,
) -> bool {
    current_route == Some(expected_route) && current_generation == Some(expected_generation)
}

/// Resolve one inbound lane while the route-generation table is fenced.
///
/// Route ids are intentionally reused. Taking the lane and generation samples
/// in separate critical sections lets this sequence tag a predecessor AU as
/// its same-id successor: resolve route R, begin successor generation G2, read
/// R's generation as G2. Locking before lane resolution makes the returned
/// generation belong to the route ownership observed by that resolution.
fn snapshot_video_route_generation(
    generations: &Mutex<VideoRouteGenerations>,
    route_for_lane: impl FnOnce() -> Option<String>,
) -> (Option<String>, Option<u64>) {
    let generations = generations.lock();
    let route_id = route_for_lane();
    let generation = route_id
        .as_deref()
        .and_then(|route_id| generations.current(route_id));
    (route_id, generation)
}

/// Commit one queued access unit while its route generation is still current.
///
/// The generation guard stays held for the whole callback. A successor must
/// take the same guard before it advances the generation and clears the old
/// decoder/watcher state, so either this commit lands first and is then
/// flushed, or the successor lands first and this callback is never run.
fn commit_current_video_generation<T>(
    generations: &Mutex<VideoRouteGenerations>,
    route_id: &str,
    generation: u64,
    commit: impl FnOnce() -> T,
) -> Option<T> {
    let generations = generations.lock();
    generations.is_current(route_id, generation).then(commit)
}

fn needs_inbound_video_generation(route: &Route, local_node: &str) -> bool {
    matches!(route.media, MediaKind::Display | MediaKind::Video)
        && same_node(&node_of(route.to.as_str()), local_node)
        && !same_node(&node_of(route.from.as_str()), local_node)
}

impl ViewerPacket {
    fn new(bytes: Vec<u8>, profile_id: u64, frame_ts_us: Option<u64>) -> Self {
        Self {
            bytes,
            profile_id,
            frame_ts_us,
            enqueued_at: crate::pipeline_profile::stamp(),
        }
    }
}

/// A drained local viewer batch whose packet payloads remain separately
/// owned. [`crate::node_control`] writes these segments directly to the local
/// node socket, preserving the existing `[u32 len][packet]...` bytes without
/// first copying a full decoded frame into a second contiguous buffer.
/// Neither this type nor its profiling metadata crosses the mesh.
pub(crate) struct VideoPollBatch {
    packets: std::collections::VecDeque<ViewerPacket>,
    encoded_len: usize,
}

impl VideoPollBatch {
    fn new(packets: std::collections::VecDeque<ViewerPacket>) -> Self {
        let encoded_len = packets.iter().fold(0usize, |total, packet| {
            total.saturating_add(4usize.saturating_add(packet.bytes.len()))
        });
        Self {
            packets,
            encoded_len,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }

    pub(crate) fn encoded_len(&self) -> usize {
        self.encoded_len
    }

    pub(crate) fn packets(&self) -> impl Iterator<Item = &[u8]> {
        self.packets.iter().map(|packet| packet.bytes.as_slice())
    }

    fn into_bytes(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.encoded_len);
        for packet in self.packets {
            out.extend_from_slice(&(packet.bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(&packet.bytes);
        }
        out
    }

    #[cfg(test)]
    pub(crate) fn from_test_packets(packets: Vec<Vec<u8>>) -> Self {
        Self::new(
            packets
                .into_iter()
                .map(|bytes| ViewerPacket {
                    bytes,
                    profile_id: 0,
                    frame_ts_us: None,
                    enqueued_at: None,
                })
                .collect(),
        )
    }
}

/// One registered "save this download to disk" sink: the open file the
/// chunks stream into, where it lives, and progress accounting for the
/// `allmystuff://file-progress` events.
struct DownloadSink {
    file: std::fs::File,
    path: std::path::PathBuf,
    written: u64,
    last_progress: std::time::Instant,
}

/// One file offered into a room's Shared Files area: the absolute path on
/// this disk and the pubkeys allowed to fetch it (the room's members, as
/// the uploader stated them). The token that keys it in `Mesh::shared` is
/// what travels — never this path.
struct SharedReg {
    path: std::path::PathBuf,
    allowed: std::collections::HashSet<String>,
}

/// Receive-side counters for one route's stream.
struct VideoInStats {
    since: std::time::Instant,
    frames: u32,
    bytes: u64,
    label: &'static str,
}

impl VideoInStats {
    fn new(label: &'static str) -> Self {
        VideoInStats {
            since: std::time::Instant::now(),
            frames: 0,
            bytes: 0,
            label,
        }
    }
}

/// Raw JPEG bytes per video chunk: after base64 (+33%) and the JSON
/// envelope, a chunk message stays comfortably under the data channel's
/// ~64 KiB ceiling (the WebRTC SCTP max message size).
const MAX_JPEG_CHUNK_BYTES: usize = 40 * 1024;

/// Raw PTY bytes per terminal Data frame — same ceiling arithmetic as the
/// video chunks, sized small so a `cat bigfile` interleaves with
/// keystrokes instead of wedging the channel behind one giant message.
const MAX_TERM_DATA_BYTES: usize = 16 * 1024;

/// A terminal host whose sends keep failing this long (viewer offline,
/// network gone) kills the shell and tears the route down — nothing else
/// reaps a session whose peer silently vanished.
const TERM_SEND_PATIENCE: std::time::Duration = std::time::Duration::from_secs(60);

/// Initial PTY size for a freshly opened terminal session — the viewer's
/// first `Resize` reconciles the shared PTY to its real emulator size
/// moments later (and an attach to an existing session keeps that session's
/// reconciled size). A sane 80×24 beats a 0×0 PTY in the gap.
const TERM_INIT_COLS: u16 = 80;
const TERM_INIT_ROWS: u16 = 24;

/// Media-plane send failures repeat at frame rate; warn at most this often.
const WARN_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// Minimum spacing between decode-recovery asks for one route.
const VIDEO_REFRESH_FLOOR: std::time::Duration = std::time::Duration::from_millis(300);

fn reserve_video_refresh(
    asks: &mut HashMap<String, Instant>,
    route_id: &str,
    now: Instant,
) -> bool {
    if asks
        .get(route_id)
        .is_some_and(|last| now.duration_since(*last) < VIDEO_REFRESH_FLOOR)
    {
        return false;
    }
    asks.insert(route_id.to_string(), now);
    true
}

/// Current MyOwnMesh peer setup pre-negotiates only lane 0. Opening lane 1+
/// creates an RTP track and schedules a new SDP offer through signaling. The
/// product boundary forbids video work from causing signaling activity, so
/// this client may use only the lane already present in the initial session.
/// Extra simultaneous streams take the legacy data-channel fallback until the
/// daemon can report a measured, fixed pre-negotiated pool separately from its
/// dynamic lane ceiling.
const PRENEGOTIATED_MEDIA_LANES: u8 = 1;

/// The current daemon-to-node binary media-source frame omits its network id.
/// A source pipe can outlive a one-to-many network transition, so even opening
/// it during a one-network moment is unsafe. Keep the network-tagged event path
/// until a versioned binary frame makes this true.
const MEDIA_SOURCE_HAS_NETWORK_IDENTITY: bool = false;

/// One item on a route-local video queue. Generation, recovery, and profiler
/// metadata are strictly process-local: the packet reaches the established
/// media sender with the same bytes and duration as before.
struct VideoOut {
    peer: String,
    route_id: String,
    generation: u64,
    incarnation: Option<String>,
    packet: VideoPacket,
    recovery_epoch: u64,
    recovery: Arc<VideoRecovery>,
    profile_id: u64,
    enqueued_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalMediaClass {
    General,
    PriorityVideo,
    Audio,
    BackgroundVideo,
}

#[derive(Default)]
struct VideoRouteGenerations {
    next: u64,
    current: HashMap<String, u64>,
}

impl VideoRouteGenerations {
    fn begin(&mut self, route_id: &str) -> (u64, Option<u64>) {
        self.next = self.next.wrapping_add(1).max(1);
        let generation = self.next;
        let replaced = self.current.insert(route_id.to_string(), generation);
        (generation, replaced)
    }

    fn retire(&mut self, route_id: &str) {
        self.current.remove(route_id);
    }

    fn current(&self, route_id: &str) -> Option<u64> {
        self.current.get(route_id).copied()
    }

    fn is_current(&self, route_id: &str, generation: u64) -> bool {
        self.current
            .get(route_id)
            .is_some_and(|current| *current == generation)
    }
}

/// A predecessor remains eligible to arm a switch guard for this long. The
/// real switch is normally a few milliseconds; the wider retention only makes
/// the bookkeeping tolerant of a loaded viewer. It does not widen the actual
/// teardown-ignore window below.
const VIDEO_SWITCH_PREDECESSOR_AGE: Duration = Duration::from_secs(2);
/// Fence closes this soon after a display-switch successor starts. 100 ms is
/// well above the 7 ms field failure while remaining a narrow intent check.
/// Every duplicate inside the same window is fenced; none consumes the guard.
const VIDEO_SWITCH_TEARDOWN_GUARD: Duration = Duration::from_millis(100);
/// A poll that was already in flight when disconnect began is not proof. The
/// window's safety loop runs every 16 ms, so require a poll at least two ticks
/// later and observe for long enough that an active loop can produce one.
const VIDEO_LOCAL_POLL_PROOF_MIN_AGE: Duration = Duration::from_millis(32);
const VIDEO_LOCAL_POLL_OBSERVE: Duration = Duration::from_millis(80);
/// A first close that races a just-started display successor waits briefly for
/// proof that the replacement is alive. Viewer feedback is emitted at most two
/// seconds apart, so 2.5 seconds covers one full beat without letting a genuine
/// one-shot close strand an encoder indefinitely.
const VIDEO_INBOUND_TEARDOWN_QUARANTINE: Duration = Duration::from_millis(2_500);
/// Ignore an immediate, possibly already-in-flight feedback beat. A periodic
/// viewer report that arrives after this floor is evidence produced by the
/// replacement route, not merely setup traffic queued beside its offer.
const VIDEO_TEARDOWN_LIVENESS_MIN_AGE: Duration = Duration::from_millis(250);
/// Lifecycle entries outlive the 2.5-second quarantine but are pruned during
/// later route activity, bounding the bookkeeping on long-running nodes.
const VIDEO_SWITCH_BOOK_RETENTION: Duration = Duration::from_secs(10);

struct StoppedVideoRoute {
    peer: String,
    sink: String,
    at: Instant,
}

struct StartedVideoRoute {
    peer: String,
    /// The recent route whose stop made this start a display switch. It remains
    /// readable for the whole narrow guard window so duplicate local/backend
    /// calls cannot defeat the fence merely by racing one another.
    predecessor: Option<String>,
    at: Instant,
    incarnation: u64,
}

struct PendingVideoTeardown {
    token: u64,
    armed_at: Instant,
    incarnation: u64,
}

#[derive(Default)]
struct VideoSwitchGuards {
    stopped: HashMap<String, StoppedVideoRoute>,
    started: HashMap<String, StartedVideoRoute>,
    /// Early inbound teardown quarantines, route → opaque local token. An
    /// mature periodic viewer report cancels the token; duplicate closes
    /// coalesce behind the same bounded timer.
    pending: HashMap<String, PendingVideoTeardown>,
    next_pending: u64,
    next_incarnation: u64,
}

struct VideoSwitchGuardHit {
    predecessor: String,
    age: Duration,
    incarnation: u64,
}

enum InboundVideoTeardownGate {
    Commit,
    CoalesceDuplicate {
        token: u64,
    },
    Quarantine {
        predecessor: String,
        age: Duration,
        token: u64,
        incarnation: u64,
    },
}

impl VideoSwitchGuards {
    fn note_stop(&mut self, route_id: &str, peer: &str, sink: &str, now: Instant) {
        self.started.remove(route_id);
        self.pending.remove(route_id);
        self.started.retain(|_, start| {
            now.saturating_duration_since(start.at) <= VIDEO_SWITCH_BOOK_RETENTION
        });
        self.stopped.retain(|_, stop| {
            now.saturating_duration_since(stop.at) <= VIDEO_SWITCH_PREDECESSOR_AGE
        });
        self.stopped.insert(
            route_id.to_string(),
            StoppedVideoRoute {
                peer: pubkey_part(peer).to_string(),
                sink: sink.to_string(),
                at: now,
            },
        );
    }

    fn note_start(&mut self, route_id: &str, peer: &str, sink: &str, now: Instant) {
        // A real re-offer supersedes any old delayed-close timer for this
        // deterministic route id.
        self.pending.remove(route_id);
        self.started.retain(|_, start| {
            now.saturating_duration_since(start.at) <= VIDEO_SWITCH_BOOK_RETENTION
        });
        self.stopped.retain(|_, stop| {
            now.saturating_duration_since(stop.at) <= VIDEO_SWITCH_PREDECESSOR_AGE
        });
        let peer = pubkey_part(peer).to_string();
        // Prefer the newest matching predecessor. The same-id case is a codec
        // re-offer; a different id with the same sink is a monitor switch.
        let predecessor = self
            .stopped
            .iter()
            .filter(|(_, stop)| stop.peer == peer && stop.sink == sink)
            .max_by_key(|(_, stop)| stop.at)
            .map(|(id, _)| id.clone());
        self.next_incarnation = self.next_incarnation.wrapping_add(1).max(1);
        let incarnation = self.next_incarnation;
        self.started.insert(
            route_id.to_string(),
            StartedVideoRoute {
                peer,
                predecessor,
                at: now,
                incarnation,
            },
        );
    }

    fn take_early_teardown(
        &mut self,
        route_id: &str,
        peer: &str,
        now: Instant,
    ) -> Option<VideoSwitchGuardHit> {
        let start = self.started.get_mut(route_id)?;
        if start.peer != pubkey_part(peer) {
            return None;
        }
        let age = now.saturating_duration_since(start.at);
        if age > VIDEO_SWITCH_TEARDOWN_GUARD {
            return None;
        }
        let predecessor = start.predecessor.clone()?;
        Some(VideoSwitchGuardHit {
            predecessor,
            age,
            incarnation: start.incarnation,
        })
    }

    fn gate_inbound_teardown(
        &mut self,
        route_id: &str,
        peer: &str,
        now: Instant,
    ) -> InboundVideoTeardownGate {
        if let Some(pending) = self.pending.get(route_id) {
            return InboundVideoTeardownGate::CoalesceDuplicate {
                token: pending.token,
            };
        }
        let Some(hit) = self.take_early_teardown(route_id, peer, now) else {
            return InboundVideoTeardownGate::Commit;
        };
        let token = self.arm_pending(route_id, hit.incarnation, now);
        InboundVideoTeardownGate::Quarantine {
            predecessor: hit.predecessor,
            age: hit.age,
            token,
            incarnation: hit.incarnation,
        }
    }

    fn arm_pending(&mut self, route_id: &str, incarnation: u64, now: Instant) -> u64 {
        self.next_pending = self.next_pending.wrapping_add(1).max(1);
        let token = self.next_pending;
        self.pending.insert(
            route_id.to_string(),
            PendingVideoTeardown {
                token,
                armed_at: now,
                incarnation,
            },
        );
        token
    }

    fn cancel_pending(&mut self, route_id: &str) -> Option<u64> {
        self.pending.remove(route_id).map(|pending| pending.token)
    }

    fn cancel_pending_on_mature_liveness(&mut self, route_id: &str, now: Instant) -> Option<u64> {
        let pending = self.pending.get(route_id)?;
        if now.saturating_duration_since(pending.armed_at) < VIDEO_TEARDOWN_LIVENESS_MIN_AGE {
            return None;
        }
        self.cancel_pending(route_id)
    }

    fn take_pending_if_current(&mut self, route_id: &str, token: u64, incarnation: u64) -> bool {
        let pending_matches = self
            .pending
            .get(route_id)
            .is_some_and(|pending| pending.token == token && pending.incarnation == incarnation);
        let incarnation_matches = self
            .started
            .get(route_id)
            .is_some_and(|started| started.incarnation == incarnation);
        if !pending_matches || !incarnation_matches {
            return false;
        }
        self.pending.remove(route_id);
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InboundVideoDisposition {
    Accept,
    /// The authenticated peer is sending for the correct display/video route,
    /// but its first media beat outran Accept. Drop it quietly: rejecting a
    /// stable route id here would tear down the same-id successor.
    Pending,
    Reject,
}

fn inbound_video_disposition_from_facts(
    state: Option<&RouteState>,
    video_media: bool,
    sinks_here: bool,
    sender_is_peer: bool,
) -> InboundVideoDisposition {
    if !video_media || !sinks_here || !sender_is_peer {
        return InboundVideoDisposition::Reject;
    }
    match state {
        Some(RouteState::Active) => InboundVideoDisposition::Accept,
        Some(RouteState::Offered | RouteState::Incoming) => InboundVideoDisposition::Pending,
        _ => InboundVideoDisposition::Reject,
    }
}

/// The receiver's first sample must independently open a decoder reference
/// chain. The daemon's `key` bit recognizes H.264 IDRs, while HEVC/AV1 entry
/// AUs are identified by their parameter sets in the payload.
fn should_hold_first_video_sample(first: bool, key: bool, data: &[u8]) -> bool {
    first && !key && !crate::video_decode::is_decode_entry(data)
}

/// Queue-local recovery state shared by capture and its sender worker. The
/// epoch prevents an older keyframe from declaring recovery after a newer
/// drop: only a key produced in the current damage epoch and successfully
/// handed to the existing media pipe can release dependent deltas.
struct VideoRecovery {
    route_id: String,
    diag_key: String,
    /// `(epoch << 1) | awaiting_key`. Keeping both facts in one atomic makes a
    /// drop and a delivered-key decision indivisible: a stale key cannot land
    /// between separate epoch/awaiting writes and falsely release deltas.
    state: AtomicU64,
    drops: AtomicU64,
    suppressed: AtomicU64,
    /// A zero allocator budget intentionally sheds the entire encoded chain.
    /// Track that separately from accidental loss so a paused route does not
    /// force an IDR on every capture tick; resume arms exactly one repair.
    policy_paused: AtomicBool,
    /// Legacy MJPEG has no encoder bitrate control. Shape complete,
    /// independently decodable JPEG frames at admission so that fallback
    /// transport cannot silently ignore the peer allocator's video grant.
    jpeg_gate: Mutex<JpegRateGate>,
    jpeg_shaped: AtomicU64,
}

#[derive(Default)]
struct JpegRateGate {
    rate_bps: u64,
    next_at: Option<Instant>,
}

impl JpegRateGate {
    fn admit(&mut self, rate_bps: u64, wire_bytes: usize, now: Instant) -> bool {
        if rate_bps == 0 {
            return false;
        }
        if rate_bps == u64::MAX {
            self.rate_bps = rate_bps;
            self.next_at = None;
            return true;
        }
        // A new allocator grant takes effect immediately in either direction;
        // stale debt from an old, lower cap must not pin the route after focus
        // moves back to it.
        if self.rate_bps != rate_bps {
            self.rate_bps = rate_bps;
            self.next_at = None;
        }
        if self.next_at.is_some_and(|deadline| now < deadline) {
            return false;
        }
        let wire_bits = (wire_bytes as u128).saturating_mul(8);
        let interval_ns = wire_bits
            .saturating_mul(1_000_000_000)
            .div_ceil(u128::from(rate_bps))
            .min(u128::from(u64::MAX)) as u64;
        self.next_at = Some(now + Duration::from_nanos(interval_ns));
        true
    }
}

impl VideoRecovery {
    fn new(route_id: &str) -> Self {
        Self {
            route_id: route_id.to_string(),
            diag_key: format!("video-recovery:{route_id}"),
            state: AtomicU64::new(0),
            drops: AtomicU64::new(0),
            suppressed: AtomicU64::new(0),
            policy_paused: AtomicBool::new(false),
            jpeg_gate: Mutex::new(JpegRateGate::default()),
            jpeg_shaped: AtomicU64::new(0),
        }
    }

    fn admits_jpeg(&self, mesh: &Mesh, route_budget_bps: u64, frame: &VideoFrame) -> bool {
        // JSON/base64 is the actual legacy data-plane representation. Count
        // its expansion and a conservative envelope per chunk instead of
        // pretending the raw JPEG length is the wire cost.
        let chunks = frame.jpeg.len().div_ceil(MAX_JPEG_CHUNK_BYTES).max(1);
        let base64_bytes = frame.jpeg.len().div_ceil(3).saturating_mul(4);
        let wire_bytes = base64_bytes.saturating_add(chunks.saturating_mul(512));
        if self
            .jpeg_gate
            .lock()
            .admit(route_budget_bps, wire_bytes, Instant::now())
        {
            return true;
        }
        let shaped = self.jpeg_shaped.fetch_add(1, Ordering::Relaxed) + 1;
        if mesh.diag_ok(&format!("video-jpeg-shape:{}", self.route_id)) {
            tracing::info!(
                "video policy {}: shaped {shaped} MJPEG frames to the {} kbps route grant",
                self.route_id,
                route_budget_bps / 1_000
            );
        }
        false
    }

    /// Return true while policy intentionally pauses this route. The first
    /// packet after a pause enters the normal recovery epoch so only a clean
    /// key can reopen the dependency chain.
    fn policy_pauses(&self, mesh: &Mesh, route_budget_bps: u64, key: Option<bool>) -> bool {
        if route_budget_bps == 0 {
            if !self.policy_paused.swap(true, Ordering::AcqRel) {
                tracing::info!(
                    "video policy {}: route paused at a zero video allocation",
                    self.route_id
                );
            }
            return true;
        }
        if self.policy_paused.swap(false, Ordering::AcqRel) {
            tracing::info!(
                "video policy {}: route allocation restored; reopening from a clean unit",
                self.route_id
            );
            if key.is_some() {
                self.note_drop(mesh, key, "policy allocation resumed");
            }
        }
        false
    }

    fn epoch(&self) -> u64 {
        self.state.load(Ordering::Acquire) >> 1
    }

    fn suppresses(&self, key: Option<bool>) -> bool {
        suppress_dependent_after_drop(self.state.load(Ordering::Acquire) & 1 != 0, key)
    }

    fn note_suppressed(&self, mesh: &Mesh) {
        let suppressed = self.suppressed.fetch_add(1, Ordering::Relaxed) + 1;
        if mesh.diag_ok(&self.diag_key) {
            tracing::warn!(
                "video queue recovery for {}: {} total drops, {suppressed} total dependent deltas suppressed; awaiting delivered IDR",
                self.route_id,
                self.drops.load(Ordering::Relaxed)
            );
        }
    }

    fn note_drop(&self, mesh: &Mesh, key: Option<bool>, reason: &str) {
        let (arm, dropped, _) = self.mark_drop(key);
        // The first loss starts recovery. A keyframe that itself fails must
        // re-arm it; suppressed deltas never do, avoiding an IDR storm.
        if arm {
            mesh.video.force_idr(&self.route_id);
        }
        if mesh.diag_ok(&self.diag_key) {
            tracing::warn!(
                "video queue recovery for {}: {dropped} total drops ({reason}); {}",
                self.route_id,
                if arm {
                    "IDR armed"
                } else {
                    "awaiting delivered IDR"
                }
            );
        }
    }

    /// Advance the damage epoch and enter recovery. Returns whether the
    /// encoder must be armed, the episode drop count, and the new epoch.
    fn mark_drop(&self, key: Option<bool>) -> (bool, u64, u64) {
        let mut old = self.state.load(Ordering::Acquire);
        let (arm, epoch) = loop {
            let was_awaiting = old & 1 != 0;
            // A dependent unit that raced into a send before recovery began
            // is covered by the repair already in flight. Advancing its epoch
            // would stale that repair without arming a replacement.
            if was_awaiting && key == Some(false) {
                break (false, old >> 1);
            }
            let next_epoch = (old >> 1).wrapping_add(1);
            let new = (next_epoch << 1) | 1;
            match self
                .state
                .compare_exchange_weak(old, new, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break (!was_awaiting || key == Some(true), next_epoch),
                Err(actual) => old = actual,
            }
        };
        let dropped = self.drops.fetch_add(1, Ordering::Relaxed) + 1;
        (arm, dropped, epoch)
    }

    fn note_key_delivered(&self, packet_epoch: u64) -> bool {
        let recovering = (packet_epoch << 1) | 1;
        if self
            .state
            .compare_exchange(
                recovering,
                packet_epoch << 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return false;
        }
        let drops = self.drops.load(Ordering::Relaxed);
        let suppressed = self.suppressed.load(Ordering::Relaxed);
        tracing::info!(
            "video queue recovery for {}: IDR delivered (lifetime totals: {drops} drops, {suppressed} suppressed deltas)",
            self.route_id
        );
        true
    }
}

/// Auto-re-map after a site route is rejected: how many times to retry, and the
/// base backoff (grown by the attempt number). ~11s of retrying across 5 tries
/// — enough to ride out a KVM reconnect, few enough to give up (not loop) if the
/// host is genuinely refusing us.
const SITE_REMAP_ATTEMPTS: u32 = 5;
const SITE_REMAP_BACKOFF: std::time::Duration = std::time::Duration::from_millis(750);

/// Per-route cooldown for the dead-site-route NACK, so a client draining a full
/// pipe onto a route we no longer hold gets one Reject, not one per frame.
const SITE_NACK_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a CEC connect-request may wait for acknowledged delivery to
/// the customer's node. Covers the WebRTC bring-up plus a mid-dial
/// network wobble with room to spare; past it, the customer is genuinely
/// unreachable and the session honestly ends. (Delivery ≠ decision — the
/// customer can take as long as they like to click once the prompt is up.)
const CEC_CONNECT_TTL: std::time::Duration = std::time::Duration::from_secs(90);
const OFFER_SWEEP: std::time::Duration = std::time::Duration::from_secs(5);
const OFFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DaemonContext {
    epoch: u64,
    client_id: ClientId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ReliableControlKind {
    Offer,
    Accept,
    Reject,
    Teardown,
    VideoLane,
    DeadLane,
    MissingRoute,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ReliableControlScope {
    Route {
        route_id: String,
        incarnation: Option<String>,
    },
    DeadLane {
        media: String,
        lane: u8,
        networks: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ReliableControlKey {
    peer: String,
    scope: ReliableControlScope,
}

#[derive(Clone)]
struct ReliableControlOut {
    peer: String,
    networks: Vec<String>,
    payload: Value,
    kind: ReliableControlKind,
    daemon: DaemonContext,
}

#[derive(Default)]
struct ReliableControlPending {
    order: VecDeque<ReliableControlKind>,
    jobs: HashMap<ReliableControlKind, ReliableControlOut>,
}

impl ReliableControlPending {
    /// Keep only the newest unsent value of each protocol kind. Replacing a
    /// kind moves it to the tail, preserving a legacy no-incarnation
    /// `Teardown` barrier before a same-id successor `Offer`.
    fn push(&mut self, job: ReliableControlOut) -> bool {
        let kind = job.kind;
        let replaced = self.jobs.insert(kind, job).is_some();
        if replaced {
            self.order.retain(|pending| *pending != kind);
        }
        self.order.push_back(kind);
        replaced
    }

    fn pop(&mut self) -> Option<ReliableControlOut> {
        while let Some(kind) = self.order.pop_front() {
            if let Some(job) = self.jobs.remove(&kind) {
                return Some(job);
            }
        }
        None
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.jobs.len()
    }
}

struct ReliableControlWorkerHandle {
    worker_id: u64,
    daemon: DaemonContext,
    pending: Arc<Mutex<ReliableControlPending>>,
}

#[derive(Clone)]
struct DesiredRoute {
    route: Route,
    peer: String,
    requested_video: Vec<String>,
    requested_audio: Vec<String>,
    term_session: Option<String>,
    local_generation: u64,
    current_incarnation: Option<String>,
}

#[derive(Clone)]
struct PendingTeardown {
    peer: String,
    message: ControlMessage,
    network: Option<String>,
    created: Instant,
}

/// Stable local handle for one explicit route intent. `route_id` describes
/// endpoints and may be reused; `generation` identifies the caller's exact
/// local lifetime so a late close cannot tear down its successor.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RouteConnectHandle {
    pub route_id: String,
    pub generation: u64,
}

struct RouteLifecycleGuard {
    route_id: String,
    lock: Arc<tokio::sync::Mutex<()>>,
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    locks: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl Drop for RouteLifecycleGuard {
    fn drop(&mut self) {
        self.guard.take();
        let mut locks = self.locks.lock();
        if locks
            .get(&self.route_id)
            .is_some_and(|current| Arc::ptr_eq(current, &self.lock))
            && Arc::strong_count(&self.lock) == 2
        {
            locks.remove(&self.route_id);
        }
    }
}

struct State {
    session: Option<Session>,
    /// Primary network — the fallback for route control/media when we don't
    /// yet know which network a peer is on.
    network: Option<String>,
    /// Every joined network. Presence is broadcast on all of them so peers
    /// find each other regardless of which network the daemon lists first.
    networks: Vec<String>,
    /// Monotonic local generation for the joined network set. Async peer-list
    /// responses must match it before changing reachability or link class.
    network_generation: u64,
    /// Per-config incarnation of the local daemon PeerSession. A config id can
    /// leave and rejoin unchanged, so the string alone cannot validate an old
    /// route pin.
    network_epochs: HashMap<String, u64>,
    network_epoch_clock: u64,
    /// Which network each peer was last seen on (canonical pubkey → network
    /// config_id). You can be on several networks at once and a given peer may
    /// only share one of them, so control/media must be addressed to the
    /// network that peer actually lives on — not a single "primary" mesh.
    peer_networks: HashMap<String, PeerNetworkState>,
    /// Daemon-confirmed outbound network for an exact route lifetime. Peer
    /// reachability can span several meshes and its preferred path may change
    /// as unrelated traffic arrives. Media must remain on the path that
    /// actually carried this route's Offer/Accept, otherwise a later presence
    /// or control exchange can move dependent frames to a network where the
    /// route was never established. The incarnation in the key prevents a
    /// delayed predecessor from steering a same-id successor.
    route_networks: HashMap<(String, Option<String>), RouteNetworkPin>,
    /// App features each peer last advertised (canonical pubkey → feature
    /// list from its presence profile). Read to decide whether a peer can
    /// ride the media-lane pool — `FEATURE_MEDIA_LANES` present means both
    /// ends ship the lane-pool daemon and can split streams across lanes.
    peer_features: HashMap<String, Vec<String>>,
    /// How each peer's nominated ICE pair actually flows (canonical pubkey →
    /// LAN/WAN), from the daemon's `PeersList` `selected_pair` — the LAN
    /// gate's signal for how generous the AUTOMATIC video dials may be.
    /// A peer with no reported pair (ICE unsettled, old daemon) simply isn't
    /// in the map: transient unknowns must never downgrade a learned class.
    peer_links: HashMap<(String, String), crate::video::LinkClass>,
    /// Last presence boot id seen per peer (canonical pubkey). A boot id we
    /// haven't recorded means the peer just (re)started and missed our
    /// adverts — we answer with our state directly. This is what lets
    /// gossip be event-driven instead of a heartbeat.
    peer_boots: HashMap<String, u64>,
    /// Every nonzero boot superseded for a still-reachable peer. Boot ids are
    /// random epochs, not ordered counters, so this tombstone set is the only
    /// sound way to reject an old presence duplicate that arrives later on a
    /// second network. Entries are released when the peer leaves every joined
    /// network.
    peer_retired_boots: HashMap<String, std::collections::HashSet<u64>>,
    client_id: Option<ClientId>,
    profile: Option<NodeProfile>,
}

/// One immutable data-plane path for an exact route lifetime. An outbound
/// Offer installs a tentative path after the local daemon accepts the send.
/// The peer's authenticated Accept confirms the path and may replace that
/// tentative choice once. After confirmation, ordinary route controls cannot
/// move the route to another PeerSession.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RouteNetworkPin {
    network: String,
    network_epoch: u64,
    confirmed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteNetworkObservation {
    OutboundOffer,
    InboundOffer,
    InboundAccept,
}

fn observe_route_network(
    pins: &mut HashMap<(String, Option<String>), RouteNetworkPin>,
    key: (String, Option<String>),
    network: &str,
    network_epoch: u64,
    observation: RouteNetworkObservation,
) -> bool {
    match pins.get_mut(&key) {
        None => {
            pins.insert(
                key,
                RouteNetworkPin {
                    network: network.to_string(),
                    network_epoch,
                    confirmed: observation != RouteNetworkObservation::OutboundOffer,
                },
            );
            true
        }
        Some(pin) if pin.network == network && pin.network_epoch == network_epoch => {
            if observation != RouteNetworkObservation::OutboundOffer {
                pin.confirmed = true;
            }
            true
        }
        Some(pin) if !pin.confirmed && observation == RouteNetworkObservation::InboundAccept => {
            pin.network = network.to_string();
            pin.network_epoch = network_epoch;
            pin.confirmed = true;
            true
        }
        Some(_) => false,
    }
}

#[derive(Debug, Clone)]
struct NetworkSubscriptionState {
    daemon_epoch: u64,
    client_id: ClientId,
    channels: std::collections::HashSet<String>,
    video: bool,
    audio: bool,
}

impl NetworkSubscriptionState {
    fn new(daemon_epoch: u64, client_id: ClientId) -> Self {
        Self {
            daemon_epoch,
            client_id,
            channels: std::collections::HashSet::new(),
            video: false,
            audio: false,
        }
    }

    fn belongs_to(&self, daemon_epoch: u64, client_id: ClientId) -> bool {
        self.daemon_epoch == daemon_epoch && self.client_id == client_id
    }
}

#[derive(Debug, Clone)]
enum SubscriptionTarget {
    Channel(String),
    Video,
    Audio,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PeerNetworkState {
    /// Last network that carried a confirmed outbound send or inbound app
    /// frame. It is tried first but never treated as the peer's only path.
    preferred: Option<String>,
    /// Paths in the latest daemon peer snapshot. A later PeersList response may
    /// replace this evidence without erasing paths independently proven by app
    /// traffic.
    daemon_reachable: std::collections::HashSet<String>,
    /// Paths proven by an authenticated inbound frame or a confirmed outbound
    /// daemon send during this joined-network generation.
    observed_reachable: std::collections::HashSet<String>,
}

impl PeerNetworkState {
    fn contains(&self, network: &str) -> bool {
        self.daemon_reachable.contains(network) || self.observed_reachable.contains(network)
    }

    fn is_empty(&self) -> bool {
        self.daemon_reachable.is_empty() && self.observed_reachable.is_empty()
    }

    fn retain_joined(&mut self, joined: &std::collections::HashSet<String>) {
        self.daemon_reachable
            .retain(|network| joined.contains(network));
        self.observed_reachable
            .retain(|network| joined.contains(network));
    }

    fn networks(&self) -> std::collections::HashSet<&String> {
        self.daemon_reachable
            .iter()
            .chain(self.observed_reachable.iter())
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeerBootDisposition {
    Current,
    Fresh,
    Retired,
    LegacyDowngrade,
}

fn peer_boot_disposition(
    current: Option<u64>,
    retired: Option<&std::collections::HashSet<u64>>,
    candidate: u64,
) -> PeerBootDisposition {
    if candidate == 0 {
        return if current.is_some_and(|boot| boot != 0) {
            PeerBootDisposition::LegacyDowngrade
        } else {
            PeerBootDisposition::Current
        };
    }
    if current == Some(candidate) {
        return PeerBootDisposition::Current;
    }
    if retired.is_some_and(|boots| boots.contains(&candidate)) {
        return PeerBootDisposition::Retired;
    }
    PeerBootDisposition::Fresh
}

fn admit_peer_boot(
    boots: &mut HashMap<String, u64>,
    retired_boots: &mut HashMap<String, std::collections::HashSet<u64>>,
    peer: &str,
    candidate: u64,
) -> PeerBootDisposition {
    let current = boots.get(peer).copied();
    let disposition = peer_boot_disposition(current, retired_boots.get(peer), candidate);
    if disposition == PeerBootDisposition::Fresh {
        if let Some(retired) = current.filter(|boot| *boot != 0) {
            retired_boots
                .entry(peer.to_string())
                .or_default()
                .insert(retired);
        }
        boots.insert(peer.to_string(), candidate);
    }
    disposition
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VideoLaneBinding {
    route_id: String,
    incarnation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OutboundVideoLanePin {
    network: String,
    lane: u8,
}

#[derive(Debug)]
struct RouteIncarnationClock {
    boot: u64,
    sequence: u64,
}

impl RouteIncarnationClock {
    fn new() -> Self {
        Self {
            boot: fresh_boot_id(),
            sequence: 0,
        }
    }

    fn reset(&mut self) {
        self.boot = fresh_boot_id();
        self.sequence = 0;
    }

    fn next(&mut self) -> String {
        if self.sequence == u64::MAX {
            self.reset();
        }
        self.sequence += 1;
        format!("{}:{}", self.boot, self.sequence)
    }
}

/// M2 — the pacer's requested-vs-actual gap ledger (a minute at a time),
/// plus M1's daemon-write span (the pipe await per chunk).
#[derive(Default)]
struct PaceGapStats {
    n: u64,
    req_us: u64,
    act_us: u64,
    worst_over_us: u64,
    over_1ms: u64,
    write_us: u64,
    writes: u64,
    last_log: Option<Instant>,
}

/// M3 + T1.1 — one inbound video route's arrival measurement: the current
/// chunk train being timed, the dispersion-derived bandwidth estimate,
/// and the one-way-delay trend window.
struct ArrivalState {
    /// The train in progress (chunks sharing one RTP timestamp).
    ts: u32,
    first: Instant,
    last: Instant,
    bytes: usize,
    chunks: u32,
    /// EWMA of per-train dispersion samples (kbps); 0 = none yet. What a
    /// timed train measures is min(sender's drain rate, bottleneck) —
    /// exactly the number a closed loop can act on.
    est_kbps: f64,
    /// This minute's samples (Mbps) for the log line's percentiles.
    window: Vec<f64>,
    /// (arrival, relative one-way delay µs) over the last ~2 s — the
    /// slope is a standing queue growing before loss says so. Clock skew
    /// between the sender's RTP clock and our monotonic is ppm-scale,
    /// two orders under the trend threshold.
    owd: std::collections::VecDeque<(Instant, i64)>,
    /// Wall/RTP anchor for the relative delay; re-anchored periodically
    /// so u32 RTP wrap (~13 h) never crosses a window.
    base: Option<(Instant, u32)>,
    last_log: Instant,
}

/// Keep pacing inside the route's actual frame slot. The configured budget is
/// still the ceiling at ordinary frame rates; high-refresh streams get 90% of
/// one frame so pacing can never create a standing frame of sender latency.
fn pace_budget(configured_ms: u64, fps: u32) -> std::time::Duration {
    let configured_us = configured_ms.saturating_mul(1_000);
    let frame_us = 900_000 / u64::from(fps.max(1));
    std::time::Duration::from_micros(configured_us.min(frame_us).max(1))
}

/// Cap a requested gap to the AU's absolute pacing deadline. Pipe-lock and
/// write waits consume the same budget, so contention can shorten or remove a
/// later sleep but can never add a standing frame of deliberate pacing delay.
fn pace_gap_until(
    deadline: Instant,
    now: Instant,
    requested: std::time::Duration,
) -> std::time::Duration {
    requested.min(deadline.saturating_duration_since(now))
}

fn base64_media_len_allowed(encoded_len: usize) -> bool {
    let max_encoded = MAX_MEDIA_FRAME_BYTES
        .saturating_add(2)
        .div_ceil(3)
        .saturating_mul(4);
    encoded_len <= max_encoded
}

fn suppress_dependent_after_drop(awaiting_key: bool, key: Option<bool>) -> bool {
    awaiting_key && key == Some(false)
}

/// Execute one pacing gap against a deadline: bulk asynchronously (the
/// worker stays free for audio interleave), finish with the precise
/// sleeper so 100–1500 µs requests are real instead of timer-wheel
/// millisecond roundings. Falls back to the plain async sleep on a
/// current-thread runtime (tests), where blocking a worker would deadlock.
async fn paced_gap(gap: std::time::Duration) {
    let deadline = Instant::now() + gap;
    let precise_ok = tokio::runtime::Handle::try_current()
        .map(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
        .unwrap_or(false);
    loop {
        let now = Instant::now();
        let Some(rem) = deadline.checked_duration_since(now) else {
            return;
        };
        if rem > std::time::Duration::from_millis(3) {
            tokio::time::sleep(rem - std::time::Duration::from_millis(2)).await;
            continue;
        }
        if precise_ok {
            tokio::task::block_in_place(|| crate::os_perf::precise_sleep(rem));
        } else {
            tokio::time::sleep(rem).await;
        }
        return;
    }
}

impl Mesh {
    pub fn new(client: Arc<ControlClient>, sink: Arc<dyn UiSink>) -> Arc<Self> {
        // Audio keeps three packets of jitter tolerance. Video queues are
        // created per route when capture starts: a route-local one-AU handoff
        // plus one persistent writer is the ordering boundary for dependent
        // frames, even while focus changes its priority classification.
        let (audio_out, audio_rx) = mpsc::channel::<AudioOut>(usize::from(AUDIO_HANDOFF_PACKETS));
        let (reliable_control_epoch, _) = watch::channel(0);
        Arc::new(Mesh {
            client: client.clone(),
            media_pipe: MediaPipe::new(client.clone()),
            realtime_video_pipe: MediaPipe::new(client.clone()),
            audio_pipe: MediaPipe::new(client.clone()),
            background_video_pipe: MediaPipe::new(client.clone()),
            audio_track_pipe: MediaTrackPipe::new(client.clone()),
            sink,
            audio: Arc::new(AudioBridge::new()),
            video: Arc::new(VideoBridge::new()),
            video_decode: Arc::new(DecodeBridge::new()),
            pace_gaps: Mutex::new(PaceGapStats::default()),
            video_arrivals: Mutex::new(HashMap::new()),
            injector: Injector::new(),
            terminal: TerminalHost::new(),
            term_seq: AtomicU64::new(0),
            term_pumps: Mutex::new(std::collections::HashSet::new()),
            term_rx_seq: Mutex::new(HashMap::new()),
            term_in_seq: Mutex::new(HashMap::new()),
            files: FilesPlane::new(),
            file_seq: AtomicU64::new(0),
            sites: SitesProxy::load(),
            site_seq: AtomicU64::new(0),
            site_remap_inflight: Mutex::new(std::collections::HashSet::new()),
            site_nack_at: Mutex::new(HashMap::new()),
            downloads: Mutex::new(HashMap::new()),
            shared: Mutex::new(HashMap::new()),
            state: Mutex::new(State {
                session: None,
                network: None,
                networks: Vec::new(),
                network_generation: 0,
                network_epochs: HashMap::new(),
                network_epoch_clock: 0,
                peer_networks: HashMap::new(),
                route_networks: HashMap::new(),
                peer_features: HashMap::new(),
                peer_links: HashMap::new(),
                peer_boots: HashMap::new(),
                peer_retired_boots: HashMap::new(),
                client_id: None,
                profile: None,
            }),
            network_sync_serial: tokio::sync::Mutex::new(()),
            peer_refresh_serial: tokio::sync::Mutex::new(()),
            ownership: Arc::new(Ownership::load()),
            fleet_authorized: Mutex::new(std::collections::HashSet::new()),
            pending_claims: Mutex::new(std::collections::HashSet::new()),
            peer_clock_skew: Mutex::new(HashMap::new()),
            clock_skew_warned: std::sync::atomic::AtomicBool::new(false),
            offer_first_seen: Mutex::new(HashMap::new()),
            desired_routes: Mutex::new(HashMap::new()),
            route_intent_generation: AtomicU64::new(fresh_js_counter_seed()),
            pending_teardowns: Mutex::new(HashMap::new()),
            last_status: Mutex::new(("unknown".into(), None)),
            reliable_control_workers: Arc::new(Mutex::new(HashMap::new())),
            reliable_control_worker_seq: AtomicU64::new(0),
            reliable_control_epoch,
            active_daemon_context: Arc::new(Mutex::new(None)),
            fleet_roster_cache: Mutex::new(Vec::new()),
            shares: Arc::new(Shares::load()),
            audio_out,
            audio_rx: Mutex::new(Some(audio_rx)),
            media_policy: Mutex::new(MediaPolicyController::default()),
            video_policy_apply_serial: Mutex::new(()),
            effective_plan_echoes: Mutex::new(HashMap::new()),
            effective_plan_echo_running: AtomicBool::new(false),
            effective_plan_echo_epoch: AtomicU64::new(1),
            requested_video_tunes: Mutex::new(HashMap::new()),
            input_seq: AtomicU64::new(0),
            input_in_seq: Mutex::new(HashMap::new()),
            clipboard_seq: AtomicU64::new(0),
            clipboard_transfer: AtomicU64::new(0),
            clipboard: ClipboardService::spawn(),
            clip_inbound: Mutex::new(HashMap::new()),
            clip_pull_at: Mutex::new(HashMap::new()),
            route_incarnation_clock: Mutex::new(RouteIncarnationClock::new()),
            daemon_session_epoch: Arc::new(AtomicU64::new(0)),
            video_in: Mutex::new(VideoAssembler::new()),
            video_watchers: Mutex::new(VideoWatchRegistry::default()),
            video_watch_token: AtomicU64::new(fresh_js_counter_seed()),
            daemon_video: std::sync::atomic::AtomicBool::new(false),
            network_subscriptions: Mutex::new(HashMap::new()),
            subscription_serial: tokio::sync::Mutex::new(()),
            subscription_retry_epoch: AtomicU64::new(0),
            video_in_stats: Mutex::new(HashMap::new()),
            video_diag_last: Mutex::new(HashMap::new()),
            dead_lane_since: Mutex::new(HashMap::new()),
            refresh_asks: Mutex::new(HashMap::new()),
            profile_req: Mutex::new(HashMap::new()),
            audio_decoders: Mutex::new(HashMap::new()),
            audio_encoders: Mutex::new(HashMap::new()),
            pcm_audio_routes: Mutex::new(HashMap::new()),
            daemon_audio: std::sync::atomic::AtomicBool::new(false),
            daemon_lanes: std::sync::atomic::AtomicU8::new(1),
            daemon_media_pipes: std::sync::atomic::AtomicBool::new(false),
            video_lane_pins: Mutex::new(HashMap::new()),
            video_route_generations: Mutex::new(VideoRouteGenerations::default()),
            active_media_incarnations: Mutex::new(HashMap::new()),
            route_lifecycle_locks: Arc::new(Mutex::new(HashMap::new())),
            video_switch_guards: Mutex::new(VideoSwitchGuards::default()),
            video_lane_binds: Mutex::new(HashMap::new()),
            disabled_networks: Mutex::new(None),
            cec: crate::cec::Cec::new(crate::cec::consent_store_path()),
        })
    }

    /// Share the disabled-networks park store with this mesh (see the field
    /// doc). Called once at assembly, before `start`.
    pub fn attach_disabled_networks(&self, store: Arc<crate::networks_store::DisabledNetworks>) {
        *self.disabled_networks.lock() = Some(store);
    }

    /// Whether `key` (config id or network id) sits parked in the shared
    /// disabled-networks store. Without a store attached nothing is parked.
    fn network_parked(&self, key: &str) -> bool {
        self.disabled_networks
            .lock()
            .as_ref()
            .is_some_and(|s| s.contains(key))
    }

    /// Spawn the media forwarders that drain captured frames out to peers on
    /// the media channel, both bounded (see the field docs). Send failures are
    /// *surfaced* (rate-limited): a silently-dying media plane is exactly the
    /// "connected but nothing arrives" mystery.
    ///
    /// Called from [`Mesh::start`] rather than [`Mesh::new`] so the tasks land
    /// on the runtime `start` runs on — `new` is built in the GUI's sync Tauri
    /// `setup`, where `tokio::spawn` would panic with "no reactor running".
    /// Idempotent: the receivers are taken once, so a second call is a no-op.
    fn spawn_media_forwarders(self: &Arc<Self>) {
        if let Some(mut audio_rx) = self.audio_rx.lock().take() {
            let mesh = self.clone();
            crate::spawn(async move {
                let mut last_warn = std::time::Instant::now() - WARN_EVERY;
                while let Some(out) = audio_rx.recv().await {
                    let (peer, result) = match out {
                        AudioOut::Channel(peer, frame) => {
                            let Ok(payload) = serde_json::to_value(&frame) else {
                                continue;
                            };
                            let r = mesh.send_media_value(&peer, payload).await;
                            (peer, r)
                        }
                        AudioOut::Lane {
                            peer,
                            route,
                            duration_us,
                            data,
                        } => {
                            // Same lane discipline as video: drop rather than
                            // ship on lane 0 when the route has no current lane
                            // (torn down, or past the audio lane pool), which
                            // would otherwise play one stream's audio on
                            // another's route.
                            match mesh.audio_lane(&route, &peer, true) {
                                Some(lane) => {
                                    let r = mesh
                                        .send_audio_track(&peer, &route, lane, duration_us, data)
                                        .await;
                                    (peer, r)
                                }
                                None => {
                                    if mesh.diag_ok(&format!("nolane-a:{route}")) {
                                        tracing::debug!(
                                            "no audio lane for {route} right now; dropping Opus frame"
                                        );
                                    }
                                    (peer, Ok(()))
                                }
                            }
                        }
                    };
                    if let Err(e) = result {
                        if last_warn.elapsed() >= WARN_EVERY {
                            last_warn = std::time::Instant::now();
                            tracing::warn!("audio frame to {} failed: {e}", short_id(&peer));
                        }
                    }
                }
            });
        }
    }

    fn spawn_video_forwarder(self: &Arc<Self>, mut video_rx: mpsc::Receiver<VideoOut>) {
        let mesh = Arc::clone(self);
        crate::spawn(async move {
            // These writers belong to exactly one route incarnation. Every AU
            // from that route therefore crosses the same local socket in
            // producer order; a priority election changes budgets, never the
            // ordering domain underneath an H.264 reference chain.
            let json_pipe = MediaPipe::new(mesh.client.clone());
            let track_pipe = MediaTrackPipe::new(mesh.client.clone());
            let mut last_warn = std::time::Instant::now() - WARN_EVERY;
            while let Some(out) = video_rx.recv().await {
                crate::pipeline_profile::record_since(
                    &out.route_id,
                    out.profile_id,
                    None,
                    crate::pipeline_profile::Stage::OutboundRouteQueueWait,
                    out.enqueued_at,
                );
                let outcome = mesh
                    .forward_video_packet(
                        &out.peer,
                        &out.route_id,
                        out.generation,
                        out.incarnation,
                        out.packet,
                        out.recovery_epoch,
                        &out.recovery,
                        out.profile_id,
                        &json_pipe,
                        &track_pipe,
                    )
                    .await;
                if let Err(e) = outcome {
                    if last_warn.elapsed() >= WARN_EVERY {
                        last_warn = std::time::Instant::now();
                        tracing::warn!("route video to {} failed: {e}", short_id(&out.peer));
                    }
                }
            }
        });
    }

    /// Send one media-channel payload to `peer` (canonicalised to the bare
    /// pubkey the daemon's peer set is keyed by) down the pipelined media
    /// pipe. `Ok` means the daemon has the bytes; its verdict (peer gone,
    /// message too large) still reaches a log — the pipe's response drain
    /// warns on refusals instead of this path stalling a round trip per
    /// chunk to hear them.
    /// Deliver one packet through the same established media functions used by
    /// the shipped shared worker. Labs scheduling changes only which bounded
    /// queue owns the packet; it does not introduce a channel, request, or
    /// signaling operation.
    #[allow(clippy::too_many_arguments)]
    async fn forward_video_packet(
        &self,
        peer: &str,
        route_id: &str,
        generation: u64,
        incarnation: Option<String>,
        packet: VideoPacket,
        packet_epoch: u64,
        recovery: &VideoRecovery,
        profile_id: u64,
        json_pipe: &MediaPipe,
        track_pipe: &MediaTrackPipe,
    ) -> Result<(), String> {
        if !self.video_generation_is_current(route_id, generation) {
            tracing::debug!(
                "discarding stale video AU for {route_id} generation {generation} before media send"
            );
            return Ok(());
        }
        match packet {
            VideoPacket::Jpeg(mut frame) => {
                frame.incarnation = incarnation;
                for chunk in frame.into_chunks(MAX_JPEG_CHUNK_BYTES) {
                    // Teardown/re-offer can run while a large frame is being
                    // chunked. Stop at the first generation change so the
                    // predecessor cannot finish onto the successor's reused
                    // fixed track.
                    if !self.video_generation_is_current(route_id, generation) {
                        tracing::debug!(
                            "stopping stale JPEG AU for {route_id} generation {generation} during media send"
                        );
                        return Ok(());
                    }
                    let Ok(payload) = serde_json::to_value(&chunk) else {
                        continue;
                    };
                    self.send_route_video_value(json_pipe, peer, route_id, profile_id, payload)
                        .await?;
                }
                Ok(())
            }
            VideoPacket::H264 {
                data,
                key,
                duration_us,
                ..
            } => {
                // Capture cannot retract deltas already in the queue when a
                // newer packet is dropped. Re-check at dequeue so none cross
                // the missing reference before the delivered repair key.
                if recovery.suppresses(Some(key)) {
                    recovery.note_suppressed(self);
                    return Ok(());
                }
                let Some(lane) = self.video_lane(route_id, peer, true) else {
                    recovery.note_drop(self, Some(key), "no route lane");
                    return Ok(());
                };
                let pace = self.video.route_pace(route_id);
                match self
                    .send_video_paced(
                        json_pipe,
                        track_pipe,
                        peer,
                        route_id,
                        generation,
                        lane,
                        &data,
                        duration_us,
                        pace,
                        profile_id,
                    )
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => return Ok(()),
                    Err(e) => {
                        recovery.note_drop(self, Some(key), "media send failed");
                        return Err(e);
                    }
                }
                if key {
                    recovery.note_key_delivered(packet_epoch);
                }
                Ok(())
            }
        }
    }

    async fn send_media_value(&self, peer: &str, payload: Value) -> Result<(), String> {
        let route_id = payload.get("route").and_then(Value::as_str);
        let network = match route_id {
            Some(route_id) => self.network_for_route(route_id, peer),
            None => self.network_for_peer(peer),
        };
        let Some(network) = network else {
            return Err("no shared network".into());
        };
        let class = self.classify_local_media(&payload);
        let pipe = match class {
            LocalMediaClass::General => &self.media_pipe,
            LocalMediaClass::PriorityVideo => &self.realtime_video_pipe,
            LocalMediaClass::Audio => &self.audio_pipe,
            LocalMediaClass::BackgroundVideo => &self.background_video_pipe,
        };
        pipe.send(&Request::ChannelSendTo {
            network,
            channel: CHANNEL_MEDIA.to_string(),
            peer: pubkey_part(peer).to_string(),
            payload,
        })
        .await
        .map_err(|e| e.to_string())
    }

    /// Route-local JSON video send. MJPEG chunks stay on the same persistent
    /// writer for the lifetime of one capture incarnation; using the generic
    /// class pipes here would let a focus change split adjacent chunks/frames
    /// across independently serviced daemon sockets.
    async fn send_route_video_value(
        &self,
        pipe: &MediaPipe,
        peer: &str,
        route_id: &str,
        profile_id: u64,
        payload: Value,
    ) -> Result<(), String> {
        let Some(network) = self.network_for_route(route_id, peer) else {
            return Err("no shared network".into());
        };
        pipe.send_profiled(
            &Request::ChannelSendTo {
                network,
                channel: CHANNEL_MEDIA.to_string(),
                peer: pubkey_part(peer).to_string(),
                payload,
            },
            route_id,
            profile_id,
        )
        .await
        .map_err(|e| e.to_string())
    }

    fn classify_local_media(&self, payload: &Value) -> LocalMediaClass {
        let tag = payload.get("t").and_then(Value::as_str);
        match tag {
            Some("video" | "vstat") => {
                let priority = payload
                    .get("route")
                    .and_then(Value::as_str)
                    .is_none_or(|route| self.media_policy.lock().is_priority(route));
                if priority {
                    LocalMediaClass::PriorityVideo
                } else {
                    LocalMediaClass::BackgroundVideo
                }
            }
            // AudioFrame intentionally has no `t` for v0.1 compatibility.
            None => LocalMediaClass::Audio,
            Some(_) => LocalMediaClass::General,
        }
    }

    /// Send one H.264 access unit, paced when the dial is on: the unit is
    /// split at slice-NAL boundaries into ≤[`video::PACE_SLICE_BYTES`]
    /// chunks and each chunk goes out as its own track send, spaced by a
    /// small gap — on the wire, one keyframe's back-to-back packet wall
    /// becomes a few ~20-packet bursts a shallow bottleneck queue can
    /// absorb. Non-final chunks carry `duration_us = 0`, so every chunk
    /// shares one RTP timestamp. These samples are fragments of one access
    /// unit, not independent pictures; the feature is opt-in until every
    /// receive path has complete-AU finality/reassembly. This task is the route's only video
    /// sender, so chunk order is inherent; the gaps also release the
    /// pipe's writer between chunks, letting audio frames interleave
    /// instead of queueing behind a keyframe. A mid-unit send failure
    /// surfaces like any send failure; receiver recovery must discard the
    /// incomplete access unit and request a clean entry.
    ///
    /// The drain model is link-fitted: on a LAN the historical 800 Mbps
    /// shape (shallow-buffer smoothing, budget 6/10 ms) stands; on a
    /// WAN-class path the spread rate is the route's OWN send bitrate
    /// ×1.5 with a one-frame-interval budget — spreading a wall across
    /// its own frame slot adds zero pipeline latency by definition, while
    /// the old constants handed a 40 Mbps path a ~2-frame standing queue
    /// per keyframe (a ~190 KB wall in 1.7 ms is an instantaneous
    /// ~890 Mbps). `ALLMYSTUFF_PACE_DRAIN_MBPS` pins the drain for A/B.
    /// Gaps are executed against a deadline with [`os_perf::precise_sleep`]
    /// under the hood — the requested spacing is real now, not
    /// timer-wheel-rounded — and every gap is ledgered (the `pace gaps`
    /// line, one per minute).
    #[allow(clippy::too_many_arguments)]
    async fn send_video_paced(
        &self,
        json_pipe: &MediaPipe,
        track_pipe: &MediaTrackPipe,
        peer: &str,
        route_id: &str,
        generation: u64,
        lane: u8,
        data: &[u8],
        duration_us: u64,
        pace: (bool, bool, u32, u32),
        profile_id: u64,
    ) -> Result<bool, String> {
        let current = || self.video_generation_is_current(route_id, generation);
        if !crate::video::paced_slices_enabled() {
            if !current() {
                return Ok(false);
            }
            self.send_video_track(
                json_pipe,
                track_pipe,
                peer,
                route_id,
                lane,
                data,
                duration_us,
                profile_id,
            )
            .await?;
            return Ok(true);
        }
        let chunks = crate::video::split_annexb_paced(data, crate::video::PACE_SLICE_BYTES);
        if chunks.len() < 2 {
            if !current() {
                return Ok(false);
            }
            self.send_video_track(
                json_pipe,
                track_pipe,
                peer,
                route_id,
                lane,
                data,
                duration_us,
                profile_id,
            )
            .await?;
            return Ok(true);
        }
        // (game posture, WAN-class path, current send rate bps) — the
        // shape `VideoBridge::route_pace` hands the forwarder.
        let (game, wan, rate_bps, fps) = pace;
        static DRAIN_OVERRIDE_MBPS: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
            std::env::var("ALLMYSTUFF_PACE_DRAIN_MBPS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0)
        });
        // (drain bps, per-gap cap µs, whole-AU budget ms).
        let (drain_bps, gap_cap_us, budget_ms) = if *DRAIN_OVERRIDE_MBPS > 0 {
            (*DRAIN_OVERRIDE_MBPS * 1_000_000, 8_000, 16)
        } else if wan && rate_bps > 0 {
            // The link is asked to carry `rate_bps` steady-state; walls
            // spread at 1.5× that (peaks are real) across at most one
            // frame interval.
            ((rate_bps as u64) * 3 / 2, 8_000, 16)
        } else if game {
            (800_000_000, 1_000, 6)
        } else {
            (800_000_000, 1_500, 10)
        };
        let total_budget = pace_budget(budget_ms, fps);
        let budget_each = total_budget / (chunks.len() as u32 - 1);
        let pace_deadline = Instant::now() + total_budget;
        let last = chunks.len() - 1;
        let mut ledger: Vec<(u64, u64)> = Vec::with_capacity(last);
        // M1's pace+write split: gap time is the ledger above; this is
        // the daemon-pipe await itself — if the daemon ever backpressures
        // (wedged reader, saturated socket), it shows here first.
        let (mut write_us, mut writes) = (0u64, 0u64);
        for (i, range) in chunks.into_iter().enumerate() {
            if !current() {
                tracing::debug!(
                    "stopping stale H.264 AU for {route_id} generation {generation} during paced media send"
                );
                return Ok(false);
            }
            let dur = if i == last { duration_us } else { 0 };
            let sent_bytes = range.len();
            let tw = Instant::now();
            self.send_video_track(
                json_pipe,
                track_pipe,
                peer,
                route_id,
                lane,
                &data[range],
                dur,
                profile_id,
            )
            .await?;
            write_us += tw.elapsed().as_micros() as u64;
            writes += 1;
            if i != last {
                let drain_us =
                    (sent_bytes as u64 * 8_000_000 / drain_bps.max(1)).clamp(100, gap_cap_us);
                let requested = std::time::Duration::from_micros(drain_us).min(budget_each);
                // The shared media writer is the final serialization point.
                // Time waiting for it consumes this AU's pacing slot; never
                // sleep the full nominal gap after that budget has elapsed.
                let gap = pace_gap_until(pace_deadline, Instant::now(), requested);
                if !gap.is_zero() {
                    let t0 = std::time::Instant::now();
                    paced_gap(gap).await;
                    let actual = t0.elapsed();
                    crate::pipeline_profile::record(
                        route_id,
                        profile_id,
                        None,
                        crate::pipeline_profile::Stage::OutboundPaceWait,
                        actual,
                    );
                    ledger.push((gap.as_micros() as u64, actual.as_micros() as u64));
                }
            }
        }
        self.note_pace_gaps(&ledger, write_us, writes);
        Ok(true)
    }

    /// Fold one delivered video sample into the route's arrival state:
    /// time the chunk train it belongs to (same RTP timestamp), and when
    /// a new train opens, finalize the previous one into the bandwidth
    /// estimate, the delay-trend window, and the minute log (M3 + T1.1).
    fn note_video_arrival(&self, route_id: &str, rtp_timestamp: u32, bytes: usize) {
        let now = Instant::now();
        let mut map = self.video_arrivals.lock();
        let st = map
            .entry(route_id.to_string())
            .or_insert_with(|| ArrivalState {
                ts: rtp_timestamp,
                first: now,
                last: now,
                bytes: 0,
                chunks: 0,
                est_kbps: 0.0,
                window: Vec::new(),
                owd: std::collections::VecDeque::new(),
                base: None,
                last_log: now,
            });
        if st.ts != rtp_timestamp && st.chunks > 0 {
            // Train complete. Dispersion needs ≥3 timed chunks and a
            // non-degenerate spread to say anything about rate.
            let spread_us = st.last.duration_since(st.first).as_micros() as u64;
            if st.chunks >= 3 && spread_us >= 300 {
                let mbps = (st.bytes as f64 * 8.0) / spread_us as f64;
                st.window.push(mbps);
                let kbps = mbps * 1000.0;
                st.est_kbps = if st.est_kbps <= 0.0 {
                    kbps
                } else {
                    st.est_kbps * 0.8 + kbps * 0.2
                };
            }
            // One-way-delay trend: relative delay of this train's FIRST
            // chunk vs the anchor, windowed to ~2 s.
            let (base_wall, base_rtp) = *st.base.get_or_insert((st.first, st.ts));
            let rtp_delta_us = i64::from(st.ts.wrapping_sub(base_rtp) as i32) * 1000 / 90;
            let wall_delta_us = st.first.duration_since(base_wall).as_micros() as i64;
            st.owd.push_back((st.first, wall_delta_us - rtp_delta_us));
            while st
                .owd
                .front()
                .is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(2))
            {
                st.owd.pop_front();
            }
            // Re-anchor every ~5 min: RTP u32 wraps at ~13 h, and the
            // relative math must never straddle it.
            if st.first.duration_since(base_wall) > Duration::from_secs(300) {
                st.base = Some((st.first, st.ts));
                st.owd.clear();
            }
            if st.last_log.elapsed() >= Duration::from_secs(60) && !st.window.is_empty() {
                st.window.sort_by(f64::total_cmp);
                let p = |q: f64| st.window[((st.window.len() - 1) as f64 * q) as usize];
                tracing::info!(
                    "video in {route_id}: chunk-trains {} · implied p5 {:.1} · p50 {:.1} Mbps · est {:.1} Mbps · delay trend {:+} µs/s",
                    st.window.len(),
                    p(0.05),
                    p(0.50),
                    st.est_kbps / 1000.0,
                    Self::owd_trend_us_per_s(&st.owd),
                );
                st.window.clear();
                st.last_log = now;
            }
            (st.ts, st.first, st.bytes, st.chunks) = (rtp_timestamp, now, 0, 0);
        } else if st.chunks == 0 {
            (st.ts, st.first) = (rtp_timestamp, now);
        }
        st.last = now;
        st.bytes += bytes;
        st.chunks += 1;
    }

    /// The delay-trend slope over the window: µs of added one-way delay
    /// per second, endpoint-to-endpoint. Coarse on purpose — the signal
    /// that matters is "tens of milliseconds per second", not noise.
    fn owd_trend_us_per_s(owd: &std::collections::VecDeque<(Instant, i64)>) -> i32 {
        let (Some((t0, d0)), Some((t1, d1))) = (owd.front(), owd.back()) else {
            return 0;
        };
        let span = t1.duration_since(*t0).as_secs_f64();
        if span < 0.5 {
            return 0;
        }
        (((d1 - d0) as f64) / span) as i32
    }

    /// The estimator's current answer for a route: `(est_kbps, trend)`,
    /// zeros when unknown — what [`Self::send_video_feedback`] attaches.
    fn route_link_estimate(&self, route_id: &str) -> (u32, i32) {
        let map = self.video_arrivals.lock();
        let Some(st) = map.get(route_id) else {
            return (0, 0);
        };
        // Feedback is periodic. Never let a dead/stalled route's last burst
        // masquerade as a current path ceiling after media has gone quiet.
        if st.last.elapsed() > Duration::from_secs(5) {
            return (0, 0);
        }
        (st.est_kbps as u32, Self::owd_trend_us_per_s(&st.owd))
    }

    /// Fold one AU's gap + daemon-write measurements into the minute
    /// ledger and emit the `pace gaps` line when it's due — M2's honesty
    /// check on the pacer plus M1's pace/write split.
    fn note_pace_gaps(&self, ledger: &[(u64, u64)], write_us: u64, writes: u64) {
        if ledger.is_empty() && writes == 0 {
            return;
        }
        let mut g = self.pace_gaps.lock();
        for &(req, act) in ledger {
            g.n += 1;
            g.req_us += req;
            g.act_us += act;
            let over = act.saturating_sub(req);
            g.worst_over_us = g.worst_over_us.max(over);
            if over > 1_000 {
                g.over_1ms += 1;
            }
        }
        g.write_us += write_us;
        g.writes += writes;
        let due = g
            .last_log
            .map(|t| t.elapsed() >= std::time::Duration::from_secs(60))
            .unwrap_or(true);
        if due && g.n > 0 {
            tracing::info!(
                "pace gaps: {} gaps · requested avg {} µs → actual avg {} µs · worst +{:.1} ms · >1 ms err {:.2}% · daemon write avg {} µs/chunk",
                g.n,
                g.req_us / g.n,
                g.act_us / g.n,
                g.worst_over_us as f64 / 1000.0,
                g.over_1ms as f64 * 100.0 / g.n as f64,
                g.write_us / g.writes.max(1),
            );
            *g = PaceGapStats {
                last_log: Some(std::time::Instant::now()),
                ..PaceGapStats::default()
            };
        }
    }

    /// Send one H.264 access unit to `peer` over the daemon's video track
    /// lane — raw binary on the control socket (no base64), RTP on the wire.
    #[allow(clippy::too_many_arguments)]
    async fn send_video_track(
        &self,
        json_pipe: &MediaPipe,
        track_pipe: &MediaTrackPipe,
        peer: &str,
        route_id: &str,
        lane: u8,
        data: &[u8],
        duration_us: u64,
        profile_id: u64,
    ) -> Result<(), String> {
        let Some(network) = self.network_for_route(route_id, peer) else {
            return Err("no shared network".into());
        };
        // Binary media pipe when the daemon speaks it; otherwise the legacy
        // base64 video_send op (so an older daemon still streams).
        if self.daemon_media_pipes.load(Ordering::SeqCst) {
            track_pipe
                .send_profiled_video(
                    &network,
                    pubkey_part(peer),
                    lane,
                    duration_us,
                    data,
                    route_id,
                    profile_id,
                )
                .await
                .map_err(|e| e.to_string())
        } else {
            use base64::Engine as _;
            json_pipe
                .send_profiled(
                    &Request::VideoSend {
                        network,
                        peer: pubkey_part(peer).to_string(),
                        stream: lane,
                        duration_us,
                        data: base64::engine::general_purpose::STANDARD.encode(data),
                    },
                    route_id,
                    profile_id,
                )
                .await
                .map_err(|e| e.to_string())
        }
    }

    /// Send one encoded Opus frame to `peer` over the daemon's audio track
    /// lane — binary media pipe when supported, else legacy base64.
    async fn send_audio_track(
        &self,
        peer: &str,
        route_id: &str,
        lane: u8,
        duration_us: u64,
        data: Vec<u8>,
    ) -> Result<(), String> {
        let Some(network) = self.network_for_route(route_id, peer) else {
            return Err("no shared network".into());
        };
        if self.daemon_media_pipes.load(Ordering::SeqCst) {
            self.audio_track_pipe
                .send_audio(&network, pubkey_part(peer), lane, duration_us, &data)
                .await
                .map_err(|e| e.to_string())
        } else {
            use base64::Engine as _;
            self.audio_pipe
                .send(&Request::AudioSend {
                    network,
                    peer: pubkey_part(peer).to_string(),
                    stream: lane,
                    duration_us,
                    data: base64::engine::general_purpose::STANDARD.encode(&data),
                })
                .await
                .map_err(|e| e.to_string())
        }
    }

    /// The network to reach `peer` on: the one we last saw them on (an inbound
    /// app frame, or the daemon's peer list — see [`Mesh::refresh_peer_networks`]),
    /// falling back to the primary. This is what lets a connection cross to a
    /// peer that only shares a secondary network with us.
    fn network_for_peer(&self, peer: &str) -> Option<String> {
        let st = self.state.lock();
        network_for_peer_locked(&st, peer)
    }

    /// Resolve media for one exact route lifetime. A route pin is installed
    /// only after the daemon confirms an outbound lifecycle message on that
    /// network. The peer-wide preference remains the bootstrap/fail-safe when
    /// no exact pin exists, but unrelated traffic can no longer move a live
    /// route between meshes.
    fn network_for_route(&self, route_id: &str, peer: &str) -> Option<String> {
        let st = self.state.lock();
        let route = st
            .session
            .as_ref()
            .and_then(|session| session.route(route_id));
        if let Some(route) =
            route.filter(|route| pubkey_part(route.peer.as_str()) == pubkey_part(peer))
        {
            if route.state != RouteState::Active
                || self.active_media_incarnations.lock().get(route_id) != Some(&route.incarnation)
            {
                return None;
            }
            let key = (route_id.to_string(), route.incarnation.clone());
            if let Some(pin) = st.route_networks.get(&key) {
                let current_epoch = st.network_epochs.get(&pin.network).copied();
                return (st.networks.contains(&pin.network)
                    && current_epoch == Some(pin.network_epoch)
                    && pin.confirmed)
                    .then(|| pin.network.clone());
            }
            // A fenced lifetime must never move to another PeerSession merely
            // because the exact path disappeared. Its owner will retire it and
            // negotiate a fresh incarnation. Legacy lifetimes have no such
            // identity and are safe to infer only in a single-network session.
            if route.incarnation.is_some() || st.networks.len() != 1 {
                return None;
            }
        }
        network_for_peer_locked(&st, peer)
    }

    fn network_video_ready(&self, network: &str) -> bool {
        let daemon_epoch = self.daemon_session_epoch.load(Ordering::SeqCst);
        let Some(client_id) = self.state.lock().client_id else {
            return false;
        };
        let ready = self
            .network_subscriptions
            .lock()
            .get(network)
            .is_some_and(|state| state.belongs_to(daemon_epoch, client_id) && state.video);
        ready
            && self.daemon_session_epoch.load(Ordering::SeqCst) == daemon_epoch
            && self.state.lock().client_id == Some(client_id)
    }

    fn network_audio_ready(&self, network: &str) -> bool {
        let daemon_epoch = self.daemon_session_epoch.load(Ordering::SeqCst);
        let Some(client_id) = self.state.lock().client_id else {
            return false;
        };
        let ready = self
            .network_subscriptions
            .lock()
            .get(network)
            .is_some_and(|state| state.belongs_to(daemon_epoch, client_id) && state.audio);
        ready
            && self.daemon_session_epoch.load(Ordering::SeqCst) == daemon_epoch
            && self.state.lock().client_id == Some(client_id)
    }

    fn daemon_context_is_current(&self, epoch: u64, client_id: ClientId) -> bool {
        self.daemon_session_epoch.load(Ordering::SeqCst) == epoch
            && *self.active_daemon_context.lock() == Some(DaemonContext { epoch, client_id })
    }

    fn network_snapshot_is_current(
        &self,
        epoch: u64,
        client_id: ClientId,
        generation: u64,
        network: &str,
    ) -> bool {
        if self.daemon_session_epoch.load(Ordering::SeqCst) != epoch {
            return false;
        }
        let state = self.state.lock();
        state.client_id == Some(client_id)
            && state.network_generation == generation
            && state.networks.iter().any(|joined| joined == network)
    }

    fn peer_video_ready(&self, peer: &str) -> bool {
        self.peer_reachable_networks(peer)
            .iter()
            .any(|network| self.network_video_ready(network))
    }

    fn peer_audio_ready(&self, peer: &str) -> bool {
        self.peer_reachable_networks(peer)
            .iter()
            .any(|network| self.network_audio_ready(network))
    }

    fn peer_reachable_networks(&self, peer: &str) -> Vec<String> {
        let state = self.state.lock();
        let mut networks = state
            .peer_networks
            .get(pubkey_part(peer))
            .map(|paths| {
                paths
                    .networks()
                    .into_iter()
                    .filter(|network| state.networks.contains(*network))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        // Before the first peer-list or inbound observation there is no proven
        // path. Probe joined networks for compatibility. Once evidence exists,
        // media readiness must be evaluated only on those actual peer paths.
        if networks.is_empty() {
            networks = state.networks.clone();
        }
        networks.sort();
        networks.dedup();
        networks
    }

    fn route_video_ready(&self, route_id: &str, peer: &str) -> bool {
        self.network_for_route(route_id, peer)
            .is_some_and(|network| self.network_video_ready(&network))
    }

    fn route_audio_ready(&self, route_id: &str, peer: &str) -> bool {
        self.network_for_route(route_id, peer)
            .is_some_and(|network| self.network_audio_ready(&network))
    }

    fn route_link_class(&self, route_id: &str, peer: &str) -> crate::video::LinkClass {
        let state = self.state.lock();
        let Some(route) = state
            .session
            .as_ref()
            .and_then(|session| session.route(route_id))
            .filter(|route| pubkey_part(route.peer.as_str()) == pubkey_part(peer))
        else {
            return crate::video::LinkClass::Unknown;
        };
        let key = (route_id.to_string(), route.incarnation.clone());
        let Some(pin) = state.route_networks.get(&key).filter(|pin| {
            pin.confirmed
                && state.network_epochs.get(&pin.network).copied() == Some(pin.network_epoch)
        }) else {
            return crate::video::LinkClass::Unknown;
        };
        state
            .peer_links
            .get(&(pin.network.clone(), pubkey_part(peer).to_string()))
            .copied()
            .unwrap_or_default()
    }

    /// Use one exact data-plane path for every control in a route lifetime.
    /// Only an Offer with no pin probes peer candidates. Once a path is pinned,
    /// failure is surfaced so recovery creates a fresh route incarnation.
    fn route_network_candidates(&self, peer: &str, message: &ControlMessage) -> Vec<String> {
        let mut candidates = self.peer_network_candidates(peer);
        let (needs_video, needs_audio) = match message {
            ControlMessage::Route(RouteControl::Offer { video, audio, .. }) => {
                (!video.is_empty(), !audio.is_empty())
            }
            _ => route_control_network_key(message)
                .and_then(|(route_id, incarnation)| {
                    self.state
                        .lock()
                        .session
                        .as_ref()
                        .and_then(|session| session.route(route_id))
                        .filter(|route| route.incarnation.as_deref() == incarnation)
                        .map(|route| (!route.video.is_empty(), !route.audio.is_empty()))
                })
                .unwrap_or((false, false)),
        };
        if needs_video || needs_audio {
            let reachable = self.peer_reachable_networks(peer);
            candidates.retain(|network| {
                reachable.contains(network)
                    && (!needs_video || self.network_video_ready(network))
                    && (!needs_audio || self.network_audio_ready(network))
            });
        }
        let Some((route_id, incarnation)) = route_control_network_key(message) else {
            return candidates;
        };
        let route_path = {
            let st = self.state.lock();
            let current = st
                .session
                .as_ref()
                .and_then(|session| session.route(route_id))
                .is_some_and(|route| {
                    pubkey_part(route.peer.as_str()) == pubkey_part(peer)
                        && route.incarnation.as_deref() == incarnation
                });
            current.then(|| {
                let key = (route_id.to_string(), incarnation.map(str::to_string));
                (
                    st.route_networks.get(&key).cloned(),
                    st.networks.clone(),
                    st.network_epochs.clone(),
                )
            })
        };
        match route_path {
            Some((Some(pin), joined, epochs)) => {
                return if joined.contains(&pin.network)
                    && epochs.get(&pin.network).copied() == Some(pin.network_epoch)
                {
                    vec![pin.network]
                } else {
                    Vec::new()
                };
            }
            Some((None, _, _))
                if incarnation.is_some()
                    && !matches!(
                        message,
                        ControlMessage::Route(
                            RouteControl::Offer { .. } | RouteControl::MissingRoute { .. }
                        )
                    ) =>
            {
                return Vec::new();
            }
            _ => {}
        }
        candidates
    }

    /// Tentatively bind a newly dispatched outbound Offer. No other outbound
    /// control is allowed to install or move a route path.
    fn note_outbound_offer_network(&self, peer: &str, message: &ControlMessage, network: &str) {
        if !matches!(message, ControlMessage::Route(RouteControl::Offer { .. })) {
            return;
        }
        let Some((route_id, incarnation)) = route_control_network_key(message) else {
            return;
        };
        let mut st = self.state.lock();
        if !st.networks.iter().any(|joined| joined == network) {
            return;
        }
        let Some(network_epoch) = st.network_epochs.get(network).copied() else {
            return;
        };
        let current = st
            .session
            .as_ref()
            .and_then(|session| session.route(route_id))
            .is_some_and(|route| {
                pubkey_part(route.peer.as_str()) == pubkey_part(peer)
                    && route.incarnation.as_deref() == incarnation
                    && matches!(
                        route.state,
                        RouteState::Offered | RouteState::Incoming | RouteState::Active
                    )
            });
        if current {
            let key = (route_id.to_string(), incarnation.map(str::to_string));
            if !observe_route_network(
                &mut st.route_networks,
                key,
                network,
                network_epoch,
                RouteNetworkObservation::OutboundOffer,
            ) {
                tracing::warn!(
                    route = %route_id,
                    network,
                    disposition = "outbound_offer_path_change_refused",
                    "route lifetime is already pinned to another data-plane network"
                );
            }
        }
    }

    /// Validate an authenticated inbound route control against the immutable
    /// path for its exact lifetime. An Accept may confirm a different path
    /// while the outbound Offer pin is still tentative. No control may move a
    /// confirmed route.
    fn inbound_route_control_path_ok(
        &self,
        peer: &str,
        message: &ControlMessage,
        network: &str,
    ) -> bool {
        let state = self.state.lock();
        if !state.networks.iter().any(|joined| joined == network) {
            return false;
        }
        let Some((route_id, incarnation)) = route_control_network_key(message) else {
            return true;
        };
        let exact_route = state
            .session
            .as_ref()
            .and_then(|session| session.route(route_id))
            .filter(|route| {
                pubkey_part(route.peer.as_str()) == pubkey_part(peer)
                    && route.incarnation.as_deref() == incarnation
            });

        // A new inbound Offer has no route table entry yet. A same-id
        // successor likewise has a new incarnation and may establish its own
        // independent path.
        if matches!(message, ControlMessage::Route(RouteControl::Offer { .. }))
            && exact_route.is_none()
        {
            return true;
        }
        let Some(route) = exact_route else {
            return false;
        };
        // MissingRoute is the recovery message for an exact lifetime whose
        // immutable PeerSession disappeared. It must be allowed to cross a
        // surviving data-plane PeerSession, otherwise removing network A
        // strands the owner on A while the receiver's request is prohibited
        // from reaching it on B. The handler still requires the authenticated
        // peer and exact current incarnation, tears that lifetime down, and
        // mints a fresh Offer; this exception never moves the old pin.
        if route.incarnation.is_some()
            && matches!(
                message,
                ControlMessage::Route(RouteControl::MissingRoute { .. })
            )
        {
            return true;
        }
        let key = (route_id.to_string(), route.incarnation.clone());
        let current_network_epoch = state.network_epochs.get(network).copied();
        match state.route_networks.get(&key) {
            Some(pin)
                if pin.network == network && current_network_epoch == Some(pin.network_epoch) =>
            {
                true
            }
            Some(pin)
                if !pin.confirmed
                    && matches!(message, ControlMessage::Route(RouteControl::Accept { .. })) =>
            {
                true
            }
            Some(pin)
                if !pin.confirmed
                    && route.incarnation.is_some()
                    && route.origin == allmystuff_session::Origin::Outbound
                    && route.state == RouteState::Offered
                    && current_network_epoch.is_some()
                    && matches!(
                        message,
                        ControlMessage::Route(RouteControl::Reject { .. })
                    ) =>
            {
                // A reliable Offer can be acknowledged on a different
                // PeerSession than the daemon-accepted fast attempt. The exact
                // peer may reject that pending lifetime without moving or
                // confirming its tentative path.
                true
            }
            None => {
                // ChannelSendReliable can deliver an outbound Offer after every
                // addressed ChannelSendTo attempt failed. That path has no
                // tentative pin, but the only valid replies still name the
                // exact authenticated peer and exact current offer lifetime.
                // Admit those terminal negotiation replies while the route is
                // still Offered. Accept will install a confirmed pin only after
                // Session accepts it; Reject terminates without installing one.
                let exact_unpinned_offer_reply = route.incarnation.is_some()
                    && route.origin == allmystuff_session::Origin::Outbound
                    && route.state == RouteState::Offered
                    && current_network_epoch.is_some()
                    && matches!(
                        message,
                        ControlMessage::Route(
                            RouteControl::Accept { .. } | RouteControl::Reject { .. }
                        )
                    );
                // Legacy peers cannot name route lifetimes. Preserve their
                // single-network behavior, but never guess in a multi-network
                // session or for a fenced route.
                exact_unpinned_offer_reply
                    || (route.incarnation.is_none()
                        && state.networks.len() == 1
                        && state.networks[0] == network)
            }
            _ => false,
        }
    }

    /// Commit an inbound Offer/Accept path after the Session accepted the
    /// message. The state lock covers both the route identity check and the
    /// pin transition, preventing teardown or same-id replacement ABA.
    fn commit_inbound_route_network_locked(
        state: &mut State,
        peer: &str,
        message: &ControlMessage,
        network: &str,
    ) {
        let observation = match message {
            ControlMessage::Route(RouteControl::Offer { .. }) => {
                RouteNetworkObservation::InboundOffer
            }
            ControlMessage::Route(RouteControl::Accept { .. }) => {
                RouteNetworkObservation::InboundAccept
            }
            _ => return,
        };
        if !state.networks.iter().any(|joined| joined == network) {
            return;
        }
        let Some(network_epoch) = state.network_epochs.get(network).copied() else {
            return;
        };
        let Some((route_id, incarnation)) = route_control_network_key(message) else {
            return;
        };
        let current = state
            .session
            .as_ref()
            .and_then(|session| session.route(route_id))
            .is_some_and(|route| {
                pubkey_part(route.peer.as_str()) == pubkey_part(peer)
                    && route.incarnation.as_deref() == incarnation
                    && matches!(
                        route.state,
                        RouteState::Offered | RouteState::Incoming | RouteState::Active
                    )
            });
        if !current {
            return;
        }
        let key = (route_id.to_string(), incarnation.map(str::to_string));
        if !observe_route_network(
            &mut state.route_networks,
            key,
            network,
            network_epoch,
            observation,
        ) {
            tracing::warn!(
                route = %route_id,
                network,
                observation = ?observation,
                disposition = "confirmed_route_path_change_refused",
                "inbound control attempted to move an active route to another data-plane network"
            );
        }
    }

    /// Seed `peer_networks` from the daemon's per-network peer list — the same
    /// reliable view the graph reads a peer's "online + on AllMyStuff" from.
    ///
    /// [`Mesh::network_for_peer`] otherwise learns a peer's network *only* from an
    /// inbound app frame (its presence advert, a route `Accept`, …). A peer the
    /// daemon already reports connected — so it shows online and, via its
    /// advertised endpoints, fully wireable — but that we have not yet heard from
    /// directly has no entry, so `network_for_peer` falls back to the **primary**
    /// network. A peer that shares only a **secondary** mesh then has every
    /// control/media frame addressed to the wrong network, where the daemon
    /// silently drops it: the machine "shows up online, in the graph, but the
    /// console wires up with no audio or video, and nothing else reaches it
    /// either." Learning the network from the peer list closes that gap — the
    /// first offer/update already lands on the right mesh, and the peer's reply
    /// keeps the mapping fresh thereafter.
    ///
    /// Records only a network the daemon reports the peer **reachable** on, and
    /// never clobbers one already learned from an inbound frame (that one is
    /// proven to carry traffic to us) — it just fills the gap. The stored id is
    /// the network's `config_id`, matching what an inbound frame records and what
    /// [`Mesh::prune_unjoined_peers`] reconciles against.
    async fn refresh_peer_networks(self: &Arc<Self>) {
        let _refresh = self.peer_refresh_serial.lock().await;
        let epoch = self.daemon_session_epoch.load(Ordering::SeqCst);
        let (client_id, generation, networks) = {
            let state = self.state.lock();
            (
                state.client_id,
                state.network_generation,
                state.networks.clone(),
            )
        };
        let Some(client_id) = client_id else { return };
        for network in networks {
            let Ok(resp) = self
                .client
                .request(&Request::PeersList {
                    network: network.clone(),
                })
                .await
            else {
                continue;
            };
            if !self.network_snapshot_is_current(epoch, client_id, generation, &network) {
                return;
            }
            let Some(peers) = resp
                .data
                .as_ref()
                .and_then(|d| d.get("peers"))
                .and_then(|v| v.as_array())
            else {
                continue;
            };
            let changed = {
                let mut st = self.state.lock();
                if st.client_id != Some(client_id)
                    || st.network_generation != generation
                    || !st.networks.contains(&network)
                    || self.daemon_session_epoch.load(Ordering::SeqCst) != epoch
                {
                    return;
                }
                // This PeersList is the current truth for this network. Clear
                // only this path from prior observations, then add its live
                // rows back. Failed PeersList requests never reach here and
                // therefore never destructively erase a path.
                for paths in st.peer_networks.values_mut() {
                    paths.daemon_reachable.remove(&network);
                    if paths.preferred.as_deref() == Some(network.as_str())
                        && !paths.contains(&network)
                    {
                        paths.preferred = None;
                    }
                }
                seed_peer_networks(&mut st.peer_networks, peers, &network);
                seed_peer_links(&mut st.peer_links, peers, &network)
            };
            // A peer's link class landing (or flipping — an ICE-restart
            // handoff can move a link LAN→STUN mid-life) re-gates its live
            // streams' automatic dials. Compose the allocator plan with the
            // new class before touching a route so a successor cannot inherit
            // the preceding policy generation's cap.
            for (peer, class) in changed {
                let route_ids = self
                    .video
                    .route_ids()
                    .into_iter()
                    .filter(|route_id| {
                        self.route_peer(route_id).is_some_and(|p| {
                            pubkey_part(&p) == peer
                                && self.network_for_route(route_id, &p).as_deref()
                                    == Some(network.as_str())
                        })
                    })
                    .collect::<Vec<_>>();
                if route_ids.is_empty() {
                    continue;
                }
                let changed_route_ids = route_ids.iter().cloned().collect::<HashSet<_>>();
                let policy_plans = {
                    let serial = self.video_policy_apply_serial.lock();
                    // PCM suspension mutates audio accounting. Do it before
                    // the link-class recompute, then publish only the final
                    // generation.
                    self.stop_policy_pcm_for_peer(&peer, &serial);
                    let policy_plans = {
                        let mut policy = self.media_policy.lock();
                        for route_id in &route_ids {
                            let _ = policy.register_route(
                                &peer,
                                route_id,
                                class == crate::video::LinkClass::Lan,
                            );
                        }
                        policy.plans_for_peer(&peer)
                    };
                    for plan in &policy_plans {
                        let cap = Some(plan.route_budget_bps.min(u64::from(u32::MAX)) as u32);
                        if changed_route_ids.contains(&plan.route_id) {
                            if self.video.retune_link_policy(
                                &plan.route_id,
                                class,
                                cap,
                                plan.auto_resolution,
                            ) {
                                tracing::info!(
                                    "link to {} classified {:?} — restarted {route_id} once with the current video policy",
                                    short_id(&peer),
                                    class,
                                    route_id = plan.route_id,
                                );
                            }
                        } else {
                            // Rebalancing one route can change a sibling's
                            // share even when its link class did not move.
                            self.video
                                .apply_policy_cap(&plan.route_id, cap, plan.auto_resolution);
                        }
                    }
                    policy_plans
                };
                self.send_effective_plans(policy_plans).await;
            }
        }
    }

    /// This node's mesh id once known (the daemon device id), else `None`.
    pub fn local_node_id(&self) -> Option<String> {
        self.state
            .lock()
            .session
            .as_ref()
            .map(|s| s.me().to_string())
    }

    /// This node's mesh id, resolved even before the live session starts: the
    /// session id once `start()` has run, else the daemon identity's device id
    /// (available as soon as the control socket is up). So a scan at launch
    /// already carries the real id and the local node never lingers under the
    /// `"this"` placeholder (which is what made this machine briefly show as a
    /// bare "not on AllMyStuff" twin). `None` only when the daemon is
    /// unreachable.
    pub async fn resolve_local_id(&self) -> Option<String> {
        if let Some(id) = self.local_node_id() {
            return Some(id);
        }
        if let Some(id) = self.fetch_identity().await {
            return Some(id);
        }
        // During a daemon/event-socket restart the ephemeral Session is
        // intentionally absent, but the last authenticated local profile is
        // still enough to preserve a user's route intent. Bring-up will replay
        // that intent after it confirms the daemon identity again.
        self.state
            .lock()
            .profile
            .as_ref()
            .map(|profile| profile.node.to_string())
    }

    /// Bring the session online and keep it online: identify, pick a
    /// network, subscribe, pump events — and when the daemon link drops
    /// (daemon crashed, restarted, or wasn't up yet), reconnect on a capped
    /// backoff and re-run the whole bring-up. Historically this was
    /// fire-once: a failed first subscribe returned permanently and a dying
    /// event stream just emitted "disconnected", leaving a running app
    /// meshless until a full relaunch — despite two comments elsewhere
    /// promising "the event pump will retry". Now it actually does.
    pub async fn start(self: Arc<Self>) {
        // Register the runtime we're on so the engine can spawn from any
        // thread — capture/audio callbacks run on their own OS threads, where
        // a bare `tokio::spawn` panics ("no reactor running"). All engine
        // spawns go through `crate::spawn`, which uses this handle. Set first,
        // before anything (the forwarders below) spawns.
        crate::set_runtime(tokio::runtime::Handle::current());

        // Spawn the media forwarders now that we're on a runtime (see
        // `spawn_media_forwarders` — `new` runs in the GUI's sync setup).
        self.spawn_media_forwarders();
        self.spawn_media_policy_sweep();

        // Devices change under a running app; the watcher re-scans on a slow
        // cadence and re-advertises when the picture changed. Once for the
        // engine's life — it survives daemon-link drops untouched.
        self.spawn_inventory_watch();

        // Offers need a deadline: a route offered to a machine whose
        // AllMyStuff app died (daemon still up, so it looks present) used to
        // sit "awaiting accept" forever — a black console with no error.
        self.spawn_offer_reaper();

        // Enforce CEC consent by teardown on a ~2s sweep rather than on every
        // input frame: a lapsed grant (revoke/expiry) tears the session's
        // routes down here. Engine-lifetime; a no-op on a technician node.
        self.spawn_cec_consent_sweep();

        // The daemon-link loop: subscribe → bring up → drain events → and on
        // any end of the stream, around again with a fresh subscription and
        // a full re-bring-up (fresh client_id, channel subscribes, media
        // pipes, presence) — the daemon that comes back knows nothing about
        // the old session. Backoff 1s → 8s while the socket stays dead, reset
        // the moment a subscribe lands.
        let mesh = self.clone();
        crate::spawn(async move {
            let mut backoff = std::time::Duration::from_secs(1);
            loop {
                let (tx, mut rx) = mpsc::channel::<Value>(512);
                let client_id = match mesh.client.subscribe_events(tx).await {
                    Ok(id) => {
                        backoff = std::time::Duration::from_secs(1);
                        id
                    }
                    Err(e) => {
                        tracing::warn!(
                            "mesh: event subscribe failed ({e}); retrying in {backoff:?}"
                        );
                        mesh.emit_status("disconnected", Some(&e.to_string()));
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(std::time::Duration::from_secs(8));
                        continue;
                    }
                };
                mesh.bring_up(client_id).await;
                let daemon_epoch = mesh.daemon_session_epoch.load(Ordering::SeqCst);
                // Base64 media can be large enough that decoding it inline
                // starves route control and presence on the daemon's single
                // event stream. Keep the event reader hot and hand media to
                // the same bounded queue depths already reviewed for the
                // video/audio pipelines. Control events remain ordered here.
                let (video_event_tx, mut video_event_rx) =
                    mpsc::channel::<QueuedVideoEvent>(usize::from(VIDEO_HANDOFF_FRAMES));
                let (audio_event_tx, mut audio_event_rx) =
                    mpsc::channel::<Value>(usize::from(AUDIO_HANDOFF_PACKETS));
                let video_mesh = mesh.clone();
                crate::spawn(async move {
                    while let Some(event) = video_event_rx.recv().await {
                        if !video_mesh.daemon_context_is_current(daemon_epoch, client_id) {
                            break;
                        }
                        video_mesh.handle_base64_video_value(
                            event.value,
                            (event.route_id, event.generation),
                        );
                    }
                });
                let audio_mesh = mesh.clone();
                crate::spawn(async move {
                    while let Some(value) = audio_event_rx.recv().await {
                        if !audio_mesh.daemon_context_is_current(daemon_epoch, client_id) {
                            break;
                        }
                        audio_mesh.handle_value(value).await;
                    }
                });
                while let Some(value) = rx.recv().await {
                    match value.get("kind").and_then(Value::as_str) {
                        Some("video_inbound") => {
                            if let Some(event) = mesh.bind_base64_video_event(value) {
                                if let Err(error) = video_event_tx.try_send(event) {
                                    let dropped = error.into_inner();
                                    mesh.note_base64_video_dispatch_drop(&dropped);
                                }
                            }
                        }
                        Some("audio_inbound") => {
                            if let Err(error) = audio_event_tx.try_send(value) {
                                let dropped = error.into_inner();
                                mesh.note_base64_media_dispatch_drop("audio", &dropped);
                            }
                        }
                        _ => mesh.handle_value(value).await,
                    }
                }
                drop(video_event_tx);
                drop(audio_event_tx);
                mesh.retire_daemon_event_context(client_id);
                // The local injector outlives the daemon event socket. Release
                // every held key and mouse button as soon as that socket dies,
                // even if the daemon never reaches a replacement bring-up.
                mesh.injector.release_all();
                // Stream ended: the daemon died or dropped the socket. Say
                // so, then go re-subscribe — this loop *is* the retry.
                tracing::warn!("mesh: daemon event stream ended — reconnecting");
                mesh.emit_status("disconnected", None);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        });
    }

    /// Invalidate work tied to a daemon event client as soon as its socket
    /// ends. Waiting for the next successful bring-up left a reconnect gap in
    /// which reliable-control and media workers could still target the dead
    /// client. The client-id check makes a delayed old task harmless if a
    /// successor context has already been installed.
    fn retire_daemon_event_context(&self, client_id: ClientId) {
        let retired = {
            let mut active = self.active_daemon_context.lock();
            if active.is_some_and(|context| context.client_id == client_id) {
                *active = None;
                true
            } else {
                false
            }
        };
        if !retired {
            return;
        }
        let daemon_epoch = self
            .daemon_session_epoch
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        self.reliable_control_epoch.send_replace(daemon_epoch);
        self.reliable_control_workers.lock().clear();
        let mut state = self.state.lock();
        if state.client_id == Some(client_id) {
            state.client_id = None;
            state.network_generation = state.network_generation.wrapping_add(1);
        }
    }

    fn bind_base64_video_event(self: &Arc<Self>, value: Value) -> Option<QueuedVideoEvent> {
        let network = value.get("network").and_then(Value::as_str).unwrap_or("");
        let from = value.get("from").and_then(Value::as_str).unwrap_or("");
        let lane = value.get("stream").and_then(Value::as_u64).unwrap_or(0) as u8;
        let (route_id, generation) = self.video_route_generation_for_lane(network, from, lane);
        let Some(route_id) = route_id else {
            if self.diag_ok(&format!("lane:{network}:{}:{lane}", pubkey_part(from))) {
                tracing::warn!(
                    "H.264 samples arriving from {} on lane {lane} but no route maps to it — dropped before fallback dispatch",
                    short_id(from)
                );
            }
            self.nack_dead_lane(network, from, "video", lane);
            return None;
        };
        let Some(generation) = generation else {
            tracing::debug!(
                "base64 H.264 for {route_id} arrived before its video generation started — dropped"
            );
            return None;
        };
        Some(QueuedVideoEvent {
            value,
            route_id,
            generation,
        })
    }

    /// Record a bounded event-pump drop without blocking the daemon reader.
    /// A dropped H.264 access unit invalidates dependent deltas, so ask for a
    /// fresh keyframe through the existing route-control data message. Audio
    /// loss is left to the existing jitter buffer and PLC path.
    fn note_base64_media_dispatch_drop(self: &Arc<Self>, media: &'static str, value: &Value) {
        let network = value.get("network").and_then(Value::as_str).unwrap_or("");
        let from = value.get("from").and_then(Value::as_str).unwrap_or("");
        let lane = value.get("stream").and_then(Value::as_u64).unwrap_or(0) as u8;
        let key = format!(
            "event-media-drop:{network}:{media}:{}:{lane}",
            pubkey_part(from)
        );
        if self.diag_ok(&key) {
            tracing::warn!(
                peer = %short_id(from),
                network,
                media,
                lane,
                "bounded daemon media dispatcher dropped an overloaded packet"
            );
        }
    }

    fn note_base64_video_dispatch_drop(self: &Arc<Self>, event: &QueuedVideoEvent) {
        self.note_base64_media_dispatch_drop("video", &event.value);
        if self.video_generation_is_current(&event.route_id, event.generation) {
            let mesh = self.clone();
            let route_id = event.route_id.clone();
            crate::spawn(async move {
                let _ = mesh.request_refresh_for_recovery(route_id).await;
            });
        }
    }

    /// Tear down process-local media state before installing a daemon's fresh
    /// session. The restarted daemon owns none of the old lanes/routes; keeping
    /// allocator ids, captures, decoders, or lane bindings would let a later
    /// route inherit stale priority and audio reservations. Deliberately do not
    /// send lane-close requests here: they would target the new daemon and could
    /// race a successor route onto the same positional lane.
    async fn reset_media_for_fresh_session(&self) {
        let daemon_epoch = self
            .daemon_session_epoch
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        *self.active_daemon_context.lock() = None;
        self.reliable_control_epoch.send_replace(daemon_epoch);
        let retired_reliable_workers = {
            let mut workers = self.reliable_control_workers.lock();
            let count = workers.len();
            workers.clear();
            count
        };
        if retired_reliable_workers != 0 {
            tracing::info!(
                daemon_epoch,
                retired_reliable_workers,
                "retired stale reliable route-control workers for fresh daemon session"
            );
        }
        self.rotate_route_boot();
        // Invalidate every old route gate atomically before doing any teardown
        // work. The rest of bring-up awaits identity/network queries; leaving
        // the old Session visible during those awaits would let a late media
        // task repopulate the maps after this reset.
        let old_session = {
            let mut state = self.state.lock();
            state.client_id = None;
            state.network_generation = state.network_generation.wrapping_add(1);
            state.network_epochs.clear();
            state.route_networks.clear();
            state.peer_networks.clear();
            state.peer_links.clear();
            state.session.take()
        };
        // The injector, not the vanished daemon session, is authoritative for
        // what this process still holds on the OS. Lift every old key and mouse
        // button before installing replacement routes.
        self.injector.release_all();
        let mut routes = std::collections::BTreeSet::new();
        if let Some(session) = old_session.as_ref() {
            routes.extend(
                session
                    .routes()
                    .filter(|live| {
                        matches!(
                            live.route.media,
                            MediaKind::Audio | MediaKind::Display | MediaKind::Video
                        )
                    })
                    .map(|live| live.route.id.clone()),
            );
        }
        routes.extend(self.video.route_ids());
        routes.extend(self.audio_encoders.lock().keys().cloned());
        routes.extend(self.audio_decoders.lock().keys().cloned());
        routes.extend(self.pcm_audio_routes.lock().keys().cloned());

        if !routes.is_empty() {
            tracing::info!(
                "fresh daemon session — retiring {} stale local media route(s)",
                routes.len()
            );
        }
        for route_id in &routes {
            let _lifecycle = self.lock_route_lifecycle(route_id).await;
            let mut generations = self.video_route_generations.lock();
            generations.retire(route_id);
            self.reset_video_receive_generation_locked(route_id, &generations);
            drop(generations);
            self.audio.stop(route_id);
            self.video.stop(route_id);
        }

        {
            let _serial = self.video_policy_apply_serial.lock();
            self.media_policy.lock().reset();
        }
        self.effective_plan_echo_epoch
            .fetch_add(1, Ordering::SeqCst);
        self.effective_plan_echoes.lock().clear();
        // Keep the user's requested quality posture beside desired route
        // intent. The replacement route replays it after activation; clearing
        // it here is what made a daemon reconnect silently fall back to Auto.
        self.pcm_audio_routes.lock().clear();
        self.audio_decoders.lock().clear();
        self.audio_encoders.lock().clear();
        *self.video_in.lock() = VideoAssembler::new();
        self.video_watchers.lock().reset_for_reconnect();
        self.video_arrivals.lock().clear();
        self.video_in_stats.lock().clear();
        self.video_diag_last.lock().clear();
        self.dead_lane_since.lock().clear();
        self.refresh_asks.lock().clear();
        self.video_lane_pins.lock().clear();
        self.video_lane_binds.lock().clear();
        self.active_media_incarnations.lock().clear();
        self.input_in_seq.lock().clear();
        *self.video_switch_guards.lock() = VideoSwitchGuards::default();
        self.daemon_video.store(false, Ordering::SeqCst);
        self.daemon_audio.store(false, Ordering::SeqCst);
        self.network_subscriptions.lock().clear();
        self.daemon_media_pipes.store(false, Ordering::SeqCst);
        self.daemon_lanes.store(1, Ordering::SeqCst);
    }

    /// One full session bring-up against a freshly-subscribed daemon link:
    /// identity → profile → networks → media-pipe probe → channel
    /// subscribes → ownership/presence. Runs on every (re)connect — after a
    /// daemon restart nothing of the old session survives daemon-side, so
    /// everything is re-established, and peers re-learn us from the fresh
    /// presence broadcast.
    async fn bring_up(self: &Arc<Self>, client_id: ClientId) {
        self.reset_media_for_fresh_session().await;
        // Identity → our node id + presence profile. The label is the
        // user's optional override; `build_profile` falls back to the
        // hostname when it's unset.
        let me = self
            .fetch_identity()
            .await
            .unwrap_or_else(|| NodeId::this().to_string());
        let label = self.fetch_identity_label().await;
        // Capability fields that presence advertises must be known before the
        // profile is built and broadcast. Probing lane count later during
        // VideoSubscribe left every fresh session permanently advertising a
        // single lane even when the daemon reported a pool.
        let daemon_status = self
            .client
            .request(&Request::Status)
            .await
            .ok()
            .and_then(|response| response.data);
        if let Some(lanes) = daemon_status
            .as_ref()
            .and_then(|data| data.get("media_lanes"))
            .and_then(Value::as_u64)
        {
            if lanes > u64::from(PRENEGOTIATED_MEDIA_LANES) {
                tracing::info!(
                    reported_lanes = lanes,
                    usable_lanes = PRENEGOTIATED_MEDIA_LANES,
                    "dynamic media lanes require SDP renegotiation; restricting video/audio to pre-negotiated lane 0"
                );
            }
            self.daemon_lanes
                .store(PRENEGOTIATED_MEDIA_LANES, Ordering::SeqCst);
        }
        let media_pipes = daemon_status
            .as_ref()
            .and_then(|data| data.get("media_pipes"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.daemon_media_pipes.store(media_pipes, Ordering::SeqCst);
        let profile = self.build_profile(&me, label);
        // Join the claim-rendezvous networks *before* listing networks, so
        // the LAN claim network (and the claim-code network, when public
        // claims are on) is in the set we subscribe below. This is what
        // makes a fresh, otherwise-unconfigured box discoverable by a
        // same-LAN claimer with zero setup.
        self.ensure_claim_networks().await;
        // Every joined network; route control/media operate on the primary.
        let networks = self.fetch_networks().await;
        let primary = networks.first().cloned();

        {
            let mut st = self.state.lock();
            st.client_id = Some(client_id);
            st.session = Some(Session::new(me.clone()));
            st.profile = Some(profile.clone());
            st.network = primary.clone();
            st.networks = networks.clone();
            st.network_generation = st.network_generation.wrapping_add(1);
            reconcile_network_epochs(&mut st, &networks, true);
        }
        let daemon_epoch = self.daemon_session_epoch.load(Ordering::SeqCst);
        *self.active_daemon_context.lock() = Some(DaemonContext {
            epoch: daemon_epoch,
            client_id,
        });

        // Probe the daemon's binary-media-pipe capability up front (the version
        // pin can't gate it — the feature predates a release). This gates the
        // inbound source pipe below and the outbound sends in
        // `send_video_track`/`send_audio_track`. A daemon without it (an older
        // build still on the socket) keeps streaming over the base64 path.
        // Inbound media (H.264/Opus from peers) rides a dedicated binary pipe —
        // no base64 — instead of the JSON event socket. Open it for our event
        // `client_id` before subscribing video/audio, so the daemon has the
        // sink registered when its pumps start. When the daemon doesn't speak it,
        // skip the pipe entirely — its pumps then emit base64
        // `video_inbound`/`audio_inbound` events, which the value dispatcher
        // below still decodes and handles.
        // The v1 binary source frame omits its network id. It is unambiguous
        // only when one network is joined; with two PeerSessions the same
        // peer+lane can exist independently on both. In that case leave the
        // binary sink unregistered so the daemon uses its base64 event path,
        // whose VideoInbound/AudioInbound events carry `network`.
        if media_pipes && MEDIA_SOURCE_HAS_NETWORK_IDENTITY && networks.len() == 1 {
            let source_network = networks[0].clone();
            let (media_tx, mut media_rx) = mpsc::channel::<ProfiledInboundFrame>(256);
            match self
                .client
                .subscribe_profiled_media_source(client_id, media_tx)
                .await
            {
                Ok(()) => {
                    tracing::info!(
                        "binary media pipes active — H.264/Opus carry raw over the IPC (no base64) in both directions"
                    );
                    let mesh = self.clone();
                    let source_epoch = self.daemon_session_epoch.load(Ordering::SeqCst);
                    crate::spawn(async move {
                        while let Some(mut profiled) = media_rx.recv().await {
                            if mesh.daemon_session_epoch.load(Ordering::SeqCst) != source_epoch {
                                tracing::debug!(
                                    source_epoch,
                                    "retiring stale daemon media-source task"
                                );
                                break;
                            }
                            profiled.record_dispatch_wait();
                            let profile_id = profiled.profile_id;
                            let f = profiled.frame;
                            match f.kind {
                                MEDIA_KIND_VIDEO => mesh.handle_video_inbound_profiled(
                                    &source_network,
                                    &f.from,
                                    f.stream,
                                    f.rtp_timestamp,
                                    f.key,
                                    f.data,
                                    profile_id,
                                    None,
                                ),
                                MEDIA_KIND_AUDIO => mesh.handle_audio_inbound(
                                    &source_network,
                                    &f.from,
                                    f.stream,
                                    f.rtp_timestamp,
                                    f.data,
                                ),
                                _ => {}
                            }
                        }
                    });
                }
                Err(e) => {
                    // Registered nothing daemon-side, so its pumps stay on base64
                    // events — still handled below.
                    tracing::warn!("mesh: media-source pipe unavailable, using base64 events: {e}");
                    self.daemon_media_pipes.store(false, Ordering::SeqCst);
                }
            }
        } else if media_pipes {
            tracing::warn!(
                networks = networks.len(),
                "binary media-source v1 omits network identity and can outlive network-set changes; using network-tagged events to prevent cross-mesh lane aliasing"
            );
        } else {
            tracing::info!(
                "daemon has no binary media pipes — inbound video/audio arrive as base64 events (rebuild myownmesh from this branch to enable the binary pipes)"
            );
        }

        if networks.is_empty() {
            // Still run the claim-status check (it sanitizes stale fleet
            // residue and refreshes the UI); the broadcasts inside are
            // no-ops with no networks to send on.
            self.ownership_check(None).await;
            self.emit_status("no_network", None);
        } else {
            // Every AllMyStuff channel on *every* network. Presence + the
            // owned-fleet roster so two machines discover each other (and
            // converge their fleet) no matter which network the daemon lists
            // first — and control + media too, because point-to-point traffic
            // is addressed to whichever network *we* last saw the peer on,
            // which need not be the peer's first-listed one. With these on
            // the primary only, a claim or route offer arriving on a shared
            // secondary network had no subscriber on the receiving side and
            // the daemon silently dropped it.
            self.subscribe_channels(client_id, &networks).await;
            // Learn which network each *already-connected* peer lives on from the
            // daemon's peer list (their "approved" events fired before we
            // subscribed, so we'd otherwise only learn it once they send us a
            // frame). Without this the first offer/update to a peer that shares
            // only a secondary mesh is addressed to the primary and dropped.
            self.refresh_peer_networks().await;
            // App-load trigger of the claim-status check: sanitize stale
            // fleet residue, then assert presence + roster to everyone.
            self.ownership_check(None).await;
            self.emit_status("live", None);
        }

        // The daemon-backed Session above is intentionally fresh. Reinstall
        // only routes the local user still wants, after channel subscriptions
        // and presence/ownership adverts are in place. A control Offer may
        // still outrun its presence message on another SCTP channel; fenced
        // receivers hold it out and the existing offer sweep retries it.
        self.replay_desired_routes(None).await;

        // No periodic re-broadcast: gossip is event-driven. Late joiners are
        // covered twice over — the daemon's "peer approved" event triggers a
        // targeted ownership check at them, and a presence advert carrying a
        // boot id we haven't recorded (their app just started while the
        // daemon link stayed up) gets answered with our state directly. The
        // mesh carries traffic when something *happens*, not on a heartbeat.
        // (The inventory watcher lives in `start` — engine-lifetime, not
        // per-connect.)
    }

    /// Sweep outbound route offers nobody has answered and expire them to
    /// `Rejected` with a reason the UI can show. The wire has no offer
    /// deadline and the session is deliberately clock-free, so the timer
    /// lives here: the first sweep that sees an offer stamps it, and one
    /// still `Offered` [`OFFER_TIMEOUT`] later flips to rejected — the
    /// console then explains "no answer" instead of connecting forever. A
    /// late `Accept` after expiry is harmless (the route reads rejected
    /// here; re-connecting mints a fresh route id).
    fn spawn_offer_reaper(self: &Arc<Self>) {
        let mesh = Arc::downgrade(self);
        crate::spawn(async move {
            loop {
                tokio::time::sleep(OFFER_SWEEP).await;
                let Some(mesh) = mesh.upgrade() else { break };
                let teardown_retries = {
                    let now = Instant::now();
                    let mut pending = mesh.pending_teardowns.lock();
                    pending.retain(|(route_id, incarnation), teardown| {
                        let keep = now.duration_since(teardown.created) < OFFER_TIMEOUT;
                        if !keep {
                            tracing::warn!(
                                route = %route_id,
                                incarnation = ?incarnation,
                                peer = %short_id(&teardown.peer),
                                "route teardown acknowledgement timed out; retiring retry state"
                            );
                        }
                        keep
                    });
                    pending.values().cloned().collect::<Vec<_>>()
                };
                for teardown in teardown_retries {
                    let result = if let Some(network) = teardown.network.as_deref() {
                        mesh.send_control_retry_on_network(
                            &teardown.peer,
                            &teardown.message,
                            network,
                        )
                        .await
                    } else {
                        mesh.send_control_retry(&teardown.peer, &teardown.message)
                            .await
                    };
                    if let Err(error) = result {
                        tracing::debug!(
                            peer = %short_id(&teardown.peer),
                            error = %error,
                            "route teardown retry did not reach the peer app"
                        );
                    }
                }
                let mut expired: Vec<(String, Option<String>)> = Vec::new();
                let mut retries: Vec<(String, ControlMessage)> = Vec::new();
                {
                    let mut seen = mesh.offer_first_seen.lock();
                    let mut st = mesh.state.lock();
                    let Some(session) = st.session.as_mut() else {
                        seen.clear();
                        continue;
                    };
                    let offered: std::collections::HashSet<(String, Option<String>)> = session
                        .routes()
                        .filter(|r| {
                            r.origin == allmystuff_session::Origin::Outbound
                                && r.state == allmystuff_session::RouteState::Offered
                        })
                        .map(|r| (r.route.id.clone(), r.incarnation.clone()))
                        .collect();
                    // Anything no longer an unanswered outbound offer stops
                    // being timed (accepted, rejected, torn down, gone).
                    seen.retain(|key, _| offered.contains(key));
                    let now = std::time::Instant::now();
                    for (id, incarnation) in offered {
                        let key = (id.clone(), incarnation.clone());
                        let first = *seen.entry(key.clone()).or_insert(now);
                        let did_expire = now.duration_since(first) >= OFFER_TIMEOUT
                            && session.expire_offer_incarnation(
                                &id,
                                incarnation.as_deref(),
                                "no answer from the far side — its AllMyStuff app may not be \
                                 running (its mesh daemon can still advertise it)",
                            );
                        if did_expire {
                            seen.remove(&key);
                            expired.push(key);
                        } else if let Some(route) = session.route(&id).filter(|route| {
                            route.state == RouteState::Offered && route.incarnation == incarnation
                        }) {
                            retries.push((
                                route.peer.to_string(),
                                ControlMessage::Route(RouteControl::Offer {
                                    route: route.route.clone(),
                                    incarnation: route.incarnation.clone(),
                                    video: route.video.clone(),
                                    audio: route.audio.clone(),
                                    session: route.term_session.clone(),
                                }),
                            ));
                        }
                    }
                }
                for (peer, message) in retries {
                    if let Err(error) = mesh.send_control_retry(&peer, &message).await {
                        tracing::debug!(
                            peer = %short_id(&peer),
                            error = %error,
                            "route offer retry did not reach the peer"
                        );
                    }
                }
                if !expired.is_empty() {
                    {
                        let mut state = mesh.state.lock();
                        for key in &expired {
                            state.route_networks.remove(key);
                        }
                    }
                    let replay_peers = {
                        let desired = mesh.desired_routes.lock();
                        expired
                            .iter()
                            .filter_map(|(id, _)| desired.get(id).map(|route| route.peer.clone()))
                            .collect::<std::collections::BTreeSet<_>>()
                    };
                    for (id, _) in &expired {
                        tracing::warn!(
                            "route offer {id} went unanswered for {OFFER_TIMEOUT:?} — expired \
                             (is the far side's AllMyStuff app running?)"
                        );
                    }
                    mesh.emit_snapshot();
                    // The Session attempt is ephemeral; the user's still-open
                    // console intent is not. Rebuild it with a fresh wire
                    // incarnation on the same established app-data path. This
                    // also recovers a same-boot outage where no new presence
                    // event exists to trigger replay.
                    for peer in replay_peers {
                        mesh.replay_desired_routes(Some(&peer)).await;
                    }
                }
            }
        });
    }

    /// Expire stale media-path estimates independently of route-offer
    /// housekeeping. This task only retunes active media routes and echoes the
    /// resulting effective media plans over their already-established data
    /// channel; it cannot delay or mutate the offer/signaling reaper.
    fn spawn_media_policy_sweep(self: &Arc<Self>) {
        const SWEEP: std::time::Duration = std::time::Duration::from_secs(5);
        let mesh = Arc::downgrade(self);
        crate::spawn(async move {
            loop {
                tokio::time::sleep(SWEEP).await;
                let Some(mesh) = mesh.upgrade() else { break };
                let plans = {
                    let serial = mesh.video_policy_apply_serial.lock();
                    let plans = mesh.media_policy.lock().expire_stale_path_estimates();
                    mesh.apply_video_policy_caps_locked(&plans, &serial);
                    plans
                };
                mesh.send_effective_plans(plans).await;
            }
        });
    }

    /// Enforce CEC consent by teardown on a slow sweep instead of on every
    /// frame. A dialed technician's screen-view and control authority is the
    /// customer's live consent grant; that grant is checked once when a route
    /// is *offered* (admission — the offer gate's [`Self::sender_may_drive`] /
    /// [`Self::cec_screen_offer_denied`]) and then **not re-evaluated per
    /// frame**. This sweep is the other half: every [`SWEEP`] it re-checks each
    /// live CEC route against those same gates and tears down any that a lapsed
    /// grant — revoked, expired, or an "Approve Once" that ended — no longer
    /// covers. A revoke that lands between sweeps still bites at once through its
    /// own explicit teardown ([`Self::cec_revoke`]); this backstops **expiry**,
    /// which nothing else tears down, and closes its screen twin — a lapsed
    /// grant used to leave the customer *still streaming their screen* until they
    /// disconnected, because only the input plane was gated per frame. The cost
    /// is one grant evaluation per live CEC route every couple of seconds, versus
    /// the tens per second an input stream drove. Customer-side only: a
    /// technician node hosts nothing consent-gated and `knows_technician` is
    /// false there, so the body no-ops.
    fn spawn_cec_consent_sweep(self: &Arc<Self>) {
        const SWEEP: std::time::Duration = std::time::Duration::from_secs(2);
        let mesh = Arc::downgrade(self);
        crate::spawn(async move {
            // The last `cec://viewing` map this sweep emitted — technician
            // canonical id → (screen live, control live). `None` until the
            // first pass so a fresh node always emits once (even an empty
            // map), giving a GUI that hydrated before us a baseline.
            let mut last_viewing: Option<std::collections::BTreeMap<String, (bool, bool)>> = None;
            loop {
                tokio::time::sleep(SWEEP).await;
                let Some(mesh) = mesh.upgrade() else { break };
                // NOTE: no `is_technician` skip here. That early-out assumed a
                // technician node hosts nothing consent-gated — but the role
                // flips permanently on the first dial, and a DUAL-ROLE node
                // (dialed someone once, yet also reachable as a customer) very
                // much still hosts a consent-gated screen. On a pure technician
                // node the body no-ops anyway: its routes point at customers,
                // and `knows_technician` is false for those peers.
                // Snapshot every live route (peer, id, is-screen, drive-plane)
                // under the state lock, then drop it before touching the CEC
                // store or tearing anything down (`disconnect` re-locks state).
                let routes: Vec<(String, String, bool, Option<DrivePlane>)> = {
                    let st = mesh.state.lock();
                    match st.session.as_ref() {
                        Some(session) => session
                            .routes()
                            .filter(|r| r.is_active())
                            .map(|r| {
                                (
                                    r.peer.as_str().to_string(),
                                    r.route.id.clone(),
                                    matches!(r.route.media, MediaKind::Display | MediaKind::Video),
                                    route_drive_plane(&r.route),
                                )
                            })
                            .collect(),
                        None => Vec::new(),
                    }
                };
                let mut stale_routes: Vec<String> = Vec::new();
                let mut lapsed_techs: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                // What each technician is ACTUALLY doing right now, from the
                // routes themselves: screen = a live display route, control = a
                // live input route. This — not session state — is what the
                // customer's "Viewing/Controlling your screen" chip keys on:
                // a technician closing their console tears the routes down but
                // sends no session event, and chat keeps the session itself
                // alive, so session state over-claims. A route this very pass
                // is tearing down for lapsed consent doesn't count.
                let mut viewing: std::collections::BTreeMap<String, (bool, bool)> =
                    std::collections::BTreeMap::new();
                for (peer, route_id, is_screen, plane) in routes {
                    // Only CEC technicians are consent-gated; an owner/fleet or
                    // ordinary peer's routes are none of this sweep's business.
                    if !mesh.cec.knows_technician(&peer) {
                        continue;
                    }
                    let screen_lapsed = is_screen && mesh.cec_screen_offer_denied(&peer);
                    let drive_lapsed = plane.is_some_and(|pl| !mesh.sender_may_drive(&peer, pl));
                    if screen_lapsed || drive_lapsed {
                        stale_routes.push(route_id);
                        lapsed_techs.insert(crate::cec::pubkey_part(&peer).to_string());
                        continue;
                    }
                    let entry = viewing
                        .entry(crate::cec::pubkey_part(&peer).to_string())
                        .or_insert((false, false));
                    entry.0 |= is_screen;
                    entry.1 |= plane == Some(DrivePlane::Input);
                }
                if !stale_routes.is_empty() {
                    for id in &stale_routes {
                        tracing::info!(
                            "CEC consent lapsed — tearing down route {id} (approval revoked or expired)"
                        );
                        let _ = mesh.disconnect(id.clone()).await;
                    }
                    // End the lapsed technicians' sessions so the customer's
                    // "connected" banner clears — a route teardown alone emits no
                    // session event — and retire any leftover "Approve Once" grant.
                    for tech in lapsed_techs {
                        for sid in mesh.cec.end_sessions_for(&tech) {
                            mesh.sink.emit(
                                "cec://session",
                                json!({ "session_id": sid, "state": "ended" }),
                            );
                        }
                        mesh.cec.retire_once(&tech);
                    }
                    mesh.cec_emit_grants();
                    mesh.emit_snapshot();
                }
                // Tell the GUI what's live whenever the picture changes (and
                // once at startup, so a GUI that hydrated before this sweep
                // gets its baseline). The event carries the whole map, so a
                // missed frame self-heals on the next change.
                if last_viewing.as_ref() != Some(&viewing) {
                    mesh.sink.emit("cec://viewing", cec_viewing_value(&viewing));
                    last_viewing = Some(viewing);
                }
            }
        });
    }

    /// `cec_viewing` (customer): what each connected technician is actually
    /// doing right now — `{ techs: { <canonical tech>: { screen, control } } }`
    /// — derived from the LIVE routes, not session state. The event twin
    /// (`cec://viewing`) is pushed by the consent sweep on every change; this
    /// command is the pull for GUI hydrate, so an app that starts mid-session
    /// paints the chip without waiting for a transition.
    pub async fn cec_viewing(self: &Arc<Self>) -> Result<Value, String> {
        let mut viewing: std::collections::BTreeMap<String, (bool, bool)> =
            std::collections::BTreeMap::new();
        {
            let st = self.state.lock();
            if let Some(session) = st.session.as_ref() {
                for r in session.routes() {
                    let peer = r.peer.as_str();
                    if !self.cec.knows_technician(peer) {
                        continue;
                    }
                    let entry = viewing
                        .entry(crate::cec::pubkey_part(peer).to_string())
                        .or_insert((false, false));
                    entry.0 |= matches!(r.route.media, MediaKind::Display | MediaKind::Video);
                    entry.1 |= route_drive_plane(&r.route) == Some(DrivePlane::Input);
                }
            }
        }
        Ok(cec_viewing_value(&viewing))
    }

    /// Re-scan this machine's inventory every [`INVENTORY_RESCAN`] and
    /// refresh the live presence profile when the device picture changed,
    /// so a display that woke (or detached), a camera that appeared, or a
    /// changed default reaches the graph — local drawer and peers both —
    /// without an app restart. The scan is cheap by design ("cheap enough
    /// to call on a button press"), and steady state broadcasts nothing.
    fn spawn_inventory_watch(self: &Arc<Self>) {
        // A Windows inventory pass launches several CIM/PowerShell probes.
        // Keep that unrelated and bursty work out of an explicitly profiled
        // video run. Daemon bring-up still builds the initial presence profile
        // after this engine-lifetime watcher decision, so this only suspends
        // later hot-plug refreshes for the opt-in profiler process.
        if crate::pipeline_profile::enabled() {
            tracing::info!(
                disposition = "inventory_rescan_quiesced_for_video_profile",
                "periodic inventory rescans are paused during video profiling"
            );
            return;
        }
        const INVENTORY_RESCAN: std::time::Duration = std::time::Duration::from_secs(10);
        let mesh = Arc::downgrade(self);
        crate::spawn(async move {
            loop {
                tokio::time::sleep(INVENTORY_RESCAN).await;
                let Some(mesh) = mesh.upgrade() else {
                    return;
                };
                let Some(node) = mesh.state.lock().profile.as_ref().map(|p| p.node.clone()) else {
                    continue; // live session not up yet
                };
                let scanned = tokio::task::spawn_blocking(move || {
                    let inv = allmystuff_inventory::scan();
                    (
                        allmystuff_bridge::node_summary(&inv),
                        Self::advertised_capabilities(&inv, &node),
                    )
                })
                .await;
                let Ok((summary, capabilities)) = scanned else {
                    continue;
                };
                let changed = {
                    let mut st = mesh.state.lock();
                    let Some(p) = st.profile.as_mut() else {
                        continue;
                    };
                    let fresh = profile_fingerprint(&summary, &capabilities);
                    if profile_fingerprint(&p.summary, &p.capabilities) == fresh {
                        false
                    } else {
                        p.summary = summary;
                        p.capabilities = capabilities;
                        true
                    }
                };
                if changed {
                    tracing::info!("device picture changed on rescan — re-broadcasting presence");
                    mesh.broadcast_presence().await;
                    // Keep the peer-list copy of the summary fresh too, so peers
                    // that read it from the capability matrix (not the presence
                    // advert) see the new stats.
                    mesh.advertise_capabilities().await;
                    mesh.emit_snapshot();
                }
            }
        });
    }

    async fn fetch_identity(&self) -> Option<String> {
        let resp = self.client.request(&Request::IdentityShow).await.ok()?;
        resp.data?
            .get("device_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }

    /// The user's optional display-name override from the daemon identity.
    /// `None` (or empty) means "use the hostname".
    async fn fetch_identity_label(&self) -> Option<String> {
        let resp = self.client.request(&Request::IdentityShow).await.ok()?;
        resp.data?
            .get("label")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty())
    }

    /// Update this node's display label (the identity override) on the live
    /// presence profile and re-broadcast so peers pick it up. An empty label
    /// resets the display to the machine hostname.
    pub async fn set_label(self: &Arc<Self>, label: String) {
        {
            let mut st = self.state.lock();
            if let Some(p) = st.profile.as_mut() {
                p.label = if label.trim().is_empty() {
                    p.hostname.clone()
                } else {
                    label
                };
            }
        }
        self.broadcast_presence().await;
    }

    /// Recompute this node's advertised `sites` from a fresh scan + the
    /// current exposed set, then re-broadcast presence — so a change to what
    /// the owner exposes reaches peers' Sites tabs promptly. User-triggered
    /// and rare, so the scan here is well off any hot path.
    async fn restamp_profile(self: &Arc<Self>) {
        // Scan off the async runtime (lsof on macOS, /proc walks on Linux).
        let mesh = self.clone();
        let sites = tokio::task::spawn_blocking(move || {
            let inv = allmystuff_inventory::scan();
            allmystuff_bridge::sites::sites_from_inventory(&inv, &mesh.sites.exposed_map())
        })
        .await
        .unwrap_or_default();
        let count = sites.len();
        {
            let mut st = self.state.lock();
            if let Some(p) = st.profile.as_mut() {
                p.sites = sites;
            }
        }
        tracing::info!("re-advertising {count} exposed site(s) to peers");
        self.reassert_presence().await;
        // Our own UI (and any console window) reflects the change at once.
        self.emit_snapshot();
    }

    /// Push this node's presence out so a change reaches every connected
    /// peer: the broadcast to all, *and* a targeted send to each peer the
    /// session already knows. The targeted half is the belt-and-suspenders —
    /// a `ChannelSendAll` can miss an already-connected peer mid-session,
    /// where a `ChannelSendTo` per peer lands (the same path that answers a
    /// peer that just restarted).
    async fn reassert_presence(self: &Arc<Self>) {
        self.broadcast_presence().await;
        let peers: Vec<String> = {
            let st = self.state.lock();
            st.session
                .as_ref()
                .map(|s| s.peers().map(|p| p.node.to_string()).collect())
                .unwrap_or_default()
        };
        for peer in peers {
            self.send_presence_to(&peer).await;
        }
    }

    /// All joined networks' config ids. The daemon wraps the list as
    /// `{ "networks": [...] }`, so we read that field (an earlier version
    /// called `as_array()` on the wrapper and always got nothing — which left
    /// presence un-subscribed and peers unable to see each other).
    async fn fetch_networks(&self) -> Vec<String> {
        let Some(resp) = self.client.request(&Request::NetworksList).await.ok() else {
            return Vec::new();
        };
        resp.data
            .as_ref()
            .and_then(|d| d.get("networks"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|n| {
                        n.get("config_id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn build_profile(&self, me: &str, label_override: Option<String>) -> NodeProfile {
        let inv = allmystuff_inventory::scan();
        let node = NodeId::from(me);
        let hostname = inv.host.hostname.clone();
        // Display name = override if the user set one, else the hostname.
        let label = label_override
            .filter(|l| !l.trim().is_empty())
            .unwrap_or_else(|| hostname.clone());
        NodeProfile {
            protocol: PROTOCOL_VERSION,
            node: node.clone(),
            // Cloned (not moved) so `fleet_owner` below can fall back to this
            // device's label for an unnamed fleet it owns.
            label: label.clone(),
            hostname,
            summary: allmystuff_bridge::node_summary(&inv),
            capabilities: Self::advertised_capabilities(&inv, &node),
            // Tell peers who owns this device and whether it's up for
            // adoption, so they can't silently grab a box that's already
            // spoken for (or one that was never put into claim mode).
            owner: self.ownership.owner().map(NodeId::from),
            claimable: self.ownership.claimable(),
            boot: self.route_incarnation_clock.lock().boot,
            // This build can host mesh-native terminals on every OS the
            // app ships for (openpty / ConPTY) — advertise it so peers
            // know to offer one. Runtime spawn failures still degrade
            // in-band (the viewer sees the error in its terminal). Same
            // for file sessions: plain std::fs everywhere we ship.
            // …and it speaks the virtual-rooms plane (invites, join/leave,
            // chat on CHANNEL_ROOMS), so room UIs can badge members that
            // can't hear them. Camera streaming likewise rides every OS
            // (V4L2 / AVFoundation / Media Foundation); a camera that
            // won't open at route time degrades in-band too (`vstat`).
            features: Self::advertised_features(),
            // Only the services the owner opted to expose (the exposed set is
            // the host's allow-list); a scan that found a dozen listeners
            // advertises only those, each under its chosen name. Empty until
            // the user exposes one.
            sites: allmystuff_bridge::sites::sites_from_inventory(&inv, &self.sites.exposed_map()),
            // The build this process is running, so a fleet peer can tell
            // when this machine is behind the channel's latest release and
            // offer to upgrade it. It's the running binary's own version: a
            // staged update only becomes our reported version once we restart
            // onto it (which an `Upgrade` triggers), so this stays honest.
            version: env!("CARGO_PKG_VERSION").to_string(),
            // The fleet's display name ("Casey"), shared fleet-wide (handed
            // down with the fleet key), so a peer groups + labels this device's
            // fleet straight from presence. Empty when not in a fleet / unnamed.
            fleet_name: self.ownership.fleet_name(),
            // The fleet **owner's** (person) name — never the owner device's
            // hostname. See [`Mesh::fleet_owner_name`].
            fleet_owner: self.fleet_owner_name(&label),
            // An ordinary machine is not a KVM appliance — only a NanoKVM-class
            // device (its Go mesh bridge) ever fills this in. See FEATURE_KVM.
            kvm: None,
            // Stamped per send (broadcast_presence / send_presence_to), not
            // at build: a profile can sit in state for minutes between
            // sends, and a stale stamp would read as clock skew.
            sent_at: 0,
        }
    }

    /// The fleet owner's display name to advertise in presence — the *person*
    /// who owns the fleet, never the owner device's hostname. A fleet is named
    /// for its owner, so this is the fleet name when one is set; otherwise the
    /// owner device falls back to its own label (`own_label`) so an as-yet-
    /// unnamed fleet still says *who* owns it, while a member of an unnamed
    /// fleet leaves it empty (it can't name the owner until the fleet is named
    /// or — once roles converge — the signed roster tells it who the owner is).
    fn fleet_owner_name(&self, own_label: &str) -> String {
        let name = self.ownership.fleet_name();
        if !name.trim().is_empty() {
            name
        } else if self.ownership.is_fleet_owner() {
            own_label.to_string()
        } else {
            String::new()
        }
    }

    /// Advertise an AllMyStuff marker (plus this build's features and version)
    /// on the **mesh** capability matrix, so every peer learns through the
    /// reliable handshake + peer-list that this is an app node — not a bare
    /// `myownmesh` daemon — independent of the bespoke presence broadcast. The
    /// receiver flips a peer to "on AllMyStuff" off its polled peer view, so a
    /// dropped presence advert no longer leaves a connected peer mesh-only.
    /// Idempotent: `CapabilitiesSet` replaces the advertised matrix, so
    /// re-running it on each network sync is cheap.
    async fn advertise_capabilities(&self) {
        let (networks, profile) = {
            let st = self.state.lock();
            (st.networks.clone(), st.profile.clone())
        };
        let mut tags = vec![allmystuff_protocol::CAP_TAG_ALLMYSTUFF.to_string()];
        if let Some(p) = &profile {
            tags.extend(p.features.iter().cloned());
        }
        let capabilities = json!({
            "tags": tags,
            "app_version": env!("CARGO_PKG_VERSION"),
            // The daemon's `CapabilityAdvert` is a typed struct — only `tags`,
            // `app_version`, `max_connections`, and a freeform `extra` survive
            // its (de)serialization. Anything app-specific MUST ride `extra`,
            // or serde drops it at the control boundary (which silently sank an
            // earlier attempt to carry these at the top level). So nest the
            // embedder data under `extra`:
            //  - summary: the device stats (OS / CPU / RAM / device count), so a
            //    peer whose bespoke presence frame was missed still shows them.
            //  - endpoints: the wireable control / audio / video / display sinks
            //    & sources rooms and remote-control resolve a route through.
            //    These used to ride *only* the flaky presence advert, so a missed
            //    frame left a peer showing its buttons but advertising no
            //    endpoint — "no audio/control/video path to that machine". The
            //    polled peer list is reliable, so a path resolves regardless.
            "extra": {
                "summary": profile.as_ref().map(|p| &p.summary),
                "endpoints": profile.as_ref().map(|p| &p.capabilities),
            },
        });
        for network in networks {
            let _ = self
                .client
                .request(&Request::CapabilitiesSet {
                    network,
                    capabilities: capabilities.clone(),
                })
                .await;
        }
    }

    async fn broadcast_presence(&self) {
        let (networks, profile) = {
            let st = self.state.lock();
            (st.networks.clone(), st.profile.clone())
        };
        let Some(mut profile) = profile else { return };
        // Stamp our wall clock at the moment of send — receivers read it as
        // a passive clock-skew sample (see NodeProfile::sent_at).
        profile.sent_at = unix_now_ms();
        for network in networks {
            // Claimable presence is per-network: only the claim-rendezvous
            // networks ever carry `claimable: true` (see
            // `claimable_advertised_on`) — on every other mesh this device
            // reads as a plain, unclaimable node, so it can't be discovered
            // for claiming over the public mesh unless that's deliberately
            // enabled here.
            let mut scoped = profile.clone();
            scoped.claimable = profile.claimable && self.claimable_advertised_on(&network);
            let Ok(payload) = serde_json::to_value(&scoped) else {
                continue;
            };
            let _ = self
                .client
                .request(&Request::ChannelSendAll {
                    network,
                    channel: CHANNEL_PRESENCE.to_string(),
                    payload,
                })
                .await;
        }
    }

    /// Whether `claimable: true` may be advertised on `network`: the LAN
    /// claim network always; this device's own claim-code network while
    /// public claims are enabled; and — for a legacy claimer that only
    /// shares an ordinary mesh with us — anywhere, once public claims are
    /// deliberately on. Mirrors [`Mesh::claim_network_allowed`], so we
    /// never advertise somewhere we'd then decline.
    fn claimable_advertised_on(&self, network: &str) -> bool {
        self.claim_network_allowed(network)
    }

    fn handle_base64_video_value(self: &Arc<Self>, value: Value, expected: (String, u64)) {
        let network = value.get("network").and_then(Value::as_str).unwrap_or("");
        let from = value.get("from").and_then(Value::as_str).unwrap_or("");
        let stream = value.get("stream").and_then(Value::as_u64).unwrap_or(0) as u8;
        let (expected_route, expected_generation) = &expected;
        let (current_route, current_generation) =
            self.video_route_generation_for_lane(network, from, stream);
        if !queued_video_binding_matches(
            current_route.as_deref(),
            current_generation,
            expected_route,
            *expected_generation,
        ) {
            tracing::debug!(
                route = %expected_route,
                generation = expected_generation,
                network,
                peer = %short_id(from),
                stream,
                "dropping queued base64 H.264 before allocating its decoded payload"
            );
            return;
        }
        let Some(data) = value.get("data").and_then(Value::as_str) else {
            return;
        };
        let key = value.get("key").and_then(Value::as_bool).unwrap_or(false);
        let rtp_timestamp = value
            .get("rtp_timestamp")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        if !base64_media_len_allowed(data.len()) {
            tracing::warn!(
                peer = %short_id(from),
                network,
                encoded_bytes = data.len(),
                "dropping oversized base64 video frame before decode"
            );
            return;
        }
        // Base64 fallback path (a daemon without the binary media-source
        // pipe): decode here so the handler always gets raw bytes.
        use base64::Engine as _;
        let Ok(data) = base64::engine::general_purpose::STANDARD.decode(data) else {
            return;
        };
        self.handle_video_inbound_profiled(
            network,
            from,
            stream,
            rtp_timestamp,
            key,
            data,
            0,
            Some(expected),
        );
    }

    async fn handle_value(self: &Arc<Self>, value: Value) {
        let Some(kind) = value.get("kind").and_then(|v| v.as_str()) else {
            return;
        };
        match kind {
            "channel_inbound" => {
                let channel = value.get("channel").and_then(|v| v.as_str()).unwrap_or("");
                let from = value
                    .get("from")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // The network this frame arrived on — so we learn which network
                // each peer lives on and can address replies back to it.
                let network = value
                    .get("network")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let payload = value.get("payload").cloned().unwrap_or(Value::Null);
                self.handle_channel(channel, from, network, payload).await;
            }
            "video_inbound" => {
                if let Some(event) = self.bind_base64_video_event(value) {
                    self.handle_base64_video_value(event.value, (event.route_id, event.generation));
                }
            }
            "audio_inbound" => {
                let network = value.get("network").and_then(|v| v.as_str()).unwrap_or("");
                let from = value.get("from").and_then(|v| v.as_str()).unwrap_or("");
                let Some(data) = value.get("data").and_then(|v| v.as_str()) else {
                    return;
                };
                let rtp_timestamp = value
                    .get("rtp_timestamp")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let stream = value.get("stream").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
                if self.audio_route_for_lane(network, from, stream).is_none() {
                    self.nack_dead_lane(network, from, "audio", stream);
                    return;
                }
                if !base64_media_len_allowed(data.len()) {
                    tracing::warn!(
                        peer = %short_id(from),
                        network,
                        encoded_bytes = data.len(),
                        "dropping oversized base64 audio frame before decode"
                    );
                    return;
                }
                use base64::Engine as _;
                let Ok(data) = base64::engine::general_purpose::STANDARD.decode(data) else {
                    return;
                };
                self.handle_audio_inbound(network, from, stream, rtp_timestamp, data);
            }
            "event" => {
                if let Some(event) = value.get("event") {
                    // Connection establishment is a claim-status trigger: a
                    // peer just went live for app traffic ("approved"), so
                    // re-assert presence + fleet roster straight at it —
                    // there is no heartbeat to catch it up later.
                    let approved = event.get("event_kind").and_then(|v| v.as_str()) == Some("peer")
                        && event.get("kind").and_then(|v| v.as_str()) == Some("approved");
                    if approved {
                        if let Some(device) = event.get("device_id").and_then(|v| v.as_str()) {
                            let mesh = self.clone();
                            let device = device.to_string();
                            crate::spawn(async move {
                                // Record which network this peer just went live on
                                // *before* anything is sent to it (the ownership
                                // check below included): otherwise the very first
                                // frame to a peer sharing only a secondary mesh
                                // falls back to the primary network and is dropped.
                                mesh.refresh_peer_networks().await;
                                mesh.ownership_check(Some(&device)).await;
                            });
                        }
                    }
                    // The daemon proved THIS DEVICE was evicted from a
                    // network by its signed governance (it verified the
                    // log itself — this is not a peer's claim). If that
                    // network is our FLEET mesh, the fleet is over for
                    // this device: run the same teardown the owner's
                    // cooperative Release performs, so an eviction that
                    // happened while we were offline finally cleans up
                    // instead of leaving a dead credential camping on a
                    // mesh that denies it everywhere. Any other network's
                    // eviction is daemon-side only (it already stood the
                    // engine down); nothing to tear here.
                    if event.get("event_kind").and_then(|v| v.as_str()) == Some("diag")
                        && event.get("category").and_then(|v| v.as_str()) == Some("governance")
                        && event
                            .get("detail")
                            .and_then(|d| d.get("hint"))
                            .and_then(|v| v.as_str())
                            == Some("self_evicted")
                    {
                        let evicted_net = event
                            .get("network_id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                        let fleet_net = self.ownership.fleet_network_id();
                        if evicted_net.is_some() && evicted_net == fleet_net {
                            tracing::warn!(
                                "the fleet's signed governance evicted this device — clearing fleet state"
                            );
                            let mesh = self.clone();
                            crate::spawn(async move {
                                mesh.apply_fleet_release().await;
                            });
                        }
                    }
                    // The daemon's own clock diagnostic (its heartbeat-based
                    // estimator, on daemons new enough to run one): surface
                    // it on the same UI event the presence-based estimate
                    // uses, so the front-end has one warning to render
                    // whichever detector fired first.
                    if event.get("event_kind").and_then(|v| v.as_str()) == Some("diag")
                        && event.get("category").and_then(|v| v.as_str()) == Some("clock")
                    {
                        let warn = event.get("level").and_then(|v| v.as_str()) == Some("warn");
                        let detail = event.get("detail").cloned().unwrap_or(Value::Null);
                        self.sink.emit(
                            "allmystuff://clock-skew",
                            serde_json::json!({
                                "state": if warn { "warn" } else { "clear" },
                                "skew_ms": detail.get("skew_ms").cloned().unwrap_or(Value::Null),
                                "peers": detail.get("peers").cloned().unwrap_or(Value::Null),
                                "message": event.get("message").cloned().unwrap_or(Value::Null),
                                "source": "daemon",
                            }),
                        );
                    }
                    self.sink.emit("allmystuff://event", event.clone());
                }
            }
            _ => {}
        }
    }

    async fn handle_channel(
        self: &Arc<Self>,
        channel: &str,
        from: String,
        network: String,
        payload: Value,
    ) {
        // Remember which network this peer is reachable on, so control/media
        // we send back goes to the right one (a peer may share only one of the
        // several networks we're on).
        if channel != CHANNEL_PRESENCE && !network.is_empty() && !from.is_empty() {
            self.note_peer_network_observed(&from, &network);
        }
        match channel {
            CHANNEL_PRESENCE => {
                // Never silently discard a node-information update on a parse
                // slip: a peer's presence is how we learn its name, owner,
                // sites, version and fleet, so a dropped advert is a node that
                // never appears or never refreshes. Parse once and log the
                // reason on failure — failing closed with no trace is what hid
                // this for so long. The profile is lenient about absent/older
                // fields now (they default), so a hard error here is genuinely
                // malformed input worth seeing.
                let parsed = serde_json::from_value::<NodeProfile>(payload);
                if let Err(e) = &parsed {
                    tracing::warn!("dropping presence advert from {}: {e}", short_id(&from));
                }
                if let Ok(profile) = parsed {
                    // We answer a peer's presence with our own (+ roster) when
                    // either it's the first we've heard of them this session or
                    // their app just (re)started — so the bootstrap is mutual
                    // even when our earlier advert raced their subscription and
                    // was dropped. This (plus the connection-approved trigger)
                    // is what replaced the periodic re-broadcast; the reply
                    // can't loop because once we each hold the other's presence
                    // neither condition fires again. `boot == 0` is an older
                    // heartbeating peer. Our own echo never replies to itself.
                    let canon = pubkey_part(profile.node.as_str()).to_string();
                    if canon != pubkey_part(&from) {
                        tracing::warn!(
                            from = %short_id(&from),
                            claimed = %short_id(profile.node.as_str()),
                            "dropping presence whose body node does not match its authenticated channel sender"
                        );
                        return;
                    }
                    let is_self = self
                        .local_node_id()
                        .is_some_and(|me| pubkey_part(&me) == canon);
                    let boot_disposition = if is_self {
                        PeerBootDisposition::Current
                    } else {
                        let mut state = self.state.lock();
                        let State {
                            peer_boots,
                            peer_retired_boots,
                            ..
                        } = &mut *state;
                        admit_peer_boot(peer_boots, peer_retired_boots, &canon, profile.boot)
                    };
                    if matches!(
                        boot_disposition,
                        PeerBootDisposition::Retired | PeerBootDisposition::LegacyDowngrade
                    ) {
                        tracing::warn!(
                            from = %short_id(&from),
                            boot = profile.boot,
                            disposition = ?boot_disposition,
                            "dropping stale peer presence before it can change routes, capabilities, authorization, or path preference"
                        );
                        return;
                    }
                    // Presence can steer the preferred return path only after
                    // its peer identity and boot lifetime have passed the gate.
                    if !network.is_empty() {
                        self.note_peer_network_observed(&from, &network);
                    }
                    self.state
                        .lock()
                        .peer_features
                        .insert(canon.clone(), profile.features.clone());
                    // A stamped advert is a free clock-skew sample: the
                    // sender's wall clock at send vs ours at receipt
                    // (delivery is one data-channel hop — milliseconds,
                    // noise against the 10 s threshold). Absent (`0`) on
                    // older senders; skipped, never guessed.
                    if !is_self && profile.sent_at > 0 {
                        let sample = profile.sent_at as i64 - unix_now_ms() as i64;
                        self.note_peer_clock(&canon, sample);
                    }
                    let new_boot = boot_disposition == PeerBootDisposition::Fresh;
                    // Whether this peer's presence was already on file *before*
                    // we fold in this advert. A peer we don't yet know gets an
                    // answer regardless of boot id, so a single dropped first
                    // reply self-heals on their next frame instead of waiting
                    // for a manual network refresh.
                    let node_id = profile.node.clone();
                    // What this device says about its own ownership, captured
                    // before the advert is folded in (moved): used to self-heal
                    // our fleet roster below.
                    let advertised_owner = profile
                        .owner
                        .as_ref()
                        .map(|o| pubkey_part(o.as_str()).to_string());
                    let known = {
                        let st = self.state.lock();
                        st.session
                            .as_ref()
                            .is_some_and(|s| s.peer(&node_id).is_some())
                    };
                    // A fresh boot id from a peer we already knew: every route
                    // wired to its PREVIOUS incarnation is dead on its side —
                    // but ours would keep capturing and encoding into the void
                    // (the far end logs "no route maps to it" for as long as
                    // the orphan lives, and its stale lane pin can shadow the
                    // next session's stream). Reap them now; the fresh
                    // incarnation is folded right back in below and re-offers
                    // whatever it actually wants.
                    if new_boot && known {
                        let effects = {
                            let mut st = self.state.lock();
                            st.session
                                .as_mut()
                                .map(|s| s.reap_peer_routes(&node_id))
                                .unwrap_or_default()
                        };
                        if !effects.is_empty() {
                            tracing::info!(
                                "peer {} restarted — reaping {} stale route(s) to its previous incarnation",
                                short_id(&from),
                                effects.len()
                            );
                            self.process_effects(effects).await;
                        }
                    }
                    let changed = {
                        let mut st = self.state.lock();
                        st.session
                            .as_mut()
                            .map(|s| s.apply_presence(profile))
                            .unwrap_or(false)
                    };
                    // Self-heal the fleet: if a device we still list as a fleet
                    // member now advertises a *positively different* owner, it
                    // has been re-claimed — evict it so the roster reflects
                    // reality even when the explicit leave notification never
                    // arrived (it was offline, crashed, or was claimed straight
                    // out from under us).
                    //
                    // An advert with *no* owner is not departure evidence: it's
                    // ambiguous between "went unclaimed" and a merely-defaulted
                    // field (an advert sent before the peer's ownership store
                    // loaded, an older build, a foreign bridge like the KVM's).
                    // Dropping on it authors a signed Evict tombstone that the
                    // daemon's roster convergence then mirrors onto *every*
                    // fleet device — permanently stripping the member from the
                    // rosters that authorize remote control, which surfaced as
                    // "video streams but keyboard/mouse are refused". A device
                    // that truly went unclaimed keeps advertising ownerless and
                    // claimable; evict it when it positively advertises its new
                    // owner, or deliberately from the fleet UI.
                    if !is_self && self.ownership.is_fleet_owner() {
                        let me = self.local_node_id().map(|m| pubkey_part(&m).to_string());
                        let peer = pubkey_part(node_id.as_str()).to_string();
                        let in_my_fleet = self
                            .ownership
                            .fleet_member_ids()
                            .iter()
                            .any(|d| pubkey_part(d) == peer)
                            || self.fleet_authorized.lock().contains(&peer);
                        let still_ours = advertised_owner.as_deref() == me.as_deref();
                        if in_my_fleet
                            && fleet_departure(advertised_owner.as_deref(), me.as_deref())
                        {
                            tracing::info!(
                                "fleet member {} now answers to a different owner — dropping",
                                short_id(node_id.as_str())
                            );
                            self.fleet_drop_member(node_id.to_string()).await;
                        } else if in_my_fleet && still_ours {
                            if new_boot || !known {
                                // A member that's still ours just (re)appeared. If the
                                // original fleet-key handoff was lost — we were offline
                                // when it accepted the claim, or the frame dropped — it's
                                // claimed-but-keyless and stuck outside the closed
                                // network. Re-hand the key now; the member's
                                // `adopt_fleet_key` is a no-op when it already holds it,
                                // so this is safe to repeat on every (re)appearance and
                                // self-heals the handoff without a manual nudge. Gated on
                                // the member still being in *our* roster so it never
                                // undoes an eviction (an evicted device we dropped is no
                                // longer `in_my_fleet`, so it isn't re-keyed).
                                tracing::info!(
                                    "fleet member {} (re)appeared — re-handing the fleet key in case it was missed",
                                    short_id(node_id.as_str())
                                );
                                self.send_fleet_key(node_id.as_str()).await;
                            }
                            // Signed-roster self-heal. The claim-time admit authors a
                            // new member's RoleGrant *before* that member has joined the
                            // fleet network — the key handoff (and so the join) lands
                            // after — and a constrained co-node like a KVM appliance can
                            // be absent from the closed net at that instant, so the grant
                            // never takes and nothing re-admits it until a restart. The
                            // member then sits on the fleet mesh yet missing from the
                            // signed roster the graph reads: it shows "unclaimed /
                            // unknown fleet" with its owner-gated controls (web Site,
                            // reboot, Wi-Fi, unclaim) dead, even though it holds the right
                            // key and name. So whenever a still-ours member is present but
                            // not yet in our signed roster, re-run the idempotent admit;
                            // the admit loop skips members already in the log, so this
                            // quiesces the moment the roster converges.
                            if !self.fleet_authorized.lock().contains(&peer) {
                                tracing::info!(
                                    "fleet member {} is present but unsigned — admitting it to the fleet roster",
                                    short_id(node_id.as_str())
                                );
                                self.ensure_fleet_network().await;
                                self.refresh_fleet_authorization().await;
                            }
                        }
                    }
                    if new_boot || (!is_self && !known) {
                        tracing::info!(
                            "peer {} {} — answering with our presence + roster",
                            short_id(&from),
                            if new_boot {
                                "(re)started"
                            } else {
                                "is new to us"
                            }
                        );
                        self.ownership_check(Some(&from)).await;
                        // A peer just came up while our hand is raised —
                        // beacon the help room right now instead of waiting
                        // out the next keep-alive beat. Network-scoped, so it
                        // only reaches help-room peers; harmless if the peer
                        // was on some other room.
                        if self.cec.asking_help() {
                            if let Some(me) = self.resolve_local_id().await {
                                let (help_net, _) = crate::cec::help_network_config();
                                let reached =
                                    self.cec_broadcast_presence(&help_net, &me, true).await;
                                self.sink.emit("cec://help", json!({ "watchers": reached }));
                            }
                        }
                        if new_boot {
                            // A fresh peer app boot cannot retain any route
                            // from the previous lifetime, so outstanding
                            // teardown confirmations are now proven complete.
                            self.pending_teardowns.lock().retain(|_, teardown| {
                                pubkey_part(&teardown.peer) != pubkey_part(&from)
                            });
                        }
                        let mesh = self.clone();
                        let replay_peer = from.clone();
                        crate::spawn(async move {
                            mesh.replay_desired_routes(Some(&replay_peer)).await;
                        });
                    }
                    if changed {
                        self.emit_snapshot();
                    }
                }
            }
            CHANNEL_CONTROL => {
                if let Ok(msg) = serde_json::from_value::<ControlMessage>(payload) {
                    // Claims are gated **by arrival network** before the
                    // session ever sees them (the network id is dropped at
                    // the `Effect::Ownership` boundary, so this is the only
                    // place that can enforce it): the LAN claim network is
                    // always honored, anything else only when public claims
                    // are deliberately enabled on this device. The decline
                    // names the fix so the claimer's toast is actionable.
                    if let ControlMessage::Ownership(OwnershipControl::Claim { .. }) = &msg {
                        if !self.claim_network_allowed(&network) {
                            tracing::warn!(
                                "claim from {} over {network:?} refused — claims over the \
                                 public mesh are disabled on this device",
                                short_id(&from)
                            );
                            let _ = self
                                .send_control(
                                    &from,
                                    &ControlMessage::Ownership(OwnershipControl::Declined {
                                        reason: "claims over the public mesh are disabled on \
                                                 this device — claim it from the same local \
                                                 network instead"
                                            .into(),
                                    }),
                                )
                                .await;
                            return;
                        }
                    }
                    // Terminal and files offers are screened *before* the
                    // session sees them: the session auto-accepts (Accept +
                    // StartMedia in one step), and a shell — or this disk —
                    // is owner/fleet-only, the same rule as input injection,
                    // enforced before any reply exists.
                    if let ControlMessage::Route(RouteControl::Offer {
                        route, incarnation, ..
                    }) = &msg
                    {
                        // Log every inbound offer at the point it's received, so
                        // a host's node log shows whether an offer even arrived
                        // (vs. an offerer stuck "awaiting accept" because nothing
                        // here ever processed it). The accept itself is silent
                        // otherwise; a refusal logs the warn below.
                        tracing::info!(
                            route = %route.id,
                            from = %short_id(&from),
                            "route offer received"
                        );
                        let hosts_here = self
                            .local_node_id()
                            .is_some_and(|me| node_of(route.from.as_str()) == me);
                        // Authorized for this exact plane: owner/fleet, or a
                        // share grant the owner extended for it. Non-privileged
                        // routes (`None` plane) are never refused here.
                        let authorized = route_drive_plane(route)
                            .is_none_or(|plane| self.sender_may_drive(&from, plane));
                        // CEC screen gate: a customer only lets a dialed
                        // technician view its screen while a live consent grant
                        // covers it — the screen twin of the per-plane
                        // `sender_may_drive` gate (an ordinary Display route is
                        // `plane: None`, so it wouldn't otherwise be screened
                        // here).
                        if hosts_here
                            && matches!(route.media, MediaKind::Display | MediaKind::Video)
                            && self.cec_screen_offer_denied(&from)
                        {
                            tracing::warn!(
                                "CEC screen offer {} from {} refused: no live consent grant",
                                route.id,
                                short_id(&from)
                            );
                            let _ = self
                                .send_control_on_network(
                                    &from,
                                    &ControlMessage::Route(RouteControl::Reject {
                                        route_id: route.id.clone(),
                                        incarnation: incarnation.clone(),
                                        reason: "the customer hasn't approved screen sharing \
                                                 for you (or revoked it)"
                                            .into(),
                                    }),
                                    &network,
                                )
                                .await;
                            return;
                        }
                        // Media-source gate: a Display/Video/Audio offer whose
                        // source endpoint is a capability on THIS machine makes
                        // us capture our own screen/camera/microphone and stream
                        // it to the offerer — every bit as sensitive as letting
                        // them drive us, but `route_drive_plane` never classified
                        // it, so the `authorized` computed above is
                        // unconditionally true for it. Require the same
                        // owner/fleet-or-explicit-grant authority here. (A known
                        // CEC technician without live consent was already refused
                        // just above; `sender_may_source_media` honours a
                        // technician's live ScreenView grant and person-to-person
                        // screen/camera/mic shares.)
                        if hosts_here
                            && matches!(
                                route.media,
                                MediaKind::Display | MediaKind::Video | MediaKind::Audio
                            )
                            && !self.sender_may_source_media(&from, route)
                        {
                            tracing::warn!(
                                "media source offer {} from {} refused: not owner/fleet/share",
                                route.id,
                                short_id(&from)
                            );
                            let _ = self
                                .send_control_on_network(
                                    &from,
                                    &ControlMessage::Route(RouteControl::Reject {
                                        route_id: route.id.clone(),
                                        incarnation: incarnation.clone(),
                                        reason:
                                            "not authorized: capturing this device's screen, \
                                                 camera, or microphone needs owner/fleet or a share"
                                                .into(),
                                    }),
                                    &network,
                                )
                                .await;
                            return;
                        }
                        if let Some(reason) =
                            privileged_offer_refusal(route, hosts_here, authorized)
                        {
                            tracing::warn!(
                                "privileged offer {} from {} refused: not owner/fleet/share",
                                route.id,
                                short_id(&from)
                            );
                            let _ = self
                                .send_control_on_network(
                                    &from,
                                    &ControlMessage::Route(RouteControl::Reject {
                                        route_id: route.id.clone(),
                                        incarnation: incarnation.clone(),
                                        reason,
                                    }),
                                    &network,
                                )
                                .await;
                            return;
                        }

                        // A fenced offer is only meaningful after presence has
                        // bound its boot to this peer. Presence and control use
                        // separate application channels, so the offer can
                        // legally arrive first or the one-shot presence frame
                        // can be lost. Ask for the profile on the existing data
                        // channel instead of silently dropping every retry for
                        // this boot forever. The existing per-peer backoff keeps
                        // repeated offers from turning this into a request loop.
                        if let Some(incarnation) = incarnation.as_deref() {
                            let advertised_boot = route_incarnation_boot(incarnation);
                            let learned_boot = self
                                .state
                                .lock()
                                .session
                                .as_ref()
                                .and_then(|session| session.peer(&NodeId::from(from.as_str())))
                                .map(|profile| profile.boot);
                            if advertised_boot.is_none() || learned_boot != advertised_boot {
                                tracing::warn!(
                                    route = %route.id,
                                    from = %short_id(&from),
                                    advertised_boot = ?advertised_boot,
                                    learned_boot = ?learned_boot,
                                    disposition = "presence_required",
                                    "fenced route offer arrived before matching peer presence"
                                );
                                if advertised_boot.is_some() && self.allow_profile_request(&from) {
                                    let _ = self
                                        .send_control_on_network(
                                            &from,
                                            &ControlMessage::ProfileRequest,
                                            &network,
                                        )
                                        .await;
                                }
                                return;
                            }
                        }
                    }
                    // Only a periodic viewer report produced after the close's
                    // minimum age proves the replacement survived. One-shot
                    // Offer/Accept/Tune/Lane controls can already be in flight
                    // and are deliberately not treated as liveness.
                    if let Some(route_id) = inbound_video_feedback_liveness_route_id(&msg) {
                        if let Some(token) = self.cancel_pending_video_teardown(route_id, &from) {
                            tracing::warn!(
                                route = %route_id,
                                from = %short_id(&from),
                                network = %network,
                                token,
                                disposition = "quarantine_canceled_by_liveness",
                                "inbound video route control"
                            );
                        }
                    }

                    if let ControlMessage::Route(RouteControl::TeardownAck {
                        route_id,
                        incarnation,
                    }) = &msg
                    {
                        let key = (route_id.clone(), incarnation.clone());
                        let mut pending = self.pending_teardowns.lock();
                        let matches = pending.get(&key).is_some_and(|teardown| {
                            pubkey_part(&teardown.peer) == pubkey_part(&from)
                                && teardown
                                    .network
                                    .as_deref()
                                    .is_none_or(|expected| expected == network)
                        });
                        if matches {
                            pending.remove(&key);
                            tracing::debug!(
                                route = %route_id,
                                from = %short_id(&from),
                                "peer app acknowledged route teardown"
                            );
                        } else {
                            tracing::warn!(
                                route = %route_id,
                                from = %short_id(&from),
                                disposition = "unexpected_teardown_ack_ignored",
                                "route teardown acknowledgement did not match a pending exact lifetime"
                            );
                        }
                        return;
                    }

                    // MissingRoute/DeadLane recovery deliberately challenges a
                    // sender with the exact Accept for the lifetime it still
                    // considers active. If this side no longer has that exact
                    // outbound offer, answer with an exact terminal response.
                    // A delayed Accept for predecessor A therefore cannot touch
                    // same-id successor B, while the orphaned A encoder still
                    // converges to StopMedia on its owner.
                    if let ControlMessage::Route(RouteControl::Accept {
                        route_id,
                        incarnation: Some(incarnation),
                        ..
                    }) = &msg
                    {
                        let terminal = {
                            let state = self.state.lock();
                            let route = state
                                .session
                                .as_ref()
                                .and_then(|session| session.route(route_id));
                            exact_accept_terminal_response(route_id, &from, incarnation, route)
                        };
                        if let Some(terminal) = terminal {
                            tracing::warn!(
                                route = %route_id,
                                from = %short_id(&from),
                                incarnation = %incarnation,
                                disposition = "exact_accept_reconciled_terminal",
                                "received an Accept for a route lifetime that is not live here"
                            );
                            let _ = self
                                .send_control_on_network(&from, &terminal, &network)
                                .await;
                            return;
                        }
                    }

                    #[allow(clippy::collapsible_if)]
                    if !self.inbound_route_control_path_ok(&from, &msg, &network) {
                        if route_control_network_key(&msg).is_some() {
                            tracing::warn!(
                                from = %short_id(&from),
                                network = %network,
                                disposition = "route_path_mismatch_ignored",
                                "inbound route control did not arrive on its exact data-plane path"
                            );
                            return;
                        }
                    }

                    // Teardown used to be the one destructive route control
                    // that was not peer-checked. Authentication, guard choice,
                    // and (when committing) Session mutation are one state-lock
                    // transaction, so a same-id replacement cannot enter in
                    // between and be killed by an old peer message.
                    if let ControlMessage::Route(RouteControl::Teardown {
                        route_id,
                        incarnation: route_incarnation,
                    }) = &msg
                    {
                        let lifecycle = self.lock_route_lifecycle(route_id).await;
                        let (facts, gate, effects) = {
                            let mut st = self.state.lock();
                            let Some(session) = st.session.as_mut() else {
                                return;
                            };
                            let facts = session.route(route_id).map(|r| {
                                (r.peer.as_str().to_string(), r.state.clone(), r.route.media)
                            });
                            if facts
                                .as_ref()
                                .is_some_and(|(peer, _, _)| pubkey_part(peer) != pubkey_part(&from))
                            {
                                tracing::warn!(
                                    route = %route_id,
                                    from = %short_id(&from),
                                    network = %network,
                                    expected = %facts.as_ref().map(|f| short_id(&f.0)).unwrap_or_default(),
                                    disposition = "foreign_peer_refused",
                                    "inbound route teardown"
                                );
                                return;
                            }
                            let eligible = session.route(route_id).is_some_and(|route| {
                                route.state == RouteState::Active
                                    && matches!(
                                        route.route.media,
                                        MediaKind::Display | MediaKind::Video
                                    )
                                    && route.incarnation.is_none()
                                    && route_incarnation.is_none()
                            });
                            let gate = if eligible {
                                self.video_switch_guards.lock().gate_inbound_teardown(
                                    route_id,
                                    &from,
                                    Instant::now(),
                                )
                            } else {
                                InboundVideoTeardownGate::Commit
                            };
                            let effects = if matches!(gate, InboundVideoTeardownGate::Commit) {
                                let effects = session.handle(
                                    NodeId::from(from.as_str()),
                                    ControlMessage::Route(RouteControl::Teardown {
                                        route_id: route_id.clone(),
                                        incarnation: route_incarnation.clone(),
                                    }),
                                );
                                st.route_networks
                                    .remove(&(route_id.clone(), route_incarnation.clone()));
                                effects
                            } else {
                                Vec::new()
                            };
                            (facts, gate, effects)
                        };
                        let generation = self.video_route_generations.lock().current(route_id);
                        let state_before = facts.as_ref().map(|(_, state, _)| state);
                        let media = facts.as_ref().map(|(_, _, media)| media);
                        match gate {
                            InboundVideoTeardownGate::Quarantine {
                                predecessor,
                                age,
                                token,
                                incarnation,
                            } => {
                                tracing::warn!(
                                    route = %route_id,
                                    from = %short_id(&from),
                                    network = %network,
                                    state_before = ?state_before,
                                    media = ?media,
                                    generation = ?generation,
                                    predecessor = %predecessor,
                                    age_us = age.as_micros(),
                                    token,
                                    incarnation,
                                    quarantine_ms = VIDEO_INBOUND_TEARDOWN_QUARANTINE.as_millis(),
                                    disposition = "quarantined",
                                    "inbound route teardown"
                                );
                                let mesh = self.clone();
                                let route_id = route_id.clone();
                                let route_incarnation = route_incarnation.clone();
                                crate::spawn(async move {
                                    mesh.commit_quarantined_video_teardown(
                                        route_id,
                                        from,
                                        network,
                                        token,
                                        incarnation,
                                        route_incarnation,
                                    )
                                    .await;
                                });
                                return;
                            }
                            InboundVideoTeardownGate::CoalesceDuplicate { token } => {
                                tracing::warn!(
                                    route = %route_id,
                                    from = %short_id(&from),
                                    network = %network,
                                    state_before = ?state_before,
                                    media = ?media,
                                    generation = ?generation,
                                    token,
                                    disposition = "duplicate_coalesced",
                                    "inbound route teardown"
                                );
                                return;
                            }
                            InboundVideoTeardownGate::Commit => {
                                tracing::info!(
                                    route = %route_id,
                                    from = %short_id(&from),
                                    network = %network,
                                    state_before = ?state_before,
                                    media = ?media,
                                    generation = ?generation,
                                    disposition = "commit",
                                    "inbound route teardown"
                                );
                                self.remove_desired_route_exact(
                                    &from,
                                    route_id,
                                    route_incarnation.as_deref(),
                                );
                                let mut deferred = Vec::new();
                                for effect in effects {
                                    match effect {
                                        Effect::StopMedia {
                                            route_id,
                                            incarnation,
                                        } => self.apply_stop_media_locked(route_id, incarnation),
                                        other => deferred.push(other),
                                    }
                                }
                                drop(lifecycle);
                                self.process_effects(deferred).await;
                                if self.peer_supports_teardown_ack(&from) {
                                    let _ = self
                                        .send_control_on_network(
                                            &from,
                                            &ControlMessage::Route(RouteControl::TeardownAck {
                                                route_id: route_id.clone(),
                                                incarnation: route_incarnation.clone(),
                                            }),
                                            &network,
                                        )
                                        .await;
                                }
                                self.emit_snapshot();
                                return;
                            }
                        }
                    } else if let ControlMessage::Route(RouteControl::Reject {
                        route_id,
                        incarnation,
                        reason,
                    }) = &msg
                    {
                        tracing::info!(
                            "inbound route reject for {route_id} from {}: {reason}",
                            short_id(&from)
                        );
                        self.remove_desired_route_exact(&from, route_id, incarnation.as_deref());
                    } else if let ControlMessage::Route(RouteControl::Accept {
                        route_id,
                        incarnation,
                        session,
                    }) = &msg
                    {
                        self.update_desired_terminal_session(
                            &from,
                            route_id,
                            incarnation.as_deref(),
                            session.as_deref(),
                        );
                    }
                    // Site management (list a co-owned machine's sites,
                    // re-expose them) and the terminal-sessions picker plane
                    // (list this host's open shells, the host's answer) ride
                    // this channel but are the backend's to handle, gated
                    // owner/fleet — the session never sees them.
                    match msg {
                        ControlMessage::Site(sc) => {
                            self.handle_site_control(&from, sc).await;
                        }
                        ControlMessage::Route(RouteControl::TerminalSessionsRequest) => {
                            self.handle_terminal_sessions_request(&from).await;
                        }
                        ControlMessage::Route(RouteControl::TerminalSessions { sessions }) => {
                            // A host's answer to *our* picker request — surface
                            // it to the front-end (it picks one to attach to).
                            self.sink.emit(
                                "allmystuff://terminal-sessions",
                                json!({ "from": from, "sessions": sessions }),
                            );
                        }
                        ControlMessage::Route(RouteControl::VideoLane {
                            route_id,
                            incarnation,
                            lane,
                        }) => {
                            // The streamer told us which track lane this route's
                            // H.264 rides — record it so inbound samples demux to
                            // the right console window by binding, not by guess.
                            self.record_video_lane(&network, &from, &route_id, incarnation, lane);
                        }
                        ControlMessage::Route(RouteControl::DeadLane { media, lane }) => {
                            // A receiver says our media on that lane has no
                            // route on its side (it restarted and lost the
                            // name). Resolve the lane back to the route we
                            // pinned it to and fold it through the session as
                            // that route's Reject — stopping the encoder.
                            self.handle_dead_lane(&network, &from, &media, lane).await;
                        }
                        ControlMessage::Route(RouteControl::MissingRoute {
                            route_id,
                            incarnation,
                        }) => {
                            self.handle_missing_route(
                                &network,
                                &from,
                                &route_id,
                                incarnation.as_deref(),
                            )
                            .await;
                        }
                        ControlMessage::ProfileRequest => {
                            // A peer's refresh asks us to re-announce — send our
                            // current presence straight back so it re-learns us
                            // on the spot. The asker spaces these under its own
                            // backoff envelope, so we just answer.
                            tracing::debug!(
                                "presence re-announce requested by {}",
                                short_id(&from)
                            );
                            self.send_presence_to(&from).await;
                        }
                        msg => {
                            let route_path_message = msg.clone();
                            let terminal_route_key = match &route_path_message {
                                ControlMessage::Route(RouteControl::Reject {
                                    route_id,
                                    incarnation,
                                    ..
                                }) => Some((route_id.clone(), incarnation.clone())),
                                _ => None,
                            };
                            let accepted_route = match &msg {
                                ControlMessage::Route(RouteControl::Accept {
                                    route_id, ..
                                }) => Some(route_id.clone()),
                                _ => None,
                            };
                            // A Reject landing on one of our client-side site
                            // mappings is the host saying its route is gone (a
                            // reconnect / network change tore it down). Grab the
                            // mapping now — the session's StopMedia is about to
                            // remove it — so we can auto-re-map it on the SAME
                            // local port and heal the tunnel with no unmap/remap.
                            // (A user-initiated unmap goes through disconnect(),
                            // never an inbound Reject, so this never fights a
                            // deliberate teardown.)
                            let heal_site = match &msg {
                                ControlMessage::Route(RouteControl::Reject {
                                    route_id, ..
                                }) => self
                                    .sites
                                    .mapping_details(route_id)
                                    .map(|d| (route_id.clone(), d)),
                                _ => None,
                            };
                            let effects = {
                                let mut st = self.state.lock();
                                let effects = st
                                    .session
                                    .as_mut()
                                    .map(|s| s.handle(NodeId::from(from.as_str()), msg))
                                    .unwrap_or_default();
                                Self::commit_inbound_route_network_locked(
                                    &mut st,
                                    &from,
                                    &route_path_message,
                                    &network,
                                );
                                if let Some(key) = terminal_route_key.as_ref() {
                                    st.route_networks.remove(key);
                                }
                                effects
                            };
                            self.process_effects(effects).await;
                            if let Some(route_id) = accepted_route {
                                self.replay_requested_video_tune(&route_id).await;
                            }
                            if let Some((old_route, (node, host_port, local_port))) = heal_site {
                                // Guarantee the dead route is fully cleared — a
                                // reject on a not-yet-active offer emits no
                                // StopMedia, so its mapping/listener would
                                // otherwise linger and block the re-map — then
                                // heal on the same local port off the hot path.
                                self.sites.stop_route(&old_route);
                                {
                                    let mut st = self.state.lock();
                                    if let Some(s) = st.session.as_mut() {
                                        let _ = s.teardown(&old_route);
                                    }
                                }
                                let mesh = self.clone();
                                crate::spawn(async move {
                                    mesh.remap_site_route(node, host_port, local_port).await;
                                });
                            }
                            self.emit_snapshot();
                        }
                    }
                }
            }
            CHANNEL_MEDIA => {
                let Some(media) = MediaPayload::decode(payload) else {
                    return;
                };
                match media {
                    MediaPayload::Audio(frame) => {
                        if self.inbound_media_ok(&frame.route, &from, MediaKind::Audio)
                            && self.inbound_route_network_ok(&frame.route, &from, &network)
                        {
                            self.audio.feed(&frame.route, &frame);
                        }
                    }
                    MediaPayload::Video(frame) => {
                        // Surface frames only for a route this session knows
                        // is live, sinks here, and belongs to the sender —
                        // the watching window (console stage, room tile)
                        // renders them. Display and camera routes share the
                        // frame shape. Chunked frames reassemble first; the
                        // first complete frame of a stream is logged so
                        // "connected but no pixels" is attributable from
                        // this side too.
                        match self.inbound_video_disposition(&frame.route, &from) {
                            InboundVideoDisposition::Accept => {}
                            InboundVideoDisposition::Pending => {
                                tracing::debug!(
                                    "early video frame for {} from {} dropped while route activation is pending",
                                    frame.route,
                                    short_id(&from)
                                );
                                return;
                            }
                            InboundVideoDisposition::Reject => {
                                tracing::debug!(
                                    "dropped video frame for {} from {} (route not live here)",
                                    frame.route,
                                    short_id(&from)
                                );
                                self.nack_dead_route(&network, &from, &frame.route);
                                return;
                            }
                        }
                        if !self.inbound_route_network_ok(&frame.route, &from, &network) {
                            tracing::debug!(
                                route = %frame.route,
                                network = %network,
                                "dropped MJPEG frame from a network that does not own this route lifetime"
                            );
                            return;
                        }
                        if !self.inbound_video_incarnation_ok(
                            &frame.route,
                            &from,
                            frame.incarnation.as_deref(),
                        ) {
                            tracing::debug!(
                                route = %frame.route,
                                network = %network,
                                incarnation = ?frame.incarnation,
                                "dropped MJPEG chunk from a predecessor route lifetime"
                            );
                            return;
                        }
                        let full = { self.video_in.lock().push(frame) };
                        if let Some(full) = full {
                            if full.seq == 0 {
                                tracing::info!(
                                    "first video frame for {} ({}×{})",
                                    full.route,
                                    full.width,
                                    full.height
                                );
                            }
                            self.note_video_in(&full.route, "MJPEG", full.jpeg.len());
                            // latest_wins: every JPEG is a complete picture, so
                            // an unread backlog is pure latency — supersede it.
                            // Without this the viewer replays history frame by
                            // frame ("always catching up") whenever decode or
                            // the wire runs behind the capture rate.
                            let profile_id = crate::pipeline_profile::next_frame_id();
                            let _ = self.enqueue_for_watcher(
                                &full.route,
                                video_ipc_bytes(&full),
                                true,
                                profile_id,
                                None,
                            );
                        }
                    }
                    MediaPayload::VideoStatus(status) => {
                        // The host explaining its capture state ("display
                        // asleep", "camera failed"…). Gated like the frames
                        // it stands in for; the console window shows it on
                        // the stage.
                        if !self.inbound_video_ok(&status.route, &from)
                            || !self.inbound_route_network_ok(&status.route, &from, &network)
                        {
                            return;
                        }
                        tracing::info!(
                            "capture status for {}: {:?}{}",
                            status.route,
                            status.state,
                            status
                                .detail
                                .as_deref()
                                .map(|d| format!(" ({d})"))
                                .unwrap_or_default()
                        );
                        self.sink.emit(
                            "allmystuff://video-status",
                            serde_json::json!({
                                "route": status.route,
                                "state": status.state,
                                "detail": status.detail,
                            }),
                        );
                    }
                    MediaPayload::Input(ev) => {
                        // Injecting keystrokes is the most privileged thing
                        // on the mesh, so it takes two gates: a live input
                        // route from this exact sender, *and* the sender being
                        // authorized to drive this machine's control plane —
                        // its recorded owner, a co-owned fleet member, or a
                        // person the owner deliberately granted control to (the
                        // share path; without it a shared "Control" route
                        // activates but every event is dropped here). A CEC
                        // technician's authority is their customer's consent
                        // grant, which is evaluated at route *admission* and by
                        // the ~2s consent sweep — never per frame (see
                        // `sender_may_drive_admitted` / `spawn_cec_consent_sweep`):
                        // a lapsed grant tears the route down within a couple of
                        // seconds, so here a live CEC route just passes.
                        // Capture the injector generation before checking the
                        // route. If teardown runs at any later point, its
                        // release invalidates this lease and the queued event
                        // cannot re-press input after cleanup.
                        let input_lease = self.injector.lease(&ev.route);
                        let route_ok = self.inbound_media_ok_incarnation(
                            &ev.route,
                            &from,
                            MediaKind::Input,
                            ev.incarnation.as_deref(),
                        ) && self
                            .inbound_route_network_ok(&ev.route, &from, &network);
                        if route_ok && self.sender_may_drive_admitted(&from, DrivePlane::Input) {
                            if !accept_input_sequence(
                                &mut self.input_in_seq.lock(),
                                &ev.route,
                                &ev.incarnation,
                                ev.seq,
                            ) {
                                tracing::debug!(
                                    route = %ev.route,
                                    incarnation = ?ev.incarnation,
                                    seq = ev.seq,
                                    "dropped duplicate or reordered input event"
                                );
                            } else if let Some(lease) = input_lease {
                                self.injector.apply(&ev.route, ev.action, lease);
                            } else if self.diag_ok(&format!("input-lease:{}", ev.route)) {
                                tracing::warn!(
                                    "dropped input for active route {} because its local lifetime was not registered",
                                    ev.route
                                );
                            }
                        } else {
                            // Refusing silently is how "controls just stopped
                            // working" went undiagnosable — say which gate
                            // failed, tell the viewer, and tell our own UI.
                            self.refuse_control_frame(&from, &ev.route, "input", route_ok);
                        }
                    }
                    MediaPayload::Terminal(frame) => {
                        if self.inbound_route_network_ok(&frame.route, &from, &network) {
                            self.handle_term_frame(&from, frame);
                        }
                    }
                    MediaPayload::File(frame) => {
                        if self.inbound_route_network_ok(&frame.route, &from, &network) {
                            self.handle_file_frame(&from, frame);
                        }
                    }
                    MediaPayload::Clipboard(frame) => {
                        if self.inbound_route_network_ok(&frame.route, &from, &network) {
                            self.handle_clipboard_frame(&from, frame);
                        }
                    }
                    MediaPayload::Site(frame) => {
                        if self.inbound_route_network_ok(&frame.route, &from, &network) {
                            self.handle_site_frame(&from, frame);
                        }
                    }
                }
            }
            CHANNEL_ROOMS => {
                // The rooms plane is deliberately thin backend-side: rooms
                // live in the GUI (like relationships do), so a decoded
                // message is simply forwarded to every window. Decoding
                // here rather than passing raw JSON keeps the same skew
                // discipline as every other channel — a message this build
                // doesn't understand is dropped, never an error.
                if let Ok(msg) = serde_json::from_value::<RoomMessage>(payload) {
                    self.sink
                        .emit("allmystuff://room", json!({ "from": from, "message": msg }));
                }
            }
            // CEC Support's own control channel (`cec.control`) — the
            // connect/approve/deny/end handshake. Distinct from AllMyStuff's
            // `CHANNEL_CONTROL` so CEC traffic never crosses into an ordinary
            // route negotiation.
            other if other == allmystuff_cec_protocol::CHANNEL_CONTROL => {
                tracing::info!("cec control in from {} on {network}", short_id(&from));
                self.handle_cec_control(from, network, payload).await;
            }
            // CEC presence beacons. The only ones this node *acts* on are the
            // global help room's: a Client beacon there IS the "I need help"
            // signal (`available: false` withdraws it). Presence on a number
            // room stays informational — dial discovery rides the daemon peer
            // list, not this channel.
            other
                if other == allmystuff_cec_protocol::CHANNEL_PRESENCE
                    && network == allmystuff_cec_protocol::HELP_NETWORK_ID =>
            {
                let Ok(p) =
                    serde_json::from_value::<allmystuff_cec_protocol::SupportPresence>(payload)
                else {
                    return;
                };
                // The dialable number derives from the *authenticated* sender
                // id — never the payload — so a beacon can't park someone
                // else's number in the queue.
                let number = allmystuff_cec_protocol::support_id_from_device(&from);
                let changed = if p.available
                    && matches!(p.role, allmystuff_cec_protocol::Role::Client)
                {
                    tracing::info!("cec help beacon from {} (number {number})", short_id(&from));
                    self.cec
                        .record_help_beacon(&from, &number, &p.label, &p.hostname)
                } else {
                    self.cec.remove_help_beacon(&from)
                };
                if changed {
                    self.sink
                        .emit("cec://help", json!({ "waiting": self.cec.help_list() }));
                }
            }
            _ => {}
        }
    }

    /// Drop the per-route video state a route that just ended leaves behind —
    /// its receive-side counters, any pending re-key ask, its native decoder,
    /// the host-side pinned track lane (freeing it for the next stream), and
    /// the viewer-side lane→route binding.
    fn release_video_lanes(self: &Arc<Self>, route_id: &str) {
        self.note_video_route_stopped(route_id);
        // Retire and flush under the same fence used by final AU admission.
        // If an old admission wins first, this waits and then clears it. If
        // teardown wins first, the admission observes no current generation.
        let mut generations = self.video_route_generations.lock();
        generations.retire(route_id);
        self.reset_video_receive_generation_locked(route_id, &generations);
        // Host side: free the local pin so a later stream can reuse it. Keep
        // the daemon's fixed video track alive until its peer connection ends.
        // Closing it asynchronously here has an ABA race: a same-id re-offer
        // can reclaim lane N before the old MediaLaneClose arrives, and that
        // late close then destroys the replacement stream. Reusing the idle
        // fixed lane is both cheaper and generation-safe.
        self.video_lane_pins.lock().remove(route_id);
        // Viewer side: drop any lane binding that pointed at this route.
        let mut binds = self.video_lane_binds.lock();
        for per_peer in binds.values_mut() {
            per_peer.retain(|_, binding| binding.route_id != route_id);
        }
        binds.retain(|_, per_peer| !per_peer.is_empty());
        drop(binds);
        drop(generations);
    }

    /// Record both ends of a video route's local lifecycle. These timestamps
    /// never leave this process; they only recognize the tiny old-screen →
    /// new-screen handoff in which a duplicate close can otherwise kill the
    /// successor before its first capture frame.
    fn note_video_route_started(&self, route: &Route) {
        if !matches!(route.media, MediaKind::Display | MediaKind::Video) {
            return;
        }
        let Some(peer) = self.route_peer(&route.id) else {
            return;
        };
        self.video_switch_guards.lock().note_start(
            &route.id,
            &peer,
            route.to.as_str(),
            Instant::now(),
        );
    }

    fn note_video_route_stopped(&self, route_id: &str) {
        let facts = {
            let st = self.state.lock();
            st.session
                .as_ref()
                .and_then(|s| s.route(route_id))
                .filter(|r| matches!(r.route.media, MediaKind::Display | MediaKind::Video))
                .map(|r| (r.peer.as_str().to_string(), r.route.to.as_str().to_string()))
        };
        let Some((peer, sink)) = facts else { return };
        self.video_switch_guards
            .lock()
            .note_stop(route_id, &peer, &sink, Instant::now());
    }

    /// Read the narrow switch guard for an authenticated, currently-live
    /// display route. Call this before mutating the session to TornDown: after
    /// that mutation the replacement is indistinguishable from its predecessor
    /// by route id alone.
    fn take_early_video_teardown_guard(&self, route_id: &str) -> Option<VideoSwitchGuardHit> {
        let peer = {
            let st = self.state.lock();
            let route = st.session.as_ref()?.route(route_id)?;
            if !matches!(route.route.media, MediaKind::Display | MediaKind::Video)
                || route.state != RouteState::Active
            {
                return None;
            }
            route.peer.as_str().to_string()
        };
        self.video_switch_guards
            .lock()
            .take_early_teardown(route_id, &peer, Instant::now())
    }

    /// Cancel a quarantined close only when the same authenticated peer emits a
    /// periodic report for the active route after the in-flight-control floor.
    /// Feedback is an existing app control carried on the ICE data path; no new
    /// wire or signaling message is introduced.
    fn cancel_pending_video_teardown(&self, route_id: &str, from: &str) -> Option<u64> {
        let st = self.state.lock();
        let live = st
            .session
            .as_ref()
            .and_then(|s| s.route(route_id))
            .is_some_and(|route| {
                route.state == RouteState::Active
                    && matches!(route.route.media, MediaKind::Display | MediaKind::Video)
                    && pubkey_part(route.peer.as_str()) == pubkey_part(from)
            });
        live.then(|| {
            self.video_switch_guards
                .lock()
                .cancel_pending_on_mature_liveness(route_id, Instant::now())
        })
        .flatten()
    }

    /// A new local offer is an explicit replacement action, not a duplicate
    /// control delivery. Cancel its predecessor's delayed close before the
    /// local Session overwrites the deterministic route id.
    fn cancel_pending_video_teardown_replaced(&self, route_id: &str, from: &str) -> Option<u64> {
        let st = self.state.lock();
        let same_peer = st
            .session
            .as_ref()
            .and_then(|s| s.route(route_id))
            .is_some_and(|route| pubkey_part(route.peer.as_str()) == pubkey_part(from));
        same_peer
            .then(|| self.video_switch_guards.lock().cancel_pending(route_id))
            .flatten()
    }

    /// Apply a quarantined peer close after its grace period. This folds the
    /// original message through the session exactly once but deliberately does
    /// not call `disconnect`: echoing a new Teardown would turn a receive-side
    /// lifecycle decision into another wire message.
    async fn commit_quarantined_video_teardown(
        self: &Arc<Self>,
        route_id: String,
        from: String,
        network: String,
        token: u64,
        guard_incarnation: u64,
        route_incarnation: Option<String>,
    ) {
        tokio::time::sleep(VIDEO_INBOUND_TEARDOWN_QUARANTINE).await;
        let lifecycle = self.lock_route_lifecycle(&route_id).await;
        let (effects, state_before, media) = {
            let mut st = self.state.lock();
            let Some(session) = st.session.as_mut() else {
                self.video_switch_guards.lock().take_pending_if_current(
                    &route_id,
                    token,
                    guard_incarnation,
                );
                return;
            };
            let Some(route) = session.route(&route_id) else {
                self.video_switch_guards.lock().take_pending_if_current(
                    &route_id,
                    token,
                    guard_incarnation,
                );
                return;
            };
            if route.state != RouteState::Active
                || !matches!(route.route.media, MediaKind::Display | MediaKind::Video)
                || pubkey_part(route.peer.as_str()) != pubkey_part(&from)
                || route.incarnation != route_incarnation
            {
                self.video_switch_guards.lock().take_pending_if_current(
                    &route_id,
                    token,
                    guard_incarnation,
                );
                return;
            }
            if !self.video_switch_guards.lock().take_pending_if_current(
                &route_id,
                token,
                guard_incarnation,
            ) {
                return;
            }
            let state_before = route.state.clone();
            let media = route.route.media;
            let effects = session.handle(
                NodeId::from(from.as_str()),
                ControlMessage::Route(RouteControl::Teardown {
                    route_id: route_id.clone(),
                    incarnation: route_incarnation.clone(),
                }),
            );
            st.route_networks
                .remove(&(route_id.clone(), route_incarnation.clone()));
            (effects, state_before, media)
        };
        let generation = self.video_route_generations.lock().current(&route_id);
        tracing::warn!(
            route = %route_id,
            from = %short_id(&from),
            network = %network,
            state_before = ?state_before,
            media = ?media,
            generation = ?generation,
            token,
            guard_incarnation,
            disposition = "quarantine_expired_commit",
            "inbound route teardown"
        );
        self.remove_desired_route_exact(&from, &route_id, route_incarnation.as_deref());
        let mut deferred = Vec::new();
        for effect in effects {
            match effect {
                Effect::StopMedia {
                    route_id,
                    incarnation,
                } => self.apply_stop_media_locked(route_id, incarnation),
                other => deferred.push(other),
            }
        }
        drop(lifecycle);
        self.process_effects(deferred).await;
        if self.peer_supports_teardown_ack(&from) {
            let _ = self
                .send_control_on_network(
                    &from,
                    &ControlMessage::Route(RouteControl::TeardownAck {
                        route_id: route_id.clone(),
                        incarnation: route_incarnation,
                    }),
                    &network,
                )
                .await;
        }
        self.emit_snapshot();
    }

    /// Allocate this actual `StartMedia` effect's process-local incarnation.
    /// The session state machine suppresses duplicate starts under its lock;
    /// therefore every effect reaching this boundary is a real start and must
    /// supersede any same-id predecessor even when a stale stop was correctly
    /// ignored before it could retire the old generation.
    fn begin_video_generation(&self, route_id: &str) -> u64 {
        let mut generations = self.video_route_generations.lock();
        let (generation, replaced) = generations.begin(route_id);
        self.reset_video_receive_generation_locked(route_id, &generations);
        if let Some(old) = replaced {
            tracing::warn!(
                "video route generation {old} replaced by {generation} for same-id successor {route_id}"
            );
        } else {
            tracing::info!("video route generation {generation} started for {route_id}");
        }
        generation
    }

    /// Clear every receive-side object that can retain an access unit or
    /// decoded picture from a preceding same-id route. The generation guard
    /// is an explicit parameter so callers cannot accidentally create a
    /// check-then-clear window.
    fn reset_video_receive_generation_locked(
        &self,
        route_id: &str,
        _generations: &parking_lot::MutexGuard<'_, VideoRouteGenerations>,
    ) {
        self.video_in.lock().clear_route(route_id);
        self.video_arrivals.lock().remove(route_id);
        self.video_in_stats.lock().remove(route_id);
        self.refresh_asks.lock().remove(route_id);
        self.video_decode.stop(route_id);
        self.video_watchers
            .lock()
            .reset_route_for_reconnect(route_id);
    }

    fn video_generation_is_current(&self, route_id: &str, generation: u64) -> bool {
        self.video_route_generations
            .lock()
            .is_current(route_id, generation)
    }

    fn apply_video_policy_caps_locked(
        &self,
        plans: &[EffectivePlan],
        _serial: &parking_lot::MutexGuard<'_, ()>,
    ) {
        for plan in plans {
            self.video.apply_policy_cap(
                &plan.route_id,
                Some(plan.route_budget_bps.min(u64::from(u32::MAX)) as u32),
                plan.auto_resolution,
            );
        }
    }

    /// Stop legacy PCM captures as soon as the same peer gains a governed
    /// video plan. Raw PCM is retained only for uncapped legacy/audio-only
    /// sessions; it is far larger than every encoded-audio reservation and
    /// would make the effective aggregate cap dishonest.
    fn stop_policy_pcm_for_peer(
        self: &Arc<Self>,
        peer: &str,
        _serial: &parking_lot::MutexGuard<'_, ()>,
    ) {
        let peer = pubkey_part(peer);
        let routes = {
            let mut active = self.pcm_audio_routes.lock();
            let routes = active
                .iter()
                .filter(|(_, route_peer)| pubkey_part(route_peer) == peer)
                .map(|(route_id, _)| route_id.clone())
                .collect::<Vec<_>>();
            for route_id in &routes {
                active.remove(route_id);
            }
            routes
        };
        for route_id in routes {
            tracing::warn!(
                "audio unavailable for {route_id}: peer now has an enforced media aggregate; \
                 legacy PCM stopped because it cannot fit the encoded-audio reservation"
            );
            self.audio.stop(&route_id);
            // The route itself is still Active. Remove only its aggregate
            // reservation. The pre-negotiated daemon lane stays idle until its
            // PeerSession ends.
            self.audio_encoders.lock().remove(&route_id);
            self.media_policy.lock().remove_audio_route(&route_id);
        }
    }

    /// The audio twin of [`Self::release_video_lanes`]: drop route-local codec
    /// and policy state. Pre-negotiated lane 0 stays idle until its existing
    /// PeerSession ends. Closing it by peer-wide preference can target another
    /// network's independent lane 0 and may change the peer-connection tracks.
    fn release_audio_lanes(self: &Arc<Self>, route_id: &str) {
        self.pcm_audio_routes.lock().remove(route_id);
        self.audio_decoders.lock().remove(route_id);
        self.audio_encoders.lock().remove(route_id);
        let policy_plans = {
            let serial = self.video_policy_apply_serial.lock();
            let plans = self.media_policy.lock().remove_audio_route(route_id);
            self.apply_video_policy_caps_locked(&plans, &serial);
            plans
        };
        if !policy_plans.is_empty() {
            let mesh = self.clone();
            crate::spawn(async move { mesh.send_effective_plans(policy_plans).await });
        }
    }

    /// One Opus frame arrived on a peer's audio lane `stream`. It belongs
    /// to whichever of our routes maps to that lane (the lane-th Opus route
    /// from this peer in sorted order — [`Self::audio_route_for_lane`]),
    /// gated exactly like every other media frame (route live, sinks here,
    /// sender is the route's peer) — then decodes straight into the
    /// route's playback ring.
    fn handle_audio_inbound(
        self: &Arc<Self>,
        network: &str,
        from: &str,
        stream: u8,
        rtp_timestamp: u32,
        data: Vec<u8>,
    ) {
        let Some(route_id) = self.audio_route_for_lane(network, from, stream) else {
            // The audio twin of the video lane's "no route for it" warn
            // (rate-limited the same way): Opus arriving with nowhere to
            // decode it is the caller-hears-nothing drop, and it used to be
            // a DEBUG whisper while the room sat silent.
            if self.diag_ok(&format!(
                "audio-lane:{network}:{}:{stream}",
                pubkey_part(from)
            )) {
                tracing::warn!(
                    "Opus frames arriving from {} on lane {stream} but no route maps to it — dropped (caller hears nothing)",
                    short_id(from)
                );
            }
            self.nack_dead_lane(network, from, "audio", stream);
            return;
        };
        self.clear_dead_lane(network, from, "audio", stream);
        if !self.inbound_media_ok(&route_id, from, MediaKind::Audio)
            || !self.inbound_route_network_ok(&route_id, from, network)
        {
            tracing::debug!("audio frame for {route_id} refused (route not live here)");
            self.nack_dead_route(network, from, &route_id);
            return;
        }
        let profile = audio_profile_for_mode(self.media_mode_for_peer(from));
        let decoded = {
            let mut decoders = self.audio_decoders.lock();
            let dec = match decoders.entry(route_id.clone()) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(v) => match OpusReceiver::new(profile) {
                    Ok(d) => v.insert(d),
                    Err(e) => {
                        tracing::warn!("opus decoder for {route_id} failed: {e}");
                        return;
                    }
                },
            };
            dec.set_profile(profile);
            match dec.decode(rtp_timestamp, &data) {
                Ok(frames) => frames,
                Err(e) => {
                    tracing::debug!("opus decode for {route_id} failed: {e}");
                    return;
                }
            }
        };
        for decoded in decoded {
            if decoded.kind != OpusDecodeKind::Normal
                && self.diag_ok(&format!("opus-recovery:{route_id}"))
            {
                tracing::debug!(
                    "opus {kind:?} recovery for {route_id} at RTP {rtp_timestamp}",
                    kind = decoded.kind
                );
            }
            let frame = AudioFrame::new_timestamped(
                route_id.clone(),
                decoded.seq,
                crate::audio::OPUS_RATE,
                2,
                decoded.media_timestamp_us,
                decoded.pcm,
            );
            self.audio.feed(&route_id, &frame);
        }
    }

    /// Count one inbound video payload for `route_id` and emit the
    /// receive-side dial-in line every few seconds:
    /// `video in <route>: 28.4 fps · 4.1 Mbps · H.264`.
    fn note_video_in(&self, route_id: &str, label: &'static str, bytes: usize) {
        const EVERY: std::time::Duration = std::time::Duration::from_secs(5);
        let mut map = self.video_in_stats.lock();
        let st = map
            .entry(route_id.to_string())
            .or_insert_with(|| VideoInStats::new(label));
        st.label = label;
        st.frames += 1;
        st.bytes += bytes as u64;
        let elapsed = st.since.elapsed();
        if elapsed >= EVERY {
            let secs = elapsed.as_secs_f64();
            let line = format!(
                "video in {route_id}: {:.1} fps · {:.1} Mbps · {}",
                st.frames as f64 / secs,
                (st.bytes as f64 * 8.0) / secs / 1_000_000.0,
                st.label,
            );
            if crate::video::stats_to_info() {
                tracing::info!("{line}");
            } else {
                tracing::debug!("{line}");
            }
            st.since = std::time::Instant::now();
            st.frames = 0;
            st.bytes = 0;
        }
    }

    /// One assembled H.264 access unit arrived on a peer's track lane
    /// `stream`. It belongs to whichever of our routes maps to that lane
    /// (the lane-th H.264 route from this peer in sorted order —
    /// [`Self::video_route_for_lane`]), gated exactly like MJPEG frames
    /// (route live, sinks here, sender is the route's peer) before it
    /// reaches a console window. Where it goes next is the watcher's
    /// choice: access units straight through (the webview decodes —
    /// WebCodecs), or through the native decoder, which hands the window
    /// ready-to-paint RGBA frames.
    /// The binary media-pipe caller carries a process-local profiler id from
    /// the local IPC reader into the decoder queue. The id is not present on
    /// the network or any daemon protocol frame. The base64 fallback passes
    /// an expected route generation so queued predecessor access units cannot
    /// cross a lane rebind.
    #[allow(clippy::too_many_arguments)]
    fn handle_video_inbound_profiled(
        self: &Arc<Self>,
        network: &str,
        from: &str,
        stream: u8,
        rtp_timestamp: u32,
        key: bool,
        data: Vec<u8>,
        profile_id: u64,
        expected: Option<(String, u64)>,
    ) {
        let canon = pubkey_part(from).to_string();
        let (current_route, current_generation) = if expected.is_some() {
            self.video_route_generation_for_lane(network, from, stream)
        } else {
            (self.video_route_for_lane(network, from, stream), None)
        };
        if let Some((expected_route, generation)) = expected.as_ref() {
            if !queued_video_binding_matches(
                current_route.as_deref(),
                current_generation,
                expected_route,
                *generation,
            ) {
                tracing::debug!(
                    route = %expected_route,
                    generation,
                    network,
                    peer = %short_id(from),
                    stream,
                    "dropping queued base64 H.264 after its lane or route generation changed"
                );
                return;
            }
        }
        let Some(route_id) = current_route else {
            // The sender is streaming the track lane at us but no route
            // here maps to it — the one-sided stream the viewer reads as
            // "connecting forever". Loud (rate-limited): this exact drop
            // was a debug whisper while the stage sat black.
            if self.diag_ok(&format!("lane:{network}:{canon}:{stream}")) {
                tracing::warn!(
                    "H.264 samples arriving from {} on lane {stream} but no route maps to it — dropped (viewer shows nothing)",
                    short_id(from)
                );
            }
            self.nack_dead_lane(network, from, "video", stream);
            return;
        };
        self.clear_dead_lane(network, from, "video", stream);
        if !self.inbound_route_network_ok(&route_id, from, network) {
            if self.diag_ok(&format!("route-network:{route_id}:{network}")) {
                tracing::warn!(
                    route = %route_id,
                    network = %network,
                    peer = %short_id(from),
                    "dropping H.264 from a network that does not own this route lifetime"
                );
            }
            return;
        }
        match self.inbound_video_disposition(&route_id, from) {
            InboundVideoDisposition::Accept => {}
            InboundVideoDisposition::Pending => {
                if self.diag_ok(&format!("pending:{route_id}")) {
                    tracing::info!(
                        "early H.264 sample for {route_id} dropped during Offer→Accept; replacement route left intact"
                    );
                }
                return;
            }
            InboundVideoDisposition::Reject => {
                if self.diag_ok(&format!("gate:{route_id}")) {
                    tracing::warn!(
                        "H.264 samples for {route_id} refused — {}",
                        self.route_diag(&route_id, from)
                    );
                }
                self.nack_dead_route(network, from, &route_id);
                return;
            }
        }
        if let Some((expected_route, generation)) = expected {
            let committed = commit_current_video_generation(
                &self.video_route_generations,
                &expected_route,
                generation,
                || {
                    // Generation and binding changes take this same outer
                    // fence. Re-resolve the lane at the commit boundary so a
                    // multi-monitor lane move cannot admit its old queued AU.
                    if self.video_route_for_lane(network, from, stream).as_deref()
                        != Some(expected_route.as_str())
                    {
                        return false;
                    }
                    self.commit_video_inbound_profiled(
                        from,
                        route_id,
                        rtp_timestamp,
                        key,
                        data,
                        profile_id,
                    );
                    true
                },
            );
            if committed != Some(true) {
                tracing::debug!(
                    route = %expected_route,
                    generation,
                    network,
                    peer = %short_id(from),
                    stream,
                    "dropping queued base64 H.264 at final decoder/watcher admission after its binding changed"
                );
            }
        } else {
            self.commit_video_inbound_profiled(
                from,
                route_id,
                rtp_timestamp,
                key,
                data,
                profile_id,
            );
        }
    }

    /// Commit one already-gated H.264 access unit to native decode or the raw
    /// watcher queue. Base64 callers run this entire method under
    /// [`commit_current_video_generation`], which is the final route-lifetime
    /// fence. The binary media source is not route-identifying yet and enters
    /// through the legacy unfenced call until that daemon capability exists.
    fn commit_video_inbound_profiled(
        self: &Arc<Self>,
        from: &str,
        route_id: String,
        rtp_timestamp: u32,
        key: bool,
        data: Vec<u8>,
        profile_id: u64,
    ) {
        // The arrival side of the sender's "route active — streaming"
        // line: one INFO per stream, so a healthy hop is attributable
        // from this end too (the MJPEG path has logged its first frame
        // this way all along).
        let first = !self.video_in_stats.lock().contains_key(&route_id);
        if should_hold_first_video_sample(first, key, &data) {
            if self.diag_ok(&format!("entry:{route_id}")) {
                tracing::warn!(
                    "holding video deltas for {route_id} until a clean decode entry starts the current route generation"
                );
            }
            let mesh = self.clone();
            let refresh_route = route_id.clone();
            crate::spawn(async move {
                let _ = mesh.request_refresh_for_recovery(refresh_route).await;
            });
            return;
        }
        self.note_video_in(&route_id, "H.264", data.len());
        // Time the pacer's chunk trains as they land — the bandwidth
        // estimate + delay trend the feedback loop reports back (M3/T1.1).
        self.note_video_arrival(&route_id, rtp_timestamp, data.len());
        let decode_epoch = self
            .video_watchers
            .lock()
            .get(&route_id)
            .filter(|watcher| watcher.decode)
            .map(|watcher| watcher.decode_epoch);
        let wants_decode = decode_epoch.is_some();
        if first {
            tracing::info!(
                "first H.264 sample for {route_id} from {} ({} bytes, key={key}, native decode={wants_decode})",
                short_id(from),
                data.len(),
            );
        }
        // 90 kHz RTP clock → µs for the decoder's timestamps.
        let ts_us = rtp_timestamp as u64 * 1000 / 90;
        let decode_entry = key || crate::video_decode::is_decode_entry(&data);
        if wants_decode {
            let mesh = Arc::downgrade(self);
            let rid = route_id.clone();
            let decode_epoch = decode_epoch.expect("native decoder claim checked above");
            let glitch_mesh = Arc::downgrade(self);
            let glitch_rid = route_id.clone();
            let _accepted = self.video_decode.feed_profiled(
                &route_id,
                Au { ts_us, key, data },
                profile_id,
                move |packet, frame_id, frame_ts_us| {
                    if let Some(mesh) = mesh.upgrade() {
                        mesh.enqueue_decoded_for_epoch(
                            &rid,
                            decode_epoch,
                            packet,
                            frame_id,
                            frame_ts_us,
                        );
                    }
                },
                move |lost_ts_us| {
                    // The native decoder hit a corrupt unit or dumped its
                    // queue: name the broken AU in feedback (a capable
                    // sender heals with a GDR wave, no keyframe wall) AND
                    // keep the rate-limited re-key ask — old senders need
                    // it, and for new ones the wave lands first so the
                    // wall it forces is the same one today's path forced.
                    if let Some(mesh) = glitch_mesh.upgrade() {
                        let rid = glitch_rid.clone();
                        crate::spawn(async move {
                            let policy_v1 = mesh.media_policy.lock().plan(&rid).is_some();
                            if lost_ts_us.is_some() {
                                let _ = mesh
                                    .send_video_feedback(rid.clone(), 0, 1, 0, lost_ts_us)
                                    .await;
                            }
                            // A v1 sender consumes the loss report with one
                            // capability-aware wave-or-IDR strategy. Only an
                            // older peer also needs the legacy Refresh ask;
                            // sending both made GDR immediately self-defeat
                            // into a keyframe wall.
                            if !policy_v1 {
                                let _ = mesh.request_refresh_for_recovery(rid).await;
                            }
                        });
                    }
                },
            );
        } else {
            // NOT latest_wins: H.264 deltas must all reach the decoder in
            // order — freshest-wins happens after decode (enqueue_decoded) or
            // at the GUI's paint slot instead.
            let profile_id = if profile_id == 0 {
                crate::pipeline_profile::next_frame_id()
            } else {
                profile_id
            };
            let enqueue = self.enqueue_for_watcher(
                &route_id,
                h264_ipc_bytes(ts_us, decode_entry, &data),
                false,
                profile_id,
                Some(ts_us),
            );
            match enqueue {
                WatcherEnqueue::NeedsRefresh => {
                    let mesh = self.clone();
                    crate::spawn(async move {
                        let _ = mesh.request_refresh_for_recovery(route_id).await;
                    });
                }
                WatcherEnqueue::Accepted | WatcherEnqueue::Dropped => {}
            }
        }
    }

    /// Queue one packet for a watching console window; drop the packet
    /// (with a debug note) when no window watches the route. A queue
    /// nobody drains (webview wedged or closing) caps at a few seconds
    /// of stream and is then cleared wholesale — the decoder re-keys on
    /// the sender's next IDR, and `video_unwatch`/route teardown remove
    /// the entry entirely.
    fn enqueue_for_watcher(
        &self,
        route_id: &str,
        packet: Vec<u8>,
        latest_wins: bool,
        profile_id: u64,
        frame_ts_us: Option<u64>,
    ) -> WatcherEnqueue {
        // About 100 ms at 60 fps. Encoded H.264 must remain ordered, but a
        // one-second (formerly two-to-four-second) WebCodecs backlog is
        // already stale. Overflow below re-enters only on a key unit.
        const MAX_QUEUED: usize = 6;
        let mut map = self.video_watchers.lock();
        let Some(w) = map.get_mut(route_id) else {
            drop(map);
            // Routine for a beat while a window boots; a *persistent* run
            // of these is a stream with nowhere to land — say so at a
            // visible level (rate-limited) instead of the debug whisper
            // that read as a silent black stage.
            if self.diag_ok(&format!("watchless:{route_id}")) {
                tracing::info!(
                    "frames flowing for {route_id} but no window is watching it — dropping until one does"
                );
            }
            return WatcherEnqueue::Dropped;
        };
        let was_empty = w.queue.is_empty();
        let packet_is_key = !latest_wins && packet.first() == Some(&2) && packet.get(1) == Some(&1);
        if latest_wins {
            // Self-contained frames (MJPEG): anything the window hasn't
            // pulled yet is stale the moment a newer picture exists —
            // painting history buys nothing but lag. Mirrors
            // enqueue_decoded's freshest-wins.
            w.queue.clear();
        } else if w.awaiting_key {
            if !packet_is_key {
                return WatcherEnqueue::Dropped;
            }
            w.queue.clear();
            w.awaiting_key = false;
        } else if w.queue.len() >= MAX_QUEUED {
            // H.264 backlog: skip forward to the NEWEST queued key unit —
            // decode re-enters there cleanly, and the viewer jumps to
            // near-live instead of replaying the whole backlog. The ipc
            // header carries the key flag (kind 2 at byte 0, key at byte
            // 1). No queued key (one delta chain longer than the cap, or
            // a keyless backlog): the old wholesale clear, recovering on
            // the sender's next IDR.
            match w
                .queue
                .iter()
                .rposition(|p| p.bytes.first() == Some(&2) && p.bytes.get(1) == Some(&1))
            {
                Some(i) if i > 0 => {
                    tracing::debug!(
                        "video queue for {route_id} unread — skipped {i} stale packets to its newest keyframe"
                    );
                    w.queue.drain(..i);
                }
                Some(_) => {
                    // The only key is already at the front and the queue is
                    // still at cap — the chain itself outgrew the bound.
                    tracing::debug!("video queue for {route_id} unread for a second — cleared");
                    w.queue.clear();
                    if !packet_is_key {
                        w.awaiting_key = true;
                        return WatcherEnqueue::NeedsRefresh;
                    }
                }
                None => {
                    tracing::debug!("video queue for {route_id} unread and keyless — cleared");
                    w.queue.clear();
                    if !packet_is_key {
                        w.awaiting_key = true;
                        return WatcherEnqueue::NeedsRefresh;
                    }
                }
            }
        }
        w.queue
            .push_back(ViewerPacket::new(packet, profile_id, frame_ts_us));
        // Poke the watcher when the queue goes non-empty: the console
        // pulls on a timer, but Chromium throttles timers in occluded
        // windows (a non-maximized console behind the main window paints
        // ~1 fps) — the event rides eval, which isn't throttled, and it
        // also shaves the poll interval off delivery latency. Coalesced
        // by construction: no further pokes until the queue drains.
        drop(map);
        if was_empty {
            self.sink.emit("allmystuff://video-ready", json!(route_id));
        }
        WatcherEnqueue::Accepted
    }

    /// Queue one natively decoded frame, freshest-wins: a decoded picture
    /// supersedes anything the window hasn't pulled yet (each is a complete
    /// screen — painting two per tick buys nothing but latency). Encoded
    /// packets append instead, because H.264 deltas must all reach their
    /// decoder; that distinction is the whole reason for two enqueues.
    #[cfg(test)]
    fn enqueue_decoded(&self, route_id: &str, packet: Vec<u8>, profile_id: u64, frame_ts_us: u64) {
        let Some(epoch) = self
            .video_watchers
            .lock()
            .get(route_id)
            .filter(|watcher| watcher.decode)
            .map(|watcher| watcher.decode_epoch)
        else {
            return;
        };
        self.enqueue_decoded_for_epoch(route_id, epoch, packet, profile_id, frame_ts_us);
    }

    fn enqueue_decoded_for_epoch(
        &self,
        route_id: &str,
        decode_epoch: u64,
        packet: Vec<u8>,
        profile_id: u64,
        frame_ts_us: u64,
    ) {
        let mut map = self.video_watchers.lock();
        let Some(w) = map.get_mut(route_id) else {
            tracing::debug!("no console window watching {route_id} — decoded frame dropped");
            return;
        };
        if !w.decode || w.decode_epoch != decode_epoch {
            return;
        }
        let was_empty = w.queue.is_empty();
        w.queue.clear();
        w.queue
            .push_back(ViewerPacket::new(packet, profile_id, Some(frame_ts_us)));
        drop(map);
        if was_empty {
            self.sink.emit("allmystuff://video-ready", json!(route_id));
        }
    }

    /// Front-end command: offer a route from `from` to `to`.
    pub async fn connect(
        self: &Arc<Self>,
        from: String,
        to: String,
        media: String,
        video: Vec<String>,
    ) -> Result<String, String> {
        self.connect_term(from, to, media, video, None).await
    }

    /// [`connect`](Self::connect) with an optional terminal **session** to
    /// attach to (the multi-attach entry point): `Some(id)` makes the
    /// terminal Offer name that already-running host shell to join, `None`
    /// (and every non-terminal route) mints a fresh session as before.
    pub async fn connect_term(
        self: &Arc<Self>,
        from: String,
        to: String,
        media: String,
        video: Vec<String>,
        session: Option<String>,
    ) -> Result<String, String> {
        self.connect_term_handle(from, to, media, video, session)
            .await
            .map(|handle| handle.route_id)
    }

    /// Handle-returning form used by GUI callers that must fence a delayed
    /// local close from a same-id successor. The legacy string-returning form
    /// remains for CLI and embedding compatibility.
    pub async fn connect_term_handle(
        self: &Arc<Self>,
        from: String,
        to: String,
        media: String,
        video: Vec<String>,
        session: Option<String>,
    ) -> Result<RouteConnectHandle, String> {
        let requested_video = video;
        let me = self.resolve_local_id().await.ok_or("mesh not ready")?;
        let media = parse_media(&media);
        let route = Route {
            id: format!("route:{from}→{to}"),
            from: from.clone().into(),
            to: to.clone().into(),
            media,
        };
        let from_node = node_of(&from);
        let to_node = node_of(&to);
        // Self / loopback is decided by *canonical* node id: the route's
        // endpoints carry the suffixed display id the UI built them from,
        // while `me` is the bare node id, so a raw `==` would miss a genuine
        // self-route and offer it over the wire (where it never returns) —
        // which is exactly what stopped local terminals from opening.
        let from_is_me = same_node(&from_node, &me);
        let to_is_me = same_node(&to_node, &me);
        let peer = if from_is_me { to_node } else { from_node };
        // Receive readiness belongs to the peer's candidate networks, not to
        // one process-wide boolean. Preserve requested transports separately
        // so a later daemon-session replay can restore them after a dark
        // network's supervised subscription heals.
        let video = if requested_video.is_empty() {
            Vec::new()
        } else {
            self.await_video_bringup(&peer).await;
            if self.peer_video_ready(&peer) {
                requested_video.clone()
            } else {
                Vec::new()
            }
        };
        let requested_audio = if media == MediaKind::Audio && to_is_me {
            vec!["opus".to_string()]
        } else {
            Vec::new()
        };
        let audio = if requested_audio.is_empty() || !self.peer_audio_ready(&peer) {
            Vec::new()
        } else {
            requested_audio.clone()
        };
        let local_generation = self.next_route_intent_generation();
        let handle = RouteConnectHandle {
            route_id: route.id.clone(),
            generation: local_generation,
        };
        // Connect and disconnect for a deterministic route id share this
        // critical section. Desired-intent generation, Session lifetime, and
        // the first wire Offer therefore advance as one ordered operation;
        // a delayed close for predecessor A cannot remove or tear down B.
        let lifecycle = self.lock_route_lifecycle(&route.id).await;
        let previous_incarnation = {
            self.active_media_incarnations
                .lock()
                .get(&route.id)
                .cloned()
        };
        if let Some(previous_incarnation) = previous_incarnation {
            self.apply_stop_media_locked(route.id.clone(), previous_incarnation);
        }
        self.pending_teardowns
            .lock()
            .retain(|(pending_route, _), _| pending_route != &route.id);
        self.state
            .lock()
            .route_networks
            .retain(|(pinned_route, _), _| pinned_route != &route.id);
        self.desired_routes.lock().insert(
            route.id.clone(),
            DesiredRoute {
                route: route.clone(),
                peer: peer.clone(),
                requested_video: requested_video.clone(),
                requested_audio: requested_audio.clone(),
                term_session: session.clone(),
                local_generation,
                current_incarnation: None,
            },
        );

        // A daemon event reconnect deliberately removes the ephemeral Session.
        // Preserve the user's intent and return its handle instead of failing
        // the UI action. `bring_up` replays every desired route once the fresh
        // local subscriptions and presence profile are installed.
        if self.state.lock().session.is_none() {
            tracing::info!(
                route = %route.id,
                generation = local_generation,
                "route intent retained while the mesh session is rebuilding"
            );
            drop(lifecycle);
            return Ok(handle);
        }

        if from_is_me && to_is_me {
            // Local loopback (e.g. this machine's mic to its own speakers):
            // no peer to negotiate with — record it active and stream now.
            // Offer-then-Accept drives the session to Active and yields the
            // StartMedia effect we process below.
            let incarnation = self.next_route_incarnation(&me);
            if let Some(desired) = self.desired_routes.lock().get_mut(&route.id) {
                if desired.local_generation == local_generation {
                    desired.current_incarnation = incarnation.clone();
                }
            }
            let (offer, effects) = {
                let mut st = self.state.lock();
                let Some(s) = st.session.as_mut() else {
                    tracing::info!(
                        route = %route.id,
                        generation = local_generation,
                        "loopback route intent deferred to fresh mesh session"
                    );
                    drop(st);
                    drop(lifecycle);
                    return Ok(handle);
                };
                // Loopback terminals carry the attach session too, so two
                // local windows can share one local shell (multi-attach to
                // yourself); harmless `None` on every other loopback route.
                let offer = s.offer_terminal_with_incarnation(
                    route.clone(),
                    me.as_str(),
                    Vec::new(),
                    Vec::new(),
                    session.clone(),
                    incarnation.clone(),
                );
                let effects = s.handle(
                    NodeId::from(me.as_str()),
                    ControlMessage::Route(RouteControl::Accept {
                        route_id: route.id.clone(),
                        incarnation,
                        session: None,
                    }),
                );
                (offer, effects)
            };
            let Some(loopback_network) = self
                .route_network_candidates(&me, &offer)
                .into_iter()
                .next()
            else {
                self.desired_routes.lock().remove(&route.id);
                if let Some(session) = self.state.lock().session.as_mut() {
                    let _ = session.teardown(&route.id);
                }
                drop(lifecycle);
                return Err("no joined data-plane network for loopback route".into());
            };
            {
                let mut state = self.state.lock();
                Self::commit_inbound_route_network_locked(
                    &mut state,
                    &me,
                    &offer,
                    &loopback_network,
                );
            }
            drop(lifecycle);
            self.process_effects(effects).await;
            self.replay_requested_video_tune(&route.id).await;
            self.emit_snapshot();
            return Ok(handle);
        }

        if matches!(route.media, MediaKind::Display | MediaKind::Video) {
            if let Some(token) =
                self.cancel_pending_video_teardown_replaced(&route.id, peer.as_str())
            {
                tracing::warn!(
                    route = %route.id,
                    peer = %short_id(peer.as_str()),
                    token,
                    disposition = "quarantine_canceled_by_local_reoffer",
                    "local video route control"
                );
            }
        }
        let incarnation = self.next_route_incarnation(peer.as_str());
        if let Some(desired) = self.desired_routes.lock().get_mut(&route.id) {
            if desired.local_generation == local_generation {
                desired.current_incarnation = incarnation.clone();
            }
        }
        let msg = {
            let mut st = self.state.lock();
            let Some(s) = st.session.as_mut() else {
                tracing::info!(
                    route = %route.id,
                    generation = local_generation,
                    "remote route intent deferred to fresh mesh session"
                );
                drop(st);
                drop(lifecycle);
                return Ok(handle);
            };
            s.offer_terminal_with_incarnation(
                route.clone(),
                peer.as_str(),
                video.clone(),
                audio.clone(),
                session.clone(),
                incarnation.clone(),
            )
        };
        if let Err(e) = self.send_control(&peer, &msg).await {
            // The peer never saw the offer — drop it rather than leave a
            // phantom half-open route in the session.
            tracing::warn!(
                "route {} offer to {} not dispatched on the fast path: {e}; retaining it for retry",
                route.id,
                short_id(&peer)
            );
        }
        drop(lifecycle);
        // The accept lands moments later as the route's "active" line; an
        // offer that goes nowhere has its own warns above and below. At
        // INFO (not DEBUG) so a default-level capture shows the whole
        // offer → accept → active arc — the silence after this line is the
        // tell when a route is offered but the peer never accepts.
        tracing::info!(
            "route {} offered to {} — awaiting accept",
            route.id,
            short_id(&peer)
        );
        self.emit_snapshot();
        Ok(handle)
    }

    /// Register interest in one route's inbound frames (replacing any
    /// previous watcher — the route shows in one window). Packets queue
    /// from this moment; the window drains them with [`Self::video_poll`].
    /// `decode` asks the backend to run inbound H.264 through the native
    /// decoder and queue ready-to-paint RGBA frames instead of access
    /// units — for webviews without WebCodecs, and the last rung of the
    /// console's decode ladder. Returns the claim token to pass back to
    /// [`Self::video_unwatch`].
    pub fn video_watch(self: &Arc<Self>, route_id: String, decode: bool) -> u64 {
        let token = next_js_safe_counter(&self.video_watch_token);
        let decode_epoch = if decode {
            self.video_watchers
                .lock()
                .get(&route_id)
                .filter(|watcher| watcher.decode)
                .map(|watcher| watcher.decode_epoch)
                .unwrap_or_else(next_native_decode_epoch)
        } else {
            0
        };
        // One line per watch claim, so a viewer-side log shows which
        // window holds each stream and on which decode path — the missing
        // half of "frames flowing but no window watching".
        tracing::info!("window watching {route_id} (native decode: {decode})");
        // A fresh watch (a re-open, an input switch) must start its peer's
        // video dead-lane grace over. When the previous session closed, its
        // orphaned frames drained with no route here and left a stale
        // "unmapped since" mark for that peer's lane. Without this reset, the
        // very first frame of the NEW stream — arriving in the brief gap before
        // the route is mapped — sees that elapsed grace and NACKs the sender
        // instantly, whose handle_dead_lane then StopMedia's the capture we
        // just restarted: the "reconnect shows nothing / connecting forever"
        // loop, and it bit every video route, not just CEC. Clearing only on a
        // *new* watch keeps a genuine close (no re-watch) NACKing as before.
        //
        // Derive the peer straight from the route id (`route:{from}→{to}`), NOT
        // the session: at watch time the route often isn't registered yet (the
        // offer and this watch land in the same tick — the daemon logs both at
        // the same millisecond), so route_peer would return None and silently
        // skip the reset. For an inbound video route the `from` end is the
        // streaming peer, which is exactly what nack_dead_lane keys on.
        if let Some(from_cap) = route_id
            .strip_prefix("route:")
            .and_then(|s| s.split_once('→'))
            .map(|(from, _)| from)
        {
            let peer_node = node_of(from_cap);
            let prefix = format!("deadlane:video:{}:", pubkey_part(&peer_node));
            self.dead_lane_since
                .lock()
                .retain(|k, _| !k.starts_with(&prefix));
        }
        self.video_watchers.lock().claim(
            route_id.clone(),
            VideoWatcher {
                token,
                decode,
                decode_epoch,
                queue: std::collections::VecDeque::new(),
                awaiting_key: false,
                last_poll: None,
            },
        );
        if !decode {
            // Publish pass-through ownership before stopping the old decoder.
            // stop can wait for its worker; inbound AUs and late callbacks in
            // that interval must see the successor mode and token.
            self.video_decode.stop(&route_id);
            let mesh = self.clone();
            let refresh_route = route_id.clone();
            crate::spawn(async move {
                let _ = mesh.request_refresh(refresh_route).await;
            });
        }
        token
    }

    /// Release a watch claim — only if `token` still owns the route. A
    /// late unwatch from a replaced watcher is a no-op instead of
    /// deleting its successor's queue.
    pub fn video_unwatch(self: &Arc<Self>, route_id: &str, token: u64) {
        let release = self.video_watchers.lock().release(route_id, token);
        let Some((removed_decode, restored_decode)) = release else {
            return;
        };
        if !removed_decode && restored_decode == Some(true) {
            if let Some(restored) = self.video_watchers.lock().get_mut(route_id) {
                restored.decode_epoch = next_native_decode_epoch();
            }
        }
        if restored_decode != Some(true) {
            self.video_decode.stop(route_id);
        }
        if restored_decode.is_some_and(|decode| decode != removed_decode || !decode) {
            let mesh = self.clone();
            let route_id = route_id.to_string();
            crate::spawn(async move {
                let _ = mesh.request_refresh(route_id).await;
            });
        }
    }

    /// Validate a GUI-originated health report against the active local watch.
    /// This token never leaves the machine; it prevents a displaced webview
    /// from keeping a route alive or adapting its sender after losing ownership.
    pub fn video_watcher_is_current(&self, route_id: &str, token: u64) -> bool {
        self.video_watchers
            .lock()
            .get(route_id)
            .is_some_and(|watcher| watcher.token == token)
    }

    /// Drain everything queued for `route_id` while measuring the viewer-side
    /// lock, poll cadence, and queue residence. Framing happens after this
    /// returns, outside the watcher lock.
    fn drain_video_poll(
        &self,
        route_id: &str,
        token: Option<u64>,
    ) -> std::collections::VecDeque<ViewerPacket> {
        let lock_started = crate::pipeline_profile::stamp();
        let mut map = self.video_watchers.lock();
        let lock_acquired = crate::pipeline_profile::stamp();
        let Some(w) = map.get_mut(route_id) else {
            return std::collections::VecDeque::new();
        };
        if token.is_some_and(|token| token != w.token) {
            return std::collections::VecDeque::new();
        }
        let polled_at = Instant::now();
        let poll_cadence = w
            .last_poll
            .replace(polled_at)
            .map(|last| polled_at.saturating_duration_since(last));
        let packets = std::mem::take(&mut w.queue);
        drop(map);

        if let Some(poll_cadence) = poll_cadence {
            crate::pipeline_profile::record_at(
                route_id,
                0,
                None,
                crate::pipeline_profile::Stage::ViewerPollCadence,
                poll_cadence,
                polled_at,
            );
        }
        if let (Some(started), Some(ended)) = (lock_started, lock_acquired) {
            crate::pipeline_profile::record_at(
                route_id,
                0,
                None,
                crate::pipeline_profile::Stage::ViewerPollLockWait,
                ended.saturating_duration_since(started),
                ended,
            );
        }
        for packet in &packets {
            if let Some(enqueued_at) = packet.enqueued_at {
                crate::pipeline_profile::record_at(
                    route_id,
                    packet.profile_id,
                    packet.frame_ts_us,
                    crate::pipeline_profile::Stage::ViewerQueueWait,
                    polled_at.saturating_duration_since(enqueued_at),
                    polled_at,
                );
            }
        }
        packets
    }

    /// Drain queued video packets for the local node-control server's
    /// segmented writer. The resulting on-socket payload remains byte-for-byte
    /// identical to [`Self::video_poll`], but native RGBA frames are not copied
    /// into an intermediate batch allocation.
    pub(crate) fn video_poll_batch(&self, route_id: &str, token: Option<u64>) -> VideoPollBatch {
        let packets = self.drain_video_poll(route_id, token);
        if packets.is_empty() {
            return VideoPollBatch::new(packets);
        }

        let batch_started = crate::pipeline_profile::stamp();
        let batch = VideoPollBatch::new(packets);
        crate::pipeline_profile::record_since(
            route_id,
            0,
            None,
            crate::pipeline_profile::Stage::ViewerBatchBusy,
            batch_started,
        );
        batch
    }

    /// Compatibility form of [`Self::video_poll_batch`] for in-process
    /// callers: one contiguous `[u32 len][packet]...` payload.
    pub fn video_poll(&self, route_id: &str) -> Vec<u8> {
        self.video_poll_for(route_id, None)
    }

    pub fn video_poll_for(&self, route_id: &str, token: Option<u64>) -> Vec<u8> {
        let packets = self.drain_video_poll(route_id, token);
        if packets.is_empty() {
            return Vec::new();
        }

        let batch_started = crate::pipeline_profile::stamp();
        let out = VideoPollBatch::new(packets).into_bytes();
        crate::pipeline_profile::record_since(
            route_id,
            0,
            None,
            crate::pipeline_profile::Stage::ViewerBatchBusy,
            batch_started,
        );
        out
    }

    pub async fn disconnect(self: &Arc<Self>, route_id: String) -> Result<(), String> {
        self.disconnect_expected(route_id, None).await
    }

    /// Tear down a local route intent, optionally requiring the exact
    /// process-local generation returned by [`Self::connect_term_handle`].
    /// Generation-less callers retain the legacy behavior; current GUI
    /// callers always provide it for ABA-safe display switching.
    pub async fn disconnect_expected(
        self: &Arc<Self>,
        route_id: String,
        expected_generation: Option<u64>,
    ) -> Result<(), String> {
        // Fast rejection avoids entering the monitor-switch observation window
        // for an already-proven stale close. This is only an optimization; the
        // generation is checked again under the route lifecycle lock below.
        if let Some(expected) = expected_generation {
            let current = self
                .desired_routes
                .lock()
                .get(&route_id)
                .map(|route| route.local_generation);
            if current != Some(expected) {
                tracing::warn!(
                    route = %route_id,
                    expected_generation = expected,
                    current_generation = ?current,
                    disposition = "stale_local_teardown_ignored",
                    "local route teardown generation mismatch"
                );
                return Ok(());
            }
        }
        if let Some(hit) = self.take_early_video_teardown_guard(&route_id) {
            // Do not infer intent from watcher *presence*: unwatch is a
            // separate fire-and-forget command and can lag. A window that polls
            // after this disconnect began is positive proof the successor is
            // still live; an intentional close stops its 16 ms poll loop even
            // if cleanup delivery itself is delayed.
            let guarded_at = Instant::now();
            tokio::time::sleep(
                VIDEO_SWITCH_TEARDOWN_GUARD
                    .saturating_sub(hit.age)
                    .max(VIDEO_LOCAL_POLL_OBSERVE),
            )
            .await;
            let polled_after_request = self
                .video_watchers
                .lock()
                .get(&route_id)
                .is_some_and(|watcher| watcher_poll_proves_liveness(watcher.last_poll, guarded_at));
            let still_nonterminal = self
                .state
                .lock()
                .session
                .as_ref()
                .and_then(|s| s.route(&route_id))
                .is_some_and(|r| {
                    matches!(
                        r.state,
                        RouteState::Offered | RouteState::Incoming | RouteState::Active
                    )
                });
            if polled_after_request && still_nonterminal {
                tracing::warn!(
                    "ignored stale local video teardown for {route_id} after switch from {} — the successor window kept polling",
                    hit.predecessor,
                );
                return Ok(());
            }
            tracing::info!(
                "early local video teardown for {route_id} confirmed after switch from {} (no successor poll) — committing",
                hit.predecessor,
            );
        }
        let lifecycle = self.lock_route_lifecycle(&route_id).await;

        // Validate and remove the exact desired lifetime only after every
        // await and under the same lock used by connect/replay. If successor B
        // installed while delayed close A was observing watcher liveness, this
        // second check returns without touching B's Session or media resources.
        {
            let mut desired = self.desired_routes.lock();
            if let Some(expected) = expected_generation {
                let current = desired.get(&route_id).map(|route| route.local_generation);
                if current != Some(expected) {
                    tracing::warn!(
                        route = %route_id,
                        expected_generation = expected,
                        current_generation = ?current,
                        disposition = "stale_local_teardown_ignored_after_wait",
                        "local route teardown generation changed before commit"
                    );
                    drop(lifecycle);
                    return Ok(());
                }
            }
            desired.remove(&route_id);
        }
        self.requested_video_tunes.lock().remove(&route_id);

        let (msg, peer, route_network) = {
            let mut st = self.state.lock();
            let route_facts = st
                .session
                .as_ref()
                .and_then(|session| session.route(&route_id))
                .map(|route| (route.peer.to_string(), route.incarnation.clone()));
            let route_network = route_facts
                .as_ref()
                .and_then(|(_, incarnation)| {
                    st.route_networks
                        .remove(&(route_id.clone(), incarnation.clone()))
                })
                .map(|pin| pin.network);
            let peer = route_facts.map(|(peer, _)| peer);
            let message = st.session.as_mut().and_then(|s| s.teardown(&route_id));
            (message, peer, route_network)
        };
        tracing::info!("local route teardown committing for {route_id}");
        self.audio.stop(&route_id);
        self.video.stop(&route_id);
        let policy_plans = {
            let serial = self.video_policy_apply_serial.lock();
            let plans = self.media_policy.lock().remove_route(&route_id);
            self.apply_video_policy_caps_locked(&plans, &serial);
            plans
        };
        self.send_effective_plans(policy_plans).await;
        self.video_watchers.lock().remove(&route_id);
        self.release_video_lanes(&route_id);
        self.release_audio_lanes(&route_id);
        self.active_media_incarnations.lock().remove(&route_id);
        self.terminal.stop(&route_id);
        self.files.stop(&route_id);
        // The unmapping (client) side gets no local StopMedia effect — only
        // the wire Teardown goes out — so close the listener + connections
        // here, or they'd leak (the port stays bound, the accept loop runs).
        self.sites.stop_route(&route_id);
        self.drop_downloads(&route_id);
        if let (Some(msg), Some(peer)) = (&msg, peer) {
            if self.peer_supports_teardown_ack(&peer) {
                if let ControlMessage::Route(RouteControl::Teardown {
                    route_id,
                    incarnation,
                }) = msg
                {
                    self.pending_teardowns.lock().insert(
                        (route_id.clone(), incarnation.clone()),
                        PendingTeardown {
                            peer: peer.clone(),
                            message: msg.clone(),
                            network: route_network.clone(),
                            created: Instant::now(),
                        },
                    );
                }
            }
            // Transport delivery is best-effort here; capable peers confirm
            // consumption with TeardownAck and the existing sweep retries
            // until that arrives or a fresh peer boot proves old state gone.
            let _ = if let Some(network) = route_network.as_deref() {
                self.send_control_on_network(&peer, msg, network).await
            } else {
                self.send_control(&peer, msg).await
            };
        }
        self.emit_snapshot();
        Ok(())
    }

    pub fn snapshot(&self) -> Value {
        let st = self.state.lock();
        let Some(session) = st.session.as_ref() else {
            return json!({ "ready": false });
        };
        let me = session.me().to_string();
        let network = st.network.clone();
        // A CEC customer a technician dialed is an ordinary mesh peer here, with
        // no special grouping: the CEC mesh is Silent (no roster), so there is no
        // "fleet" to seat it under. The CEC tab lists dialed customers from CEC
        // state (`cec_dialed`), not from the graph.
        let peers: Vec<Value> = session
            .peers()
            .map(|p| serde_json::to_value(p).unwrap_or(Value::Null))
            .collect();
        let routes: Vec<_> = session.routes().collect();
        // Durable shares (person + unioned grants) so the GUI reclassifies a
        // peer as *shared* with its grants across a restart, rather than
        // forgetting them and defaulting to unclaimed.
        let shares = self.shares.shares();
        json!({
            "ready": true,
            "me": me,
            "network": network,
            "peers": peers,
            "routes": routes,
            "shares": shares,
        })
    }

    fn route_peer(&self, route_id: &str) -> Option<String> {
        self.state
            .lock()
            .session
            .as_ref()
            .and_then(|s| s.route(route_id).map(|r| r.peer.to_string()))
    }

    // ---- CEC Support (technician + customer) --------------------------
    //
    // CEC Support rides this exact engine: every CEC node — customer and
    // technician — lives on the one shared **support area**
    // (`cecsupport-clients`, hub-shaped so customers connect only to CEC infra
    // and see nobody). A technician answers a raised hand (or a phoned-in
    // number) by dialing that customer's device on the area with `connect_peer`;
    // from then on that customer is an ordinary AllMyStuff graph peer with the
    // normal screen/control features. The only substitution is trust — a CEC
    // route is authorized by the customer's live consent grant ([`crate::cec`])
    // rather than owner/fleet, checked per frame in [`Self::sender_may_drive`]
    // so a revoke bites mid-session. Every command mirrors the node-control
    // surface the CEC client app and this app's CEC tab both depend on verbatim.

    /// `cec_status`: this node's CEC snapshot — its own support number (a
    /// display label), the shared support area, its role, and whether the
    /// technician's help-queue view is armed.
    pub async fn cec_status(&self) -> Result<Value, String> {
        let me = self.resolve_local_id().await;
        let mut status = self.cec.status(me.as_deref());
        if let Some(o) = status.as_object_mut() {
            // The technician's "watch the help queue" opt-in — a view state
            // the node holds, surfaced so the Support tab's toggle reflects it
            // across a reload.
            o.insert(
                "help_watching".into(),
                Value::Bool(self.cec.watching_help()),
            );
        }
        Ok(status)
    }

    /// `cec_online` (customer): take up residence on the shared support area
    /// (`cecsupport-clients`) — the CEC Support app calls this at bring-up and
    /// the membership is standing. This replaced per-number hosting: there is
    /// no number-derived room to advertise on anymore; the customer simply
    /// lives on the area (connected only to CEC's infra hubs under the area's
    /// hub topology, never to other customers) where technicians can see and
    /// deliberately dial them. Joining raises no hand — beacons are
    /// `cec_ask_help`'s job. Returns `{ number }` for the app's display: the
    /// digits a customer reads over the phone, derived from the device key.
    pub async fn cec_online(self: &Arc<Self>) -> Result<Value, String> {
        let me = self
            .resolve_local_id()
            .await
            .ok_or_else(|| "this device has no mesh identity yet".to_string())?;
        let number = self.cec.own_number(Some(&me));
        let (network_id, config) = crate::cec::help_network_config();
        self.cec_join_silent(&network_id, config).await?;
        self.cec_purge_legacy_rooms().await;
        tracing::info!("CEC Support: on the support area {network_id} as number {number}");
        Ok(json!({ "number": number }))
    }

    /// One-time sweep for installs upgrading from the per-number model:
    /// remove every `cec-<9 digits>` Silent room the daemon still carries —
    /// a customer's own number room, and every number room a technician's
    /// dials accumulated. Exactly prefix + digits, so the NanoKVM claim
    /// meshes (`cec-kvm-…`) can never match. Purge is deliberate: those
    /// rooms' rosters are meaningless now and a re-add would mint fresh
    /// state anyway.
    async fn cec_purge_legacy_rooms(self: &Arc<Self>) {
        let stale: Vec<String> = {
            let st = self.state.lock();
            st.networks
                .iter()
                .filter(|n| {
                    n.strip_prefix(allmystuff_cec_protocol::CEC_NETWORK_PREFIX)
                        .is_some_and(|rest| {
                            rest.len() == 9 && rest.chars().all(|c| c.is_ascii_digit())
                        })
                })
                .cloned()
                .collect()
        };
        if stale.is_empty() {
            return;
        }
        tracing::info!(
            "CEC Support: removing {} legacy per-number room(s) — sessions ride the shared area now",
            stale.len()
        );
        for network in stale {
            let _ = self
                .client
                .request(&Request::NetworkRemove {
                    network,
                    purge: true,
                })
                .await;
        }
        self.sync_networks().await;
    }

    /// `cec_ask_help { on }` (customer): raise the hand on the support area —
    /// beacon "I need help" until a technician connects or the customer
    /// cancels. The area is already home (`cec_online`), so asking is purely
    /// a beacon; the technician answers by dialing the beacon's device id
    /// right here on the area. The beacon carries want, never access: a
    /// session still takes the full consent handshake.
    pub async fn cec_ask_help(self: &Arc<Self>, on: bool) -> Result<Value, String> {
        let me = self
            .resolve_local_id()
            .await
            .ok_or_else(|| "this device has no mesh identity yet".to_string())?;
        if on {
            // Idempotent — bring-up already joined; a hand raised before the
            // first `cec_online` (or after a manual mesh removal) self-heals.
            let _ = self.cec_online().await;
            self.cec.set_asking_help(true);
            let epoch = self.cec.help_epoch();
            let (network_id, _) = crate::cec::help_network_config();
            let reached = self.cec_broadcast_presence(&network_id, &me, true).await;
            self.sink.emit("cec://help", json!({ "watchers": reached }));
            tracing::info!("CEC Support: asking for help on {network_id} (reached {reached})");
            // The room is Open, so the daemon is already auto-dialing every
            // watcher it sights — but the broadcast above races those dials
            // (a mid-handshake peer receives nothing). Re-beacon on a fast
            // burst (t=2,5,10,20s) so the hand is up within a couple of
            // seconds of the first wire, then settle into the keep-alive
            // cadence — technicians age a silent beacon out after
            // HELP_TTL_SECS. Every beat reports how many watchers it actually
            // reached, so the waiting card can say "raising your hand…" vs
            // "CEC can see you" honestly. The epoch guard means a cancel +
            // re-ask leaves exactly one loop beaconing.
            let mesh = self.clone();
            crate::spawn(async move {
                let mut burst = [2u64, 3, 5, 10].into_iter();
                loop {
                    let wait = burst.next().unwrap_or(crate::cec::HELP_BEACON_SECS);
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    if !mesh.cec.asking_help() || mesh.cec.help_epoch() != epoch {
                        return;
                    }
                    let (network_id, _) = crate::cec::help_network_config();
                    let reached = mesh.cec_broadcast_presence(&network_id, &me, true).await;
                    mesh.sink.emit("cec://help", json!({ "watchers": reached }));
                }
            });
        } else {
            self.cec_stop_asking_help(&me).await;
        }
        Ok(json!({ "asking": on }))
    }

    /// Withdraw the help ask: beacon `available: false` so technician queue
    /// caches clear at once instead of waiting out the TTL. Shared by the
    /// explicit cancel and the automatic clear when a session gets approved —
    /// help arrived, stop asking for it. Membership is untouched: the area is
    /// where this node lives (and where the approved session is riding), so
    /// lowering the hand only silences the beacon.
    async fn cec_stop_asking_help(self: &Arc<Self>, me: &str) {
        if !self.cec.set_asking_help(false) {
            return;
        }
        let (network_id, _) = crate::cec::help_network_config();
        let _ = self.cec_broadcast_presence(&network_id, me, false).await;
        // Tell this customer's own UI (the CEC Support app's waiting card) —
        // the automatic clear on approval otherwise leaves it looking armed.
        self.sink.emit("cec://help", json!({ "asking": false }));
        tracing::info!("CEC Support: no longer asking for help");
    }

    /// `cec_help_watch { on }` (technician): arm or disarm the help-queue
    /// view. Arming joins the support area (idempotent — a technician with
    /// dialed customers is already living there) and starts surfacing raised
    /// hands; disarming clears and hides the queue. Disarming does NOT leave
    /// the area: dialed customers' sessions ride it, and "stop watching the
    /// queue" must never mean "hang up on everyone". Watching is a view
    /// state, not a membership.
    pub async fn cec_help_watch(self: &Arc<Self>, on: bool) -> Result<Value, String> {
        if on {
            let (network_id, config) = crate::cec::help_network_config();
            self.cec_join_silent(&network_id, config).await?;
            self.cec_purge_legacy_rooms().await;
            self.cec.set_watching_help(true);
            tracing::info!("CEC Support: watching the help queue on {network_id}");
        } else {
            self.cec.set_watching_help(false);
            self.cec.clear_help();
            self.sink.emit("cec://help", json!({ "waiting": [] }));
            tracing::info!("CEC Support: stopped watching the help queue");
        }
        Ok(json!({ "watching": on }))
    }

    /// `cec_help_list` (technician): the customers currently waiting on the
    /// global help room, longest-waiting first. Read-only: it returns what the
    /// cache holds and never joins the room — joining is `cec_help_watch`'s
    /// job, an explicit opt-in, so merely opening the tab can't silently sign
    /// a node up for the global queue.
    pub async fn cec_help_list(self: &Arc<Self>) -> Result<Value, String> {
        Ok(Value::Array(self.cec.help_list()))
    }

    /// `cec_dial` (technician): the dial-by-number fallback — the digits a
    /// customer reads over the phone, for when the raised-hand list is too
    /// crowded to spot them (or they just prefer saying a number). Resolves
    /// the digits to a device id **on the support area** — a raised hand
    /// first (the beacon's authenticated sender), else any area member whose
    /// key-derived number matches — then dials that device like any answered
    /// hand. Numbers never name a room anymore. Returns `{ node }`.
    pub async fn cec_dial(
        self: &Arc<Self>,
        number: String,
        agent_name: String,
    ) -> Result<Value, String> {
        let digits = crate::cec::number_digits(&number);
        if digits.len() != 9 {
            return Err(format!(
                "'{number}' isn't a support number (9 digits, spacing optional)"
            ));
        }
        // The area is where customers are — be on it before looking.
        let (area, config) = crate::cec::help_network_config();
        self.cec_join_silent(&area, config).await?;
        self.cec_purge_legacy_rooms().await;
        let node = match self.cec.help_seeker_by_number(&digits) {
            Some(node) => node,
            None => self
                .cec_member_by_number(&area, &digits)
                .await
                .ok_or_else(|| {
                    format!(
                        "no customer with number {} is on the support area right now — \
                     have them open CEC Support (or raise their hand) and try again",
                        crate::cec::grouped_number(&digits)
                    )
                })?,
        };
        self.cec_dial_node(node, agent_name).await
    }

    /// Scan the support area's member list for the device whose key-derived
    /// support number matches `digits`. Presence-level (Sighted counts) — the
    /// customer doesn't need a connection to be found, just to be alive on
    /// the area.
    async fn cec_member_by_number(&self, area: &str, digits: &str) -> Option<String> {
        let resp = self
            .client
            .request(&Request::PeersList {
                network: area.to_string(),
            })
            .await
            .ok()?;
        let peers = resp.data?.get("peers")?.as_array()?.to_owned();
        peers.iter().find_map(|p| {
            let id = p.get("device_id")?.as_str()?;
            (allmystuff_cec_protocol::support_id_from_device(id) == digits).then(|| id.to_string())
        })
    }

    /// `cec_dial_node` (technician): open a support session with `node` on
    /// the shared area — the headline path, fed straight from a raised
    /// hand's beacon (its authenticated device id), and the tail of the
    /// dial-by-number fallback. Pins the connection (a support session is a
    /// standing dial), records the customer in the device-keyed directory,
    /// and sends the consent connect-request stamped with `agent_name`.
    /// Returns `{ node }`.
    pub async fn cec_dial_node(
        self: &Arc<Self>,
        node: String,
        agent_name: String,
    ) -> Result<Value, String> {
        if !agent_name.trim().is_empty() {
            self.cec.set_agent_name(agent_name.clone());
        }
        let agent_name = if agent_name.trim().is_empty() {
            self.cec.agent_name()
        } else {
            agent_name
        };
        self.cec.note_technician();
        let (network_id, config) = crate::cec::help_network_config();
        self.cec_join_silent(&network_id, config).await?;
        let customer = node;
        let canonical = crate::cec::pubkey_part(&customer).to_string();
        let number = allmystuff_cec_protocol::support_id_from_device(&customer);
        // The row is directory-worthy from the moment of the dial — emitted
        // immediately so the CEC tab shows it right away; the post-connect
        // refresh below fills in the live ident.
        let (label, hostname) = self.cec_peer_ident(&canonical).unwrap_or_default();
        let attempt =
            self.cec
                .record_dialed(customer.clone(), number.clone(), label, hostname, false);
        self.sink.emit("cec://peer", attempt.to_value());
        let cancel = self.cec.begin_dial();
        self.client
            .request(&Request::NetworkConnectPeer {
                network: network_id.clone(),
                peer: canonical.clone(),
                // A support session is a standing dial: the daemon redials
                // this customer on every announce (the Silent room's one
                // exception) and never ages the intent out — the far end
                // sleeping, roaming, or rebooting no longer kills the
                // relationship. Persisted daemon-side with the network.
                pin: true,
                wait_ms: 0,
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|resp| {
                if resp.ok {
                    Ok(())
                } else {
                    Err(resp
                        .error
                        .unwrap_or_else(|| "connect_peer refused by the daemon".into()))
                }
            })?;

        let (label, hostname) = self.cec_peer_ident(&canonical).unwrap_or_default();
        let record =
            self.cec
                .record_dialed(customer.clone(), number.clone(), label, hostname, true);
        tracing::info!(
            "CEC Support: dialed customer {} on the support area",
            short_id(&customer)
        );

        // The connect handshake — the customer's node raises the 3-choice
        // prompt from this. `connect_peer` above only *initiates* the WebRTC
        // connection; the daemon's acknowledged-delivery contract does the
        // rest: the Request is queued until the customer's link is up,
        // retransmitted across session rebuilds, and the reply resolves only
        // when the customer's node has actually taken the frame — the 2s
        // retransmit loop this used to need is the daemon's job now. The
        // send rides a spawned task so the dial returns immediately; a
        // delivery failure (TTL, terminal drop) marks the session ended so
        // the GUI's waiting badge tells the truth instead of hanging.
        let session_id = format!("cec-{}-{}", short_id(&customer), fresh_boot_id());
        let want_control = true;
        self.cec.set_session(&session_id, "requested");
        let request = allmystuff_cec_protocol::ControlMessage::Connect(
            allmystuff_cec_protocol::ConnectControl::Request {
                session_id: session_id.clone(),
                agent_name,
                want_control,
            },
        );
        {
            let mesh = self.clone();
            let net = network_id.clone();
            let peer = canonical.clone();
            let sid = session_id.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                match mesh
                    .cec_send_control_acked(&net, &peer, &request, CEC_CONNECT_TTL)
                    .await
                {
                    Ok(()) => {
                        tracing::info!(
                            "CEC Support: connect request delivered to {}",
                            short_id(&peer)
                        );
                    }
                    Err(e) => {
                        // "Stop trying" beat us here, or delivery genuinely
                        // lapsed — either way the waiting badge clears.
                        if !cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            tracing::warn!("cec connect request undelivered: {e}");
                        }
                        mesh.cec.set_session(&sid, "ended");
                        mesh.sink.emit(
                            "cec://session",
                            json!({ "session_id": sid, "state": "ended" }),
                        );
                    }
                }
            });
        }
        self.sink.emit("cec://peer", record.to_value());
        self.sink.emit(
            "cec://session",
            json!({ "session_id": session_id, "state": "requested" }),
        );
        self.emit_snapshot();
        Ok(json!({ "node": customer }))
    }

    /// `cec_cancel_dial` (technician): stop whatever the in-flight dial is
    /// trying — the discovery poll and the connect-request re-send loop both
    /// quit at the flag. The attempt row stays (the directory is permanent);
    /// a no-dial-in-flight cancel is a harmless no-op.
    pub async fn cec_cancel_dial(self: &Arc<Self>) -> Result<Value, String> {
        self.cec.cancel_dial();
        Ok(Value::Null)
    }

    /// `cec_pending` (customer): the inbound connect-requests awaiting a choice.
    pub async fn cec_pending(&self) -> Result<Value, String> {
        Ok(Value::Array(self.cec.pending()))
    }

    /// `cec_approve` (customer): record the chosen `scope` grant for `tech` and
    /// drive the mesh approval so the session goes Active. The grant is what the
    /// per-frame gate then consults, so the technician's screen/input rides the
    /// normal engine.
    pub async fn cec_approve(
        self: &Arc<Self>,
        tech: String,
        scope: String,
        session_id: String,
        want_control: bool,
    ) -> Result<Value, String> {
        let scope = crate::cec::parse_scope(&scope)?;
        let agent_name = self.cec.pending_agent_name(&tech);
        self.cec.approve(&tech, &agent_name, scope, want_control)?;
        self.cec.set_session(&session_id, "active");
        // Bind the session to this technician so the consent sweep can end
        // exactly their sessions when the grant later lapses.
        self.cec.bind_session(&session_id, &tech);
        let canonical = crate::cec::pubkey_part(&tech).to_string();
        if let Some(network_id) = self.network_for_peer(&tech) {
            self.cec_send_decision(
                network_id,
                canonical.clone(),
                allmystuff_cec_protocol::ControlMessage::Connect(
                    allmystuff_cec_protocol::ConnectControl::Approve {
                        session_id: session_id.clone(),
                        scope,
                    },
                ),
            );
        }
        // Carry `tech`/`agent_name` on the event (like the auto-approve path
        // does), so the customer GUI can bind the session — and its chat — to
        // this technician even when no `cec://request` preceded it.
        self.sink.emit(
            "cec://session",
            json!({
                "session_id": session_id,
                "state": "active",
                "tech": tech,
                "agent_name": agent_name,
            }),
        );
        self.cec_emit_grants();
        // Help arrived — an approved session withdraws the ask automatically,
        // so the customer never has to remember they raised their hand.
        if self.cec.asking_help() {
            if let Some(me) = self.resolve_local_id().await {
                self.cec_stop_asking_help(&me).await;
            }
        }
        Ok(Value::Null)
    }

    /// `cec_deny` (customer): decline a pending request (no grant recorded).
    pub async fn cec_deny(
        self: &Arc<Self>,
        tech: String,
        session_id: String,
    ) -> Result<Value, String> {
        self.cec.deny(&tech);
        self.cec.set_session(&session_id, "denied");
        let canonical = crate::cec::pubkey_part(&tech).to_string();
        if let Some(network_id) = self.network_for_peer(&tech) {
            self.cec_send_decision(
                network_id,
                canonical.clone(),
                allmystuff_cec_protocol::ControlMessage::Connect(
                    allmystuff_cec_protocol::ConnectControl::Deny {
                        session_id: session_id.clone(),
                        reason: "declined".into(),
                    },
                ),
            );
        }
        self.sink.emit(
            "cec://session",
            json!({ "session_id": session_id, "state": "denied" }),
        );
        Ok(Value::Null)
    }

    /// `cec_chat_send` (either side): send one live chat line to `peer` over the
    /// existing CEC session, then echo it into our own transcript. Chat is
    /// live-only — it rides the `cec.control` channel of a session that already
    /// exists, so with no known network/route to the peer there is nothing to
    /// carry it and this errs (the GUI only offers chat inside a live session).
    pub async fn cec_chat_send(
        self: &Arc<Self>,
        peer: String,
        text: String,
    ) -> Result<Value, String> {
        let canonical = crate::cec::pubkey_part(&peer).to_string();
        let network = self
            .network_for_peer(&peer)
            .ok_or_else(|| "no live CEC session with this peer to carry chat".to_string())?;
        // `from` is THIS node's own side of the session, which is what the far
        // GUI aligns the bubble from. We are the technician exactly when we
        // dialed this peer (they sit in our dialed-customer directory);
        // otherwise we are the customer who answered a request — the two sides a
        // CEC session ever has. The wire message's own `from` is never trusted
        // for the peer key, only for rendering.
        let from = if self.cec.is_dialed(&canonical) {
            allmystuff_cec_protocol::Role::Technician
        } else {
            allmystuff_cec_protocol::Role::Client
        };
        let msg = allmystuff_cec_protocol::ChatMessage {
            id: fresh_chat_id(),
            from,
            text,
            ts: crate::cec::now_secs(),
        };
        // Send chat over the very same peer-to-peer path as everything else on
        // the session (the connect handshake, presence, roster): direct over the
        // P2P link, the topology's forwarders only if there's no direct edge. The
        // acked/reliable path was the wrong tool — its per-peer outbox flushes
        // ONLY over a direct link and *parks* the frame when that link isn't up
        // (an ICE flap drops `data_channel_open`), so on the hub-shaped CEC area a
        // technician's line could sit unsent instead of just going P2P like the
        // rest of the session.
        self.cec_send_control(
            &network,
            &canonical,
            &allmystuff_cec_protocol::ControlMessage::Chat(msg.clone()),
        )
        .await?;
        tracing::info!(
            "cec chat out to {} ({} chars) on {network}",
            short_id(&canonical),
            msg.text.chars().count()
        );
        // Append + echo our own line so the sender's history is complete and the
        // GUI has ONE render path (the `cec://chat` event) for sent and received
        // alike.
        self.cec.push_chat(&canonical, msg.clone());
        self.emit_cec_chat(&canonical, &msg);
        Ok(json!({ "id": msg.id, "ts": msg.ts }))
    }

    /// `cec_chat_history` (either side): the persisted transcript with `peer`,
    /// oldest-first, as `{ messages: [ { id, from, text, ts } ] }` — what a GUI
    /// loads when it opens the chat pane. Both sent and received lines are here,
    /// since a sent line is echoed into the store on the way out.
    pub async fn cec_chat_history(self: &Arc<Self>, peer: String) -> Result<Value, String> {
        let canonical = crate::cec::pubkey_part(&peer).to_string();
        let messages: Vec<Value> = self
            .cec
            .chat_history(&canonical)
            .iter()
            .map(|m| serde_json::to_value(m).unwrap_or(Value::Null))
            .collect();
        Ok(json!({ "messages": messages }))
    }

    /// `cec_revoke` (customer): "Forget this technician" — drop every grant and
    /// tear the session down. The consent revoke bites the next privileged frame
    /// even if this wire End is lost.
    pub async fn cec_revoke(self: &Arc<Self>, tech: String) -> Result<Value, String> {
        let removed = self.cec.revoke(&tech)?;
        let canonical = crate::cec::pubkey_part(&tech).to_string();
        if let Some(network_id) = self.network_for_peer(&tech) {
            self.cec_send_decision(
                network_id,
                canonical.clone(),
                allmystuff_cec_protocol::ControlMessage::Connect(
                    allmystuff_cec_protocol::ConnectControl::End {
                        session_id: String::new(),
                    },
                ),
            );
        }
        // Tear down any live routes with the technician, exactly like forgetting
        // a node.
        self.teardown_and_drop_peer(&canonical).await;
        self.cec_emit_grants();
        Ok(json!({ "revoked": removed }))
    }

    /// `cec_grants` (customer): the live consent grants.
    pub async fn cec_grants(&self) -> Result<Value, String> {
        Ok(Value::Array(self.cec.grants()))
    }

    /// `cec_dialed` (technician): the customers this node has dialed, for the
    /// CEC tab's "Active connections" list. Dialed customers are ordinary graph
    /// peers — this is CEC state, not a graph grouping.
    pub async fn cec_dialed(&self) -> Result<Value, String> {
        // The dialed directory is durable — it survives node restarts and grant
        // expiry, so a technician keeps every machine they've serviced. Reconcile
        // each entry's `online` against the daemon's live peer set so the tab
        // shows which stored machines are reachable right now (and can be
        // reconnected — an expired grant just re-prompts the customer).
        let records = self.cec.dialed_records();
        let area = allmystuff_cec_protocol::HELP_NETWORK_ID;
        // The tab polls this, so fetch the peer set ONCE and reconcile the whole
        // directory against it — not a round-trip per serviced machine.
        let reachable = self.cec_reachable_set(area).await;
        let mut out = Vec::with_capacity(records.len());
        for r in records {
            let canonical = crate::cec::pubkey_part(&r.node).to_string();
            let online = reachable.contains(&canonical);
            if online != r.online {
                self.cec.set_customer_online(&canonical, online);
            }
            let mut v = r.to_value();
            v["online"] = json!(online);
            out.push(v);
        }
        Ok(Value::Array(out))
    }

    /// `forget_node` — an **app-wide** feature on every node's gear, not a CEC
    /// one: drop `node` from the graph + roster and tear its live routes down.
    /// Any AllMyStuff node can forget any peer this way. When the peer happens to
    /// be a CEC customer this technician dialed (or a CEC technician this
    /// customer approved), [`Self::cec_forget_cleanup`] also unwinds that CEC
    /// state — but the core teardown is identical for every node.
    pub async fn forget_node(self: &Arc<Self>, node: String) -> Result<Value, String> {
        let canonical = crate::cec::pubkey_part(&node).to_string();
        // App-wide: tear down live routes to the peer and drop it from the
        // roster on whatever network it was reachable on.
        self.teardown_and_drop_peer(&canonical).await;
        // CEC add-on: a no-op for an ordinary node.
        self.cec_forget_cleanup(&node, &canonical).await;
        self.emit_snapshot();
        Ok(json!({ "forgotten": node }))
    }

    // ---- CEC internals ------------------------------------------------

    /// CEC-specific cleanup layered onto [`Self::forget_node`] — a no-op for an
    /// ordinary (non-CEC) peer. Revokes any grant for `node` (customer side)
    /// and drops the dialed record (technician side). Nothing network-level:
    /// sessions ride the shared area, which is never torn down for one
    /// forgotten peer (the pinned dial died with `teardown_and_drop_peer`).
    async fn cec_forget_cleanup(self: &Arc<Self>, node: &str, canonical: &str) {
        let _ = self.cec.forget_dialed(canonical);
        // Customer side: forgetting a technician is also a revoke.
        let _ = self.cec.revoke(node);
        self.cec_emit_grants();
    }

    /// Join a Silent mesh via the daemon and re-subscribe this session's
    /// channels onto it.
    async fn cec_join_silent(
        self: &Arc<Self>,
        network_id: &str,
        config: Value,
    ) -> Result<(), String> {
        let resp = self
            .client
            .request(&Request::NetworkAdd {
                config: config.clone(),
            })
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok {
            let err = resp.error.unwrap_or_default();
            // The daemon persists CEC rooms and auto-rejoins them at startup, so
            // a re-host (or re-dial) hits "config id already in use" — that's
            // success, not failure: we ARE on the room. Treating it as an error
            // made `cec_start_hosting` bail before advertising presence (the
            // customer then never shows up as a host) and blocked a re-dial from
            // refreshing the room. Any *other* failure is still real.
            if err.contains("already in use") || err.contains("already joined") {
                // But a persisted room keeps its persisted *config* — a help
                // room saved as Silent by an older build would still never
                // auto-dial. Push the current config over it so kind changes
                // (Silent -> Open) heal in place; a failed update degrades to
                // the old behavior rather than failing the join.
                let _ = self
                    .client
                    .request(&Request::NetworkUpdate { config })
                    .await;
                self.sync_networks().await;
                return Ok(());
            }
            return Err(if err.is_empty() {
                format!("couldn't join the CEC mesh {network_id}")
            } else {
                err
            });
        }
        self.sync_networks().await;
        Ok(())
    }

    /// Whether `canonical` (bare pubkey) is currently a peer on `network_id`,
    /// per the daemon's `PeersList` — the live-reachability check behind a stored
    /// customer's online dot. A daemon error or a network we've left reads as
    /// offline (best-effort; the row stays, it just shows unreachable).
    /// The set of canonical (bare-pubkey) ids **connected** on `network_id`
    /// right now. "Reachable" is the `active`/`shelved` cut the graph reads
    /// online from — an offline / sighted / handshaking row is a peer the
    /// daemon remembers, not one a technician can reach, so it must not read as
    /// online. (The old per-id check ignored status, so a still-listed but
    /// offline customer read "online" until the app restarted.)
    async fn cec_reachable_set(&self, network_id: &str) -> std::collections::HashSet<String> {
        let mut set = std::collections::HashSet::new();
        let Ok(resp) = self
            .client
            .request(&Request::PeersList {
                network: network_id.to_string(),
            })
            .await
        else {
            return set;
        };
        let Some(peers) = resp
            .data
            .as_ref()
            .and_then(|d| d.get("peers"))
            .and_then(|p| p.as_array())
        else {
            return set;
        };
        for p in peers {
            if !status_is_reachable(p.get("status").and_then(|v| v.as_str())) {
                continue;
            }
            if let Some(id) = p.get("device_id").and_then(|v| v.as_str()) {
                set.insert(crate::cec::pubkey_part(id).to_string());
            }
        }
        set
    }

    /// Fire a customer *decision* (Approve / Deny / End) at a technician
    /// under the acked contract, without blocking the GUI op that made
    /// the decision: the send is spawned, queued daemon-side until the
    /// technician's link is up, retransmitted across rebuilds, and a
    /// terminal delivery failure is logged loudly — the one case left is
    /// a technician gone past the TTL, who re-dials and (for standing
    /// grants) auto-approves without the customer doing anything.
    fn cec_send_decision(
        self: &Arc<Self>,
        network: String,
        peer: String,
        message: allmystuff_cec_protocol::ControlMessage,
    ) {
        let mesh = self.clone();
        crate::spawn(async move {
            if let Err(e) = mesh
                .cec_send_control_acked(
                    &network,
                    &peer,
                    &message,
                    std::time::Duration::from_secs(30),
                )
                .await
            {
                tracing::warn!(
                    "cec decision undelivered to {} (they can re-dial; standing grants auto-approve): {e}",
                    short_id(&peer)
                );
            }
        });
    }

    /// Send one CEC [`ControlMessage`](allmystuff_cec_protocol::ControlMessage)
    /// under the daemon's acknowledged-delivery contract: queued until the
    /// peer's link is up, retransmitted across session rebuilds, resolved
    /// when the peer's node has taken the frame (or errs at `ttl`). The
    /// client read deadline is sized past the TTL so the daemon's honest
    /// timeout answer always wins over the socket's.
    async fn cec_send_control_acked(
        &self,
        network: &str,
        peer: &str,
        message: &allmystuff_cec_protocol::ControlMessage,
        ttl: std::time::Duration,
    ) -> Result<(), String> {
        let payload = serde_json::to_value(message).map_err(|e| e.to_string())?;
        let resp = self
            .client
            .request_with_timeout(
                &Request::ChannelSendReliable {
                    network: network.to_string(),
                    channel: allmystuff_cec_protocol::CHANNEL_CONTROL.to_string(),
                    peer: crate::cec::pubkey_part(peer).to_string(),
                    payload,
                    ttl_ms: ttl.as_millis() as u64,
                },
                ttl + std::time::Duration::from_secs(5),
            )
            .await
            .map_err(|e| e.to_string())?;
        if resp.ok {
            Ok(())
        } else {
            Err(resp
                .error
                .unwrap_or_else(|| "cec acked control send failed".into()))
        }
    }

    /// Send one CEC [`ControlMessage`](allmystuff_cec_protocol::ControlMessage)
    /// on the `cec.control` channel to `peer` (bare pubkey) on `network`.
    async fn cec_send_control(
        &self,
        network: &str,
        peer: &str,
        message: &allmystuff_cec_protocol::ControlMessage,
    ) -> Result<(), String> {
        let payload = serde_json::to_value(message).map_err(|e| e.to_string())?;
        let resp = self
            .client
            .request(&Request::ChannelSendTo {
                network: network.to_string(),
                channel: allmystuff_cec_protocol::CHANNEL_CONTROL.to_string(),
                peer: crate::cec::pubkey_part(peer).to_string(),
                payload,
            })
            .await
            .map_err(|e| e.to_string())?;
        if resp.ok {
            Ok(())
        } else {
            Err(resp
                .error
                .unwrap_or_else(|| "cec control send failed".into()))
        }
    }

    /// Advertise a [`SupportPresence`](allmystuff_cec_protocol::SupportPresence)
    /// beacon on the CEC presence channel, so a technician on this room can find
    /// this customer. `available: false` is the explicit withdrawal — on the
    /// global help room it clears this customer from technician caches at once
    /// instead of waiting out the beacon TTL.
    /// Broadcast this node's CEC presence on a room, returning how many live
    /// peers the daemon actually dispatched it to — 0 means the beacon went
    /// into the void (no wire up yet), which is exactly what the customer's
    /// "raising your hand…" indicator needs to know.
    async fn cec_broadcast_presence(&self, network: &str, me: &str, available: bool) -> usize {
        let mut presence = allmystuff_cec_protocol::SupportPresence::new(
            me.to_string(),
            allmystuff_cec_protocol::Role::Client,
        );
        presence.available = available;
        presence.label = self
            .state
            .lock()
            .profile
            .as_ref()
            .map(|p| p.label.clone())
            .unwrap_or_default();
        // Cached: the hostname never changes within a run, and this fires on
        // every help re-beacon — a full scan() here meant a round of
        // PowerShell probes per beacon on Windows.
        static HOSTNAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        presence.hostname = HOSTNAME
            .get_or_init(|| allmystuff_inventory::scan().host.hostname)
            .clone();
        presence.sent_at = unix_now_ms() / 1000;
        let payload = match serde_json::to_value(&presence) {
            Ok(v) => v,
            Err(_) => return 0,
        };
        match self
            .client
            .request(&Request::ChannelSendAll {
                network: network.to_string(),
                channel: allmystuff_cec_protocol::CHANNEL_PRESENCE.to_string(),
                payload,
            })
            .await
        {
            Ok(resp) => resp
                .data
                .as_ref()
                .and_then(|d| d.get("dispatched_to"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize,
            Err(_) => 0,
        }
    }

    /// The best-known (label, hostname) for a peer by canonical id, from the
    /// live session — the identity pair the CEC cards spell out so the
    /// technician's row and the customer's own app match word for word.
    fn cec_peer_ident(&self, canonical: &str) -> Option<(String, String)> {
        let st = self.state.lock();
        let session = st.session.as_ref()?;
        let ident = session
            .peers()
            .find(|p| crate::cec::pubkey_part(p.node.as_str()) == canonical)
            .map(|p| (p.label.clone(), p.hostname.clone()))
            .filter(|(l, h)| !l.is_empty() || !h.is_empty());
        ident
    }

    /// Tear down every live route with a peer (by canonical id) and drop it from
    /// the daemon roster on whatever network it was reachable on — the shared
    /// body of the app-wide "Forget this node" and CEC's "Forget this technician".
    async fn teardown_and_drop_peer(self: &Arc<Self>, canonical: &str) {
        let route_ids: Vec<String> = {
            let st = self.state.lock();
            match st.session.as_ref() {
                Some(session) => session
                    .routes()
                    .filter(|r| crate::cec::pubkey_part(r.peer.as_str()) == canonical)
                    .map(|r| r.route.id.clone())
                    .collect(),
                None => Vec::new(),
            }
        };
        for id in route_ids {
            let _ = self.disconnect(id).await;
        }
        if let Some(network) = self.network_for_peer(canonical) {
            let _ = self
                .client
                .request(&Request::RosterRemove {
                    network,
                    device_id: canonical.to_string(),
                })
                .await;
        }
    }

    /// Emit the customer's current grant list (`cec://grants`).
    fn cec_emit_grants(&self) {
        self.sink
            .emit("cec://grants", json!({ "grants": self.cec.grants() }));
    }

    /// Handle one inbound CEC control message (the `cec.control` channel).
    /// Customer side: a `Request` raises the 3-choice prompt (`cec://request`);
    /// technician side: an `Approve`/`Deny`/`End` moves the session.
    async fn handle_cec_control(self: &Arc<Self>, from: String, network: String, payload: Value) {
        let msg: allmystuff_cec_protocol::ControlMessage = match serde_json::from_value(payload) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("dropping CEC control from {}: {e}", short_id(&from));
                return;
            }
        };
        // Chat rides the same control channel as the connect handshake but is a
        // pure transcript event, not part of the approve/deny state machine —
        // attribute it to the authenticated sender, store it, surface it, and
        // return before the Connect dispatch below (which consumes `msg`).
        if let allmystuff_cec_protocol::ControlMessage::Chat(chat) = &msg {
            self.on_cec_chat_in(&from, chat).await;
            return;
        }
        if let allmystuff_cec_protocol::ControlMessage::Connect(connect) = msg {
            tracing::info!(
                "cec connect message from {}: {}",
                short_id(&from),
                match &connect {
                    allmystuff_cec_protocol::ConnectControl::Request { session_id, .. } =>
                        format!("Request session={session_id}"),
                    allmystuff_cec_protocol::ConnectControl::Approve { session_id, .. } =>
                        format!("Approve session={session_id}"),
                    allmystuff_cec_protocol::ConnectControl::Deny { session_id, .. } =>
                        format!("Deny session={session_id}"),
                    allmystuff_cec_protocol::ConnectControl::End { session_id } =>
                        format!("End session={session_id}"),
                    other => format!("{other:?}"),
                }
            );
            match connect {
                allmystuff_cec_protocol::ConnectControl::Request {
                    session_id,
                    agent_name,
                    want_control,
                } => {
                    // The technician retransmits its Request every 2s until it
                    // sees an answer — because a single send can be dropped
                    // before the data channel is up (the very race the Request
                    // retransmit was added to beat). Our *reply* can be dropped
                    // the same way, so each incoming beat is our cue to re-assert
                    // our current decision, answered on the network it arrived
                    // on. Without this, an approval whose one Approve was dropped
                    // leaves the technician re-requesting forever (the customer
                    // re-prompted every beat) and never seeing the session.
                    match self.cec.session_state(&session_id).as_deref() {
                        Some("active") => {
                            // Already approved; re-send the Approve. The scope is
                            // cosmetic to the technician (its Approve handler only
                            // moves the session to active) — default it if the
                            // grant is gone.
                            let scope = self
                                .cec
                                .active_scope_for(&from)
                                .unwrap_or(allmystuff_cec_protocol::ApprovalScope::Once);
                            let _ = self
                                .cec_send_control(
                                    &network,
                                    &from,
                                    &allmystuff_cec_protocol::ControlMessage::Connect(
                                        allmystuff_cec_protocol::ConnectControl::Approve {
                                            session_id,
                                            scope,
                                        },
                                    ),
                                )
                                .await;
                        }
                        Some("denied") => {
                            // Already declined; re-send the Deny so the tech's
                            // dial loop can stop instead of re-prompting us.
                            let _ = self
                                .cec_send_control(
                                    &network,
                                    &from,
                                    &allmystuff_cec_protocol::ControlMessage::Connect(
                                        allmystuff_cec_protocol::ConnectControl::Deny {
                                            session_id,
                                            reason: "declined".into(),
                                        },
                                    ),
                                )
                                .await;
                        }
                        _ => {
                            // A still-valid standing grant (3-hours / Forever)
                            // auto-approves the reconnect — the customer set it so
                            // they wouldn't be re-asked, which is what lets a
                            // technician reuse a connection without the customer
                            // doing anything. An expired or absent grant (or an
                            // "Once" that never persisted) falls through to the
                            // prompt, so reconnecting to a lapsed machine pops the
                            // box again, exactly like the first time.
                            // Standing grants only: an "Approve Once" covers
                            // exactly its own session, so a *new* dial from a
                            // once-approved technician re-prompts instead of
                            // silently reattaching off the leftover grant.
                            if let Some(scope) = self.cec.standing_scope_for(&from) {
                                // Each dial mints a fresh session id — end any
                                // older live session with this same technician
                                // first, so a re-dial supersedes rather than
                                // piling "X is viewing your screen" rows up.
                                for stale in self.cec.end_other_sessions(&session_id) {
                                    self.sink.emit(
                                        "cec://session",
                                        json!({ "session_id": stale, "state": "ended" }),
                                    );
                                }
                                self.cec.set_session(&session_id, "active");
                                // Bind the auto-approved session to this
                                // technician so the consent sweep can end it
                                // when the standing grant later lapses.
                                self.cec.bind_session(&session_id, &from);
                                if let Some(rec) =
                                    self.cec.touch_dialed(crate::cec::pubkey_part(&from))
                                {
                                    self.sink.emit("cec://peer", rec.to_value());
                                }
                                self.sink.emit(
                                    "cec://session",
                                    json!({
                                        "session_id": session_id.clone(),
                                        "state": "active",
                                        "agent_name": agent_name.clone(),
                                        "tech": from.clone(),
                                    }),
                                );
                                tracing::info!(
                                    "cec auto-approve: standing grant covers {} — replying Approve session={session_id}",
                                    short_id(&from)
                                );
                                // Acked: a new-era technician sends its Request
                                // exactly once, so this reply must survive drops
                                // on its own — the daemon queues, retransmits
                                // across rebuilds, and only gives up at the TTL.
                                // (An old technician re-beats; its duplicate
                                // Requests just re-spawn cheap dedup'd replies.)
                                self.cec_send_decision(
                                    network.clone(),
                                    from.clone(),
                                    allmystuff_cec_protocol::ControlMessage::Connect(
                                        allmystuff_cec_protocol::ConnectControl::Approve {
                                            session_id,
                                            scope,
                                        },
                                    ),
                                );
                                // Help arrived (a standing grant answered the
                                // beacon) — withdraw the ask, same as an
                                // explicit approve does.
                                if self.cec.asking_help() {
                                    if let Some(me) = self.resolve_local_id().await {
                                        self.cec_stop_asking_help(&me).await;
                                    }
                                }
                            } else {
                                // Undecided: raise the prompt on the first beat and
                                // refresh the pending record on later ones — but
                                // don't re-emit `cec://request`, or the customer's
                                // approval dialog is spammed once every 2s.
                                let already = self.cec.has_pending_session(&session_id);
                                let verification_code =
                                    crate::cec::verification_code(&from, &session_id);
                                let req = crate::cec::PendingRequest {
                                    tech: from.clone(),
                                    agent_name: agent_name.clone(),
                                    want_control,
                                    session_id: session_id.clone(),
                                    verification_code: verification_code.clone(),
                                };
                                self.cec.record_pending(req);
                                if !already {
                                    self.sink.emit(
                                        "cec://request",
                                        json!({
                                            "tech": from,
                                            "agent_name": agent_name,
                                            "want_control": want_control,
                                            "session_id": session_id,
                                            "verification_code": verification_code,
                                        }),
                                    );
                                }
                            }
                        }
                    }
                }
                allmystuff_cec_protocol::ConnectControl::Approve { session_id, .. } => {
                    self.cec.set_session(&session_id, "active");
                    // The customer just approved — this connection is now in
                    // active use. Stamp its `last_used` (and re-emit the peer so
                    // the CEC tab's time-since refreshes) so the technician's
                    // stale-connection cleanup reflects real activity.
                    if let Some(rec) = self.cec.touch_dialed(crate::cec::pubkey_part(&from)) {
                        self.sink.emit("cec://peer", rec.to_value());
                    }
                    self.sink.emit(
                        "cec://session",
                        json!({ "session_id": session_id, "state": "active" }),
                    );
                }
                allmystuff_cec_protocol::ConnectControl::Deny { session_id, .. } => {
                    self.cec.set_session(&session_id, "denied");
                    self.sink.emit(
                        "cec://session",
                        json!({ "session_id": session_id, "state": "denied" }),
                    );
                }
                allmystuff_cec_protocol::ConnectControl::End { session_id } => {
                    self.cec.set_session(&session_id, "ended");
                    // The session an "Approve Once" covered is over — retire it
                    // now, so a later console open or re-dial has to earn a
                    // fresh approval instead of riding a leftover in-memory
                    // grant. Standing grants (3h / Forever) survive: outliving
                    // sessions is their whole point.
                    if self.cec.retire_once(&from) {
                        self.cec_emit_grants();
                    }
                    self.sink.emit(
                        "cec://session",
                        json!({ "session_id": session_id, "state": "ended" }),
                    );
                }
                allmystuff_cec_protocol::ConnectControl::Unknown => {}
            }
        }
    }

    /// Handle an inbound [`ChatMessage`](allmystuff_cec_protocol::ChatMessage)
    /// off the `cec.control` channel: attribute it to the **authenticated**
    /// sender (`from`) — never the message's self-declared `chat.from`, which is
    /// only the Role the far side renders as — append it to that peer's
    /// transcript, and surface it to the GUI. The sender's own line is echoed by
    /// [`Self::cec_chat_send`], so this path covers received lines only.
    async fn on_cec_chat_in(
        self: &Arc<Self>,
        from: &str,
        chat: &allmystuff_cec_protocol::ChatMessage,
    ) {
        let canonical = pubkey_part(from).to_string();
        tracing::info!(
            "cec chat in from {} ({} chars)",
            short_id(&canonical),
            chat.text.chars().count()
        );
        self.cec.push_chat(&canonical, chat.clone());
        self.emit_cec_chat(&canonical, chat);
    }

    /// Emit the `cec://chat` GUI event for one message on `peer`'s transcript —
    /// the single render path for both an inbound receive and the echo of a sent
    /// line, so the GUI has ONE way to draw a chat bubble. The message object is
    /// the wire [`ChatMessage`](allmystuff_cec_protocol::ChatMessage) serialized
    /// as-is (`from` → `"client"` / `"technician"`), so the event shape can never
    /// drift from the protocol type.
    fn emit_cec_chat(&self, peer: &str, chat: &allmystuff_cec_protocol::ChatMessage) {
        let message = serde_json::to_value(chat).unwrap_or(Value::Null);
        self.sink
            .emit("cec://chat", json!({ "peer": peer, "message": message }));
    }

    // ---- shares (durable, person-scoped grants) -----------------------
    //
    // The GUI resolves the person + node and hands them down; the node is the
    // source of truth (enforcement lives here) and the next [`Mesh::snapshot`]
    // reflects the change. These persist *my* policy so it survives a restart
    // **and** tell the peer over the control channel, so a share is a mutual
    // fact rather than one-sided local policy — what made it no better than a
    // room. The send is best-effort: the durable record is written first, so a
    // peer that's offline now just isn't notified yet (the local policy still
    // holds, and a later phase re-asserts on reconnect).

    /// Record an **outbound** grant — what this person may do with my stuff —
    /// persist it, and offer my full current grant set to their device.
    pub async fn share_grant(
        &self,
        person: Person,
        node: NodeId,
        grant: Grant,
    ) -> Result<(), String> {
        self.shares.grant(&person, &node, grant);
        self.emit_snapshot();
        self.send_share_invite(&person, &node).await
    }

    /// Tell `node` the full set of grants this person currently holds from me.
    /// Sent whole because the peer records inbound by **replacement**, so the
    /// complete set is the authoritative "here's everything you may do".
    async fn send_share_invite(&self, person: &Person, node: &NodeId) -> Result<(), String> {
        let grants = self.shares.out_grants_for(&person.id);
        let msg = ControlMessage::Share(ShareControl::Invite {
            from: self.local_person(),
            grants,
        });
        self.send_control(node.as_str(), &msg).await
    }

    /// Revoke a grant by its (content-derived) id from a person's share, and
    /// tell every device they bring to drop it too (revocation is unilateral —
    /// the content-derived id names the same grant on both ends).
    pub async fn share_revoke(&self, person: PersonId, grant_id: String) -> Result<(), String> {
        self.shares.revoke(&person, &grant_id);
        self.emit_snapshot();
        let mut last_err = None;
        for node in self.shares.nodes_for(&person) {
            let msg = ControlMessage::Share(ShareControl::Revoke {
                grant_id: grant_id.clone(),
            });
            if let Err(e) = self.send_control(node.as_str(), &msg).await {
                last_err = Some(e);
            }
        }
        last_err.map_or(Ok(()), Err)
    }

    /// Stop sharing with a person entirely — drop the whole durable record and
    /// revoke each outbound grant on their devices (captured before the drop).
    pub async fn share_stop(&self, person: PersonId) -> Result<(), String> {
        let nodes = self.shares.nodes_for(&person);
        let grant_ids: Vec<String> = self
            .shares
            .out_grants_for(&person)
            .into_iter()
            .map(|g| g.id)
            .collect();
        self.shares.stop_sharing(&person);
        self.emit_snapshot();
        for node in &nodes {
            for grant_id in &grant_ids {
                let msg = ControlMessage::Share(ShareControl::Revoke {
                    grant_id: grant_id.clone(),
                });
                let _ = self.send_control(node.as_str(), &msg).await;
            }
        }
        Ok(())
    }

    /// This machine's owner-or-self as a graph [`Person`] — the identity an
    /// outbound [`ShareControl::Invite`] carries. Keyed `person:<pubkey>` to
    /// mirror the GUI's `person:<owner ?? self>`, so both ends agree on "me".
    fn local_person(&self) -> Person {
        let me = self.local_node_id().unwrap_or_default();
        let owner = self.ownership.owner().unwrap_or_else(|| me.clone());
        Person {
            id: format!("person:{}", pubkey_part(&owner)).into(),
            name: self
                .profile_label()
                .unwrap_or_else(|| me.chars().take(10).collect()),
        }
    }

    /// The [`Person`] we attribute an inbound share to — keyed by the
    /// **authenticated** sender's pubkey, *never* the self-asserted body id.
    /// This is the load-bearing trust rule: an inbound offer can only ever bind
    /// the sender's own node into the sender's own share, so a peer can't slip
    /// its node into someone else's person (which later outbound enforcement
    /// would otherwise trust). The body supplies only a display name.
    fn peer_person(&self, from: &NodeId, name: Option<&str>) -> Person {
        let display = name
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.peer_label(from));
        Person {
            id: format!("person:{}", pubkey_part(from.as_str())).into(),
            name: display,
        }
    }

    /// Apply an inbound share-control message. Unlike app-control or a
    /// privileged offer, this is **not** gated on `sender_may_control`: a share
    /// is person-to-person, so the sharer is never the recipient's owner/fleet.
    /// The mesh's ed25519 handshake already authenticates `from`; recording
    /// what they offer is safe because an *inbound* grant only ever widens what
    /// *I* may pull from *them*, never what they may do to me (that direction is
    /// my own outbound grant, minted only by my explicit action).
    async fn handle_share(&self, from: NodeId, message: ShareControl) {
        match message {
            ShareControl::Invite { from: body, grants } => {
                let person = self.peer_person(&from, Some(&body.name));
                self.shares.record_inbound(&person, &from, grants);
                self.emit_snapshot();
                self.sink.emit(
                    "allmystuff://share",
                    json!({ "from": from.to_string(), "kind": "invite", "person": person.name }),
                );
                // Acknowledge, carrying any grants I already extend back — so
                // sharing can be mutual in one round trip (empty if I've granted
                // them nothing; the ack never *mints* an outbound grant).
                let back = self.shares.out_grants_for(&person.id);
                let reply = ControlMessage::Share(ShareControl::Accept { grants: back });
                if let Err(e) = self.send_control(from.as_str(), &reply).await {
                    tracing::warn!("couldn't ack share from {}: {e}", short_id(from.as_str()));
                }
            }
            ShareControl::Accept { grants } => {
                let person = self.peer_person(&from, None);
                self.shares.record_inbound(&person, &from, grants);
                self.emit_snapshot();
                self.sink.emit(
                    "allmystuff://share",
                    json!({ "from": from.to_string(), "kind": "accept", "person": person.name }),
                );
            }
            ShareControl::Decline => {
                tracing::info!("share declined by {}", short_id(from.as_str()));
                self.sink.emit(
                    "allmystuff://share",
                    json!({ "from": from.to_string(), "kind": "decline" }),
                );
            }
            ShareControl::Revoke { grant_id } => {
                let person = self.peer_person(&from, None);
                self.shares.revoke(&person.id, &grant_id);
                self.emit_snapshot();
                self.sink.emit(
                    "allmystuff://share",
                    json!({ "from": from.to_string(), "kind": "revoke" }),
                );
            }
            // A share-control kind a newer build introduced — nothing to do.
            ShareControl::Unknown => {}
        }
    }

    /// Commit one exact StopMedia while the caller owns the route lifecycle
    /// lock. Keeping the body non-async lets inbound teardown mutate Session
    /// and retire its resources in one critical section, including legacy
    /// routes whose wire incarnation is `None`.
    fn apply_stop_media_locked(self: &Arc<Self>, id: String, incarnation: Option<String>) {
        let owns_resources = {
            let mut active = self.active_media_incarnations.lock();
            let matches = active
                .get(&id)
                .is_some_and(|running| running == &incarnation);
            if matches {
                active.remove(&id);
            }
            matches
        };
        if !owns_resources {
            tracing::warn!(
                "stale StopMedia for {id} ignored because its route incarnation does not own the local resources"
            );
            return;
        }
        self.state
            .lock()
            .route_networks
            .remove(&(id.clone(), incarnation.clone()));
        let stop_state = self
            .state
            .lock()
            .session
            .as_ref()
            .and_then(|session| session.route(&id))
            .map(|route| format!("{:?}", route.state))
            .unwrap_or_else(|| "absent".into());
        tracing::info!("session StopMedia committing for {id} (route state {stop_state})");
        self.audio.stop(&id);
        self.video.stop(&id);
        let policy_plans = {
            let serial = self.video_policy_apply_serial.lock();
            let plans = self.media_policy.lock().remove_route(&id);
            self.apply_video_policy_caps_locked(&plans, &serial);
            plans
        };
        self.queue_effective_plans(policy_plans);
        let reconnecting = self.desired_routes.lock().contains_key(&id);
        if reconnecting {
            self.video_watchers.lock().reset_route_for_reconnect(&id);
        } else {
            self.video_watchers.lock().remove(&id);
            self.requested_video_tunes.lock().remove(&id);
        }
        self.release_video_lanes(&id);
        self.release_audio_lanes(&id);
        self.injector.release_route(&id);
        self.input_in_seq
            .lock()
            .retain(|(route_id, _), _| route_id != &id);
        self.terminal.detach(&id);
        self.term_pumps.lock().remove(&id);
        self.term_rx_seq.lock().remove(&id);
        self.term_in_seq.lock().remove(&id);
        self.files.stop(&id);
        self.sites.stop_route(&id);
        self.drop_downloads(&id);
    }

    async fn process_effects(self: &Arc<Self>, effects: Vec<Effect>) {
        for e in effects {
            match e {
                Effect::Send { peer, message } => {
                    // Replies ride best-effort; the failure is already logged.
                    let _ = self.send_control(&peer.to_string(), &message).await;
                }
                Effect::StartMedia { route, incarnation } => {
                    let _lifecycle = self.lock_route_lifecycle(&route.id).await;
                    let needs_inbound_generation = self
                        .local_node_id()
                        .is_some_and(|local| needs_inbound_video_generation(&route, &local));
                    if matches!(route.media, MediaKind::Display | MediaKind::Video) {
                        let still_active = self
                            .state
                            .lock()
                            .session
                            .as_ref()
                            .and_then(|s| s.route(&route.id))
                            .is_some_and(|live| {
                                live.state == RouteState::Active
                                    && live.route == route
                                    && live.incarnation == incarnation
                            });
                        if !still_active {
                            tracing::warn!(
                                "stale StartMedia for {} abandoned after video bring-up wait — route is no longer the active incarnation",
                                route.id
                            );
                            continue;
                        }
                        self.video_in.lock().clear_route(&route.id);
                        self.note_video_route_started(&route);
                    }
                    if !self.claim_media_incarnation_if_active(&route.id, incarnation.as_deref()) {
                        tracing::warn!(
                            "stale StartMedia for {} ignored after its route lifetime changed",
                            route.id
                        );
                        continue;
                    }
                    if needs_inbound_generation {
                        self.begin_video_generation(&route.id);
                    }
                    if route.media == MediaKind::Input {
                        self.injector.activate_route(&route.id);
                    }
                    self.start_media(&route)
                }
                Effect::RefreshMedia {
                    route_id,
                    incarnation,
                } => {
                    if self.route_is_active_incarnation(&route_id, incarnation.as_deref()) {
                        self.video.force_idr(&route_id);
                    }
                }
                Effect::TuneMedia {
                    route_id,
                    incarnation,
                    max_edge,
                    bitrate,
                    fps,
                    game,
                    mode,
                    ext,
                } => {
                    if !self.route_is_active_incarnation(&route_id, incarnation.as_deref()) {
                        continue;
                    }
                    // Keep the controller mutation and every resulting route
                    // retune in one ordered transaction. Generation retries
                    // inside VideoBridge handle capture churn, while this
                    // outer gate orders values from competing policy sources.
                    let _policy_serial = self.video_policy_apply_serial.lock();
                    let legacy_dials_absent = max_edge.is_none()
                        && bitrate.is_none()
                        && fps.is_none()
                        && !game
                        && mode.is_none();
                    let mut policy_cap = None;
                    let mut policy_auto_resolution = false;
                    let mut plans_to_echo = Vec::new();
                    let mut audio_profile_update = None;
                    let mut effective_video_mode = None;
                    let mut election_only = false;
                    if let Some(envelope) = PolicyEnvelope::from_ext(&ext) {
                        match envelope.payload {
                            PolicyPayload::Effective { plan } => {
                                // Streamer → viewer answer. Cache it for the
                                // effective panel; never reflect it back or
                                // interpret it as a local encoder command.
                                if plan.route_id != route_id {
                                    tracing::warn!(
                                        "ignoring media-policy effective route mismatch: Tune {route_id}, plan {}",
                                        plan.route_id
                                    );
                                    continue;
                                }
                                let peer = self.route_peer(&route_id);
                                let mode = plan.effective_mode;
                                self.media_policy.lock().record_effective(plan);
                                if let Some(peer) = peer {
                                    self.apply_audio_profile_for_peer(&peer, mode);
                                }
                                continue;
                            }
                            PolicyPayload::Request {
                                route_id: ext_route,
                                request,
                                capabilities,
                            } if ext_route == route_id => {
                                if let Some(peer) = self.route_peer(&route_id) {
                                    let lan = self.route_link_class(&route_id, &peer)
                                        == crate::video::LinkClass::Lan;
                                    election_only = request.priority_only
                                        || (request.priority
                                            && request.peer_cap_bps.is_none()
                                            && request.route_cap_bps.is_none()
                                            && legacy_dials_absent);
                                    plans_to_echo = if election_only {
                                        // OS/window focus is a scheduler hint,
                                        // not a quality request. Preserve the
                                        // aggregate cap and every route dial.
                                        self.media_policy.lock().elect_priority(&route_id)
                                    } else {
                                        self.media_policy.lock().apply_request(
                                            pubkey_part(&peer),
                                            &route_id,
                                            request,
                                            capabilities,
                                            lan,
                                        )
                                    };
                                    if let Some(plan) = plans_to_echo
                                        .iter()
                                        .find(|plan| plan.priority)
                                        .or_else(|| plans_to_echo.first())
                                    {
                                        audio_profile_update =
                                            Some((peer.clone(), plan.effective_mode));
                                    }
                                    if let Some(plan) =
                                        plans_to_echo.iter().find(|plan| plan.route_id == route_id)
                                    {
                                        effective_video_mode = Some(plan.effective_mode);
                                        policy_cap =
                                            Some(plan.route_budget_bps.min(u64::from(u32::MAX))
                                                as u32);
                                        policy_auto_resolution = plan.auto_resolution;
                                    }
                                }
                            }
                            PolicyPayload::Request {
                                route_id: ext_route,
                                ..
                            } => {
                                tracing::warn!(
                                    "ignoring media-policy route mismatch: Tune {route_id}, ext {ext_route}"
                                );
                            }
                        }
                    }
                    if election_only {
                        self.video
                            .apply_policy_cap(&route_id, policy_cap, policy_auto_resolution);
                    } else {
                        self.video.retune_dials(
                            &route_id,
                            max_edge,
                            bitrate,
                            fps,
                            game,
                            resolved_encoder_mode(mode.as_deref(), effective_video_mode),
                            policy_cap,
                            policy_auto_resolution,
                        );
                    }
                    for plan in &plans_to_echo {
                        if plan.route_id != route_id {
                            self.video.apply_policy_cap(
                                &plan.route_id,
                                Some(plan.route_budget_bps.min(u64::from(u32::MAX)) as u32),
                                plan.auto_resolution,
                            );
                        }
                    }
                    if let Some((peer, mode)) = audio_profile_update {
                        self.apply_audio_profile_for_peer(&peer, mode);
                    }
                    self.queue_effective_plans(plans_to_echo);
                }
                Effect::VideoFeedback {
                    route_id,
                    incarnation,
                    recv_fps,
                    decode_fails,
                    queue_depth,
                    lost_ts_us,
                    ext,
                } => {
                    if !self.route_is_active_incarnation(&route_id, incarnation.as_deref()) {
                        continue;
                    }
                    // The pipeline's own feedback shape lives in the opaque
                    // ext — parse it here, at the backend edge, so the
                    // wire crates never learned what a bandwidth estimate
                    // is (the seam that keeps tuning backend-only).
                    let pf = crate::video::PipelineFeedback::from_ext(&ext);
                    if let Some(ts) = lost_ts_us {
                        // Frame health: the viewer named the AU that died.
                        // This signal follows decoder or queue abandonment.
                        // The receiver now rejects dependent frames until a key
                        // AU, so a GDR wave alone cannot recover it. Force an
                        // IDR until soft damage has a distinct recovery signal.
                        tracing::info!(
                            "frame health {route_id}: viewer lost AU at {ts} us; forcing IDR"
                        );
                        self.video.force_idr(&route_id);
                    }
                    self.video.note_feedback(
                        &route_id,
                        recv_fps,
                        decode_fails,
                        queue_depth,
                        pf.est_kbps,
                        pf.delay_trend_us_per_s,
                    );
                    if pf.audio_underruns > 0 {
                        tracing::info!(
                            "audio health {route_id}: {} underruns / {} frames, jitter {} us, buffer {}/{} ms",
                            pf.audio_underruns,
                            pf.audio_underrun_frames,
                            pf.audio_arrival_jitter_us,
                            pf.audio_buffered_ms,
                            pf.audio_target_ms,
                        );
                    }
                    if let Some(peer) = self.route_peer(&route_id) {
                        let estimate = (pf.est_kbps > 0)
                            .then_some(u64::from(pf.est_kbps).saturating_mul(1_000));
                        let serial = self.video_policy_apply_serial.lock();
                        let plans = self
                            .media_policy
                            .lock()
                            .note_path_estimate(pubkey_part(&peer), estimate);
                        self.apply_video_policy_caps_locked(&plans, &serial);
                        self.queue_effective_plans(plans);
                    }
                }
                Effect::StopMedia {
                    route_id: id,
                    incarnation,
                } => {
                    let _lifecycle = self.lock_route_lifecycle(&id).await;
                    self.apply_stop_media_locked(id, incarnation);
                }
                Effect::Share { from, message } => self.handle_share(from, message).await,
                Effect::Ownership { from, message } => self.handle_ownership(from, message).await,
                Effect::App { from, message } => self.handle_app_control(from, message).await,
            }
        }
    }

    /// Apply an inbound app-control command. These are fleet-only — a machine
    /// only acts on the say-so of its owner or a fleet co-member (the same
    /// rule a terminal/remote-control offer is screened by), so a command
    /// from anyone else is logged and dropped.
    async fn handle_app_control(self: &Arc<Self>, from: NodeId, message: AppControl) {
        if !self.sender_may_control(from.as_str()) {
            tracing::warn!(
                "app-control {:?} from {} ignored: not owner/fleet",
                message,
                short_id(from.as_str())
            );
            return;
        }
        match message {
            AppControl::Upgrade => {
                tracing::info!(
                    "upgrade requested by {} — running self-update",
                    short_id(from.as_str())
                );
                // Download + apply off the inbound-frame task (it does network
                // I/O), then restart onto the new build. The peer gets no
                // reply: our next presence advert (the new version) is the
                // confirmation, and the button it pressed disappears when the
                // upgrade lands — exactly how a claim confirms by re-advert.
                let sink = self.sink.clone();
                crate::spawn(async move {
                    match allmystuff_updater::update_now().await {
                        Ok(allmystuff_updater::UpdateNowOutcome::Updated { to, components }) => {
                            tracing::info!(
                                "self-update applied {to} ({}) — restarting",
                                components.join("+")
                            );
                            sink.restart();
                        }
                        Ok(other) => {
                            tracing::info!("upgrade request: nothing to do ({other:?})")
                        }
                        Err(e) => tracing::warn!("upgrade request failed: {e}"),
                    }
                });
            }
            AppControl::Restart => {
                tracing::info!(
                    "app restart requested by {} — relaunching this node",
                    short_id(from.as_str())
                );
                // No update, no network I/O — just relaunch onto the same
                // build (the OS-aware relaunch the sink owns). Like the upgrade
                // path, the confirmation is the node's next presence advert; no
                // reply is sent. Done on a fresh task so the relaunch's
                // never-returning exec/exit doesn't strand the inbound-frame
                // loop mid-handler.
                let sink = self.sink.clone();
                crate::spawn(async move {
                    sink.restart();
                });
            }
            AppControl::RestartDevice => {
                tracing::info!(
                    "device reboot requested by {} — handing to the OS",
                    short_id(from.as_str())
                );
                // Tell whoever is sitting at this machine why it's about to
                // go down, then ask the OS off the inbound-frame task. The
                // OS's own privilege rules still apply (see `crate::reboot`);
                // a refusal is logged rather than silently swallowed.
                self.sink.emit(
                    "allmystuff://device-restart",
                    serde_json::json!({ "from": from.as_str() }),
                );
                crate::spawn(async move {
                    match tokio::task::spawn_blocking(crate::reboot::restart_device).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => tracing::warn!("device reboot refused: {e}"),
                        Err(e) => tracing::warn!("device reboot task failed: {e}"),
                    }
                });
            }
            // An app command a newer build introduced that this one doesn't
            // implement (decoded as `Unknown` rather than failing the
            // control message) — nothing to act on.
            AppControl::Unknown => {}
        }
    }

    /// Apply an inbound ownership message. A [`OwnershipControl::Claim`] is
    /// the load-bearing one: this device only lets the claim take if it's
    /// actually claimable (in claim mode and unowned) — that's the rule that
    /// stops a peer flat-out taking a box. The other variants are feedback
    /// the claimer's UI surfaces.
    async fn handle_ownership(self: &Arc<Self>, from: NodeId, message: OwnershipControl) {
        match message {
            OwnershipControl::Claim { owner } => {
                // The owner of record is the *authenticated sender* the mesh
                // delivered (`from`), never an arbitrary value in the body —
                // otherwise a peer could claim a box "for" someone else. The
                // claimer asserts its display id while the daemon delivers the
                // bare pubkey, so compare by pubkey (self-asserted) and record
                // the authenticated `from`.
                let reply = if pubkey_part(owner.as_str()) != pubkey_part(from.as_str()) {
                    OwnershipControl::Declined {
                        reason: "a claim must be self-asserted".into(),
                    }
                } else if self.ownership.try_accept_claim(from.as_str()) {
                    // The claim took — a claim change runs the full status
                    // check: re-advertise with the new owner so the claimer
                    // (and everyone) sees it's now spoken for. Any stale
                    // fleet state was reset by the accept; the owner's
                    // roster lands next on the owned channel.
                    tracing::info!(
                        "claim accepted: {} now owns this device",
                        short_id(from.as_str())
                    );
                    self.ownership_check(None).await;
                    // Push our own owned roster now so this device's GUI knows
                    // it's claimed (in a fleet) immediately — before the owner's
                    // `FleetKey` handoff lands. Without this, an owned-but-keyless
                    // window would read as "not in a fleet" while we'd already
                    // refuse to be made claimable, the very contradiction the
                    // roster's `claimed` flag exists to resolve.
                    self.emit_owned().await;
                    OwnershipControl::Claimed { owner }
                } else {
                    tracing::info!(
                        "claim from {} declined: not in claim mode",
                        short_id(from.as_str())
                    );
                    OwnershipControl::Declined {
                        reason: "this device isn't in claim mode".into(),
                    }
                };
                if let Err(e) = self
                    .send_control(&from.to_string(), &ControlMessage::Ownership(reply))
                    .await
                {
                    tracing::warn!(
                        "couldn't send the claim reply to {}: {e}",
                        short_id(from.as_str())
                    );
                }
            }
            OwnershipControl::Release => {
                // The recorded owner is letting this device go (compare by
                // pubkey — same display-vs-bare id reconciliation as Claim).
                // This also covers a kick: the owner sends Release alongside
                // the closed-network Evict so the device ejects itself even if
                // it missed (or won't honour) the signed removal.
                let owner = self.ownership.owner();
                if owner.as_deref().map(pubkey_part) == Some(pubkey_part(from.as_str())) {
                    tracing::info!("released by {} — unowned again", short_id(from.as_str()));
                    self.apply_fleet_release().await;
                }
            }
            OwnershipControl::Claimed { owner } => {
                // Honour a claim confirmation only from a device THIS node
                // actually sent a `Claim` to. Without this guard any
                // authenticated peer could send an *unsolicited* `Claimed` and
                // drive itself into our fleet member list *and* signed roster —
                // both of which `sender_may_control` trusts — i.e. hand itself
                // full control of this machine (input, shell, disk, clipboard).
                // This is the outbound-claim mirror of the per-sender guards the
                // sibling arms already apply (`Release`/`FleetKey` check the
                // recorded owner). Consumed on use, so a replayed or duplicate
                // confirmation is ignored.
                if !self
                    .pending_claims
                    .lock()
                    .remove(pubkey_part(from.as_str()))
                {
                    tracing::warn!(
                        "ignoring unsolicited claim confirmation from {} — this device never claimed it",
                        short_id(from.as_str())
                    );
                    return;
                }
                // The device we claimed (`from`) accepted us as its owner.
                // Make the claim *do* something durable: mint our fleet key on
                // the first adoption, record ourselves and the new device in
                // the owner's re-admit list, found the fleet's closed network
                // (electing us Owner) and admit the new device into its signed
                // roster, then hand the fleet key down to it so it derives and
                // joins the same network. The signed roster — not gossip — is
                // now the authority for membership and control.
                let key = self.ownership.ensure_fleet_key();
                if let Some(me) = self.local_node_id() {
                    let my_label = self.profile_label().unwrap_or_else(|| me.clone());
                    self.ownership.upsert_member(&me, &my_label);
                }
                let label = self.peer_label(&from);
                self.ownership.upsert_member(from.as_str(), &label);
                tracing::info!(
                    "claim confirmed by {}; fleet key …{} now {} member(s)",
                    short_id(from.as_str()),
                    key_tail(&key),
                    self.ownership.fleet_member_ids().len(),
                );
                // Found the closed network (if new) and admit every member —
                // including the one just claimed — into its signed roster.
                self.ensure_fleet_network().await;
                self.refresh_fleet_authorization().await;
                // Hand the new device its fleet credential point-to-point so it
                // joins the same closed network and converges its roster.
                self.send_fleet_key(from.as_str()).await;
                self.emit_owned().await;
                // Surface the claim feedback for the claimer's toast, too.
                self.sink.emit(
                    "allmystuff://ownership",
                    json!({
                        "from": from.to_string(),
                        "message": OwnershipControl::Claimed { owner },
                    }),
                );
            }
            OwnershipControl::FleetKey { key, name, venue } => {
                // Our owner handed us the fleet credential. Adopt the key (so we
                // derive the same closed network), join it, and converge our
                // signed roster from the owner's governance. Only honoured from
                // our recorded owner — a stray key from anyone else is ignored.
                let from_is_owner = self.ownership.owner().as_deref().map(pubkey_part)
                    == Some(pubkey_part(from.as_str()));
                if !from_is_owner {
                    tracing::warn!(
                        "ignoring fleet key from {} — not our owner",
                        short_id(from.as_str())
                    );
                    return;
                }
                if self.ownership.adopt_fleet_key(&key, &name) {
                    tracing::info!(
                        "adopted fleet key …{} from {} — joining its closed network",
                        key_tail(&key),
                        short_id(from.as_str())
                    );
                    self.ensure_fleet_network().await;
                    // The handoff landed — the claim rendezvous has done its
                    // job. Tear the claim-code network down and rotate the
                    // (now spent) code.
                    self.ensure_claim_networks().await;
                    self.refresh_fleet_authorization().await;
                    self.emit_owned().await;
                }
                // Apply the owner's venue regardless of whether the key changed —
                // the owner may have re-handed it *only* to update the venue (a
                // venue change re-broadcasts with the same key+name).
                if let Some(venue) = venue {
                    self.apply_fleet_venue(&venue).await;
                }
            }
            OwnershipControl::FleetDeparted => {
                // A member is telling us it left the fleet. Evict it from the
                // signed roster so our view (and every other member's) reflects
                // reality. Only the fleet owner acts on this.
                if self.ownership.is_fleet_owner() {
                    tracing::info!(
                        "{} left the fleet — dropping from the roster",
                        short_id(from.as_str())
                    );
                    self.fleet_drop_member(from.to_string()).await;
                }
            }
            other => {
                // Declined — feedback for the claimer's UI.
                tracing::info!(
                    "ownership reply from {}: {:?}",
                    short_id(from.as_str()),
                    other
                );
                self.sink.emit(
                    "allmystuff://ownership",
                    json!({ "from": from.to_string(), "message": other }),
                );
            }
        }
    }

    /// The device-side fleet teardown — everything "this device just left
    /// / was let go from its fleet" implies, in one place: tear out of the
    /// fleet's closed network (purging its signed state so a later rejoin
    /// can't reload a stale genesis), clear the durable owner/key record,
    /// and re-broadcast the now-unowned presence. Shared by the
    /// cooperative path (the owner's `Release` frame) and the verified
    /// path (the daemon's `self_evicted` governance event — the device
    /// PROVED its own eviction from the signed log, which is stronger
    /// authority than any frame a peer could send). Deliberately does NOT
    /// re-enter claim mode: adoption is per-event consent on this device.
    async fn apply_fleet_release(self: &Arc<Self>) {
        // Tear out of the fleet's closed network before clearing the
        // credential (set_owner(None) drops the key it derives from).
        let fleet_net = self.ownership.fleet_network_id();
        self.ownership.set_owner(None);
        if let Some(network) = fleet_net {
            let _ = self
                .client
                // We've left this fleet — purge its signed state too: no
                // stale genesis to reload if we later join a different one.
                .request(&Request::NetworkRemove {
                    network,
                    purge: true,
                })
                .await;
        }
        self.refresh_fleet_authorization().await;
        self.ownership_check(None).await;
    }

    /// Re-stamp the live presence profile's owner/claimable from the store
    /// and broadcast, so an ownership change propagates immediately.
    async fn refresh_profile_ownership(self: &Arc<Self>) {
        {
            let mut st = self.state.lock();
            if let Some(p) = st.profile.as_mut() {
                p.owner = self.ownership.owner().map(NodeId::from);
                p.claimable = self.ownership.claimable();
            }
        }
        self.broadcast_presence().await;
        self.emit_snapshot();
    }

    // ---- owned fleet gossip ------------------------------------------

    /// This node's current display label from the live presence profile.
    fn profile_label(&self) -> Option<String> {
        self.state.lock().profile.as_ref().map(|p| p.label.clone())
    }

    /// Best-known display label for a peer (matched by canonical pubkey, since
    /// the daemon delivers a bare pubkey while presence is keyed by display
    /// id), else a short id. Gives fleet members a friendly name.
    fn peer_label(&self, peer: &NodeId) -> String {
        let canon = pubkey_part(peer.as_str());
        {
            let st = self.state.lock();
            if let Some(session) = st.session.as_ref() {
                for p in session.peers() {
                    if pubkey_part(p.node.as_str()) == canon && !p.label.trim().is_empty() {
                        return p.label.clone();
                    }
                }
            }
        }
        let s = peer.as_str();
        if s.len() > 12 {
            format!("{}…", &s[..10])
        } else {
            s.to_string()
        }
    }

    /// The display label to hand a KVM so it can name itself `KVM-<target>` —
    /// the target's *real* advertised name, or empty when we don't know it.
    /// Distinct from [`Self::peer_label`], which falls back to a truncated id:
    /// here an unknown target must yield "" so the KVM falls back to the node
    /// id itself rather than being named `KVM-abcd1234ef…` with a literal
    /// ellipsis. The attach picker's default target is often *this* machine,
    /// which is never a session peer, so that case is resolved from our own
    /// presence profile.
    fn attach_target_label(&self, target: &NodeId) -> String {
        let canon = pubkey_part(target.as_str());
        let st = self.state.lock();
        // This machine (the picker's frequent default): our own profile label.
        if let Some(p) = st.profile.as_ref() {
            if target.is_this() || pubkey_part(p.node.as_str()) == canon {
                let l = p.label.trim();
                return if l.is_empty() {
                    String::new()
                } else {
                    l.to_string()
                };
            }
        }
        if let Some(session) = st.session.as_ref() {
            for p in session.peers() {
                if pubkey_part(p.node.as_str()) == canon && !p.label.trim().is_empty() {
                    return p.label.clone();
                }
            }
        }
        String::new()
    }

    /// Hand a freshly-claimed device its fleet credential point-to-point: the
    /// shared key (so it derives the same closed-network id and joins it) and
    /// the fleet name. This replaces the old gossiped `OwnedRoster` — the
    /// device's signed-roster membership converges from the owner's governance
    /// once it's in the network.
    async fn send_fleet_key(&self, peer: &str) {
        let Some(key) = self.ownership.fleet_key() else {
            return;
        };
        let name = self.ownership.fleet_name();
        // Hand the fleet's venue (transport servers) down with the key, so the
        // member rides the same calling-out point as the rest of the fleet.
        let venue = self.fleet_venue_json().await;
        let msg = ControlMessage::Ownership(OwnershipControl::FleetKey { key, name, venue });
        match self.send_control(peer, &msg).await {
            Ok(()) => tracing::info!("handed the fleet key to {}", short_id(peer)),
            Err(e) => tracing::warn!("couldn't hand the fleet key to {}: {e}", short_id(peer)),
        }
    }

    /// The owner's fleet-network **venue** — its transport servers (signaling /
    /// STUN / TURN) — as a JSON object string, read from the live daemon config,
    /// to hand a member so it calls out where the fleet does. Just the transport
    /// fields; the member owns its own id/label/kind. `None` when the fleet
    /// network isn't configured yet or carries no servers (defaults are fine).
    async fn fleet_venue_json(&self) -> Option<String> {
        let network = self.ownership.fleet_network_id()?;
        let resp = self.client.request(&Request::ConfigShow).await.ok()?;
        if !resp.ok {
            return None;
        }
        let data = resp.data?;
        let nets = data.pointer("/config/networks")?.as_array()?;
        let cfg = nets.iter().find(|n| {
            let id = n.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            let nid = n
                .get("network_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            id == network || nid == network
        })?;
        let mut venue = serde_json::Map::new();
        for k in ["signaling", "stun_servers", "turn_servers"] {
            if let Some(v) = cfg.get(k) {
                venue.insert(k.to_string(), v.clone());
            }
        }
        if venue.is_empty() {
            return None;
        }
        serde_json::to_string(&Value::Object(venue)).ok()
    }

    /// Apply the owner's handed-down fleet **venue** to this device's fleet
    /// network, so it calls out where the rest of the fleet does. Members mirror
    /// the owner's venue; they don't define it. A best-effort `NetworkUpdate`
    /// over just the transport fields, keyed to our own fleet network id.
    async fn apply_fleet_venue(self: &Arc<Self>, venue_json: &str) {
        let Some(network) = self.ownership.fleet_network_id() else {
            return;
        };
        let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(venue_json) else {
            return;
        };
        let mut config = serde_json::Map::new();
        config.insert("id".into(), Value::String(network.clone()));
        config.insert("network_id".into(), Value::String(network.clone()));
        for (k, v) in obj {
            config.insert(k, v);
        }
        let _ = self
            .client
            .request(&Request::NetworkUpdate {
                config: Value::Object(config),
            })
            .await;
        self.sync_networks().await;
    }

    /// Whether `network` is this device's fleet mesh.
    pub fn is_fleet_network(&self, network: &str) -> bool {
        self.ownership.fleet_network_id().as_deref() == Some(network)
    }

    /// Owner-only: re-hand the fleet key — which now carries the fleet-network
    /// venue — to every member, so a venue the owner just changed propagates to
    /// the whole fleet. A no-op for a non-owner: members don't define the venue,
    /// only the owner broadcasts it (managers manage members, not core settings).
    pub async fn fleet_broadcast_config(self: &Arc<Self>) {
        if !self.ownership.is_fleet_owner() {
            return;
        }
        let me = self.local_node_id().map(|m| pubkey_part(&m).to_string());
        for member in self.ownership.fleet_member_ids() {
            if Some(pubkey_part(&member).to_string()) == me {
                continue;
            }
            self.send_fleet_key(&member).await;
        }
    }

    /// Push the current fleet roster to the front-end. Sourced from the
    /// closed network's **signed roster**, so the GUI shows authenticated
    /// membership, not a gossiped guess.
    async fn emit_owned(&self) {
        let value = self.fleet_roster_value().await;
        self.sink.emit("allmystuff://owned", value);
    }

    /// The current fleet roster as JSON — for the `owned_roster` command and
    /// the `allmystuff://owned` event, in the `OwnedRoster` shape the GUI
    /// expects: the shared key + name from local state, members from the
    /// fleet's closed-network **signed roster** (`RosterList`). An empty
    /// key/members when there's no fleet yet, so the front-end always gets a
    /// well-formed shape.
    pub async fn fleet_roster_value(&self) -> Value {
        // The single membership truth the whole GUI reads: `in_fleet`. A device
        // is in a fleet the moment it's claimed — it belongs to its owner's
        // fleet even before the owner's `FleetKey` handoff lands (which can lag
        // or fail if the owner is briefly offline) — or whenever it holds a key.
        // The GUI never sees the *local* node's own `owner`, so it leans on this
        // one flag; every place that asks "am I in a fleet" (the drawer, the
        // settings pane, the leave button) reads it, so they can't disagree.
        let in_fleet = self.ownership.in_fleet();
        // Not in a fleet at all → the empty, well-formed shape. Everything below
        // assumes membership, and the GUI keys solely on `in_fleet`.
        if !in_fleet {
            let mut v = empty_owned();
            if let Some(o) = v.as_object_mut() {
                o.insert("in_fleet".into(), Value::Bool(false));
                // The device-local public-claims setting rides the owned
                // payload in both shapes — the toggle is usable before a
                // fleet exists (it gates this machine's own claiming).
                o.insert(
                    "public_claims".into(),
                    Value::Bool(self.ownership.public_claims()),
                );
            }
            return v;
        }
        // In a fleet. The key/network may be absent — an owned-but-keyless
        // member that's been claimed but hasn't received its owner's key
        // handoff is in a fleet with no closed network of its own yet. In that
        // case there's no signed roster to read; the membership the user sees is
        // still real (self, plus the owner's local list when we're the owner).
        let key = self.ownership.fleet_key().unwrap_or_default();
        let network = self.ownership.fleet_network_id();
        let mut members: Vec<OwnedMember> = Vec::new();
        let mut member_roles: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        if let Some(network) = network.as_deref() {
            if let Ok(r) = self
                .client
                .request(&Request::RosterList {
                    network: network.to_string(),
                })
                .await
            {
                if r.ok {
                    if let Some(arr) = r
                        .data
                        .as_ref()
                        .and_then(|d| d.get("roster"))
                        .and_then(|v| v.as_array())
                    {
                        for e in arr {
                            if let Some(id) = e.get("device_id").and_then(|v| v.as_str()) {
                                let label = e
                                    .get("label")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                // The governance role projection ("member" /
                                // "controller" / "owner"), so the GUI can label
                                // the grant/withdraw controls per member.
                                let role = e
                                    .get("role")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("member")
                                    .to_string();
                                member_roles.insert(pubkey_part(id).to_string(), role);
                                members.push(OwnedMember {
                                    device: NodeId::from(pubkey_part(id)),
                                    label,
                                });
                            }
                        }
                    }
                }
            }
        }
        // Member-side resilience for the signed roster (the symmetric twin of
        // the owner's `fleet_members()` fallback below). `members` here holds
        // exactly what the closed network's signed roster returned. A non-empty
        // read is authoritative — cache it. An empty read means the fleet's
        // closed network is momentarily unreadable (mid-(re)join), not that the
        // fleet emptied: fall back to the last cached roster so a co-member
        // doesn't flicker to "another fleet" during a reconnect. Because a
        // non-empty read always replaces the cache, an eviction propagates the
        // instant the roster is readable again.
        if members.is_empty() {
            for m in self.fleet_roster_cache.lock().iter() {
                let canon = pubkey_part(m.device.as_str()).to_string();
                member_roles
                    .entry(canon)
                    .or_insert_with(|| "member".to_string());
                members.push(m.clone());
            }
        } else {
            *self.fleet_roster_cache.lock() = members.clone();
        }
        // Fold in the owner's durable local member list so its devices show as
        // members immediately — before the closed network's signed roster
        // re-converges on startup, and through a transient roster-read failure —
        // and so the roster the GUI sees matches the owner's actual membership
        // rather than diverging from it. A left or evicted device is dropped
        // from this list too (the removal paths clear both), so the merge never
        // resurrects one; a non-owner member's list is empty, a no-op there.
        for m in self.ownership.fleet_members() {
            let canon = pubkey_part(m.device.as_str()).to_string();
            if !members
                .iter()
                .any(|x| pubkey_part(x.device.as_str()) == canon)
            {
                members.push(OwnedMember {
                    device: NodeId::from(canon.as_str()),
                    label: m.label.clone(),
                });
            }
            member_roles
                .entry(canon)
                .or_insert_with(|| "member".to_string());
        }
        // The signed roster a node holds never lists *itself* — each device is
        // locally authoritative and isn't re-added from a peer's roster gossip
        // (MyOwnMesh `on_roster_entries` skips the self entry). But the fleet
        // the user sees includes this device: it holds the key. Add self so the
        // GUI's "am I in my fleet" check (and the relationship reconcile that
        // depends on it) is true for members, not just the owner.
        if let Some(me) = self.local_node_id() {
            let canon = pubkey_part(&me).to_string();
            if !members
                .iter()
                .any(|m| pubkey_part(m.device.as_str()) == canon)
            {
                let label = self.profile_label().unwrap_or_else(|| me.clone());
                members.push(OwnedMember {
                    device: NodeId::from(canon.as_str()),
                    label,
                });
            }
            // Best-effort role for this device (it isn't in its own roster):
            // the founder is the owner, everyone else defaults to member.
            member_roles.entry(canon).or_insert_with(|| {
                if self.ownership.is_fleet_owner() {
                    "owner"
                } else {
                    "member"
                }
                .to_string()
            });
        }
        // The whole fleet should see *who the owner is*, not just the owner
        // machine. A member always knows its owner locally — the device that
        // claimed it — so stamp that device "owner" here, covering the window
        // before the closed network's signed roster converges its role and the
        // owned-but-keyless case (claimed, no closed network yet, so no roster
        // to read at all). `or_insert` never overrides a role the signed roster
        // already projected, and the owner is added to the member list if the
        // roster hasn't surfaced it yet (label left blank — the GUI resolves it
        // by canonical id). The MyOwnMesh roster gossip converges the same fact
        // network-wide; this is the local fast path / fallback.
        if !self.ownership.is_fleet_owner() {
            if let Some(owner) = self.ownership.owner() {
                let canon = pubkey_part(&owner).to_string();
                if !members
                    .iter()
                    .any(|m| pubkey_part(m.device.as_str()) == canon)
                {
                    members.push(OwnedMember {
                        device: NodeId::from(canon.as_str()),
                        label: String::new(),
                    });
                }
                member_roles
                    .entry(canon)
                    .or_insert_with(|| "owner".to_string());
            }
        }
        let roster = OwnedRoster {
            key,
            name: self.ownership.fleet_name(),
            version: self.ownership.fleet_version(),
            members,
        };
        // "Owner" for the GUI is the **signed** owner role OR the structural
        // key-holder — a device the founder granted the owner role is a full
        // owner and must see owner actions (evict, promote, …), not be gated out
        // as a second-class member.
        let is_owner_flag = self.ownership.is_fleet_owner()
            || match network.as_deref() {
                Some(n) => self.is_fleet_owner_signed(n).await,
                None => false,
            };
        let governed_topology = match network.as_deref() {
            Some(n) => self.fleet_governed_topology(n).await.unwrap_or(Value::Null),
            None => Value::Null,
        };
        let mut value = serde_json::to_value(roster).unwrap_or_else(|_| empty_owned());
        if let Some(obj) = value.as_object_mut() {
            // Whether this device may take owner actions (signed owner or the
            // structural key-holder), so the GUI can gate owner-only controls.
            obj.insert("is_owner".into(), Value::Bool(is_owner_flag));
            // The fleet's closed-network id, so the GUI can spot which mesh in
            // the list is the fleet mesh and lock it (you leave it by leaving
            // the fleet, not by removing the mesh). Empty for a keyless member
            // that hasn't joined a closed network yet.
            obj.insert(
                "network_id".into(),
                Value::String(network.unwrap_or_default()),
            );
            // The single membership flag — always true here (we returned early
            // when not in a fleet), so the GUI's "am I in a fleet" check is the
            // same regardless of whether we hold a key yet.
            obj.insert("in_fleet".into(), Value::Bool(in_fleet));
            // This device's public-claims setting (device-local, never
            // synced) — the Fleet pane's toggle reads it from here.
            obj.insert(
                "public_claims".into(),
                Value::Bool(self.ownership.public_claims()),
            );
            // Governed topology (daemon ≥ 0.2.36): the owner-signed
            // network-wide shape the fleet runs, or null when ungoverned
            // (or the daemon predates it). The Fleet pane's infra-hub
            // toggles render from this.
            obj.insert("topology".into(), governed_topology);
            // Stamp each member with its governance role for the drawer's
            // grant/withdraw controls.
            if let Some(arr) = obj.get_mut("members").and_then(|v| v.as_array_mut()) {
                for m in arr {
                    let canon = m
                        .get("device")
                        .and_then(|v| v.as_str())
                        .map(|d| pubkey_part(d).to_string())
                        .unwrap_or_default();
                    let role = member_roles
                        .get(&canon)
                        .cloned()
                        .unwrap_or_else(|| "member".to_string());
                    if let Some(mo) = m.as_object_mut() {
                        mo.insert("role".into(), Value::String(role));
                    }
                }
            }
        }
        value
    }

    /// Front-end command: claim `node` as owned by this device. Only the
    /// target deciding it's claimable makes it stick; we just send intent —
    /// but a send the daemon couldn't deliver (device dropped offline, no
    /// shared network) is surfaced so the UI can say so rather than leaving
    /// "asking…" hanging forever.
    pub async fn claim(self: &Arc<Self>, node: String) -> Result<(), String> {
        let me = self.local_node_id().ok_or("mesh not ready")?;
        // Claimer-side mirror of the claimee's arrival-network gate: with
        // public claims off (the default), a claim only goes out over the
        // LAN claim rendezvous. The error names the fix — either walk the
        // two machines onto one local network, or deliberately enable the
        // public-mesh path on this device.
        if !self.ownership.public_claims_allowed() {
            let route = self.network_for_peer(&node);
            if route.as_deref() != Some(LOCAL_CLAIM_NETWORK_ID) {
                return Err(
                    "this device was discovered over a public mesh — put both machines on \
                     the same local network, or enable \"Allow claiming over the public \
                     mesh\" in Fleet settings on this machine"
                        .into(),
                );
            }
        }
        // Record that we're now awaiting this device's `Claimed` confirmation,
        // so the inbound handler honours only a confirmation we actually
        // solicited (see `pending_claims` / the `Claimed` arm). Recorded before
        // the send; if the send fails the peer never answers, so the leftover
        // entry is harmless.
        self.pending_claims
            .lock()
            .insert(pubkey_part(&node).to_string());
        tracing::info!("claiming {} (sending ownership claim)", short_id(&node));
        let msg = ControlMessage::Ownership(OwnershipControl::Claim { owner: me.into() });
        self.send_control(&node, &msg).await
    }

    /// Front-end command: claim a **remote** device by the claim code its
    /// operator read off it (device web UI, service log). Joins the code's
    /// randomized rendezvous network — unguessable, so unlike the old
    /// well-known public claim mesh nobody can lurk there — waits for the
    /// device's claimable presence, sends the claim, waits for it to land
    /// in the fleet, and tears the rendezvous down again either way.
    pub async fn claim_via_code(self: &Arc<Self>, code: String) -> Result<(), String> {
        if !self.ownership.public_claims_allowed() {
            return Err(
                "remote claiming is off on this device — enable \"Allow claiming over the \
                 public mesh\" in Fleet settings first"
                    .into(),
            );
        }
        let network = claim_code_network_id(&code);
        if network == claim_code_network_id("") {
            return Err("enter the claim code shown on the device".into());
        }
        tracing::info!("remote claim: joining rendezvous {network}");
        let _ = self
            .client
            .request(&Request::NetworkAdd {
                config: json!({
                    "id": network.as_str(),
                    "network_id": network.as_str(),
                    "label": "Remote claiming",
                    "kind": "open",
                    "auto_approve": true,
                    "signaling": { "strategy": "nostr", "mdns": true },
                }),
            })
            .await;
        self.sync_networks().await;

        let result = self.claim_via_code_inner(&network).await;

        // Rendezvous down again, success or not — it existed for this one
        // claim. `purge` drops its signed-state residue too.
        let _ = self
            .client
            .request(&Request::NetworkRemove {
                network: network.clone(),
                purge: true,
            })
            .await;
        self.sync_networks().await;
        result
    }

    async fn claim_via_code_inner(self: &Arc<Self>, network: &str) -> Result<(), String> {
        // Wait for the device's claimable presence on the rendezvous.
        const DISCOVER_DEADLINE: std::time::Duration = std::time::Duration::from_secs(75);
        const CLAIM_DEADLINE: std::time::Duration = std::time::Duration::from_secs(45);
        const POLL: std::time::Duration = std::time::Duration::from_millis(500);

        let discover_by = std::time::Instant::now() + DISCOVER_DEADLINE;
        let target = loop {
            if let Some(node) = self.claimable_on_network(network) {
                break node;
            }
            if std::time::Instant::now() > discover_by {
                return Err(
                    "no claimable device answered on that code — check the code, make sure \
                     remote claiming is still enabled on the device, and that it is online"
                        .into(),
                );
            }
            tokio::time::sleep(POLL).await;
        };

        tracing::info!(
            "remote claim: found claimable device {} on the rendezvous",
            short_id(&target)
        );
        self.claim(target.clone()).await?;

        // The `Claimed` reply mints the fleet key and records the member —
        // that's the durable signal the claim landed.
        let claimed_by = std::time::Instant::now() + CLAIM_DEADLINE;
        loop {
            let claimed = self
                .ownership
                .fleet_member_ids()
                .iter()
                .any(|m| pubkey_part(m) == pubkey_part(&target));
            if claimed {
                return Ok(());
            }
            if std::time::Instant::now() > claimed_by {
                return Err(
                    "the device saw the claim but confirmation never arrived — it may have \
                     declined (already owned, or claim mode off); check its screen or logs"
                        .into(),
                );
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// A claimable node whose last-seen network is `network`, if any.
    fn claimable_on_network(&self, network: &str) -> Option<String> {
        let st = self.state.lock();
        let session = st.session.as_ref()?;
        let mut claimables = session
            .peers()
            .filter(|p| p.claimable)
            .map(|p| p.node.to_string());
        claimables.find(|id| {
            st.peer_networks
                .get(pubkey_part(id))
                .is_some_and(|paths| paths.contains(network))
        })
    }

    /// Front-end command: ask a fleet machine to update itself to the
    /// channel's latest release and restart. The far side enforces owner/fleet
    /// before acting (and decides there's nothing to do if it's already
    /// current); its next presence advert — carrying the new version — is the
    /// confirmation. A send the daemon couldn't deliver is surfaced so the UI
    /// can say so rather than leaving the ask hanging.
    pub async fn request_upgrade(self: &Arc<Self>, node: String) -> Result<(), String> {
        tracing::info!("asking {} to upgrade + restart", short_id(&node));
        let msg = ControlMessage::App(AppControl::Upgrade);
        self.send_control(&node, &msg).await
    }

    /// Ask a fleet machine to **restart** its AllMyStuff app (relaunch onto the
    /// same build — no update). The target enforces owner/fleet before acting;
    /// its next presence advert is the confirmation.
    pub async fn request_restart(self: &Arc<Self>, node: String) -> Result<(), String> {
        tracing::info!("asking {} to restart its app", short_id(&node));
        let msg = ControlMessage::App(AppControl::Restart);
        self.send_control(&node, &msg).await
    }

    /// Front-end command: reboot a machine's whole OS — the recovery step
    /// heavier than [`Mesh::request_restart`]. Our own device reboots
    /// directly (no wire round-trip to ourselves); a peer is asked with
    /// [`AppControl::RestartDevice`], gated owner/fleet on its side exactly
    /// like the app restart. Its presence dropping and returning is the
    /// confirmation. An older peer decodes the command as `Unknown` and
    /// ignores it — the ask goes unanswered, never misread.
    pub async fn request_restart_device(self: &Arc<Self>, node: String) -> Result<(), String> {
        let is_self = self
            .local_node_id()
            .is_some_and(|me| pubkey_part(&node) == pubkey_part(&me));
        if is_self {
            tracing::info!("rebooting this device (asked from its own gear menu)");
            return tokio::task::spawn_blocking(crate::reboot::restart_device)
                .await
                .map_err(|e| e.to_string())?;
        }
        tracing::info!("asking {} to reboot its device", short_id(&node));
        let msg = ControlMessage::App(AppControl::RestartDevice);
        self.send_control(&node, &msg).await
    }

    /// Re-learn a node's details on demand — the per-node refresh control.
    ///
    /// For **this** device (`None`, or our own id), re-scan its hardware and
    /// re-advertise the fresh profile, so both our own capabilities and what
    /// peers see of us update. For a **peer**, re-stamp + re-send our presence
    /// to it (an ownership/fleet re-sync that also nudges it) and re-request its
    /// exposed sites; the daemon already holds the peer's latest capability
    /// advert, so the GUI's follow-up resync picks up the rest. Best-effort: a
    /// site request to a non-managed peer is simply refused on the far side.
    pub async fn refresh_node(self: &Arc<Self>, node: Option<String>) -> Result<(), String> {
        let is_self = match (&node, self.local_node_id()) {
            (None, _) => true,
            (Some(n), Some(me)) => pubkey_part(n) == pubkey_part(&me),
            _ => false,
        };
        if is_self {
            tracing::info!("refresh: re-scanning this device + re-advertising");
            self.restamp_profile().await;
            return Ok(());
        }
        let peer = node.unwrap_or_default();
        if peer.is_empty() {
            return Ok(());
        }
        // One backoff tick guards every peer-bound action of a refresh, so a
        // held-down refresh can't hammer the peer (the envelope grows from once
        // every 5 s to once a minute over a sustained burst).
        if !self.allow_profile_request(&peer) {
            tracing::debug!("refresh of {} throttled by backoff", short_id(&peer));
            return Ok(());
        }
        tracing::info!("refresh: re-learning {}", short_id(&peer));
        // The guaranteed round-trip: ask the peer to re-announce its profile so
        // we re-learn it now (it answers with an ordinary presence advert).
        let _ = self
            .send_control(&peer, &ControlMessage::ProfileRequest)
            .await;
        // And re-sync our ownership/fleet view + its exposed sites while we're
        // here.
        self.ownership_check(Some(pubkey_part(&peer))).await;
        let _ = self.site_remote_list(peer).await;
        Ok(())
    }

    /// Reconnect mesh transport **in place** — redial signaling and renegotiate
    /// ICE without leaving the room. The non-destructive twin of a leave+rejoin
    /// (`network_set_enabled` off-then-on): every session and all app-level
    /// state survives, so a refresh on one side never strands the other.
    ///
    /// Resolution of what to reconnect: a set `network` is every peer on that
    /// mesh (the global refresh control); `peer` alone is that one node, on the
    /// mesh it's reachable on (the same network resolution our sends use, for
    /// the per-node refresh); neither is every joined mesh.
    ///
    /// Best-effort: a per-network failure is logged and the rest still run.
    pub async fn reconnect(
        self: &Arc<Self>,
        network: Option<String>,
        peer: Option<String>,
    ) -> Result<(), String> {
        let networks: Vec<String> = match (&network, &peer) {
            (Some(net), _) => vec![net.clone()],
            (None, Some(p)) => self.network_for_peer(p).into_iter().collect(),
            (None, None) => self.state.lock().networks.clone(),
        };
        if networks.is_empty() {
            return Err("no joined network to reconnect on".into());
        }
        // The daemon keys peer sessions by canonical pubkey, so strip any
        // display decoration off the node id before forwarding.
        let peer_canon = peer.as_deref().map(|p| pubkey_part(p).to_string());
        let mut any_ok = false;
        let mut last_err: Option<String> = None;
        for net in networks {
            match self
                .client
                .request(&Request::NetworkReconnect {
                    network: net.clone(),
                    peer: peer_canon.clone(),
                })
                .await
            {
                Ok(resp) if resp.ok => any_ok = true,
                Ok(resp) => {
                    let e = resp.error.unwrap_or_else(|| "reconnect rejected".into());
                    tracing::warn!("reconnect on {net}: {e}");
                    last_err = Some(e);
                }
                Err(e) => {
                    tracing::warn!("reconnect on {net} failed: {e}");
                    last_err = Some(e.to_string());
                }
            }
        }
        // A partial success still counts as success (the failed mesh is logged
        // above); a total failure surfaces so the GUI can report it.
        if any_ok {
            Ok(())
        } else {
            Err(last_err.unwrap_or_else(|| "reconnect failed".into()))
        }
    }

    /// Whether a refresh round-trip to `peer` is allowed under the backoff
    /// envelope right now, recording it as sent when it is. Keyed per canonical
    /// peer so refreshing different machines stays independent. See
    /// [`profile_req_decide`] for the envelope itself.
    fn allow_profile_request(&self, peer: &str) -> bool {
        let now = std::time::Instant::now();
        let key = pubkey_part(peer).to_string();
        let mut map = self.profile_req.lock();
        let (allow, st) = profile_req_decide(map.get(&key).copied(), now);
        map.insert(key, st);
        allow
    }

    /// Front-end command: point a KVM appliance (`node`) at the machine it
    /// controls (`target`). The KVM enforces owner/fleet before applying, then
    /// re-advertises its new binding ([`NodeProfile::kvm`]) — that presence is
    /// the authoritative confirmation, exactly as a claim confirms by
    /// re-advertising its new owner. A send the daemon couldn't deliver is
    /// surfaced so the UI can say so rather than leaving the ask hanging.
    pub async fn kvm_attach(self: &Arc<Self>, node: String, target: String) -> Result<(), String> {
        tracing::info!("pointing KVM {} at {}", short_id(&node), short_id(&target));
        // Ride the target's display label along so the KVM can rename itself
        // `KVM-<label>` — best-effort and cosmetic (empty when the target has
        // no label we know; the KVM then falls back to the node id, never a
        // truncated-id string).
        let label = self.attach_target_label(&NodeId::from(target.clone()));
        let msg = ControlMessage::Kvm(KvmControl::Attach {
            node: target.into(),
            label,
        });
        self.send_control(&node, &msg).await
    }

    /// Front-end command: clear a KVM appliance's binding — it no longer
    /// represents any machine. Same delivery + presence-confirmation model as
    /// [`Mesh::kvm_attach`].
    pub async fn kvm_detach(self: &Arc<Self>, node: String) -> Result<(), String> {
        tracing::info!("detaching KVM {}", short_id(&node));
        let msg = ControlMessage::Kvm(KvmControl::Detach);
        self.send_control(&node, &msg).await
    }

    /// Front-end command: walk a KVM appliance onto another mesh — the fleet
    /// owner's membership tool. The KVM validates the id, refuses its own
    /// fleet mesh, joins, and re-advertises [`NodeProfile::kvm`] with the new
    /// membership list — that presence is the authoritative confirmation.
    pub async fn kvm_mesh_add(
        self: &Arc<Self>,
        node: String,
        network_id: String,
    ) -> Result<(), String> {
        let network_id = network_id.trim().to_lowercase();
        if network_id.is_empty() {
            return Err("a mesh name is required".into());
        }
        tracing::info!("asking KVM {} to join mesh {network_id}", short_id(&node));
        let msg = ControlMessage::Kvm(KvmControl::MeshAdd { network_id });
        self.send_control(&node, &msg).await
    }

    /// Front-end command: take a KVM appliance off a mesh. The KVM refuses
    /// its fleet mesh (that membership is governed by the fleet key); same
    /// presence-confirmation model as [`Mesh::kvm_mesh_add`].
    pub async fn kvm_mesh_remove(
        self: &Arc<Self>,
        node: String,
        network_id: String,
    ) -> Result<(), String> {
        let network_id = network_id.trim().to_lowercase();
        if network_id.is_empty() {
            return Err("a mesh name is required".into());
        }
        tracing::info!("asking KVM {} to leave mesh {network_id}", short_id(&node));
        let msg = ControlMessage::Kvm(KvmControl::MeshRemove { network_id });
        self.send_control(&node, &msg).await
    }

    /// Front-end command: put *this* device into (or out of) claim mode, so
    /// another of your machines can adopt it. Re-advertises immediately.
    pub async fn set_claimable(self: &Arc<Self>, on: bool) -> Result<bool, String> {
        self.ownership.set_claim_mode(on);
        // Claim-rendezvous membership follows claim mode (the claim-code
        // network only exists while claimable with public claims on).
        self.ensure_claim_networks().await;
        self.refresh_profile_ownership().await;
        Ok(self.ownership.claimable())
    }

    /// Front-end command: flip **this device's** public-claims setting —
    /// whether it participates in claiming over the public mesh, in either
    /// role (offering itself via a claim code while claimable; claiming
    /// remote devices by code as an owner). Strictly device-local: it is
    /// never synced from a fleet and no remote peer can flip it. Off by
    /// default; claiming stays LAN-only until someone at this machine turns
    /// it on.
    pub async fn set_public_claims(self: &Arc<Self>, on: bool) -> Result<bool, String> {
        if !self.ownership.set_public_claims(on) {
            return Err("couldn't persist the setting".into());
        }
        tracing::info!(
            "claims over the public mesh {} on this device",
            if on { "ENABLED" } else { "disabled" }
        );
        // Rendezvous membership and presence both follow the setting.
        self.ensure_claim_networks().await;
        self.refresh_profile_ownership().await;
        self.emit_owned().await;
        Ok(self.ownership.public_claims())
    }

    /// The closed network backing this device's fleet (derived from the fleet
    /// key). The GUI targets the fleet's custody-MFA enroll/status at this id.
    pub fn fleet_network_id(&self) -> Option<String> {
        self.ownership.fleet_network_id()
    }

    /// The claim-status check — "is what we believe about ownership still
    /// true, and does everyone else know it?" Re-stamps the live profile from
    /// the ownership store, then re-asserts presence. Runs **targeted** at one
    /// peer right after its connection establishes or its app (re)starts — so
    /// the two sides converge on the event itself; there is no heartbeat — and
    /// **broadcast** on the local triggers: session start, a claim/release,
    /// and fleet membership changes.
    pub async fn ownership_check(self: &Arc<Self>, peer: Option<&str>) {
        if self.local_node_id().is_none() {
            return;
        }
        {
            let mut st = self.state.lock();
            if let Some(p) = st.profile.as_mut() {
                p.owner = self.ownership.owner().map(NodeId::from);
                p.claimable = self.ownership.claimable();
                // Re-stamp the fleet metadata too: a claim/adopt/leave/rename
                // is exactly when the fleet name + owner change, and this is the
                // path that re-broadcasts presence, so peers regroup correctly.
                p.fleet_name = self.ownership.fleet_name();
                p.fleet_owner = self.fleet_owner_name(&p.label.clone());
            }
        }
        match peer {
            Some(peer) => {
                tracing::debug!("ownership check → {}", short_id(peer));
                self.send_presence_to(peer).await;
            }
            None => {
                self.broadcast_presence().await;
            }
        }
        self.emit_owned().await;
        self.emit_snapshot();

        // Keep the closed-network fleet and its signed-roster cache in step
        // with this ownership change. Founding (owner-side `NetworkAdd` +
        // founder self-election + member admits) runs on the
        // broadcast/startup/claim path only; the authorised-controller cache
        // refresh runs on every check.
        if peer.is_none() {
            self.ensure_fleet_network().await;
            // Claim rendezvous follows claim state on the same cadence: the
            // LAN claim network is (re)asserted, and the claim-code network
            // comes up / goes down / rotates with claimability.
            self.ensure_claim_networks().await;
            // Same cadence, opposite policy: every *non*-fleet mesh is made
            // fully open (auto-approve), so older meshes are migrated and any
            // newly joined one is reconciled — no mesh keeps a stale approval
            // gate now that the approval queue is gone.
            self.ensure_open_meshes_auto_approve().await;
        }
        self.refresh_fleet_authorization().await;
    }

    /// Keep the claim-rendezvous networks in step with this device's claim
    /// state. Two networks, two scopes:
    ///
    ///  * the **local claim network** ([`LOCAL_CLAIM_NETWORK_ID`]) — always
    ///    joined, LAN-only (daemon signaling `strategy:"none", mdns:true`,
    ///    no STUN/TURN). Claimable presence lives here; a claimer discovers
    ///    a claimable box here with zero configuration and zero public
    ///    infrastructure. This is the default — and with public claims off,
    ///    the only — claim path.
    ///  * the **claim-code network** (`amsclaim-<code>`) — the WAN
    ///    rendezvous, joined only while this device sits claimable *and*
    ///    public claims are deliberately enabled on it (the device-local
    ///    setting or `ALLMYSTUFF_PUBLIC_CLAIMS`). The code is unguessable
    ///    and shown out-of-band (log line here; a device UI elsewhere), so
    ///    strangers can't find — let alone race-claim — the box the way
    ///    they could on a well-known open network. Kept joined through the
    ///    claimed-but-keyless window so the `Claimed` reply and the
    ///    `FleetKey` handoff can still ride it, then torn down, with the
    ///    code rotated once the fleet key lands (a code that admitted an
    ///    owner is spent).
    async fn ensure_claim_networks(self: &Arc<Self>) {
        // The always-on LAN rendezvous. Explicit empty STUN/TURN lists opt
        // out of the daemon's public defaults — this network must touch no
        // remote infrastructure at all. A duplicate NetworkAdd (already
        // joined) returns an error we ignore, same as the fleet network.
        // "Always-on" bows to one thing: the user flipping it *off* (the
        // network can't be left, only toggled, so the park store is its
        // only off switch) — re-joining here would make the toggle snap
        // back on at the next claim-state change.
        if !self.network_parked(LOCAL_CLAIM_NETWORK_ID) {
            let _ = self
                .client
                .request(&Request::NetworkAdd {
                    config: json!({
                        "id": LOCAL_CLAIM_NETWORK_ID,
                        "network_id": LOCAL_CLAIM_NETWORK_ID,
                        "label": "Local claiming (this LAN)",
                        "kind": "open",
                        "auto_approve": true,
                        "signaling": { "strategy": "none", "mdns": true },
                        "stun_servers": [],
                        "turn_servers": [],
                    }),
                })
                .await;
        }

        // The WAN rendezvous, tracking claim state.
        let claimable = self.ownership.claimable();
        let public_ok = self.ownership.public_claims_allowed();
        let keyless_claimed =
            self.ownership.owner().is_some() && self.ownership.fleet_key().is_none();
        if claimable && public_ok {
            let code = self.ownership.ensure_claim_code();
            let network = claim_code_network_id(&code);
            tracing::info!(
                "remote claiming enabled — claim code: {}  (claim this device from \
                 another machine's Fleet settings by entering that code)",
                format_claim_code(&code)
            );
            let _ = self
                .client
                .request(&Request::NetworkAdd {
                    config: json!({
                        "id": network.as_str(),
                        "network_id": network.as_str(),
                        "label": "Remote claiming",
                        "kind": "open",
                        "auto_approve": true,
                        "signaling": { "strategy": "nostr", "mdns": true },
                    }),
                })
                .await;
        } else if let Some(code) = self.ownership.claim_code() {
            if !keyless_claimed {
                // Not claimable and not waiting on a fleet-key handoff —
                // the rendezvous has no business staying up.
                let _ = self
                    .client
                    .request(&Request::NetworkRemove {
                        network: claim_code_network_id(&code),
                        purge: false,
                    })
                    .await;
                if self.ownership.owner().is_some() {
                    // Fully claimed (owner + fleet key): this code admitted
                    // an owner and is spent.
                    self.ownership.rotate_claim_code();
                }
            }
        }
        self.sync_networks().await;
    }

    /// Whether an inbound `Claim` arriving on `network` may be honored.
    /// LAN-first policy: the local claim network always may; anything else
    /// (the claim-code rendezvous, a shared public mesh from a legacy
    /// claimer) only when public claims are deliberately enabled **on this
    /// device**.
    fn claim_network_allowed(&self, network: &str) -> bool {
        network == LOCAL_CLAIM_NETWORK_ID || self.ownership.public_claims_allowed()
    }

    /// Make sure the fleet's closed network exists, is genuinely closed, and
    /// its signed roster reflects the fleet.
    ///
    /// Both sides `NetworkAdd` the network as **open** first — seeding it
    /// closed would block the founder self-election, which is only valid
    /// `open → closed`. The **owner** then proposes the `KindChange → closed`
    /// (a single-signer founder self-election that auto-ratifies, electing it
    /// Owner and making governance genuinely closed — without which the roles
    /// map stays empty and fleet-MFA guards nothing), and admits every member
    /// into the signed roster. A **member** just joins open and converges to
    /// closed from the owner's broadcast governance. All steps are idempotent;
    /// best-effort, with failures logged by the daemon.
    async fn ensure_fleet_network(self: &Arc<Self>) {
        let Some(network) = self.ownership.fleet_network_id() else {
            return;
        };
        let config = json!({
            "id": network.as_str(),
            "network_id": network.as_str(),
            "label": fleet_label(&self.ownership.fleet_name()),
            "kind": "open",
        });
        // A duplicate `NetworkAdd` (already joined) returns an error we ignore.
        let _ = self.client.request(&Request::NetworkAdd { config }).await;

        // Keep the fleet-mesh **label** converged. `NetworkAdd` is a no-op once
        // joined, so it never refreshes the label — but a rename handed down to a
        // member arrives as a fresh key+name and re-runs this. Without an explicit
        // update the member's fleet-mesh pill (and anywhere the mesh label titles
        // things) would keep the old name even though its graph fleet-name pill,
        // fed by the roster, already updated. NetworkUpdate makes the owner's
        // rename actually spread to every member's mesh label too.
        let label = fleet_label(&self.ownership.fleet_name());
        let _ = self
            .client
            .request(&Request::NetworkUpdate {
                config: json!({
                    "id": network.as_str(),
                    "network_id": network.as_str(),
                    "label": label,
                }),
            })
            .await;

        // The set of joined networks just changed — pick the fleet network up
        // everywhere: refresh `st.networks`, (re)subscribe its channels, and
        // re-advertise the `allmystuff` capability + presence on it. Without
        // this the joiner is on the fleet mesh but never advertises the app tag
        // there, so peers (e.g. the owner whose graph centres on this network)
        // see it connected-but-mesh-only — "online, not on AllMyStuff" — until
        // some unrelated network change happens to trigger a sync.
        self.sync_networks().await;

        // "Owner" for admit purposes is the **signed** role, not just the
        // structural key-holder: a device the founder granted the owner role is
        // a full owner and admits members like any other. (Founding itself is
        // still gated on `is_fleet_founder` below — only the key-minter elects
        // the genesis — but every owner runs the admit loop.)
        let is_owner =
            self.ownership.is_fleet_owner() || self.is_fleet_owner_signed(&network).await;
        // A **manager** (controller) isn't an owner but the signed governance
        // gives it authority to admit members too, so it also runs the admit
        // loop. We read the *converged* signed role, so a freshly-promoted
        // manager/owner only starts admitting once it has adopted the grant.
        let is_manager = !is_owner && self.is_fleet_manager(&network).await;
        if !is_owner {
            // A non-owner pre-rosters its **owner**. Fleet membership is mutual
            // trust established by the claim, but MyOwnMesh only auto-approves a
            // connection from a peer that's already in your roster — so without
            // this the device would be prompted to "let in" its own owner (and
            // approving it would admit the owner via the handshake). The owner
            // already pre-rosters the member at claim time; this is the
            // symmetric half. We trust our owner inherently (it owns us), so
            // there's no authority gap.
            if let Some(owner) = self.ownership.owner() {
                let _ = self
                    .client
                    .request(&Request::RosterApprove {
                        network: network.clone(),
                        device_id: pubkey_part(&owner).to_string(),
                        label: None,
                    })
                    .await;
            }
            // A plain member has no roster authority and stops here; a manager
            // continues to the admit loop.
            if !is_manager {
                return;
            }
        }

        // Custody lock: if this device enrolled a per-network TOTP, the daemon
        // requires a fresh code to *author* any governance transition — which
        // this background loop can't supply. Firing silent `mfa_code: None`
        // founds/admits would just be refused on every startup (and, pre-fix,
        // looked like "the fleet roster silently stopped updating"). So when
        // locked, skip the signed-governance steps here and let the owner author
        // founding + admits interactively from the Governance UI (with a code) —
        // which is the whole point of the lock. The local `RosterApprove` calls
        // below are NOT custody-gated (they're roster ops, not governance
        // authoring), so peer auto-approve still reflects members either way.
        let custody_locked = self.fleet_mfa_enrolled(&network).await;
        if custody_locked {
            tracing::info!(
                "fleet network {network} is custody-locked — skipping automatic \
                 found/admit; author membership changes from the Governance UI"
            );
        }

        // Found the closed governance only if we're the genuine **founder** —
        // the device that MINTED the fleet key. A structural owner that merely
        // adopted a key must NOT self-elect a parallel genesis: the engine would
        // (correctly) refuse to merge it, leaving two split-brain fleets that
        // only a deliberate leave-and-rejoin can consolidate. A manager never
        // founds either. `is_fleet_founded` reads the signed state.
        if !custody_locked
            && self.ownership.is_fleet_founder()
            && !self.is_fleet_founded(&network).await
        {
            match self
                .client
                .request(&Request::GovernanceProposeKindChange {
                    network: network.clone(),
                    to: "closed".into(),
                    mfa_code: None,
                })
                .await
            {
                Ok(r) if r.ok => {
                    tracing::info!("founded fleet closed network {network} (self-elected owner)")
                }
                Ok(r) => tracing::warn!(
                    "founding fleet network {network} refused: {}",
                    r.error.unwrap_or_else(|| "(no error)".into())
                ),
                Err(e) => tracing::warn!("founding fleet network {network} failed: {e}"),
            }
        }

        // Before the admit loop re-asserts our claimed-list, reconcile it against
        // the signed governance. A device *another* owner evicted converges out of
        // the signed roster network-wide, but that eviction never reached THIS
        // owner's local member list — it's only pruned locally by the owner who
        // authored the kick (`kick_member`). Left in place, the loop below would
        // re-sign the device into the member log (a fresh, later-stamped admit that
        // wins the last-writer-wins tie) and re-approve it — silently resurrecting
        // an evicted device across the whole fleet, so co-owners "can't see it
        // gone." Drop any locally listed device the signed logs have removed so the
        // eviction sticks here too. Best-effort: an empty set (no fleet, an older
        // daemon that doesn't report it, or a read error) prunes nothing, so a
        // transient failure never drops a live member.
        let signed_evicted = self.signed_evicted(&network).await;
        if !signed_evicted.is_empty() {
            let mut pruned = false;
            for member in self.ownership.fleet_member_ids() {
                if signed_evicted.contains(pubkey_part(&member)) {
                    tracing::info!(
                        "pruning {} from the local fleet list — the signed governance evicted it",
                        short_id(&member)
                    );
                    let _ = self.ownership.kick_member(&member);
                    pruned = true;
                }
            }
            if pruned {
                // Reflect the removal now: the authorised-controller cache and the
                // GUI's fleet roster must both drop the evicted device immediately,
                // not on the next poll.
                self.refresh_fleet_authorization().await;
                self.emit_owned().await;
            }
        }

        // Admit every fleet member by **signing** them into the closed
        // network's **member log** — a ratified `RoleGrant` authored by an owner
        // or manager. This is what makes membership signed and self-sufficient:
        // every other member derives the complete roster from the *verified*
        // log, so they no longer depend on receiving live (unsigned) roster
        // gossip while the author happens to be online. That dependency was the
        // fleet bug — a member couldn't see its co-members until the owner
        // re-gossiped. The member log is union-merged, so a manager re-asserting
        // here converges with the owner's admits instead of forking them.
        //
        // We sign in only members the log doesn't already carry. Re-granting
        // `member` to someone already signed would be a redundant transition at
        // best, and — for a device we'd promoted to manager/owner — a *demotion*
        // back to member. So pull who's already signed (any role) and skip them;
        // this also migrates fleets whose members were only ever plain
        // roster-approved before signed membership (they aren't in the log yet,
        // so they get signed now). Re-asserting on every startup is therefore
        // free once converged. We keep the local `RosterApprove` for everyone so
        // our own auto-approve and peer list reflect each member immediately,
        // before ratification mirrors the grant into the roster projection.
        let already_signed = self.signed_role_holders(&network).await;
        let me = self.local_node_id().map(|m| pubkey_part(&m).to_string());
        for member in self.ownership.fleet_member_ids() {
            let device_id = pubkey_part(&member).to_string();
            // Never author a grant over ourselves: the founder election already
            // made us Owner, and a `member` grant here would demote us.
            if Some(&device_id) == me.as_ref() {
                continue;
            }
            if !custody_locked && !already_signed.contains(&device_id) {
                let _ = self
                    .client
                    .request(&Request::GovernanceProposeRoleGrant {
                        network: network.clone(),
                        target: device_id.clone(),
                        role: "member".to_string(),
                        mfa_code: None,
                    })
                    .await;
            }
            let _ = self
                .client
                .request(&Request::RosterApprove {
                    network: network.clone(),
                    device_id,
                    label: None,
                })
                .await;
        }
    }

    /// Make every ordinary (non-fleet) mesh fully open by turning on
    /// `auto_approve`: any node that joins is admitted automatically, with no
    /// per-mesh approval gate. AllMyStuff shapes who can mesh with you through
    /// private venues, the Fleet, and Sharing — not by approving devices one by
    /// one — so the approval queue is gone and every mesh must auto-admit or
    /// peers would be stranded with no way in.
    ///
    /// New meshes are already created auto-approve by the GUI; this migrates any
    /// older mesh (made before the open default, or joined some other way) on
    /// the next launch. The fleet's own mesh is **skipped**: its membership is
    /// the signed roster (claim-based), never open admission, so a stranger can
    /// never auto-join it. Idempotent — a mesh already open is left untouched,
    /// so there is no churn after the first pass.
    async fn ensure_open_meshes_auto_approve(self: &Arc<Self>) {
        let fleet = self.ownership.fleet_network_id();
        let resp = match self.client.request(&Request::ConfigShow).await {
            Ok(r) if r.ok => r,
            _ => return,
        };
        let Some(data) = resp.data else { return };
        let Some(nets) = data.pointer("/config/networks").and_then(|v| v.as_array()) else {
            return;
        };
        // Snapshot the configs that need flipping first, so no borrow of `data`
        // is held across the awaited NetworkUpdate calls below.
        let to_open: Vec<Value> = nets
            .iter()
            .filter(|n| {
                let nid = n
                    .get("network_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if nid.is_empty() {
                    return false;
                }
                // Never auto-open the fleet's closed mesh — its members are the
                // signed roster, not anyone who connects.
                if fleet.as_deref() == Some(nid) {
                    return false;
                }
                // Already open → nothing to do (keeps this idempotent).
                !n.get("auto_approve")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        for mut config in to_open {
            let nid = config
                .get("network_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            // Safety net for closed networks. The fleet-id match above is the
            // primary skip, but if `fleet_network_id` is momentarily unset
            // (mid-leave, or the ownership store still loading) the fleet's mesh
            // could slip past it — and auto-opening a *closed* governance network
            // would let anyone connect straight into it. So never open a network
            // whose **signed** governance is closed, whatever its config `kind`
            // (the fleet mesh is created `open` then transitioned, so the config
            // field lies) or our fleet state says.
            if self.is_closed_governance(&nid).await {
                tracing::debug!(
                    "leaving closed-governance mesh {nid} approval-gated (not auto-opened)"
                );
                continue;
            }
            if let Some(obj) = config.as_object_mut() {
                obj.insert("auto_approve".into(), Value::Bool(true));
            }
            // A full-config round-trip (only `auto_approve` changed) — the same
            // shape `network_set_enabled` parks and re-adds, so the daemon
            // hot-applies it without dropping live peers.
            match self
                .client
                .request(&Request::NetworkUpdate { config })
                .await
            {
                Ok(r) if r.ok => tracing::info!("opened mesh {nid} — auto-approve on (fully open)"),
                Ok(r) => tracing::warn!(
                    "couldn't open mesh {nid}: {}",
                    r.error.unwrap_or_else(|| "(no error)".into())
                ),
                Err(e) => tracing::warn!("couldn't open mesh {nid}: {e}"),
            }
        }
    }

    /// Whether `network`'s **authoritative** governance — the signed state log,
    /// not the config's initial `kind` field — is closed. A closed network must
    /// never be auto-opened: its membership is the signed roster, not anyone who
    /// connects. Mirrors the GovernanceState read in [`Mesh::is_fleet_founded`].
    /// Any error reads as *not* closed: the fleet-id skip in
    /// [`Mesh::ensure_open_meshes_auto_approve`] remains the first line of
    /// defence, and an ordinary open mesh has no governance log to consult.
    async fn is_closed_governance(self: &Arc<Self>, network: &str) -> bool {
        let data = match self
            .client
            .request(&Request::GovernanceState {
                network: network.to_string(),
            })
            .await
        {
            Ok(r) if r.ok => r.data.unwrap_or(Value::Null),
            _ => return false,
        };
        data.pointer("/state/kind").and_then(|v| v.as_str()) == Some("closed")
    }

    /// Whether this device already holds the founder-Owner role on the fleet's
    /// closed network — i.e. the `KindChange → closed` self-election has
    /// ratified. Reads the signed governance state; on any error assumes
    /// not-yet-founded (a redundant propose is cheaper to avoid than a missed
    /// one is to recover). `me` is matched in bare-pubkey form, as the roles
    /// map keys it.
    async fn is_fleet_founded(self: &Arc<Self>, network: &str) -> bool {
        let Some(me) = self.local_node_id() else {
            return false;
        };
        let me = pubkey_part(&me).to_string();
        let data = match self
            .client
            .request(&Request::GovernanceState {
                network: network.to_string(),
            })
            .await
        {
            Ok(r) if r.ok => r.data.unwrap_or(Value::Null),
            _ => return false,
        };
        let state = data.get("state").unwrap_or(&Value::Null);
        let closed = state.get("kind").and_then(|v| v.as_str()) == Some("closed");
        let i_am_owner = state
            .get("roles")
            .and_then(|v| v.as_object())
            .and_then(|roles| {
                roles
                    .iter()
                    .find(|(k, _)| pubkey_part(k) == me)
                    .map(|(_, v)| v.as_str() == Some("owner"))
            })
            .unwrap_or(false);
        closed && i_am_owner
    }

    /// Whether this device holds a custody (TOTP) lock on `network`'s
    /// governance. Once enrolled, the daemon refuses to *author* a governance
    /// transition (found, admit, promote, evict) without a fresh second-factor
    /// code — so this background found/admit loop, which has no code to give,
    /// must not fire silent `mfa_code: None` proposals that the daemon will only
    /// reject on every startup. Any daemon/parse error reads as *not* enrolled,
    /// so the automatic path keeps working on the common (unlocked) fleet.
    /// `enrolled` is the field [`Request::GovernanceMfaStatus`] returns.
    async fn fleet_mfa_enrolled(self: &Arc<Self>, network: &str) -> bool {
        match self
            .client
            .request(&Request::GovernanceMfaStatus {
                network: network.to_string(),
            })
            .await
        {
            Ok(r) if r.ok => r
                .data
                .and_then(|d| d.get("enrolled").and_then(|v| v.as_bool()))
                .unwrap_or(false),
            _ => false,
        }
    }

    /// This device's **signed** governance role in `network` — `"owner"`,
    /// `"controller"`, or `"member"` — or `None` if it holds none / the state
    /// can't be read. This is the authoritative answer for "what am I on the
    /// fleet": a device the founder *granted* the owner role is an owner here,
    /// even though it isn't the structural key-holder ([`Ownership::is_fleet_owner`]).
    /// Owners are owners; there is no second-class owner. `me` is matched in
    /// bare-pubkey form, as the roles map keys it.
    async fn fleet_signed_role(&self, network: &str) -> Option<String> {
        let me = pubkey_part(&self.local_node_id()?).to_string();
        let data = match self
            .client
            .request(&Request::GovernanceState {
                network: network.to_string(),
            })
            .await
        {
            Ok(r) if r.ok => r.data.unwrap_or(Value::Null),
            _ => return None,
        };
        data.get("state")
            .and_then(|v| v.get("roles"))
            .and_then(|v| v.as_object())
            .and_then(|roles| {
                roles
                    .iter()
                    .find(|(k, _)| pubkey_part(k) == me)
                    .and_then(|(_, v)| v.as_str())
                    .map(str::to_string)
            })
    }

    /// The fleet network's governed topology — the owner-signed,
    /// network-wide shape (daemon ≥ 0.2.36) — as the raw snapshot JSON
    /// (`{"kind":"hubs","hubs":[…],"spoke_redundancy":…}` etc). `None`
    /// when the network isn't governed, the daemon predates governed
    /// topology (no `topology` key in the snapshot), or the state can't
    /// be read — callers treat all three as "no governed shape".
    async fn fleet_governed_topology(&self, network: &str) -> Option<Value> {
        let data = match self
            .client
            .request(&Request::GovernanceState {
                network: network.to_string(),
            })
            .await
        {
            Ok(r) if r.ok => r.data.unwrap_or(Value::Null),
            _ => return None,
        };
        let topo = data.get("state")?.get("topology")?.clone();
        if topo.is_null() {
            None
        } else {
            Some(topo)
        }
    }

    /// True if this device is a **signed owner** of `network` — granted the
    /// owner role in the governance log — regardless of whether it minted the
    /// fleet key. Management authority (evict, admit, promote) keys on this, not
    /// on the structural key-holder check, so a granted owner is a full owner.
    async fn is_fleet_owner_signed(&self, network: &str) -> bool {
        self.fleet_signed_role(network).await.as_deref() == Some("owner")
    }

    /// True if this device holds the **manager** (controller) role in `network`.
    async fn is_fleet_manager(&self, network: &str) -> bool {
        self.fleet_signed_role(network).await.as_deref() == Some("controller")
    }

    /// The device ids (bare pubkey form) that already hold *any* signed role in
    /// `network`'s governance log — owners, controllers, and members alike.
    ///
    /// The fleet-admit path uses this to sign in only members the log doesn't
    /// already carry. Re-granting `member` to a device already in the log is a
    /// redundant transition at best and, for one we'd promoted to
    /// controller/owner, a *demotion* back to member. On any daemon/parse error
    /// this returns the empty set, so the caller falls back to re-asserting the
    /// grant — idempotent and safe, just chattier than necessary.
    async fn signed_role_holders(
        self: &Arc<Self>,
        network: &str,
    ) -> std::collections::HashSet<String> {
        let data = match self
            .client
            .request(&Request::GovernanceState {
                network: network.to_string(),
            })
            .await
        {
            Ok(r) if r.ok => r.data.unwrap_or(Value::Null),
            _ => return std::collections::HashSet::new(),
        };
        data.get("state")
            .and_then(|s| s.get("roles"))
            .and_then(|v| v.as_object())
            .map(|roles| roles.keys().map(|k| pubkey_part(k).to_string()).collect())
            .unwrap_or_default()
    }

    /// The canonical device ids the fleet's signed logs have **removed** —
    /// evicted (or member-tier revoked), the authoritative "no longer in the
    /// fleet" set the daemon projects from the member log. `ensure_fleet_network`
    /// uses it to prune this owner's local claimed-list of a device *another*
    /// owner evicted, whose eviction converged the signed roster but never
    /// reached this device's local list — so the background admit loop stops
    /// resurrecting it. Empty on any daemon/parse error (and against an older
    /// daemon that doesn't report the field), so a transient read failure or a
    /// version skew never prunes a live member — it just falls back to the old
    /// (re-asserting) behaviour.
    async fn signed_evicted(self: &Arc<Self>, network: &str) -> std::collections::HashSet<String> {
        let data = match self
            .client
            .request(&Request::GovernanceState {
                network: network.to_string(),
            })
            .await
        {
            Ok(r) if r.ok => r.data.unwrap_or(Value::Null),
            _ => return std::collections::HashSet::new(),
        };
        data.get("evicted")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|k| pubkey_part(k).to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Refresh the authorised-controller cache ([`Mesh::fleet_authorized`])
    /// from the fleet's closed-network **signed roster** (`RosterList`). No
    /// fleet → clear it (only the owner, via the direct check in
    /// `sender_may_control`, may control). Daemon unreachable → keep the prior
    /// cache rather than briefly denying a legitimate controller.
    async fn refresh_fleet_authorization(self: &Arc<Self>) {
        let Some(network) = self.ownership.fleet_network_id() else {
            self.fleet_authorized.lock().clear();
            return;
        };
        let data = match self.client.request(&Request::RosterList { network }).await {
            Ok(r) if r.ok => r.data.unwrap_or(Value::Null),
            _ => return,
        };
        let mut set = std::collections::HashSet::new();
        if let Some(arr) = data.get("roster").and_then(|v| v.as_array()) {
            for e in arr {
                if let Some(id) = e.get("device_id").and_then(|v| v.as_str()) {
                    set.insert(pubkey_part(id).to_string());
                }
            }
        }
        *self.fleet_authorized.lock() = set;
    }

    /// Fold one peer's passive clock-skew sample (from its presence advert's
    /// `sent_at` stamp) into the network verdict, and raise / clear the
    /// out-of-sync warning on the transitions.
    ///
    /// The estimate is the conservative median across peers with a fresh
    /// sample, so one machine with a broken clock reads as *that peer's*
    /// problem (its own node warns, against all of *its* peers) — only when
    /// the majority of the network disagrees with us the same way does this
    /// device conclude its own clock is off. Motivated by real damage: the
    /// fleet's signed member-log converges last-writer-wins on wall-clock
    /// stamps (a skewed clock can strand a device evicted — the "remote
    /// control silently refused" failure), custody TOTP tolerates ±30 s,
    /// and cross-device timestamps stop lining up. Entirely passive — built
    /// from adverts that were flowing anyway, no extra calls to any node.
    fn note_peer_clock(&self, peer: &str, sample_ms: i64) {
        use std::sync::atomic::Ordering;
        const SAMPLE_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);
        let (estimate, peers) = {
            let mut map = self.peer_clock_skew.lock();
            map.insert(peer.to_string(), (sample_ms, std::time::Instant::now()));
            map.retain(|_, (_, at)| at.elapsed() < SAMPLE_TTL);
            let samples: Vec<i64> = map.values().map(|(s, _)| *s).collect();
            (conservative_median(&samples), samples.len())
        };
        let Some(skew_ms) = estimate else { return };
        let warned = self.clock_skew_warned.load(Ordering::SeqCst);
        if !warned && skew_ms.abs() >= CLOCK_SKEW_WARN_MS {
            self.clock_skew_warned.store(true, Ordering::SeqCst);
            let secs = skew_ms.abs() as f64 / 1000.0;
            let direction = if skew_ms > 0 { "behind" } else { "ahead of" };
            let message = if peers >= 2 {
                format!(
                    "This device's clock is ~{secs:.0}s {direction} the rest of the network — \
                     fleet roster updates and cross-device timestamps can misbehave. Sync this \
                     machine's clock (NTP)."
                )
            } else {
                format!(
                    "This device's clock and its peer's disagree by ~{secs:.0}s — one of the \
                     two is wrong. Sync both machines' clocks (NTP)."
                )
            };
            tracing::warn!("{message} (skew {skew_ms} ms across {peers} peer(s))");
            self.sink.emit(
                "allmystuff://clock-skew",
                serde_json::json!({
                    "state": "warn",
                    "skew_ms": skew_ms,
                    "peers": peers,
                    "message": message,
                    "source": "presence",
                }),
            );
        } else if warned && skew_ms.abs() <= CLOCK_SKEW_CLEAR_MS {
            self.clock_skew_warned.store(false, Ordering::SeqCst);
            tracing::info!("this device's clock is back in sync with the network");
            self.sink.emit(
                "allmystuff://clock-skew",
                serde_json::json!({
                    "state": "clear",
                    "skew_ms": skew_ms,
                    "peers": peers,
                    "message": "This device's clock is back in sync with the network.",
                    "source": "presence",
                }),
            );
        }
    }

    /// Send this node's presence profile straight to one peer — the
    /// targeted half of `broadcast_presence`, for a peer that just
    /// connected or restarted and so has never heard us.
    async fn send_presence_to(&self, peer: &str) {
        let profile = { self.state.lock().profile.clone() };
        let Some(mut profile) = profile else { return };
        // Same send-time stamp as `broadcast_presence` — a passive
        // clock-skew sample for the receiver.
        profile.sent_at = unix_now_ms();
        let Some(network) = self.network_for_peer(peer) else {
            return;
        };
        // Same per-network claimable scoping as `broadcast_presence`.
        profile.claimable = profile.claimable && self.claimable_advertised_on(&network);
        if let Ok(payload) = serde_json::to_value(&profile) {
            let _ = self
                .client
                .request(&Request::ChannelSendTo {
                    network,
                    channel: CHANNEL_PRESENCE.to_string(),
                    peer: pubkey_part(peer).to_string(),
                    payload,
                })
                .await;
        }
    }

    /// Front-end command: leave the fleet this device belongs to. Tell the
    /// owner first (so it evicts us from the signed roster instead of believing
    /// we're still a member — the leave-side mirror of the owner's kick), then
    /// drop the local credential, tear out of the fleet's closed network, and —
    /// since membership follows ownership — let any recorded owner go and
    /// re-advertise unowned.
    pub async fn fleet_leave(self: &Arc<Self>) -> Result<(), String> {
        // Notify *before* we leave the network, while we can still route a
        // control frame on the fleet mesh.
        if self.ownership.is_fleet_owner() {
            // We're the owner dissolving our own fleet — there's no owner to
            // tell. Tell every member to release instead, so they stop deriving
            // the (now-defunct) closed network and showing each other as fleet.
            // Best-effort per member (mirrors fleet_kick's direct Release); an
            // offline member just keeps a dead key until it next reconciles.
            for member in self.ownership.fleet_member_ids() {
                let _ = self
                    .send_control(
                        &member,
                        &ControlMessage::Ownership(OwnershipControl::Release),
                    )
                    .await;
            }
        } else if let Some(owner) = self.ownership.owner() {
            // We're a member: tell the owner so it evicts us from the signed
            // roster. Best-effort — surface the failure (don't swallow it) so
            // it's diagnosable; our re-advertised "unowned" presence below is
            // the backstop (the owner drops a member that answers to a
            // different owner / none).
            if let Err(e) = self
                .send_control(
                    &owner,
                    &ControlMessage::Ownership(OwnershipControl::FleetDeparted),
                )
                .await
            {
                tracing::warn!(
                    "couldn't tell the fleet owner we left ({e}); relying on our unowned re-advert to clear us from its roster"
                );
            }
        }
        // Leaving clears all local fleet/ownership state atomically (owner
        // included). It returns the closed network to tear out of, or `None`
        // when there was no key to derive one (an owned-but-keyless member that
        // never joined a network — it has still left); `Err` only when there
        // was genuinely nothing to leave.
        let network = self.ownership.leave_fleet()?;
        if let Some(network) = network {
            tracing::info!("leaving the fleet — forgetting closed network {network}");
            let _ = self
                .client
                // A deliberate leave: purge the signed governance state + roster
                // so a later rejoin can't reload a stale (forked) genesis.
                .request(&Request::NetworkRemove {
                    network,
                    purge: true,
                })
                .await;
        } else {
            tracing::info!(
                "left the fleet (was claimed but keyless — no closed network to forget)"
            );
        }
        self.refresh_fleet_authorization().await;
        self.refresh_profile_ownership().await;
        self.emit_owned().await;
        Ok(())
    }

    /// Danger Zone: leave the fleet **and** forget every network on the daemon —
    /// clears this device's fleet membership and purges each mesh's roster +
    /// signed governance state, keeping the device identity. A clean networking
    /// slate for a wedged node. Leaves the fleet first (clears our ownership +
    /// purges the fleet's closed network), then tells the daemon to forget the
    /// rest; the daemon exits so a fresh one reloads clean, and the GUI restarts
    /// the app around it. Best-effort per step — we're resetting regardless.
    pub async fn reset_networking(self: &Arc<Self>) -> Result<(), String> {
        let _ = self.fleet_leave().await;
        if let Err(e) = self.client.request(&Request::ForgetAllNetworks).await {
            // A pre-reset-op daemon can't parse it; the fleet leave above still
            // did the important part. Surface it but don't fail the reset.
            tracing::warn!("reset networking: daemon forget-all errored: {e}");
        }
        Ok(())
    }

    /// Danger Zone: factory reset — wipe this device back to brand-new. Clears
    /// our local ownership record first (so the node can't re-persist
    /// `allmystuff-ownership.json` after the daemon deletes it), then tells the
    /// daemon to wipe its **entire** state directory (`~/.myownmesh`: identity,
    /// config, every network, and our co-located ownership file) and exit. The
    /// GUI restarts the app; a fresh node + daemon come up on empty state with a
    /// new identity. The daemon's response may race its own exit, so a transport
    /// error after the request is treated as "reset underway", not a failure.
    pub async fn factory_reset(self: &Arc<Self>) -> Result<(), String> {
        // Quiesce our ownership writer so it can't rewrite the file the daemon is
        // about to delete. Best-effort — the daemon wipe is the authority, and
        // we're restarting the whole stack regardless.
        let _ = self.ownership.leave_fleet();
        self.emit_owned().await;
        match self.client.request(&Request::FactoryReset).await {
            Ok(_) => Ok(()),
            Err(e) => {
                tracing::warn!(
                    "factory reset: daemon request errored (it is likely already exiting): {e}"
                );
                Ok(())
            }
        }
    }

    /// Front-end command: kick `device` out of the fleet. Only the fleet
    /// **owner** can — eviction is an owner-authority governance act on the
    /// closed network. The signed `Evict` propagates the removal to every
    /// member (so the device loses control authorisation everywhere, even if
    /// it's lost/stolen), and a best-effort `Release` tells a cooperative
    /// device to eject itself immediately. `code` is the owner's custody
    /// second factor when fleet MFA is enrolled (the GUI prompts for it);
    /// otherwise it's `None`.
    pub async fn fleet_kick(
        self: &Arc<Self>,
        device: String,
        code: Option<String>,
    ) -> Result<(), String> {
        let network = self
            .ownership
            .fleet_network_id()
            .ok_or("this device isn't in a fleet")?;
        // Authority mirrors the daemon's Evict quorum, keyed on the **signed**
        // role — not the structural key-holder. A signed owner (even one the
        // founder granted, not the key-minter) may evict anyone; a manager may
        // evict managers/members. Gating on the structural `is_fleet_owner`
        // alone made a granted owner a second-class owner that couldn't evict.
        // The daemon is the final arbiter (it rejects an under-powered evict);
        // this local check just avoids a doomed request.
        let structural_owner = self.ownership.is_fleet_owner();
        if !structural_owner
            && !self.is_fleet_owner_signed(&network).await
            && !self.is_fleet_manager(&network).await
        {
            return Err("only a fleet owner or a manager can remove a device".into());
        }
        // Keep the owner's local re-admit list honest so a kicked device isn't
        // re-admitted next `ensure` — a no-op for a manager (empty list). The
        // returned id equals `network`; we keep the one from `fleet_network_id`.
        self.ownership.kick_member(&device)?;
        let target = pubkey_part(&device).to_string();
        // Tell the device directly FIRST, while it's still a live peer on the
        // fleet mesh. The `Evict` below ratifies synchronously on this daemon
        // and drops the peer session, so a `Release` sent afterwards would
        // find no delivery path — the device would be evicted from everyone's
        // roster but never told to reset itself. Order matters for the KVM
        // *unclaim*: a cooperative device must receive this to leave its
        // meshes and return to claim mode. A lost/stolen device simply
        // ignores it; the propagating `Evict` still does its job.
        let _ = self
            .send_control(
                &device,
                &ControlMessage::Ownership(OwnershipControl::Release),
            )
            .await;
        tracing::info!(
            "evicting {} from fleet network {network}",
            short_id(&device)
        );
        let resp = self
            .client
            .request(&Request::GovernanceProposeEvict {
                network,
                target,
                mfa_code: code,
            })
            .await;
        match resp {
            Ok(r) if r.ok => {}
            Ok(r) => {
                return Err(r
                    .error
                    .unwrap_or_else(|| "couldn't evict the device".into()))
            }
            Err(e) => return Err(e.to_string()),
        }
        self.refresh_fleet_authorization().await;
        self.emit_owned().await;
        Ok(())
    }

    /// Internal: drop `device` from the fleet *locally* — a plain roster
    /// remove, not the propagating governance `Evict`. Used for automatic
    /// roster cleanup (a member told us it left, or a device reappeared under
    /// a new owner) where there's no user to supply an MFA code and the device
    /// is already gone anyway, so a local removal that keeps the owner's view
    /// honest is the right, friction-free tool. Best-effort.
    async fn fleet_drop_member(self: &Arc<Self>, device: String) {
        let Ok(network) = self.ownership.kick_member(&device) else {
            return;
        };
        let target = pubkey_part(&device).to_string();
        tracing::info!(
            "dropping {} from the fleet roster (local)",
            short_id(&device)
        );
        let _ = self
            .client
            .request(&Request::RosterRemove {
                network,
                device_id: target,
            })
            .await;
        self.refresh_fleet_authorization().await;
        self.emit_owned().await;
    }

    /// Front-end command: name (or rename) the fleet. Owner-authoritative:
    /// the name is set locally, pushed onto the closed network's label, and —
    /// since the owner is the source of truth for the fleet name — re-handed
    /// to every member so it propagates instead of having to be set on each
    /// device. (Members got the name with their fleet key at claim time; a
    /// rename re-sends it.) The UI refreshes from `allmystuff://owned`.
    pub async fn fleet_set_name(self: &Arc<Self>, name: String) -> Result<(), String> {
        self.ownership.set_fleet_name(&name)?;
        tracing::info!("fleet named {:?}", self.ownership.fleet_name());
        if let Some(network) = self.ownership.fleet_network_id() {
            let config = json!({
                "id": network.as_str(),
                "network_id": network.as_str(),
                "label": fleet_label(&self.ownership.fleet_name()),
            });
            let _ = self
                .client
                .request(&Request::NetworkUpdate { config })
                .await;
        }
        // Re-hand the (now-renamed) fleet key to every member so the name
        // converges across the fleet. Owner-only — a member has no members to
        // notify, and the name is the owner's to set.
        if self.ownership.is_fleet_owner() {
            let me = self.local_node_id().map(|m| pubkey_part(&m).to_string());
            for member in self.ownership.fleet_member_ids() {
                if Some(pubkey_part(&member).to_string()) == me {
                    continue;
                }
                self.send_fleet_key(&member).await;
            }
        }
        self.emit_owned().await;
        Ok(())
    }

    /// Front-end command: grant `device` a fleet role. `role` is the UI term
    /// — "manager" (a controller: can admit members) or "owner" (full
    /// authority, co-signs governance). Authoring a role grant is an owner
    /// authority act on the closed network; the daemon enforces the quorum and
    /// rejects the proposal if this device lacks the authority, so we just
    /// float it and surface any refusal. The roster's role projection updates
    /// once it ratifies, and the GUI refreshes from `allmystuff://owned`.
    pub async fn fleet_grant_role(
        self: &Arc<Self>,
        device: String,
        role: String,
        code: Option<String>,
    ) -> Result<(), String> {
        let network = self
            .ownership
            .fleet_network_id()
            .ok_or("this device isn't in a fleet")?;
        // Map the UI's "manager" onto MyOwnMesh's "controller".
        let role = match role.as_str() {
            "manager" | "controller" => "controller",
            "owner" => "owner",
            other => return Err(format!("unknown fleet role: {other}")),
        };
        let target = pubkey_part(&device).to_string();
        tracing::info!("granting {role} to {} on {network}", short_id(&device));
        let resp = self
            .client
            .request(&Request::GovernanceProposeRoleGrant {
                network,
                target,
                role: role.to_string(),
                mfa_code: code,
            })
            .await;
        match resp {
            Ok(r) if r.ok => {}
            Ok(r) => return Err(r.error.unwrap_or_else(|| "couldn't grant the role".into())),
            Err(e) => return Err(e.to_string()),
        }
        self.refresh_fleet_authorization().await;
        self.emit_owned().await;
        Ok(())
    }

    /// Front-end command: designate the fleet's infra hubs — the owner-signed,
    /// network-wide shape every member's daemon converges onto (≥ 0.2.36).
    /// A non-empty `hubs` proposes the hub tier (hubs full-mesh each other,
    /// every other member rides `redundancy` of them); an empty list proposes
    /// `full_mesh`, the shape a fleet has before anyone designates hubs. The
    /// daemon enforces owner authority; we float the proposal and surface any
    /// refusal — including the "op unknown" parse error a pre-0.2.36 daemon
    /// gives back, translated into an update hint.
    pub async fn fleet_set_hubs(
        self: &Arc<Self>,
        hubs: Vec<String>,
        redundancy: Option<u32>,
        code: Option<String>,
    ) -> Result<(), String> {
        let network = self
            .ownership
            .fleet_network_id()
            .ok_or("this device isn't in a fleet")?;
        let canon: Vec<String> = hubs
            .iter()
            .map(|h| pubkey_part(h).to_string())
            .filter(|h| !h.is_empty())
            .collect();
        let (topology, hub) = if canon.is_empty() {
            ("full_mesh".to_string(), None)
        } else {
            let spec = match redundancy {
                Some(r) => format!("{}:{r}", canon.join(",")),
                None => canon.join(","),
            };
            ("hubs".to_string(), Some(spec))
        };
        tracing::info!(
            "proposing fleet topology {topology} ({} hubs) on {network}",
            canon.len()
        );
        let resp = self
            .client
            .request(&Request::GovernanceProposeTopology {
                network,
                topology,
                hub,
                mfa_code: code,
            })
            .await;
        match resp {
            Ok(r) if r.ok => {}
            Ok(r) => {
                let msg = r
                    .error
                    .unwrap_or_else(|| "couldn't set the fleet topology".into());
                // A pre-0.2.36 daemon can't parse the op at all — its serde
                // error reads like gibberish in the UI, so translate it.
                if msg.contains("unknown variant") || msg.contains("expected one of") {
                    return Err(
                        "the mesh daemon on this device predates governed topology — \
                         it needs 0.2.36+ (it self-updates shortly after release)"
                            .into(),
                    );
                }
                return Err(msg);
            }
            Err(e) => return Err(e.to_string()),
        }
        self.emit_owned().await;
        Ok(())
    }

    /// Front-end command: withdraw `device`'s fleet role — revoke it back to a
    /// plain member. Used for "withdraw as manager / owner". Like a grant, the
    /// daemon enforces who may revoke (authority over the target's current
    /// role); we float the proposal and surface any refusal.
    pub async fn fleet_revoke_role(
        self: &Arc<Self>,
        device: String,
        code: Option<String>,
    ) -> Result<(), String> {
        let network = self
            .ownership
            .fleet_network_id()
            .ok_or("this device isn't in a fleet")?;
        let target = pubkey_part(&device).to_string();
        tracing::info!("revoking role from {} on {network}", short_id(&device));
        let resp = self
            .client
            .request(&Request::GovernanceProposeRoleRevoke {
                network,
                target,
                mfa_code: code,
            })
            .await;
        match resp {
            Ok(r) if r.ok => {}
            Ok(r) => {
                return Err(r
                    .error
                    .unwrap_or_else(|| "couldn't withdraw the role".into()))
            }
            Err(e) => return Err(e.to_string()),
        }
        self.refresh_fleet_authorization().await;
        self.emit_owned().await;
        Ok(())
    }

    /// Re-read the joined networks, (re)subscribe every channel on each, then
    /// re-advertise. Called after the set of networks changes (create / join /
    /// leave) or a network's transport is restarted by a signaling/STUN/TURN
    /// edit — so the session follows the user across *every* network they're
    /// on, not just the ones present at launch. Re-subscribing an existing
    /// channel is idempotent on the daemon.
    pub async fn sync_networks(self: &Arc<Self>) {
        let _sync = self.network_sync_serial.lock().await;
        let epoch = self.daemon_session_epoch.load(Ordering::SeqCst);
        let client_id = { self.state.lock().client_id };
        let Some(client_id) = client_id else { return };
        let networks = self.fetch_networks().await;
        if !self.daemon_context_is_current(epoch, client_id) {
            return;
        }
        let primary = networks.first().cloned();
        let generation = {
            let mut st = self.state.lock();
            if st.client_id != Some(client_id)
                || self.daemon_session_epoch.load(Ordering::SeqCst) != epoch
            {
                return;
            }
            let rotate_existing = st.networks == networks;
            st.network_generation = st.network_generation.wrapping_add(1);
            reconcile_network_epochs(&mut st, &networks, rotate_existing);
            st.networks = networks.clone();
            st.network = primary.clone();
            st.network_generation
        };
        let (replay_peers, missing_routes) = self.retire_unjoined_route_paths().await;
        // A network reset (one disabled, removed, or left — its config_id is
        // gone from the joined set) leaves behind ghosts: peers and the
        // network-derived data we cached for them while it was up. Drop those
        // now so the graph reflects reality. This clears *network* data only —
        // long-lived state (shares, fleet membership + the signed-roster cache,
        // the saved networks, exposed sites) is untouched (see
        // [`Mesh::prune_unjoined_peers`]).
        self.subscribe_channels(client_id, &networks).await;
        if !self.daemon_context_is_current(epoch, client_id)
            || self.state.lock().network_generation != generation
        {
            return;
        }
        // The joined set changed — re-learn each connected peer's network from
        // the daemon peer list so a peer reachable only on a newly-arrived or
        // re-enabled mesh (e.g. the fleet network) is addressed there, not the
        // primary fallback.
        self.refresh_peer_networks().await;
        if !self.daemon_context_is_current(epoch, client_id)
            || self.state.lock().network_generation != generation
        {
            return;
        }
        // The fetch/commit/subscription/peer-refresh transaction is complete.
        // Release the sync gate before processing Session effects because an
        // ownership effect can legitimately request another network sync.
        drop(_sync);
        // Prune only after the daemon's surviving per-network peer sets have
        // been refreshed. Otherwise a peer reachable on both removed A and
        // surviving B can be torn down solely because its last frame used A.
        self.prune_unjoined_peers().await;
        if !self.daemon_context_is_current(epoch, client_id)
            || self.state.lock().network_generation != generation
        {
            return;
        }
        for (peer, route_id, incarnation) in missing_routes {
            let message = ControlMessage::Route(RouteControl::MissingRoute {
                route_id: route_id.clone(),
                incarnation,
            });
            if let Err(error) = self.send_control(&peer, &message).await {
                tracing::warn!(
                    route = %route_id,
                    peer = %short_id(&peer),
                    error = %error,
                    "could not request a fresh route lifetime after its data-plane network disappeared"
                );
            }
        }
        for peer in replay_peers {
            Box::pin(self.replay_desired_routes(Some(&peer))).await;
        }
        // The joined set just changed (a create / join / import / re-enable, or
        // the fleet network arriving). Reconcile open-mesh policy now so a mesh
        // doesn't wait for the next ownership broadcast to drop its approval
        // gate — in particular a legacy mesh just **re-enabled** from its parked
        // config (which kept `auto_approve: false`) would otherwise reject
        // joiners with no UI to admit them until that later pass.
        self.ensure_open_meshes_auto_approve().await;
        self.advertise_capabilities().await;
        self.broadcast_presence().await;
        self.emit_snapshot();
    }

    /// Retire every exact route whose immutable data-plane network is no
    /// longer joined. Local media stops before any fresh Offer is allowed.
    /// Outbound user intent is replayed by the caller after subscriptions and
    /// peer reachability have converged. For inbound routes, the owning peer is
    /// asked to mint the successor lifetime on a surviving app-data path.
    async fn retire_unjoined_route_paths(
        self: &Arc<Self>,
    ) -> (
        std::collections::BTreeSet<String>,
        Vec<(String, String, Option<String>)>,
    ) {
        let lost_keys = {
            let state = self.state.lock();
            state
                .route_networks
                .iter()
                .filter(|(_, pin)| {
                    !state.networks.contains(&pin.network)
                        || state.network_epochs.get(&pin.network).copied()
                            != Some(pin.network_epoch)
                })
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>()
        };
        let mut replay_peers = std::collections::BTreeSet::new();
        let mut missing_routes = Vec::new();
        for (route_id, incarnation) in lost_keys {
            let lifecycle = self.lock_route_lifecycle(&route_id).await;
            let facts = {
                let mut state = self.state.lock();
                let key = (route_id.clone(), incarnation.clone());
                let still_lost = state.route_networks.get(&key).is_some_and(|pin| {
                    !state.networks.contains(&pin.network)
                        || state.network_epochs.get(&pin.network).copied()
                            != Some(pin.network_epoch)
                });
                if !still_lost {
                    None
                } else {
                    let facts = state
                        .session
                        .as_ref()
                        .and_then(|session| session.route(&route_id))
                        .filter(|route| route.incarnation == incarnation)
                        .map(|route| (route.peer.to_string(), route.origin, route.is_active()));
                    // A same-id successor may have replaced the lost lifetime
                    // while this retirement task waited for the route lock.
                    // Retire Session state only when it is still the exact
                    // incarnation represented by the stale pin.
                    if facts.is_some() {
                        if let Some(session) = state.session.as_mut() {
                            let _ = session.teardown(&route_id);
                        }
                    }
                    state.route_networks.remove(&key);
                    facts
                }
            };
            let Some((peer, origin, was_active)) = facts else {
                drop(lifecycle);
                continue;
            };
            if was_active {
                self.apply_stop_media_locked(route_id.clone(), incarnation.clone());
            }
            let desired_matches = self
                .desired_routes
                .lock()
                .get(&route_id)
                .is_some_and(|route| {
                    pubkey_part(&route.peer) == pubkey_part(&peer)
                        && route.current_incarnation == incarnation
                });
            if origin == allmystuff_session::Origin::Outbound && desired_matches {
                replay_peers.insert(peer);
            } else if origin == allmystuff_session::Origin::Inbound {
                missing_routes.push((peer, route_id, incarnation));
            }
            drop(lifecycle);
        }
        (replay_peers, missing_routes)
    }

    /// Clear the ephemeral, network-derived caches for peers no longer
    /// reachable on any joined network — what a network reset (a disabled,
    /// removed, or left network) leaves stale. For each such peer we drop the
    /// live session entry (tearing down any routes to it) and its per-peer
    /// presence caches: the last-seen network, advertised features, and boot
    /// id. A peer still reachable on a network that survived the reset keeps
    /// its caches and re-converges on its next advert; one only on the gone
    /// network is forgotten outright.
    ///
    /// Deliberately scoped to *network* data. Long-lived state survives a
    /// reset untouched: durable shares ([`Mesh::shares`]), fleet membership and
    /// its closed-network signed-roster cache ([`Mesh::ownership`] /
    /// [`Mesh::fleet_authorized`]), the saved network configs, and the exposed
    /// sites set are all per-device or per-person, not per-network, so a
    /// network coming and going never drops them.
    async fn prune_unjoined_peers(self: &Arc<Self>) {
        let (effects, dropped) = {
            let mut st = self.state.lock();
            let joined: std::collections::HashSet<String> = st.networks.iter().cloned().collect();
            for paths in st.peer_networks.values_mut() {
                paths.retain_joined(&joined);
                if paths
                    .preferred
                    .as_ref()
                    .is_some_and(|network| !paths.contains(network))
                {
                    paths.preferred = None;
                }
            }
            st.peer_links
                .retain(|(network, _), _| joined.contains(network));
            // A peer is stale only when no surviving network still proves it
            // reachable. The former single last-seen slot dropped multi-homed
            // peers merely because their most recent advert used a removed
            // network.
            let stale: std::collections::HashSet<String> = st
                .peer_networks
                .iter()
                .filter(|(_, paths)| paths.is_empty())
                .map(|(peer, _)| peer.clone())
                .collect();
            if stale.is_empty() {
                return;
            }
            for peer in &stale {
                st.peer_networks.remove(peer);
                st.peer_features.remove(peer);
                st.peer_boots.remove(peer);
                st.peer_retired_boots.remove(peer);
                st.peer_links
                    .retain(|(_, route_peer), _| route_peer != peer);
            }
            // Drop the same peers (matched by canonical pubkey) from the live
            // session, tearing down any routes to them.
            let mut effects = Vec::new();
            let mut dropped = 0usize;
            if let Some(session) = st.session.as_mut() {
                let gone: Vec<NodeId> = session
                    .peers()
                    .filter(|p| stale.contains(pubkey_part(p.node.as_str())))
                    .map(|p| p.node.clone())
                    .collect();
                for id in gone {
                    effects.extend(session.drop_peer(&id));
                    dropped += 1;
                }
            }
            (effects, dropped)
        };
        if dropped > 0 {
            tracing::info!("network reset: cleared {dropped} stale peer(s) from a removed network");
            // This is a peer-cache change, not a restart of our application
            // route epoch. Rotating the global boot here would make every
            // surviving peer reap healthy routes because an unrelated peer
            // disappeared. A forgotten peer is already answered when its next
            // presence arrives because it is no longer `known` locally.
        }
        // Boxed to break the async-fn cycle: `process_effects` can route back
        // through ownership/`sync_networks`, and without indirection the
        // `sync_networks` → `prune_unjoined_peers` → `process_effects` chain
        // would be an infinitely-sized future.
        Box::pin(self.process_effects(effects)).await;
    }

    /// Subscribe presence, control, media, and rooms on each given network.
    /// All of them ride every network: presence broadcasts so peers are found
    /// wherever they are, and point-to-point (control/media/rooms) so a frame
    /// addressed to whichever network the *sender* last saw us on always has a
    /// subscriber here. (The fleet's `OwnedRoster` gossip channel is gone —
    /// membership is the closed network's signed roster now.)
    async fn subscribe_channels(self: &Arc<Self>, client_id: ClientId, networks: &[String]) {
        let daemon_epoch = self.daemon_session_epoch.load(Ordering::SeqCst);
        if self.state.lock().client_id != Some(client_id) {
            return;
        }
        let serial = self.subscription_serial.lock().await;
        if self.daemon_session_epoch.load(Ordering::SeqCst) != daemon_epoch
            || self.state.lock().client_id != Some(client_id)
        {
            return;
        }
        {
            let joined = networks
                .iter()
                .cloned()
                .collect::<std::collections::HashSet<_>>();
            let mut health = self.network_subscriptions.lock();
            health.retain(|network, _| joined.contains(network));
            for network in networks {
                let replace = health
                    .get(network)
                    .is_none_or(|state| !state.belongs_to(daemon_epoch, client_id));
                if replace {
                    health.insert(
                        network.clone(),
                        NetworkSubscriptionState::new(daemon_epoch, client_id),
                    );
                }
            }
        }

        // Preserve the existing immediate retry envelope, but issue all
        // missing slots in parallel so one dark mesh cannot delay a healthy
        // mesh's presence, control, or video subscription.
        let mut missing = usize::MAX;
        for attempt in 0..3u32 {
            missing = self
                .subscribe_missing_once(client_id, daemon_epoch, networks, attempt)
                .await;
            if missing == 0 {
                break;
            }
            if attempt < 2 {
                tokio::time::sleep(Duration::from_millis(500 * u64::from(attempt + 1))).await;
            }
        }
        drop(serial);

        if missing > 0 {
            tracing::error!(
                missing,
                "mesh subscription set is degraded; healthy networks remain live and missing slots will retry for this daemon session"
            );
        }
        self.emit_subscription_health(missing);
        self.start_subscription_retry_worker(client_id, daemon_epoch);
    }

    async fn subscribe_missing_once(
        &self,
        client_id: ClientId,
        daemon_epoch: u64,
        networks: &[String],
        attempt: u32,
    ) -> usize {
        let targets = {
            let health = self.network_subscriptions.lock();
            let mut targets = Vec::new();
            for network in networks {
                let Some(current) = health
                    .get(network)
                    .filter(|state| state.belongs_to(daemon_epoch, client_id))
                    .cloned()
                else {
                    continue;
                };
                for channel in required_subscription_channels() {
                    if !current.channels.contains(channel) {
                        targets.push((
                            network.clone(),
                            SubscriptionTarget::Channel(channel.to_string()),
                        ));
                    }
                }
                if !current.video {
                    targets.push((network.clone(), SubscriptionTarget::Video));
                }
                if !current.audio {
                    targets.push((network.clone(), SubscriptionTarget::Audio));
                }
            }
            targets
        };

        let mut tasks = tokio::task::JoinSet::new();
        for (network, target) in targets {
            let client = self.client.clone();
            let request = match &target {
                SubscriptionTarget::Channel(channel) => Request::ChannelSubscribe {
                    client_id,
                    network: network.clone(),
                    channel: channel.clone(),
                },
                SubscriptionTarget::Video => Request::VideoSubscribe {
                    client_id,
                    network: network.clone(),
                },
                SubscriptionTarget::Audio => Request::AudioSubscribe {
                    client_id,
                    network: network.clone(),
                },
            };
            tasks.spawn(async move {
                let outcome = client
                    .request(&request)
                    .await
                    .map(|response| (response.ok, response.error))
                    .map_err(|error| error.to_string());
                (network, target, outcome)
            });
        }

        while let Some(result) = tasks.join_next().await {
            let Ok((network, target, outcome)) = result else {
                tracing::warn!(attempt, "subscription attempt task failed to join");
                continue;
            };
            match outcome {
                Ok((true, _)) => {
                    if self.daemon_session_epoch.load(Ordering::SeqCst) != daemon_epoch {
                        continue;
                    }
                    let mut health = self.network_subscriptions.lock();
                    let Some(current) = health
                        .get_mut(&network)
                        .filter(|state| state.belongs_to(daemon_epoch, client_id))
                    else {
                        continue;
                    };
                    match target {
                        SubscriptionTarget::Channel(channel) => {
                            current.channels.insert(channel);
                        }
                        SubscriptionTarget::Video => current.video = true,
                        SubscriptionTarget::Audio => current.audio = true,
                    }
                }
                Ok((false, error)) => tracing::warn!(
                    network = %network,
                    target = ?target,
                    attempt,
                    "subscription refused: {}",
                    error.as_deref().unwrap_or("(no error)")
                ),
                Err(error) => tracing::warn!(
                    network = %network,
                    target = ?target,
                    attempt,
                    "subscription failed: {error}"
                ),
            }
        }

        if self.daemon_session_epoch.load(Ordering::SeqCst) != daemon_epoch
            || self.state.lock().client_id != Some(client_id)
        {
            return 0;
        }
        let health = self.network_subscriptions.lock();
        self.daemon_video.store(
            health
                .values()
                .any(|state| state.belongs_to(daemon_epoch, client_id) && state.video),
            Ordering::SeqCst,
        );
        self.daemon_audio.store(
            health
                .values()
                .any(|state| state.belongs_to(daemon_epoch, client_id) && state.audio),
            Ordering::SeqCst,
        );
        subscription_missing_count(&health, daemon_epoch, client_id, networks)
    }

    fn emit_subscription_health(&self, missing: usize) {
        let health = self.network_subscriptions.lock();
        let networks = health
            .iter()
            .map(|(network, state)| {
                json!({
                    "network": network,
                    "channels": state.channels.len(),
                    "video": state.video,
                    "audio": state.audio,
                })
            })
            .collect::<Vec<_>>();
        self.sink.emit(
            "allmystuff://subscription-health",
            json!({
                "status": if missing == 0 { "healthy" } else { "degraded" },
                "missing": missing,
                "networks": networks,
            }),
        );
    }

    fn start_subscription_retry_worker(self: &Arc<Self>, client_id: ClientId, epoch: u64) {
        if self.daemon_session_epoch.load(Ordering::SeqCst) != epoch
            || self.state.lock().client_id != Some(client_id)
        {
            return;
        }
        if self.subscription_retry_epoch.swap(epoch, Ordering::SeqCst) == epoch {
            return;
        }
        let mesh = Arc::downgrade(self);
        crate::spawn(async move {
            loop {
                tokio::time::sleep(OFFER_SWEEP).await;
                let Some(mesh) = mesh.upgrade() else { return };
                if mesh.daemon_session_epoch.load(Ordering::SeqCst) != epoch {
                    return;
                }
                let networks = {
                    let state = mesh.state.lock();
                    if state.client_id != Some(client_id) {
                        return;
                    }
                    state.networks.clone()
                };
                if networks.is_empty() {
                    continue;
                }
                let serial = mesh.subscription_serial.lock().await;
                if mesh.daemon_session_epoch.load(Ordering::SeqCst) != epoch
                    || mesh.state.lock().client_id != Some(client_id)
                {
                    return;
                }
                let missing = mesh
                    .subscribe_missing_once(client_id, epoch, &networks, 3)
                    .await;
                drop(serial);
                if missing > 0 {
                    tracing::warn!(
                        missing,
                        "subscription healer still has dark slots; healthy networks remain usable"
                    );
                }
                mesh.emit_subscription_health(missing);
            }
        });
    }

    #[allow(dead_code)]
    async fn subscribe_channels_legacy(&self, client_id: ClientId, networks: &[String]) {
        let channels = [
            CHANNEL_PRESENCE,
            CHANNEL_CONTROL,
            CHANNEL_MEDIA,
            CHANNEL_ROOMS,
            // CEC Support rides the same engine on its own channels; subscribing
            // everywhere is harmless (they're empty on non-CEC meshes) and means
            // a CEC Silent mesh is live for connect-requests the moment it's
            // joined.
            allmystuff_cec_protocol::CHANNEL_CONTROL,
            allmystuff_cec_protocol::CHANNEL_PRESENCE,
        ];
        for network in networks {
            for channel in channels {
                // A failed subscribe used to be discarded (`let _ =`) —
                // and one transient refusal meant presence/control/media on
                // that network were dead for the whole session, silently:
                // peers never appeared, offers to us were dropped
                // daemon-side, nothing logged. Retry a couple of times with
                // a beat between, and if it still fails, say exactly which
                // network+channel is dark — a half-subscribed mesh must be
                // diagnosable from the log.
                let mut ok = false;
                for attempt in 0..3u32 {
                    if attempt > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            500 * u64::from(attempt),
                        ))
                        .await;
                    }
                    match self
                        .client
                        .request(&Request::ChannelSubscribe {
                            client_id,
                            network: network.clone(),
                            channel: channel.to_string(),
                        })
                        .await
                    {
                        Ok(resp) if resp.ok => {
                            ok = true;
                            break;
                        }
                        Ok(resp) => {
                            tracing::warn!(
                                network = %network,
                                channel = %channel,
                                "channel subscribe refused: {}",
                                resp.error.as_deref().unwrap_or("(no error)")
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                network = %network,
                                channel = %channel,
                                "channel subscribe failed: {e}"
                            );
                        }
                    }
                }
                if !ok {
                    tracing::error!(
                        network = %network,
                        channel = %channel,
                        "channel is DARK for this session — peers on this mesh won't see us \
                         on it (presence/control/media affected); a daemon-link reconnect \
                         will retry the full bring-up"
                    );
                }
            }
            // The video track lane's inbound side: assembled H.264
            // access units arrive as `video_inbound` events. The verdict
            // doubles as the capability probe: a daemon that predates the
            // lane refuses the op, and we pin `daemon_video` accordingly
            // so every transport choice (ours and what we ask peers for)
            // degrades to MJPEG instead of a stream nobody can carry.
            match self
                .client
                .request(&Request::VideoSubscribe {
                    client_id,
                    network: network.clone(),
                })
                .await
            {
                Ok(resp) if resp.ok => {
                    self.daemon_video.store(true, Ordering::SeqCst);
                    // Learn the daemon's media-lane pool size, so we know how many
                    // simultaneous streams to one peer can ride separate lanes,
                    // and whether it speaks the binary media pipes (a capability
                    // flag, since the feature predates a release and the version
                    // pin can't gate it). Both come off the same Status.
                    if let Some(d) = self
                        .client
                        .request(&Request::Status)
                        .await
                        .ok()
                        .and_then(|r| r.data)
                    {
                        if let Some(n) = d.get("media_lanes").and_then(|v| v.as_u64()) {
                            if n > u64::from(PRENEGOTIATED_MEDIA_LANES) {
                                tracing::debug!(
                                    reported_lanes = n,
                                    usable_lanes = PRENEGOTIATED_MEDIA_LANES,
                                    "ignoring dynamic media-lane ceiling at the no-signaling boundary"
                                );
                            }
                            self.daemon_lanes
                                .store(PRENEGOTIATED_MEDIA_LANES, Ordering::SeqCst);
                        }
                        let pipes = d
                            .get("media_pipes")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        self.daemon_media_pipes.store(pipes, Ordering::SeqCst);
                        if !pipes {
                            tracing::info!(
                                "daemon has no binary media pipes — H.264/Opus ride the base64 video_send/audio_send path (rebuild myownmesh from this branch to enable the binary pipes)"
                            );
                        }
                    }
                }
                _ => {
                    if !self.daemon_video.load(Ordering::SeqCst) {
                        let version = self
                            .client
                            .request(&Request::Status)
                            .await
                            .ok()
                            .and_then(|r| r.data)
                            .and_then(|d| {
                                d.get("version").and_then(|v| v.as_str()).map(String::from)
                            })
                            .unwrap_or_else(|| "unknown".into());
                        tracing::warn!(
                            "daemon v{version} doesn't speak the video track lane (needs myownmesh ≥ 0.2.1) — screen shares fall back to MJPEG"
                        );
                    }
                }
            }
            // The audio lane's inbound side + capability probe, exactly
            // like video's: a daemon that predates the lane refuses the
            // op, and audio rides PCM frames over the media channel.
            match self
                .client
                .request(&Request::AudioSubscribe {
                    client_id,
                    network: network.clone(),
                })
                .await
            {
                Ok(resp) if resp.ok => {
                    self.daemon_audio.store(true, Ordering::SeqCst);
                }
                _ => {
                    if !self.daemon_audio.load(Ordering::SeqCst) {
                        tracing::info!(
                            "daemon doesn't speak the audio track lane (needs myownmesh ≥ 0.2.4) — audio rides the data channel"
                        );
                    }
                }
            }
        }
    }

    /// Begin carrying media for a now-active route. Audio, display (screen
    /// streaming), video (camera streaming), and input (remote control)
    /// are wired; storage still shows active without a transport, and the
    /// log says so.
    fn start_media(self: &Arc<Self>, route: &Route) {
        let Some(me) = self.local_node_id() else {
            return;
        };
        // Compare endpoints to ourselves *canonically* — the route's ids carry
        // the UI's display suffix while `me` is the bare node id. Without this
        // a loopback (e.g. a local terminal) matches neither the loopback arm
        // nor the host/viewer arms, and nothing starts. The bare ids only feed
        // `== me` checks, log labels, and the peer arg to the capture starts
        // (which the routing layer canonicalises again), so normalising them
        // here is safe.
        let me = pubkey_part(&me).to_string();
        let from_node = pubkey_part(&node_of(route.from.as_str())).to_string();
        let to_node = pubkey_part(&node_of(route.to.as_str())).to_string();

        match route.media {
            MediaKind::Audio => {
                // We source: capture what the routed capability names — the
                // machine's own playback for the synthetic `system-audio`,
                // the default mic for a scanned input device — and stream
                // it to the sink. Transport: the offer said what the sink
                // can consume — Opus on the daemon's audio track lane when
                // both stacks carry it and this peer's lane is free. Legacy
                // PCM over the media channel remains available only to an
                // uncapped audio-only peer; it is never counted as though it
                // fit an encoded-audio reservation in a governed aggregate.
                if from_node == me {
                    let source = audio_capture_source(route);
                    let audio_profile = audio_profile_for_mode(self.media_mode_for_peer(&to_node));
                    let policy_enforced = self
                        .media_policy
                        .lock()
                        .has_video_routes(pubkey_part(&to_node));
                    let accepts_opus = self
                        .state
                        .lock()
                        .session
                        .as_ref()
                        .and_then(|s| s.route(&route.id))
                        .map(|r| r.audio.iter().any(|a| a == "opus"))
                        .unwrap_or(false);
                    let lane = accepts_opus
                        && self.route_audio_ready(&route.id, &to_node)
                        && self.audio_lane(&route.id, &to_node, true).is_some();
                    if policy_enforced && !lane {
                        tracing::warn!(
                            "audio unavailable for {}: peer lacks a usable Opus lane; legacy PCM \
                             is disabled while a peer-wide media aggregate is enforced",
                            route.id
                        );
                        return;
                    }
                    let peer = to_node.clone();
                    let tx = self.audio_out.clone();
                    let encoder = if lane {
                        match OpusStream::with_profile(audio_profile) {
                            Ok(enc) => {
                                let candidate = Arc::new(parking_lot::Mutex::new(enc));
                                let encoder = self
                                    .audio_encoders
                                    .lock()
                                    .entry(route.id.clone())
                                    .or_insert_with(|| candidate.clone())
                                    .clone();
                                // A duplicate StartMedia retains the same Arc
                                // captured by the live pump, but still applies
                                // the newest mode contract in place.
                                if !Arc::ptr_eq(&encoder, &candidate) {
                                    *encoder.lock() = OpusStream::with_profile(audio_profile)
                                        .expect("profile already constructed above");
                                }
                                Some(encoder)
                            }
                            Err(e) => {
                                let existing = self.audio_encoders.lock().get(&route.id).cloned();
                                if let Some(existing) = existing {
                                    tracing::warn!(
                                        "replacement Opus encoder for {} failed ({e}); retaining the live encoder",
                                        route.id
                                    );
                                    Some(existing)
                                } else if policy_enforced {
                                    tracing::warn!(
                                        "audio unavailable for {}: Opus encoder failed ({e}); \
                                         legacy PCM is disabled while a peer-wide media aggregate \
                                         is enforced",
                                        route.id
                                    );
                                    return;
                                } else {
                                    tracing::warn!(
                                        "opus encoder for {} failed ({e}); falling back to legacy PCM \
                                         because this peer has no governed video plan",
                                        route.id
                                    );
                                    // The route remains live and no daemon lane
                                    // was opened, so positional lane teardown
                                    // would risk closing a neighbour.
                                    None
                                }
                            }
                        }
                    } else {
                        None
                    };
                    if encoder.is_some() {
                        self.pcm_audio_routes.lock().remove(&route.id);
                    } else {
                        self.pcm_audio_routes
                            .lock()
                            .insert(route.id.clone(), to_node.clone());
                    }
                    let policy_plans = {
                        let serial = self.video_policy_apply_serial.lock();
                        let plans = self
                            .media_policy
                            .lock()
                            .register_audio_route(pubkey_part(&to_node), &route.id);
                        self.apply_video_policy_caps_locked(&plans, &serial);
                        plans
                    };
                    if !policy_plans.is_empty() {
                        let mesh = self.clone();
                        crate::spawn(async move { mesh.send_effective_plans(policy_plans).await });
                    }
                    tracing::info!(
                        "route {} active — streaming {} to {} ({})",
                        route.id,
                        match source {
                            CaptureSource::System => "system audio",
                            CaptureSource::Mic => "mic audio",
                        },
                        short_id(&to_node),
                        if encoder.is_some() {
                            "Opus lane"
                        } else {
                            "legacy PCM channel"
                        }
                    );
                    let rid = route.id.clone();
                    let seq = Arc::new(AtomicU64::new(0));
                    let media_samples = Arc::new(AtomicU64::new(0));
                    self.audio.start_capture_interleaved(
                        route.id.clone(),
                        source,
                        move |pcm, rate, channels| {
                            // try_send everywhere: a full queue drops this
                            // buffer; the next one carries fresher sound.
                            if let Some(enc) = &encoder {
                                let mut enc = enc.lock();
                                let duration_us = enc.profile().frame_duration_us();
                                enc.push_interleaved(&pcm, rate, channels, |data| {
                                    let _ = tx.try_send(AudioOut::Lane {
                                        peer: peer.clone(),
                                        route: rid.clone(),
                                        duration_us,
                                        data,
                                    });
                                });
                            } else {
                                let s = seq.fetch_add(1, Ordering::Relaxed);
                                let frames = pcm.len() / channels.max(1) as usize;
                                let start_sample =
                                    media_samples.fetch_add(frames as u64, Ordering::Relaxed);
                                let media_timestamp_us = start_sample
                                    .saturating_mul(1_000_000)
                                    .saturating_div(u64::from(rate.max(1)));
                                let frame = AudioFrame::new_timestamped(
                                    rid.clone(),
                                    s,
                                    rate,
                                    channels,
                                    media_timestamp_us,
                                    pcm,
                                );
                                let _ = tx.try_send(AudioOut::Channel(peer.clone(), frame));
                            }
                        },
                    );
                }
                // We sink: play inbound frames for this route. Inbound Opus
                // lane samples find their route on demand
                // ([`Self::audio_route_for_lane`]) — the peer maps each
                // active-codec route to a lane by sorted position the same
                // way we do, so no claim is recorded here (the sender may
                // still pick PCM, in which case the lane simply never sees a
                // frame).
                if to_node == me {
                    tracing::info!(
                        "route {} active — playing audio from {}",
                        route.id,
                        short_id(&from_node)
                    );
                    let profile = audio_profile_for_mode(self.media_mode_for_peer(&from_node));
                    self.audio
                        .start_playback_with_profile(route.id.clone(), profile);
                }
            }
            MediaKind::Display => {
                // We're the screen being looked at: capture and stream to
                // the viewer. The transport comes from the offer: when the
                // viewer can decode H.264 and this peer's track lane is
                // free, the stream rides RTP; otherwise MJPEG over the
                // media channel, exactly as v1. The viewer side starts no
                // capture — it claims the inbound lane so arriving samples
                // route to its console window.
                if from_node == me && to_node != me {
                    let mode = self.pick_outbound_video_mode(route, &to_node);
                    // Which monitor: the synthetic `screen` is the primary;
                    // a `screen:<id>` capability names one of the others
                    // (the ids come from this machine's own monitor
                    // enumeration — see `video::extra_screens`).
                    let monitor = device_of(route.from.as_str())
                        .and_then(|dev| dev.strip_prefix("screen:").map(str::to_string))
                        .and_then(|id| id.parse::<u32>().ok());
                    tracing::info!(
                        "route {} active — streaming this {} to {} ({})",
                        route.id,
                        match monitor {
                            Some(id) => format!("monitor {id}"),
                            None => "screen".to_string(),
                        },
                        short_id(&to_node),
                        mode_label(mode),
                    );
                    self.start_video_stream(route, &to_node, mode, VideoSource::Screen(monitor));
                    // Tell the viewer the pinned lane (best-effort, off the
                    // sync start path; the pin is already assigned above).
                    let (mesh, rid, peer) = (self.clone(), route.id.clone(), to_node.clone());
                    crate::spawn(async move { mesh.announce_video_lane(&rid, &peer).await });
                } else if to_node == me {
                    tracing::info!(
                        "route {} active — expecting screen frames from {}",
                        route.id,
                        short_id(&from_node)
                    );
                }
            }
            MediaKind::Video => {
                // A camera route — same stream, different lens: the source
                // capability names one of this machine's scanned cameras,
                // and its frames ride exactly the pipeline a screen does
                // (transport negotiation, lanes, tuning, status reports
                // included). The viewer side claims the inbound lane and
                // renders in whichever window watches the route — a
                // console's camera tab, a room's tile.
                if from_node == me && to_node != me {
                    let mode = self.pick_outbound_video_mode(route, &to_node);
                    let device = device_of(route.from.as_str()).unwrap_or_default();
                    tracing::info!(
                        "route {} active — streaming camera {device} to {} ({})",
                        route.id,
                        short_id(&to_node),
                        mode_label(mode),
                    );
                    self.start_video_stream(route, &to_node, mode, VideoSource::Camera(device));
                    let (mesh, rid, peer) = (self.clone(), route.id.clone(), to_node.clone());
                    crate::spawn(async move { mesh.announce_video_lane(&rid, &peer).await });
                } else if to_node == me {
                    tracing::info!(
                        "route {} active — expecting camera frames from {}",
                        route.id,
                        short_id(&from_node)
                    );
                }
            }
            MediaKind::Input => {
                // The sink injects lazily per inbound event (behind the
                // ownership gate); the source is driven by the console
                // window via `send_input`. Nothing to start eagerly — but
                // say the link is live, so "awaiting accept" is never the
                // last word on a working control route.
                if from_node == me {
                    tracing::info!(
                        "route {} active — keyboard/mouse control to {}",
                        route.id,
                        short_id(&to_node)
                    );
                } else if to_node == me {
                    tracing::info!(
                        "route {} active — accepting control from {}",
                        route.id,
                        short_id(&from_node)
                    );
                }
            }
            MediaKind::Clipboard => {
                // Nothing to start eagerly: the source reads + streams its
                // clipboard per paste (`clipboard_paste`), and the sink
                // reassembles + writes it on arrival (`handle_clipboard_frame`).
                // Say the link is live so "awaiting accept" isn't the last
                // word on a working clipboard route.
                if from_node == me {
                    tracing::info!(
                        "route {} active — clipboard to {}",
                        route.id,
                        short_id(&to_node)
                    );
                } else if to_node == me {
                    tracing::info!(
                        "route {} active — accepting clipboard from {}",
                        route.id,
                        short_id(&from_node)
                    );
                }
            }
            MediaKind::Generic if is_terminal_route(route) => {
                if from_node == me && to_node == me {
                    // Loopback: a terminal to the machine we're sitting at.
                    // We're both shell *and* viewer — there's no peer to
                    // negotiate frames with, so the PTY's output goes
                    // straight into the local viewer queue (the same one the
                    // remote path enqueues into), and the window drains it
                    // exactly as it would a remote session.
                    self.start_terminal_loopback(route);
                } else if from_node == me && to_node != me {
                    // We're the shell end: spawn a PTY and pump it to the
                    // viewer (after re-clearing the owner/fleet gate).
                    self.start_terminal_host(route);
                } else if to_node == me && from_node != me {
                    // We're the viewer: buffer output from the very first
                    // byte — the host's prompt arrives right after Accept,
                    // before the terminal window has subscribed, and unlike
                    // a video frame a dropped byte never heals.
                    self.terminal.ensure_queue(&route.id);
                    tracing::info!(
                        "route {} active — terminal session from {}",
                        route.id,
                        short_id(&from_node)
                    );
                }
            }
            MediaKind::Generic if is_files_route(route) => {
                if from_node == me && to_node != me {
                    // We're the disk end: requests drive everything — the
                    // owner/fleet gate re-clears per inbound frame.
                    tracing::info!(
                        "route {} active — hosting files for {}",
                        route.id,
                        short_id(&to_node)
                    );
                } else if to_node == me && from_node != me {
                    // We're the viewer: buffer responses from the first
                    // frame, before the files window has subscribed.
                    self.files.ensure_queue(&route.id);
                    tracing::info!(
                        "route {} active — files session from {}",
                        route.id,
                        short_id(&from_node)
                    );
                }
            }
            MediaKind::Generic if is_shared_route(route) => {
                // A room's Shared Files fetch lane — the files plumbing,
                // but token-gated instead of owner/fleet (see
                // `handle_file_frame`). Downloads stream straight to disk
                // via the registered sink, so the viewer side just needs a
                // buffer for any reply that beats the registration.
                if from_node == me && to_node != me {
                    tracing::info!(
                        "route {} active — serving shared files to {}",
                        route.id,
                        short_id(&to_node)
                    );
                } else if to_node == me && from_node != me {
                    self.files.ensure_queue(&route.id);
                    tracing::info!(
                        "route {} active — shared-files fetch from {}",
                        route.id,
                        short_id(&from_node)
                    );
                }
            }
            MediaKind::Generic if is_site_route(route) => {
                // Nothing to start eagerly. The *client* (sink) already bound
                // its local listener at `site_map` time and opens tunnels as
                // connections arrive; the *host* (source) reacts to each
                // `SiteEvent::Open` (re-checking its own exposed allow-list)
                // in `handle_site_frame`. Just confirm the link is live.
                if from_node == me && to_node != me {
                    tracing::info!(
                        "route {} active — hosting site for {}",
                        route.id,
                        short_id(&to_node)
                    );
                } else if to_node == me && from_node != me {
                    tracing::info!(
                        "route {} active — site proxy from {}",
                        route.id,
                        short_id(&from_node)
                    );
                }
            }
            other => {
                tracing::info!(
                    "route {} active ({other:?}); media transport for it is a follow-up",
                    route.id
                );
            }
        }
    }

    /// The capability list this node advertises. Desktop/server `host` builds
    /// retain the hardware-derived bridge contract. A capture-less mobile
    /// build uses the mobile-core viewer/controller contract instead of
    /// trimming desktop capabilities after the fact. That provides the
    /// synthetic Display and Audio sinks a remote desktop needs, while never
    /// advertising the inert desktop control, system-audio, or clipboard
    /// endpoints backed by no-op stubs on this build.
    fn advertised_capabilities(
        inv: &allmystuff_inventory::Inventory,
        node: &allmystuff_graph::NodeId,
    ) -> Vec<allmystuff_graph::Capability> {
        #[cfg(feature = "host")]
        {
            allmystuff_bridge::capabilities_with_screens(inv, node, &crate::video::extra_screens())
        }
        #[cfg(not(feature = "host"))]
        {
            let _ = inv;
            allmystuff_mobile_core::mobile_capabilities(
                node,
                allmystuff_mobile_core::MobileScope::ViewerController,
            )
        }
    }

    /// Feature tags must describe the same platform profile as the capability
    /// list. Desktop keeps its existing host feature set. Capture-less mobile
    /// starts with the mobile-core contract, then adds the lifecycle features
    /// implemented by this shared Mesh engine.
    fn advertised_features() -> Vec<String> {
        #[cfg(feature = "host")]
        let mut features = vec![
            allmystuff_protocol::FEATURE_FILES.to_string(),
            allmystuff_protocol::FEATURE_ROOMS.to_string(),
            allmystuff_protocol::FEATURE_SITES.to_string(),
            allmystuff_protocol::FEATURE_TERMINAL.to_string(),
            allmystuff_protocol::FEATURE_CAMERA.to_string(),
        ];
        #[cfg(not(feature = "host"))]
        let mut features = allmystuff_mobile_core::mobile_features(
            allmystuff_mobile_core::MobileScope::ViewerController,
        );
        features.push(FEATURE_ROUTE_INCARNATION.to_string());
        features.push(FEATURE_ROUTE_TEARDOWN_ACK.to_string());
        features.push(FEATURE_MEDIA_INCARNATION.to_string());
        features
    }

    /// The ids of the **active** codec media routes between us and `peer` in
    /// one direction, sorted — the shared, signalling-free basis for lane
    /// assignment: both ends compute the identical list from their own copy of
    /// the session, so a route lands on the same lane on both. `codec` is
    /// "h264" (video) or "opus" (audio); `outbound` = we are the source.
    ///
    /// Only **active** routes count. A route still negotiating (Offered /
    /// Incoming) or already torn down must not occupy a lane slot: it carries
    /// no media, yet — being in `routes()` — it would shift every later
    /// route's index and so its lane, decoding a live stream's frames into
    /// the wrong window for as long as the transient lasts. Restricting the
    /// basis to active routes keeps the two ends agreeing on a stable lane for
    /// the whole life of each stream (both ends process Active/Teardown), so
    /// an unrelated route coming or going no longer reshuffles a live one.
    fn sorted_media_routes(&self, peer: &str, outbound: bool, codec: &str) -> Vec<String> {
        let Some(me) = self.local_node_id() else {
            return Vec::new();
        };
        let mp = pubkey_part(&me).to_string();
        let pc = pubkey_part(peer).to_string();
        let st = self.state.lock();
        let Some(session) = st.session.as_ref() else {
            return Vec::new();
        };
        let mut ids: Vec<String> = session
            .active_routes()
            .filter(|r| {
                let codecs = if codec == "opus" { &r.audio } else { &r.video };
                codecs.iter().any(|c| c == codec) && {
                    let src = pubkey_part(node_of(r.route.from.as_str()).as_str()).to_string();
                    let dst = pubkey_part(node_of(r.route.to.as_str()).as_str()).to_string();
                    if outbound {
                        src == mp && dst == pc
                    } else {
                        src == pc && dst == mp
                    }
                }
            })
            .map(|r| r.route.id.clone())
            .collect();
        ids.sort_unstable();
        ids
    }

    fn peer_supports_route_incarnation(&self, peer: &str) -> bool {
        let canon = pubkey_part(peer);
        let advertised = self
            .state
            .lock()
            .peer_features
            .get(canon)
            .is_some_and(|features| features.iter().any(|f| f == FEATURE_ROUTE_INCARNATION));
        advertised
            || self
                .local_node_id()
                .is_some_and(|local| same_node(&local, peer))
    }

    fn peer_supports_teardown_ack(&self, peer: &str) -> bool {
        let canon = pubkey_part(peer);
        self.state
            .lock()
            .peer_features
            .get(canon)
            .is_some_and(|features| features.iter().any(|f| f == FEATURE_ROUTE_TEARDOWN_ACK))
            || self
                .local_node_id()
                .is_some_and(|local| same_node(&local, peer))
    }

    fn peer_supports_media_incarnation(&self, peer: &str) -> bool {
        let canon = pubkey_part(peer);
        self.state
            .lock()
            .peer_features
            .get(canon)
            .is_some_and(|features| features.iter().any(|f| f == FEATURE_MEDIA_INCARNATION))
            || self
                .local_node_id()
                .is_some_and(|local| same_node(&local, peer))
    }

    /// Rotate the presence/route boot as one published state transition. Any
    /// peer that receives a successor route token must also be able to learn
    /// the same boot from our cached profile; resetting only the clock makes
    /// every subsequent fenced Offer look stale.
    fn rotate_route_boot(&self) -> u64 {
        let boot = {
            let mut clock = self.route_incarnation_clock.lock();
            clock.reset();
            clock.boot
        };
        if let Some(profile) = self.state.lock().profile.as_mut() {
            profile.boot = boot;
        }
        boot
    }

    /// Allocate the next ordered route lifetime for a peer that advertised the
    /// fence. Older peers receive no field and retain the legacy wire shape.
    fn next_route_incarnation(&self, peer: &str) -> Option<String> {
        if !self.peer_supports_route_incarnation(peer) {
            return None;
        }
        Some(self.route_incarnation_clock.lock().next())
    }

    fn next_route_intent_generation(&self) -> u64 {
        next_js_safe_counter(&self.route_intent_generation)
    }

    fn desired_route_is_current(&self, route_id: &str, generation: u64) -> bool {
        self.desired_routes
            .lock()
            .get(route_id)
            .is_some_and(|desired| desired.local_generation == generation)
    }

    fn remove_desired_route_exact(
        &self,
        peer: &str,
        route_id: &str,
        incarnation: Option<&str>,
    ) -> bool {
        let mut desired = self.desired_routes.lock();
        let matches = desired.get(route_id).is_some_and(|route| {
            pubkey_part(&route.peer) == pubkey_part(peer)
                && route.current_incarnation.as_deref() == incarnation
        });
        if matches {
            desired.remove(route_id);
            self.requested_video_tunes.lock().remove(route_id);
        }
        matches
    }

    fn update_desired_terminal_session(
        &self,
        peer: &str,
        route_id: &str,
        incarnation: Option<&str>,
        session: Option<&str>,
    ) {
        let Some(session) = session else { return };
        if let Some(desired) = self.desired_routes.lock().get_mut(route_id) {
            if pubkey_part(&desired.peer) == pubkey_part(peer)
                && desired.current_incarnation.as_deref() == incarnation
            {
                desired.term_session = Some(session.to_string());
            }
        }
    }

    /// Rebuild user-owned outbound routes after the daemon session or a peer
    /// app lifetime changes. This only recreates RouteControl on the existing
    /// application data channel. It does not create or modify signaling,
    /// discovery, SDP, ICE candidates, or STUN/TURN configuration.
    async fn replay_desired_routes(self: &Arc<Self>, only_peer: Option<&str>) {
        let desired = self
            .desired_routes
            .lock()
            .values()
            .filter(|route| {
                only_peer.is_none_or(|peer| pubkey_part(peer) == pubkey_part(&route.peer))
            })
            .cloned()
            .collect::<Vec<_>>();

        for desired_route in desired {
            if !self
                .desired_route_is_current(&desired_route.route.id, desired_route.local_generation)
            {
                continue;
            }
            let route_id = desired_route.route.id.clone();

            let (message, effects, peer, local) = {
                let _lifecycle = self.lock_route_lifecycle(&route_id).await;
                // Re-read the intent after acquiring the lock. A concurrent
                // replay or connect can update the same generation's current
                // wire incarnation while this task is waiting; comparing with
                // the stale pre-lock clone would needlessly replace it again.
                let Some(current_desired) = self
                    .desired_routes
                    .lock()
                    .get(&route_id)
                    .filter(|current| current.local_generation == desired_route.local_generation)
                    .cloned()
                else {
                    continue;
                };
                let peer = current_desired.peer.clone();
                let local = self.local_node_id().is_some_and(|me| same_node(&me, &peer));

                let already_current = self
                    .state
                    .lock()
                    .session
                    .as_ref()
                    .and_then(|session| session.route(&route_id))
                    .is_some_and(|route| {
                        matches!(
                            route.state,
                            RouteState::Offered | RouteState::Incoming | RouteState::Active
                        ) && route.incarnation == current_desired.current_incarnation
                    });
                if already_current {
                    continue;
                }

                // Allocate only while holding the route lock. Two concurrent
                // replays can no longer mint inc1/inc2 and install them in the
                // opposite order.
                let incarnation = self.next_route_incarnation(&peer);

                {
                    let mut routes = self.desired_routes.lock();
                    let Some(current) = routes.get_mut(&route_id) else {
                        continue;
                    };
                    if current.local_generation != current_desired.local_generation {
                        continue;
                    }
                    current.current_incarnation = incarnation.clone();
                }

                let video = if self.peer_video_ready(&peer) {
                    current_desired.requested_video.clone()
                } else {
                    Vec::new()
                };
                let audio = if self.peer_audio_ready(&peer) {
                    current_desired.requested_audio.clone()
                } else {
                    Vec::new()
                };
                let mut state = self.state.lock();
                let Some(session) = state.session.as_mut() else {
                    continue;
                };
                let message = session.offer_terminal_with_incarnation(
                    current_desired.route.clone(),
                    peer.as_str(),
                    video,
                    audio,
                    current_desired.term_session.clone(),
                    incarnation.clone(),
                );
                let effects = if local {
                    session.handle(
                        NodeId::from(peer.as_str()),
                        ControlMessage::Route(RouteControl::Accept {
                            route_id: route_id.clone(),
                            incarnation,
                            session: current_desired.term_session.clone(),
                        }),
                    )
                } else {
                    Vec::new()
                };
                (message, effects, peer, local)
            };

            if local {
                // A loopback replay can reach ownership handling, which may
                // reconcile networks and re-enter desired-route replay. Keep
                // that recovery edge heap-indirected so the async future has a
                // finite type, matching the network-prune replay path.
                Box::pin(self.process_effects(effects)).await;
            } else if let Err(error) = self.send_control(&peer, &message).await {
                tracing::warn!(
                    route = %route_id,
                    peer = %short_id(&peer),
                    error = %error,
                    "desired route replay remains queued for the next sweep/presence event"
                );
            } else {
                tracing::info!(
                    route = %route_id,
                    peer = %short_id(&peer),
                    "desired route replayed after session recovery"
                );
            }
            self.emit_snapshot();
        }
    }

    fn route_incarnation(&self, route_id: &str) -> Option<String> {
        self.state
            .lock()
            .session
            .as_ref()
            .and_then(|session| session.route(route_id))
            .and_then(|route| route.incarnation.clone())
    }

    fn route_is_active_incarnation(&self, route_id: &str, incarnation: Option<&str>) -> bool {
        self.state
            .lock()
            .session
            .as_ref()
            .and_then(|session| session.route(route_id))
            .is_some_and(|route| {
                route.state == RouteState::Active && route.incarnation.as_deref() == incarnation
            })
    }

    async fn lock_route_lifecycle(&self, route_id: &str) -> RouteLifecycleGuard {
        let lock = {
            let mut locks = self.route_lifecycle_locks.lock();
            locks
                .entry(route_id.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let guard = lock.clone().lock_owned().await;
        RouteLifecycleGuard {
            route_id: route_id.to_string(),
            lock,
            guard: Some(guard),
            locks: self.route_lifecycle_locks.clone(),
        }
    }

    fn claim_media_incarnation_if_active(&self, route_id: &str, incarnation: Option<&str>) -> bool {
        let state = self.state.lock();
        let active = state
            .session
            .as_ref()
            .and_then(|session| session.route(route_id))
            .is_some_and(|route| {
                route.state == RouteState::Active && route.incarnation.as_deref() == incarnation
            });
        if active {
            self.active_media_incarnations
                .lock()
                .insert(route_id.to_string(), incarnation.map(str::to_string));
        }
        active
    }

    fn peer_media_lane_count(&self, peer: &str) -> u8 {
        let canon = pubkey_part(peer);
        self.state
            .lock()
            .peer_features
            .get(canon)
            .and_then(|features| {
                features.iter().find_map(|feature| {
                    feature
                        .strip_prefix("media-lanes:")
                        .and_then(|count| count.parse::<u8>().ok())
                        .filter(|count| *count > 0)
                })
            })
            // The old binary tag proves support for lane 0 only. It does not
            // prove the receiver provisioned the same pool size as us.
            .unwrap_or(1)
    }

    /// The media-lane pool size we and `peer` can both use for video: 0 when the
    /// local daemon has no track lane at all (everything MJPEG), 1 when either
    /// side predates the lane pool (only lane 0; extra streams fall back to
    /// MJPEG — the original behaviour), else the local pool size (both ends ship
    /// the same pinned daemon, so the counts match).
    fn effective_video_lanes(&self, peer: &str) -> u8 {
        if !self.daemon_video.load(Ordering::SeqCst) {
            return 0;
        }
        if self.peer_supports_lanes(peer) {
            self.daemon_lanes
                .load(Ordering::SeqCst)
                .max(1)
                .min(self.peer_media_lane_count(peer))
                .min(PRENEGOTIATED_MEDIA_LANES)
        } else {
            1
        }
    }

    /// The audio twin of [`Self::effective_video_lanes`], gated on the audio lane.
    fn effective_audio_lanes(&self, peer: &str) -> u8 {
        if !self.daemon_audio.load(Ordering::SeqCst) {
            return 0;
        }
        if self.peer_supports_lanes(peer) {
            self.daemon_lanes
                .load(Ordering::SeqCst)
                .max(1)
                .min(self.peer_media_lane_count(peer))
                .min(PRENEGOTIATED_MEDIA_LANES)
        } else {
            1
        }
    }

    /// Whether `peer` advertised the media-lane pool in its presence features.
    fn peer_supports_lanes(&self, peer: &str) -> bool {
        let canon = pubkey_part(peer);
        self.state.lock().peer_features.get(canon).is_some_and(|f| {
            f.iter().any(|x| {
                x == allmystuff_protocol::FEATURE_MEDIA_LANES || x.starts_with("media-lanes:")
            })
        })
    }

    /// Pin (or look up) the RTP video track lane an outbound H.264 route to
    /// `peer` streams on — the **lowest free** lane in the peer's pool among
    /// that peer's already-pinned routes, held for the route's lifetime.
    /// `None` when the pool is exhausted or the daemon has no video lane (the
    /// route then rides MJPEG). Called once when the stream's transport is
    /// chosen; thereafter [`Self::video_lane`] just reads the pin.
    ///
    /// Pinning is what makes the lane stable: a second feed opening (or a
    /// third tearing down) no longer renumbers a live feed's lane, so the
    /// viewer — told the binding over [`RouteControl::VideoLane`] — never
    /// briefly maps one monitor's frames onto another's window.
    fn assign_video_lane(&self, peer: &str, route_id: &str) -> Option<u8> {
        let cap = self.effective_video_lanes(peer);
        if cap == 0 {
            return None;
        }
        let network = self.network_for_route(route_id, peer)?;
        let peer_canon = pubkey_part(peer);
        // The whole get/compute/insert runs under the pin lock — two screens
        // activating at once can never both pick "lane 0" (the lock serialises
        // us; the second sees the first's pin). Sampling the live session for
        // the taken lanes instead raced: it was read before the lock, so a
        // sibling route not yet visible there left its lane looking free, and
        // both screens collapsed onto one track.
        let mut pins = self.video_lane_pins.lock();
        let lane = free_lane_for_peer(&pins, &network, peer_canon, route_id, cap)?;
        pins.insert(route_id.to_string(), OutboundVideoLanePin { network, lane });
        Some(lane)
    }

    /// The video track lane an outbound H.264 route to `peer` is streaming on:
    /// the lane [`Self::assign_video_lane`] pinned at stream start. `None` once
    /// the route has torn down (its pin freed) — the forwarder then drops the
    /// frame rather than guessing a lane. `outbound` is kept for symmetry with
    /// the audio twin; the receive side resolves lanes via
    /// [`Self::video_route_for_lane`], never here.
    fn video_lane(&self, route_id: &str, peer: &str, outbound: bool) -> Option<u8> {
        if outbound {
            return self
                .video_lane_pins
                .lock()
                .get(route_id)
                .map(|pin| pin.lane);
        }
        let cap = self.effective_video_lanes(peer);
        if cap == 0 {
            return None;
        }
        let idx = self
            .sorted_media_routes(peer, outbound, "h264")
            .iter()
            .position(|id| id == route_id)?;
        (idx < cap as usize).then_some(idx as u8)
    }

    /// The audio twin of [`Self::video_lane`] (Opus on the audio lane).
    fn audio_lane(&self, route_id: &str, peer: &str, outbound: bool) -> Option<u8> {
        let cap = self.effective_audio_lanes(peer);
        if cap == 0 {
            return None;
        }
        let network = outbound
            .then(|| self.network_for_route(route_id, peer))
            .flatten();
        let routes = self.sorted_media_routes(peer, outbound, "opus");
        let idx = routes
            .iter()
            .filter(|id| {
                network.as_ref().is_none_or(|network| {
                    self.network_for_route(id, peer).as_deref() == Some(network.as_str())
                })
            })
            .position(|id| id == route_id)?;
        (idx < cap as usize).then_some(idx as u8)
    }

    /// Record the lane→route binding a streamer announced
    /// ([`RouteControl::VideoLane`]) so inbound H.264 on that lane routes to
    /// the right console window regardless of the local route order.
    fn record_video_lane(
        &self,
        network: &str,
        peer: &str,
        route_id: &str,
        incarnation: Option<String>,
        lane: u8,
    ) {
        // Final queued-AU admission holds this same fence through decoder or
        // watcher insertion. A lane ownership change therefore happens wholly
        // before or wholly after that commit, never between its last check and
        // insertion.
        let _generations = self.video_route_generations.lock();
        let canon = pubkey_part(peer).to_string();
        let cap = self.effective_video_lanes(peer);
        let valid = if lane < cap {
            let state = self.state.lock();
            let current = state
                .session
                .as_ref()
                .and_then(|session| session.route(route_id))
                .is_some_and(|route| {
                    route.state == RouteState::Active
                        && pubkey_part(route.peer.as_str()) == canon
                        && route.incarnation == incarnation
                        && route.video.iter().any(|codec| codec == "h264")
                });
            let path_matches = state
                .route_networks
                .get(&(route_id.to_string(), incarnation.clone()))
                .is_some_and(|pin| {
                    pin.confirmed
                        && pin.network == network
                        && state.network_epochs.get(network).copied() == Some(pin.network_epoch)
                });
            current && path_matches
        } else {
            false
        };
        if !valid {
            tracing::warn!(
                route = %route_id,
                peer = %short_id(peer),
                network = %network,
                lane,
                cap,
                "ignoring video-lane binding that does not match an active route lifetime"
            );
            return;
        }
        let mut binds = self.video_lane_binds.lock();
        let bind_key = (network.to_string(), canon);
        let per_peer = binds.entry(bind_key).or_default();
        // A lane is reused only after its previous route tore down (which
        // clears its binding), so overwriting here just records the current
        // owner; drop any other lane that stale-pointed at this same route.
        per_peer.retain(|l, binding| {
            *l == lane || binding.route_id != route_id || binding.incarnation != incarnation
        });
        per_peer.insert(
            lane,
            VideoLaneBinding {
                route_id: route_id.to_string(),
                incarnation,
            },
        );
    }

    /// The route whose inbound video samples arrive on `lane` from `peer`.
    ///
    /// Once a peer has announced *any* lane binding ([`Self::record_video_lane`])
    /// the announced map is **authoritative**: this lane is whatever it bound,
    /// or — if it hasn't bound this lane yet — `None`. We deliberately do NOT
    /// fall back to a positional guess there: the streamer pins lanes
    /// non-positionally (lowest-free), so guessing by sorted position would put
    /// one monitor's frames in another monitor's window (and `None` simply
    /// leaves that window holding its last frame until the real binding lands).
    ///
    /// Only a peer that predates route incarnation uses the positional sort.
    /// A capable peer with no binding yet returns `None` until its reliable
    /// exact-lifetime VideoLane arrives.
    fn video_route_for_lane(&self, network: &str, peer: &str, lane: u8) -> Option<String> {
        let canon = pubkey_part(peer);
        let bind_key = (network.to_string(), canon.to_string());
        let announced = {
            let binds = self.video_lane_binds.lock();
            if let Some(per_peer) = binds.get(&bind_key) {
                Some(per_peer.get(&lane).cloned()?)
            } else {
                None
            }
        };
        if let Some(binding) = announced {
            let lifetime_current = {
                let state = self.state.lock();
                state
                    .session
                    .as_ref()
                    .and_then(|session| session.route(&binding.route_id))
                    .is_some_and(|route| {
                        route.state == RouteState::Active
                            && pubkey_part(route.peer.as_str()) == canon
                            && route.incarnation == binding.incarnation
                    })
            };
            let current =
                lifetime_current && self.inbound_route_network_ok(&binding.route_id, peer, network);
            if current {
                return Some(binding.route_id);
            }
            let mut binds = self.video_lane_binds.lock();
            if let Some(per_peer) = binds.get_mut(&bind_key) {
                if per_peer.get(&lane) == Some(&binding) {
                    per_peer.remove(&lane);
                }
                if per_peer.is_empty() {
                    binds.remove(&bind_key);
                }
            }
            return None;
        }
        // A peer that negotiated route lifetimes also reliably announces the
        // exact VideoLane binding. Until it arrives, dropping the first access
        // units is safe; positional guessing can poison the wrong monitor's
        // decoder whenever lane allocation order differs from route sort order.
        if self.peer_supports_route_incarnation(peer) {
            return None;
        }
        // No binding announced yet (a fresh peer, or every lane freed when the
        // last route to it tore down). Positional over the peer's active h264
        // routes — the pre-binding behaviour.
        if let Some(r) = self
            .sorted_media_routes(peer, false, "h264")
            .into_iter()
            .filter(|route_id| self.inbound_route_network_ok(route_id, peer, network))
            .nth(lane as usize)
        {
            return Some(r);
        }
        // Re-open fallback. On a re-open the SAME route id comes back and the
        // sender re-establishes its RTP track — so samples land on the lane
        // again — BEFORE the daemon's session re-tags that route's codec as
        // h264. The positional filter above keys on that tag, so it misses the
        // re-opened route and the frames are dropped into the void ("connecting
        // forever" on the second open). The console IS watching the route, so
        // map by position over the video routes we actually watch from this
        // peer — knowledge that doesn't depend on the re-tag timing. Position
        // keeps multi-monitor correct, and an authoritative binding (above)
        // still wins the instant the streamer's VideoLane announce lands.
        let mut watched = self.watched_video_routes_from(canon);
        watched.retain(|route_id| self.inbound_route_network_ok(route_id, peer, network));
        watched.sort_unstable();
        watched.into_iter().nth(lane as usize)
    }

    /// Atomically associate a lane resolution with the process-local route
    /// generation that owned it. The generation mutex is deliberately taken
    /// before [`Self::video_route_for_lane`]; `begin_video_generation` cannot
    /// advance a same-id successor between the two samples.
    fn video_route_generation_for_lane(
        &self,
        network: &str,
        peer: &str,
        lane: u8,
    ) -> (Option<String>, Option<u64>) {
        snapshot_video_route_generation(&self.video_route_generations, || {
            self.video_route_for_lane(network, peer, lane)
        })
    }

    /// Route ids of the inbound video routes this viewer currently watches whose
    /// streamer is `canon` (bare pubkey) — the re-open fallback for
    /// [`Self::video_route_for_lane`]. Cheap: the watcher map holds one entry
    /// per open console stream.
    fn watched_video_routes_from(&self, canon: &str) -> Vec<String> {
        self.video_watchers
            .lock()
            .keys()
            .filter(|rid| {
                rid.strip_prefix("route:")
                    .and_then(|s| s.split_once('→'))
                    .map(|(from, _)| pubkey_part(&node_of(from)) == canon)
                    .unwrap_or(false)
            })
            .cloned()
            .collect()
    }

    /// The audio twin of [`Self::video_route_for_lane`].
    fn audio_route_for_lane(&self, network: &str, peer: &str, lane: u8) -> Option<String> {
        self.sorted_media_routes(peer, false, "opus")
            .into_iter()
            .filter(|route_id| self.inbound_route_network_ok(route_id, peer, network))
            .nth(lane as usize)
    }

    /// The transport for a stream this machine is about to send on
    /// `route` — shared by the display and camera arms of
    /// [`Self::start_media`]: H.264 on the peer's track lane when the
    /// offer asked for it and the route's sorted position falls inside
    /// the effective lane pool; MJPEG over the media channel otherwise,
    /// exactly as v1.
    /// Bounded wait for the daemon's video bring-up before a one-shot
    /// transport decision. Dials are fast now (the area dial has no
    /// discovery pause), and racing the VideoSubscribe probe stripped
    /// h264 and pinned capable pairs on MJPEG for the whole session. A
    /// daemon that truly predates the track lane never flips the flag —
    /// the timeout falls through to the honest MJPEG pick.
    async fn await_video_bringup(&self, peer: &str) {
        const DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);
        let by = std::time::Instant::now() + DEADLINE;
        while !self.peer_video_ready(peer) && std::time::Instant::now() < by {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    fn pick_outbound_video_mode(&self, route: &Route, to_node: &str) -> VideoMode {
        let accepts_h264 = self
            .state
            .lock()
            .session
            .as_ref()
            .and_then(|s| s.route(&route.id))
            .map(|r| r.video.iter().any(|v| v == "h264"))
            .unwrap_or(false);
        let route_video_ready = self.route_video_ready(&route.id, to_node);
        if accepts_h264 && !route_video_ready {
            tracing::warn!(
                "route {} — viewer accepts H.264 but its exact network has no confirmed local video subscription; streaming MJPEG",
                route.id
            );
        }
        // Pin a track lane for this route now (lowest free in the peer's
        // pool). A pin is what lets us tell the viewer a stable binding; no
        // pin (pool exhausted / no daemon lane) means MJPEG, exactly as v1.
        if accepts_h264 && route_video_ready && self.assign_video_lane(to_node, &route.id).is_some()
        {
            VideoMode::H264
        } else {
            VideoMode::Mjpeg
        }
    }

    /// Tell the viewer which video track lane this route streams on, so it
    /// demuxes inbound H.264 by the announced binding instead of a positional
    /// guess. No-op for an MJPEG route (no pinned lane). Best-effort: a viewer
    /// that never hears it (older build, a dropped message) falls back to the
    /// positional lane, exactly as before.
    async fn announce_video_lane(&self, route_id: &str, peer: &str) {
        let Some(lane) = self.video_lane(route_id, peer, true) else {
            return;
        };
        let incarnation = self.route_incarnation(route_id);
        if let Err(e) = self
            .send_control(
                peer,
                &ControlMessage::Route(RouteControl::VideoLane {
                    route_id: route_id.to_string(),
                    incarnation,
                    lane,
                }),
            )
            .await
        {
            tracing::debug!("announcing video lane for {route_id} failed: {e}");
        }
    }

    /// Start the capture behind an outbound display/camera stream, wired
    /// to the packet forwarder and the in-band capture-status reports.
    fn start_video_stream(
        self: &Arc<Self>,
        route: &Route,
        to_node: &str,
        mode: VideoMode,
        source: VideoSource,
    ) {
        let peer = to_node.to_string();
        let status_mesh = Arc::downgrade(self);
        let status_peer = peer.clone();
        let status_route = route.id.clone();
        let route_id = route.id.clone();
        let route_incarnation = self.route_incarnation(&route_id);
        let generation = self.begin_video_generation(&route_id);
        let recovery = Arc::new(VideoRecovery::new(&route_id));
        // One route, one ordered AU queue, one persistent local writer. Focus
        // changes can alter this route's budget immediately without moving its
        // dependent frame chain to a differently scheduled socket.
        let (route_video_out, route_video_rx) =
            mpsc::channel::<VideoOut>(usize::from(VIDEO_HANDOFF_FRAMES));
        self.spawn_video_forwarder(route_video_rx);
        // The LAN gate: the automatic fps/bitrate dials open up only on a
        // link the daemon has classified host↔host. Unknown (ICE not yet
        // introspected) starts conservative; the nudge below upgrades the
        // live stream as soon as the class lands.
        let link = self.route_link_class(&route.id, to_node);
        // A first governed video route makes legacy PCM invalid. Suspend it
        // before taking any policy snapshot so the encoder and readout see one
        // final allocator generation rather than racing an intermediate plan.
        let policy_serial = self.video_policy_apply_serial.lock();
        self.stop_policy_pcm_for_peer(to_node, &policy_serial);
        let (initial_plan, policy_plans) = {
            let mut policy = self.media_policy.lock();
            let _ = policy.register_route(
                pubkey_part(to_node),
                &route.id,
                link == crate::video::LinkClass::Lan,
            );
            let initial = policy.plan(&route.id).cloned().unwrap_or_default();
            let plans = policy.plans_for_peer(pubkey_part(to_node));
            (initial, plans)
        };
        // Adding this display can lower existing siblings' shares. Apply
        // those live where the encoder supports it; the new capture receives
        // its cap in the Tune below.
        for plan in &policy_plans {
            if plan.route_id != route.id {
                self.video.apply_policy_cap(
                    &plan.route_id,
                    Some(plan.route_budget_bps.min(u64::from(u32::MAX)) as u32),
                    plan.auto_resolution,
                );
            }
        }
        if link == crate::video::LinkClass::Unknown {
            // The class usually lands within a couple of seconds of ICE
            // settling — poll the daemon shortly after the stream starts so
            // a LAN viewer isn't stuck on the conservative dials until the
            // next natural refresh (peer approval / snapshot).
            let mesh = Arc::downgrade(self);
            crate::spawn(async move {
                for delay_ms in [2_000u64, 6_000] {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    let Some(mesh) = mesh.upgrade() else { return };
                    mesh.refresh_peer_networks().await;
                }
            });
        }
        // Queue admission is not delivery. Recovery begins on either a full
        // producer queue or a downstream lane/write failure, holds dependent
        // deltas at both sides of the queue, and ends only after the current
        // epoch's IDR reaches the existing media pipe successfully.
        let recovery_mesh = Arc::downgrade(self);
        let capture_recovery = recovery;
        self.video.start_capture(
            route.id.clone(),
            mode,
            source,
            crate::video::Tune {
                link,
                mode: crate::video::parse_posture(initial_plan.effective_mode.wire_name()),
                policy_cap_bps: Some(initial_plan.route_budget_bps.min(u64::from(u32::MAX)) as u32),
                policy_auto_resolution: initial_plan.auto_resolution,
                ..Default::default()
            },
            move |packet| {
                let key = match &packet {
                    VideoPacket::H264 { key, .. } => Some(*key),
                    VideoPacket::Jpeg(_) => None,
                };
                let Some(mesh) = recovery_mesh.upgrade() else {
                    return false;
                };
                let route_budget_bps = {
                    let policy = mesh.media_policy.lock();
                    policy
                        .plan(&route_id)
                        .map(|plan| plan.route_budget_bps)
                        .unwrap_or(u64::MAX)
                };
                if capture_recovery.policy_pauses(&mesh, route_budget_bps, key) {
                    return false;
                }
                if let VideoPacket::Jpeg(frame) = &packet {
                    if !capture_recovery.admits_jpeg(&mesh, route_budget_bps, frame) {
                        return false;
                    }
                }
                if capture_recovery.suppresses(key) {
                    capture_recovery.note_suppressed(&mesh);
                    return false;
                }
                let epoch = capture_recovery.epoch();
                let packet_profile_id = match packet.profile_id() {
                    0 => crate::pipeline_profile::next_frame_id(),
                    id => id,
                };
                let failure = match route_video_out.try_send(VideoOut {
                    peer: peer.clone(),
                    route_id: route_id.clone(),
                    generation,
                    incarnation: route_incarnation.clone(),
                    packet,
                    recovery_epoch: epoch,
                    recovery: capture_recovery.clone(),
                    profile_id: packet_profile_id,
                    enqueued_at: crate::pipeline_profile::stamp(),
                }) {
                    Ok(()) => None,
                    Err(mpsc::error::TrySendError::Full(_)) => Some("route one-frame queue full"),
                    Err(mpsc::error::TrySendError::Closed(_)) => Some("route queue closed"),
                };
                if let Some(reason) = failure {
                    if key.is_some() {
                        capture_recovery.note_drop(&mesh, key, reason);
                    }
                    false
                } else {
                    true
                }
            },
            move |state, detail| {
                // Capture-state transitions travel to the viewer in-band
                // (`vstat`), so its console can explain a black stage
                // instead of just showing one.
                let Some(mesh) = status_mesh.upgrade() else {
                    return;
                };
                let route = status_route.clone();
                if !mesh.video_generation_is_current(&route, generation) {
                    tracing::debug!(
                        "discarding stale video status for {route} generation {generation}"
                    );
                    return;
                }
                let frame = VideoStatusFrame::new(route.clone(), state, detail);
                let peer = status_peer.clone();
                crate::spawn(async move {
                    if !mesh.video_generation_is_current(&route, generation) {
                        return;
                    }
                    let Ok(payload) = serde_json::to_value(&frame) else {
                        return;
                    };
                    if let Err(e) = mesh.send_media_value(&peer, payload).await {
                        tracing::debug!("capture status to {} failed: {e}", short_id(&peer));
                    }
                });
            },
        );
        drop(policy_serial);
        // Route creation itself changes every sibling's allocation. Publish
        // that state even before the viewer touches a control, so the
        // effective panel and background/priority labels never wait on a
        // later Tune. This remains a best-effort message on the established
        // route data channel.
        let mesh = self.clone();
        crate::spawn(async move { mesh.send_effective_plans(policy_plans).await });
    }

    /// The host side of a terminal route going active: spawn this user's
    /// shell and pump its output to the viewer. The owner/fleet gate
    /// already ran at offer time ([`terminal_offer_refusal`]); it's
    /// re-checked here — and on every inbound byte — so a session can
    /// never outlive the authorization that allowed it.
    fn start_terminal_host(self: &Arc<Self>, route: &Route) {
        let viewer = node_of(route.to.as_str());
        let peer = self.route_peer(&route.id).unwrap_or(viewer);
        let rid = route.id.clone();
        if !self.sender_may_drive(&peer, DrivePlane::Terminal) {
            tracing::warn!(
                "route {rid} — terminal for non-controller {} refused",
                short_id(&peer)
            );
            let mesh = self.clone();
            crate::spawn(async move {
                let _ = mesh.disconnect(rid).await;
            });
            return;
        }
        // One pump per viewer route. A duplicate `StartMedia` for this route
        // — the offer arriving on more than one shared network, say — must
        // not spawn a second pump onto it: two pumps fan the one shell's
        // output out twice (doubled/tripled terminal). The first start wins;
        // later duplicates are ignored until the pump ends and releases.
        if !self.term_pumps.lock().insert(rid.clone()) {
            tracing::debug!(
                "route {rid} — terminal pump already running; ignoring duplicate start"
            );
            return;
        }
        // The session the viewer asked to attach to: `Some(id)` joins that
        // shared shell (tmux-style — scrollback replayed, keyboard shared),
        // `None` mints a fresh one. The default emulator size is 80×24; the
        // viewer's first resize reconciles the shared PTY to its real size.
        let requested = self.requested_term_session(&route.id);
        match self
            .terminal
            .open(requested.as_deref(), &rid, TERM_INIT_COLS, TERM_INIT_ROWS)
        {
            Ok(attach) => {
                let session_id = attach.session_id.clone();
                tracing::info!(
                    "route {rid} active — {} terminal session {session_id} for {} ({} now attached)",
                    if attach.created { "hosting new" } else { "attaching to" },
                    short_id(&peer),
                    self.terminal
                        .list_sessions()
                        .iter()
                        .find(|s| s.session_id == session_id)
                        .map(|s| s.attachers)
                        .unwrap_or(1),
                );
                // Record the resolved id on our (host) route and echo it to
                // the viewer on a follow-up Accept, so its UI learns which
                // shell this is (and how to re-attach). Best-effort: the
                // first Accept already started the viewer's media.
                self.record_and_announce_term_session(&route.id, &peer, &session_id);
                let mesh = self.clone();
                crate::spawn(async move {
                    mesh.clone()
                        .pump_term_attach(rid.clone(), peer, attach)
                        .await;
                    // The pump ended (viewer detached, shell exited) — release
                    // the route so a genuine fresh start can pump again.
                    mesh.term_pumps.lock().remove(&rid);
                });
            }
            Err(e) => {
                // The shell never opened — release the route we just claimed.
                self.term_pumps.lock().remove(&rid);
                // Tell the viewer in its own terms — a terminal renders a
                // line of text better than a silently vanished route — then
                // tear the route down.
                tracing::warn!("route {rid} — shell didn't start: {e}");
                let mesh = self.clone();
                crate::spawn(async move {
                    let note = format!("[couldn't start a shell here: {e}]\r\n");
                    for frame in [
                        TermFrame::new(
                            &rid,
                            0,
                            TermEvent::Data {
                                bytes: note.into_bytes(),
                            },
                        ),
                        TermFrame::new(&rid, 1, TermEvent::Exit { code: None }),
                    ] {
                        if let Ok(payload) = serde_json::to_value(&frame) {
                            let _ = mesh.send_media_value(&peer, payload).await;
                        }
                    }
                    let _ = mesh.disconnect(rid).await;
                });
            }
        }
    }

    /// The terminal session this route asked to attach to, from the session
    /// snapshot — `Some(id)` for an explicit attach, `None` for "new shell".
    fn requested_term_session(&self, route_id: &str) -> Option<String> {
        self.state
            .lock()
            .session
            .as_ref()
            .and_then(|s| s.route(route_id))
            .and_then(|r| r.term_session.clone())
    }

    /// Record the resolved terminal session id on this (host) route, then
    /// echo it to the viewer with a follow-up `Accept` so its UI learns the
    /// shared id (for "shared with N" and re-attach). The first Accept the
    /// session auto-sent already started the viewer's media; this one only
    /// carries the resolved id.
    fn record_and_announce_term_session(
        self: &Arc<Self>,
        route_id: &str,
        peer: &str,
        session: &str,
    ) {
        {
            let mut st = self.state.lock();
            if let Some(s) = st.session.as_mut() {
                s.set_term_session(route_id, session.to_string());
            }
        }
        self.emit_snapshot();
        let incarnation = self.route_incarnation(route_id);
        let mesh = self.clone();
        let peer = peer.to_string();
        let route_id = route_id.to_string();
        let session = session.to_string();
        crate::spawn(async move {
            let _ = mesh
                .send_control(
                    &peer,
                    &ControlMessage::Route(RouteControl::Accept {
                        route_id,
                        incarnation,
                        session: Some(session),
                    }),
                )
                .await;
        });
    }

    /// Pump one attacher's view of a shared terminal session to its viewer:
    /// replay the scrollback first (a fresh attach paints the current
    /// screen), then forward the session's live broadcast — this attacher's
    /// own pump to its own viewer route, so several viewers on one session
    /// each get the output (and, via `term_send`→`terminal.write`, each type
    /// into the one shell). `Lagged` skips ahead (output is live media);
    /// `Closed`/`Exit` ends *this* viewer's pump only.
    async fn pump_term_attach(
        self: Arc<Self>,
        rid: String,
        peer: String,
        attach: crate::terminal::TermAttach,
    ) {
        use tokio::sync::broadcast::error::RecvError;
        let crate::terminal::TermAttach {
            scrollback, mut rx, ..
        } = attach;
        let mut seq: u64 = 0;
        let mut last_ok = std::time::Instant::now();
        let mut last_warn = std::time::Instant::now() - WARN_EVERY;

        // Replay the current screen to *this* viewer before the live stream.
        if !scrollback.is_empty() {
            for frame in TermFrame::data_frames(&rid, seq, &scrollback, MAX_TERM_DATA_BYTES) {
                seq = frame.seq + 1;
                if let Ok(payload) = serde_json::to_value(&frame) {
                    let _ = self.send_media_value(&peer, payload).await;
                }
            }
        }

        loop {
            let msg = match rx.recv().await {
                Ok(msg) => msg,
                // A slow attacher fell behind the broadcast ring — output is
                // live media, so skip ahead rather than wedge the shell.
                Err(RecvError::Lagged(n)) => {
                    tracing::debug!("terminal {rid} — viewer lagged {n} chunks; skipping ahead");
                    continue;
                }
                // The session ended (shell exited / closed) — end this pump.
                Err(RecvError::Closed) => return,
            };
            // This viewer detached (closed its tab, or its route was torn
            // down) — stop pumping to it. The shell lives on for the other
            // attachers; the last one leaving arms the idle reaper. Checked
            // here so a closed viewer's pump never keeps streaming to a dead
            // route.
            if !self.terminal.is_attached(&rid) {
                return;
            }
            match msg {
                OutMsg::Data(bytes) => {
                    for frame in TermFrame::data_frames(&rid, seq, &bytes, MAX_TERM_DATA_BYTES) {
                        seq = frame.seq + 1;
                        let Ok(payload) = serde_json::to_value(&frame) else {
                            continue;
                        };
                        match self.send_media_value(&peer, payload).await {
                            Ok(()) => last_ok = std::time::Instant::now(),
                            Err(e) => {
                                if last_warn.elapsed() >= WARN_EVERY {
                                    last_warn = std::time::Instant::now();
                                    tracing::warn!(
                                        "terminal output to {} failed: {e}",
                                        short_id(&peer)
                                    );
                                }
                                // Nothing else reaps a session whose viewer
                                // silently vanished (peer drops never reach
                                // the session) — the pump is the watchdog.
                                // Detach this viewer only; the shell lives on
                                // for the other attachers (or a re-attach
                                // that replays scrollback), never killed
                                // because one viewer's link blipped.
                                if last_ok.elapsed() > TERM_SEND_PATIENCE {
                                    tracing::warn!(
                                        "terminal {rid} — viewer unreachable; detaching (shell kept for reattach)"
                                    );
                                    self.terminal.detach(&rid);
                                    return;
                                }
                            }
                        }
                    }
                }
                OutMsg::Resize { cols, rows } => {
                    // The shared PTY's authoritative size changed — tell this
                    // viewer so it renders (letterboxes) to the one shell's
                    // size and its wrapping matches everyone else's.
                    let frame = TermFrame::new(&rid, seq, TermEvent::Resize { cols, rows });
                    seq += 1;
                    if let Ok(payload) = serde_json::to_value(&frame) {
                        let _ = self.send_media_value(&peer, payload).await;
                    }
                }
                OutMsg::Exit(code) => {
                    tracing::info!("terminal {rid} — shell ended ({code:?})");
                    let frame = TermFrame::new(&rid, seq, TermEvent::Exit { code });
                    if let Ok(payload) = serde_json::to_value(&frame) {
                        let _ = self.send_media_value(&peer, payload).await;
                    }
                    // The shell ended for *everyone* on this session — tear
                    // this viewer's route down. Other attachers' pumps see
                    // the same `Exit`/`Closed` and end on their own.
                    let _ = self.disconnect(rid.clone()).await;
                    return;
                }
            }
        }
    }

    /// A **loopback** terminal route going active: a terminal to the very
    /// machine we're sitting at, where this node is both shell *and* viewer.
    /// There's no peer, so instead of framing the PTY's output onto the mesh
    /// we feed it straight into the local viewer queue (the same one the
    /// remote viewer path enqueues into) and poke the window — the Terminal
    /// UI can't tell a loopback session from a remote one. Keystrokes and
    /// resizes from the window short-circuit to `terminal.write/resize`
    /// locally (see [`Self::term_send`]). The owner/fleet gate is re-cleared
    /// for consistency with the remote host path — it's our own machine, so
    /// it passes.
    fn start_terminal_loopback(self: &Arc<Self>, route: &Route) {
        let rid = route.id.clone();
        // The peer here is ourselves; the gate must still pass (owner or a
        // fleet member always controls their own machine), and re-running it
        // keeps the loopback path honest with the remote one.
        let peer = self
            .route_peer(&rid)
            .unwrap_or_else(|| node_of(route.to.as_str()));
        if !self.sender_may_drive(&peer, DrivePlane::Terminal) {
            tracing::warn!(
                "route {rid} — local terminal refused (not owner/fleet of this machine)"
            );
            let mesh = self.clone();
            crate::spawn(async move {
                let _ = mesh.disconnect(rid).await;
            });
            return;
        }
        // One pump per route, exactly as the remote host path: a duplicate
        // local `StartMedia` must not spawn a second loopback pump onto this
        // route (which would double the window's output). First start wins.
        if !self.term_pumps.lock().insert(rid.clone()) {
            tracing::debug!(
                "route {rid} — local terminal pump already running; ignoring duplicate"
            );
            return;
        }
        // Buffer output from the very first byte — the shell's prompt is
        // produced right after Accept, before the window has subscribed, and
        // a dropped terminal byte never heals.
        self.terminal.ensure_queue(&rid);
        // The session this local window asked to attach to: `Some(id)` lets
        // two local windows share one local shell (multi-attach to yourself),
        // `None` mints a fresh one — the same session model as the remote
        // host path, just feeding the local queue instead of the mesh.
        let requested = self.requested_term_session(&rid);
        match self
            .terminal
            .open(requested.as_deref(), &rid, TERM_INIT_COLS, TERM_INIT_ROWS)
        {
            Ok(attach) => {
                let session_id = attach.session_id.clone();
                tracing::info!(
                    "route {rid} active — local terminal session {session_id} ({})",
                    if attach.created {
                        "new shell"
                    } else {
                        "attached"
                    },
                );
                // Record the resolved id locally so a snapshot surfaces it
                // (the loopback UI shows the same "shared with N" line); there
                // is no peer to Accept back to.
                {
                    let mut st = self.state.lock();
                    if let Some(s) = st.session.as_mut() {
                        s.set_term_session(&rid, session_id.clone());
                    }
                }
                self.emit_snapshot();
                let crate::terminal::TermAttach {
                    scrollback, mut rx, ..
                } = attach;
                // Replay the current screen into this window's queue first
                // (an attach to an already-running local shell paints it),
                // then pump the shared broadcast in.
                if !scrollback.is_empty() && self.terminal.enqueue(&rid, scrollback) {
                    self.sink.emit("allmystuff://term-ready", json!(rid));
                }
                let mesh = self.clone();
                crate::spawn(async move {
                    use tokio::sync::broadcast::error::RecvError;
                    loop {
                        let msg = match rx.recv().await {
                            Ok(msg) => msg,
                            Err(RecvError::Lagged(_)) => continue,
                            Err(RecvError::Closed) => break,
                        };
                        match msg {
                            OutMsg::Data(bytes) => {
                                // Straight into the local viewer queue. A
                                // queue going empty → non-empty is the cue to
                                // poke the window, exactly as the inbound
                                // remote viewer path does.
                                if mesh.terminal.enqueue(&rid, bytes) {
                                    mesh.sink.emit("allmystuff://term-ready", json!(rid));
                                }
                            }
                            OutMsg::Resize { cols, rows } => {
                                // Two local windows sharing one shell: tell this
                                // window the shared size so it letterboxes to it.
                                mesh.sink.emit(
                                    "allmystuff://term-resize",
                                    json!({ "route": rid, "cols": cols, "rows": rows }),
                                );
                            }
                            OutMsg::Exit(code) => {
                                tracing::info!("local terminal {rid} — shell ended ({code:?})");
                                mesh.sink.emit(
                                    "allmystuff://term-exit",
                                    json!({ "route": rid, "code": code }),
                                );
                                let _ = mesh.disconnect(rid.clone()).await;
                                break;
                            }
                        }
                    }
                    // Pump ended — release the route so a fresh start can pump.
                    mesh.term_pumps.lock().remove(&rid);
                });
            }
            Err(e) => {
                // The shell never opened — release the route we just claimed.
                self.term_pumps.lock().remove(&rid);
                // Render the failure to the window in its own terms — a line
                // of text, then the exit — then tear the route down.
                tracing::warn!("route {rid} — local shell didn't start: {e}");
                let note = format!("[couldn't start a shell here: {e}]\r\n");
                if self.terminal.enqueue(&rid, note.into_bytes()) {
                    self.sink.emit("allmystuff://term-ready", json!(rid));
                }
                self.sink.emit(
                    "allmystuff://term-exit",
                    json!({ "route": rid, "code": serde_json::Value::Null }),
                );
                let mesh = self.clone();
                crate::spawn(async move {
                    let _ = mesh.disconnect(rid).await;
                });
            }
        }
    }

    /// Whether a terminal frame on `route` is fresh (record its seq and take
    /// it) or a duplicate to drop — used both for output the viewer takes
    /// (`term_rx_seq`) and input the host takes (`term_in_seq`). Each sending
    /// side numbers a route's frames strictly increasing, so any seq at or
    /// below the last we took is the same send arriving again over another
    /// shared network (control and media ride them all). A forward jump (the
    /// sender skipped ahead after a broadcast lag) is still fresh.
    fn accept_term_seq(seqs: &Mutex<HashMap<String, u64>>, route: &str, seq: u64) -> bool {
        let mut seqs = seqs.lock();
        match seqs.get(route) {
            Some(&last) if seq <= last => false,
            _ => {
                seqs.insert(route.to_string(), seq);
                true
            }
        }
    }

    /// One inbound terminal frame. Which side we are comes from the route
    /// itself: keystrokes/resizes landing on the *host* (the route sources
    /// here) clear the same two gates as input injection — live route from
    /// this exact sender, and the sender being an authorized controller;
    /// output/exit landing on the *viewer* (the route sinks here) goes to
    /// the watching terminal window.
    fn handle_term_frame(&self, from: &str, frame: TermFrame) {
        let Some(me) = self.local_node_id() else {
            return;
        };
        let (hosts_here, views_here) = {
            let st = self.state.lock();
            let Some(r) = st.session.as_ref().and_then(|s| s.route(&frame.route)) else {
                return;
            };
            if !(r.is_active()
                && is_terminal_route(&r.route)
                && pubkey_part(r.peer.as_str()) == pubkey_part(from))
            {
                tracing::debug!(
                    "terminal frame for {} refused (route not live here)",
                    frame.route
                );
                return;
            }
            (
                node_of(r.route.from.as_str()) == me,
                node_of(r.route.to.as_str()) == me,
            )
        };
        if hosts_here {
            if !self.sender_may_drive(from, DrivePlane::Terminal) {
                tracing::warn!("dropped terminal input from {from}: not an authorized controller");
                return;
            }
            // Drop a duplicate keystroke/resize: the viewer numbers its
            // outbound frames strictly increasing, so a seq we've already
            // applied is the same send redelivered on another shared network.
            // Without this the PTY is written N times and the shell echoes
            // `aaaa` for one keypress.
            if !Self::accept_term_seq(&self.term_in_seq, &frame.route, frame.seq) {
                return;
            }
            match frame.event {
                TermEvent::Data { bytes } => {
                    let _ = self.terminal.write(&frame.route, bytes);
                }
                TermEvent::Resize { cols, rows } => {
                    let _ = self.terminal.resize(&frame.route, cols, rows);
                }
                // Ending the shell is the host's report, never the
                // viewer's request — a viewer ends a session by tearing
                // the route down.
                TermEvent::Exit { .. } => {}
                // A terminal event a newer viewer introduced — ignore it.
                TermEvent::Unknown => {}
            }
        } else if views_here {
            // Drop a duplicate delivery (see `accept_term_seq`): the same send
            // arriving again over another shared network. Without this the
            // window paints every byte — and the shell appears to echo every
            // keystroke — once per shared network: the doubled/tripled terminal.
            if !Self::accept_term_seq(&self.term_rx_seq, &frame.route, frame.seq) {
                return;
            }
            match frame.event {
                TermEvent::Data { bytes } => {
                    if self.terminal.enqueue(&frame.route, bytes) {
                        // Queue went empty → non-empty: poke the window to
                        // drain (a lost poke costs latency, never bytes —
                        // the safety poll catches up).
                        self.sink
                            .emit("allmystuff://term-ready", json!(frame.route));
                    }
                }
                TermEvent::Exit { code } => {
                    self.sink.emit(
                        "allmystuff://term-exit",
                        json!({ "route": frame.route, "code": code }),
                    );
                }
                TermEvent::Resize { cols, rows } => {
                    // The host's authoritative shared size — the window renders
                    // (letterboxes) to it so its wrapping matches the one shell.
                    self.sink.emit(
                        "allmystuff://term-resize",
                        json!({ "route": frame.route, "cols": cols, "rows": rows }),
                    );
                }
                // A terminal event a newer host introduced — ignore it.
                TermEvent::Unknown => {}
            }
        }
    }

    /// Front-end command: keystrokes/resizes from a terminal window down
    /// its active terminal route. This machine must be the route's
    /// *viewer* (its sink side); `Exit` is the host's word and is refused.
    pub async fn term_send(
        self: &Arc<Self>,
        route_id: String,
        event: TermEvent,
    ) -> Result<(), String> {
        let me = self.local_node_id().ok_or("mesh not ready")?;
        let (peer, loopback) = {
            let st = self.state.lock();
            let r = st
                .session
                .as_ref()
                .and_then(|s| s.route(&route_id))
                .ok_or("unknown route")?;
            // Endpoint self-checks compare *canonically*: the UI builds the
            // route's host endpoint from the suffixed display id while `me` is
            // the bare node id, so a raw `==` misses a genuine self-route (see
            // `same_node`). This machine must be the route's viewer…
            if !(r.is_active()
                && is_terminal_route(&r.route)
                && same_node(&node_of(r.route.to.as_str()), &me))
            {
                return Err("route isn't an active terminal session here".into());
            }
            // …and a terminal whose *source* is this machine too has no peer to
            // frame to: the shell is hosted right here, so input/resize go
            // straight to the local PTY rather than out over the mesh. The raw
            // `==` this replaces left a loopback ConPTY blank on Windows — the
            // viewer's cursor-position reply (CSI 6 n) was framed to a
            // non-existent peer, and ConPTY withholds all output until that
            // reply lands.
            let loopback = same_node(&node_of(r.route.from.as_str()), &me);
            (r.peer.to_string(), loopback)
        };
        if loopback {
            match event {
                TermEvent::Data { bytes } => {
                    let _ = self.terminal.write(&route_id, bytes);
                    return Ok(());
                }
                TermEvent::Resize { cols, rows } => {
                    let _ = self.terminal.resize(&route_id, cols, rows);
                    return Ok(());
                }
                TermEvent::Exit { .. } => {
                    return Err("exit is reported by the host, not sent".into())
                }
                TermEvent::Unknown => return Err("unknown terminal event".into()),
            }
        }
        match event {
            TermEvent::Data { bytes } => {
                // A paste can be arbitrarily large: chunk to the channel
                // budget and await each send, so big pastes throttle
                // themselves instead of flooding the daemon.
                let frames = TermFrame::data_frames(&route_id, 0, &bytes, MAX_TERM_DATA_BYTES);
                let first = self
                    .term_seq
                    .fetch_add(frames.len() as u64, Ordering::Relaxed);
                for (i, mut frame) in frames.into_iter().enumerate() {
                    frame.seq = first + i as u64;
                    let payload = serde_json::to_value(&frame).map_err(|e| e.to_string())?;
                    self.send_media_value(&peer, payload).await?;
                }
                Ok(())
            }
            TermEvent::Resize { .. } => {
                let seq = self.term_seq.fetch_add(1, Ordering::Relaxed);
                let frame = TermFrame::new(&route_id, seq, event);
                let payload = serde_json::to_value(&frame).map_err(|e| e.to_string())?;
                self.send_media_value(&peer, payload).await
            }
            TermEvent::Exit { .. } => Err("exit is reported by the host, not sent".into()),
            // We never originate an `Unknown` event; reject it for exhaustiveness.
            TermEvent::Unknown => Err("unknown terminal event".into()),
        }
    }

    /// A terminal window claims an active route's buffered output (returns
    /// the token scoping its unwatch). Pure plumbing to [`TerminalHost`].
    pub fn term_watch(&self, route_id: &str) -> u64 {
        self.terminal.watch_output(route_id)
    }

    pub fn term_unwatch(&self, route_id: &str, token: u64) {
        self.terminal.unwatch(route_id, token);
    }

    /// Drain buffered terminal output (`[u32 le len][bytes]…`), emptied by
    /// the window on each `allmystuff://term-ready` poke or safety poll.
    pub fn term_poll(&self, route_id: &str) -> Vec<u8> {
        self.terminal.poll(route_id)
    }

    /// Front-end command: ask `node` for its open terminal sessions so the
    /// picker can offer to *attach* to one (multi-attach) instead of always
    /// minting a new shell. For a remote machine this fires a
    /// [`RouteControl::TerminalSessionsRequest`]; the host's answer arrives
    /// asynchronously as an `allmystuff://terminal-sessions` event. For the
    /// **local** machine there's no peer to ask — we answer at once from our
    /// own [`TerminalHost`], returning the list directly (and `None` for a
    /// remote ask, whose reply rides the event). Gated owner/fleet exactly
    /// like opening a terminal — the host re-checks it too.
    pub async fn request_terminal_sessions(
        self: &Arc<Self>,
        node: String,
    ) -> Result<Option<Vec<TerminalSessionInfo>>, String> {
        let me = self.local_node_id().ok_or("mesh not ready")?;
        if pubkey_part(&node) == pubkey_part(&me) {
            // Our own shells — answer straight from the local host.
            return Ok(Some(self.terminal_session_infos()));
        }
        self.send_control(
            &node,
            &ControlMessage::Route(RouteControl::TerminalSessionsRequest),
        )
        .await?;
        Ok(None)
    }

    /// The local terminal host's open sessions in the protocol's wire shape.
    fn terminal_session_infos(&self) -> Vec<TerminalSessionInfo> {
        self.terminal
            .list_sessions()
            .into_iter()
            .map(|s| TerminalSessionInfo {
                session_id: s.session_id,
                title: s.title,
                created_unix: s.created_unix,
                attachers: s.attachers,
            })
            .collect()
    }

    /// Answer a viewer's [`RouteControl::TerminalSessionsRequest`]: reply on
    /// the control channel with this host's open terminal sessions — gated by
    /// the same owner/fleet check the terminal host itself uses, so a
    /// stranger on the mesh can't even enumerate our shells.
    async fn handle_terminal_sessions_request(self: &Arc<Self>, from: &str) {
        if !self.sender_may_drive(from, DrivePlane::Terminal) {
            tracing::warn!(
                "terminal-sessions request from {} ignored: not owner/fleet",
                short_id(from)
            );
            return;
        }
        let sessions = self.terminal_session_infos();
        let _ = self
            .send_control(
                from,
                &ControlMessage::Route(RouteControl::TerminalSessions { sessions }),
            )
            .await;
    }

    /// One inbound file frame. Which side we are comes from the route
    /// itself: requests landing on the *host* (the route sources here)
    /// clear the same two gates as terminal input — live route from this
    /// exact sender, and the sender being an authorized controller;
    /// responses landing on the *viewer* (the route sinks here) go to the
    /// watching files window — except chunks of a registered download,
    /// which stream straight to disk.
    fn handle_file_frame(self: &Arc<Self>, from: &str, frame: FileFrame) {
        let Some(me) = self.local_node_id() else {
            return;
        };
        let (hosts_here, views_here, shared) = {
            let st = self.state.lock();
            let Some(r) = st.session.as_ref().and_then(|s| s.route(&frame.route)) else {
                return;
            };
            let shared = is_shared_route(&r.route);
            if !(r.is_active()
                && (is_files_route(&r.route) || shared)
                && pubkey_part(r.peer.as_str()) == pubkey_part(from))
            {
                tracing::debug!(
                    "file frame for {} refused (route not live here)",
                    frame.route
                );
                return;
            }
            (
                node_of(r.route.from.as_str()) == me,
                node_of(r.route.to.as_str()) == me,
                shared,
            )
        };
        if hosts_here && shared {
            // A Shared Files lane: token-gated, never owner/fleet, and only
            // ever a `Fetch` — no path browsing, no writes. The token's
            // allow-list (the room's members, as the uploader stated them)
            // is the gate, re-cleared per request.
            match &frame.event {
                FileEvent::Fetch { req, token } => match self.shared_path_for(token, from) {
                    Some(path) => self.start_files_request(
                        &frame.route,
                        from,
                        FileEvent::Read { req: *req, path },
                    ),
                    None => {
                        tracing::warn!(
                            "dropped shared-file fetch from {}: token not shared with them",
                            short_id(from)
                        );
                        self.send_file_event(
                            frame.route.clone(),
                            from.to_string(),
                            FileEvent::Err {
                                req: *req,
                                reason: "that file isn't shared with you (or no longer is)".into(),
                            },
                        );
                    }
                },
                // A `:shared` route carries nothing else from the viewer.
                other => tracing::debug!("shared-files host ignoring {other:?}"),
            }
        } else if hosts_here {
            if !self.sender_may_drive(from, DrivePlane::Files) {
                tracing::warn!("dropped file request from {from}: not an authorized controller");
                return;
            }
            match &frame.event {
                // Upload pieces are applied inline: pieces of one upload
                // must land in arrival order (the viewer sends them
                // sequentially), and a piece is one small append.
                FileEvent::Write { .. } => {
                    if let Some(reply) = crate::files::write_piece(&frame.event) {
                        self.send_file_event(frame.route.clone(), from.to_string(), reply);
                    }
                }
                FileEvent::List { .. }
                | FileEvent::Read { .. }
                | FileEvent::Mkdir { .. }
                | FileEvent::Rename { .. }
                | FileEvent::Delete { .. } => {
                    self.start_files_request(&frame.route, from, frame.event);
                }
                // Response kinds (and `Fetch`, which only a `:shared` route
                // serves) landing on the files host are a confused peer.
                _ => {}
            }
        } else if views_here {
            // A chunk of a registered download streams to disk, not to
            // the window; everything else is queued for the window.
            if let FileEvent::Chunk { req, .. } = &frame.event {
                if self.feed_download(&frame.route, *req, &frame.event) {
                    return;
                }
            }
            if let FileEvent::Err { req, .. } = &frame.event {
                // A failed request that had a download registered: close
                // and discard the partial file, then let the window see
                // the error too.
                self.fail_download(&frame.route, *req, &frame.event);
            }
            let Ok(bytes) = serde_json::to_vec(&frame) else {
                return;
            };
            if self.files.enqueue(&frame.route, bytes) {
                self.sink
                    .emit("allmystuff://file-ready", json!(frame.route));
            }
        }
    }

    /// Host side: run one request against the local filesystem and pump
    /// its response events back to the viewer. A send failure aborts the
    /// pump (dropping the receiver cancels the op at its next chunk) —
    /// unlike a shell, a request/response op is simply retried by the
    /// viewer.
    fn start_files_request(self: &Arc<Self>, route_id: &str, peer: &str, event: FileEvent) {
        let mut rx = self.files.handle(route_id, event);
        let mesh = self.clone();
        let rid = route_id.to_string();
        let peer = peer.to_string();
        crate::spawn(async move {
            while let Some(ev) = rx.recv().await {
                let seq = mesh.file_seq.fetch_add(1, Ordering::Relaxed);
                let frame = FileFrame::new(&rid, seq, ev);
                let Ok(payload) = serde_json::to_value(&frame) else {
                    continue;
                };
                if let Err(e) = mesh.send_media_value(&peer, payload).await {
                    tracing::warn!("file response to {} failed: {e}", short_id(&peer));
                    return; // dropping rx cancels the op
                }
            }
        });
    }

    /// Send one host-side file event (an upload piece's reply) to the
    /// viewer, fire-and-forget.
    fn send_file_event(self: &Arc<Self>, route_id: String, peer: String, event: FileEvent) {
        let mesh = self.clone();
        crate::spawn(async move {
            let seq = mesh.file_seq.fetch_add(1, Ordering::Relaxed);
            let frame = FileFrame::new(&route_id, seq, event);
            if let Ok(payload) = serde_json::to_value(&frame) {
                if let Err(e) = mesh.send_media_value(&peer, payload).await {
                    tracing::warn!("file reply to {} failed: {e}", short_id(&peer));
                }
            }
        });
    }

    /// Front-end command: one file *request* from a files window down its
    /// active files route. This machine must be the route's *viewer* (its
    /// sink side); response kinds are the host's word and are refused.
    pub async fn file_send(
        self: &Arc<Self>,
        route_id: String,
        event: FileEvent,
    ) -> Result<(), String> {
        // A `Fetch` rides a `:shared` route (the Shared Files area); every
        // other request rides a `:files` route (the file manager). Pairing
        // the event to its route keeps a shared lane fetch-only.
        let want_shared = matches!(event, FileEvent::Fetch { .. });
        match event {
            FileEvent::List { .. }
            | FileEvent::Read { .. }
            | FileEvent::Fetch { .. }
            | FileEvent::Write { .. }
            | FileEvent::Mkdir { .. }
            | FileEvent::Rename { .. }
            | FileEvent::Delete { .. } => {}
            _ => return Err("responses come from the host, not the viewer".into()),
        }
        let me = self.local_node_id().ok_or("mesh not ready")?;
        let peer = {
            let st = self.state.lock();
            let r = st
                .session
                .as_ref()
                .and_then(|s| s.route(&route_id))
                .ok_or("unknown route")?;
            let kind_ok = if want_shared {
                is_shared_route(&r.route)
            } else {
                is_files_route(&r.route)
            };
            if !(r.is_active() && kind_ok && node_of(r.route.to.as_str()) == me) {
                return Err("route isn't an active files session here".into());
            }
            r.peer.to_string()
        };
        let seq = self.file_seq.fetch_add(1, Ordering::Relaxed);
        let frame = FileFrame::new(&route_id, seq, event);
        let payload = serde_json::to_value(&frame).map_err(|e| e.to_string())?;
        self.send_media_value(&peer, payload).await
    }

    // ---- sites (the reverse proxy) --------------------------------------

    /// This machine's discovered listening services (the full set, so the
    /// UI can offer each to expose). The active banner probe runs here, off
    /// the presence-build path, with a short per-port timeout.
    pub fn site_scan(&self) -> Vec<allmystuff_inventory::ListeningService> {
        let mut listening = allmystuff_inventory::scan().listening;
        allmystuff_inventory::listening::probe_services(
            &mut listening,
            std::time::Duration::from_millis(200),
        );
        // Diagnostic: which listening ports the scan found (set
        // ALLMYSTUFF_GUI_LOG=info to see it). "0 found" on a box that's
        // clearly serving means the platform probe came up empty.
        tracing::info!(
            "site scan found {} listening service(s): {}",
            listening.len(),
            listening
                .iter()
                .map(|s| format!(":{}", s.port))
                .collect::<Vec<_>>()
                .join(" ")
        );
        listening
    }

    /// The services this machine currently advertises, as id → display name.
    pub fn site_exposed(&self) -> std::collections::BTreeMap<String, String> {
        self.sites.exposed_map()
    }

    /// Set the exposed set (id → display name) and re-stamp presence so peers
    /// see the change.
    pub async fn site_set_exposed(
        self: &Arc<Self>,
        exposed: std::collections::BTreeMap<String, String>,
    ) -> std::collections::BTreeMap<String, String> {
        let map = self.sites.set_exposed(exposed);
        // Rebuild + re-advertise this node's profile (its `sites` follow the
        // exposed set). Re-broadcast so peers' Sites tabs update promptly.
        self.restamp_profile().await;
        map
    }

    /// Every site this device currently has mapped: `(node, host_port,
    /// local_port)`.
    pub fn site_mappings(&self) -> Vec<(String, u16, u16)> {
        self.sites.list_mappings()
    }

    // ---- remote site management (a fleet device's drawer) -------------

    /// Ask a co-owned machine for its full site list (to manage its exposure
    /// from its drawer). The reply lands as the `allmystuff://node-sites`
    /// event. Fire-and-forget; the far side gates on owner/fleet.
    pub async fn site_remote_list(self: &Arc<Self>, node: String) -> Result<(), String> {
        self.send_control(&node, &ControlMessage::Site(SiteControl::List))
            .await
    }

    /// Tell a co-owned machine to advertise exactly `exposed` (id → name).
    /// The far side gates on owner/fleet, applies it, and re-advertises.
    pub async fn site_remote_set_exposed(
        self: &Arc<Self>,
        node: String,
        exposed: std::collections::BTreeMap<String, String>,
    ) -> Result<(), String> {
        self.send_control(
            &node,
            &ControlMessage::Site(SiteControl::SetExposed { exposed }),
        )
        .await
    }

    /// One inbound site-management control message. `List` / `SetExposed` are
    /// privileged (they read or change what this machine exposes), so only an
    /// owner/fleet sender is answered — the same gate as the proxy itself.
    /// `Sites` is a reply we surface to the front-end.
    async fn handle_site_control(self: &Arc<Self>, from: &str, sc: SiteControl) {
        match sc {
            SiteControl::List => {
                if !self.sender_may_drive(from, DrivePlane::Sites) {
                    tracing::warn!("site list from {} refused: not owner/fleet", short_id(from));
                    return;
                }
                // Scan + probe is blocking, so do it off the event loop, then
                // reply to the asking machine.
                let mesh = self.clone();
                let peer = from.to_string();
                crate::spawn(async move {
                    let scan = mesh.clone();
                    let Ok((services, exposed)) = tokio::task::spawn_blocking(move || {
                        let services = scan
                            .site_scan()
                            .into_iter()
                            .map(|s| SiteService {
                                id: s.id,
                                name: s.name,
                                port: s.port,
                                scheme: s.scheme,
                                loopback: s.loopback,
                                process: s.process,
                                title: s.title,
                            })
                            .collect::<Vec<_>>();
                        (services, scan.sites.exposed_map())
                    })
                    .await
                    else {
                        return;
                    };
                    let _ = mesh
                        .send_control(
                            &peer,
                            &ControlMessage::Site(SiteControl::Sites { services, exposed }),
                        )
                        .await;
                });
            }
            SiteControl::Sites { services, exposed } => {
                // A managed machine's answer — hand it to the drawer.
                self.sink.emit(
                    "allmystuff://node-sites",
                    serde_json::json!({ "from": from, "services": services, "exposed": exposed }),
                );
            }
            SiteControl::SetExposed { exposed } => {
                if !self.sender_may_control(from) {
                    tracing::warn!(
                        "site set-exposed from {} refused: not owner/fleet",
                        short_id(from)
                    );
                    return;
                }
                self.sites.set_exposed(exposed);
                self.restamp_profile().await;
            }
            // A site-management kind a newer build introduced — ignore it.
            SiteControl::Unknown => {}
        }
    }

    /// Map a peer's site to a local port: bind a local listener (direct port
    /// when free, else remapped), offer the reverse-proxy route, and start
    /// the accept loop. Returns the bound local port. The far side gates the
    /// offer owner/fleet and re-checks every connection's port against its
    /// own exposed allow-list.
    pub async fn site_map(self: &Arc<Self>, node: String, port: u16) -> Result<u16, String> {
        let me = self.local_node_id().ok_or("mesh not ready")?;
        if pubkey_part(&node) == pubkey_part(&me) {
            return Err("that's this device".into());
        }
        // Already mapped? Hand back the existing local port (idempotent).
        if let Some((_, _, lp)) = self
            .sites
            .list_mappings()
            .into_iter()
            .find(|(n, hp, _)| pubkey_part(n) == pubkey_part(&node) && *hp == port)
        {
            return Ok(lp);
        }
        // Bind a local listener, preferring the same port number, then a free
        // one — the OS is the final arbiter, so retry on a lost race.
        let (listener, local_port) = self.bind_site_listener(port).await?;
        self.establish_site_route(node, port, listener, local_port)
            .await?;
        Ok(local_port)
    }

    /// Offer a site route for `node`:`host_port` over an already-bound local
    /// `listener` (on `local_port`), start its accept loop, and record the
    /// mapping. The route is minted the same way every time — generic media,
    /// source `<host>:site`, a per-mapping viewer sink — so both a fresh
    /// [`Self::site_map`] and a post-reconnect [`Self::remap_site_route`] speak
    /// the identical contract. Returns the minted route id.
    async fn establish_site_route(
        self: &Arc<Self>,
        node: String,
        host_port: u16,
        listener: tokio::net::TcpListener,
        local_port: u16,
    ) -> Result<String, String> {
        let me = self.local_node_id().ok_or("mesh not ready")?;
        // Mint the route: generic media, source `<host>:site`, sink a
        // per-mapping viewer endpoint (never a catalog capability — shape is
        // the contract, like terminal/files).
        let seq = self.site_seq.fetch_add(1, Ordering::Relaxed);
        let from = format!("{node}:site");
        let to = format!("{me}:site-view:{}-{seq}", host_port);
        let incarnation = self.next_route_incarnation(&node);
        let route_id = format!("route:{from}→{to}");
        // Offer the route through the session (drives offer→accept→active).
        let msg = {
            let mut st = self.state.lock();
            let s = st.session.as_mut().ok_or("mesh not ready")?;
            let route = Route {
                id: route_id.clone(),
                from: from.clone().into(),
                to: to.clone().into(),
                media: MediaKind::Generic,
            };
            s.offer_with_incarnation(route, node.as_str(), Vec::new(), Vec::new(), incarnation)
        };
        if let Err(e) = self.send_control(&node, &msg).await {
            let mut st = self.state.lock();
            if let Some(s) = st.session.as_mut() {
                let _ = s.teardown(&route_id);
            }
            return Err(e);
        }
        // Start accepting local connections; each becomes one tunneled conn.
        let accept = self.spawn_site_accept(route_id.clone(), node.clone(), host_port, listener);
        self.sites.add_mapping(
            route_id.clone(),
            ClientMapping::new(node, host_port, local_port, accept),
        );
        Ok(route_id)
    }

    /// Auto-re-map a site whose host just rejected its route — a KVM reconnect
    /// or network change tore the old route down and the host NACKed a stray
    /// frame. Re-offers a fresh route onto the *same* local port so an open
    /// `localhost:<port>` keeps working with no manual unmap/remap. Bounded
    /// retries with a growing backoff: enough to ride out a reconnect, few
    /// enough to give up (rather than loop) if we've genuinely lost access and
    /// the host keeps refusing.
    async fn remap_site_route(self: &Arc<Self>, node: String, host_port: u16, local_port: u16) {
        let key = format!("{}:{}", pubkey_part(&node), host_port);
        if !self.site_remap_inflight.lock().insert(key.clone()) {
            return; // already healing this mapping
        }
        for attempt in 0..SITE_REMAP_ATTEMPTS {
            tokio::time::sleep(SITE_REMAP_BACKOFF.saturating_mul(attempt + 1)).await;
            // A manual remap (or a prior attempt) already restored it.
            if self.sites.route_for(&node, host_port).is_some() {
                break;
            }
            let listener = match self.bind_exact_local_port(local_port).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::debug!("site re-map bind :{local_port} failed: {e}");
                    continue;
                }
            };
            match self
                .establish_site_route(node.clone(), host_port, listener, local_port)
                .await
            {
                Ok(route_id) => {
                    if self.await_route_active(&route_id).await {
                        tracing::info!(
                            "site {}:{} re-mapped on :{} after reconnect",
                            short_id(&node),
                            host_port,
                            local_port
                        );
                        break;
                    }
                    // Host didn't accept in time — clear this attempt fully so
                    // the next one re-binds cleanly, then retry.
                    self.sites.stop_route(&route_id);
                    let mut st = self.state.lock();
                    if let Some(s) = st.session.as_mut() {
                        let _ = s.teardown(&route_id);
                    }
                }
                Err(e) => tracing::debug!("site re-map offer failed: {e}"),
            }
        }
        self.site_remap_inflight.lock().remove(&key);
    }

    /// Bind a loopback listener on *exactly* `port`, retrying briefly — a
    /// just-aborted accept loop may not have released the port yet. The re-map
    /// path needs the identical local port an open tab is already on, so unlike
    /// [`Self::bind_site_listener`] it never falls back to another number.
    async fn bind_exact_local_port(&self, port: u16) -> Result<tokio::net::TcpListener, String> {
        use std::net::{Ipv4Addr, SocketAddr};
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let mut last = String::new();
        for _ in 0..20 {
            match tokio::net::TcpListener::bind(addr).await {
                Ok(l) => return Ok(l),
                Err(e) => {
                    last = e.to_string();
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
        Err(format!("local port :{port} still busy after 2s: {last}"))
    }

    /// NACK a site frame that arrived on a route we don't hold live — the
    /// symmetric twin of the KVM bridge's `nackDeadRoute`: tell the sender its
    /// route is gone so it re-offers (which [`Self::remap_site_route`] does),
    /// instead of tunnelling into the void. Rate-limited per route so a client
    /// draining a full pipe onto a dead route produces one Reject, not a flood.
    fn nack_dead_site_route(self: &Arc<Self>, from: &str, route: &str) {
        {
            let now = std::time::Instant::now();
            let mut at = self.site_nack_at.lock();
            if let Some(t) = at.get(route) {
                if now.duration_since(*t) < SITE_NACK_COOLDOWN {
                    return;
                }
            }
            // Bound the map across many short-lived route ids.
            if at.len() > 128 {
                at.retain(|_, t| now.duration_since(*t) < SITE_NACK_COOLDOWN * 4);
            }
            at.insert(route.to_string(), now);
        }
        let mesh = self.clone();
        let (from, route) = (from.to_string(), route.to_string());
        crate::spawn(async move {
            let _ = mesh
                .send_control(
                    &from,
                    &ControlMessage::Route(RouteControl::Reject {
                        route_id: route,
                        incarnation: None,
                        reason: "route not live on this device — re-offer to reconnect".into(),
                    }),
                )
                .await;
        });
    }

    /// Unmap a site: tear the route down (closing the listener + every
    /// connection via `StopMedia`) and tell the host.
    pub async fn site_unmap(self: &Arc<Self>, node: String, port: u16) -> Result<(), String> {
        let Some(route_id) = self.sites.route_for(&node, port) else {
            return Ok(()); // nothing mapped — idempotent
        };
        self.disconnect(route_id).await
    }

    /// Bind a local TCP listener for a site, preferring the host's port
    /// number ("direct"), falling back to a remapped high port, and finally
    /// to an OS-assigned one — so a mapping always lands somewhere.
    async fn bind_site_listener(
        &self,
        host_port: u16,
    ) -> Result<(tokio::net::TcpListener, u16), String> {
        use std::net::{Ipv4Addr, SocketAddr};
        let taken = self.sites.taken_local_ports();
        let preferred = allmystuff_bridge::sites::allocate_local_port(host_port, &taken);
        // Bind loopback only — a mapped site is for *this* machine's clients,
        // never re-exposed onto this machine's LAN.
        for candidate in [preferred, 0] {
            let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, candidate));
            if let Ok(listener) = tokio::net::TcpListener::bind(addr).await {
                let port = listener
                    .local_addr()
                    .map(|a| a.port())
                    .map_err(|e| e.to_string())?;
                return Ok((listener, port));
            }
        }
        Err(format!(
            "couldn't bind a local port for the site on :{host_port}"
        ))
    }

    /// Client side: accept local connections on `listener` and tunnel each
    /// over `route_id`. One mesh route multiplexes every connection by a
    /// client-minted `conn` id.
    fn spawn_site_accept(
        self: &Arc<Self>,
        route_id: String,
        peer: String,
        host_port: u16,
        listener: tokio::net::TcpListener,
    ) -> tokio::task::JoinHandle<()> {
        let mesh = self.clone();
        crate::spawn(async move {
            // Wait for the host to accept before taking connections — until
            // the route is active a tunnel's `Open` would be dropped, leaving
            // a connecting client hung. (Pending TCP connections sit in the
            // OS backlog meanwhile.) If the host rejects or never answers, we
            // give up and the listener closes with this task.
            if !mesh.await_route_active(&route_id).await {
                tracing::warn!("site route {route_id} never went active — not accepting");
                return;
            }
            let mut next_conn: u64 = 0;
            loop {
                let (socket, _addr) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::debug!("site listener {route_id} stopped: {e}");
                        return;
                    }
                };
                next_conn += 1;
                let conn = next_conn;
                // Register the channel before wiring, then tunnel (the
                // client sends `Open` so the host dials loopback). Over the
                // per-route cap → refuse this one cleanly.
                match mesh.sites.open_conn(&route_id, conn) {
                    Some(rx) => mesh.wire_conn(&route_id, &peer, conn, socket, rx, Some(host_port)),
                    None => {
                        mesh.send_site_event(&peer, &route_id, SiteEvent::Close { conn })
                            .await;
                        tracing::warn!(
                            "site route {route_id} at connection cap — refused conn {conn}"
                        );
                    }
                }
            }
        })
    }

    /// Poll until a route is active (it just went through offer→accept), or
    /// give up after ~5s — the client's accept loop gate, so it never opens a
    /// tunnel the host isn't ready for (and bails cleanly if the host
    /// rejected the offer). Returns whether it became active.
    async fn await_route_active(&self, route_id: &str) -> bool {
        for _ in 0..100 {
            let active = {
                let st = self.state.lock();
                st.session
                    .as_ref()
                    .and_then(|s| s.route(route_id))
                    .map(|r| r.is_active())
            };
            match active {
                Some(true) => return true,
                None => return false, // route gone (torn down / never made)
                Some(false) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
            }
        }
        false
    }

    /// Wire one tunneled connection whose inbound channel is already
    /// registered (via `open_conn`, so `rx` is its receiver): split the
    /// socket, spawn the inbound writer and the socket→mesh reader, and
    /// attach the reader. Read and write run as independent tasks — full
    /// duplex — so a WebSocket-upgraded (or otherwise long-lived,
    /// bidirectional) connection flows both ways for its whole life. When
    /// `open_port` is set (the client side), a `SiteEvent::Open` goes first
    /// so the host dials loopback. Shared by both ends.
    fn wire_conn(
        self: &Arc<Self>,
        route_id: &str,
        peer: &str,
        conn: u64,
        socket: tokio::net::TcpStream,
        rx: mpsc::Receiver<Vec<u8>>,
        open_port: Option<u16>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut read_half, mut write_half) = socket.into_split();

        // Inbound bytes (from the peer) → this connection's socket. Detached:
        // it ends when `tx` is dropped (close_conn / teardown), then shuts
        // the write half so the local client sees a clean close. It drains
        // any bytes that were buffered before the socket was wired.
        let mut rx = rx;
        crate::spawn(async move {
            while let Some(bytes) = rx.recv().await {
                if write_half.write_all(&bytes).await.is_err() {
                    break;
                }
            }
            let _ = write_half.shutdown().await;
        });

        // Socket bytes → the peer, as `SiteEvent::Data` frames (backpressured
        // by the mesh send — a slow link parks this read, never drops bytes).
        // On EOF a `Close`, then close_conn (dropping `tx` stops the writer).
        let mesh = self.clone();
        let rid = route_id.to_string();
        let peer_s = peer.to_string();
        let reader = crate::spawn(async move {
            if let Some(port) = open_port {
                mesh.send_site_event(&peer_s, &rid, SiteEvent::Open { conn, port })
                    .await;
            }
            let mut buf = vec![0u8; SITE_CHUNK_BYTES];
            loop {
                match read_half.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        mesh.send_site_event(
                            &peer_s,
                            &rid,
                            SiteEvent::Data {
                                conn,
                                data: buf[..n].to_vec(),
                            },
                        )
                        .await;
                    }
                }
            }
            mesh.send_site_event(&peer_s, &rid, SiteEvent::Close { conn })
                .await;
            mesh.sites.close_conn(&rid, conn);
        });

        self.sites.attach_reader(route_id, conn, reader);
    }

    /// Send one `SiteEvent` to `peer` on the media channel, fire-and-forget
    /// (a send failure is logged; the route's teardown handles the rest).
    async fn send_site_event(self: &Arc<Self>, peer: &str, route_id: &str, event: SiteEvent) {
        let seq = self.site_seq.fetch_add(1, Ordering::Relaxed);
        let frame = SiteFrame::new(route_id, seq, event);
        if let Ok(payload) = serde_json::to_value(&frame) {
            if let Err(e) = self.send_media_value(peer, payload).await {
                tracing::debug!("site frame to {} failed: {e}", short_id(peer));
            }
        }
    }

    /// One inbound site frame. Which side we are comes from the route: a
    /// frame for a route that *sources* here lands on the host (it dials
    /// loopback); one that *sinks* here lands on the client (it writes to a
    /// local socket). Either way the route must be live, a site route, and
    /// from this exact peer; the host additionally re-checks the sender is an
    /// authorized controller and the requested port is one *it* advertises.
    fn handle_site_frame(self: &Arc<Self>, from: &str, frame: SiteFrame) {
        let Some(me) = self.local_node_id() else {
            return;
        };
        let placement = {
            let st = self.state.lock();
            match st.session.as_ref().and_then(|s| s.route(&frame.route)) {
                Some(r)
                    if r.is_active()
                        && is_site_route(&r.route)
                        && pubkey_part(r.peer.as_str()) == pubkey_part(from) =>
                {
                    Some((
                        node_of(r.route.from.as_str()) == me,
                        node_of(r.route.to.as_str()) == me,
                    ))
                }
                _ => None,
            }
        };
        let Some((hosts_here, views_here)) = placement else {
            tracing::debug!(
                "site frame for {} refused (route not live here)",
                frame.route
            );
            // Tell the sender its route is gone so it re-offers, instead of
            // tunnelling into the void — the symmetric twin of the KVM bridge's
            // nackDeadRoute. Rate-limited per route; sent off the state lock.
            self.nack_dead_site_route(from, &frame.route);
            return;
        };

        if hosts_here {
            // The proxy *into* this machine — as privileged as the terminal,
            // so the same owner/fleet gate, re-cleared per frame.
            if !self.sender_may_drive(from, DrivePlane::Sites) {
                tracing::warn!(
                    "dropped site frame from {}: not an authorized controller",
                    short_id(from)
                );
                return;
            }
            match frame.event {
                SiteEvent::Open { conn, port } => {
                    // The load-bearing control: dial only a port *we* expose,
                    // never the client's free choice. Over the per-route cap,
                    // or unexposed → refuse with a `Close`.
                    let rx = if self.sites.is_port_exposed(port) {
                        self.sites.open_conn(&frame.route, conn)
                    } else {
                        tracing::warn!(
                            "site open from {} for :{port} refused — not an exposed service",
                            short_id(from)
                        );
                        None
                    };
                    match rx {
                        Some(rx) => self.spawn_site_host_connect(
                            frame.route.clone(),
                            from.to_string(),
                            conn,
                            port,
                            rx,
                        ),
                        None => {
                            let mesh = self.clone();
                            let (route, peer) = (frame.route.clone(), from.to_string());
                            crate::spawn(async move {
                                mesh.send_site_event(&peer, &route, SiteEvent::Close { conn })
                                    .await;
                            });
                        }
                    }
                }
                SiteEvent::Data { conn, data } => self.feed_site_conn(&frame.route, conn, data),
                SiteEvent::Close { conn } => self.sites.close_conn(&frame.route, conn),
                // A site event a newer client introduced — ignore it.
                SiteEvent::Unknown => {}
            }
        } else if views_here {
            // The client end — the host's bytes for one of our mapped
            // connections. We never receive `Open` here (we mint those).
            match frame.event {
                SiteEvent::Data { conn, data } => self.feed_site_conn(&frame.route, conn, data),
                SiteEvent::Close { conn } => self.sites.close_conn(&frame.route, conn),
                SiteEvent::Open { conn, .. } => {
                    tracing::debug!("ignoring unexpected site Open {conn} on the client side");
                }
                // A site event a newer host introduced — ignore it.
                SiteEvent::Unknown => {}
            }
        }
    }

    /// Deliver inbound bytes to a connection's local socket. Non-blocking:
    /// if the socket is too backed up to take more (its queue is full), the
    /// connection is *reset* rather than dropping bytes or growing unbounded
    /// — a TCP client just reconnects.
    fn feed_site_conn(self: &Arc<Self>, route_id: &str, conn: u64, data: Vec<u8>) {
        let Some(tx) = self.sites.conn_tx(route_id, conn) else {
            return; // unknown/closed connection
        };
        if tx.try_send(data).is_err() {
            self.sites.close_conn(route_id, conn);
        }
    }

    /// Host side: a validated `Open` whose channel is already registered
    /// (`rx` is its receiver). Connect to the local service and wire the
    /// tunnel; inbound `Data` that arrived during the connect sits buffered
    /// in `rx` and is drained once the writer starts. A failed connect closes
    /// the connection back to the client (and drops its registration).
    fn spawn_site_host_connect(
        self: &Arc<Self>,
        route_id: String,
        peer: String,
        conn: u64,
        port: u16,
        rx: mpsc::Receiver<Vec<u8>>,
    ) {
        use std::net::{Ipv4Addr, SocketAddr};
        let mesh = self.clone();
        crate::spawn(async move {
            let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
            match tokio::net::TcpStream::connect(addr).await {
                Ok(socket) => {
                    // The host doesn't send Open (the client already did).
                    mesh.wire_conn(&route_id, &peer, conn, socket, rx, None);
                }
                Err(e) => {
                    tracing::warn!("site connect to 127.0.0.1:{port} failed: {e}");
                    mesh.sites.close_conn(&route_id, conn);
                    mesh.send_site_event(&peer, &route_id, SiteEvent::Close { conn })
                        .await;
                }
            }
        });
    }

    // ---- Shared Files (the call's "Shared Files" area) ------------------

    /// Offer files into a room's Shared Files area. Each readable path gets
    /// an opaque fetch token, registered with the set of members allowed to
    /// pull it (`members`, canonical node ids). Returns one
    /// [`SharedFileMeta`] per file that could be read — the GUI hands these
    /// to the room's host, which restates them in the room's list. The
    /// bytes stay here; only the token + name + size travel.
    pub fn room_share_files(
        &self,
        members: Vec<String>,
        paths: Vec<String>,
    ) -> Vec<SharedFileMeta> {
        let allowed: std::collections::HashSet<String> =
            members.iter().map(|m| pubkey_part(m).to_string()).collect();
        let mut out = Vec::new();
        let mut reg = self.shared.lock();
        for path in paths {
            let p = std::path::PathBuf::from(&path);
            let Ok(meta) = std::fs::metadata(&p) else {
                tracing::warn!("can't share {path}: not readable");
                continue;
            };
            if meta.is_dir() {
                tracing::warn!("can't share {path}: it's a folder");
                continue;
            }
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file".to_string());
            let token = fresh_share_token();
            reg.insert(
                token.clone(),
                SharedReg {
                    path: p,
                    allowed: allowed.clone(),
                },
            );
            out.push(SharedFileMeta {
                token,
                name,
                size: meta.len(),
            });
        }
        out
    }

    /// Refresh the members allowed to fetch a set of shared tokens — the
    /// room's roster changed (a join, an admit, a removal) while these
    /// files were on offer. Unknown tokens are skipped.
    pub fn room_set_share_peers(&self, tokens: Vec<String>, members: Vec<String>) {
        let allowed: std::collections::HashSet<String> =
            members.iter().map(|m| pubkey_part(m).to_string()).collect();
        let mut reg = self.shared.lock();
        for t in tokens {
            if let Some(s) = reg.get_mut(&t) {
                s.allowed = allowed.clone();
            }
        }
    }

    /// Stop offering files (the uploader removed them, or left the room).
    pub fn room_unshare(&self, tokens: Vec<String>) {
        let mut reg = self.shared.lock();
        for t in tokens {
            reg.remove(&t);
        }
    }

    /// Resolve a fetch token to its on-disk path, but only for a peer it
    /// was shared with — the Shared Files gate. `None` when the token is
    /// unknown or `from` isn't on its allow-list.
    fn shared_path_for(&self, token: &str, from: &str) -> Option<String> {
        let reg = self.shared.lock();
        let s = reg.get(token)?;
        if !s.allowed.contains(pubkey_part(from)) {
            return None;
        }
        Some(s.path.to_string_lossy().into_owned())
    }

    /// A files window claims an active route's buffered responses (returns
    /// the token scoping its unwatch). Pure plumbing to [`FilesPlane`].
    pub fn file_watch(&self, route_id: &str) -> u64 {
        self.files.watch(route_id)
    }

    pub fn file_unwatch(&self, route_id: &str, token: u64) {
        self.files.unwatch(route_id, token);
    }

    /// Drain buffered file responses (`[u32 le len][frame json]…`), emptied
    /// by the window on each `allmystuff://file-ready` poke or safety poll.
    pub fn file_poll(&self, route_id: &str) -> Vec<u8> {
        self.files.poll(route_id)
    }

    /// Register a download sink: the `Chunk`s of `(route_id, req)` stream
    /// into this machine's Downloads folder under `name` (unique-ified)
    /// instead of the window's queue. Called *before* the Read request is
    /// sent, so the first chunk can't race the registration. Returns the
    /// destination path.
    pub fn file_download(&self, route_id: String, req: u64, name: &str) -> Result<String, String> {
        // The name comes from the remote listing — keep only its final
        // component so it can't steer the write outside Downloads.
        let base = std::path::Path::new(name)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty() && n != "." && n != "..")
            .unwrap_or_else(|| "download".to_string());
        let dir = dirs::download_dir()
            .or_else(dirs::home_dir)
            .ok_or("no Downloads folder here")?;
        let path = unique_path(&dir, &base);
        let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        self.downloads.lock().insert(
            (route_id, req),
            DownloadSink {
                file,
                path: path.clone(),
                written: 0,
                last_progress: std::time::Instant::now(),
            },
        );
        Ok(path.to_string_lossy().into_owned())
    }

    /// Stream one chunk into its registered download, if any. Returns
    /// `true` when the chunk was consumed here (don't queue it). Finishing
    /// (or failing) emits `allmystuff://file-saved` so the window can say
    /// where it landed.
    fn feed_download(&self, route_id: &str, req: u64, event: &FileEvent) -> bool {
        use std::io::Write as _;
        let FileEvent::Chunk {
            data, total, eof, ..
        } = event
        else {
            return false;
        };
        let key = (route_id.to_string(), req);
        let mut map = self.downloads.lock();
        let Some(sink) = map.get_mut(&key) else {
            return false;
        };
        if let Err(e) = sink.file.write_all(data) {
            let path = sink.path.clone();
            map.remove(&key);
            drop(map);
            let _ = std::fs::remove_file(&path);
            self.sink.emit(
                "allmystuff://file-saved",
                json!({ "route": route_id, "req": req, "path": null, "error": e.to_string() }),
            );
            return true;
        }
        sink.written += data.len() as u64;
        if *eof {
            let Some(sink) = map.remove(&key) else {
                return true;
            };
            drop(map);
            let _ = sink.file.sync_all();
            self.sink.emit(
                "allmystuff://file-saved",
                json!({
                    "route": route_id, "req": req,
                    "path": sink.path.to_string_lossy(), "error": null,
                }),
            );
        } else if sink.last_progress.elapsed() >= std::time::Duration::from_millis(250) {
            sink.last_progress = std::time::Instant::now();
            let written = sink.written;
            drop(map);
            self.sink.emit(
                "allmystuff://file-progress",
                json!({ "route": route_id, "req": req, "written": written, "total": total }),
            );
        }
        true
    }

    /// The host answered a registered download with an error: discard the
    /// partial file and tell the window.
    fn fail_download(&self, route_id: &str, req: u64, event: &FileEvent) {
        let FileEvent::Err { reason, .. } = event else {
            return;
        };
        let key = (route_id.to_string(), req);
        let Some(sink) = self.downloads.lock().remove(&key) else {
            return;
        };
        let _ = std::fs::remove_file(&sink.path);
        self.sink.emit(
            "allmystuff://file-saved",
            json!({ "route": route_id, "req": req, "path": null, "error": reason }),
        );
    }

    /// Discard every download sink a route had (it ended) — partial files
    /// are deleted, never left half-written in Downloads.
    fn drop_downloads(&self, route_id: &str) {
        let mut map = self.downloads.lock();
        let keys: Vec<_> = map
            .keys()
            .filter(|(rid, _)| rid == route_id)
            .cloned()
            .collect();
        for key in keys {
            if let Some(sink) = map.remove(&key) {
                let _ = std::fs::remove_file(&sink.path);
            }
        }
    }

    /// Whether an inbound media frame is acceptable: its route is one this
    /// session knows, is live, carries `media`, sinks on this machine, and
    /// the daemon-authenticated sender is the route's peer.
    fn inbound_media_ok(&self, route_id: &str, sender: &str, media: MediaKind) -> bool {
        let Some(me) = self.local_node_id() else {
            return false;
        };
        let st = self.state.lock();
        let Some(r) = st.session.as_ref().and_then(|s| s.route(route_id)) else {
            return false;
        };
        r.is_active()
            && r.route.media == media
            && node_of(r.route.to.as_str()) == me
            && pubkey_part(r.peer.as_str()) == pubkey_part(sender)
    }

    /// A media sample must arrive on the network that established this exact
    /// route lifetime. Peer and lane are insufficient because one peer can
    /// have an independent lane 0 in each network's PeerSession.
    fn inbound_route_network_ok(&self, route_id: &str, sender: &str, network: &str) -> bool {
        let state = self.state.lock();
        let Some(route) = state
            .session
            .as_ref()
            .and_then(|session| session.route(route_id))
            .filter(|route| pubkey_part(route.peer.as_str()) == pubkey_part(sender))
        else {
            return false;
        };
        let key = (route_id.to_string(), route.incarnation.clone());
        match state.route_networks.get(&key) {
            Some(pin) => {
                pin.confirmed
                    && pin.network == network
                    && state.network_epochs.get(network).copied() == Some(pin.network_epoch)
            }
            // Legacy/binary-v1 compatibility is unambiguous only when this
            // daemon session has exactly one joined network.
            None => {
                route.incarnation.is_none()
                    && state.networks.len() == 1
                    && state.networks[0] == network
            }
        }
    }

    /// Input is destructive state, not a replaceable media sample. In addition
    /// to the ordinary active-route/peer gate, require the event to name the
    /// exact negotiated lifetime. A delayed key-down from predecessor A then
    /// cannot be injected under same-id successor B. Legacy peers remain
    /// compatible because both the route and its events carry `None`.
    fn inbound_media_ok_incarnation(
        &self,
        route_id: &str,
        sender: &str,
        media: MediaKind,
        incarnation: Option<&str>,
    ) -> bool {
        let Some(me) = self.local_node_id() else {
            return false;
        };
        let state = self.state.lock();
        let Some(route) = state
            .session
            .as_ref()
            .and_then(|session| session.route(route_id))
        else {
            return false;
        };
        route.is_active()
            && route.route.media == media
            && node_of(route.route.to.as_str()) == me
            && pubkey_part(route.peer.as_str()) == pubkey_part(sender)
            && route.incarnation.as_deref() == incarnation
    }

    /// Classify inbound screen/camera media without conflating the normal
    /// Offer→Accept gap with an orphan route. A destructive NACK is correct for
    /// a dead/foreign route, but not for an authenticated same-id re-offer whose
    /// media beat arrived a few milliseconds before its Accept.
    fn inbound_video_disposition(&self, route_id: &str, sender: &str) -> InboundVideoDisposition {
        let Some(me) = self.local_node_id() else {
            return InboundVideoDisposition::Reject;
        };
        let st = self.state.lock();
        let route = st.session.as_ref().and_then(|s| s.route(route_id));
        inbound_video_disposition_from_facts(
            route.map(|r| &r.state),
            route.is_some_and(|r| matches!(r.route.media, MediaKind::Display | MediaKind::Video)),
            route.is_some_and(|r| node_of(r.route.to.as_str()) == me),
            route.is_some_and(|r| pubkey_part(r.peer.as_str()) == pubkey_part(sender)),
        )
    }

    /// New peers bind every MJPEG chunk to the route incarnation. Legacy
    /// senders did not carry this field, so an absent value remains acceptable
    /// only when the authenticated sender did not advertise the binding
    /// feature. A present value is always checked, regardless of feature tags.
    fn inbound_video_incarnation_ok(
        &self,
        route_id: &str,
        sender: &str,
        incarnation: Option<&str>,
    ) -> bool {
        let require_exact = incarnation.is_some() || self.peer_supports_media_incarnation(sender);
        if !require_exact {
            return true;
        }
        self.state
            .lock()
            .session
            .as_ref()
            .and_then(|session| session.route(route_id))
            .is_some_and(|route| {
                pubkey_part(route.peer.as_str()) == pubkey_part(sender)
                    && route.incarnation.as_deref() == incarnation
            })
    }

    /// [`Self::inbound_media_ok`] for the frame kinds two media share:
    /// video frames (and their `vstat` reports) belong to a display route
    /// *or* a camera one — same pipeline, different lens.
    fn inbound_video_ok(&self, route_id: &str, sender: &str) -> bool {
        self.inbound_video_disposition(route_id, sender) == InboundVideoDisposition::Accept
    }

    /// Why an inbound video frame was refused, in one diagnosable line —
    /// which [`Self::inbound_media_ok`] condition failed, with the facts.
    fn route_diag(&self, route_id: &str, sender: &str) -> String {
        let me = self.local_node_id().unwrap_or_default();
        let st = self.state.lock();
        match st.session.as_ref().and_then(|s| s.route(route_id)) {
            None => "this session doesn't know the route".to_string(),
            Some(r) => format!(
                "route state {:?} · media {:?} · sinks here: {} · sender is its peer: {}",
                r.state,
                r.route.media,
                node_of(r.route.to.as_str()) == me,
                pubkey_part(r.peer.as_str()) == pubkey_part(sender),
            ),
        }
    }

    /// Rate limit for the inbound-video diagnostics: true at most once per
    /// [`WARN_EVERY`] per `key`, so a dead stream explains itself in the
    /// log without arriving at frame rate.
    fn diag_ok(&self, key: &str) -> bool {
        let mut map = self.video_diag_last.lock();
        let now = std::time::Instant::now();
        match map.get(key) {
            Some(t) if now.duration_since(*t) < WARN_EVERY => false,
            _ => {
                map.insert(key.to_string(), now);
                true
            }
        }
    }

    /// Whether `sender` may drive this device's privileged planes (terminal,
    /// files, input, sites, console). Trust comes from **authenticated**
    /// sources only: the recorded owner, or membership in the fleet's
    /// closed-network **signed roster** (cached in [`Mesh::fleet_authorized`]
    /// from the daemon). Nobody else — not even a peer a route auto-accepted
    /// for.
    ///
    /// No gossiped roster is consulted — the fleet has none any more. The old
    /// `CHANNEL_OWNED` `OwnedRoster` gossip was exactly the conscription vector
    /// this closes (AMS-01); membership is now the signed roster a peer can
    /// only enter via the owner's governance. Fails closed — an empty or stale
    /// cache denies control rather than guessing.
    ///
    /// This is the **owner/fleet** trust only. A person-to-person *share* is the
    /// other authorized path (the owner deliberately granting one plane to
    /// someone outside their fleet); it's honoured per-plane in
    /// [`Self::sender_may_drive`], never here, so a screen-share grant can't
    /// leak into the planes it didn't name.
    fn sender_may_control(&self, sender: &str) -> bool {
        let canon = pubkey_part(sender);
        // You always control your own machine. A loopback terminal/console to
        // the box you're sitting at must pass even when it's unclaimed (no
        // owner) and in no fleet — otherwise opening a terminal to *this*
        // machine on a fresh install is refused, because the owner/fleet roster
        // is empty. `sender` is the authenticated mesh identity, so only a
        // genuine self-route (this node's own id) can match here.
        if let Some(me) = self.local_node_id() {
            if pubkey_part(&me) == canon {
                return true;
            }
        }
        if self.ownership.owner().as_deref().map(pubkey_part) == Some(canon) {
            return true;
        }
        // The owner's own admit records are as authenticated as the signed
        // roster: this device wrote them itself when it admitted (or claimed)
        // the member — local state, never gossip, and already what
        // `in_my_fleet` trusts when deciding evictions. Consulting them here
        // keeps a member controlling its owner's machine working across the
        // window where the daemon's converged roster is still healing (or
        // briefly lost the member to a stale tombstone) — the gap that
        // surfaced as "video streams but keyboard/mouse are refused".
        if self.ownership.any_fleet_member(|d| pubkey_part(d) == canon) {
            return true;
        }
        self.fleet_authorized.lock().contains(canon)
    }

    /// Whether `sender` may drive one privileged `plane` on this machine, at
    /// **admission grade** — evaluating a CEC technician's *live* consent grant.
    /// Used where a route is first authorized (the offer gate) and by the consent
    /// sweep; the per-frame input path uses [`Self::sender_may_drive_admitted`]
    /// instead. See [`Self::may_drive`] for the full owner/fleet/share/CEC rules.
    fn sender_may_drive(&self, sender: &str, plane: DrivePlane) -> bool {
        self.may_drive(sender, plane, true)
    }

    /// The per-frame twin of [`Self::sender_may_drive`]: identical owner/fleet
    /// and person-share checks, but it does **not** re-evaluate a CEC consent
    /// grant. A CEC route is authorized once at admission (the offer gate calls
    /// [`Self::sender_may_drive`]) and torn down within a couple of seconds of
    /// its grant lapsing by [`Self::spawn_cec_consent_sweep`] — so a *live* CEC
    /// route from a known technician is authorized by construction, and the
    /// input hot path (tens of frames a second) must not pay the grant + expiry
    /// evaluation on every one. Owner/fleet and share revocations are *not*
    /// swept, so those stay evaluated here per frame, unchanged.
    fn sender_may_drive_admitted(&self, sender: &str, plane: DrivePlane) -> bool {
        self.may_drive(sender, plane, false)
    }

    /// Whether `sender` may drive one privileged `plane` on this machine: the
    /// owner/fleet trust of [`Self::sender_may_control`], **or** an explicit
    /// person-to-person *share grant* this machine extended that names exactly
    /// that plane. Honouring the grant is what makes a share actually work — the
    /// route authorization already lets a granted route activate, so without
    /// this the console's terminal/files/control/clipboard frames would reach an
    /// active route and then be dropped here ("appears to work but doesn't pass
    /// through"). A grant authorizes only its own plane — a control grant never
    /// opens a shell, a files grant never injects — and the owner/fleet check
    /// runs first, so this only ever *widens* access to exactly who the owner
    /// chose, never narrows the existing owner/fleet path. Config writes
    /// (`SetExposed`) and the `Upgrade` command deliberately stay
    /// owner/fleet-only and keep calling [`Self::sender_may_control`] directly.
    ///
    /// `eval_cec_grant` selects the CEC arm: `true` evaluates the customer's
    /// live consent grant (admission / sweep); `false` trusts an already-admitted
    /// route from a known technician (the per-frame path — see the two wrappers
    /// above). Every drive plane (input, terminal, files, sites, clipboard) maps
    /// to the `Control` capability; screen *viewing* is gated separately at the
    /// Display offer and by the sweep.
    fn may_drive(&self, sender: &str, plane: DrivePlane, eval_cec_grant: bool) -> bool {
        if self.sender_may_control(sender) {
            return true;
        }
        // CEC Support: a dialed technician holds no fleet membership, so the
        // owner/fleet check above fails for them. Their authority is the
        // customer's consent grant — evaluated live at admission and by the
        // ~2s sweep, or (per frame) trusted via the admitted route: a still-live
        // route from a `knows_technician` peer was admitted under a valid grant
        // and has not been swept, so it need not re-hit the store per frame.
        // It only ever *widens* access (the owner/fleet path already said no).
        let cec_ok = if eval_cec_grant {
            self.cec
                .is_allowed(sender, allmystuff_cec_consent::Capability::Control)
        } else {
            self.cec.knows_technician(sender)
        };
        if cec_ok {
            return true;
        }
        let Some(person) = self.shares.person_for_node(pubkey_part(sender)) else {
            return false;
        };
        self.shares
            .out_grants_for(&person.id)
            .iter()
            .any(|g| grant_authorizes_plane(g, plane))
    }

    /// Whether a **screen-viewing** (`Display`/`Video`) offer from `sender` is
    /// authorized under CEC. Refused only when this node is on the customer
    /// side (never dialed anyone) *and* `sender` is a CEC technician it knows
    /// but hasn't granted screen view — the one case a screen offer must be
    /// blocked. Everything else (an ordinary AllMyStuff screen share, a
    /// technician's own node) falls through to the normal path. This is the
    /// screen twin of the per-frame `Control` gate above: a revoke closes it
    /// the next time an offer (or re-offer) is screened. Customer-ness is
    /// role-derived now — with standing area membership there is no hosting
    /// toggle to key on.
    fn cec_screen_offer_denied(&self, sender: &str) -> bool {
        !self.cec.is_technician()
            && self.cec.knows_technician(sender)
            && !self
                .cec
                .is_allowed(sender, allmystuff_cec_consent::Capability::ScreenView)
    }

    /// Whether `sender` may make THIS machine *source* (capture and stream) the
    /// media `route` asks for — its screen, camera, or microphone. This is the
    /// capture-side twin of [`Self::sender_may_drive`]: [`route_drive_plane`]
    /// only ever classified the drive planes (input/terminal/files/sites/
    /// clipboard), so a `Display`/`Video`/`Audio` *source* offer sailed past the
    /// offer gate and reached `start_media` with no authorization at all — the
    /// gap that let any authenticated peer pull this device's screen, webcam, or
    /// mic. Owner and fleet may source anything; a dialed CEC technician's live
    /// `ScreenView` consent covers the screen kinds (`Display`/`Video`; audio is
    /// not part of the CEC consent model); otherwise it takes an explicit
    /// person-to-person share this device extended that lets that person
    /// *consume* this media on this capability (the "Receive your screen"
    /// grant). Mirrors the owner/fleet → CEC → share layering of
    /// [`Self::may_drive`]; screen viewing stays gated here at the offer (and
    /// torn down by [`Self::spawn_cec_consent_sweep`]), never per frame.
    fn sender_may_source_media(&self, sender: &str, route: &Route) -> bool {
        if self.sender_may_control(sender) {
            return true;
        }
        // A dialed CEC technician holds no fleet membership; their authority to
        // view the screen is the customer's live ScreenView consent grant.
        if matches!(route.media, MediaKind::Display | MediaKind::Video)
            && self
                .cec
                .is_allowed(sender, allmystuff_cec_consent::Capability::ScreenView)
        {
            return true;
        }
        // An explicit share this device extended letting that person consume
        // exactly this media on this capability (honours capability pinning and
        // the media/role scope via the canonical `Grant::permits`).
        let Some(person) = self.shares.person_for_node(pubkey_part(sender)) else {
            return false;
        };
        self.shares.out_grants_for(&person.id).iter().any(|g| {
            g.permits(
                route.media,
                allmystuff_graph::GrantRole::Consume,
                &route.from,
            )
        })
    }

    /// Media keeps arriving for a route this side doesn't hold live — our
    /// app restarted (fresh session, old routes gone), or the route tore
    /// down here while the sender missed it. Tell the sender, rate-limited:
    /// its session marks the route rejected and **stops the encoder**
    /// (`Reject` on an active route now returns `StopMedia`), instead of
    /// capturing + encoding into the void indefinitely. An older sender
    /// ignores a Reject for an active route — exactly today's behaviour.
    fn nack_dead_route(self: &Arc<Self>, network: &str, from: &str, route_id: &str) {
        if !self.diag_ok(&format!("nack:{route_id}")) {
            return;
        }
        let mesh = self.clone();
        let network = network.to_string();
        let from = from.to_string();
        let route_id = route_id.to_string();
        crate::spawn(async move {
            if mesh.peer_supports_route_incarnation(&from) {
                let _ = mesh
                    .send_control_on_network(
                        &from,
                        &ControlMessage::Route(RouteControl::MissingRoute {
                            incarnation: mesh.route_incarnation(&route_id),
                            route_id,
                        }),
                        &network,
                    )
                    .await;
                return;
            }
            let _ = mesh
                .send_control_on_network(
                    &from,
                    &ControlMessage::Route(RouteControl::Reject {
                        route_id,
                        incarnation: None,
                        reason: "route not live on the receiving side — re-offer to reconnect"
                            .into(),
                    }),
                    &network,
                )
                .await;
        });
    }

    /// The lane-shaped twin of [`Self::nack_dead_route`], for the one case
    /// a Reject can't reach: media keeps arriving on a track lane no route
    /// here maps to. That's this app restarted (fresh session — same daemon,
    /// same boot id, so the peer-restart reap never fires) or an orphan
    /// stream shadowing a lane after its route was lost one-sided. We can't
    /// name the dead route — the name is exactly what we lost — but the
    /// sender's own pin still knows, so we report the *lane*
    /// ([`RouteControl::DeadLane`]) and the sender resolves it into a
    /// Reject of that route, stopping its encoder.
    ///
    /// Guarded twice: nothing is sent until the lane has stayed unmapped a
    /// full [`WARN_EVERY`] (a stream's first samples can legally outrun the
    /// Accept/VideoLane control messages at start — a NACK there would kill
    /// a healthy stream being born; [`Self::clear_dead_lane`] wipes the
    /// clock the moment the lane resolves), then rate-limited like every
    /// other diagnostic while the condition persists. An older sender
    /// doesn't know the message and drops it — it keeps streaming exactly
    /// as today.
    fn nack_dead_lane(self: &Arc<Self>, network: &str, from: &str, media: &'static str, lane: u8) {
        // A lane number alone cannot distinguish predecessor A from same-lane
        // successor B. Only peers that negotiated route incarnations can turn
        // this into the non-destructive exact-Accept challenge below. For a
        // legacy peer, keep the stream and diagnostic rather than risk killing
        // the wrong live route.
        if !self.peer_supports_route_incarnation(from) {
            if self.diag_ok(&format!(
                "deadlane-legacy:{network}:{media}:{}:{lane}",
                pubkey_part(from)
            )) {
                tracing::warn!(
                    peer = %short_id(from),
                    media,
                    lane,
                    "unmapped legacy media lane cannot be reconciled safely without route incarnation"
                );
            }
            return;
        }
        let key = format!("deadlane:{network}:{media}:{}:{lane}", pubkey_part(from));
        {
            let mut since = self.dead_lane_since.lock();
            let now = std::time::Instant::now();
            let first = *since.entry(key.clone()).or_insert(now);
            if now.duration_since(first) < WARN_EVERY {
                return;
            }
        }
        if !self.diag_ok(&key) {
            return;
        }
        tracing::warn!(
            "asking {} to stop its unmapped {media} stream on lane {lane} (no route here maps to it)",
            short_id(from)
        );
        let mesh = self.clone();
        let network = network.to_string();
        let from = from.to_string();
        crate::spawn(async move {
            let _ = mesh
                .send_control_on_network(
                    &from,
                    &ControlMessage::Route(RouteControl::DeadLane {
                        media: media.into(),
                        lane,
                    }),
                    &network,
                )
                .await;
        });
    }

    /// The lane resolved to a route again — forget its "unmapped since"
    /// mark so a later unmapped spell starts a fresh [`WARN_EVERY`] grace
    /// instead of inheriting an old clock and NACKing instantly.
    fn clear_dead_lane(&self, network: &str, from: &str, media: &str, lane: u8) {
        let key = format!("deadlane:{network}:{media}:{}:{lane}", pubkey_part(from));
        self.dead_lane_since.lock().remove(&key);
    }

    /// Answer a receiver's route/lane challenge with the exact active
    /// lifetime. A receiver that still owns it treats this as an idempotent
    /// Accept; an empty or terminal receiver returns an exact terminal
    /// response. That makes route-id-only and lane-only diagnostics safe under
    /// deterministic id/lane reuse.
    async fn handle_missing_route(
        self: &Arc<Self>,
        network: &str,
        from: &str,
        route_id: &str,
        incarnation: Option<&str>,
    ) {
        let (current_incarnation, pinned_network, outbound) = {
            let state = self.state.lock();
            let Some(route) = state
                .session
                .as_ref()
                .and_then(|session| session.route(route_id))
                .filter(|route| {
                    matches!(route.state, RouteState::Offered | RouteState::Active)
                        && pubkey_part(route.peer.as_str()) == pubkey_part(from)
                })
            else {
                return;
            };
            let key = (route_id.to_string(), route.incarnation.clone());
            (
                route.incarnation.clone(),
                state
                    .route_networks
                    .get(&key)
                    .filter(|pin| {
                        pin.confirmed
                            && state.network_epochs.get(&pin.network).copied()
                                == Some(pin.network_epoch)
                    })
                    .map(|pin| pin.network.clone()),
                route.origin == allmystuff_session::Origin::Outbound,
            )
        };

        if pinned_network.as_deref() == Some(network)
            && (!outbound || current_incarnation.is_none())
        {
            self.reannounce_route_challenge(from, route_id, Some(network))
                .await;
            return;
        }

        // A recovery request can replace user-owned outbound intent only when
        // it names the exact fenced lifetime. This applies on the same path as
        // well: the receiver is explicitly saying it no longer has that route,
        // so re-sending Accept would only elicit a terminal response. Legacy
        // MissingRoute has no incarnation and cannot safely churn a same-id
        // successor.
        if !outbound
            || current_incarnation.as_deref() != incarnation
            || current_incarnation.is_none()
        {
            tracing::warn!(
                route = %route_id,
                from = %short_id(from),
                network,
                incarnation = ?incarnation,
                disposition = "missing_route_reoffer_ignored",
                "missing-route recovery did not identify the current outbound lifetime"
            );
            return;
        }

        let lifecycle = self.lock_route_lifecycle(route_id).await;
        let desired_matches = self
            .desired_routes
            .lock()
            .get(route_id)
            .is_some_and(|desired| {
                pubkey_part(&desired.peer) == pubkey_part(from)
                    && desired.current_incarnation.as_deref() == incarnation
            });
        if !desired_matches {
            return;
        }
        let was_active = {
            let mut state = self.state.lock();
            let current = state
                .session
                .as_ref()
                .and_then(|session| session.route(route_id))
                .is_some_and(|route| {
                    matches!(route.state, RouteState::Offered | RouteState::Active)
                        && route.origin == allmystuff_session::Origin::Outbound
                        && pubkey_part(route.peer.as_str()) == pubkey_part(from)
                        && route.incarnation.as_deref() == incarnation
                });
            if !current {
                return;
            }
            let active = state
                .session
                .as_ref()
                .and_then(|session| session.route(route_id))
                .is_some_and(|route| route.is_active());
            if let Some(session) = state.session.as_mut() {
                let _ = session.teardown(route_id);
            }
            state
                .route_networks
                .remove(&(route_id.to_string(), current_incarnation.clone()));
            active
        };
        if was_active {
            self.apply_stop_media_locked(route_id.to_string(), current_incarnation);
        }
        drop(lifecycle);
        tracing::warn!(
            route = %route_id,
            from = %short_id(from),
            network,
            disposition = "fresh_incarnation_reoffer",
            "peer reported the exact route missing on a surviving data-plane network"
        );
        self.replay_desired_routes(Some(from)).await;
    }

    async fn reannounce_route_challenge(
        self: &Arc<Self>,
        from: &str,
        route_id: &str,
        network: Option<&str>,
    ) {
        let route_info = {
            let state = self.state.lock();
            state
                .session
                .as_ref()
                .and_then(|session| session.route(route_id))
                .filter(|route| {
                    route.state == RouteState::Active
                        && pubkey_part(route.peer.as_str()) == pubkey_part(from)
                })
                .filter(|route| {
                    let Some(network) = network else { return true };
                    let key = (route_id.to_string(), route.incarnation.clone());
                    match state.route_networks.get(&key) {
                        Some(pin) => {
                            pin.confirmed
                                && pin.network == network
                                && state.network_epochs.get(network).copied()
                                    == Some(pin.network_epoch)
                        }
                        None => {
                            route.incarnation.is_none()
                                && state.networks.len() == 1
                                && state.networks[0] == network
                        }
                    }
                })
                .map(|route| {
                    (
                        route.incarnation.clone(),
                        route.term_session.clone(),
                        route.route.media,
                    )
                })
        };
        let Some((incarnation, session, media)) = route_info else {
            return;
        };
        let response = ControlMessage::Route(RouteControl::Accept {
            route_id: route_id.to_string(),
            incarnation,
            session,
        });
        let _ = if let Some(network) = network {
            self.send_control_on_network(from, &response, network).await
        } else {
            self.send_control(from, &response).await
        };
        if matches!(media, MediaKind::Display | MediaKind::Video) {
            self.announce_video_lane(route_id, from).await;
        }
    }

    /// A receiver told us media we're sending it on track `lane` has no
    /// route on its side ([`RouteControl::DeadLane`]) — it can't name the
    /// route (its app restarted; the name is what it lost), but our own
    /// bookkeeping still can. Resolve the lane back to the route we're
    /// streaming *to that peer* — video by the lane pin
    /// ([`Self::assign_video_lane`]'s table), audio by the same positional
    /// sort the outbound forwarder picks lanes with — and fold it through
    /// the session as if the peer had rejected the route by name: the
    /// session re-checks the sender is the route's peer (a spoofed or stale
    /// lane can never kill someone else's stream) and `Reject` on an active
    /// outbound route returns `StopMedia`, which stops the capture that was
    /// encoding into the void. Resolving nothing is a quiet no-op — the
    /// stream already stopped, or an earlier NACK already landed.
    async fn handle_dead_lane(self: &Arc<Self>, network: &str, from: &str, media: &str, lane: u8) {
        if !self.peer_supports_route_incarnation(from) {
            tracing::warn!(
                peer = %short_id(from),
                media,
                lane,
                disposition = "legacy_dead_lane_ignored",
                "lane-only dead-route report cannot safely select a deterministic route lifetime"
            );
            return;
        }
        let canon = pubkey_part(from).to_string();
        let route_id = match media {
            "video" => {
                // The pin table is route→lane across all peers; two peers can
                // each hold this lane number, so match the lane and then the
                // peer (via the session, after dropping the pin lock).
                let candidates: Vec<String> = {
                    let pins = self.video_lane_pins.lock();
                    pins.iter()
                        .filter(|(_, pin)| pin.network == network && pin.lane == lane)
                        .map(|(r, _)| r.clone())
                        .collect()
                };
                candidates.into_iter().find(|rid| {
                    let peer_matches = {
                        let st = self.state.lock();
                        st.session
                            .as_ref()
                            .and_then(|s| s.route(rid))
                            .is_some_and(|r| pubkey_part(r.peer.as_str()) == canon)
                    };
                    peer_matches && self.inbound_route_network_ok(rid, from, network)
                })
            }
            "audio" => self
                .sorted_media_routes(from, true, "opus")
                .into_iter()
                .filter(|route_id| self.inbound_route_network_ok(route_id, from, network))
                .nth(lane as usize),
            // A media kind a newer build introduced — nothing of ours to
            // stop; ignore it exactly like an Unknown control message.
            _ => None,
        };
        let Some(route_id) = route_id else {
            tracing::debug!(
                "dead-lane nack from {} for {media} lane {lane} matched no route here",
                short_id(from)
            );
            return;
        };
        tracing::warn!(
            "receiver {} reports our {media} lane {lane} has no route; re-announcing {route_id}",
            short_id(from)
        );
        self.reannounce_route_challenge(from, &route_id, Some(network))
            .await;
    }

    /// An inbound input/clipboard frame failed a gate. Historically this was
    /// one rate-unlimited, cause-blind warn — which is exactly how "the mouse
    /// stopped working" became undiagnosable: the viewer's console looked
    /// connected (the route activates regardless) while every event died
    /// here. Now, rate-limited per route: log *which* gate failed with the
    /// route facts, surface it on this machine's own UI sink, and send a
    /// `RouteControl::Reject` back so the viewer's console flips its toggle
    /// off and shows the reason instead of typing into the void. (An old
    /// viewer ignores a Reject for an active route — no worse than today.)
    fn refuse_control_frame(
        self: &Arc<Self>,
        from: &str,
        route_id: &str,
        plane: &str,
        route_ok: bool,
    ) {
        // Do not release input state based on a refused frame. The sender may
        // be foreign or stale and route ids are guessable. Authoritative route
        // teardown/reset paths own cleanup and are generation-fenced.
        if !self.diag_ok(&format!("refuse:{plane}:{route_id}")) {
            return;
        }
        let reason = if route_ok {
            if self.cec.knows_technician(from) {
                // A CEC technician's authority is the customer's consent grant,
                // not the fleet roster — when their frames die here it's the
                // grant that lapsed (expired, revoked, or an "Approve Once"
                // lost to an app restart). Say that, or the technician goes
                // hunting through fleet settings that were never involved.
                format!(
                    "{plane} refused: the customer's approval no longer covers it \
                     (expired, revoked, or their app restarted) — reconnect so they \
                     can approve again"
                )
            } else {
                format!(
                    "{plane} refused: this machine doesn't recognize the controlling device as \
                     its owner or a fleet member (and no {plane} share covers it) — check the \
                     fleet roster / re-admit the device from Fleet settings"
                )
            }
        } else {
            format!(
                "{plane} refused: no live {plane} route for it here ({}) — reconnect the console",
                self.route_diag(route_id, from)
            )
        };
        tracing::warn!("dropped {plane} event from {}: {reason}", short_id(from));
        self.sink.emit(
            "allmystuff://control-refused",
            serde_json::json!({
                "route": route_id,
                "from": from,
                "plane": plane,
                "reason": reason,
            }),
        );
        let incarnation = self.route_incarnation(route_id);
        let mesh = self.clone();
        let from = from.to_string();
        let route_id = route_id.to_string();
        crate::spawn(async move {
            let _ = mesh
                .send_control(
                    &from,
                    &ControlMessage::Route(RouteControl::Reject {
                        route_id,
                        incarnation,
                        reason,
                    }),
                )
                .await;
        });
    }

    /// Recovery-triggered refreshes share the route-local time limiter below.
    /// There is deliberately no unacknowledged "outstanding" latch: Refresh
    /// is a best-effort message, so a lost ask must become eligible again on a
    /// later damaged AU. The message still uses the existing authenticated
    /// route-control channel on the ICE data path; this adds no signaling
    /// traffic and changes no wire shape.
    async fn request_refresh_for_recovery(
        self: &Arc<Self>,
        route_id: String,
    ) -> Result<(), String> {
        self.request_refresh(route_id).await
    }

    /// Ask the far end of an inbound display/camera route for a clean
    /// decode entry (IDR) *now* — the decoder here lost its place.
    /// Rate-limited per route: decode errors arrive at frame rate, the
    /// asks must not.
    /// Old peers don't know the message and drop it; recovery then waits
    /// for the periodic IDR exactly as before.
    pub async fn request_refresh(self: &Arc<Self>, route_id: String) -> Result<(), String> {
        let peer = self.route_peer(&route_id).ok_or("unknown route")?;
        let now = std::time::Instant::now();
        {
            let mut asks = self.refresh_asks.lock();
            // 300 ms floor: a re-key is the recovery from visible corruption, so
            // it must turn around fast (was 600 ms). Still throttled so a viewer
            // failing every frame can't trigger a keyframe storm — at most a few
            // re-keys/s while it's actually broken.
            if !reserve_video_refresh(&mut asks, &route_id, now) {
                return Ok(());
            }
        }
        let incarnation = self.route_incarnation(&route_id);
        tracing::debug!("asking {} to re-key {route_id}", short_id(&peer));
        let result = self
            .send_control(
                &peer,
                &ControlMessage::Route(RouteControl::Refresh {
                    route_id: route_id.clone(),
                    incarnation,
                }),
            )
            .await;
        if result.is_err() {
            let mut asks = self.refresh_asks.lock();
            if asks.get(&route_id) == Some(&now) {
                asks.remove(&route_id);
            }
        }
        result
    }

    /// Ask the far end of an inbound display/camera route to stream with
    /// these quality picks (`None` = that dial back on automatic). Old
    /// peers drop the message and stay on automatic.
    /// GUI-internal: the effective encode dials for a route THIS node is
    /// streaming — the "what we're actually doing" half of the console's
    /// quality panel (resolved posture, encoder rung, wire codec, the AIMD
    /// bitrate target + its ceiling, the fps + edge targets, and the actual
    /// output geometry). `None` when this node isn't the streamer for
    /// `route_id` (the ordinary remote-view case, where the viewer surfaces
    /// its own measured actuals). Read-only; touches no wire and no peer.
    pub fn route_dials(&self, route_id: &str) -> Option<crate::video::RouteDials> {
        let plan = self.media_policy.lock().plan(route_id).cloned();
        let mut dials = self.video.route_dials(route_id).or_else(|| {
            let plan = plan.as_ref()?;
            Some(crate::video::RouteDials {
                posture: plan.effective_mode.wire_name(),
                encoder_label: plan.encoder.clone(),
                codec: if plan.codec.eq_ignore_ascii_case("h.264") {
                    "H.264"
                } else if plan.codec.eq_ignore_ascii_case("mjpeg") {
                    "MJPEG"
                } else if plan.codec.eq_ignore_ascii_case("hevc") {
                    "HEVC"
                } else {
                    ""
                },
                target_bitrate_bps: plan.route_budget_bps.min(u64::from(u32::MAX)) as u32,
                ceiling_bps: plan.route_ceiling_bps.min(u64::from(u32::MAX)) as u32,
                fps_target: plan.fps,
                edge_cap: 0,
                out_w: 0,
                out_h: 0,
                peer_budget_bps: 0,
                route_budget_bps: 0,
                route_ceiling_bps: 0,
                priority: false,
                audio_packet_ms: 0,
                audio_jitter_ms: 0,
                audio_fec: false,
                video_queue_depth: 0,
                audio_queue_depth: 0,
                degradation_reasons: Vec::new(),
            })
        })?;
        if let Some(plan) = plan {
            dials.peer_budget_bps = plan.aggregate_budget_bps;
            dials.route_budget_bps = plan.route_budget_bps;
            dials.route_ceiling_bps = plan.route_ceiling_bps;
            dials.priority = plan.priority;
            dials.audio_packet_ms = plan.audio_packet_ms;
            dials.audio_jitter_ms = plan.audio_jitter_ms;
            dials.audio_fec = plan.audio_fec;
            dials.video_queue_depth = plan.video_queue_frames;
            dials.audio_queue_depth = plan.audio_queue_packets;
            dials.degradation_reasons = plan.degradation_reasons;
            if dials.encoder_label.is_empty() {
                dials.encoder_label = plan.encoder;
            }
        }
        Some(dials)
    }

    fn local_media_capabilities(&self) -> MediaCapabilities {
        #[cfg(all(windows, feature = "host"))]
        let native_h264_decode = {
            static AVAILABLE: std::sync::LazyLock<bool> =
                std::sync::LazyLock::new(|| crate::nvdec::NvdecH264::open().is_ok());
            *AVAILABLE
        };
        #[cfg(not(all(windows, feature = "host")))]
        let native_h264_decode = false;
        MediaCapabilities {
            policy_v1: true,
            h264: true,
            // HEVC encode/framing remains quarantined behind the fork's
            // experimental GPU-lane switch. A Windows build flag is not a
            // runtime capability, so production negotiation must not claim
            // Studio Lossless merely because this binary compiled on Windows.
            hevc: false,
            opus: true,
            native_h264_decode,
            native_hevc_decode: false,
            binary_media_pipes: self.daemon_media_pipes.load(Ordering::SeqCst),
            source_exact_444: false,
            lossless_audio: false,
        }
    }

    fn media_mode_for_peer(&self, peer: &str) -> MediaMode {
        let peer = pubkey_part(peer);
        let route_ids = {
            let st = self.state.lock();
            st.session
                .as_ref()
                .map(|session| {
                    session
                        .active_routes()
                        .filter(|route| {
                            pubkey_part(route.peer.as_str()) == peer
                                && matches!(
                                    route.route.media,
                                    MediaKind::Display | MediaKind::Video
                                )
                        })
                        .map(|route| route.route.id.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let policy = self.media_policy.lock();
        route_ids
            .iter()
            .filter_map(|route| policy.plan(route))
            .find(|plan| plan.priority)
            .or_else(|| route_ids.iter().find_map(|route| policy.plan(route)))
            .map(|plan| plan.effective_mode)
            .unwrap_or(MediaMode::Balanced)
    }

    fn apply_audio_profile_for_peer(&self, peer: &str, mode: MediaMode) {
        let profile = audio_profile_for_mode(mode);
        let Some(me) = self.local_node_id() else {
            return;
        };
        let me = pubkey_part(&me).to_string();
        let peer = pubkey_part(peer).to_string();
        let routes = {
            let st = self.state.lock();
            st.session
                .as_ref()
                .map(|session| {
                    session
                        .active_routes()
                        .filter(|route| {
                            route.route.media == MediaKind::Audio
                                && pubkey_part(route.peer.as_str()) == peer
                        })
                        .map(|route| {
                            let outbound =
                                pubkey_part(node_of(route.route.from.as_str()).as_str()) == me;
                            (route.route.id.clone(), outbound)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        for (route_id, outbound) in routes {
            if outbound {
                if let Some(encoder) = self.audio_encoders.lock().get(&route_id).cloned() {
                    match OpusStream::with_profile(profile) {
                        Ok(next) => {
                            *encoder.lock() = next;
                            tracing::debug!(
                                "audio policy {route_id}: {:?} packetization applied live",
                                mode
                            );
                        }
                        Err(error) => tracing::warn!(
                            "audio policy {route_id}: encoder reconfigure failed ({error}); keeping prior profile"
                        ),
                    }
                }
            } else {
                self.audio.set_playback_profile(&route_id, profile);
                if let Some(decoder) = self.audio_decoders.lock().get_mut(&route_id) {
                    decoder.set_profile(profile);
                }
            }
        }
    }

    async fn send_effective_plans(&self, plans: Vec<EffectivePlan>) {
        for plan in plans {
            let incarnation = self.route_incarnation(&plan.route_id);
            self.send_effective_plan_exact(plan, incarnation).await;
        }
    }

    async fn send_effective_plan_exact(
        &self,
        mut plan: EffectivePlan,
        incarnation: Option<String>,
    ) {
        if let Some(dials) = self.video.route_dials(&plan.route_id) {
            plan.encoder = dials.encoder_label;
            plan.codec = dials.codec.to_string();
        }
        let peer = self
            .state
            .lock()
            .session
            .as_ref()
            .and_then(|session| session.route(&plan.route_id))
            .filter(|route| route.state == RouteState::Active && route.incarnation == incarnation)
            .map(|route| route.peer.to_string());
        let Some(peer) = peer else { return };
        let ext = PolicyEnvelope::effective(plan.clone()).into_ext(Value::Null);
        if let Err(error) = self
            .send_control(
                &peer,
                &ControlMessage::Route(RouteControl::Tune {
                    route_id: plan.route_id,
                    incarnation,
                    max_edge: None,
                    bitrate: None,
                    fps: None,
                    game: false,
                    mode: None,
                    ext,
                }),
            )
            .await
        {
            tracing::debug!(
                "effective media plan to {} failed: {error}",
                short_id(&peer)
            );
        }
    }

    /// Coalesce effective-plan echoes and deliver them away from the inbound
    /// daemon event pump. A stalled local ChannelSendTo request can therefore
    /// no longer stop later input, terminal, or media events from being read.
    fn queue_effective_plans(self: &Arc<Self>, plans: Vec<EffectivePlan>) {
        if plans.is_empty() {
            return;
        }
        let epoch = self.effective_plan_echo_epoch.load(Ordering::SeqCst);
        // Read Session before taking the pending-map lock. The delivery worker
        // also consults Session, so keeping these lock domains separate avoids
        // an inversion with teardown/policy paths that already hold State.
        let plans = plans
            .into_iter()
            .map(|plan| {
                let incarnation = self.route_incarnation(&plan.route_id);
                (plan, incarnation)
            })
            .collect::<Vec<_>>();
        {
            let mut pending = self.effective_plan_echoes.lock();
            for (plan, incarnation) in plans {
                pending.insert((plan.route_id.clone(), incarnation), (epoch, plan));
            }
        }
        if self
            .effective_plan_echo_running
            .swap(true, Ordering::SeqCst)
        {
            return;
        }

        let mesh = self.clone();
        crate::spawn(async move {
            loop {
                let current_epoch = mesh.effective_plan_echo_epoch.load(Ordering::SeqCst);
                let drained = {
                    let mut pending = mesh.effective_plan_echoes.lock();
                    pending.drain().collect::<Vec<_>>()
                };
                let plans = drained
                    .into_iter()
                    .filter_map(|((route_id, incarnation), (epoch, plan))| {
                        (epoch == current_epoch
                            && mesh.route_is_active_incarnation(&route_id, incarnation.as_deref()))
                        .then_some((plan, incarnation))
                    })
                    .collect::<Vec<_>>();
                if !plans.is_empty() {
                    for (plan, incarnation) in plans {
                        mesh.send_effective_plan_exact(plan, incarnation).await;
                    }
                    continue;
                }

                mesh.effective_plan_echo_running
                    .store(false, Ordering::SeqCst);
                if mesh.effective_plan_echoes.lock().is_empty() {
                    break;
                }
                if mesh
                    .effective_plan_echo_running
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_err()
                {
                    break;
                }
            }
        });
    }

    pub async fn request_tune(
        self: &Arc<Self>,
        route_id: String,
        max_edge: Option<u32>,
        bitrate: Option<u32>,
        fps: Option<u32>,
        game: bool,
        mode: Option<String>,
    ) -> Result<(), String> {
        self.request_policy_tune(route_id, max_edge, bitrate, fps, game, mode, None, false)
            .await
    }

    async fn replay_requested_video_tune(self: &Arc<Self>, route_id: &str) {
        let Some(tune) = self.requested_video_tunes.lock().get(route_id).cloned() else {
            return;
        };
        if !self.route_is_active_incarnation(route_id, self.route_incarnation(route_id).as_deref())
        {
            return;
        }
        if let Err(error) = self
            .request_policy_tune(
                route_id.to_string(),
                tune.max_edge,
                tune.bitrate,
                tune.fps,
                tune.game,
                tune.mode,
                tune.peer_cap_bps,
                tune.priority,
            )
            .await
        {
            tracing::debug!(
                route = %route_id,
                error = %error,
                "deferred video tune did not reach the active peer"
            );
        }
    }

    /// Policy-aware sibling of [`Self::request_tune`]. The legacy public call
    /// shape remains intact for embedders; the desktop node-control adapter uses
    /// this form for aggregate-cap and focus requests.
    #[allow(clippy::too_many_arguments)]
    pub async fn request_policy_tune(
        self: &Arc<Self>,
        route_id: String,
        max_edge: Option<u32>,
        bitrate: Option<u32>,
        fps: Option<u32>,
        game: bool,
        mode: Option<String>,
        peer_cap_bps: Option<u64>,
        priority: bool,
    ) -> Result<(), String> {
        let priority_only = priority
            && max_edge.is_none()
            && bitrate.is_none()
            && fps.is_none()
            && !game
            && mode.is_none()
            && peer_cap_bps.is_none();
        let wire_tune = if priority_only {
            let mut requested = self.requested_video_tunes.lock();
            let tune = requested.entry(route_id.clone()).or_default();
            tune.priority = true;
            tune.clone()
        } else {
            let tune = LegacyVideoTune {
                max_edge,
                bitrate,
                fps,
                game,
                mode: mode.clone(),
                peer_cap_bps,
                priority,
            };
            self.requested_video_tunes
                .lock()
                .insert(route_id.clone(), tune.clone());
            tune
        };
        let Some(peer) = self.route_peer(&route_id) else {
            // Connect and Tune are separate local commands. Preserve the
            // user's requested posture when Tune wins that race; the route's
            // Accept/replay path sends it once the exact route exists.
            tracing::debug!(
                route = %route_id,
                "video tune recorded before route creation; deferring delivery"
            );
            return Ok(());
        };
        // The streaming side logs the retune it actually applies — one
        // line per pill change across the pair is plenty.
        tracing::debug!(
            "asking {} to tune {route_id}: edge {max_edge:?} · bitrate {bitrate:?} · fps {fps:?} · game {game} · mode {mode:?}",
            short_id(&peer)
        );
        let policy_mode = mode
            .as_deref()
            .and_then(MediaMode::parse)
            .unwrap_or(if game {
                MediaMode::Game
            } else {
                MediaMode::Balanced
            });
        let policy = PolicyRequest {
            mode: policy_mode,
            // `Some(0)` is an explicit aggregate reset. `None` stays absent
            // so a priority-only focus Tune cannot clear another window's cap.
            peer_cap_bps,
            route_cap_bps: bitrate.map(u64::from),
            priority,
            priority_only,
            source_exact_video: policy_mode == MediaMode::StudioLossless,
            lossless_audio: policy_mode == MediaMode::StudioLossless,
        };
        let ext =
            PolicyEnvelope::request(route_id.clone(), policy, self.local_media_capabilities())
                .into_ext(Value::Null);
        let incarnation = self.route_incarnation(&route_id);
        self.send_control(
            &peer,
            &ControlMessage::Route(RouteControl::Tune {
                route_id,
                incarnation,
                max_edge: wire_tune.max_edge,
                bitrate: wire_tune.bitrate,
                fps: wire_tune.fps,
                game: wire_tune.game,
                mode: wire_tune.mode,
                ext,
            }),
        )
        .await
    }

    /// Report this viewer's decode health for an inbound route back to its
    /// streamer (receiver → sender), so the streamer can adapt the stream.
    /// Best-effort and unacknowledged: an old streamer drops the message and
    /// never adapts, exactly as today.
    pub async fn send_video_feedback(
        self: &Arc<Self>,
        route_id: String,
        recv_fps: u32,
        decode_fails: u32,
        queue_depth: u32,
        lost_ts_us: Option<u64>,
    ) -> Result<(), String> {
        let peer = self.route_peer(&route_id).ok_or("unknown route")?;
        // Enrich with what this end measured about the link itself: the
        // chunk-train bandwidth estimate + delay trend (T1.1). Rides the
        // same control channel on the ICE datapath as the report always
        // did — zeros for routes with no timed trains yet.
        let (est_kbps, delay_trend_us_per_s) = self.route_link_estimate(&route_id);
        let audio_routes = {
            let me = self.local_node_id().map(|id| pubkey_part(&id).to_string());
            let peer_id = pubkey_part(&peer);
            let st = self.state.lock();
            st.session
                .as_ref()
                .map(|session| {
                    session
                        .active_routes()
                        .filter(|active| {
                            active.route.media == MediaKind::Audio
                                && pubkey_part(active.peer.as_str()) == peer_id
                                && me.as_deref().is_some_and(|local| {
                                    pubkey_part(node_of(active.route.to.as_str()).as_str()) == local
                                })
                        })
                        .map(|active| active.route.id.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let mut audio_arrival_jitter_us = 0;
        let mut audio_target_ms = 0;
        let mut audio_buffered_ms = 0;
        let mut audio_underruns = 0u64;
        let mut audio_underrun_frames = 0u64;
        for audio_route in audio_routes {
            if let Some(feedback) = self.audio.take_receive_feedback(&audio_route) {
                audio_arrival_jitter_us = audio_arrival_jitter_us.max(feedback.arrival_jitter_us);
                audio_target_ms = audio_target_ms.max(feedback.target_depth_ms);
                audio_buffered_ms = audio_buffered_ms.max(feedback.buffered_depth_ms);
                audio_underruns = audio_underruns.saturating_add(feedback.underrun_events);
                audio_underrun_frames =
                    audio_underrun_frames.saturating_add(feedback.underrun_frames);
            }
        }
        let ext = crate::video::PipelineFeedback {
            est_kbps,
            delay_trend_us_per_s,
            audio_arrival_jitter_us,
            audio_target_ms,
            audio_buffered_ms,
            audio_underruns,
            audio_underrun_frames,
        }
        .to_ext();
        let incarnation = self.route_incarnation(&route_id);
        self.send_control(
            &peer,
            &ControlMessage::Route(RouteControl::VideoFeedback {
                route_id,
                incarnation,
                recv_fps,
                decode_fails,
                queue_depth,
                lost_ts_us,
                ext,
            }),
        )
        .await
    }

    /// Front-end command: forward one keyboard/mouse event down an active
    /// outbound input route (the console window's control stream).
    pub async fn send_input(
        self: &Arc<Self>,
        route_id: String,
        action: InputAction,
    ) -> Result<(), String> {
        let me = self.local_node_id().ok_or("mesh not ready")?;
        let (peer, incarnation) = {
            let st = self.state.lock();
            let r = st
                .session
                .as_ref()
                .and_then(|s| s.route(&route_id))
                .ok_or("unknown route")?;
            if !(r.is_active()
                && r.route.media == MediaKind::Input
                && node_of(r.route.from.as_str()) == me)
            {
                return Err("route isn't an active outbound control link".into());
            }
            (r.peer.to_string(), r.incarnation.clone())
        };
        let seq = self.input_seq.fetch_add(1, Ordering::Relaxed);
        let ev = InputEvent::new_with_incarnation(route_id, incarnation, seq, action);
        let payload = serde_json::to_value(&ev).map_err(|e| e.to_string())?;
        self.send_media_value(&peer, payload).await
    }

    /// Front-end command: read this machine's clipboard and push it down an
    /// active outbound clipboard route — called the instant the console
    /// forwards a paste, so the far side pastes *our* content. Text rides one
    /// frame; an image or files ride a chunked transfer (the same shape the
    /// video/term/file planes use). This machine must be the route's source
    /// side; the far end gates the write the same way it gates input
    /// injection. The bytes are read here (the only place that can see file
    /// references on the OS clipboard).
    pub async fn clipboard_paste(self: &Arc<Self>, route_id: String) -> Result<(), String> {
        let me = self.local_node_id().ok_or("mesh not ready")?;
        let peer = {
            let st = self.state.lock();
            let r = st
                .session
                .as_ref()
                .and_then(|s| s.route(&route_id))
                .ok_or("unknown route")?;
            if !(r.is_active()
                && r.route.media == MediaKind::Clipboard
                && node_of(r.route.from.as_str()) == me)
            {
                return Err("route isn't an active outbound clipboard link".into());
            }
            r.peer.to_string()
        };
        self.send_clipboard_contents(&peer, &route_id).await
    }

    /// Front-end command: copy/cut **from** the remote — ask the far end to
    /// read its clipboard now and send it back on `route_id`, so the content
    /// it just copied lands on *this* machine. The mirror of
    /// [`Self::clipboard_paste`]: the console forwards the copy keystroke down
    /// the control route first (so the remote copies its selection into its
    /// own clipboard), then calls this. We mark the pull so the reply is let
    /// through ([`Self::handle_clipboard_frame`]) and fire the request. This
    /// machine must be the route's source side, exactly as for a paste.
    pub async fn clipboard_pull(self: &Arc<Self>, route_id: String) -> Result<(), String> {
        let me = self.local_node_id().ok_or("mesh not ready")?;
        let peer = {
            let st = self.state.lock();
            let r = st
                .session
                .as_ref()
                .and_then(|s| s.route(&route_id))
                .ok_or("unknown route")?;
            if !(r.is_active()
                && r.route.media == MediaKind::Clipboard
                && node_of(r.route.from.as_str()) == me)
            {
                return Err("route isn't an active outbound clipboard link".into());
            }
            r.peer.to_string()
        };
        // Open the acceptance window *before* the request goes out, so the
        // reply can never beat it (the remote replies on this same route).
        self.clip_pull_at
            .lock()
            .insert(route_id.clone(), std::time::Instant::now());
        self.send_clip_frame(&peer, &route_id, ClipboardEvent::Pull)
            .await
    }

    /// Read this machine's OS clipboard and stream it to `peer` on `route_id`
    /// — the shared body of [`Self::clipboard_paste`] (pushing our clipboard
    /// for the far side to paste) and the [`Pull`](ClipboardEvent::Pull)
    /// reply (sending our just-copied clipboard back to a controller). Text
    /// rides one frame; an image or files ride a chunked transfer, the same
    /// shape the video/term/file planes use.
    async fn send_clipboard_contents(
        self: &Arc<Self>,
        peer: &str,
        route_id: &str,
    ) -> Result<(), String> {
        // Read the OS clipboard off its dedicated thread (a blocking call).
        let svc = self.clipboard.clone();
        let clip = tokio::task::spawn_blocking(move || svc.read())
            .await
            .map_err(|e| e.to_string())?;
        let Some(clip) = clip else {
            return Ok(()); // empty / unreadable clipboard — nothing to send
        };
        match clip {
            LocalClip::Text(text) => {
                self.send_clip_frame(peer, route_id, ClipboardEvent::Text { text })
                    .await
            }
            LocalClip::Image(png) => {
                let transfer = self.clipboard_transfer.fetch_add(1, Ordering::Relaxed);
                let items = vec![ClipboardItem {
                    name: "image.png".into(),
                    size: png.len() as u64,
                }];
                self.send_clip_frame(
                    peer,
                    route_id,
                    ClipboardEvent::Open {
                        transfer,
                        content: ClipboardContentKind::Image,
                        items,
                    },
                )
                .await?;
                for piece in png.chunks(CLIPBOARD_CHUNK_BYTES) {
                    self.send_clip_frame(
                        peer,
                        route_id,
                        ClipboardEvent::Chunk {
                            transfer,
                            item: 0,
                            data: piece.to_vec(),
                        },
                    )
                    .await?;
                }
                self.send_clip_frame(peer, route_id, ClipboardEvent::Close { transfer })
                    .await
            }
            LocalClip::Files(files) => {
                let total: u64 = files.iter().map(|f| f.size).sum();
                if total > MAX_CLIPBOARD_BYTES {
                    return Err(format!(
                        "clipboard files are too large to paste across ({total} bytes)"
                    ));
                }
                let transfer = self.clipboard_transfer.fetch_add(1, Ordering::Relaxed);
                let items = files
                    .iter()
                    .map(|f| ClipboardItem {
                        name: f.name.clone(),
                        size: f.size,
                    })
                    .collect();
                self.send_clip_frame(
                    peer,
                    route_id,
                    ClipboardEvent::Open {
                        transfer,
                        content: ClipboardContentKind::Files,
                        items,
                    },
                )
                .await?;
                for (i, f) in files.iter().enumerate() {
                    // Stream each file from disk in channel-sized pieces, so a
                    // big paste never loads the whole file into memory.
                    let mut file = std::fs::File::open(&f.path).map_err(|e| e.to_string())?;
                    let mut buf = vec![0u8; CLIPBOARD_CHUNK_BYTES];
                    loop {
                        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
                        if n == 0 {
                            break;
                        }
                        self.send_clip_frame(
                            peer,
                            route_id,
                            ClipboardEvent::Chunk {
                                transfer,
                                item: i as u32,
                                data: buf[..n].to_vec(),
                            },
                        )
                        .await?;
                    }
                }
                self.send_clip_frame(peer, route_id, ClipboardEvent::Close { transfer })
                    .await
            }
        }
    }

    /// Send one clipboard frame to `peer` on `route_id`, fire-and-forget over
    /// the media channel (the same path control input rides).
    async fn send_clip_frame(
        &self,
        peer: &str,
        route_id: &str,
        event: ClipboardEvent,
    ) -> Result<(), String> {
        let seq = self.clipboard_seq.fetch_add(1, Ordering::Relaxed);
        let frame = ClipboardFrame::new(route_id, seq, event);
        let payload = serde_json::to_value(&frame).map_err(|e| e.to_string())?;
        self.send_media_value(peer, payload).await
    }

    /// A clipboard route carries frames both ways, like the files plane:
    ///   * **Sink side** (we're the route's `to`) — the controlled machine.
    ///     A paste pushes the controller's clipboard here, so we reassemble it
    ///     and write our OS clipboard; a [`Pull`](ClipboardEvent::Pull) asks
    ///     for *our* clipboard (a copy/cut driven from the console), so we read
    ///     it and stream it back. Either way it's part of being driven, so it
    ///     takes the same gate as input injection: a live route from this exact
    ///     sender *and* that sender being our owner or a co-owned fleet member.
    ///   * **Source side** (we're the route's `from`) — the controller. This
    ///     is the reply to a copy/cut we pulled, so we write it to our OS
    ///     clipboard — but only inside the window our own [`Self::clipboard_pull`]
    ///     opened, so a peer can never push onto our clipboard unasked.
    ///
    /// Text is one frame; an image or files arrive as a chunked transfer that
    /// commits on `Close`. A paste/copy keystroke rides the paired control
    /// route on the same ordered channel, so order is honoured end to end.
    fn handle_clipboard_frame(self: &Arc<Self>, from: &str, frame: ClipboardFrame) {
        let Some(me) = self.local_node_id() else {
            return;
        };
        let (sinks_here, sources_here) = {
            let st = self.state.lock();
            let Some(r) = st.session.as_ref().and_then(|s| s.route(&frame.route)) else {
                return;
            };
            if !(r.is_active()
                && r.route.media == MediaKind::Clipboard
                && pubkey_part(r.peer.as_str()) == pubkey_part(from))
            {
                return;
            }
            (
                node_of(r.route.to.as_str()) == me,
                node_of(r.route.from.as_str()) == me,
            )
        };

        if sinks_here {
            if !self.sender_may_drive(from, DrivePlane::Clipboard) {
                // Same loud refusal as input: the route was live (checked
                // above), so the failed gate is authorization.
                self.refuse_control_frame(from, &frame.route, "clipboard", true);
                return;
            }
            if let ClipboardEvent::Pull = frame.event {
                // Copy/cut *from* this machine: the controller forwarded the
                // copy keystroke just ahead of this on the same ordered
                // channel, so our clipboard is (about to be) the freshly-copied
                // selection. Give the OS a beat to land it, then stream it back
                // on this route — the mirror of a paste. Through `crate::spawn`
                // (never a bare `tokio::spawn`), so it rides the engine's
                // registered runtime handle like every other engine task.
                let mesh = self.clone();
                let peer = from.to_string();
                let route = frame.route;
                crate::spawn(async move {
                    tokio::time::sleep(CLIPBOARD_COPY_SETTLE).await;
                    if let Err(e) = mesh.send_clipboard_contents(&peer, &route).await {
                        tracing::warn!("clipboard pull reply failed: {e}");
                    }
                });
                return;
            }
            self.apply_clipboard_event(frame.route, frame.event);
        } else if sources_here {
            // Accept a reply only inside the window our own pull opened; a
            // transfer's opening frame consumes that window (one reply per
            // pull), and its later Chunk/Close ride through on the
            // clip_inbound entry the Open registered (unknown transfers no-op).
            let accept = match &frame.event {
                ClipboardEvent::Text { .. } | ClipboardEvent::Open { .. } => self
                    .clip_pull_at
                    .lock()
                    .remove(&frame.route)
                    .is_some_and(|t| t.elapsed() < CLIPBOARD_PULL_WINDOW),
                ClipboardEvent::Chunk { .. } | ClipboardEvent::Close { .. } => true,
                ClipboardEvent::Pull | ClipboardEvent::Unknown => false,
            };
            if accept {
                self.apply_clipboard_event(frame.route, frame.event);
            }
        }
    }

    /// Write one received clipboard event to this machine's OS clipboard —
    /// the shared body of both directions of [`Self::handle_clipboard_frame`].
    /// Text commits at once; an image or files reassemble across a transfer
    /// and commit on `Close`. File bytes stream to a per-transfer staging dir
    /// the OS clipboard is then pointed at.
    fn apply_clipboard_event(&self, route: String, event: ClipboardEvent) {
        match event {
            ClipboardEvent::Text { text } => self.clipboard.set_text(text),
            ClipboardEvent::Open {
                transfer,
                content,
                items,
            } => {
                let total: u64 = items.iter().map(|i| i.size).sum();
                if total > MAX_CLIPBOARD_BYTES {
                    tracing::warn!("clipboard transfer too large ({total} bytes) — refused");
                    return;
                }
                if content == ClipboardContentKind::Files {
                    let dir = crate::clipboard::staging_dir(transfer);
                    let _ = std::fs::remove_dir_all(&dir);
                    if let Err(e) = std::fs::create_dir_all(&dir) {
                        tracing::warn!("clipboard staging dir failed: {e}");
                        return;
                    }
                }
                self.clip_inbound
                    .lock()
                    .insert((route, transfer), ClipInbound::new(content, items));
            }
            ClipboardEvent::Chunk {
                transfer,
                item,
                data,
            } => {
                let key = (route, transfer);
                let mut inbound = self.clip_inbound.lock();
                let Some(t) = inbound.get_mut(&key) else {
                    return; // unknown / already-dropped transfer
                };
                t.received += data.len() as u64;
                let over = t.received > MAX_CLIPBOARD_BYTES;
                if !over {
                    match t.content {
                        ClipboardContentKind::Image => t.image.extend_from_slice(&data),
                        ClipboardContentKind::Files => {
                            if let Some(name) = t.items.get(item as usize).map(|i| i.name.clone()) {
                                let first = !t.started[item as usize];
                                t.started[item as usize] = true;
                                let path =
                                    crate::clipboard::staging_dir(transfer).join(safe_name(&name));
                                if let Err(e) = append_chunk(&path, &data, first) {
                                    tracing::warn!("clipboard stage write failed: {e}");
                                }
                            }
                        }
                        // A content kind a newer build introduced — drop the
                        // bytes (we've nowhere to put them).
                        ClipboardContentKind::Unknown => {}
                    }
                }
                // `t`'s borrow ends above; only now can the map be mutated.
                if over {
                    inbound.remove(&key);
                    tracing::warn!("clipboard transfer exceeded cap — dropped");
                }
            }
            ClipboardEvent::Close { transfer } => {
                let entry = self.clip_inbound.lock().remove(&(route, transfer));
                let Some(t) = entry else {
                    return;
                };
                match t.content {
                    ClipboardContentKind::Image => self.clipboard.set_image(t.image),
                    ClipboardContentKind::Files => {
                        let dir = crate::clipboard::staging_dir(transfer);
                        let paths = t
                            .items
                            .iter()
                            .map(|i| dir.join(safe_name(&i.name)).to_string_lossy().into_owned())
                            .collect();
                        self.clipboard.set_files(paths);
                    }
                    // A content kind a newer build introduced — nothing to
                    // commit to the OS clipboard.
                    ClipboardContentKind::Unknown => {}
                }
            }
            // Pull is handled by the caller (sink side only); a newer build's
            // event is ignored rather than failing the frame.
            ClipboardEvent::Pull | ClipboardEvent::Unknown => {}
        }
    }

    /// Fan one room-plane message out to the given members — the rooms
    /// channel's point-to-point sends (an invite, a join/leave, a chat
    /// line). Best-effort per member: one with no shared network right now
    /// (offline, or never seen) is skipped — the rooms plane has no acks,
    /// and presence plus re-stated invites heal the gaps. Returns how many
    /// members the daemon actually dispatched to, so the UI can be honest
    /// about a line that reached nobody.
    pub async fn room_send(
        &self,
        members: Vec<String>,
        message: RoomMessage,
    ) -> Result<u32, String> {
        let me = self.local_node_id();
        let payload = serde_json::to_value(&message).map_err(|e| e.to_string())?;
        let mut delivered = 0u32;
        for member in members {
            // Never loop a message back at ourselves (the GUI already
            // applied it locally).
            if me
                .as_deref()
                .is_some_and(|m| pubkey_part(m) == pubkey_part(&member))
            {
                continue;
            }
            let Some(network) = self.network_for_peer(&member) else {
                continue;
            };
            let resp = self
                .client
                .request(&Request::ChannelSendTo {
                    network,
                    channel: CHANNEL_ROOMS.to_string(),
                    peer: pubkey_part(&member).to_string(),
                    payload: payload.clone(),
                })
                .await;
            match resp {
                Ok(r) if r.ok => delivered += 1,
                Ok(r) => tracing::debug!(
                    "room send to {} refused: {}",
                    short_id(&member),
                    r.error.unwrap_or_default()
                ),
                Err(e) => tracing::debug!("room send to {} failed: {e}", short_id(&member)),
            }
        }
        Ok(delivered)
    }

    /// Ordered networks to try when sending to `peer`: the last network the
    /// slot proved (an inbound frame, or a prior confirmed send), then the
    /// primary network, then every other joined network. A multi-homed peer —
    /// a KVM sits on its fleet mesh, the local-claim mesh and the CEC help
    /// mesh at once, broadcasting presence on all of them — keeps overwriting
    /// the single `peer_networks` slot with whichever mesh delivered its last
    /// advert, and that mesh is not necessarily one that carries OUR frames
    /// back (hub topologies relay a beacon without giving us a direct lane).
    /// One slot, one attempt was the lottery behind "shows up online, the
    /// site opens, but nothing connects."
    fn peer_network_candidates(&self, peer: &str) -> Vec<String> {
        let st = self.state.lock();
        ordered_send_candidates(
            st.peer_networks.get(pubkey_part(peer)),
            st.network.as_ref(),
            &st.networks,
        )
    }

    /// Record that a send to `peer` was daemon-confirmed on `network`, so the
    /// tunnel traffic that follows (site/input frames ride the slot) sticks to
    /// a mesh that provably reaches the peer — until the next inbound frame or
    /// confirmed send updates it again.
    fn note_peer_network(&self, peer: &str, network: &str) {
        let mut st = self.state.lock();
        if !st.networks.iter().any(|joined| joined == network) {
            return;
        }
        let key = pubkey_part(peer).to_string();
        let paths = st.peer_networks.entry(key).or_default();
        paths.observed_reachable.insert(network.to_string());
        paths.preferred = Some(network.to_string());
    }

    /// Record a path that carried traffic *from* a peer. This proves
    /// reachability but not that the topology carries our frames back. Keep a
    /// daemon-confirmed outbound preference once one exists; otherwise use the
    /// observed path only as the first probe and let send_control replace it
    /// after a confirmed dispatch.
    fn note_peer_network_observed(&self, peer: &str, network: &str) {
        let mut state = self.state.lock();
        if !state.networks.iter().any(|joined| joined == network) {
            return;
        }
        let paths = state
            .peer_networks
            .entry(pubkey_part(peer).to_string())
            .or_default();
        paths.observed_reachable.insert(network.to_string());
        if paths.preferred.is_none() {
            paths.preferred = Some(network.to_string());
        }
    }

    fn queue_reliable_control(
        &self,
        peer: &str,
        networks: Vec<String>,
        message: &ControlMessage,
        payload: Value,
    ) {
        let peer = pubkey_part(peer).to_string();
        let Some((scope, kind)) = reliable_control_identity(message, &networks) else {
            tracing::warn!(
                peer = %short_id(&peer),
                "reliable route-control message has no bounded worker identity"
            );
            return;
        };
        let Some(daemon) = *self.active_daemon_context.lock() else {
            tracing::debug!(
                peer = %short_id(&peer),
                ?kind,
                "reliable route-control confirmation skipped without an active daemon context"
            );
            return;
        };
        if self.daemon_session_epoch.load(Ordering::SeqCst) != daemon.epoch {
            tracing::debug!(
                peer = %short_id(&peer),
                ?kind,
                daemon_epoch = daemon.epoch,
                "reliable route-control confirmation skipped during daemon reset"
            );
            return;
        }
        let key = ReliableControlKey {
            peer: peer.clone(),
            scope,
        };
        let job = ReliableControlOut {
            peer: peer.clone(),
            networks,
            payload,
            kind,
            daemon,
        };
        let mut workers = self.reliable_control_workers.lock();
        if let Some(worker) = workers.get(&key) {
            if worker.daemon == daemon {
                let replaced = worker.pending.lock().push(job);
                if replaced {
                    tracing::debug!(
                        peer = %short_id(&peer),
                        ?kind,
                        daemon_epoch = daemon.epoch,
                        "coalesced superseded reliable route-control confirmation"
                    );
                }
                return;
            }
            workers.remove(&key);
        }

        let worker_id = self
            .reliable_control_worker_seq
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let pending = Arc::new(Mutex::new(ReliableControlPending::default()));
        workers.insert(
            key.clone(),
            ReliableControlWorkerHandle {
                worker_id,
                daemon,
                pending: pending.clone(),
            },
        );
        drop(workers);

        let client = self.client.clone();
        let daemon_epoch = self.daemon_session_epoch.clone();
        let active_daemon_context = self.active_daemon_context.clone();
        let epoch_rx = self.reliable_control_epoch.subscribe();
        let workers = self.reliable_control_workers.clone();
        crate::spawn(async move {
            Self::run_reliable_control_worker(
                client,
                daemon_epoch,
                active_daemon_context,
                epoch_rx,
                workers,
                key,
                worker_id,
                job,
            )
            .await;
        });
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_reliable_control_worker(
        client: Arc<ControlClient>,
        daemon_epoch: Arc<AtomicU64>,
        active_daemon_context: Arc<Mutex<Option<DaemonContext>>>,
        mut epoch_rx: watch::Receiver<u64>,
        workers: Arc<Mutex<HashMap<ReliableControlKey, ReliableControlWorkerHandle>>>,
        key: ReliableControlKey,
        worker_id: u64,
        first_job: ReliableControlOut,
    ) {
        let mut job = first_job;
        loop {
            if !Self::deliver_reliable_control(
                &client,
                &daemon_epoch,
                &active_daemon_context,
                &mut epoch_rx,
                &job,
            )
            .await
            {
                let mut map = workers.lock();
                if map
                    .get(&key)
                    .is_some_and(|worker| worker.worker_id == worker_id)
                {
                    map.remove(&key);
                }
                return;
            }

            // Queueing takes this same map lock before touching `pending`.
            // A concurrent enqueue therefore either lands before this pop or
            // observes our removal and creates a fresh worker.
            let next = {
                let mut map = workers.lock();
                let Some(worker) = map.get(&key).filter(|worker| worker.worker_id == worker_id)
                else {
                    return;
                };
                let next = worker.pending.lock().pop();
                if next.is_none() {
                    map.remove(&key);
                }
                next
            };
            let Some(next) = next else {
                return;
            };
            job = next;
        }
    }

    async fn await_reliable_control_response<F, T>(
        expected_epoch: u64,
        epoch_rx: &mut watch::Receiver<u64>,
        response: F,
    ) -> Option<T>
    where
        F: std::future::Future<Output = T>,
    {
        if *epoch_rx.borrow_and_update() != expected_epoch {
            return None;
        }
        tokio::select! {
            biased;
            _ = epoch_rx.changed() => None,
            output = response => Some(output),
        }
    }

    async fn deliver_reliable_control(
        client: &ControlClient,
        daemon_epoch: &AtomicU64,
        active_daemon_context: &Mutex<Option<DaemonContext>>,
        epoch_rx: &mut watch::Receiver<u64>,
        job: &ReliableControlOut,
    ) -> bool {
        let context_is_current = || {
            daemon_epoch.load(Ordering::SeqCst) == job.daemon.epoch
                && *active_daemon_context.lock() == Some(job.daemon)
        };
        if !context_is_current() {
            tracing::debug!(
                peer = %short_id(&job.peer),
                ?job.kind,
                daemon_epoch = job.daemon.epoch,
                "retired stale reliable route-control confirmation before send"
            );
            return false;
        }

        let mut last_error = String::new();
        for network in &job.networks {
            if !context_is_current() {
                return false;
            }
            let request = Request::ChannelSendReliable {
                network: network.clone(),
                channel: CHANNEL_CONTROL.to_string(),
                peer: job.peer.clone(),
                payload: job.payload.clone(),
                ttl_ms: OFFER_TIMEOUT.as_millis() as u64,
            };
            let response = client.request_with_timeout(&request, OFFER_TIMEOUT + OFFER_SWEEP);
            let Some(response) =
                Self::await_reliable_control_response(job.daemon.epoch, epoch_rx, response).await
            else {
                tracing::debug!(
                    peer = %short_id(&job.peer),
                    ?job.kind,
                    daemon_epoch = job.daemon.epoch,
                    "cancelled stalled reliable route-control send on daemon reset"
                );
                return false;
            };
            if !context_is_current() {
                return false;
            }
            match response {
                Ok(response) if response.ok => return true,
                Ok(response) => {
                    last_error = response
                        .error
                        .unwrap_or_else(|| "reliable channel send failed".into());
                }
                Err(error) => last_error = error.to_string(),
            }
        }
        tracing::debug!(
            peer = %short_id(&job.peer),
            ?job.kind,
            daemon_epoch = job.daemon.epoch,
            error = %last_error,
            "reliable route-control confirmation did not complete; fast-path delivery remains in effect"
        );
        true
    }

    /// Send a control message to one peer, reporting whether the daemon
    /// actually dispatched it. The daemon's peer set is keyed by the *bare
    /// pubkey* (what signaling announces), while AllMyStuff mostly holds
    /// display ids (`pubkey-SUFFIX`, what presence and `IdentityShow` carry)
    /// — so the id is canonicalised here, at the daemon boundary. Addressing
    /// the display form made every send come back "peer not found", an error
    /// this used to swallow: a claim showed "asking…" and then nothing.
    ///
    /// Tries every shared network until the daemon confirms one (the KVM's
    /// bridge sweeps its networks the same way — "the correct network's send
    /// reaches them and others are harmless no-ops"), then pins the peer's
    /// slot to the network that actually delivered, so the media frames that
    /// follow a route offer ride the proven mesh instead of the last one a
    /// presence advert happened to arrive on.
    async fn send_control(&self, peer: &str, message: &ControlMessage) -> Result<(), String> {
        self.send_control_inner(peer, message, true).await
    }

    /// Send a recovery/control message back through the exact network on which
    /// its triggering frame arrived. Lane ids are network-scoped, so letting a
    /// DeadLane challenge fall through a peer-wide candidate list can target a
    /// different PeerSession's unrelated lane 0.
    async fn send_control_on_network(
        &self,
        peer: &str,
        message: &ControlMessage,
        network: &str,
    ) -> Result<(), String> {
        self.send_control_on_network_inner(peer, message, network, true)
            .await
    }

    async fn send_control_retry_on_network(
        &self,
        peer: &str,
        message: &ControlMessage,
        network: &str,
    ) -> Result<(), String> {
        self.send_control_on_network_inner(peer, message, network, false)
            .await
    }

    async fn send_control_on_network_inner(
        &self,
        peer: &str,
        message: &ControlMessage,
        network: &str,
        queue_reliable: bool,
    ) -> Result<(), String> {
        if !self
            .state
            .lock()
            .networks
            .iter()
            .any(|joined| joined == network)
        {
            return Err(format!("network {network} is no longer joined"));
        }
        let payload = serde_json::to_value(message).map_err(|error| error.to_string())?;
        if queue_reliable && route_control_requires_reliable_delivery(message) {
            self.queue_reliable_control(peer, vec![network.to_string()], message, payload.clone());
        }
        let response = self
            .client
            .request(&Request::ChannelSendTo {
                network: network.to_string(),
                channel: CHANNEL_CONTROL.to_string(),
                peer: pubkey_part(peer).to_string(),
                payload,
            })
            .await
            .map_err(|error| error.to_string())?;
        if !response.ok {
            return Err(response
                .error
                .unwrap_or_else(|| "channel send failed".into()));
        }
        self.note_peer_network(peer, network);
        Ok(())
    }

    /// Retry path for a lifecycle message already represented by one durable
    /// worker job. Repeating the fast application delivery is what reaches a
    /// peer app that subscribed after the daemon acknowledged the first copy;
    /// enqueueing another reliable job on every sweep would only build a
    /// duplicate backlog behind a stalled network candidate.
    async fn send_control_retry(&self, peer: &str, message: &ControlMessage) -> Result<(), String> {
        self.send_control_inner(peer, message, false).await
    }

    async fn send_control_inner(
        &self,
        peer: &str,
        message: &ControlMessage,
        queue_reliable: bool,
    ) -> Result<(), String> {
        let candidates = self.route_network_candidates(peer, message);
        if candidates.is_empty() {
            return Err(format!("no shared network with {peer}"));
        }
        let payload = serde_json::to_value(message).map_err(|e| e.to_string())?;
        if queue_reliable && route_control_requires_reliable_delivery(message) {
            self.queue_reliable_control(peer, candidates.clone(), message, payload.clone());
        }
        let mut last_err = String::new();
        for network in candidates {
            let resp = self
                .client
                .request(&Request::ChannelSendTo {
                    network: network.clone(),
                    channel: CHANNEL_CONTROL.to_string(),
                    peer: pubkey_part(peer).to_string(),
                    payload: payload.clone(),
                })
                .await;
            match resp {
                Ok(r) if r.ok => {
                    self.note_peer_network(peer, &network);
                    self.note_outbound_offer_network(peer, message, &network);
                    return Ok(());
                }
                Ok(r) => {
                    last_err = r.error.unwrap_or_else(|| "channel send failed".into());
                }
                Err(e) => last_err = e.to_string(),
            }
        }
        tracing::warn!("control send to {peer} failed on every shared network: {last_err}");
        Err(last_err)
    }

    fn emit_snapshot(&self) {
        self.sink.emit("allmystuff://session", self.snapshot());
    }

    fn emit_status(&self, status: &str, error: Option<&str>) {
        // Remember it: the emit is fire-and-forget (a GUI that subscribed
        // late never hears it), so `mesh_status` answers with this instead
        // of the front-end inferring liveness from unrelated calls.
        *self.last_status.lock() = (status.to_string(), error.map(str::to_string));
        self.sink.emit(
            "allmystuff://subscription",
            json!({ "status": status, "error": error }),
        );
    }

    /// The daemon-link status as last emitted on `allmystuff://subscription`
    /// (`live` / `no_network` / `disconnected`, plus the error that caused
    /// it) — the front-end's poll-safe way to learn the *current* state
    /// instead of hoping it caught a one-shot event.
    pub fn link_status(&self) -> (String, Option<String>) {
        self.last_status.lock().clone()
    }
}

/// A well-formed but empty owned roster (no fleet yet).
fn empty_owned() -> Value {
    json!({ "key": "", "version": 0, "members": [], "is_owner": false, "network_id": "" })
}

/// The fleet network's display label. A fleet is a closed network owned by the
/// originating node, so when it carries an owner name it reads "<name>'s
/// Fleet"; unnamed, the label is empty and MyOwnMesh falls back to the
/// word-salad network id (the human-communicable name derived from the key).
fn fleet_label(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        String::new()
    } else {
        format!("{name}'s Fleet")
    }
}

// The shape video takes on a console window's IPC channel: a fixed
// 28-byte little-endian header, then the payload. No JSON, no base64;
// the webview hands the bytes straight to a decoder (or, for kind 3,
// straight to the canvas). The route isn't carried — the channel itself
// is per-route.
//
//   [0]      kind: 1 = JPEG frame, 2 = H.264 access unit, 3 = raw RGBA
//   [1]      flags: bit 0 = key (H.264 IDR)
//   [2..4]   reserved
//   [4..8]   width  (JPEG/raw — an H.264 unit carries its size in the SPS)
//   [8..12]  height
//   [12..16] source_width  (JPEG only)
//   [16..20] source_height
//   [20..28] JPEG: frame seq · H.264/raw: timestamp in µs

pub(crate) const VIDEO_IPC_HEADER_LEN: usize = 28;

/// One comparable string for "what this machine advertises": the presence
/// summary + capability list, serialized. The inventory watcher diffs it
/// across rescans — JSON equality is exactly "would peers see something
/// different", since this *is* what presence carries.
fn profile_fingerprint(
    summary: &impl serde::Serialize,
    capabilities: &impl serde::Serialize,
) -> String {
    serde_json::to_string(&(summary, capabilities)).unwrap_or_default()
}

pub(crate) fn video_ipc_header(
    kind: u8,
    flags: u8,
    dims: [u32; 4],
    tail: u64,
    payload_len: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(VIDEO_IPC_HEADER_LEN + payload_len);
    out.push(kind);
    out.push(flags);
    out.extend_from_slice(&[0u8; 2]);
    for d in dims {
        out.extend_from_slice(&d.to_le_bytes());
    }
    out.extend_from_slice(&tail.to_le_bytes());
    out
}

fn video_ipc_bytes(f: &VideoFrame) -> Vec<u8> {
    let mut out = video_ipc_header(
        1,
        0,
        [f.width, f.height, f.source_width, f.source_height],
        f.seq,
        f.jpeg.len(),
    );
    out.extend_from_slice(&f.jpeg);
    out
}

fn h264_ipc_bytes(ts_us: u64, key: bool, data: &[u8]) -> Vec<u8> {
    let mut out = video_ipc_header(2, key as u8, [0; 4], ts_us, data.len());
    out.extend_from_slice(data);
    out
}

/// Node id from a capability id (`"<node>:<device>"`). The node segment is
/// everything before the first colon.
fn node_of(cap_id: &str) -> String {
    cap_id
        .split_once(':')
        .map(|(n, _)| n.to_string())
        .unwrap_or_else(|| cap_id.to_string())
}

/// Whether two node ids name the **same machine**, ignoring the display
/// suffix ([`pubkey_part`] strips the `-<5char>` the UI appends). Routing and
/// presence carry the bare node id, while the front-end builds a route's
/// capability ids from the suffixed display id — so a self / loopback check
/// (`is this route to my own machine?`) must compare canonically. A raw `==`
/// misses a genuine self-route when the two forms differ and tries to send a
/// local terminal out over the wire, where it never comes back.
fn same_node(a: &str, b: &str) -> bool {
    pubkey_part(a) == pubkey_part(b)
}

/// Accept one destructive input event at most once for an exact route
/// lifetime. The media channel is ordered, so a sequence not greater than the
/// last injected value is either a duplicate pump delivery or stale reorder.
fn accept_input_sequence(
    sequences: &mut HashMap<(String, Option<String>), u64>,
    route_id: &str,
    incarnation: &Option<String>,
    seq: u64,
) -> bool {
    let key = (route_id.to_string(), incarnation.clone());
    match sequences.entry(key) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(seq);
            true
        }
        std::collections::hash_map::Entry::Occupied(mut entry) if seq > *entry.get() => {
            entry.insert(seq);
            true
        }
        std::collections::hash_map::Entry::Occupied(_) => false,
    }
}

/// The RTP video lane to pin a new route to `peer_canon` on: its existing pin
/// if it already has one, else the **lowest lane in `[0, cap)` not already
/// taken** by another of that peer's pinned routes. `None` only when the pool
/// is full. Pure (takes the pin map directly) so the race-free assignment is
/// unit-tested. A pinned route's peer is the `to` node of its id
/// (`route:<from>→<to>`); pins for other peers don't constrain this one.
fn free_lane_for_peer(
    pins: &std::collections::HashMap<String, OutboundVideoLanePin>,
    network: &str,
    peer_canon: &str,
    route_id: &str,
    cap: u8,
) -> Option<u8> {
    if let Some(pin) = pins.get(route_id) {
        return (pin.network == network).then_some(pin.lane);
    }
    let used: std::collections::HashSet<u8> = pins
        .iter()
        .filter(|(rid, pin)| {
            pin.network == network
                && rid.as_str() != route_id
                && rid
                    .split_once('→')
                    .is_some_and(|(_, to)| pubkey_part(&node_of(to)) == peer_canon)
        })
        .map(|(_, pin)| pin.lane)
        .collect();
    (0..cap).find(|l| !used.contains(l))
}

/// The device part of a capability id — everything after the node
/// (`"<node>:cam:video0"` → `"cam:video0"`). `None` for a bare node id.
fn device_of(cap_id: &str) -> Option<String> {
    cap_id.split_once(':').map(|(_, dev)| dev.to_string())
}

/// The transport's name for route-active log lines.
fn mode_label(mode: VideoMode) -> &'static str {
    match mode {
        VideoMode::H264 => "H.264 track",
        VideoMode::Mjpeg => "MJPEG",
    }
}

fn audio_profile_for_mode(mode: MediaMode) -> AudioProfile {
    match mode {
        MediaMode::Reach => AudioProfile::Reach,
        MediaMode::Balanced => AudioProfile::Balanced,
        MediaMode::Game => AudioProfile::Game,
        MediaMode::Studio | MediaMode::StudioLossless => AudioProfile::Studio,
    }
}

/// Resolve the actual encoder posture. A policy plan is authoritative over
/// the legacy requested string, so an unsupported Studio Lossless request
/// downgraded to Studio cannot still open an HEVC QP0 encoder.
fn resolved_encoder_mode(
    legacy_mode: Option<&str>,
    effective_mode: Option<MediaMode>,
) -> Option<&str> {
    effective_mode.map(MediaMode::wire_name).or(legacy_mode)
}

// ---- refresh round-trip backoff -------------------------------------------
//
// The per-node refresh asks a peer to re-announce its profile
// ([`ControlMessage::ProfileRequest`]). To keep a held-down refresh from
// hammering a peer, the asker spaces those requests per target under a growing
// envelope: at most one every `PROFILE_REQ_MIN_SECS`, and that floor *doubles*
// each minute of a sustained burst up to a `PROFILE_REQ_MAX_SECS` ceiling
// (5 → 10 → 20 → 40 → 60 s). The envelope resets to its fast floor after a
// `PROFILE_REQ_RESET_IDLE` quiet spell, or after it's sat at the ceiling for
// `PROFILE_REQ_CAP_HOLD` (so a steady once-a-minute refresh eventually earns a
// fresh fast window).

/// Floor between refresh round-trips to one peer — "at most every 5 s".
const PROFILE_REQ_MIN_SECS: u64 = 5;
/// Ceiling the floor grows to over a sustained burst — "down to once a minute".
const PROFILE_REQ_MAX_SECS: u64 = 60;
/// Quiet spell after which the envelope resets to its fast floor.
const PROFILE_REQ_RESET_IDLE: std::time::Duration = std::time::Duration::from_secs(5 * 60);
/// How long the envelope may sit at the ceiling before it resets anyway.
const PROFILE_REQ_CAP_HOLD: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Per-peer backoff state for the refresh round-trip.
#[derive(Clone, Copy)]
struct ProfileReqState {
    /// When the current burst of refreshes began (drives the growing floor).
    burst_start: std::time::Instant,
    /// When we last actually sent a request.
    last_request: std::time::Instant,
}

/// The minimum spacing between refresh round-trips given how long the current
/// burst has run: `PROFILE_REQ_MIN_SECS` through the first minute, then doubling
/// each further minute (10, 20, 40 s …) up to the `PROFILE_REQ_MAX_SECS` ceiling.
fn profile_req_interval(burst_age: std::time::Duration) -> std::time::Duration {
    let level = (burst_age.as_secs() / 60).min(64) as u32; // burst minute (0-based)
    let secs = PROFILE_REQ_MIN_SECS
        .checked_shl(level)
        .unwrap_or(PROFILE_REQ_MAX_SECS)
        .min(PROFILE_REQ_MAX_SECS);
    std::time::Duration::from_secs(secs)
}

/// The burst age at which the floor first reaches the ceiling — where the
/// "sat at the cap" reset window starts counting from.
fn profile_req_cap_reached() -> std::time::Duration {
    let mut level = 0u32;
    while PROFILE_REQ_MIN_SECS.checked_shl(level).unwrap_or(u64::MAX) < PROFILE_REQ_MAX_SECS {
        level += 1;
    }
    std::time::Duration::from_secs(u64::from(level) * 60)
}

/// The pure backoff decision (factored out so the envelope is unit-testable
/// without a clock): given the prior per-peer state and `now`, whether a
/// refresh round-trip is allowed, and the state to store. Resets the burst
/// after a long idle or a long hold at the ceiling.
fn profile_req_decide(
    prev: Option<ProfileReqState>,
    now: std::time::Instant,
) -> (bool, ProfileReqState) {
    let Some(mut st) = prev else {
        return (
            true,
            ProfileReqState {
                burst_start: now,
                last_request: now,
            },
        );
    };
    let idle = now.duration_since(st.last_request);
    if idle >= PROFILE_REQ_RESET_IDLE
        || now.duration_since(st.burst_start) >= profile_req_cap_reached() + PROFILE_REQ_CAP_HOLD
    {
        st.burst_start = now;
    }
    let interval = profile_req_interval(now.duration_since(st.burst_start));
    if now.duration_since(st.last_request) >= interval {
        st.last_request = now;
        (true, st)
    } else {
        (false, st)
    }
}

/// Whether `route` is a mesh-native terminal session: generic media whose
/// source endpoint is a machine's `…:terminal` handle. (Terminal
/// endpoints are deliberately *not* catalog capabilities — generic would
/// match every auto-wiring picker — so the shape of the route is the
/// contract.)
fn is_terminal_route(route: &Route) -> bool {
    route.media == MediaKind::Generic && route.from.as_str().ends_with(":terminal")
}

/// Whether `route` is a mesh-native file session: generic media whose
/// source endpoint is a machine's `…:files` handle — the same shape-as-
/// contract scheme the terminal uses.
fn is_files_route(route: &Route) -> bool {
    route.media == MediaKind::Generic && route.from.as_str().ends_with(":files")
}

/// Whether `route` is a room **Shared Files** fetch session: generic media
/// whose source endpoint is a machine's `…:shared` handle. Unlike a files
/// route it is *not* owner/fleet gated — any room member may open one — but
/// it can only `Fetch` by token (see [`FilesPlane`] callers); the host
/// gates each fetch on the token's allow-list, so it never browses a disk.
fn is_shared_route(route: &Route) -> bool {
    route.media == MediaKind::Generic && route.from.as_str().ends_with(":shared")
}

/// Whether `route` is a site (reverse-proxy) session: generic media whose
/// source endpoint is a machine's `…:site` handle — the same shape-as-
/// contract scheme the terminal and files use.
fn is_site_route(route: &Route) -> bool {
    route.media == MediaKind::Generic && route.from.as_str().ends_with(":site")
}

/// What an audio route this machine sources should capture: the synthetic
/// `system-audio` capability advertises "what this machine plays", so it
/// captures the machine's own output (loopback); every other audio source
/// is a scanned input device — the default mic in v1. Pure, so the rule
/// that decides between "your room" and "your sound" is unit-testable.
fn audio_capture_source(route: &Route) -> CaptureSource {
    match route.from.as_str().split_once(':') {
        Some((_, "system-audio")) => CaptureSource::System,
        _ => CaptureSource::Mic,
    }
}

/// A privileged plane a peer can drive on this machine — the unit a share
/// grant authorizes. Owner/fleet trust covers every plane; a person-to-person
/// share covers only the exact plane(s) the owner granted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DrivePlane {
    /// Keyboard/mouse injection into this machine's `:control` input sink.
    Input,
    /// A shell on this machine.
    Terminal,
    /// This machine's disk.
    Files,
    /// Reverse-proxying a service this machine exposes.
    Sites,
    /// Writing this machine's clipboard (rides with the control grant).
    Clipboard,
}

/// The privileged plane a route carries, if any — so the offer screen and the
/// per-frame gate authorize the same plane for the same route.
fn route_drive_plane(route: &Route) -> Option<DrivePlane> {
    if is_terminal_route(route) {
        Some(DrivePlane::Terminal)
    } else if is_files_route(route) {
        Some(DrivePlane::Files)
    } else if is_site_route(route) {
        Some(DrivePlane::Sites)
    } else {
        None
    }
}

/// Whether `grant` authorizes `plane`. Each plane maps to exactly the grant the
/// share builder mints for it (`gui/src/store.svelte.ts::shareCapGrants`), so
/// the planes never cross-authorize: a control (input) grant only injects, a
/// files (storage) grant only reaches the disk, terminal/sites are distinct
/// generic grants told apart by their capability suffix.
fn grant_authorizes_plane(grant: &Grant, plane: DrivePlane) -> bool {
    let cap_ends = |suffix: &str| {
        grant
            .capability
            .as_ref()
            .is_some_and(|c| c.as_str().ends_with(suffix))
    };
    match plane {
        DrivePlane::Input => grant.media == MediaKind::Input && grant.role.allows_sink(),
        DrivePlane::Terminal => grant.media == MediaKind::Generic && cap_ends(":terminal"),
        DrivePlane::Files => grant.media == MediaKind::Storage && cap_ends(":files"),
        DrivePlane::Sites => grant.media == MediaKind::Generic && cap_ends(":sites"),
        DrivePlane::Clipboard => grant.media == MediaKind::Clipboard,
    }
}

/// |skew| at which the passive clock estimate warns (10 s: far beyond
/// presence-delivery jitter, well inside the range where wall-clock
/// last-writer-wins and TOTP windows start misbehaving).
const CLOCK_SKEW_WARN_MS: i64 = 10_000;
/// |skew| the estimate must fall back under before a raised warning clears —
/// hysteresis so the warning doesn't flap at the threshold.
const CLOCK_SKEW_CLEAR_MS: i64 = 5_000;

/// Median of `samples` (odd length), or the **smaller-magnitude** middle
/// (even length). The conservative even-length pick means a *strict
/// majority* of peers must agree we're off before the network estimate
/// crosses a threshold: two peers split [0 s, 60 s] verdicts 0 — it's that
/// peer's clock that's wrong, and its own node warns against *its* peers.
fn conservative_median(samples: &[i64]) -> Option<i64> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    if n % 2 == 1 {
        return Some(sorted[n / 2]);
    }
    let (a, b) = (sorted[n / 2 - 1], sorted[n / 2]);
    Some(if a.abs() <= b.abs() { a } else { b })
}

/// This machine's wall clock as Unix-epoch milliseconds — the presence
/// `sent_at` stamp.
fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Whether a fleet member's presence advert is evidence it left the fleet:
/// only an owner it *positively names* that isn't us. `None` (no owner in the
/// advert) is ambiguous — an early advert sent before its ownership store
/// loaded, an older build, a foreign bridge — and must never author the
/// eviction tombstone that roster convergence then propagates fleet-wide.
/// Pure, because getting this wrong is how remote control silently died once.
fn fleet_departure(advertised_owner: Option<&str>, me: Option<&str>) -> bool {
    match advertised_owner {
        Some(owner) => Some(owner) != me,
        None => false,
    }
}

/// Why an inbound terminal/files/site offer must be refused, if it must: it
/// asks *this* machine to host a shell (or hand over its disk, or proxy a
/// service) and the offerer is neither owner/fleet nor holds a share grant for
/// that plane (`authorized` folds both — the caller computes it per the route's
/// plane). `None` = fine (not a privileged offer, not our side to host, or the
/// sender is authorized). Pure, so the rule that guards the most privileged
/// things on the mesh is unit-testable.
fn privileged_offer_refusal(route: &Route, hosts_here: bool, authorized: bool) -> Option<String> {
    if !hosts_here || authorized {
        return None;
    }
    if is_terminal_route(route) {
        return Some("not authorized: terminal access needs owner/fleet or a share".into());
    }
    if is_files_route(route) {
        return Some("not authorized: file access needs owner/fleet or a share".into());
    }
    if is_site_route(route) {
        return Some("not authorized: site access needs owner/fleet or a share".into());
    }
    None
}

/// `dir/name`, made unique the Finder way: `name (2).ext`, `name (3).ext`…
/// when something already sits there.
fn unique_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = std::path::Path::new(name);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string());
    let ext = p.extension().map(|e| e.to_string_lossy().into_owned());
    for n in 2.. {
        let candidate = match &ext {
            Some(ext) => dir.join(format!("{stem} ({n}).{ext}")),
            None => dir.join(format!("{stem} ({n})")),
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

/// The stable pubkey portion of a mesh device id — strip MyOwnMesh's trailing
/// 5-char display suffix (`-AB12C`). Mirrors the core's `signing::pubkey_part`,
/// so a device id in display form (`pubkey-SUFFIX`, what `IdentityShow` and
/// presence use) and bare form (`pubkey`, what the daemon delivers as a
/// channel `from`) compare equal.
fn pubkey_part(id: &str) -> &str {
    if let Some((body, suffix)) = id.rsplit_once('-') {
        if suffix.len() == 5 && suffix.chars().all(|c| c.is_ascii_alphanumeric()) {
            return body;
        }
    }
    id
}

/// Extract the presence boot from a negotiated route incarnation. The full
/// shape is validated here, not just the prefix, so a malformed token never
/// triggers a profile association or reaches the route state machine.
fn route_incarnation_boot(value: &str) -> Option<u64> {
    let (boot, sequence) = value.split_once(':')?;
    if sequence.contains(':') {
        return None;
    }
    let boot = boot.parse::<u64>().ok()?;
    let sequence = sequence.parse::<u64>().ok()?;
    (boot != 0 && sequence != 0).then_some(boot)
}

fn exact_accept_terminal_response(
    route_id: &str,
    from: &str,
    incarnation: &str,
    route: Option<&allmystuff_session::LiveRoute>,
) -> Option<ControlMessage> {
    match route {
        Some(route)
            if pubkey_part(route.peer.as_str()) == pubkey_part(from)
                && route.origin == allmystuff_session::Origin::Outbound
                && route.incarnation.as_deref() == Some(incarnation)
                && matches!(route.state, RouteState::Offered | RouteState::Active) =>
        {
            None
        }
        Some(route)
            if pubkey_part(route.peer.as_str()) == pubkey_part(from)
                && route.incarnation.as_deref() == Some(incarnation)
                && route.state == RouteState::TornDown =>
        {
            Some(ControlMessage::Route(RouteControl::Teardown {
                route_id: route_id.to_string(),
                incarnation: Some(incarnation.to_string()),
            }))
        }
        Some(route)
            if pubkey_part(route.peer.as_str()) == pubkey_part(from)
                && route.incarnation.as_deref() == Some(incarnation) =>
        {
            let reason = match &route.state {
                RouteState::Rejected { reason } => reason.clone(),
                _ => "route not live on the receiving side".into(),
            };
            Some(ControlMessage::Route(RouteControl::Reject {
                route_id: route_id.to_string(),
                incarnation: Some(incarnation.to_string()),
                reason,
            }))
        }
        _ => Some(ControlMessage::Route(RouteControl::Reject {
            route_id: route_id.to_string(),
            incarnation: Some(incarnation.to_string()),
            reason: "route not live on the receiving side".into(),
        })),
    }
}

fn route_control_requires_reliable_delivery(message: &ControlMessage) -> bool {
    matches!(
        message,
        ControlMessage::Route(
            RouteControl::Offer { .. }
                | RouteControl::Accept { .. }
                | RouteControl::Reject { .. }
                | RouteControl::Teardown { .. }
                | RouteControl::VideoLane { .. }
                | RouteControl::DeadLane { .. }
                | RouteControl::MissingRoute { .. }
        )
    )
}

fn reliable_control_identity(
    message: &ControlMessage,
    networks: &[String],
) -> Option<(ReliableControlScope, ReliableControlKind)> {
    let (scope, kind) = match message {
        ControlMessage::Route(RouteControl::Offer {
            route, incarnation, ..
        }) => (
            ReliableControlScope::Route {
                route_id: route.id.clone(),
                incarnation: incarnation.clone(),
            },
            ReliableControlKind::Offer,
        ),
        ControlMessage::Route(RouteControl::Accept {
            route_id,
            incarnation,
            ..
        }) => (
            ReliableControlScope::Route {
                route_id: route_id.clone(),
                incarnation: incarnation.clone(),
            },
            ReliableControlKind::Accept,
        ),
        ControlMessage::Route(RouteControl::Reject {
            route_id,
            incarnation,
            ..
        }) => (
            ReliableControlScope::Route {
                route_id: route_id.clone(),
                incarnation: incarnation.clone(),
            },
            ReliableControlKind::Reject,
        ),
        ControlMessage::Route(RouteControl::Teardown {
            route_id,
            incarnation,
        }) => (
            ReliableControlScope::Route {
                route_id: route_id.clone(),
                incarnation: incarnation.clone(),
            },
            ReliableControlKind::Teardown,
        ),
        ControlMessage::Route(RouteControl::VideoLane {
            route_id,
            incarnation,
            ..
        }) => (
            ReliableControlScope::Route {
                route_id: route_id.clone(),
                incarnation: incarnation.clone(),
            },
            ReliableControlKind::VideoLane,
        ),
        ControlMessage::Route(RouteControl::DeadLane { media, lane }) => (
            ReliableControlScope::DeadLane {
                media: media.clone(),
                lane: *lane,
                networks: networks.to_vec(),
            },
            ReliableControlKind::DeadLane,
        ),
        ControlMessage::Route(RouteControl::MissingRoute {
            route_id,
            incarnation,
        }) => (
            ReliableControlScope::Route {
                route_id: route_id.clone(),
                incarnation: incarnation.clone(),
            },
            ReliableControlKind::MissingRoute,
        ),
        _ => return None,
    };
    Some((scope, kind))
}

/// Exact route lifetime carried by controls that must stay on one data-plane
/// network. This helper selects an existing pin only. Pin installation is
/// deliberately limited to outbound Offer and authenticated inbound
/// Offer/Accept observations.
fn route_control_network_key(message: &ControlMessage) -> Option<(&str, Option<&str>)> {
    match message {
        ControlMessage::Route(RouteControl::Offer {
            route, incarnation, ..
        }) => Some((&route.id, incarnation.as_deref())),
        ControlMessage::Route(
            RouteControl::Accept {
                route_id,
                incarnation,
                ..
            }
            | RouteControl::Refresh {
                route_id,
                incarnation,
            }
            | RouteControl::Tune {
                route_id,
                incarnation,
                ..
            }
            | RouteControl::VideoFeedback {
                route_id,
                incarnation,
                ..
            }
            | RouteControl::VideoLane {
                route_id,
                incarnation,
                ..
            }
            | RouteControl::Reject {
                route_id,
                incarnation,
                ..
            }
            | RouteControl::Teardown {
                route_id,
                incarnation,
            }
            | RouteControl::TeardownAck {
                route_id,
                incarnation,
            }
            | RouteControl::MissingRoute {
                route_id,
                incarnation,
            },
        ) => Some((route_id, incarnation.as_deref())),
        _ => None,
    }
}

fn network_for_peer_locked(state: &State, peer: &str) -> Option<String> {
    state
        .peer_networks
        .get(pubkey_part(peer))
        .and_then(|paths| {
            paths
                .preferred
                .as_ref()
                .filter(|network| paths.contains(network) && state.networks.contains(*network))
                .cloned()
                .or_else(|| {
                    paths
                        .networks()
                        .into_iter()
                        .filter(|network| state.networks.contains(*network))
                        .min()
                        .cloned()
                })
        })
        .or_else(|| {
            state
                .network
                .as_ref()
                .filter(|network| state.networks.contains(*network))
                .cloned()
        })
}

fn reconcile_network_epochs(state: &mut State, networks: &[String], rotate_existing: bool) {
    let joined = networks
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    state
        .network_epochs
        .retain(|network, _| joined.contains(network));
    for network in networks {
        if rotate_existing || !state.network_epochs.contains_key(network) {
            state.network_epoch_clock = state.network_epoch_clock.wrapping_add(1);
            if state.network_epoch_clock == 0 {
                state.network_epoch_clock = 1;
            }
            state
                .network_epochs
                .insert(network.clone(), state.network_epoch_clock);
        }
    }
}

fn required_subscription_channels() -> [&'static str; 6] {
    [
        CHANNEL_PRESENCE,
        CHANNEL_CONTROL,
        CHANNEL_MEDIA,
        CHANNEL_ROOMS,
        allmystuff_cec_protocol::CHANNEL_CONTROL,
        allmystuff_cec_protocol::CHANNEL_PRESENCE,
    ]
}

fn subscription_missing_count(
    health: &HashMap<String, NetworkSubscriptionState>,
    daemon_epoch: u64,
    client_id: ClientId,
    networks: &[String],
) -> usize {
    networks
        .iter()
        .map(|network| {
            let Some(state) = health
                .get(network)
                .filter(|state| state.belongs_to(daemon_epoch, client_id))
            else {
                return required_subscription_channels().len() + 2;
            };
            let channels = required_subscription_channels()
                .into_iter()
                .filter(|channel| !state.channels.contains(*channel))
                .count();
            channels + usize::from(!state.video) + usize::from(!state.audio)
        })
        .sum()
}

/// Video feedback is generated repeatedly by a window that still owns the
/// route, even for a static screen whose paint rate is zero. One-shot
/// setup/tune controls are intentionally excluded: they can already be in
/// flight beside the stale close we are fencing.
fn inbound_video_feedback_liveness_route_id(msg: &ControlMessage) -> Option<&str> {
    match msg {
        ControlMessage::Route(RouteControl::VideoFeedback { route_id, .. }) => Some(route_id),
        _ => None,
    }
}

fn watcher_poll_proves_liveness(last_poll: Option<Instant>, disconnect_started: Instant) -> bool {
    last_poll.is_some_and(|last| last >= disconnect_started + VIDEO_LOCAL_POLL_PROOF_MIN_AGE)
}

/// Fold one network's daemon peer list into the `pubkey → network` map that
/// [`Mesh::network_for_peer`] addresses control/media with. Each peer the daemon
/// reports **reachable** (`active`/`shelved` — the same cut the graph reads
/// "online" from) learns *this* network as where to address it, keyed by
/// canonical pubkey and only when it has no network yet: a mapping already
/// learned from an inbound frame is proven to carry traffic to us and must win,
/// so this only *fills the gap* for a peer the daemon reports connected but that
/// we have not yet heard from directly. Pure (no daemon, no lock) so the
/// reachable-only / gap-fill / canonical-key rules are unit-tested. See
/// [`Mesh::refresh_peer_networks`] for why the gap is what stranded a peer
/// sharing only a secondary mesh.
/// The daemon peer-status values that count as **reachable** — a live link
/// (`active`/`shelved`), the same cut the graph reads "online" from. An
/// offline / sighted / handshaking / errored row is a peer the daemon
/// remembers, not one you can reach right now.
fn status_is_reachable(status: Option<&str>) -> bool {
    matches!(status, Some("active") | Some("shelved"))
}

fn seed_peer_networks(map: &mut HashMap<String, PeerNetworkState>, peers: &[Value], network: &str) {
    for p in peers {
        if !status_is_reachable(p.get("status").and_then(|v| v.as_str())) {
            continue;
        }
        if let Some(id) = p.get("device_id").and_then(|v| v.as_str()) {
            map.entry(pubkey_part(id).to_string())
                .or_default()
                .daemon_reachable
                .insert(network.to_string());
        }
    }
}

/// The try-order for sending to one peer: its slot (last proven network)
/// first, then the primary, then every other joined network, deduped in that
/// priority. Pure, so the order — the part that decides whether a
/// multi-homed peer's tunnel finds its live mesh — is testable on its own;
/// [`Mesh::peer_network_candidates`] feeds it the live state.
fn ordered_send_candidates(
    paths: Option<&PeerNetworkState>,
    primary: Option<&String>,
    joined: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut reachable = paths
        .map(|paths| paths.networks().into_iter().collect::<Vec<_>>())
        .unwrap_or_default();
    reachable.sort_unstable();
    for n in paths
        .and_then(|paths| paths.preferred.as_ref())
        .into_iter()
        .chain(reachable)
        .chain(primary)
        .chain(joined)
    {
        if joined.contains(n) && !out.contains(n) {
            out.push(n.clone());
        }
    }
    out
}

/// One peer row's link class off the daemon's `selected_pair` — the
/// daemon's own LAN/STUN/TURN rule (host↔host = LAN, which already folds
/// in its private-address override), reduced to the two classes the video
/// gate cares about. No pair reported (ICE unsettled, or a daemon that
/// predates the field) is `Unknown` — the caller must treat that as
/// "don't know", never as a downgrade.
fn link_class_of(peer: &Value) -> crate::video::LinkClass {
    use crate::video::LinkClass;
    let Some(pair) = peer.get("selected_pair").filter(|v| !v.is_null()) else {
        return LinkClass::Unknown;
    };
    let kind = |k: &str| pair.get(k).and_then(|v| v.as_str());
    match (kind("local"), kind("remote")) {
        (Some("host"), Some("host")) => LinkClass::Lan,
        (Some(_), Some(_)) => LinkClass::Wan,
        _ => LinkClass::Unknown,
    }
}

/// Seed `peer_links` from one network's peer list, returning the peers
/// whose class actually CHANGED (Lan↔Wan, or first classification) — the
/// callers retune live streams on those. `Unknown` never touches the map:
/// the daemon clears `selected_pair` on a transient ICE Disconnected, and
/// yanking a stream's dials on a blip would be the gate punishing
/// recovery. Pure (no daemon, no lock), like [`seed_peer_networks`], so
/// the keep-on-unknown rule is unit-tested.
fn seed_peer_links(
    map: &mut HashMap<(String, String), crate::video::LinkClass>,
    peers: &[Value],
    network: &str,
) -> Vec<(String, crate::video::LinkClass)> {
    use crate::video::LinkClass;
    let mut changed = Vec::new();
    for p in peers {
        let Some(id) = p.get("device_id").and_then(|v| v.as_str()) else {
            continue;
        };
        let class = link_class_of(p);
        if class == LinkClass::Unknown {
            continue;
        }
        let peer = pubkey_part(id).to_string();
        let key = (network.to_string(), peer.clone());
        if map.get(&key) != Some(&class) {
            map.insert(key, class);
            changed.push((peer, class));
        }
    }
    changed
}

/// A fresh opaque fetch token for one shared file — 16 random bytes as
/// hex, so it can't be guessed and never leaks the path it stands for.
fn fresh_share_token() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::getrandom(&mut bytes).is_err() {
        // RNG unavailable (vanishingly rare): a wall-clock nonce still
        // makes a unique-enough token for one app run.
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(1);
        return format!("share_{n:032x}");
    }
    let mut s = String::with_capacity(6 + 32);
    s.push_str("share_");
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// A fresh opaque id for one chat line — 16 random bytes as hex, unique within a
/// session so the receiver can dedupe and the sender can recognise the echo of
/// its own message. Mirrors [`fresh_share_token`]; the `msg_` prefix only tells
/// the two apart in a trace.
/// The `cec://viewing` event / `cec_viewing` command payload: technician
/// canonical id → what their live routes actually carry right now.
fn cec_viewing_value(viewing: &std::collections::BTreeMap<String, (bool, bool)>) -> Value {
    let techs: serde_json::Map<String, Value> = viewing
        .iter()
        .map(|(tech, (screen, control))| {
            (
                tech.clone(),
                json!({ "screen": screen, "control": control }),
            )
        })
        .collect();
    json!({ "techs": techs })
}

fn fresh_chat_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::getrandom(&mut bytes).is_err() {
        // RNG unavailable (vanishingly rare): a wall-clock nonce is unique
        // enough for one app run.
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(1);
        return format!("msg_{n:032x}");
    }
    let mut s = String::with_capacity(4 + 32);
    s.push_str("msg_");
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// A fresh random boot id for this app run — never 0, which presence
/// reserves for older peers without the field.
fn fresh_boot_id() -> u64 {
    let mut bytes = [0u8; 8];
    if getrandom::getrandom(&mut bytes).is_err() {
        // RNG unavailable (vanishingly rare): fall back to wall-clock nanos
        // — uniqueness across restarts is all this id needs.
        return std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1)
            .max(1);
    }
    u64::from_le_bytes(bytes).max(1)
}

/// Largest integer that JavaScript can round-trip exactly through a JSON
/// number. Route handles and watcher tokens cross the Tauri boundary as
/// numbers, then return to Rust on disconnect/poll, so their process-local
/// counters must stay inside this range.
const JS_SAFE_INTEGER_MAX: u64 = (1_u64 << 53) - 1;

fn fresh_js_counter_seed() -> u64 {
    (fresh_boot_id() & JS_SAFE_INTEGER_MAX).max(1)
}

fn next_js_safe_counter(counter: &AtomicU64) -> u64 {
    loop {
        let current = counter.load(Ordering::Relaxed);
        let next = if current >= JS_SAFE_INTEGER_MAX {
            1
        } else {
            current + 1
        };
        if counter
            .compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return next;
        }
    }
}

/// Log-friendly head of a mesh id — enough to tell two machines apart in a
/// trace without drowning it in base32.
fn short_id(id: &str) -> String {
    if id.len() > 10 {
        format!("{}…", &id[..10])
    } else {
        id.to_string()
    }
}

/// Log-friendly tail of a fleet key — enough to compare two machines' logs
/// ("do we hold the same key?") without printing the grouping secret.
fn key_tail(key: &str) -> &str {
    let n = key.len();
    if n > 6 {
        &key[n - 6..]
    } else {
        key
    }
}

/// Most bytes a single clipboard paste may move across — a guard against a
/// pathological "copy a huge folder, paste over the mesh". Generous for real
/// copy/paste (documents, images, a handful of files).
const MAX_CLIPBOARD_BYTES: u64 = 256 * 1024 * 1024;

/// How long the controlled side waits after a copy/cut keystroke before
/// reading its clipboard for a [`Pull`](ClipboardEvent::Pull) reply — the
/// beat an app needs to actually land the copied selection on the OS
/// clipboard. The keystroke arrives just ahead of the pull on the same
/// ordered channel; this covers the asynchronous gap after injection.
const CLIPBOARD_COPY_SETTLE: std::time::Duration = std::time::Duration::from_millis(120);

/// How long after sending a [`Pull`](ClipboardEvent::Pull) the controller
/// will accept the reply onto its own clipboard. Generous for the round trip
/// plus the settle above; outside it, a clipboard frame on a route we source
/// is unsolicited and dropped.
const CLIPBOARD_PULL_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

/// An inbound clipboard transfer being reassembled (see
/// [`Mesh::handle_clipboard_frame`]).
struct ClipInbound {
    content: ClipboardContentKind,
    items: Vec<ClipboardItem>,
    /// Per-item: whether its staging file exists yet — so the first chunk
    /// truncates and the rest append.
    started: Vec<bool>,
    /// Accumulated bytes for an image transfer (files stream to disk).
    image: Vec<u8>,
    /// Running total, enforced against [`MAX_CLIPBOARD_BYTES`].
    received: u64,
}

impl ClipInbound {
    fn new(content: ClipboardContentKind, items: Vec<ClipboardItem>) -> Self {
        let n = items.len();
        ClipInbound {
            content,
            items,
            started: vec![false; n],
            image: Vec::new(),
            received: 0,
        }
    }
}

/// Keep only a path's final component, so a crafted item name can't write
/// outside the staging dir.
fn safe_name(name: &str) -> String {
    Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into())
}

/// Append one staging-file piece — the first chunk creates+truncates, the
/// rest append.
fn append_chunk(path: &Path, data: &[u8], first: bool) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true);
    if first {
        opts.truncate(true);
    } else {
        opts.append(true);
    }
    opts.open(path)?.write_all(data)
}

fn parse_media(s: &str) -> MediaKind {
    match s {
        "audio" => MediaKind::Audio,
        "video" => MediaKind::Video,
        "display" => MediaKind::Display,
        "input" => MediaKind::Input,
        "storage" => MediaKind::Storage,
        "clipboard" => MediaKind::Clipboard,
        _ => MediaKind::Generic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reliable_test_job(kind: ReliableControlKind, marker: &str) -> ReliableControlOut {
        ReliableControlOut {
            peer: "peer".into(),
            networks: vec!["network".into()],
            payload: json!({ "marker": marker }),
            kind,
            daemon: DaemonContext {
                epoch: 7,
                client_id: ClientId(11),
            },
        }
    }

    #[test]
    fn reliable_control_pending_is_protocol_bounded_and_preserves_teardown_barrier() {
        let mut pending = ReliableControlPending::default();
        assert!(!pending.push(reliable_test_job(ReliableControlKind::Offer, "old-offer")));
        assert!(!pending.push(reliable_test_job(ReliableControlKind::Teardown, "teardown")));
        assert!(pending.push(reliable_test_job(
            ReliableControlKind::Offer,
            "successor-offer"
        )));
        assert!(!pending.push(reliable_test_job(
            ReliableControlKind::VideoLane,
            "old-lane"
        )));
        assert!(pending.push(reliable_test_job(
            ReliableControlKind::VideoLane,
            "successor-lane"
        )));

        // One slot per reliable route-control kind, with duplicates replacing
        // in place instead of extending a FIFO.
        assert_eq!(pending.len(), 3);
        let teardown = pending.pop().expect("teardown barrier");
        let offer = pending.pop().expect("successor offer");
        let lane = pending.pop().expect("successor lane");
        assert_eq!(teardown.kind, ReliableControlKind::Teardown);
        assert_eq!(offer.kind, ReliableControlKind::Offer);
        assert_eq!(offer.payload["marker"], "successor-offer");
        assert_eq!(lane.kind, ReliableControlKind::VideoLane);
        assert_eq!(lane.payload["marker"], "successor-lane");
        assert!(pending.pop().is_none());
    }

    #[test]
    fn reliable_dead_lane_workers_are_scoped_to_the_exact_network() {
        let message = ControlMessage::Route(RouteControl::DeadLane {
            media: "video".into(),
            lane: 0,
        });
        let (network_a, kind_a) =
            reliable_control_identity(&message, &["network-a".into()]).expect("reliable identity");
        let (network_b, kind_b) =
            reliable_control_identity(&message, &["network-b".into()]).expect("reliable identity");

        assert_eq!(kind_a, ReliableControlKind::DeadLane);
        assert_eq!(kind_b, ReliableControlKind::DeadLane);
        assert_ne!(network_a, network_b);
    }

    #[tokio::test]
    async fn daemon_epoch_change_cancels_a_stalled_reliable_send() {
        let (epoch_tx, mut epoch_rx) = watch::channel(41u64);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let stalled = async move {
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        };
        let task = tokio::spawn(async move {
            Mesh::await_reliable_control_response(41, &mut epoch_rx, stalled).await
        });

        started_rx.await.expect("stalled send entered");
        epoch_tx.send_replace(42);
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("epoch reset must cancel the in-flight send")
            .expect("worker task");
        assert!(result.is_none());
    }

    #[test]
    fn route_connect_handle_wire_shape_matches_frontend_contract() {
        let value = serde_json::to_value(RouteConnectHandle {
            route_id: "route:display".into(),
            generation: JS_SAFE_INTEGER_MAX,
        })
        .expect("serialize route handle");
        assert_eq!(
            value,
            json!({
                "route_id": "route:display",
                "generation": JS_SAFE_INTEGER_MAX
            })
        );
    }

    #[test]
    fn javascript_boundary_counters_are_exact_nonzero_and_wrap_safely() {
        for _ in 0..256 {
            let seed = fresh_js_counter_seed();
            assert!((1..=JS_SAFE_INTEGER_MAX).contains(&seed));
        }

        let counter = AtomicU64::new(JS_SAFE_INTEGER_MAX - 1);
        assert_eq!(next_js_safe_counter(&counter), JS_SAFE_INTEGER_MAX);
        assert_eq!(next_js_safe_counter(&counter), 1);
        assert_eq!(next_js_safe_counter(&counter), 2);
    }

    #[test]
    fn jpeg_rate_gate_enforces_wire_rate_and_recovers_on_cap_change() {
        let t0 = Instant::now();
        let mut gate = JpegRateGate::default();
        // 125 kB at 1 Mbps consumes exactly one second of the grant.
        assert!(gate.admit(1_000_000, 125_000, t0));
        assert!(!gate.admit(1_000_000, 125_000, t0 + Duration::from_millis(999)));
        assert!(gate.admit(1_000_000, 125_000, t0 + Duration::from_secs(1)));

        // A focus/cap increase must take effect now, not inherit the old
        // route's future deadline (the bitrate analogue of sticky fetch_min).
        assert!(gate.admit(8_000_000, 125_000, t0 + Duration::from_millis(1_001)));
        assert!(!gate.admit(0, 1, t0 + Duration::from_secs(2)));
    }

    #[test]
    fn video_route_generation_fences_same_id_successors() {
        let mut generations = VideoRouteGenerations::default();
        let (first, replaced) = generations.begin("route:display");
        assert_eq!(replaced, None);
        assert!(generations.is_current("route:display", first));

        // A real successor can start before the predecessor's stale StopMedia
        // arrives, so no retire occurs between these calls. It still must mint
        // a new generation and fence every queued predecessor callback.
        let (successor, replaced) = generations.begin("route:display");
        assert_eq!(replaced, Some(first));
        assert_ne!(successor, first);
        assert!(!generations.is_current("route:display", first));
        assert!(generations.is_current("route:display", successor));

        generations.retire("route:display");
        assert!(!generations.is_current("route:display", successor));
        let (third, replaced) = generations.begin("route:display");
        assert_eq!(replaced, None);
        assert_ne!(third, successor);
        assert!(generations.is_current("route:display", third));
    }

    #[test]
    fn queued_base64_video_is_fenced_by_route_and_generation() {
        assert!(queued_video_binding_matches(
            Some("route:display"),
            Some(7),
            "route:display",
            7,
        ));
        assert!(
            !queued_video_binding_matches(Some("route:successor"), Some(7), "route:display", 7,),
            "a lane rebind must not deliver a predecessor access unit"
        );
        assert!(
            !queued_video_binding_matches(Some("route:display"), Some(8), "route:display", 7,),
            "a same-id successor must fence the predecessor generation"
        );
        assert!(!queued_video_binding_matches(
            None,
            None,
            "route:display",
            7,
        ));
    }

    #[test]
    fn base64_route_snapshot_cannot_take_a_same_id_successor_generation() {
        let generations = Arc::new(Mutex::new(VideoRouteGenerations::default()));
        let first = generations.lock().begin("route:display").0;
        let (start_successor_tx, start_successor_rx) = std::sync::mpsc::sync_channel(0);
        let (attempting_lock_tx, attempting_lock_rx) = std::sync::mpsc::sync_channel(0);
        let successor_generations = generations.clone();
        let successor = std::thread::spawn(move || {
            start_successor_rx.recv().unwrap();
            attempting_lock_tx.send(()).unwrap();
            successor_generations.lock().begin("route:display").0
        });

        let (route_id, generation) = snapshot_video_route_generation(&generations, || {
            // The successor is released only after the snapshot owns the
            // generation fence. It cannot publish G2 until route R has
            // been associated with G1.
            start_successor_tx.send(()).unwrap();
            attempting_lock_rx.recv().unwrap();
            assert!(
                generations.try_lock().is_none(),
                "lane resolution must run inside the generation critical section"
            );
            Some("route:display".to_string())
        });
        let second = successor.join().unwrap();

        assert_eq!(route_id.as_deref(), Some("route:display"));
        assert_eq!(generation, Some(first));
        assert_ne!(second, first);
        assert!(
            !queued_video_binding_matches(
                route_id.as_deref(),
                Some(second),
                "route:display",
                first,
            ),
            "the queued predecessor must fail the consumer gate after G2 begins"
        );
    }

    #[test]
    fn final_base64_admission_and_successor_flush_are_one_ordered_commit() {
        let generations = Mutex::new(VideoRouteGenerations::default());
        let first = generations.lock().begin("route:display").0;
        let admitted = Mutex::new(Vec::new());

        let committed =
            commit_current_video_generation(&generations, "route:display", first, || {
                assert!(
                    generations.try_lock().is_none(),
                    "the generation fence must remain held through queue admission"
                );
                admitted.lock().push(first);
            });
        assert!(committed.is_some());

        // This is the same ordering used by begin_video_generation: publish
        // the successor and flush old receive state before releasing the
        // fence. Any G1 commit that won first is gone when G2 becomes visible.
        let successor = {
            let mut current = generations.lock();
            let successor = current.begin("route:display").0;
            admitted.lock().clear();
            successor
        };
        assert!(admitted.lock().is_empty());

        let stale_commit_ran = AtomicBool::new(false);
        assert!(
            commit_current_video_generation(&generations, "route:display", first, || {
                stale_commit_ran.store(true, Ordering::SeqCst)
            },)
            .is_none(),
            "a G1 AU that loses to G2 must be rejected before admission"
        );
        assert!(!stale_commit_ran.load(Ordering::SeqCst));
        assert!(generations.lock().is_current("route:display", successor));
    }

    #[test]
    fn media_policy_mutation_and_video_apply_are_serialized_in_generation_order() {
        let client = Arc::new(ControlClient::new().expect("resolve control socket path"));
        let mesh = Mesh::new(client, Arc::new(NoopSink));
        let route_id = "route:policy-order";
        let peer = "peer:policy-order";
        mesh.video
            .install_policy_test_route(route_id, crate::video::Tune::default());
        let capabilities = MediaCapabilities {
            policy_v1: true,
            h264: true,
            opus: true,
            binary_media_pipes: true,
            ..MediaCapabilities::default()
        };

        let (first_mutated_tx, first_mutated_rx) = std::sync::mpsc::sync_channel(0);
        let (release_first_tx, release_first_rx) = std::sync::mpsc::sync_channel(0);
        let first_mesh = mesh.clone();
        let first_capabilities = capabilities.clone();
        let first = std::thread::spawn(move || {
            let serial = first_mesh.video_policy_apply_serial.lock();
            let plans = first_mesh.media_policy.lock().apply_request(
                peer,
                route_id,
                PolicyRequest {
                    peer_cap_bps: Some(4_000_000),
                    priority: true,
                    ..PolicyRequest::default()
                },
                first_capabilities,
                false,
            );
            let cap = plans
                .iter()
                .find(|plan| plan.route_id == route_id)
                .expect("first route plan")
                .route_budget_bps;
            first_mutated_tx.send(cap).unwrap();
            release_first_rx.recv().unwrap();
            first_mesh.apply_video_policy_caps_locked(&plans, &serial);
            cap
        });

        let first_cap = first_mutated_rx.recv().unwrap();
        let (second_attempt_tx, second_attempt_rx) = std::sync::mpsc::sync_channel(0);
        let second_mesh = mesh.clone();
        let second = std::thread::spawn(move || {
            second_attempt_tx.send(()).unwrap();
            let serial = second_mesh.video_policy_apply_serial.lock();
            let plans = second_mesh.media_policy.lock().apply_request(
                peer,
                route_id,
                PolicyRequest {
                    peer_cap_bps: Some(12_000_000),
                    priority: true,
                    ..PolicyRequest::default()
                },
                capabilities,
                false,
            );
            let cap = plans
                .iter()
                .find(|plan| plan.route_id == route_id)
                .expect("second route plan")
                .route_budget_bps;
            second_mesh.apply_video_policy_caps_locked(&plans, &serial);
            cap
        });
        second_attempt_rx.recv().unwrap();

        assert_eq!(
            mesh.media_policy
                .lock()
                .plan(route_id)
                .expect("first policy installed")
                .route_budget_bps,
            first_cap,
            "the newer policy cannot mutate while the older apply transaction is paused"
        );
        release_first_tx.send(()).unwrap();
        assert_eq!(first.join().unwrap(), first_cap);
        let second_cap = second.join().unwrap();
        assert_ne!(first_cap, second_cap, "the fixture must exercise two caps");

        let final_plan = mesh
            .media_policy
            .lock()
            .plan(route_id)
            .expect("final policy")
            .clone();
        let (tune, rate_cap, target) = mesh
            .video
            .policy_test_snapshot(route_id)
            .expect("test route remains installed");
        let expected_cap = final_plan.route_budget_bps.min(u64::from(u32::MAX)) as u32;
        let expected = Some(expected_cap);
        assert_eq!(final_plan.route_budget_bps, second_cap);
        assert_eq!(tune.policy_cap_bps, expected);
        assert_eq!(rate_cap, expected);
        assert!(target <= expected_cap);
        mesh.video.remove_policy_test_route(route_id);
    }

    #[test]
    fn inbound_video_routes_create_the_queue_fence_on_the_receiver() {
        let inbound = inbound_test_route(
            "route:display",
            MediaKind::Display,
            "sender:screen",
            "receiver:display:0",
        );
        assert!(needs_inbound_video_generation(&inbound, "receiver"));

        let outbound = inbound_test_route(
            "route:display",
            MediaKind::Display,
            "receiver:screen",
            "sender:display:0",
        );
        assert!(!needs_inbound_video_generation(&outbound, "receiver"));

        let input = inbound_test_route(
            "route:input",
            MediaKind::Input,
            "sender:control",
            "receiver:input",
        );
        assert!(!needs_inbound_video_generation(&input, "receiver"));
        let loopback = inbound_test_route(
            "route:loopback",
            MediaKind::Display,
            "receiver:screen",
            "receiver:display:0",
        );
        assert!(!needs_inbound_video_generation(&loopback, "receiver"));
    }

    #[test]
    fn recovery_refresh_limiter_retries_after_the_floor() {
        let mut asks = HashMap::new();
        let t0 = Instant::now();
        assert!(reserve_video_refresh(&mut asks, "route:display", t0));
        assert!(!reserve_video_refresh(
            &mut asks,
            "route:display",
            t0 + VIDEO_REFRESH_FLOOR - Duration::from_millis(1),
        ));
        assert!(
            reserve_video_refresh(&mut asks, "route:display", t0 + VIDEO_REFRESH_FLOOR,),
            "a best-effort refresh has no acknowledgement latch and must become eligible again"
        );
        assert!(
            reserve_video_refresh(&mut asks, "route:other", t0 + Duration::from_millis(1),),
            "the limiter is route-local"
        );
    }

    #[test]
    fn monitor_switch_fences_duplicate_early_teardowns() {
        let mut guards = VideoSwitchGuards::default();
        let now = Instant::now();
        guards.note_stop("route:primary", "viewer-ABCDE", "viewer:display:0", now);
        guards.note_start(
            "route:secondary",
            "viewer-ABCDE",
            "viewer:display:0",
            now + Duration::from_millis(8),
        );

        let hit = guards
            .take_early_teardown(
                "route:secondary",
                "viewer-FGHIJ",
                now + Duration::from_millis(15),
            )
            .expect("the first close inside the measured switch race is fenced");
        assert_eq!(hit.predecessor, "route:primary");
        assert_eq!(hit.age, Duration::from_millis(7));
        assert!(
            guards
                .take_early_teardown(
                    "route:secondary",
                    "viewer-ABCDE",
                    now + Duration::from_millis(16),
                )
                .is_some(),
            "a concurrent duplicate cannot consume and defeat the guard"
        );
        assert!(
            guards
                .take_early_teardown(
                    "route:secondary",
                    "viewer-ABCDE",
                    now + Duration::from_millis(8)
                        + VIDEO_SWITCH_TEARDOWN_GUARD
                        + Duration::from_millis(1),
                )
                .is_none(),
            "the guard remains strictly time bounded"
        );
    }

    #[test]
    fn monitor_switch_guard_is_sink_scoped_and_time_bounded() {
        let now = Instant::now();
        let mut guards = VideoSwitchGuards::default();
        guards.note_stop("route:old", "viewer", "viewer:display:0", now);
        guards.note_start(
            "route:other-sink",
            "viewer",
            "viewer:display:1",
            now + Duration::from_millis(5),
        );
        assert!(
            guards
                .take_early_teardown(
                    "route:other-sink",
                    "viewer",
                    now + Duration::from_millis(10),
                )
                .is_none(),
            "an unrelated video sink is not a monitor-switch successor"
        );

        guards.note_start(
            "route:late",
            "viewer",
            "viewer:display:0",
            now + Duration::from_millis(5),
        );
        assert!(
            guards
                .take_early_teardown(
                    "route:late",
                    "viewer",
                    now + Duration::from_millis(5)
                        + VIDEO_SWITCH_TEARDOWN_GUARD
                        + Duration::from_millis(1),
                )
                .is_none(),
            "a deliberate close outside the narrow race window always wins"
        );
    }

    #[test]
    fn monitor_switch_quarantine_is_canceled_or_committed_exactly_once() {
        let now = Instant::now();
        let mut guards = VideoSwitchGuards::default();
        guards.note_stop("route:old-a", "viewer", "viewer:display:0", now);
        guards.note_start(
            "route:new-a",
            "viewer",
            "viewer:display:0",
            now + Duration::from_millis(5),
        );
        let InboundVideoTeardownGate::Quarantine {
            token: canceled,
            incarnation: canceled_incarnation,
            ..
        } = guards.gate_inbound_teardown("route:new-a", "viewer", now + Duration::from_millis(7))
        else {
            panic!("the early close should arm a quarantine");
        };
        assert_eq!(guards.cancel_pending("route:new-a"), Some(canceled));
        assert!(!guards.take_pending_if_current("route:new-a", canceled, canceled_incarnation));

        guards.note_stop("route:old-b", "viewer", "viewer:display:0", now);
        guards.note_start(
            "route:new-b",
            "viewer",
            "viewer:display:0",
            now + Duration::from_millis(5),
        );
        let InboundVideoTeardownGate::Quarantine {
            token: expires,
            incarnation: expires_incarnation,
            ..
        } = guards.gate_inbound_teardown("route:new-b", "viewer", now + Duration::from_millis(7))
        else {
            panic!("the second route should independently arm a quarantine");
        };
        assert!(guards.take_pending_if_current("route:new-b", expires, expires_incarnation));
        assert!(
            !guards.take_pending_if_current("route:new-b", expires, expires_incarnation),
            "an expired timer cannot commit the route twice"
        );

        guards.note_stop("route:old-c", "viewer", "viewer:display:0", now);
        guards.note_start(
            "route:new-c",
            "viewer",
            "viewer:display:0",
            now + Duration::from_millis(5),
        );
        let InboundVideoTeardownGate::Quarantine {
            token: first,
            incarnation: first_incarnation,
            ..
        } = guards.gate_inbound_teardown("route:new-c", "viewer", now + Duration::from_millis(7))
        else {
            panic!("the first close should arm a quarantine");
        };
        assert!(matches!(
            guards.gate_inbound_teardown(
                "route:new-c",
                "viewer",
                now + Duration::from_millis(8),
            ),
            InboundVideoTeardownGate::CoalesceDuplicate { token } if token == first
        ));
        assert!(
            guards.take_pending_if_current("route:new-c", first, first_incarnation),
            "duplicate closes share the original bounded timer"
        );
    }

    #[test]
    fn monitor_switch_reoffer_invalidates_an_old_quarantine_token() {
        let now = Instant::now();
        let mut guards = VideoSwitchGuards::default();
        guards.note_stop("route:old", "viewer", "viewer:display:0", now);
        guards.note_start(
            "route:stable-id",
            "viewer",
            "viewer:display:0",
            now + Duration::from_millis(5),
        );
        let InboundVideoTeardownGate::Quarantine {
            token: old_token,
            incarnation: old_incarnation,
            ..
        } = guards.gate_inbound_teardown(
            "route:stable-id",
            "viewer",
            now + Duration::from_millis(7),
        )
        else {
            panic!("the stale close should be initially eligible");
        };

        guards.note_start(
            "route:stable-id",
            "viewer",
            "viewer:display:0",
            now + Duration::from_millis(8),
        );
        assert!(
            !guards.take_pending_if_current("route:stable-id", old_token, old_incarnation),
            "a delayed timer from an older same-id incarnation is fenced"
        );
    }

    #[test]
    fn monitor_switch_ignores_inflight_feedback_then_accepts_a_mature_heartbeat() {
        let now = Instant::now();
        let armed_at = now + Duration::from_millis(7);
        let mut guards = VideoSwitchGuards::default();
        guards.note_stop("route:old", "viewer", "viewer:display:0", now);
        guards.note_start(
            "route:new",
            "viewer",
            "viewer:display:0",
            now + Duration::from_millis(5),
        );
        let InboundVideoTeardownGate::Quarantine { token, .. } =
            guards.gate_inbound_teardown("route:new", "viewer", armed_at)
        else {
            panic!("the early close should arm a quarantine");
        };
        assert_eq!(
            guards.cancel_pending_on_mature_liveness(
                "route:new",
                armed_at + VIDEO_TEARDOWN_LIVENESS_MIN_AGE - Duration::from_millis(1),
            ),
            None,
            "control already in flight beside the close is not proof"
        );
        assert_eq!(
            guards.cancel_pending_on_mature_liveness(
                "route:new",
                armed_at + VIDEO_TEARDOWN_LIVENESS_MIN_AGE,
            ),
            Some(token),
            "the next periodic viewer heartbeat proves the successor is live"
        );
    }

    #[test]
    fn zero_fps_feedback_is_still_a_static_viewer_heartbeat() {
        let feedback = ControlMessage::Route(RouteControl::VideoFeedback {
            route_id: "route:static".into(),
            incarnation: None,
            recv_fps: 0,
            decode_fails: 0,
            queue_depth: 0,
            lost_ts_us: None,
            ext: Value::Null,
        });
        assert_eq!(
            inbound_video_feedback_liveness_route_id(&feedback),
            Some("route:static")
        );
        assert_eq!(
            inbound_video_feedback_liveness_route_id(&ControlMessage::Route(
                RouteControl::Refresh {
                    route_id: "route:static".into(),
                    incarnation: None,
                },
            )),
            None,
            "one-shot setup/recovery controls are not liveness proof"
        );
    }

    #[test]
    fn local_switch_guard_requires_a_mature_post_disconnect_poll() {
        let disconnect_started = Instant::now();
        assert!(!watcher_poll_proves_liveness(None, disconnect_started));
        assert!(!watcher_poll_proves_liveness(
            Some(disconnect_started + VIDEO_LOCAL_POLL_PROOF_MIN_AGE - Duration::from_millis(1)),
            disconnect_started,
        ));
        assert!(watcher_poll_proves_liveness(
            Some(disconnect_started + VIDEO_LOCAL_POLL_PROOF_MIN_AGE),
            disconnect_started,
        ));
        assert!(VIDEO_LOCAL_POLL_OBSERVE > VIDEO_LOCAL_POLL_PROOF_MIN_AGE);
    }

    #[test]
    fn offered_video_media_is_dropped_without_killing_the_successor() {
        assert_eq!(
            inbound_video_disposition_from_facts(Some(&RouteState::Offered), true, true, true),
            InboundVideoDisposition::Pending
        );
        assert_eq!(
            inbound_video_disposition_from_facts(Some(&RouteState::Active), true, true, true),
            InboundVideoDisposition::Accept
        );
        assert_eq!(
            inbound_video_disposition_from_facts(Some(&RouteState::TornDown), true, true, true),
            InboundVideoDisposition::Reject
        );
        assert_eq!(
            inbound_video_disposition_from_facts(Some(&RouteState::Offered), true, true, false),
            InboundVideoDisposition::Reject,
            "the grace applies only to the authenticated route peer"
        );
    }

    #[test]
    fn first_video_gate_accepts_parameter_set_led_hevc_entry() {
        let hevc_vps = [0, 0, 1, 0x40, 0x01];
        assert!(crate::video_decode::is_decode_entry(&hevc_vps));
        assert!(
            !should_hold_first_video_sample(true, false, &hevc_vps),
            "HEVC entry is carried by VPS bytes because the daemon key bit is H.264-shaped"
        );

        let h264_delta = [0, 0, 1, 0x41, 0x9a];
        assert!(should_hold_first_video_sample(true, false, &h264_delta));
        assert!(!should_hold_first_video_sample(true, true, &h264_delta));
        assert!(!should_hold_first_video_sample(false, false, &h264_delta));
    }

    #[test]
    fn high_refresh_pacing_stays_inside_the_frame_slot() {
        assert_eq!(pace_budget(16, 30), std::time::Duration::from_millis(16));
        assert_eq!(pace_budget(16, 60), std::time::Duration::from_millis(15));
        assert!(pace_budget(16, 120) < std::time::Duration::from_millis(8));
        assert!(pace_budget(16, 144) < std::time::Duration::from_millis(7));
    }

    #[test]
    fn pipe_waits_consume_the_absolute_pacing_budget() {
        let start = Instant::now();
        let deadline = start + std::time::Duration::from_millis(10);
        let ask = std::time::Duration::from_millis(5);
        assert_eq!(pace_gap_until(deadline, start, ask), ask);
        assert_eq!(
            pace_gap_until(deadline, start + std::time::Duration::from_millis(8), ask),
            std::time::Duration::from_millis(2)
        );
        assert!(
            pace_gap_until(deadline, start + std::time::Duration::from_millis(11), ask).is_zero()
        );
    }

    #[test]
    fn dropped_h264_holds_deltas_until_an_accepted_keyframe() {
        assert!(suppress_dependent_after_drop(true, Some(false)));
        assert!(!suppress_dependent_after_drop(true, Some(true)));
        assert!(!suppress_dependent_after_drop(false, Some(false)));
        assert!(!suppress_dependent_after_drop(true, None));
    }

    #[test]
    fn recovery_requires_a_delivered_key_from_the_current_epoch() {
        let recovery = VideoRecovery::new("test:epoch");
        let (arm, drops, first_epoch) = recovery.mark_drop(Some(false));
        assert!(arm, "the first loss arms one IDR");
        assert_eq!(drops, 1);
        assert!(recovery.suppresses(Some(false)));

        // A dependent send that raced with recovery is covered by the same
        // repair: it neither advances the epoch nor creates an IDR storm.
        let (arm, drops, current_epoch) = recovery.mark_drop(Some(false));
        assert!(!arm, "dependent losses do not create an IDR storm");
        assert_eq!(drops, 2);
        assert_eq!(first_epoch, current_epoch);

        // A failed key advances the epoch and always re-arms. The older key's
        // eventual success cannot release deltas; only the newest one can.
        let (arm, _, newest_epoch) = recovery.mark_drop(Some(true));
        assert!(arm);
        assert_ne!(current_epoch, newest_epoch);
        assert!(!recovery.note_key_delivered(current_epoch));
        assert!(recovery.suppresses(Some(false)));
        assert!(recovery.note_key_delivered(newest_epoch));
        assert!(!recovery.suppresses(Some(false)));
        assert_eq!(recovery.drops.load(Ordering::Relaxed), 3);
        assert_eq!(recovery.suppressed.load(Ordering::Relaxed), 0);
    }

    #[cfg(not(feature = "host"))]
    #[test]
    fn captureless_live_profile_matches_the_mobile_viewer_contract() {
        let node = NodeId::from("phone-test");
        let inventory = allmystuff_inventory::scan();
        let capabilities = Mesh::advertised_capabilities(&inventory, &node);
        let expected = allmystuff_mobile_core::mobile_capabilities(
            &node,
            allmystuff_mobile_core::MobileScope::ViewerController,
        );
        assert_eq!(capabilities, expected);

        assert!(capabilities.iter().any(|capability| {
            capability.id.as_str() == "phone-test:display-in"
                && capability.media == MediaKind::Display
                && capability.flow == allmystuff_graph::Flow::Sink
        }));
        assert!(capabilities.iter().any(|capability| {
            capability.id.as_str() == "phone-test:audio-out"
                && capability.media == MediaKind::Audio
                && capability.flow == allmystuff_graph::Flow::Sink
        }));
        assert!(capabilities.iter().all(|capability| {
            !matches!(
                capability.origin.as_str(),
                "control" | "system" | "clipboard" | "screen" | "camera"
            )
        }));

        let mut expected_features = allmystuff_mobile_core::mobile_features(
            allmystuff_mobile_core::MobileScope::ViewerController,
        );
        expected_features.push(FEATURE_ROUTE_INCARNATION.to_string());
        expected_features.push(FEATURE_ROUTE_TEARDOWN_ACK.to_string());
        expected_features.push(FEATURE_MEDIA_INCARNATION.to_string());
        assert_eq!(Mesh::advertised_features(), expected_features);
    }

    #[cfg(feature = "host")]
    #[test]
    fn host_feature_advertisement_is_unchanged() {
        assert_eq!(
            Mesh::advertised_features(),
            vec![
                allmystuff_protocol::FEATURE_FILES.to_string(),
                allmystuff_protocol::FEATURE_ROOMS.to_string(),
                allmystuff_protocol::FEATURE_SITES.to_string(),
                allmystuff_protocol::FEATURE_TERMINAL.to_string(),
                allmystuff_protocol::FEATURE_CAMERA.to_string(),
                FEATURE_ROUTE_INCARNATION.to_string(),
                FEATURE_ROUTE_TEARDOWN_ACK.to_string(),
                FEATURE_MEDIA_INCARNATION.to_string(),
            ]
        );
    }

    fn term_route(from: &str, to: &str, media: MediaKind) -> Route {
        Route {
            id: format!("route:{from}→{to}"),
            from: from.into(),
            to: to.into(),
            media,
        }
    }

    fn test_profile(node: &str, boot: u64, features: Vec<String>) -> NodeProfile {
        NodeProfile {
            protocol: PROTOCOL_VERSION,
            node: node.into(),
            label: node.into(),
            hostname: node.into(),
            summary: allmystuff_protocol::InventorySummary {
                os: "test".into(),
                cpu: "test".into(),
                ram_bytes: 1,
                device_count: 0,
                product: "test".into(),
            },
            capabilities: Vec::new(),
            owner: None,
            claimable: false,
            boot,
            features,
            sites: Vec::new(),
            version: String::new(),
            fleet_name: String::new(),
            fleet_owner: String::new(),
            kvm: None,
            sent_at: 0,
        }
    }

    fn inbound_test_route(id: &str, media: MediaKind, source: &str, sink: &str) -> Route {
        Route {
            id: id.into(),
            from: source.into(),
            to: sink.into(),
            media,
        }
    }

    #[test]
    fn delayed_input_from_predecessor_cannot_inject_under_same_id_successor() {
        let client = Arc::new(ControlClient::new().expect("resolve control socket path"));
        let mesh = Mesh::new(client, Arc::new(NoopSink));
        let mut session = Session::new("receiver");
        assert!(session.apply_presence(test_profile(
            "sender",
            7,
            vec![FEATURE_ROUTE_INCARNATION.into()]
        )));
        let route = inbound_test_route(
            "route:stable-input",
            MediaKind::Input,
            "sender:control",
            "receiver:input",
        );
        let _ = session.handle(
            NodeId::from("sender"),
            ControlMessage::Route(RouteControl::Offer {
                route: route.clone(),
                incarnation: Some("7:1".into()),
                video: Vec::new(),
                audio: Vec::new(),
                session: None,
            }),
        );
        let _ = session.handle(
            NodeId::from("sender"),
            ControlMessage::Route(RouteControl::Offer {
                route,
                incarnation: Some("7:2".into()),
                video: Vec::new(),
                audio: Vec::new(),
                session: None,
            }),
        );
        mesh.state.lock().session = Some(session);

        assert!(!mesh.inbound_media_ok_incarnation(
            "route:stable-input",
            "sender",
            MediaKind::Input,
            Some("7:1")
        ));
        assert!(!mesh.inbound_media_ok_incarnation(
            "route:stable-input",
            "sender",
            MediaKind::Input,
            None
        ));
        assert!(mesh.inbound_media_ok_incarnation(
            "route:stable-input",
            "sender",
            MediaKind::Input,
            Some("7:2")
        ));
    }

    #[test]
    fn stale_lane_challenge_cannot_stop_same_id_successor() {
        let route = inbound_test_route(
            "route:stable-video",
            MediaKind::Display,
            "receiver:screen",
            "sender:display",
        );
        let mut session = Session::new("receiver");
        let _ = session.offer_with_incarnation(
            route,
            "sender",
            vec!["h264".into()],
            Vec::new(),
            Some("7:2".into()),
        );
        let _ = session.handle(
            NodeId::from("sender"),
            ControlMessage::Route(RouteControl::Accept {
                route_id: "route:stable-video".into(),
                incarnation: Some("7:2".into()),
                session: None,
            }),
        );

        assert!(exact_accept_terminal_response(
            "route:stable-video",
            "sender",
            "7:2",
            session.route("route:stable-video")
        )
        .is_none());

        let stale_terminal = exact_accept_terminal_response(
            "route:stable-video",
            "sender",
            "7:1",
            session.route("route:stable-video"),
        )
        .expect("a stale proof receives an exact stale-lifetime terminal response");
        assert!(matches!(
            &stale_terminal,
            ControlMessage::Route(RouteControl::Reject {
                incarnation: Some(incarnation),
                ..
            }) if incarnation == "7:1"
        ));
        assert!(session
            .handle(NodeId::from("sender"), stale_terminal)
            .is_empty());
        assert_eq!(
            session.route("route:stable-video").unwrap().state,
            RouteState::Active
        );

        assert!(matches!(
            exact_accept_terminal_response("missing", "sender", "7:1", None),
            Some(ControlMessage::Route(RouteControl::Reject {
                incarnation: Some(incarnation),
                ..
            })) if incarnation == "7:1"
        ));
    }

    #[test]
    fn dynamic_multilane_peer_is_limited_to_pre_negotiated_lane_zero() {
        let client = Arc::new(ControlClient::new().expect("resolve control socket path"));
        let mesh = Mesh::new(client, Arc::new(NoopSink));
        mesh.daemon_video.store(true, Ordering::SeqCst);
        mesh.daemon_lanes.store(2, Ordering::SeqCst);
        let features = vec![
            FEATURE_ROUTE_INCARNATION.into(),
            allmystuff_protocol::FEATURE_MEDIA_LANES.into(),
            format!("{}:2", allmystuff_protocol::FEATURE_MEDIA_LANES),
        ];
        let mut session = Session::new("receiver");
        assert!(session.apply_presence(test_profile("sender", 7, features.clone())));
        for (id, source, incarnation) in [
            ("route:z-started-first", "sender:screen:2", "7:1"),
            ("route:a-started-second", "sender:screen:1", "7:2"),
        ] {
            let _ = session.handle(
                NodeId::from("sender"),
                ControlMessage::Route(RouteControl::Offer {
                    route: inbound_test_route(id, MediaKind::Display, source, "receiver:display"),
                    incarnation: Some(incarnation.into()),
                    video: vec!["h264".into()],
                    audio: Vec::new(),
                    session: None,
                }),
            );
        }
        {
            let mut state = mesh.state.lock();
            state.networks = vec!["net".into()];
            state.network = Some("net".into());
            state.network_epochs.insert("net".into(), 1);
            for (route_id, incarnation) in [
                ("route:z-started-first", Some("7:1".to_string())),
                ("route:a-started-second", Some("7:2".to_string())),
            ] {
                state.route_networks.insert(
                    (route_id.to_string(), incarnation),
                    RouteNetworkPin {
                        network: "net".into(),
                        network_epoch: 1,
                        confirmed: true,
                    },
                );
            }
            state.peer_features.insert("sender".into(), features);
            state.session = Some(session);
        }

        assert_eq!(
            mesh.video_route_for_lane("net", "sender", 0),
            None,
            "a capable peer must not use lexical fallback before VideoLane"
        );
        mesh.record_video_lane(
            "net",
            "sender",
            "route:z-started-first",
            Some("7:1".into()),
            0,
        );
        mesh.record_video_lane(
            "net",
            "sender",
            "route:a-started-second",
            Some("7:2".into()),
            1,
        );
        assert_eq!(
            mesh.video_route_for_lane("net", "sender", 0).as_deref(),
            Some("route:z-started-first")
        );
        assert_eq!(
            mesh.video_route_for_lane("net", "sender", 1),
            None,
            "lane 1 would trigger SDP renegotiation and must remain unavailable"
        );
    }

    struct NoopSink;
    impl UiSink for NoopSink {
        fn emit(&self, _event: &str, _payload: Value) {}
        fn restart(&self) -> ! {
            unreachable!("test sink never restarts")
        }
    }

    fn exact_offered_route_mesh(incarnation: &str) -> (Arc<Mesh>, Route) {
        let client = Arc::new(ControlClient::new().expect("resolve control socket path"));
        let mesh = Mesh::new(client, Arc::new(NoopSink));
        let route = term_route("me:terminal", "peer:term-view:1", MediaKind::Generic);
        {
            let mut state = mesh.state.lock();
            state.networks = vec!["network-a".into(), "network-b".into()];
            state.network = Some("network-a".into());
            state.network_epochs.insert("network-a".into(), 7);
            state.network_epochs.insert("network-b".into(), 8);
            let mut session = Session::new("me");
            let _ = session.offer_terminal_with_incarnation(
                route.clone(),
                "peer",
                Vec::new(),
                Vec::new(),
                None,
                Some(incarnation.to_string()),
            );
            state.session = Some(session);
        }
        (mesh, route)
    }

    #[derive(Default)]
    struct VideoReadyCountingSink {
        ready: std::sync::atomic::AtomicUsize,
    }

    impl UiSink for VideoReadyCountingSink {
        fn emit(&self, event: &str, _payload: Value) {
            if event == "allmystuff://video-ready" {
                self.ready.fetch_add(1, Ordering::Relaxed);
            }
        }

        fn restart(&self) -> ! {
            unreachable!("test sink never restarts")
        }
    }

    #[test]
    fn decoded_video_ready_events_coalesce_until_poll() {
        let client = Arc::new(ControlClient::new().expect("resolve control socket path"));
        let sink = Arc::new(VideoReadyCountingSink::default());
        let mesh = Mesh::new(client, sink.clone());
        let route = "route:peer:screen→me:display:0";
        mesh.video_watch(route.to_string(), true);

        mesh.enqueue_decoded(route, vec![1, 2, 3], 1, 10);
        assert_eq!(sink.ready.load(Ordering::Relaxed), 1);

        // Freshest-wins replaces the queued picture but does not emit an
        // event storm while the consumer already has one outstanding poke.
        mesh.enqueue_decoded(route, vec![4, 5, 6], 2, 20);
        assert_eq!(sink.ready.load(Ordering::Relaxed), 1);

        assert!(!mesh.video_poll(route).is_empty());
        mesh.enqueue_decoded(route, vec![7, 8, 9], 3, 30);
        assert_eq!(sink.ready.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn displaced_video_watcher_cannot_drain_successor_queue() {
        let client = Arc::new(ControlClient::new().expect("resolve control socket path"));
        let mesh = Mesh::new(client, Arc::new(NoopSink));
        let route = "route:peer:screen→me:display:0";
        let stale = mesh.video_watch(route.to_string(), true);
        let current = mesh.video_watch(route.to_string(), true);

        mesh.enqueue_decoded(route, vec![1, 2, 3], 1, 10);
        assert!(mesh.video_poll_for(route, Some(stale)).is_empty());
        assert!(!mesh.video_poll_for(route, Some(current)).is_empty());
    }

    #[test]
    fn releasing_late_watch_restores_displaced_live_claim() {
        let client = Arc::new(ControlClient::new().expect("resolve control socket path"));
        let mesh = Mesh::new(client, Arc::new(NoopSink));
        let route = "route:peer:screen→me:display:0";
        let intended = mesh.video_watch(route.to_string(), true);
        // Registration is not liveness proof. The intended watcher must have
        // completed a token-valid poll before it may be restored.
        assert!(mesh.video_poll_for(route, Some(intended)).is_empty());
        let obsolete_late = mesh.video_watch(route.to_string(), true);

        mesh.video_unwatch(route, obsolete_late);
        mesh.enqueue_decoded(route, vec![1, 2, 3], 1, 10);
        assert!(!mesh.video_poll_for(route, Some(intended)).is_empty());
    }

    #[test]
    fn never_polled_or_expired_standby_watch_is_not_resurrected() {
        let client = Arc::new(ControlClient::new().expect("resolve control socket path"));
        let mesh = Mesh::new(client, Arc::new(NoopSink));
        let route = "route:test-watch";

        let never_polled = mesh.video_watch(route.to_string(), true);
        let replacement = mesh.video_watch(route.to_string(), true);
        mesh.video_unwatch(route, replacement);
        assert!(!mesh.video_watcher_is_current(route, never_polled));

        let recently_live = mesh.video_watch(route.to_string(), true);
        assert!(mesh.video_poll_for(route, Some(recently_live)).is_empty());
        let replacement = mesh.video_watch(route.to_string(), true);
        {
            let mut watchers = mesh.video_watchers.lock();
            let standby = watchers.standby.get_mut(route).unwrap();
            standby.last_mut().unwrap().last_poll =
                Some(Instant::now() - VIDEO_LOCAL_POLL_OBSERVE - Duration::from_millis(1));
        }
        mesh.video_unwatch(route, replacement);
        assert!(!mesh.video_watcher_is_current(route, recently_live));
        assert!(!mesh.video_watchers.lock().contains_key(route));
    }

    #[test]
    fn non_av_local_media_keeps_the_single_general_ipc_class() {
        let client = Arc::new(ControlClient::new().expect("resolve control socket path"));
        let mesh = Mesh::new(client, Arc::new(NoopSink));

        for tag in ["input", "term", "file", "site", "clip", "future"] {
            assert_eq!(
                mesh.classify_local_media(&json!({ "t": tag })),
                LocalMediaClass::General,
                "{tag} must not be rescheduled by the A/V isolation work"
            );
        }
        assert_eq!(
            mesh.classify_local_media(&json!({ "route": "audio" })),
            LocalMediaClass::Audio
        );
        assert_eq!(
            mesh.classify_local_media(&json!({ "t": "video" })),
            LocalMediaClass::PriorityVideo
        );
        assert_eq!(
            mesh.classify_local_media(&json!({ "t": "video", "route": "unknown" })),
            LocalMediaClass::BackgroundVideo
        );
    }

    /// Regression guard for the silent fleet-wide loss of remote control: a
    /// presence advert with *no* owner (early boot, older build, a foreign
    /// bridge) must never read as "this member left the fleet" — the evict it
    /// used to trigger authors a signed tombstone that roster convergence
    /// mirrors onto every device, and input/clipboard are then refused
    /// everywhere while video (ungated) keeps streaming. Only a positively
    /// different advertised owner is departure.
    #[test]
    fn ownerless_adverts_are_not_fleet_departure() {
        // A member that positively names another owner has left us.
        assert!(fleet_departure(Some("pkB"), Some("pkA")));
        // A member still naming us is ours.
        assert!(!fleet_departure(Some("pkA"), Some("pkA")));
        // No owner in the advert: ambiguous — never an eviction trigger.
        assert!(!fleet_departure(None, Some("pkA")));
        // Even when our own id is unknown (mesh not ready), an ownerless
        // advert stays inert; a named one can only be "not us".
        assert!(!fleet_departure(None, None));
        assert!(fleet_departure(Some("pkB"), None));
    }

    /// The clock-skew estimate must blame *us* only when the majority of
    /// peers agree: a two-way split verdicts the value nearer zero (that
    /// peer's clock is wrong, not ours), and a lone peer's sample carries as
    /// itself (the warning then words itself neutrally).
    #[test]
    fn clock_skew_median_is_conservative() {
        assert_eq!(conservative_median(&[]), None);
        assert_eq!(conservative_median(&[60_000]), Some(60_000));
        // Split 2-peer network: verdict is the sane clock, no self-blame.
        assert_eq!(conservative_median(&[0, 60_000]), Some(0));
        // Both peers agree we're off: verdict says so.
        assert_eq!(conservative_median(&[58_000, 60_000]), Some(58_000));
        assert_eq!(
            conservative_median(&[-60_000, -58_000, -59_000]),
            Some(-59_000)
        );
    }

    /// Regression guard for the GUI crash where `Mesh::new` spawned its media
    /// forwarders inline: the desktop app builds the `Mesh` in a *synchronous*
    /// Tauri `setup` with no ambient Tokio runtime, so a `tokio::spawn` in
    /// `new` panics with "there is no reactor running". This is a plain
    /// `#[test]` (no `#[tokio::test]`) precisely so it runs without a runtime —
    /// if `new` ever spawns again it will panic here. The forwarders are
    /// deferred to `start`, which is always called from an async context.
    #[test]
    fn new_does_not_require_a_running_tokio_runtime() {
        let client = Arc::new(ControlClient::new().expect("resolve control socket path"));
        let _mesh = Mesh::new(client, Arc::new(NoopSink));
    }

    /// Only a destructive local Session reset rotates this node's route boot.
    /// Peer/network cache pruning leaves it alone so surviving peers do not
    /// tear down unrelated healthy routes.
    #[test]
    fn explicit_session_reset_refreshes_a_nonzero_presence_boot_id() {
        let client = Arc::new(ControlClient::new().expect("resolve control socket path"));
        let mesh = Mesh::new(client, Arc::new(NoopSink));
        let before = mesh.route_incarnation_clock.lock().boot;
        assert_ne!(
            before, 0,
            "boot id is never 0 — 0 means a peer without the field"
        );
        mesh.rotate_route_boot();
        let after = mesh.route_incarnation_clock.lock().boot;
        assert_ne!(after, 0, "a refreshed boot id is still non-zero");
        assert_ne!(before, after, "a Session reset must change the boot id");
    }

    #[test]
    fn delayed_retired_or_legacy_presence_cannot_regress_current_boot() {
        let mut boots = HashMap::new();
        let mut retired = HashMap::new();

        assert_eq!(
            admit_peer_boot(&mut boots, &mut retired, "peer", 11),
            PeerBootDisposition::Fresh
        );
        assert_eq!(
            admit_peer_boot(&mut boots, &mut retired, "peer", 22),
            PeerBootDisposition::Fresh
        );
        assert_eq!(boots.get("peer"), Some(&22));
        assert!(retired.get("peer").is_some_and(|set| set.contains(&11)));

        assert_eq!(
            admit_peer_boot(&mut boots, &mut retired, "peer", 11),
            PeerBootDisposition::Retired
        );
        assert_eq!(
            admit_peer_boot(&mut boots, &mut retired, "peer", 0),
            PeerBootDisposition::LegacyDowngrade
        );
        assert_eq!(boots.get("peer"), Some(&22));
        assert_eq!(
            admit_peer_boot(&mut boots, &mut retired, "peer", 22),
            PeerBootDisposition::Current
        );
    }

    #[test]
    fn route_incarnation_clock_resets_boot_and_sequence_atomically() {
        let mut clock = RouteIncarnationClock::new();
        let first = clock.next();
        let original_boot = first.split_once(':').unwrap().0.to_string();
        assert_eq!(first, format!("{original_boot}:1"));
        assert_eq!(clock.next(), format!("{original_boot}:2"));

        clock.reset();
        let successor = clock.next();
        let (successor_boot, successor_sequence) = successor.split_once(':').unwrap();
        assert_ne!(successor_boot, original_boot);
        assert_eq!(successor_sequence, "1");
    }

    /// Regression guard for the screen/audio outage: the engine fires tasks
    /// from capture/audio OS threads (e.g. the DXGI status callback), where a
    /// bare `tokio::spawn` panics with "no reactor running". Every engine spawn
    /// goes through [`crate::spawn`], which must work off-runtime via the handle
    /// `start` registers. Spawn from a plain `std::thread` (no ambient runtime)
    /// and confirm the task actually runs.
    #[test]
    fn engine_spawn_runs_tasks_from_a_non_runtime_thread() {
        let rt = tokio::runtime::Runtime::new().expect("build runtime");
        crate::set_runtime(rt.handle().clone());
        // Keep the runtime (and the registered handle) alive for the process —
        // OnceLock holds the handle, and this is the only test that sets it.
        std::mem::forget(rt);

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // No ambient runtime here — `tokio::spawn` would panic.
            crate::spawn(async move {
                let _ = tx.send(());
            });
        })
        .join()
        .unwrap();

        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("spawned task should run on the registered runtime");
    }

    /// Two routes attaching to one terminal session — the multi-attach
    /// contract the mesh now drives — both see the shell's output, either can
    /// type into the one shell, and the host's session list reports them as a
    /// single shared session. This drives the same [`TerminalHost::open`] the
    /// `start_terminal_host` pump uses (without needing a live daemon), so it
    /// guards the mesh's view of sharing end to end.
    #[cfg(all(unix, feature = "host"))]
    #[test]
    fn two_routes_share_one_session_through_the_host() {
        use crate::terminal::OutMsg;
        use std::time::{Duration, Instant};

        // The mesh's idle reaper / spawns need a runtime registered.
        let rt = tokio::runtime::Runtime::new().expect("build runtime");
        crate::set_runtime(rt.handle().clone());
        std::mem::forget(rt);

        let client = Arc::new(ControlClient::new().expect("resolve control socket path"));
        let mesh = Mesh::new(client, Arc::new(NoopSink));

        // First route creates the session; second attaches to the same id —
        // exactly what an Offer carrying `session: Some(id)` resolves to.
        let a = mesh
            .terminal
            .open(Some("shared"), "routeA", 80, 24)
            .expect("create session");
        assert!(a.created, "first open creates the session");
        let b = mesh
            .terminal
            .open(Some("shared"), "routeB", 80, 24)
            .expect("attach to session");
        assert!(!b.created, "second open attaches to the shared session");

        // The host's picker list reports one shared session with two viewers.
        let infos = mesh.terminal_session_infos();
        let shared = infos
            .iter()
            .find(|s| s.session_id == "shared")
            .expect("session listed");
        assert_eq!(shared.attachers, 2, "both routes counted as attachers");

        // Either route can type into the one shell, and both pumps see it.
        let mut rxa = a.rx;
        let mut rxb = b.rx;
        assert!(mesh.terminal.write("routeB", b"echo via-B\n".to_vec()));

        let saw = |rx: &mut tokio::sync::broadcast::Receiver<OutMsg>, needle: &str| -> bool {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut seen = Vec::new();
            while Instant::now() < deadline {
                match rx.try_recv() {
                    Ok(OutMsg::Data(b)) => {
                        seen.extend_from_slice(&b);
                        if String::from_utf8_lossy(&seen).contains(needle) {
                            return true;
                        }
                    }
                    Ok(OutMsg::Resize { .. }) => {}
                    Ok(OutMsg::Exit(_)) => return false,
                    Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    Err(_) => return false,
                }
            }
            false
        };
        assert!(saw(&mut rxa, "via-B"), "route A sees route B's input");
        assert!(saw(&mut rxb, "via-B"), "route B sees its own echo");

        // Detaching one viewer keeps the shell alive for the other.
        mesh.terminal.detach("routeA");
        assert_eq!(
            mesh.terminal_session_infos()
                .iter()
                .find(|s| s.session_id == "shared")
                .map(|s| s.attachers),
            Some(1),
            "session survives one detach with the remaining attacher",
        );
        mesh.terminal.close("shared");
    }

    #[test]
    fn dedup_collapses_duplicate_terminal_frames_by_seq() {
        // The dedup that collapses a frame delivered on several shared
        // networks back to one (both directions): the sending side numbers a
        // route's frames strictly increasing, so a seq already taken is a
        // duplicate. A different route, and the other direction's map, each
        // keep an independent counter.
        let client = Arc::new(ControlClient::new().expect("resolve control socket path"));
        let mesh = Mesh::new(client, Arc::new(NoopSink));
        let out = &mesh.term_rx_seq;
        let inp = &mesh.term_in_seq;

        assert!(Mesh::accept_term_seq(out, "r", 0), "first frame is fresh");
        assert!(
            !Mesh::accept_term_seq(out, "r", 0),
            "same seq again is a duplicate"
        );
        assert!(Mesh::accept_term_seq(out, "r", 1), "the next seq is fresh");
        assert!(
            !Mesh::accept_term_seq(out, "r", 1),
            "and its duplicate drops"
        );
        assert!(
            !Mesh::accept_term_seq(out, "r", 0),
            "an older straggler drops too"
        );
        assert!(Mesh::accept_term_seq(out, "r", 2), "advancing is fresh");
        assert!(
            Mesh::accept_term_seq(out, "r", 9),
            "a forward jump (sender skipped after a lag) is still fresh"
        );
        assert!(
            Mesh::accept_term_seq(out, "other", 0),
            "a different route has its own independent counter"
        );
        // The input map (host taking keystrokes) is wholly independent of the
        // output map — the same route+seq is fresh again here.
        assert!(
            Mesh::accept_term_seq(inp, "r", 0),
            "input dedup is independent of output dedup"
        );
        assert!(
            !Mesh::accept_term_seq(inp, "r", 0),
            "but still drops its own duplicates"
        );
    }

    #[test]
    fn terminal_routes_are_recognized_by_shape() {
        // Generic media + a `…:terminal` source = a terminal session.
        let term = term_route("host:terminal", "me:term-view:1", MediaKind::Generic);
        assert!(is_terminal_route(&term));

        // Generic data that isn't a terminal stays untouched (the escape
        // hatch keeps working for whatever apps wire through it)…
        let generic = term_route("host:thing", "me:other", MediaKind::Generic);
        assert!(!is_terminal_route(&generic));

        // …and a `:terminal` id under any *other* media is not a terminal
        // (the media is part of the contract, not just the suffix).
        let display = term_route("host:terminal", "me:term-view:1", MediaKind::Display);
        assert!(!is_terminal_route(&display));
    }

    #[test]
    fn ordered_send_candidates_tries_slot_then_primary_then_the_rest() {
        let slot = "cec-help".to_string();
        let paths = PeerNetworkState {
            preferred: Some(slot.clone()),
            observed_reachable: [slot.clone()].into_iter().collect(),
            ..PeerNetworkState::default()
        };
        let primary = "joining".to_string();
        let joined = vec![
            "joining".to_string(),
            "allmystuff-local-claim-v1".to_string(),
            "cec-help".to_string(),
            "fleet-mesh".to_string(),
        ];
        // Slot first (last proven), then primary, then the remaining joined
        // networks — each exactly once. This order is what lets a send to a
        // multi-homed peer (a KVM on fleet + local-claim + help mesh at once)
        // fall through to the mesh that actually carries our frames.
        assert_eq!(
            ordered_send_candidates(Some(&paths), Some(&primary), &joined),
            vec![
                "cec-help".to_string(),
                "joining".to_string(),
                "allmystuff-local-claim-v1".to_string(),
                "fleet-mesh".to_string(),
            ]
        );
        // No slot yet (never heard from the peer): primary leads.
        assert_eq!(
            ordered_send_candidates(None, Some(&primary), &joined),
            vec![
                "joining".to_string(),
                "allmystuff-local-claim-v1".to_string(),
                "cec-help".to_string(),
                "fleet-mesh".to_string(),
            ]
        );
        // Nothing known at all: nothing to try.
        assert_eq!(
            ordered_send_candidates(None, None, &[]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn inbound_observation_cannot_steal_confirmed_outbound_path() {
        let client = Arc::new(ControlClient::new().expect("resolve control socket path"));
        let mesh = Mesh::new(client, Arc::new(NoopSink));
        mesh.state.lock().networks = vec!["confirmed-b".into(), "inbound-a".into()];
        mesh.note_peer_network("peer", "confirmed-b");
        mesh.note_peer_network_observed("peer", "inbound-a");

        assert_eq!(
            mesh.network_for_peer("peer").as_deref(),
            Some("confirmed-b")
        );
        let state = mesh.state.lock();
        let paths = state.peer_networks.get("peer").unwrap();
        assert!(paths.contains("confirmed-b"));
        assert!(paths.contains("inbound-a"));
    }

    #[test]
    fn reliable_offer_fallback_accepts_exact_unpinned_accept_and_confirms_path() {
        let incarnation = Some("11:2".to_string());
        let (mesh, route) = exact_offered_route_mesh("11:2");
        let accept = ControlMessage::Route(RouteControl::Accept {
            route_id: route.id.clone(),
            incarnation: incarnation.clone(),
            session: None,
        });

        assert!(mesh
            .state
            .lock()
            .route_networks
            .get(&(route.id.clone(), incarnation.clone()))
            .is_none());
        assert!(mesh.inbound_route_control_path_ok("peer", &accept, "network-b"));
        {
            let mut state = mesh.state.lock();
            let effects = state
                .session
                .as_mut()
                .unwrap()
                .handle(NodeId::from("peer"), accept.clone());
            assert!(effects.iter().any(|effect| {
                matches!(
                    effect,
                    Effect::StartMedia {
                        route: started,
                        incarnation: started_incarnation,
                    } if started.id == route.id && started_incarnation == &incarnation
                )
            }));
            Mesh::commit_inbound_route_network_locked(
                &mut state,
                "peer",
                &accept,
                "network-b",
            );
            let live = state.session.as_ref().unwrap().route(&route.id).unwrap();
            assert_eq!(live.state, RouteState::Active);
            assert_eq!(
                state
                    .route_networks
                    .get(&(route.id.clone(), incarnation.clone())),
                Some(&RouteNetworkPin {
                    network: "network-b".into(),
                    network_epoch: 8,
                    confirmed: true,
                })
            );
        }
        mesh.active_media_incarnations
            .lock()
            .insert(route.id.clone(), incarnation);
        assert_eq!(
            mesh.network_for_route(&route.id, "peer").as_deref(),
            Some("network-b")
        );
    }

    #[test]
    fn reliable_offer_fallback_accepts_exact_reject_only_while_offered() {
        let incarnation = Some("11:2".to_string());
        let (mesh, route) = exact_offered_route_mesh("11:2");
        let reject = ControlMessage::Route(RouteControl::Reject {
            route_id: route.id.clone(),
            incarnation: incarnation.clone(),
            reason: "not authorized".into(),
        });

        assert!(mesh.inbound_route_control_path_ok("peer", &reject, "network-b"));
        {
            let mut state = mesh.state.lock();
            let _ = state
                .session
                .as_mut()
                .unwrap()
                .handle(NodeId::from("peer"), reject.clone());
            Mesh::commit_inbound_route_network_locked(
                &mut state,
                "peer",
                &reject,
                "network-b",
            );
            let live = state.session.as_ref().unwrap().route(&route.id).unwrap();
            assert!(matches!(live.state, RouteState::Rejected { .. }));
            assert!(!state
                .route_networks
                .contains_key(&(route.id.clone(), incarnation)));
        }
        assert!(!mesh.inbound_route_control_path_ok("peer", &reject, "network-b"));
    }

    #[test]
    fn reliable_offer_fallback_accepts_exact_reject_across_tentative_path() {
        let incarnation = Some("11:2".to_string());
        let (mesh, route) = exact_offered_route_mesh("11:2");
        let offer = ControlMessage::Route(RouteControl::Offer {
            route: route.clone(),
            incarnation: incarnation.clone(),
            video: Vec::new(),
            audio: Vec::new(),
            session: None,
        });
        mesh.note_outbound_offer_network("peer", &offer, "network-a");
        let key = (route.id.clone(), incarnation.clone());
        assert_eq!(
            mesh.state.lock().route_networks.get(&key),
            Some(&RouteNetworkPin {
                network: "network-a".into(),
                network_epoch: 7,
                confirmed: false,
            })
        );

        let reject = ControlMessage::Route(RouteControl::Reject {
            route_id: route.id.clone(),
            incarnation: incarnation.clone(),
            reason: "not authorized".into(),
        });
        assert!(mesh.inbound_route_control_path_ok("peer", &reject, "network-b"));
        {
            let mut state = mesh.state.lock();
            let _ = state
                .session
                .as_mut()
                .unwrap()
                .handle(NodeId::from("peer"), reject.clone());
            Mesh::commit_inbound_route_network_locked(
                &mut state,
                "peer",
                &reject,
                "network-b",
            );
            assert!(matches!(
                state.session.as_ref().unwrap().route(&route.id).unwrap().state,
                RouteState::Rejected { .. }
            ));
            assert_eq!(
                state.route_networks.get(&key),
                Some(&RouteNetworkPin {
                    network: "network-a".into(),
                    network_epoch: 7,
                    confirmed: false,
                }),
                "Reject must not move or confirm a tentative route path"
            );
        }
        assert!(!mesh.inbound_route_control_path_ok("peer", &reject, "network-b"));

        let (confirmed_mesh, confirmed_route) = exact_offered_route_mesh("11:3");
        confirmed_mesh.state.lock().route_networks.insert(
            (confirmed_route.id.clone(), Some("11:3".into())),
            RouteNetworkPin {
                network: "network-a".into(),
                network_epoch: 7,
                confirmed: true,
            },
        );
        assert!(!confirmed_mesh.inbound_route_control_path_ok(
            "peer",
            &ControlMessage::Route(RouteControl::Reject {
                route_id: confirmed_route.id,
                incarnation: Some("11:3".into()),
                reason: "late".into(),
            }),
            "network-b",
        ));
    }

    #[test]
    fn reliable_offer_fallback_reply_exception_is_exact_and_narrow() {
        let incarnation = Some("11:2".to_string());
        let (mesh, route) = exact_offered_route_mesh("11:2");
        let accept = ControlMessage::Route(RouteControl::Accept {
            route_id: route.id.clone(),
            incarnation: incarnation.clone(),
            session: None,
        });

        assert!(!mesh.inbound_route_control_path_ok("intruder", &accept, "network-b"));
        assert!(!mesh.inbound_route_control_path_ok(
            "peer",
            &ControlMessage::Route(RouteControl::Accept {
                route_id: route.id.clone(),
                incarnation: Some("11:1".into()),
                session: None,
            }),
            "network-b",
        ));
        assert!(!mesh.inbound_route_control_path_ok("peer", &accept, "not-joined"));
        mesh.state.lock().network_epochs.remove("network-b");
        assert!(!mesh.inbound_route_control_path_ok("peer", &accept, "network-b"));
        mesh.state
            .lock()
            .network_epochs
            .insert("network-b".into(), 8);

        let other_controls = [
            ControlMessage::Route(RouteControl::Refresh {
                route_id: route.id.clone(),
                incarnation: incarnation.clone(),
            }),
            ControlMessage::Route(RouteControl::Tune {
                route_id: route.id.clone(),
                incarnation: incarnation.clone(),
                max_edge: None,
                bitrate: None,
                fps: None,
                game: false,
                mode: None,
                ext: Value::Null,
            }),
            ControlMessage::Route(RouteControl::VideoFeedback {
                route_id: route.id.clone(),
                incarnation: incarnation.clone(),
                recv_fps: 0,
                decode_fails: 0,
                queue_depth: 0,
                lost_ts_us: None,
                ext: Value::Null,
            }),
            ControlMessage::Route(RouteControl::VideoLane {
                route_id: route.id.clone(),
                incarnation: incarnation.clone(),
                lane: 0,
            }),
            ControlMessage::Route(RouteControl::Teardown {
                route_id: route.id.clone(),
                incarnation: incarnation.clone(),
            }),
            ControlMessage::Route(RouteControl::TeardownAck {
                route_id: route.id.clone(),
                incarnation: incarnation.clone(),
            }),
        ];
        for message in other_controls {
            assert!(
                !mesh.inbound_route_control_path_ok("peer", &message, "network-b"),
                "an unpinned Offered route must not admit {message:?}"
            );
        }

        {
            let mut state = mesh.state.lock();
            let _ = state
                .session
                .as_mut()
                .unwrap()
                .handle(NodeId::from("peer"), accept.clone());
        }
        assert!(!mesh.inbound_route_control_path_ok("peer", &accept, "network-b"));
        assert!(!mesh.inbound_route_control_path_ok(
            "peer",
            &ControlMessage::Route(RouteControl::Reject {
                route_id: route.id,
                incarnation,
                reason: "late".into(),
            }),
            "network-b",
        ));

        let incoming_route = term_route("peer:terminal", "me:term-view:2", MediaKind::Generic);
        let incoming_incarnation = Some("11:3".to_string());
        let mut incoming_session = Session::new("me");
        incoming_session.auto_accept = false;
        assert!(incoming_session.apply_presence(test_profile("peer", 11, Vec::new())));
        let _ = incoming_session.handle(
            NodeId::from("peer"),
            ControlMessage::Route(RouteControl::Offer {
                route: incoming_route.clone(),
                incarnation: incoming_incarnation.clone(),
                video: Vec::new(),
                audio: Vec::new(),
                session: None,
            }),
        );
        assert_eq!(
            incoming_session
                .route(&incoming_route.id)
                .map(|route| &route.state),
            Some(&RouteState::Incoming)
        );
        {
            let mut state = mesh.state.lock();
            state.session = Some(incoming_session);
            state.route_networks.clear();
        }
        assert!(!mesh.inbound_route_control_path_ok(
            "peer",
            &ControlMessage::Route(RouteControl::Accept {
                route_id: incoming_route.id,
                incarnation: incoming_incarnation,
                session: None,
            }),
            "network-b",
        ));
    }

    #[test]
    fn exact_route_pin_survives_peer_preference_changes_but_not_successor_reuse() {
        let client = Arc::new(ControlClient::new().expect("resolve control socket path"));
        let mesh = Mesh::new(client, Arc::new(NoopSink));
        let route = Route {
            id: "route:me:screen->peer:view".into(),
            from: "me:screen".into(),
            to: "peer:view".into(),
            media: MediaKind::Display,
        };
        let offer_a = {
            let mut state = mesh.state.lock();
            state.networks = vec!["route-net".into(), "other-net".into()];
            state.network = Some("other-net".into());
            state.network_epochs.insert("route-net".into(), 1);
            state.network_epochs.insert("other-net".into(), 2);
            state.peer_networks.insert(
                "peer".into(),
                PeerNetworkState {
                    preferred: Some("other-net".into()),
                    observed_reachable: ["route-net".into(), "other-net".into()]
                        .into_iter()
                        .collect(),
                    ..PeerNetworkState::default()
                },
            );
            let mut session = Session::new("me");
            let offer = session.offer_terminal_with_incarnation(
                route.clone(),
                "peer",
                vec!["h264".into()],
                Vec::new(),
                None,
                Some("11:1".into()),
            );
            state.session = Some(session);
            offer
        };

        mesh.note_outbound_offer_network("peer", &offer_a, "route-net");
        let accept = ControlMessage::Route(RouteControl::Accept {
            route_id: route.id.clone(),
            incarnation: Some("11:1".into()),
            session: None,
        });
        {
            let mut state = mesh.state.lock();
            let _ = state
                .session
                .as_mut()
                .unwrap()
                .handle(NodeId::from("peer"), accept.clone());
            Mesh::commit_inbound_route_network_locked(&mut state, "peer", &accept, "route-net");
        }
        mesh.active_media_incarnations
            .lock()
            .insert(route.id.clone(), Some("11:1".into()));
        assert_eq!(
            mesh.network_for_route(&route.id, "peer").as_deref(),
            Some("route-net"),
            "peer-wide preference must not move an established route"
        );

        // Replace the Session route with the same deterministic id but a new
        // lifetime. The predecessor pin must not steer or silently migrate the
        // successor before its own Offer is dispatched.
        {
            let mut state = mesh.state.lock();
            let mut session = Session::new("me");
            let _ = session.offer_terminal_with_incarnation(
                route.clone(),
                "peer",
                vec!["h264".into()],
                Vec::new(),
                None,
                Some("11:2".into()),
            );
            state.session = Some(session);
        }
        assert_eq!(
            mesh.network_for_route(&route.id, "peer"),
            None,
            "a predecessor pin must not alias a same-id successor"
        );
    }

    #[test]
    fn exact_missing_route_may_recover_over_surviving_data_path_without_repinning() {
        let client = Arc::new(ControlClient::new().expect("resolve control socket path"));
        let mesh = Mesh::new(client, Arc::new(NoopSink));
        let route = term_route("me:terminal", "peer:term-view:1", MediaKind::Generic);
        let incarnation = Some("11:1".to_string());
        let accept = ControlMessage::Route(RouteControl::Accept {
            route_id: route.id.clone(),
            incarnation: incarnation.clone(),
            session: None,
        });
        {
            let mut state = mesh.state.lock();
            state.networks = vec!["lost-a".into(), "surviving-b".into()];
            state.network = Some("lost-a".into());
            state.network_epochs.insert("lost-a".into(), 1);
            state.network_epochs.insert("surviving-b".into(), 2);
            state.peer_networks.insert(
                "peer".into(),
                PeerNetworkState {
                    preferred: Some("surviving-b".into()),
                    daemon_reachable: ["surviving-b".to_string()].into_iter().collect(),
                    ..PeerNetworkState::default()
                },
            );
            let mut session = Session::new("me");
            let _ = session.offer_terminal_with_incarnation(
                route.clone(),
                "peer",
                Vec::new(),
                Vec::new(),
                None,
                incarnation.clone(),
            );
            let _ = session.handle(NodeId::from("peer"), accept);
            state.session = Some(session);
            state.route_networks.insert(
                (route.id.clone(), incarnation.clone()),
                RouteNetworkPin {
                    network: "lost-a".into(),
                    network_epoch: 1,
                    confirmed: true,
                },
            );
        }

        let missing = ControlMessage::Route(RouteControl::MissingRoute {
            route_id: route.id.clone(),
            incarnation: incarnation.clone(),
        });
        assert!(mesh.inbound_route_control_path_ok("peer", &missing, "surviving-b"));
        assert!(!mesh.inbound_route_control_path_ok(
            "peer",
            &ControlMessage::Route(RouteControl::Teardown {
                route_id: route.id.clone(),
                incarnation: incarnation.clone(),
            }),
            "surviving-b",
        ));
        assert!(!mesh.inbound_route_control_path_ok(
            "peer",
            &ControlMessage::Route(RouteControl::MissingRoute {
                route_id: route.id.clone(),
                incarnation: Some("11:0".into()),
            }),
            "surviving-b",
        ));
        assert_eq!(
            mesh.state
                .lock()
                .route_networks
                .get(&(route.id.clone(), incarnation.clone()))
                .map(|pin| pin.network.as_str()),
            Some("lost-a"),
            "the recovery request must not move the predecessor pin"
        );

        mesh.state
            .lock()
            .route_networks
            .remove(&(route.id.clone(), incarnation));
        let candidates = mesh.route_network_candidates("peer", &missing);
        assert_eq!(candidates.first().map(String::as_str), Some("surviving-b"));
    }

    #[tokio::test]
    async fn retiring_predecessor_network_pin_cannot_teardown_same_id_successor() {
        let client = Arc::new(ControlClient::new().expect("resolve control socket path"));
        let mesh = Mesh::new(client, Arc::new(NoopSink));
        let route = term_route("me:terminal", "peer:term-view:1", MediaKind::Generic);
        let predecessor = Some("11:1".to_string());
        let successor = Some("11:2".to_string());
        let mut session = Session::new("me");
        let _ = session.offer_terminal_with_incarnation(
            route.clone(),
            "peer",
            Vec::new(),
            Vec::new(),
            None,
            successor.clone(),
        );
        let _ = session.handle(
            NodeId::from("peer"),
            ControlMessage::Route(RouteControl::Accept {
                route_id: route.id.clone(),
                incarnation: successor.clone(),
                session: None,
            }),
        );
        {
            let mut state = mesh.state.lock();
            state.networks = vec!["surviving-b".into()];
            state.network_epochs.insert("surviving-b".into(), 2);
            state.session = Some(session);
            state.route_networks.insert(
                (route.id.clone(), predecessor.clone()),
                RouteNetworkPin {
                    network: "lost-a".into(),
                    network_epoch: 1,
                    confirmed: true,
                },
            );
        }
        mesh.active_media_incarnations
            .lock()
            .insert(route.id.clone(), successor.clone());

        let (replay, missing) = mesh.retire_unjoined_route_paths().await;
        assert!(replay.is_empty());
        assert!(missing.is_empty());
        let state = mesh.state.lock();
        let live = state.session.as_ref().unwrap().route(&route.id).unwrap();
        assert_eq!(live.incarnation, successor);
        assert_eq!(live.state, RouteState::Active);
        assert!(!state
            .route_networks
            .contains_key(&(route.id.clone(), predecessor)));
        drop(state);
        assert_eq!(
            mesh.active_media_incarnations.lock().get(&route.id),
            Some(&Some("11:2".into()))
        );
    }

    #[test]
    fn seed_peer_networks_fills_gaps_for_reachable_peers_only() {
        use serde_json::json;
        let mut map: HashMap<String, PeerNetworkState> = HashMap::new();
        // An inbound frame already proved this peer reachable on the fleet mesh —
        // that mapping carries traffic to us and must survive the peer-list seed.
        map.insert(
            "alice".into(),
            PeerNetworkState {
                preferred: Some("fleet".into()),
                observed_reachable: ["fleet".to_string()].into_iter().collect(),
                ..PeerNetworkState::default()
            },
        );
        let peers = vec![
            // alice is also listed on the public mesh, but her proven mapping stands.
            json!({ "device_id": "alice-AB12C", "status": "active" }),
            // bob is reachable here and unknown to us → learns this network instead
            // of falling back to the primary (the bug: a secondary-only peer shows
            // online + wireable yet every frame went to the wrong mesh).
            json!({ "device_id": "bob-9Z8Y7", "status": "active" }),
            // shelved keeps its data channel open, so it is reachable too.
            json!({ "device_id": "carol", "status": "shelved" }),
            // not reachable yet → no mapping (addressing it now would mis-route).
            json!({ "device_id": "dave", "status": "handshaking" }),
            json!({ "device_id": "erin", "status": "offline" }),
        ];
        seed_peer_networks(&mut map, &peers, "public");
        // Proven inbound mapping is never clobbered…
        assert_eq!(
            map.get("alice")
                .and_then(|paths| paths.preferred.as_deref()),
            Some("fleet")
        );
        assert!(map["alice"].daemon_reachable.contains("public"));
        // …a gap is filled, keyed by canonical pubkey (suffix stripped)…
        assert!(map["bob"].daemon_reachable.contains("public"));
        assert!(map["carol"].daemon_reachable.contains("public"));
        // …and an unreachable peer claims no slot.
        assert_eq!(map.get("dave"), None);
        assert_eq!(map.get("erin"), None);
    }

    #[test]
    fn seed_peer_links_classifies_and_keeps_on_unknown() {
        use crate::video::LinkClass;
        use serde_json::json;
        let mut map: HashMap<(String, String), LinkClass> = HashMap::new();
        // First sighting: host↔host is LAN, anything reflexive/relayed is WAN.
        let peers = vec![
            json!({ "device_id": "alice-AB12C",
                    "selected_pair": { "local": "host", "remote": "host" } }),
            json!({ "device_id": "bob",
                    "selected_pair": { "local": "host", "remote": "server_reflexive" } }),
            json!({ "device_id": "carol",
                    "selected_pair": { "local": "relay", "remote": "host" } }),
            // ICE not settled (null pair) and an old daemon (field absent):
            // both stay unclassified.
            json!({ "device_id": "dave", "selected_pair": null }),
            json!({ "device_id": "erin" }),
        ];
        let changed = seed_peer_links(&mut map, &peers, "public");
        assert_eq!(
            map.get(&("public".into(), "alice".into())),
            Some(&LinkClass::Lan)
        );
        assert_eq!(
            map.get(&("public".into(), "bob".into())),
            Some(&LinkClass::Wan)
        );
        assert_eq!(
            map.get(&("public".into(), "carol".into())),
            Some(&LinkClass::Wan)
        );
        assert_eq!(map.get(&("public".into(), "dave".into())), None);
        assert_eq!(map.get(&("public".into(), "erin".into())), None);
        assert_eq!(
            changed.len(),
            3,
            "every first classification reports as a change"
        );

        // A transient unknown (the daemon clears the pair on an ICE blip)
        // must KEEP the learned class — never downgrade a stream on a wobble.
        let blip = vec![json!({ "device_id": "alice-AB12C", "selected_pair": null })];
        let changed = seed_peer_links(&mut map, &blip, "public");
        assert!(changed.is_empty());
        assert_eq!(
            map.get(&("public".into(), "alice".into())),
            Some(&LinkClass::Lan)
        );

        // A real reclassification (ICE-restart handoff LAN→STUN) reports the
        // change exactly once; a steady-state repeat reports nothing.
        let handoff = vec![json!({ "device_id": "alice-AB12C",
                "selected_pair": { "local": "host", "remote": "peer_reflexive" } })];
        let changed = seed_peer_links(&mut map, &handoff, "public");
        assert_eq!(changed, vec![("alice".to_string(), LinkClass::Wan)]);
        assert!(seed_peer_links(&mut map, &handoff, "public").is_empty());
    }

    #[test]
    fn loopback_terminal_route_is_recognized_as_self_hosted() {
        // The id the front-end mints for "open a terminal to the machine I'm
        // sitting at": both endpoints are this node, source is `…:terminal`.
        let me = "me";
        let route = term_route(
            &format!("{me}:terminal"),
            &format!("{me}:term-view:1"),
            MediaKind::Generic,
        );
        // It's a terminal route…
        assert!(is_terminal_route(&route));
        // …and the loopback predicate the new branch keys on (both ends are
        // this node) holds — so `start_media` takes the loopback path and
        // `term_send` short-circuits input/resize to the local PTY rather
        // than framing it to a peer.
        let from_node = node_of(route.from.as_str());
        let to_node = node_of(route.to.as_str());
        assert_eq!(from_node, me);
        assert_eq!(to_node, me);
        assert!(
            from_node == me && to_node == me,
            "a self-terminal is a loopback route"
        );

        // A remote terminal (viewer here, shell elsewhere) is NOT loopback —
        // it keeps the framed-to-peer path.
        let remote = term_route(
            "host:terminal",
            &format!("{me}:term-view:2"),
            MediaKind::Generic,
        );
        assert!(is_terminal_route(&remote));
        assert_ne!(node_of(remote.from.as_str()), node_of(remote.to.as_str()));
    }

    #[test]
    fn loopback_is_detected_across_node_id_forms() {
        // The regression that broke local terminals: the front-end builds the
        // route from the *display* id (`<pubkey>-ab3d9`) while the backend's
        // `me` is the *bare* node id (`<pubkey>`). A raw `==` sees them as
        // different machines and tries to offer the local terminal over the
        // wire, where it never comes back. `same_node` compares canonically.
        let me = "k7pubkeybody";
        let display = format!("{me}-ab3d9"); // what the UI mints ids from
        let from = node_of(&format!("{display}:terminal"));
        let to = node_of(&format!("{display}:term-view:1"));
        // Raw equality misses it (the suffix differs)…
        assert_ne!(from, me);
        // …but the canonical self-check the loopback branches now use holds.
        assert!(same_node(&from, me) && same_node(&to, me));

        // A genuinely remote terminal stays non-loopback under the same check.
        let host = node_of("otherpubkey-99xyz:terminal");
        assert!(!same_node(&host, me));
    }

    #[test]
    fn term_send_loopback_check_is_canonical_across_id_forms() {
        // `term_send` decides "is this a terminal to my own machine?" so input
        // (incl. xterm's ConPTY cursor-position reply) goes to the local PTY
        // instead of being framed to a peer. The realistic mixed-form case the
        // bug hit: the UI builds the *host* endpoint from the node-list display
        // id (`<pubkey>-ab3d9:terminal`) but the *viewer* endpoint from
        // `localId`, which equals the backend's bare `me`. A raw `==` on the
        // source then read the loopback as remote and framed the reply to a
        // non-existent peer — leaving a ConPTY shell blank on Windows, where no
        // output flows until that reply arrives.
        let me = "k7pubkeybody";
        let display = format!("{me}-ab3d9");
        let route = term_route(
            &format!("{display}:terminal"),
            &format!("{me}:term-view:abc-1"), // built from localId == me
            MediaKind::Generic,
        );

        // The viewer-side gate (`to` is this machine) passes either way…
        assert!(same_node(&node_of(route.to.as_str()), me));

        // …but the loopback flag keys on the *source*, where the forms differ:
        // the raw `==` the fix replaces misses it; `same_node` catches it, so
        // input short-circuits to the local PTY.
        assert_ne!(
            node_of(route.from.as_str()),
            me,
            "raw == missed the self-route"
        );
        assert!(
            same_node(&node_of(route.from.as_str()), me),
            "canonical check recognises the loopback source"
        );

        // A genuinely remote terminal (shell elsewhere) stays non-loopback, so
        // its input is still framed to the host over the mesh.
        let remote = term_route(
            "otherpubkey-99xyz:terminal",
            &format!("{me}:term-view:abc-2"),
            MediaKind::Generic,
        );
        assert!(
            same_node(&node_of(remote.to.as_str()), me),
            "we're the viewer"
        );
        assert!(
            !same_node(&node_of(remote.from.as_str()), me),
            "a remote shell is not a loopback source"
        );
    }

    #[test]
    fn video_lanes_pin_distinct_per_peer_and_reuse_when_freed() {
        use std::collections::HashMap;
        let mut pins: HashMap<String, OutboundVideoLanePin> = HashMap::new();
        let network = "network-a";
        let r0 = "route:host:screen:0→viewerkey-ab3d9:sink".to_string();
        let r1 = "route:host:screen:1→viewerkey-ab3d9:sink".to_string();
        let cap = 8;

        // First screen to this viewer takes lane 0…
        let l0 = free_lane_for_peer(&pins, network, "viewerkey", &r0, cap).unwrap();
        pins.insert(
            r0.clone(),
            OutboundVideoLanePin {
                network: network.into(),
                lane: l0,
            },
        );
        // …the second can NOT reuse it — it must get a fresh lane.
        let l1 = free_lane_for_peer(&pins, network, "viewerkey", &r1, cap).unwrap();
        pins.insert(
            r1.clone(),
            OutboundVideoLanePin {
                network: network.into(),
                lane: l1,
            },
        );
        assert_ne!(l0, l1, "two screens to one viewer never share a lane");
        assert_eq!((l0, l1), (0, 1));

        // Asking again for an already-pinned route returns its pin (idempotent).
        assert_eq!(
            free_lane_for_peer(&pins, network, "viewerkey", &r0, cap),
            Some(0)
        );

        // A route to a DIFFERENT viewer is independent — it can reuse lane 0.
        let other = "route:host:screen:0→otherkey-77zzz:sink".to_string();
        assert_eq!(
            free_lane_for_peer(&pins, network, "otherkey", &other, cap),
            Some(0)
        );

        // The same peer owns an independent lane pool in another PeerSession.
        let other_network = "network-b";
        let r_other_network = "route:host:screen:3→viewerkey-ab3d9:sink".to_string();
        assert_eq!(
            free_lane_for_peer(&pins, other_network, "viewerkey", &r_other_network, cap,),
            Some(0)
        );

        // Freeing the first screen's pin lets the next route reuse lane 0.
        pins.remove(&r0);
        let r2 = "route:host:screen:2→viewerkey-ab3d9:sink".to_string();
        assert_eq!(
            free_lane_for_peer(&pins, network, "viewerkey", &r2, cap),
            Some(0)
        );

        // A full pool yields None (the extra stream falls back to MJPEG).
        let mut full: HashMap<String, OutboundVideoLanePin> = HashMap::new();
        for l in 0..2u8 {
            full.insert(
                format!("route:host:screen:{l}→viewerkey-ab3d9:sink"),
                OutboundVideoLanePin {
                    network: network.into(),
                    lane: l,
                },
            );
        }
        let r_extra = "route:host:screen:9→viewerkey-ab3d9:sink".to_string();
        assert_eq!(
            free_lane_for_peer(&full, network, "viewerkey", &r_extra, 2),
            None
        );
    }

    #[test]
    fn input_sequence_is_once_per_exact_route_lifetime() {
        let mut sequences = HashMap::new();
        let first = Some("boot-a:1".to_string());
        let successor = Some("boot-a:2".to_string());

        assert!(accept_input_sequence(
            &mut sequences,
            "route:input",
            &first,
            0
        ));
        assert!(!accept_input_sequence(
            &mut sequences,
            "route:input",
            &first,
            0
        ));
        assert!(accept_input_sequence(
            &mut sequences,
            "route:input",
            &first,
            1
        ));
        assert!(!accept_input_sequence(
            &mut sequences,
            "route:input",
            &first,
            0
        ));

        // A same-id successor has its own sequence domain and may restart at
        // zero without reopening the predecessor's duplicates.
        assert!(accept_input_sequence(
            &mut sequences,
            "route:input",
            &successor,
            0
        ));
        assert!(!accept_input_sequence(
            &mut sequences,
            "route:input",
            &first,
            1
        ));
    }

    #[test]
    fn files_routes_are_recognized_by_shape() {
        let files = term_route("host:files", "me:files-view:1", MediaKind::Generic);
        assert!(is_files_route(&files));
        assert!(!is_terminal_route(&files), "files ≠ terminal");

        let generic = term_route("host:thing", "me:other", MediaKind::Generic);
        assert!(!is_files_route(&generic));

        let storage = term_route("host:files", "me:files-view:1", MediaKind::Storage);
        assert!(!is_files_route(&storage), "media is part of the contract");
    }

    #[test]
    fn shared_routes_are_recognized_and_distinct_from_files() {
        let shared = term_route("host:shared", "me:shared-view:1", MediaKind::Generic);
        assert!(is_shared_route(&shared));
        // A shared route is *not* a files route — that's the whole point:
        // it skips the owner/fleet offer screen and is fetch-by-token only.
        assert!(!is_files_route(&shared));
        assert!(!is_terminal_route(&shared));

        let files = term_route("host:files", "me:files-view:1", MediaKind::Generic);
        assert!(!is_shared_route(&files));

        // The media is part of the contract here too.
        let storage = term_route("host:shared", "me:shared-view:1", MediaKind::Storage);
        assert!(!is_shared_route(&storage));
    }

    #[test]
    fn capability_ids_split_into_node_and_device() {
        // The device part keeps its own colons — a camera route resolves
        // `<node>:cam:video0` back to the inventory id `cam:video0`, the
        // display arm reads `screen:<id>` the same way.
        assert_eq!(node_of("desk:cam:video0"), "desk");
        assert_eq!(device_of("desk:cam:video0").as_deref(), Some("cam:video0"));
        assert_eq!(device_of("desk:screen:7").as_deref(), Some("screen:7"));
        assert_eq!(device_of("desk:screen").as_deref(), Some("screen"));
        // A bare node id has no device half.
        assert_eq!(device_of("desk"), None);
        assert_eq!(node_of("desk"), "desk");
    }

    #[test]
    fn negotiated_video_mode_overrides_an_unsupported_lossless_request() {
        assert_eq!(
            resolved_encoder_mode(Some("studio-lossless"), Some(MediaMode::Studio)),
            Some("studio")
        );
        assert_eq!(
            resolved_encoder_mode(Some("studio-lossless"), Some(MediaMode::StudioLossless)),
            Some("studio-lossless")
        );
        assert_eq!(
            resolved_encoder_mode(Some("game"), None),
            Some("game"),
            "legacy peers without a policy plan retain the requested posture"
        );
    }

    #[test]
    fn privileged_offers_are_refused_exactly_when_unauthorized() {
        let term = term_route("me:terminal", "them:term-view:1", MediaKind::Generic);
        let files = term_route("me:files", "them:files-view:1", MediaKind::Generic);

        // Our shell/disk + an unauthorized sender = refusal with a human
        // reason naming the right plane.
        let refusal = privileged_offer_refusal(&term, true, false);
        assert!(refusal.is_some_and(|r| r.contains("terminal") && r.contains("owner/fleet")));
        let refusal = privileged_offer_refusal(&files, true, false);
        assert!(refusal.is_some_and(|r| r.contains("file") && r.contains("owner/fleet")));

        // Owner/fleet senders pass.
        assert_eq!(privileged_offer_refusal(&term, true, true), None);
        assert_eq!(privileged_offer_refusal(&files, true, true), None);

        // An offer that doesn't ask us to host (we'd be the viewer) is no
        // grounds for refusal…
        assert_eq!(privileged_offer_refusal(&term, false, false), None);
        assert_eq!(privileged_offer_refusal(&files, false, false), None);

        // …and unprivileged offers are never screened here, whoever asks.
        let audio = term_route("me:mic", "them:speaker", MediaKind::Audio);
        assert_eq!(privileged_offer_refusal(&audio, true, false), None);

        // A Shared Files (`:shared`) offer is deliberately *not* screened —
        // any room member opens one, and the per-fetch token gate (not the
        // owner/fleet rule) is what keeps it to explicitly-shared files.
        let shared = term_route("me:shared", "them:shared-view:1", MediaKind::Generic);
        assert_eq!(privileged_offer_refusal(&shared, true, false), None);
    }

    #[test]
    fn share_grants_authorize_exactly_their_own_plane() {
        use allmystuff_graph::GrantRole;
        let g = |media: MediaKind, role: GrantRole, cap: &str| Grant {
            id: "g".into(),
            media,
            role,
            capability: Some(cap.into()),
            label: String::new(),
        };

        // A control grant injects — and opens neither a shell, the disk, nor
        // anything else.
        let control = g(MediaKind::Input, GrantRole::Consume, "me:control");
        assert!(grant_authorizes_plane(&control, DrivePlane::Input));
        for p in [
            DrivePlane::Terminal,
            DrivePlane::Files,
            DrivePlane::Sites,
            DrivePlane::Clipboard,
        ] {
            assert!(
                !grant_authorizes_plane(&control, p),
                "control leaked to {p:?}"
            );
        }

        // Terminal and Sites are both Generic grants — the capability suffix is
        // what tells them apart, so neither is mistaken for the other.
        let terminal = g(MediaKind::Generic, GrantRole::Provide, "me:terminal");
        assert!(grant_authorizes_plane(&terminal, DrivePlane::Terminal));
        assert!(!grant_authorizes_plane(&terminal, DrivePlane::Sites));
        let sites = g(MediaKind::Generic, GrantRole::Provide, "me:sites");
        assert!(grant_authorizes_plane(&sites, DrivePlane::Sites));
        assert!(!grant_authorizes_plane(&sites, DrivePlane::Terminal));

        // Files is a storage grant; clipboard its own kind.
        let files = g(MediaKind::Storage, GrantRole::Both, "me:files");
        assert!(grant_authorizes_plane(&files, DrivePlane::Files));
        assert!(!grant_authorizes_plane(&files, DrivePlane::Input));
        let clip = g(MediaKind::Clipboard, GrantRole::Both, "me:clipboard");
        assert!(grant_authorizes_plane(&clip, DrivePlane::Clipboard));

        // A screen grant (watch only) authorizes NO privileged plane — sharing
        // a screen never hands over control, a shell, the disk, or the
        // clipboard.
        let screen = g(MediaKind::Display, GrantRole::Provide, "me:screen");
        for p in [
            DrivePlane::Input,
            DrivePlane::Terminal,
            DrivePlane::Files,
            DrivePlane::Sites,
            DrivePlane::Clipboard,
        ] {
            assert!(
                !grant_authorizes_plane(&screen, p),
                "screen leaked to {p:?}"
            );
        }

        // route_drive_plane classifies exactly the privileged routes.
        assert_eq!(
            route_drive_plane(&term_route("me:terminal", "them:tv:1", MediaKind::Generic)),
            Some(DrivePlane::Terminal)
        );
        assert_eq!(
            route_drive_plane(&term_route("me:files", "them:fv:1", MediaKind::Generic)),
            Some(DrivePlane::Files)
        );
        assert_eq!(
            route_drive_plane(&term_route("me:site", "them:sv:1", MediaKind::Generic)),
            Some(DrivePlane::Sites)
        );
        assert_eq!(
            route_drive_plane(&term_route("me:mic", "them:speaker", MediaKind::Audio)),
            None
        );
    }

    #[test]
    fn refresh_backoff_interval_grows_each_minute_then_caps() {
        use std::time::Duration;
        // 5 s through the first minute, doubling each further minute up to a
        // 60 s ceiling.
        assert_eq!(profile_req_interval(Duration::ZERO), Duration::from_secs(5));
        assert_eq!(
            profile_req_interval(Duration::from_secs(59)),
            Duration::from_secs(5)
        );
        assert_eq!(
            profile_req_interval(Duration::from_secs(60)),
            Duration::from_secs(10)
        );
        assert_eq!(
            profile_req_interval(Duration::from_secs(120)),
            Duration::from_secs(20)
        );
        assert_eq!(
            profile_req_interval(Duration::from_secs(180)),
            Duration::from_secs(40)
        );
        assert_eq!(
            profile_req_interval(Duration::from_secs(240)),
            Duration::from_secs(60)
        );
        assert_eq!(
            profile_req_interval(Duration::from_secs(3600)),
            Duration::from_secs(60)
        );
        // The ceiling is first reached at the 4-minute mark.
        assert_eq!(profile_req_cap_reached(), Duration::from_secs(240));
    }

    #[test]
    fn refresh_backoff_spaces_requests_and_resets_when_idle() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let at = |secs: u64| t0 + Duration::from_secs(secs);

        // The first request is always allowed.
        let (allow, st) = profile_req_decide(None, t0);
        assert!(allow);

        // A second within the 5 s floor is refused…
        let (allow, st) = profile_req_decide(Some(st), at(3));
        assert!(!allow);
        // …and allowed once the floor passes.
        let (allow, st) = profile_req_decide(Some(st), at(5));
        assert!(allow);

        // Five minutes into a sustained burst the floor has grown to the 60 s
        // ceiling: a request 295 s after the last is fine, but 30 s later is not.
        let (allow, st) = profile_req_decide(Some(st), at(300));
        assert!(allow);
        let (allow, st) = profile_req_decide(Some(st), at(330));
        assert!(!allow);

        // A five-minute quiet spell resets the envelope back to the fast floor.
        let (allow, st) = profile_req_decide(Some(st), at(300 + 5 * 60));
        assert!(allow);
        let base = 300 + 5 * 60;
        let (allow, st) = profile_req_decide(Some(st), at(base + 3));
        assert!(!allow); // 3 s — back under the 5 s floor
        let (allow, _) = profile_req_decide(Some(st), at(base + 5));
        assert!(allow);
    }

    #[test]
    fn fresh_share_tokens_are_unguessable_and_unique() {
        let a = fresh_share_token();
        let b = fresh_share_token();
        assert!(a.starts_with("share_"));
        assert_ne!(a, b, "tokens must not collide");
        // 16 random bytes as hex, after the `share_` prefix.
        assert_eq!(a.len(), "share_".len() + 32);
        assert!(a["share_".len()..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn unique_path_counts_the_finder_way() {
        let dir = std::env::temp_dir().join(format!(
            "amst-unique-test-{}-{}",
            std::process::id(),
            fresh_boot_id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a.txt"));
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a (2).txt"));
        std::fs::write(dir.join("a (2).txt"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a (3).txt"));
        std::fs::write(dir.join("noext"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "noext"), dir.join("noext (2)"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn system_audio_routes_capture_the_machines_own_output() {
        // The synthetic `system-audio` capability = "what this machine
        // plays" — its routes loop the output back…
        let system = term_route("me:system-audio", "them:system-audio", MediaKind::Audio);
        assert_eq!(audio_capture_source(&system), CaptureSource::System);

        // …while a scanned input device (and anything unrecognized,
        // including a bare node id) captures the mic, exactly as before.
        let mic = term_route("me:mic:array-1", "them:system-audio", MediaKind::Audio);
        assert_eq!(audio_capture_source(&mic), CaptureSource::Mic);
        let bare = term_route("me", "them:system-audio", MediaKind::Audio);
        assert_eq!(audio_capture_source(&bare), CaptureSource::Mic);
    }
}

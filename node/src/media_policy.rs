//! Versioned application-side media policy.
//!
//! This module deliberately has no transport dependency.  A policy message is
//! inserted into the already-existing opaque `RouteControl::Tune.ext` only
//! after a route is active.  Media bytes continue to use the negotiated media
//! lanes over the established ICE/STUN/TURN path.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MEDIA_POLICY_VERSION: u16 = 1;
pub const MEDIA_POLICY_EXT_KEY: &str = "media_policy";
pub const VIDEO_HANDOFF_FRAMES: u8 = 1;
pub const AUDIO_HANDOFF_PACKETS: u8 = 3;
/// A receiver estimate is a short-lived congestion observation, never a
/// persistent user dial. Feedback normally arrives every two seconds; ten
/// seconds tolerates ordinary scheduling gaps while bounding stale caps.
pub const PATH_ESTIMATE_MAX_AGE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaMode {
    Reach,
    #[default]
    Balanced,
    Game,
    Studio,
    StudioLossless,
}

impl MediaMode {
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Reach => "reach",
            Self::Balanced => "balanced",
            Self::Game => "game",
            Self::Studio => "studio",
            Self::StudioLossless => "studio-lossless",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "reach" => Some(Self::Reach),
            "balanced" => Some(Self::Balanced),
            "game" => Some(Self::Game),
            "studio" => Some(Self::Studio),
            "studio-lossless" => Some(Self::StudioLossless),
            _ => None,
        }
    }

    pub const fn contract(self, lan: bool) -> ModeContract {
        match self {
            Self::Reach => ModeContract {
                aggregate_bps: 1_000_000,
                route_ceiling_bps: 850_000,
                fps: if lan { 30 } else { 15 },
                audio_bps: 48_000,
                audio_packet_ms: 20,
                audio_jitter_ms: 60,
                audio_fec: true,
                auto_rate: true,
                auto_resolution: true,
                background_weight: 1,
                viable_video_floor_bps: 120_000,
            },
            Self::Balanced => ModeContract {
                aggregate_bps: if lan { 80_000_000 } else { 25_000_000 },
                route_ceiling_bps: if lan { 80_000_000 } else { 25_000_000 },
                fps: if lan { 60 } else { 30 },
                audio_bps: 96_000,
                audio_packet_ms: 10,
                audio_jitter_ms: 40,
                audio_fec: true,
                auto_rate: true,
                auto_resolution: true,
                background_weight: 2,
                viable_video_floor_bps: 1_000_000,
            },
            Self::Game => ModeContract {
                aggregate_bps: if lan { 200_000_000 } else { 60_000_000 },
                route_ceiling_bps: if lan { 200_000_000 } else { 60_000_000 },
                fps: 60,
                audio_bps: 128_000,
                audio_packet_ms: 5,
                audio_jitter_ms: 20,
                audio_fec: false,
                auto_rate: true,
                auto_resolution: true,
                background_weight: 1,
                viable_video_floor_bps: 1_000_000,
            },
            Self::Studio | Self::StudioLossless => ModeContract {
                aggregate_bps: if lan { 500_000_000 } else { 150_000_000 },
                route_ceiling_bps: if lan { 500_000_000 } else { 150_000_000 },
                fps: 60,
                audio_bps: 192_000,
                audio_packet_ms: 10,
                audio_jitter_ms: 40,
                audio_fec: false,
                auto_rate: false,
                auto_resolution: false,
                background_weight: 3,
                viable_video_floor_bps: 2_000_000,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeContract {
    pub aggregate_bps: u64,
    pub route_ceiling_bps: u64,
    pub fps: u32,
    pub audio_bps: u32,
    pub audio_packet_ms: u16,
    pub audio_jitter_ms: u16,
    pub audio_fec: bool,
    pub auto_rate: bool,
    pub auto_resolution: bool,
    pub background_weight: u32,
    pub viable_video_floor_bps: u64,
}

impl ModeContract {
    /// Reserve encoded audio plus packet/crypto/transport headroom before
    /// allocating a single bit to video.  Reach intentionally keeps a larger
    /// proportional reserve so speech survives a genuinely narrow path.
    pub const fn audio_reserve_bps(self) -> u64 {
        let headroom = if self.audio_packet_ms <= 5 {
            48_000
        } else if self.audio_packet_ms <= 10 {
            32_000
        } else {
            16_000
        };
        self.audio_bps as u64 + headroom
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaCapabilities {
    pub policy_v1: bool,
    pub h264: bool,
    pub hevc: bool,
    pub opus: bool,
    pub native_h264_decode: bool,
    pub native_hevc_decode: bool,
    pub binary_media_pipes: bool,
    pub source_exact_444: bool,
    pub lossless_audio: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyRequest {
    pub mode: MediaMode,
    /// Aggregate cap for all media to this peer. `None` leaves the peer-wide
    /// choice unchanged; `Some(0)` explicitly returns it to the mode/path
    /// contract. This is intentionally distinct from `route_cap_bps`.
    pub peer_cap_bps: Option<u64>,
    /// An explicit ceiling for this one video route.
    pub route_cap_bps: Option<u64>,
    /// A focus election, not a request for an additional transport lane.
    pub priority: bool,
    /// This envelope changes only priority. Legacy Tune fields are repeated
    /// for an older peer's benefit, but a v1 peer must not interpret them as
    /// a quality update.
    pub priority_only: bool,
    pub source_exact_video: bool,
    pub lossless_audio: bool,
}

impl Default for PolicyRequest {
    fn default() -> Self {
        Self {
            mode: MediaMode::Balanced,
            peer_cap_bps: None,
            route_cap_bps: None,
            priority: false,
            priority_only: false,
            source_exact_video: false,
            lossless_audio: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EffectivePlan {
    pub route_id: String,
    pub requested_mode: MediaMode,
    pub effective_mode: MediaMode,
    pub aggregate_budget_bps: u64,
    /// Number of simultaneous encoded-audio routes charged to this peer.
    /// Until the media owner opts into explicit route accounting the
    /// controller conservatively reserves one stream, preserving the v1
    /// behavior and preventing video from consuming the audio headroom.
    pub audio_route_count: u32,
    pub audio_reserved_bps: u64,
    pub video_pool_bps: u64,
    pub route_budget_bps: u64,
    pub route_ceiling_bps: u64,
    pub priority: bool,
    pub fps: u32,
    pub video_queue_frames: u8,
    pub audio_queue_packets: u8,
    pub audio_bps: u32,
    pub audio_packet_ms: u16,
    pub audio_jitter_ms: u16,
    pub audio_fec: bool,
    pub auto_rate: bool,
    pub auto_resolution: bool,
    pub encoder: String,
    pub codec: String,
    pub source_exact_video: bool,
    pub lossless_audio: bool,
    pub degradation_reasons: Vec<String>,
}

impl Default for EffectivePlan {
    fn default() -> Self {
        Self {
            route_id: String::new(),
            requested_mode: MediaMode::Balanced,
            effective_mode: MediaMode::Balanced,
            aggregate_budget_bps: 0,
            audio_route_count: 0,
            audio_reserved_bps: 0,
            video_pool_bps: 0,
            route_budget_bps: 0,
            route_ceiling_bps: 0,
            priority: false,
            fps: 0,
            video_queue_frames: VIDEO_HANDOFF_FRAMES,
            audio_queue_packets: AUDIO_HANDOFF_PACKETS,
            audio_bps: 0,
            audio_packet_ms: 0,
            audio_jitter_ms: 0,
            audio_fec: false,
            auto_rate: false,
            auto_resolution: false,
            encoder: String::new(),
            codec: String::new(),
            source_exact_video: false,
            lossless_audio: false,
            degradation_reasons: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PolicyPayload {
    Request {
        route_id: String,
        request: PolicyRequest,
        capabilities: MediaCapabilities,
    },
    Effective {
        plan: EffectivePlan,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyEnvelope {
    pub version: u16,
    #[serde(flatten)]
    pub payload: PolicyPayload,
}

impl PolicyEnvelope {
    pub fn request(
        route_id: impl Into<String>,
        request: PolicyRequest,
        capabilities: MediaCapabilities,
    ) -> Self {
        Self {
            version: MEDIA_POLICY_VERSION,
            payload: PolicyPayload::Request {
                route_id: route_id.into(),
                request,
                capabilities,
            },
        }
    }

    pub fn effective(plan: EffectivePlan) -> Self {
        Self {
            version: MEDIA_POLICY_VERSION,
            payload: PolicyPayload::Effective { plan },
        }
    }

    /// Read only the namespaced v1 value.  Missing, malformed, and future
    /// versions all fail soft so an extension can never tear down a stream.
    pub fn from_ext(ext: &Value) -> Option<Self> {
        let raw = ext.as_object()?.get(MEDIA_POLICY_EXT_KEY)?;
        if raw.get("version")?.as_u64()? != MEDIA_POLICY_VERSION as u64 {
            return None;
        }
        serde_json::from_value(raw.clone()).ok()
    }

    /// Merge with another pipeline's opaque keys rather than replacing them.
    pub fn into_ext(&self, ext: Value) -> Value {
        let mut root = ext.as_object().cloned().unwrap_or_default();
        let value = serde_json::to_value(self).unwrap_or(Value::Null);
        root.insert(MEDIA_POLICY_EXT_KEY.to_string(), value);
        Value::Object(root)
    }
}

#[derive(Debug, Clone)]
struct RouteState {
    request: PolicyRequest,
    capabilities: MediaCapabilities,
}

#[derive(Debug, Clone, Default)]
struct PeerState {
    routes: BTreeMap<String, RouteState>,
    audio_routes: BTreeSet<String>,
    /// False means the audio owner has not supplied lifecycle information,
    /// so one conservative reservation remains in force for compatibility.
    audio_accounting_explicit: bool,
    priority: Option<String>,
    /// The mode that owns the peer-wide automatic envelope. Focus only
    /// changes scheduling priority; it must never silently change this dial.
    aggregate_mode: MediaMode,
    aggregate_override_bps: Option<u64>,
    path_estimate_bps: Option<u64>,
    path_estimate_at: Option<Instant>,
    lan: bool,
}

/// Pure peer-wide allocator state. `Mesh` owns synchronization; keeping this
/// type free of async/transport code makes every budget invariant unit-testable.
#[derive(Debug, Default)]
pub struct MediaPolicyController {
    peers: HashMap<String, PeerState>,
    route_peer: HashMap<String, String>,
    audio_route_peer: HashMap<String, String>,
    effective: HashMap<String, EffectivePlan>,
}

impl MediaPolicyController {
    pub fn register_route(&mut self, peer: &str, route_id: &str, lan: bool) -> Vec<EffectivePlan> {
        let mut changed = Vec::new();
        if let Some(old_peer) = self
            .route_peer
            .insert(route_id.to_string(), peer.to_string())
        {
            if old_peer != peer {
                let mut remove_old_peer = false;
                if let Some(old) = self.peers.get_mut(&old_peer) {
                    old.routes.remove(route_id);
                    if old.priority.as_deref() == Some(route_id) {
                        old.priority = None;
                    }
                    remove_old_peer = old.routes.is_empty() && old.audio_routes.is_empty();
                }
                if remove_old_peer {
                    self.peers.remove(&old_peer);
                } else {
                    changed.extend(self.recompute(&old_peer));
                }
            }
        }
        let state = self.peers.entry(peer.to_string()).or_default();
        state.lan = lan;
        state
            .routes
            .entry(route_id.to_string())
            .or_insert(RouteState {
                request: PolicyRequest::default(),
                capabilities: MediaCapabilities::default(),
            });
        if state.priority.is_none() {
            state.priority = Some(route_id.to_string());
        }
        changed.extend(self.recompute(peer));
        changed
    }

    pub fn remove_route(&mut self, route_id: &str) -> Vec<EffectivePlan> {
        let Some(peer) = self.route_peer.remove(route_id) else {
            self.effective.remove(route_id);
            return Vec::new();
        };
        self.effective.remove(route_id);
        let mut remove_peer = false;
        if let Some(state) = self.peers.get_mut(&peer) {
            state.routes.remove(route_id);
            if state.priority.as_deref() == Some(route_id) {
                state.priority = state.routes.keys().next().cloned();
            }
            remove_peer = state.routes.is_empty() && state.audio_routes.is_empty();
        }
        if remove_peer {
            self.peers.remove(&peer);
            Vec::new()
        } else {
            self.recompute(&peer)
        }
    }

    pub fn apply_request(
        &mut self,
        peer: &str,
        route_id: &str,
        mut request: PolicyRequest,
        capabilities: MediaCapabilities,
        lan: bool,
    ) -> Vec<EffectivePlan> {
        if request.priority_only {
            // Focus is valid only for an already registered active route. Do
            // this before registration so even a stale/malicious focus packet
            // cannot move a route between peers or mutate the peer's LAN/path
            // classification, automatic envelope, capabilities, or dials.
            return self.elect_priority(route_id);
        }
        let registration_plans = self.register_route(peer, route_id, lan);
        // Registration may have moved this route from another peer. Preserve
        // those old peer sibling updates in the returned change set; the new
        // peer's provisional plans are recomputed below with this request.
        let mut changed = registration_plans
            .into_iter()
            .filter(|plan| {
                self.route_peer
                    .get(&plan.route_id)
                    .is_some_and(|owner| owner != peer)
            })
            .collect::<Vec<_>>();
        let state = self.peers.get_mut(peer).expect("registered above");
        // This is a full quality request (priority-only requests use
        // `elect_priority`). Persist its peer-mode choice so a later focus
        // election cannot change the automatic aggregate or audio contract.
        state.aggregate_mode = request.mode;
        if request.priority || state.priority.is_none() {
            state.priority = Some(route_id.to_string());
        }
        // The aggregate is peer state, not route state: an absent value on a
        // new background display must not erase the cap chosen in another
        // window. Zero is the explicit wire representation for
        // "All media: Auto"; absence leaves the aggregate unchanged.
        if let Some(cap) = request.peer_cap_bps {
            state.aggregate_override_bps = (cap > 0).then_some(cap);
        }
        request.peer_cap_bps = state.aggregate_override_bps;
        state.routes.insert(
            route_id.to_string(),
            RouteState {
                request,
                capabilities,
            },
        );
        changed.extend(self.recompute(peer));
        changed
    }

    /// Register one active audio route for peer-wide accounting. The media
    /// owner should call this exactly once on audio start and
    /// [`remove_audio_route`](Self::remove_audio_route) exactly once on stop.
    /// Repeated starts are idempotent, and moving an id to a different peer
    /// removes its old reservation first.
    pub fn register_audio_route(&mut self, peer: &str, route_id: &str) -> Vec<EffectivePlan> {
        if self
            .audio_route_peer
            .get(route_id)
            .is_some_and(|existing| existing == peer)
            && self
                .peers
                .get(peer)
                .is_some_and(|state| state.audio_routes.contains(route_id))
        {
            return Vec::new();
        }
        let mut old_peer_to_recompute = None;
        let mut old_peer_to_remove = None;
        if let Some(old_peer) = self
            .audio_route_peer
            .insert(route_id.to_string(), peer.to_string())
        {
            if old_peer != peer {
                if let Some(old) = self.peers.get_mut(&old_peer) {
                    old.audio_accounting_explicit = true;
                    old.audio_routes.remove(route_id);
                    if old.routes.is_empty() && old.audio_routes.is_empty() {
                        old_peer_to_remove = Some(old_peer.clone());
                    }
                }
                if old_peer_to_remove.is_none() {
                    old_peer_to_recompute = Some(old_peer);
                }
            }
        }

        if let Some(old_peer) = old_peer_to_remove {
            self.peers.remove(&old_peer);
        }

        let state = self.peers.entry(peer.to_string()).or_default();
        state.audio_accounting_explicit = true;
        state.audio_routes.insert(route_id.to_string());

        let mut changed = Vec::new();
        if let Some(old_peer) = old_peer_to_recompute {
            changed.extend(self.recompute(&old_peer));
        }
        changed.extend(self.recompute(peer));
        changed
    }

    /// Remove one active audio route and return the video plans whose budgets
    /// changed. Unknown ids are a no-op. An explicitly empty set reserves no
    /// audio; peers that have not adopted these lifecycle calls retain the
    /// conservative one-stream v1 reservation.
    pub fn remove_audio_route(&mut self, route_id: &str) -> Vec<EffectivePlan> {
        let Some(peer) = self.audio_route_peer.remove(route_id) else {
            return Vec::new();
        };
        let mut remove_peer = false;
        if let Some(state) = self.peers.get_mut(&peer) {
            state.audio_accounting_explicit = true;
            state.audio_routes.remove(route_id);
            remove_peer = state.routes.is_empty() && state.audio_routes.is_empty();
        }
        if remove_peer {
            self.peers.remove(&peer);
            Vec::new()
        } else {
            self.recompute(&peer)
        }
    }

    pub fn elect_priority(&mut self, route_id: &str) -> Vec<EffectivePlan> {
        let Some(peer) = self.route_peer.get(route_id).cloned() else {
            return Vec::new();
        };
        let Some(state) = self.peers.get_mut(&peer) else {
            return Vec::new();
        };
        if state.routes.contains_key(route_id) {
            state.priority = Some(route_id.to_string());
        }
        self.recompute(&peer)
    }

    pub fn note_path_estimate(
        &mut self,
        peer: &str,
        estimate_bps: Option<u64>,
    ) -> Vec<EffectivePlan> {
        let Some(state) = self.peers.get_mut(peer) else {
            return Vec::new();
        };
        let changed = if let Some(sample) = estimate_bps.filter(|v| *v > 0) {
            // Fast cut, slow climb. A falling path estimate takes effect
            // immediately; recovery grows by at most 5% per report so a
            // transient clear spell cannot refill the same standing queue.
            let next = match state.path_estimate_bps {
                None => sample,
                Some(current) if sample < current => sample,
                Some(current) => sample.min(current.saturating_add((current / 20).max(64_000))),
            };
            let changed = state.path_estimate_bps != Some(next);
            state.path_estimate_bps = Some(next);
            state.path_estimate_at = Some(Instant::now());
            changed
        } else {
            // None/zero is an explicit "estimate unavailable" sample,
            // not "repeat the last cap forever".
            let changed = state.path_estimate_bps.is_some();
            state.path_estimate_bps = None;
            state.path_estimate_at = None;
            changed
        };
        if !changed {
            return Vec::new();
        }
        self.recompute(peer)
    }

    /// Expire receiver estimates even when feedback stops entirely. The mesh
    /// owner must call this from an existing periodic maintenance tick and
    /// apply/echo the returned plans just like `note_path_estimate` results.
    pub fn expire_stale_path_estimates(&mut self) -> Vec<EffectivePlan> {
        self.expire_stale_path_estimates_at(Instant::now())
    }

    fn expire_stale_path_estimates_at(&mut self, now: Instant) -> Vec<EffectivePlan> {
        let stale = self
            .peers
            .iter()
            .filter_map(|(peer, state)| {
                let sampled_at = state.path_estimate_at?;
                now.checked_duration_since(sampled_at)
                    .is_some_and(|age| age >= PATH_ESTIMATE_MAX_AGE)
                    .then(|| peer.clone())
            })
            .collect::<Vec<_>>();
        let mut plans = Vec::new();
        for peer in stale {
            if let Some(state) = self.peers.get_mut(&peer) {
                state.path_estimate_bps = None;
                state.path_estimate_at = None;
            }
            plans.extend(self.recompute(&peer));
        }
        plans
    }

    pub fn plan(&self, route_id: &str) -> Option<&EffectivePlan> {
        self.effective.get(route_id)
    }

    /// Snapshot the final effective generation for every video route owned by
    /// `peer`. Callers use this after a compound lifecycle change (for example,
    /// suspending incompatible PCM and then adding a video route) so they never
    /// publish an intermediate allocator generation.
    pub fn plans_for_peer(&self, peer: &str) -> Vec<EffectivePlan> {
        let mut plans = self
            .route_peer
            .iter()
            .filter(|(_, owner)| owner.as_str() == peer)
            .filter_map(|(route_id, _)| self.effective.get(route_id).cloned())
            .collect::<Vec<_>>();
        plans.sort_by(|a, b| a.route_id.cmp(&b.route_id));
        plans
    }

    /// Drop all route, audio, and effective-plan state when the daemon starts a
    /// fresh session. No transport state lives here, so reset is immediate.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Cache the authoritative plan echoed by a remote streamer. This gives
    /// the viewer an honest effective readout without pretending its local
    /// decoder owns the sender's encoder state.
    pub fn record_effective(&mut self, plan: EffectivePlan) {
        self.effective.insert(plan.route_id.clone(), plan);
    }

    pub fn is_priority(&self, route_id: &str) -> bool {
        self.plan(route_id).is_some_and(|p| p.priority)
    }

    /// Whether outbound video for `peer` is currently governed by a policy
    /// plan. Legacy/audio-only peers have no video routes here and may retain
    /// their compatibility behavior without weakening a claimed aggregate cap.
    pub fn has_video_routes(&self, peer: &str) -> bool {
        self.peers
            .get(peer)
            .is_some_and(|state| !state.routes.is_empty())
    }

    fn recompute(&mut self, peer: &str) -> Vec<EffectivePlan> {
        let Some(state) = self.peers.get_mut(peer) else {
            return Vec::new();
        };
        if state.path_estimate_at.is_some_and(|sampled_at| {
            Instant::now()
                .checked_duration_since(sampled_at)
                .is_some_and(|age| age >= PATH_ESTIMATE_MAX_AGE)
        }) {
            state.path_estimate_bps = None;
            state.path_estimate_at = None;
        }
        if state.routes.is_empty() {
            return Vec::new();
        }
        if state
            .priority
            .as_ref()
            .is_none_or(|route| !state.routes.contains_key(route))
        {
            state.priority = state.routes.keys().next().cloned();
        }
        let priority = state.priority.clone().expect("non-empty routes");
        let peer_mode = state.aggregate_mode;
        let peer_contract = peer_mode.contract(state.lan);
        let mut aggregate = state
            .aggregate_override_bps
            .unwrap_or(peer_contract.aggregate_bps)
            .max(1);
        if let Some(estimate) = state.path_estimate_bps {
            aggregate = aggregate.min(estimate.max(1));
        }
        let audio_route_count = if state.audio_accounting_explicit {
            u32::try_from(state.audio_routes.len()).unwrap_or(u32::MAX)
        } else {
            // Compatibility default: before audio lifecycle accounting is
            // wired by the owner, retain the original one-stream reserve.
            1
        };
        let requested_audio_reserve = peer_contract
            .audio_reserve_bps()
            .saturating_mul(u64::from(audio_route_count));
        let audio_reserved = requested_audio_reserve.min(aggregate);
        let video_pool = aggregate.saturating_sub(audio_reserved);

        let mut caps = BTreeMap::<String, u64>::new();
        let mut floors = BTreeMap::<String, u64>::new();
        let mut weights = BTreeMap::<String, u32>::new();
        for (route_id, route) in &state.routes {
            let contract = route.request.mode.contract(state.lan);
            let cap = route
                .request
                .route_cap_bps
                .unwrap_or(contract.route_ceiling_bps)
                .min(contract.route_ceiling_bps);
            caps.insert(route_id.clone(), cap);
            // Keep the real viable floor even when an explicit route ceiling
            // is lower. The allocator then pauses this route instead of
            // issuing a sub-floor grant that a runtime encoder would clamp up
            // and use to oversubscribe the peer-wide hard envelope.
            floors.insert(route_id.clone(), contract.viable_video_floor_bps);
            weights.insert(
                route_id.clone(),
                if *route_id == priority {
                    8
                } else {
                    contract.background_weight.max(1)
                },
            );
        }
        let allocations = weighted_capped_allocation(video_pool, &caps, &floors, &weights);

        let mut plans = Vec::with_capacity(state.routes.len());
        for (route_id, route) in &state.routes {
            let contract = route.request.mode.contract(state.lan);
            let is_priority = *route_id == priority;
            let route_budget_bps = allocations.get(route_id).copied().unwrap_or(0);
            let mut degradation_reasons = Vec::new();
            let effective_mode = if route.request.mode == MediaMode::StudioLossless
                && !(route.capabilities.hevc && route.capabilities.native_hevc_decode)
            {
                degradation_reasons.push(
                    "Studio · Lossless unavailable without negotiated native HEVC decode; using Studio fallback"
                        .to_string(),
                );
                MediaMode::Studio
            } else {
                route.request.mode
            };
            if route.request.source_exact_video || route.request.mode == MediaMode::StudioLossless {
                degradation_reasons
                    .push("source-exact 4:4:4 refinement is unavailable".to_string());
            }
            if route.request.lossless_audio || route.request.mode == MediaMode::StudioLossless {
                degradation_reasons
                    .push("lossless audio unavailable; using realtime Opus".to_string());
            }
            if !route.capabilities.opus {
                degradation_reasons.push(
                    "peer did not advertise Opus; governed audio is unavailable because PCM cannot fit the aggregate reservation"
                        .to_string(),
                );
            }
            if aggregate < peer_contract.aggregate_bps {
                degradation_reasons
                    .push("peer/path cap reduced the mode aggregate budget".to_string());
            }
            if requested_audio_reserve > aggregate {
                degradation_reasons.push(format!(
                    "peer cap cannot sustain all {audio_route_count} reserved audio route(s)"
                ));
            }
            let viable_floor = floors.get(route_id).copied().unwrap_or(0);
            if caps.get(route_id).copied().unwrap_or(0) < viable_floor {
                degradation_reasons.push(format!(
                    "route paused because its ceiling is below the mode's {viable_floor} bps viable video floor"
                ));
            }
            if route_budget_bps == 0 {
                degradation_reasons.push(
                    "video paused because audio/priority routes consume the peer budget"
                        .to_string(),
                );
            } else if route_budget_bps < viable_floor {
                degradation_reasons
                    .push("route budget is below the mode's viable video floor".to_string());
            }
            debug_assert!(route_budget_bps == 0 || route_budget_bps >= viable_floor);
            let plan = EffectivePlan {
                route_id: route_id.clone(),
                requested_mode: route.request.mode,
                effective_mode,
                aggregate_budget_bps: aggregate,
                audio_route_count,
                audio_reserved_bps: audio_reserved,
                video_pool_bps: video_pool,
                route_budget_bps,
                route_ceiling_bps: caps.get(route_id).copied().unwrap_or(0),
                priority: is_priority,
                fps: contract.fps,
                video_queue_frames: VIDEO_HANDOFF_FRAMES,
                audio_queue_packets: AUDIO_HANDOFF_PACKETS,
                audio_bps: peer_contract.audio_bps,
                audio_packet_ms: peer_contract.audio_packet_ms,
                audio_jitter_ms: peer_contract.audio_jitter_ms,
                audio_fec: peer_contract.audio_fec,
                auto_rate: contract.auto_rate,
                auto_resolution: contract.auto_resolution,
                encoder: String::new(),
                codec: String::new(),
                source_exact_video: false,
                lossless_audio: false,
                degradation_reasons,
            };
            self.effective.insert(route_id.clone(), plan.clone());
            plans.push(plan);
        }
        plans
    }
}

fn weighted_capped_allocation(
    pool: u64,
    caps: &BTreeMap<String, u64>,
    floors: &BTreeMap<String, u64>,
    weights: &BTreeMap<String, u32>,
) -> BTreeMap<String, u64> {
    let mut out = caps
        .keys()
        .map(|route| (route.clone(), 0u64))
        .collect::<BTreeMap<_, _>>();
    if pool == 0 || caps.is_empty() {
        return out;
    }

    // A non-zero allocation is always at least the route's viable floor. A
    // ceiling below that floor makes the route ineligible; granting the small
    // ceiling would only make a runtime encoder clamp upward and violate the
    // peer envelope. Rank admission by focus weight and stable route id.
    let mut ranked = caps
        .keys()
        .filter(|route| {
            let floor = floors.get(*route).copied().unwrap_or(0);
            floor > 0 && caps.get(*route).copied().unwrap_or(0) >= floor
        })
        .cloned()
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| {
        weights
            .get(b)
            .unwrap_or(&1)
            .cmp(weights.get(a).unwrap_or(&1))
            .then_with(|| a.cmp(b))
    });

    let mut admitted = BTreeSet::new();
    let mut remaining = pool;
    // Admit the focused route first, then as many complete background floors
    // as fit. This single greedy pass also handles a u64::MAX peer cap without
    // summing all floors (whose mathematical total could exceed u64). Never
    // hand a leftover to a paused route: partial grants are precisely what
    // cause implementation-floor oversubscription. Surplus is distributed
    // only among already-viable routes below.
    for route in ranked {
        let floor = floors.get(&route).copied().unwrap_or(0);
        if floor <= remaining {
            out.insert(route.clone(), floor);
            admitted.insert(route);
            remaining -= floor;
        }
    }

    while remaining > 0 {
        let active = admitted
            .iter()
            .filter(|route| out[*route] < caps[*route])
            .cloned()
            .collect::<Vec<_>>();
        if active.is_empty() {
            break;
        }
        let total_weight = active
            .iter()
            .fold(0u64, |sum, route| {
                sum.saturating_add(u64::from(*weights.get(route).unwrap_or(&1)))
            })
            .max(1);
        let before = remaining;
        for route in active {
            if remaining == 0 {
                break;
            }
            let weight = u64::from(*weights.get(&route).unwrap_or(&1));
            let proportional = before.saturating_mul(weight) / total_weight;
            let share = proportional.max(1).min(remaining);
            let room = caps[&route].saturating_sub(out[&route]);
            let grant = share.min(room);
            *out.get_mut(&route).expect("same key set") += grant;
            remaining -= grant;
        }
        if remaining == before {
            break;
        }
    }
    debug_assert!(
        out.values()
            .fold(0u64, |sum, grant| sum.saturating_add(*grant))
            <= pool
    );
    debug_assert!(out.iter().all(|(route, grant)| {
        *grant == 0 || (*grant >= floors.get(route).copied().unwrap_or(0) && *grant <= caps[route])
    }));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> MediaCapabilities {
        MediaCapabilities {
            policy_v1: true,
            h264: true,
            opus: true,
            binary_media_pipes: true,
            ..MediaCapabilities::default()
        }
    }

    #[test]
    fn unknown_and_malformed_versions_fail_soft() {
        assert_eq!(PolicyEnvelope::from_ext(&Value::Null), None);
        assert_eq!(
            PolicyEnvelope::from_ext(&serde_json::json!({
                MEDIA_POLICY_EXT_KEY: { "version": 2, "kind": "request" }
            })),
            None
        );
        assert_eq!(
            PolicyEnvelope::from_ext(&serde_json::json!({
                MEDIA_POLICY_EXT_KEY: { "version": 1, "kind": "future" }
            })),
            None
        );
    }

    #[test]
    fn extension_merge_preserves_unrelated_pipeline_keys() {
        let msg = PolicyEnvelope::request("r1", PolicyRequest::default(), caps());
        let ext = msg.into_ext(serde_json::json!({ "another_pipeline": 7 }));
        assert_eq!(ext["another_pipeline"], 7);
        assert_eq!(PolicyEnvelope::from_ext(&ext), Some(msg));
    }

    #[test]
    fn reach_is_one_peer_wide_megabit_not_one_per_route() {
        let mut c = MediaPolicyController::default();
        for route in ["r1", "r2", "r3", "r4"] {
            c.apply_request(
                "peer",
                route,
                PolicyRequest {
                    mode: MediaMode::Reach,
                    priority: route == "r1",
                    ..PolicyRequest::default()
                },
                caps(),
                false,
            );
        }
        let plans = ["r1", "r2", "r3", "r4"].map(|route| c.plan(route).unwrap().clone());
        let first = &plans[0];
        assert_eq!(first.aggregate_budget_bps, 1_000_000);
        assert_eq!(first.audio_reserved_bps, 64_000);
        assert_eq!(plans.iter().filter(|p| p.priority).count(), 1);
        assert_eq!(
            plans.iter().map(|p| p.route_budget_bps).sum::<u64>(),
            936_000
        );
        assert!(plans[0].route_budget_bps > plans[1].route_budget_bps);
    }

    #[test]
    fn audio_is_reserved_before_video_and_caps_are_hard() {
        let mut c = MediaPolicyController::default();
        c.apply_request(
            "peer",
            "focus",
            PolicyRequest {
                mode: MediaMode::Balanced,
                peer_cap_bps: Some(1_000_000),
                route_cap_bps: Some(700_000),
                priority: true,
                ..PolicyRequest::default()
            },
            caps(),
            false,
        );
        c.apply_request(
            "peer",
            "background",
            PolicyRequest {
                mode: MediaMode::Balanced,
                route_cap_bps: Some(700_000),
                ..PolicyRequest::default()
            },
            caps(),
            false,
        );
        let a = c.plan("focus").unwrap();
        let b = c.plan("background").unwrap();
        assert_eq!(a.aggregate_budget_bps, 1_000_000);
        assert_eq!(a.audio_reserved_bps, 128_000);
        assert!(a.route_budget_bps <= 700_000);
        assert!(b.route_budget_bps <= 700_000);
        assert!(a.route_budget_bps + b.route_budget_bps + a.audio_reserved_bps <= 1_000_000);
    }

    #[test]
    fn a_route_ceiling_below_its_mode_floor_pauses_instead_of_clamping_up() {
        let mut c = MediaPolicyController::default();
        c.apply_request(
            "peer",
            "focus",
            PolicyRequest {
                mode: MediaMode::Balanced,
                peer_cap_bps: Some(10_000_000),
                route_cap_bps: Some(999_999),
                priority: true,
                ..PolicyRequest::default()
            },
            caps(),
            false,
        );
        let plan = c.plan("focus").unwrap();
        assert_eq!(plan.route_budget_bps, 0);
        assert!(plan
            .degradation_reasons
            .iter()
            .any(|reason| reason.contains("ceiling is below")));
    }

    #[test]
    fn focus_is_unique_and_survivor_promotion_is_deterministic() {
        let mut c = MediaPolicyController::default();
        c.register_route("p", "z", true);
        c.register_route("p", "a", true);
        c.elect_priority("z");
        assert!(c.plan("z").unwrap().priority);
        c.remove_route("z");
        assert!(c.plan("a").unwrap().priority);
    }

    #[test]
    fn focus_election_preserves_the_peer_cap_and_route_dials() {
        let mut c = MediaPolicyController::default();
        c.apply_request(
            "p",
            "a",
            PolicyRequest {
                mode: MediaMode::Game,
                peer_cap_bps: Some(10_000_000),
                route_cap_bps: Some(7_000_000),
                priority: true,
                ..PolicyRequest::default()
            },
            caps(),
            false,
        );
        c.apply_request(
            "p",
            "b",
            PolicyRequest {
                mode: MediaMode::Reach,
                route_cap_bps: Some(600_000),
                ..PolicyRequest::default()
            },
            caps(),
            false,
        );

        c.elect_priority("b");

        let a = c.plan("a").unwrap();
        let b = c.plan("b").unwrap();
        assert_eq!(a.aggregate_budget_bps, 10_000_000);
        assert_eq!(a.requested_mode, MediaMode::Game);
        assert_eq!(a.route_ceiling_bps, 7_000_000);
        assert_eq!(b.requested_mode, MediaMode::Reach);
        assert_eq!(b.route_ceiling_bps, 600_000);
        assert!(b.priority);
    }

    #[test]
    fn zero_peer_cap_is_an_explicit_auto_reset() {
        let mut c = MediaPolicyController::default();
        c.apply_request(
            "p",
            "r",
            PolicyRequest {
                mode: MediaMode::Balanced,
                peer_cap_bps: Some(4_000_000),
                ..PolicyRequest::default()
            },
            caps(),
            false,
        );
        assert_eq!(c.plan("r").unwrap().aggregate_budget_bps, 4_000_000);

        c.apply_request(
            "p",
            "r",
            PolicyRequest {
                mode: MediaMode::Balanced,
                peer_cap_bps: Some(0),
                ..PolicyRequest::default()
            },
            caps(),
            false,
        );
        assert_eq!(c.plan("r").unwrap().aggregate_budget_bps, 25_000_000);
    }

    #[test]
    fn sub_floor_tiny_cap_pauses_every_encoder_without_oversubscription() {
        let mut c = MediaPolicyController::default();
        for (index, route) in ["r1", "r2", "r3", "r4"].into_iter().enumerate() {
            c.apply_request(
                "p",
                route,
                PolicyRequest {
                    mode: MediaMode::Balanced,
                    peer_cap_bps: (index == 0).then_some(1_000_000),
                    priority: index == 0,
                    ..PolicyRequest::default()
                },
                caps(),
                false,
            );
        }
        let plans = ["r1", "r2", "r3", "r4"].map(|route| c.plan(route).unwrap());
        assert_eq!(plans[0].audio_reserved_bps, 128_000);
        assert_eq!(plans.iter().map(|p| p.route_budget_bps).sum::<u64>(), 0);
        assert_eq!(plans.iter().filter(|p| p.priority).count(), 1);
        assert!(plans.iter().all(|p| p.route_budget_bps == 0));
        assert!(plans[1]
            .degradation_reasons
            .iter()
            .any(|reason| reason.contains("paused")));

        c.elect_priority("r4");
        assert!(c.plan("r4").unwrap().priority);
        assert_eq!(c.plan("r4").unwrap().route_budget_bps, 0);
        assert_eq!(
            ["r1", "r2", "r3", "r4"]
                .into_iter()
                .map(|route| c.plan(route).unwrap().route_budget_bps)
                .sum::<u64>()
                + c.plan("r4").unwrap().audio_reserved_bps,
            128_000
        );
    }

    #[test]
    fn four_displays_admit_complete_floors_and_focus_moves_the_surplus() {
        let mut c = MediaPolicyController::default();
        for (index, route) in ["r1", "r2", "r3", "r4"].into_iter().enumerate() {
            c.apply_request(
                "p",
                route,
                PolicyRequest {
                    mode: MediaMode::Balanced,
                    peer_cap_bps: (index == 0).then_some(2_200_000),
                    priority: index == 0,
                    ..PolicyRequest::default()
                },
                caps(),
                false,
            );
        }
        let before = ["r1", "r2", "r3", "r4"]
            .into_iter()
            .map(|route| (route, c.plan(route).unwrap().route_budget_bps))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(before.values().sum::<u64>(), 2_072_000);
        assert!(before["r1"] >= 1_000_000);
        assert!(before.values().filter(|grant| **grant > 0).count() >= 2);
        assert!(before
            .values()
            .all(|grant| *grant == 0 || *grant >= 1_000_000));

        c.elect_priority("r4");
        let after = ["r1", "r2", "r3", "r4"]
            .into_iter()
            .map(|route| (route, c.plan(route).unwrap().route_budget_bps))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(after.values().sum::<u64>(), 2_072_000);
        assert!(after["r4"] >= 1_000_000);
        assert!(c.plan("r4").unwrap().priority);
        assert_eq!(
            ["r1", "r2", "r3", "r4"]
                .into_iter()
                .filter(|route| c.plan(route).unwrap().priority)
                .count(),
            1
        );
        assert_eq!(c.plan("r4").unwrap().aggregate_budget_bps, 2_200_000);
    }

    #[test]
    fn focus_only_request_preserves_auto_envelope_and_every_route_dial() {
        let mut c = MediaPolicyController::default();
        c.apply_request(
            "p",
            "game",
            PolicyRequest {
                mode: MediaMode::Game,
                route_cap_bps: Some(9_000_000),
                priority: true,
                ..PolicyRequest::default()
            },
            caps(),
            false,
        );
        c.apply_request(
            "p",
            "balanced",
            PolicyRequest {
                mode: MediaMode::Balanced,
                route_cap_bps: Some(7_000_000),
                ..PolicyRequest::default()
            },
            caps(),
            false,
        );
        let aggregate = c.plan("game").unwrap().aggregate_budget_bps;
        assert_eq!(aggregate, MediaMode::Balanced.contract(false).aggregate_bps);

        c.apply_request(
            "p",
            "game",
            PolicyRequest {
                // Deliberately bogus legacy-quality values: priority-only
                // semantics must ignore all of them.
                mode: MediaMode::Reach,
                peer_cap_bps: Some(1),
                route_cap_bps: Some(1),
                priority: true,
                priority_only: true,
                ..PolicyRequest::default()
            },
            MediaCapabilities::default(),
            true,
        );
        assert!(c.plan("game").unwrap().priority);
        assert_eq!(c.plan("game").unwrap().aggregate_budget_bps, aggregate);
        assert_eq!(c.plan("game").unwrap().requested_mode, MediaMode::Game);
        assert_eq!(c.plan("game").unwrap().route_ceiling_bps, 9_000_000);
        assert_eq!(
            c.plan("balanced").unwrap().requested_mode,
            MediaMode::Balanced
        );
        assert_eq!(c.plan("balanced").unwrap().route_ceiling_bps, 7_000_000);
    }

    #[test]
    fn multiple_audio_routes_are_reserved_before_any_video() {
        let mut c = MediaPolicyController::default();
        c.apply_request(
            "p",
            "v",
            PolicyRequest {
                mode: MediaMode::Balanced,
                peer_cap_bps: Some(3_000_000),
                priority: true,
                ..PolicyRequest::default()
            },
            caps(),
            false,
        );
        assert_eq!(c.plan("v").unwrap().audio_route_count, 1);
        assert_eq!(c.plan("v").unwrap().audio_reserved_bps, 128_000);

        c.register_audio_route("p", "mic");
        c.register_audio_route("p", "system");
        let two = c.plan("v").unwrap();
        assert_eq!(two.audio_route_count, 2);
        assert_eq!(two.audio_reserved_bps, 256_000);
        assert_eq!(two.route_budget_bps + two.audio_reserved_bps, 3_000_000);

        c.remove_audio_route("mic");
        assert_eq!(c.plan("v").unwrap().audio_route_count, 1);
        assert_eq!(c.plan("v").unwrap().audio_reserved_bps, 128_000);
        c.remove_audio_route("system");
        assert_eq!(c.plan("v").unwrap().audio_route_count, 0);
        assert_eq!(c.plan("v").unwrap().audio_reserved_bps, 0);
        assert_eq!(c.plan("v").unwrap().route_budget_bps, 3_000_000);
    }

    #[test]
    fn stale_path_estimate_expires_and_unknown_sample_clears_immediately() {
        let mut c = MediaPolicyController::default();
        c.apply_request("p", "v", PolicyRequest::default(), caps(), false);
        c.note_path_estimate("p", Some(4_000_000));
        assert_eq!(c.plan("v").unwrap().aggregate_budget_bps, 4_000_000);
        c.note_path_estimate("p", None);
        assert_eq!(c.plan("v").unwrap().aggregate_budget_bps, 25_000_000);

        let sampled_at = Instant::now();
        {
            let peer = c.peers.get_mut("p").unwrap();
            peer.path_estimate_bps = Some(3_000_000);
            peer.path_estimate_at = Some(sampled_at);
        }
        c.recompute("p");
        assert_eq!(c.plan("v").unwrap().aggregate_budget_bps, 3_000_000);
        let changed = c.expire_stale_path_estimates_at(sampled_at + PATH_ESTIMATE_MAX_AGE);
        assert_eq!(changed.len(), 1);
        assert_eq!(c.plan("v").unwrap().aggregate_budget_bps, 25_000_000);
    }

    #[test]
    fn moving_an_audio_route_recomputes_siblings_on_both_peers() {
        let mut c = MediaPolicyController::default();
        for (peer, route) in [("old", "old-video"), ("new", "new-video")] {
            c.apply_request(
                peer,
                route,
                PolicyRequest {
                    peer_cap_bps: Some(3_000_000),
                    ..PolicyRequest::default()
                },
                caps(),
                false,
            );
        }
        // Adopt explicit lifecycle accounting on the destination first so it
        // represents a known empty audio set rather than the v1 conservative
        // one-route compatibility reserve.
        c.register_audio_route("new", "seed-audio");
        c.remove_audio_route("seed-audio");

        c.register_audio_route("old", "audio");
        assert_eq!(c.plan("old-video").unwrap().audio_route_count, 1);
        assert_eq!(c.plan("new-video").unwrap().audio_route_count, 0);

        let changed = c.register_audio_route("new", "audio");
        let changed_routes = changed
            .iter()
            .map(|plan| plan.route_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            changed_routes,
            ["new-video", "old-video"].into_iter().collect()
        );
        assert_eq!(c.plan("old-video").unwrap().audio_route_count, 0);
        assert_eq!(c.plan("new-video").unwrap().audio_route_count, 1);
        assert!(c.register_audio_route("new", "audio").is_empty());
    }

    #[test]
    fn video_route_presence_distinguishes_governed_from_audio_only_peers() {
        let mut c = MediaPolicyController::default();
        c.register_audio_route("p", "audio");
        assert!(!c.has_video_routes("p"));

        c.apply_request("p", "video", PolicyRequest::default(), caps(), false);
        assert!(c.has_video_routes("p"));

        c.remove_route("video");
        assert!(!c.has_video_routes("p"));
    }

    #[test]
    fn reset_drops_routes_audio_and_effective_state() {
        let mut c = MediaPolicyController::default();
        c.register_audio_route("p", "audio");
        c.apply_request("p", "video", PolicyRequest::default(), caps(), false);
        assert!(c.has_video_routes("p"));
        assert!(c.plan("video").is_some());

        c.reset();
        assert!(!c.has_video_routes("p"));
        assert!(c.plan("video").is_none());
        assert!(c.plans_for_peer("p").is_empty());
        assert!(c.remove_audio_route("audio").is_empty());
    }

    #[test]
    fn repeated_path_estimates_are_idempotent_but_refresh_their_lease() {
        let mut c = MediaPolicyController::default();
        c.apply_request("p", "v", PolicyRequest::default(), caps(), false);
        assert_eq!(c.note_path_estimate("p", Some(4_000_000)).len(), 1);
        let first_seen = c.peers["p"].path_estimate_at.expect("estimate timestamp");
        assert!(c.note_path_estimate("p", Some(4_000_000)).is_empty());
        assert!(c.peers["p"].path_estimate_at.unwrap() >= first_seen);
        assert_eq!(c.note_path_estimate("p", None).len(), 1);
        assert!(c.note_path_estimate("p", None).is_empty());
    }

    #[test]
    fn mode_contracts_match_the_field_guide() {
        let reach_wan = MediaMode::Reach.contract(false);
        assert_eq!((reach_wan.aggregate_bps, reach_wan.fps), (1_000_000, 15));
        assert_eq!(
            (reach_wan.audio_packet_ms, reach_wan.audio_bps),
            (20, 48_000)
        );
        let game_lan = MediaMode::Game.contract(true);
        assert_eq!((game_lan.aggregate_bps, game_lan.fps), (200_000_000, 60));
        assert_eq!((game_lan.audio_packet_ms, game_lan.audio_bps), (5, 128_000));
        let studio_wan = MediaMode::Studio.contract(false);
        assert_eq!(studio_wan.aggregate_bps, 150_000_000);
        assert!(!studio_wan.auto_resolution);

        assert_eq!(reach_wan.viable_video_floor_bps, 120_000);
        assert_eq!(
            MediaMode::Balanced.contract(false).viable_video_floor_bps,
            1_000_000
        );
        assert_eq!(game_lan.viable_video_floor_bps, 1_000_000);
        assert_eq!(studio_wan.viable_video_floor_bps, 2_000_000);
    }

    #[test]
    fn every_mode_grants_zero_or_at_least_its_viable_floor() {
        for mode in [
            MediaMode::Reach,
            MediaMode::Balanced,
            MediaMode::Game,
            MediaMode::Studio,
            MediaMode::StudioLossless,
        ] {
            let contract = mode.contract(false);
            let mut c = MediaPolicyController::default();
            c.apply_request(
                "p",
                "v",
                PolicyRequest {
                    mode,
                    peer_cap_bps: Some(
                        contract.audio_reserve_bps() + contract.viable_video_floor_bps - 1,
                    ),
                    priority: true,
                    ..PolicyRequest::default()
                },
                caps(),
                false,
            );
            assert_eq!(c.plan("v").unwrap().route_budget_bps, 0, "{mode:?}");

            c.apply_request(
                "p",
                "v",
                PolicyRequest {
                    mode,
                    peer_cap_bps: Some(
                        contract.audio_reserve_bps() + contract.viable_video_floor_bps,
                    ),
                    priority: true,
                    ..PolicyRequest::default()
                },
                caps(),
                false,
            );
            assert_eq!(
                c.plan("v").unwrap().route_budget_bps,
                contract.viable_video_floor_bps,
                "{mode:?}"
            );
        }
    }

    #[test]
    fn allocator_exhaustively_preserves_cap_pool_and_floor_invariants() {
        const VALUES: [u64; 6] = [0, 1, 99, 100, 150, 500];
        let floors = BTreeMap::from([
            ("focus".to_string(), 100),
            ("bg-a".to_string(), 100),
            ("bg-b".to_string(), 100),
        ]);
        let weights = BTreeMap::from([
            ("focus".to_string(), 8),
            ("bg-a".to_string(), 2),
            ("bg-b".to_string(), 1),
        ]);
        for pool in VALUES {
            for focus_cap in VALUES {
                for bg_a_cap in VALUES {
                    for bg_b_cap in VALUES {
                        let caps = BTreeMap::from([
                            ("focus".to_string(), focus_cap),
                            ("bg-a".to_string(), bg_a_cap),
                            ("bg-b".to_string(), bg_b_cap),
                        ]);
                        let out = weighted_capped_allocation(pool, &caps, &floors, &weights);
                        assert!(out.values().sum::<u64>() <= pool);
                        for (route, grant) in out {
                            assert!(grant <= caps[&route]);
                            assert!(grant == 0 || grant >= floors[&route]);
                        }
                        if pool >= 100 && focus_cap >= 100 {
                            assert!(
                                weighted_capped_allocation(pool, &caps, &floors, &weights)["focus"]
                                    >= 100
                            );
                        }
                    }
                }
            }
        }

        let huge_caps = BTreeMap::from([
            ("focus".to_string(), u64::MAX),
            ("background".to_string(), u64::MAX),
        ]);
        let huge_floors = BTreeMap::from([
            ("focus".to_string(), u64::MAX - 10),
            ("background".to_string(), 20),
        ]);
        let huge_weights =
            BTreeMap::from([("focus".to_string(), 8), ("background".to_string(), 1)]);
        let huge = weighted_capped_allocation(u64::MAX, &huge_caps, &huge_floors, &huge_weights);
        assert_eq!(huge["focus"], u64::MAX);
        assert_eq!(huge["background"], 0);
    }

    #[test]
    fn studio_lossless_falls_back_honestly_without_native_hevc() {
        let mut c = MediaPolicyController::default();
        c.apply_request(
            "p",
            "v",
            PolicyRequest {
                mode: MediaMode::StudioLossless,
                source_exact_video: true,
                lossless_audio: true,
                priority: true,
                ..PolicyRequest::default()
            },
            caps(),
            true,
        );
        let fallback = c.plan("v").unwrap();
        assert_eq!(fallback.requested_mode, MediaMode::StudioLossless);
        assert_eq!(fallback.effective_mode, MediaMode::Studio);
        assert!(!fallback.source_exact_video);
        assert!(!fallback.lossless_audio);
        assert!(fallback
            .degradation_reasons
            .iter()
            .any(|reason| reason.contains("native HEVC")));

        let mut hevc = caps();
        hevc.hevc = true;
        hevc.native_hevc_decode = true;
        c.apply_request(
            "p",
            "v",
            PolicyRequest {
                mode: MediaMode::StudioLossless,
                priority: true,
                ..PolicyRequest::default()
            },
            hevc,
            true,
        );
        let supported = c.plan("v").unwrap();
        assert_eq!(supported.effective_mode, MediaMode::StudioLossless);
        assert!(!supported.source_exact_video);
        assert!(!supported.lossless_audio);
    }
}

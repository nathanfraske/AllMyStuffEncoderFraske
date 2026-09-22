//! Canonical peer/lane video freshness before a bounded ingress queue.
//! Applications own authentication, framing negotiation and actual admission.
//! This policy intentionally remains distinct from route-local assembly.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Encoded payload and its lane metadata, with opaque application context.
/// Adapters can move an existing envelope into this type without copying data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame<C> {
    pub context: C,
    pub key: bool,
    pub stream: u8,
    pub rtp_timestamp: u32,
    pub from: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event<C> {
    Frame(Frame<C>),
    Unframed(Frame<C>),
    Discontinuity {
        from: String,
        stream: u8,
        reason: &'static str,
        entry: Option<Frame<C>>,
    },
}

/// Exact synchronous admission outcome, retaining the rejected event.
pub enum TrySendError<C> {
    Full(Event<C>),
    Closed(Event<C>),
}

/// One nonblocking attempt on the caller's existing bounded queue.
/// Implementations must not buffer, wait, retry, or test closure in advance.
pub trait Sink<C> {
    fn try_send(&self, event: Event<C>) -> Result<(), TrySendError<C>>;
}

/// Per peer/lane state for the daemon -> node handoff. The daemon's media pipe
/// must keep draining even if the node briefly falls behind: blocking here lets
/// the daemon's queue grow without bound and turns a live stream into replay.
/// Paced fragments assemble before the four-AU queue. Once a complete AU is
/// shed, reset streams suppress dependent deltas until a clean entry; gradual
/// streams order one gap marker and continue admitting convergence frames.
#[derive(Debug, Clone, Copy)]
struct InboundRecoveryState {
    mode: crate::metadata::AuRecovery,
    marker_queued: bool,
    reason: &'static str,
}

const MAX_PACED_AU_CHUNKS: usize = 2048;
const MAX_PACED_AU_BYTES: usize = 16 * 1024 * 1024;
const MAX_PACED_AU_AGE: Duration = Duration::from_secs(1);

#[derive(Debug)]
struct PacedAu<C> {
    frame: Frame<C>,
    chunks: usize,
    updated: Instant,
    timing: Option<crate::timing::AssemblyClock>,
}

pub struct Freshness<C> {
    marker_count: fn(&[u8]) -> Option<usize>,
    recovering: HashMap<(String, u8), InboundRecoveryState>,
    recovery: HashMap<(String, u8), crate::metadata::AuRecovery>,
    paced: HashMap<(String, u8), PacedAu<C>>,
    last_slow_assembly_log: Option<Instant>,
}

fn canonical_media_peer(id: &str) -> &str {
    if let Some((body, suffix)) = id.rsplit_once('-') {
        if suffix.len() == 5 && suffix.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return body;
        }
    }
    id
}

impl<C> Freshness<C> {
    /// Use the application's existing marker classifier for negotiated lanes.
    pub fn new(marker_count: fn(&[u8]) -> Option<usize>) -> Self {
        Self {
            marker_count,
            recovering: HashMap::new(),
            recovery: HashMap::new(),
            paced: HashMap::new(),
            last_slow_assembly_log: None,
        }
    }

    pub fn has_pending_paced(&self) -> bool {
        !self.paced.is_empty()
    }

    fn lane(frame: &Frame<C>) -> (String, u8) {
        (canonical_media_peer(&frame.from).to_string(), frame.stream)
    }

    fn recovery_for(&self, lane: &(String, u8)) -> crate::metadata::AuRecovery {
        self.recovery
            .get(lane)
            .copied()
            .unwrap_or(crate::metadata::AuRecovery::Reset)
    }

    fn note_discontinuity(
        &mut self,
        from: String,
        stream: u8,
        reason: &'static str,
        tx: &impl Sink<C>,
    ) -> bool {
        let lane = (canonical_media_peer(&from).to_string(), stream);
        let mode = self.recovery_for(&lane);
        let state = self
            .recovering
            .entry(lane.clone())
            .or_insert(InboundRecoveryState {
                mode,
                marker_queued: false,
                reason,
            });
        state.mode = mode;
        if state.marker_queued {
            return true;
        }
        match tx.try_send(Event::Discontinuity {
            from,
            stream,
            reason: state.reason,
            entry: None,
        }) {
            Ok(()) => {
                if mode == crate::metadata::AuRecovery::Gradual {
                    self.recovering.remove(&lane);
                } else if let Some(state) = self.recovering.get_mut(&lane) {
                    state.marker_queued = true;
                }
                true
            }
            Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Closed(_)) => false,
        }
    }

    /// Assemble one negotiated paced-video fragment train before it reaches the
    /// AU queue. A large Game frame may contain dozens of fragments but still
    /// consumes exactly one queue slot when its count marker closes it.
    pub fn forward_paced(&mut self, frame: Frame<C>, tx: &impl Sink<C>) -> bool {
        let lane = Self::lane(&frame);
        if let Some(expected) = (self.marker_count)(&frame.data) {
            let Some(pending) = self.paced.remove(&lane) else {
                return self.note_discontinuity(
                    frame.from,
                    frame.stream,
                    "paced AU marker without fragments",
                    tx,
                );
            };
            if pending.frame.rtp_timestamp != frame.rtp_timestamp || pending.chunks != expected {
                return self.note_discontinuity(
                    frame.from,
                    frame.stream,
                    "paced AU timestamp/count mismatch",
                    tx,
                );
            }
            if let Some(clock) = pending.timing {
                let now = Instant::now();
                let (elapsed, max_gap) = clock.finish(now);
                let sequence = crate::metadata::peek_au_identity_marker(&pending.frame.data)
                    .map(|id| id.sequence);
                let periodic = crate::timing::periodic_sample(sequence);
                let slow = elapsed >= Duration::from_millis(50)
                    && self
                        .last_slow_assembly_log
                        .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(5));
                if periodic || slow {
                    if !periodic {
                        self.last_slow_assembly_log = Some(now);
                    }
                    tracing::debug!(target: "allmystuff_node::video_timing",
                        from = %pending.frame.from, lane = pending.frame.stream, ?sequence,
                        rtp_timestamp = pending.frame.rtp_timestamp,
                        bytes = pending.frame.data.len(), chunks = pending.chunks,
                        total_us = elapsed.as_micros() as u64,
                        fragment_gap_max_us = max_gap.as_micros() as u64,
                        "video AU assembly timing");
                }
            }
            return self.forward(pending.frame, tx);
        }

        if frame.data.len() > MAX_PACED_AU_BYTES {
            // Bound the first fragment as well as continuations. A rejected
            // replacement also invalidates the old train, so its late marker
            // cannot release stale data.
            self.paced.remove(&lane);
            return self.note_discontinuity(
                frame.from,
                frame.stream,
                "paced AU exceeded assembly bounds",
                tx,
            );
        }

        let expired = self
            .paced
            .get(&lane)
            .is_some_and(|pending| pending.updated.elapsed() > MAX_PACED_AU_AGE);
        if expired {
            self.paced.remove(&lane);
        }
        let mut damaged = expired;
        match self.paced.get_mut(&lane) {
            Some(pending) if pending.frame.rtp_timestamp == frame.rtp_timestamp => {
                if pending.chunks >= MAX_PACED_AU_CHUNKS
                    || pending.frame.data.len().saturating_add(frame.data.len())
                        > MAX_PACED_AU_BYTES
                {
                    self.paced.remove(&lane);
                    return self.note_discontinuity(
                        frame.from,
                        frame.stream,
                        "paced AU exceeded assembly bounds",
                        tx,
                    );
                }
                pending.frame.key |= frame.key;
                pending.frame.data.extend_from_slice(&frame.data);
                pending.chunks += 1;
                pending.updated = Instant::now();
                if let Some(clock) = &mut pending.timing {
                    clock.observe(pending.updated);
                }
            }
            Some(_) => {
                self.paced.insert(
                    lane.clone(),
                    PacedAu {
                        frame,
                        chunks: 1,
                        updated: Instant::now(),
                        timing: tracing::enabled!(target: "allmystuff_node::video_timing", tracing::Level::DEBUG)
                            .then(|| crate::timing::AssemblyClock::new(Instant::now())),
                    },
                );
                damaged = true;
            }
            None => {
                self.paced.insert(
                    lane.clone(),
                    PacedAu {
                        frame,
                        chunks: 1,
                        updated: Instant::now(),
                        timing: tracing::enabled!(target: "allmystuff_node::video_timing", tracing::Level::DEBUG)
                            .then(|| crate::timing::AssemblyClock::new(Instant::now())),
                    },
                );
            }
        }
        if damaged {
            let (from, stream) = {
                let pending = self.paced.get(&lane).expect("paced frame just inserted");
                (pending.frame.from.clone(), pending.frame.stream)
            };
            self.note_discontinuity(from, stream, "paced AU expired or missing end marker", tx)
        } else {
            true
        }
    }

    pub fn discard_paced_lane(&mut self, frame: &Frame<C>) {
        self.paced.remove(&Self::lane(frame));
    }

    pub fn discard_paced_peer_lane(&mut self, from: &str, stream: u8) {
        self.paced
            .remove(&(canonical_media_peer(from).to_string(), stream));
    }

    pub fn forward_transport_discontinuity(&mut self, frame: Frame<C>, tx: &impl Sink<C>) -> bool {
        // Any paced fragments preceding the transport gap are necessarily an
        // incomplete AU. Drop them before ordering the decoder reset marker.
        self.discard_paced_lane(&frame);
        self.note_discontinuity(
            frame.from,
            frame.stream,
            "MyOwnMesh reported media discontinuity (transport or daemon IPC)",
            tx,
        )
    }

    /// Return false only when the consumer has gone away and the pipe should
    /// close. Queue pressure is handled locally and always returns true.
    pub fn forward(&mut self, frame: Frame<C>, tx: &impl Sink<C>) -> bool {
        let lane = Self::lane(&frame);
        if let Some(identity) = crate::metadata::peek_au_identity_marker(&frame.data) {
            self.recovery.insert(lane.clone(), identity.recovery);
            if let Some(state) = self.recovering.get_mut(&lane) {
                state.mode = identity.recovery;
            }
        }
        if let Some(state) = self.recovering.get(&lane).copied() {
            if state.mode == crate::metadata::AuRecovery::Gradual {
                return match tx.try_send(Event::Discontinuity {
                    from: frame.from.clone(),
                    stream: frame.stream,
                    reason: state.reason,
                    entry: Some(frame),
                }) {
                    Ok(()) => {
                        self.recovering.remove(&lane);
                        true
                    }
                    Err(TrySendError::Full(_)) => true,
                    Err(TrySendError::Closed(_)) => false,
                };
            }
            if !state.marker_queued {
                let clean_entry = frame.key || crate::codec::is_decode_entry(&frame.data);
                let event = Event::Discontinuity {
                    from: frame.from.clone(),
                    stream: frame.stream,
                    reason: state.reason,
                    entry: clean_entry.then_some(frame),
                };
                return match tx.try_send(event) {
                    Ok(()) => {
                        if clean_entry {
                            self.recovering.remove(&lane);
                        } else if let Some(state) = self.recovering.get_mut(&lane) {
                            state.marker_queued = true;
                        }
                        true
                    }
                    Err(TrySendError::Full(_)) => true,
                    Err(TrySendError::Closed(_)) => false,
                };
            }

            if !(frame.key || crate::codec::is_decode_entry(&frame.data)) {
                return true;
            }
            return match tx.try_send(Event::Frame(frame)) {
                Ok(()) => {
                    self.recovering.remove(&lane);
                    true
                }
                Err(TrySendError::Full(_)) => true,
                Err(TrySendError::Closed(_)) => false,
            };
        }

        match tx.try_send(Event::Frame(frame)) {
            Ok(()) => true,
            Err(TrySendError::Full(Event::Frame(frame))) => {
                let lane = Self::lane(&frame);
                let mode = self.recovery_for(&lane);
                self.recovering.insert(
                    lane,
                    InboundRecoveryState {
                        mode,
                        marker_queued: false,
                        reason: "AMS complete-AU ingress queue overflow",
                    },
                );
                true
            }
            Err(TrySendError::Full(_)) => unreachable!("sent Frame"),
            Err(TrySendError::Closed(_)) => false,
        }
    }
}

/// Local IPC is not a playout buffer. Four H.264 access units absorb ordinary
/// task scheduling jitter (about 67 ms at 60 fps / 133 ms at 30 fps) without
/// preserving seconds of history after the viewer stalls. Audio stays on its
/// own eight-packet queue so neither plane can head-of-line block the other.
pub(crate) const MEDIA_VIDEO_QUEUE_CAPACITY: usize = 4;
pub(crate) const MEDIA_AUDIO_QUEUE_CAPACITY: usize = 8;
pub(crate) type VideoFramingFn = dyn Fn(&str, u8) -> Option<bool> + Send + Sync;

/// One item on the bounded H.264 ingress queue. Paced fragments are reassembled
/// before this boundary, so a Frame is one complete access unit. A
/// discontinuity remains ordered beside the units it describes: reset streams
/// fence to a clean entry; gradual streams carry the next wave frame inline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InboundVideoEvent {
    Frame(InboundFrame),
    /// Framing could not be resolved before the authenticated lane binding
    /// arrived. Let the mesh perform its normal route fallback and paced
    /// decision; never guess that a paced fragment is a whole access unit.
    Unframed(InboundFrame),
    Discontinuity {
        from: String,
        stream: u8,
        reason: &'static str,
        entry: Option<InboundFrame>,
    },
}

/// Per peer/lane state for the daemon -> node handoff. The daemon's media pipe
/// must keep draining even if the node briefly falls behind: blocking here lets
/// the daemon's queue grow without bound and turns a live stream into replay.
/// Paced fragments assemble before the four-AU queue. Once a complete AU is
/// shed, reset streams suppress dependent deltas until a clean entry; gradual
/// streams order one gap marker and continue admitting convergence frames.
#[derive(Debug, Clone, Copy)]
struct InboundRecoveryState {
    mode: crate::video_wire::AuRecovery,
    marker_queued: bool,
    reason: &'static str,
}

const MAX_PACED_AU_CHUNKS: usize = 2048;
const MAX_PACED_AU_BYTES: usize = 16 * 1024 * 1024;
const MAX_PACED_AU_AGE: Duration = Duration::from_secs(1);

#[derive(Debug)]
struct InboundPacedAu {
    frame: InboundFrame,
    chunks: usize,
    updated: Instant,
    timing: Option<crate::video_frame_timing::AssemblyClock>,
}

#[derive(Default)]
struct InboundVideoFreshness {
    recovering: HashMap<(String, u8), InboundRecoveryState>,
    recovery: HashMap<(String, u8), crate::video_wire::AuRecovery>,
    paced: HashMap<(String, u8), InboundPacedAu>,
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

impl InboundVideoFreshness {
    fn lane(frame: &InboundFrame) -> (String, u8) {
        (canonical_media_peer(&frame.from).to_string(), frame.stream)
    }

    fn recovery_for(&self, lane: &(String, u8)) -> crate::video_wire::AuRecovery {
        self.recovery
            .get(lane)
            .copied()
            .unwrap_or(crate::video_wire::AuRecovery::Reset)
    }

    fn note_discontinuity(
        &mut self,
        from: String,
        stream: u8,
        reason: &'static str,
        tx: &mpsc::Sender<InboundVideoEvent>,
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
        match tx.try_send(InboundVideoEvent::Discontinuity {
            from,
            stream,
            reason: state.reason,
            entry: None,
        }) {
            Ok(()) => {
                if mode == crate::video_wire::AuRecovery::Gradual {
                    self.recovering.remove(&lane);
                } else if let Some(state) = self.recovering.get_mut(&lane) {
                    state.marker_queued = true;
                }
                true
            }
            Err(mpsc::error::TrySendError::Full(_)) => true,
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    /// Assemble one negotiated paced-video fragment train before it reaches the
    /// AU queue. A large Game frame may contain dozens of fragments but still
    /// consumes exactly one queue slot when its count marker closes it.
    fn forward_paced(&mut self, frame: InboundFrame, tx: &mpsc::Sender<InboundVideoEvent>) -> bool {
        let lane = Self::lane(&frame);
        if let Some(expected) = crate::video::paced_au_marker_count(&frame.data) {
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
                let sequence = crate::video_wire::peek_au_identity_marker(&pending.frame.data)
                    .map(|id| id.sequence);
                let periodic = crate::video_frame_timing::periodic_sample(sequence);
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
                    InboundPacedAu {
                        frame,
                        chunks: 1,
                        updated: Instant::now(),
                        timing: tracing::enabled!(target: "allmystuff_node::video_timing", tracing::Level::DEBUG)
                            .then(|| crate::video_frame_timing::AssemblyClock::new(Instant::now())),
                    },
                );
                damaged = true;
            }
            None => {
                self.paced.insert(
                    lane.clone(),
                    InboundPacedAu {
                        frame,
                        chunks: 1,
                        updated: Instant::now(),
                        timing: tracing::enabled!(target: "allmystuff_node::video_timing", tracing::Level::DEBUG)
                            .then(|| crate::video_frame_timing::AssemblyClock::new(Instant::now())),
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

    fn discard_paced_lane(&mut self, frame: &InboundFrame) {
        self.paced.remove(&Self::lane(frame));
    }

    fn forward_transport_discontinuity(
        &mut self,
        frame: InboundFrame,
        tx: &mpsc::Sender<InboundVideoEvent>,
    ) -> bool {
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
    fn forward(&mut self, frame: InboundFrame, tx: &mpsc::Sender<InboundVideoEvent>) -> bool {
        let lane = Self::lane(&frame);
        if let Some(identity) = crate::video_wire::peek_au_identity_marker(&frame.data) {
            self.recovery.insert(lane.clone(), identity.recovery);
            if let Some(state) = self.recovering.get_mut(&lane) {
                state.mode = identity.recovery;
            }
        }
        if let Some(state) = self.recovering.get(&lane).copied() {
            if state.mode == crate::video_wire::AuRecovery::Gradual {
                return match tx.try_send(InboundVideoEvent::Discontinuity {
                    from: frame.from.clone(),
                    stream: frame.stream,
                    reason: state.reason,
                    entry: Some(frame),
                }) {
                    Ok(()) => {
                        self.recovering.remove(&lane);
                        true
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => true,
                    Err(mpsc::error::TrySendError::Closed(_)) => false,
                };
            }
            if !state.marker_queued {
                let clean_entry = frame.key || crate::video_decode::is_decode_entry(&frame.data);
                let event = InboundVideoEvent::Discontinuity {
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
                    Err(mpsc::error::TrySendError::Full(_)) => true,
                    Err(mpsc::error::TrySendError::Closed(_)) => false,
                };
            }

            if !(frame.key || crate::video_decode::is_decode_entry(&frame.data)) {
                return true;
            }
            return match tx.try_send(InboundVideoEvent::Frame(frame)) {
                Ok(()) => {
                    self.recovering.remove(&lane);
                    true
                }
                Err(mpsc::error::TrySendError::Full(_)) => true,
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            };
        }

        match tx.try_send(InboundVideoEvent::Frame(frame)) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(InboundVideoEvent::Frame(frame))) => {
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
            Err(mpsc::error::TrySendError::Full(_)) => unreachable!("sent Frame"),
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}

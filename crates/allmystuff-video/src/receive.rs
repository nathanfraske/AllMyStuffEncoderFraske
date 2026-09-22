//! Route-local paced assembly and sequence decisions.
//! Peer binding, transport admission and recovery requests remain caller-owned.

use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuSequenceObservation {
    Accept,
    Gap,
    DropDuplicateOrStale,
}

/// Classify one route-local AU sequence without guessing from timestamps.
/// Clean entries are allowed to reset a sender that restarted in place;
/// duplicates/stale units never move the high-water mark backwards.
pub fn observe_au_sequence(
    previous: Option<u64>,
    incoming: u64,
    clean_entry: bool,
) -> AuSequenceObservation {
    let Some(previous) = previous else {
        return AuSequenceObservation::Accept;
    };
    if incoming == previous {
        return AuSequenceObservation::DropDuplicateOrStale;
    }
    let expected = previous.wrapping_add(1);
    if incoming == expected || clean_entry {
        return AuSequenceObservation::Accept;
    }
    if incoming.wrapping_sub(expected) < (1u64 << 63) {
        AuSequenceObservation::Gap
    } else {
        AuSequenceObservation::DropDuplicateOrStale
    }
}

const MAX_PACED_AU_CHUNKS: usize = 2048;
const MAX_PACED_AU_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub struct PacedInboundAu {
    rtp_timestamp: u32,
    key: bool,
    chunks: usize,
    data: Vec<u8>,
    timing: Option<crate::timing::AssemblyClock>,
}

impl PacedInboundAu {
    fn new(rtp_timestamp: u32, key: bool, data: Vec<u8>) -> Self {
        Self {
            rtp_timestamp,
            key,
            chunks: 1,
            data,
            timing:
                tracing::enabled!(target: "allmystuff_node::video_timing", tracing::Level::DEBUG)
                    .then(|| crate::timing::AssemblyClock::new(Instant::now())),
        }
    }
}

#[derive(Debug)]
pub struct CompletePacedAu {
    pub rtp_timestamp: u32,
    pub key: bool,
    pub data: Vec<u8>,
    pub chunks: usize,
    pub timing: Option<(Duration, Duration)>,
}

pub fn accept_paced_fragment(
    pending: &mut HashMap<String, PacedInboundAu>,
    route_id: &str,
    rtp_timestamp: u32,
    key: bool,
    data: Vec<u8>,
) -> (Option<CompletePacedAu>, bool) {
    let marker_count = crate::framing::paced_au_marker_count(&data);
    if let Some(expected) = marker_count {
        let Some(au) = pending.remove(route_id) else {
            return (None, true);
        };
        if au.rtp_timestamp != rtp_timestamp || au.chunks != expected {
            return (None, true);
        }
        return (
            Some(CompletePacedAu {
                rtp_timestamp: au.rtp_timestamp,
                key: au.key,
                data: au.data,
                chunks: au.chunks,
                timing: au.timing.map(|clock| clock.finish(Instant::now())),
            }),
            false,
        );
    }

    // First and replacement fragments obey the same ceiling as continuations.
    // Drop an old unit too, so a later marker cannot complete stale data.
    if data.len() > MAX_PACED_AU_BYTES {
        pending.remove(route_id);
        return (None, true);
    }

    let mut damaged = false;
    match pending.get_mut(route_id) {
        Some(au) if au.rtp_timestamp == rtp_timestamp => {
            if au.chunks >= MAX_PACED_AU_CHUNKS
                || au.data.len().saturating_add(data.len()) > MAX_PACED_AU_BYTES
            {
                pending.remove(route_id);
                return (None, true);
            }
            au.key |= key;
            if let Some(clock) = &mut au.timing {
                clock.observe(Instant::now());
            }
            au.chunks += 1;
            au.data.extend_from_slice(&data);
        }
        Some(_) => {
            // A new timestamp before a marker proves the prior AU was
            // incomplete. Start collecting the new unit, but report the
            // damage so the sender supplies a fresh decode entry.
            pending.insert(
                route_id.to_string(),
                PacedInboundAu::new(rtp_timestamp, key, data),
            );
            damaged = true;
        }
        None => {
            pending.insert(
                route_id.to_string(),
                PacedInboundAu::new(rtp_timestamp, key, data),
            );
        }
    }
    (None, damaged)
}

pub fn suppress_dependent_after_drop(awaiting_key: bool, key: Option<bool>) -> bool {
    awaiting_key && key == Some(false)
}

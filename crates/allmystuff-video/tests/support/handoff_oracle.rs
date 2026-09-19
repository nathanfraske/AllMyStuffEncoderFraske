//! Compressed video is a reference chain, not a latest-picture mailbox.
//! Local residence time measures a stalled handoff; AU count does not, because
//! RTP repair can deliver several frames between two healthy GUI polls.
use std::collections::VecDeque;
use std::time::{Duration, Instant};

// Preserve the former six-AU queue's 30 fps residence budget, but measure it
// in local monotonic time. This is a discard deadline, never a playout delay.
const MAX_RESIDENCE: Duration = Duration::from_millis(200);
// Same scale as the media protocol's defensive frame cap. Includes metadata;
// not a bitrate/quality target. Batches also remain below node IPC's 256 MiB cap.
const MAX_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Enqueue {
    Enqueued { skipped: usize },
    AwaitingKey { started: bool },
    Converging { skipped: usize, started: bool },
}

struct Packet {
    data: Vec<u8>,
    at: Instant,
}
impl Packet {
    fn charge(&self) -> usize {
        self.data.len() + 4 + 2 * std::mem::size_of::<Self>()
    }
    fn key(&self) -> bool {
        self.data.first() == Some(&2) && self.data.get(1) == Some(&1)
    }
}

pub(crate) struct VideoHandoff {
    packets: VecDeque<Packet>,
    bytes: usize,
    awaiting_key: bool,
    convergence_requested: bool,
    max_age: Duration,
    max_bytes: usize,
}

impl Default for VideoHandoff {
    fn default() -> Self {
        Self {
            packets: VecDeque::new(),
            bytes: 0,
            awaiting_key: false,
            convergence_requested: false,
            max_age: MAX_RESIDENCE,
            max_bytes: MAX_BYTES,
        }
    }
}

impl VideoHandoff {
    pub fn len(&self) -> usize {
        self.packets.len()
    }
    fn clear(&mut self) {
        self.packets.clear();
        self.bytes = 0;
    }
    fn push(&mut self, packet: Packet) {
        self.bytes += packet.charge();
        self.packets.push_back(packet);
    }

    /// Self-contained JPEG/RGBA pictures may be superseded before paint.
    pub fn replace(&mut self, data: Vec<u8>) {
        self.clear();
        self.awaiting_key = false;
        self.convergence_requested = false;
        self.push(Packet {
            data,
            at: Instant::now(),
        });
    }

    pub fn push_h264(&mut self, data: Vec<u8>, now: Instant, gradual: bool) -> Enqueue {
        let packet = Packet { data, at: now };
        let key = packet.key();
        if self.awaiting_key && !gradual && !key {
            return Enqueue::AwaitingKey { started: false };
        }
        let too_large = packet.charge() > self.max_bytes;
        let expired = self
            .packets
            .front()
            .is_some_and(|p| now.duration_since(p.at) >= self.max_age);
        let full = self.bytes.saturating_add(packet.charge()) > self.max_bytes;
        if self.awaiting_key || expired || full {
            let mut skipped = 0;
            // Prefer the arriving key, then the newest queued key whose entire
            // suffix fits and is still fresh. Never trim arbitrary deltas.
            if !key && !too_large && !self.awaiting_key {
                if let Some(index) = self.packets.iter().rposition(Packet::key) {
                    let suffix_bytes: usize =
                        self.packets.iter().skip(index).map(Packet::charge).sum();
                    if now.duration_since(self.packets[index].at) < self.max_age
                        && suffix_bytes + packet.charge() <= self.max_bytes
                    {
                        for old in self.packets.drain(..index) {
                            skipped += 1;
                            self.bytes -= old.charge();
                        }
                        self.push(packet);
                        return Enqueue::Enqueued { skipped };
                    }
                }
            }
            skipped += self.packets.len();
            self.clear();
            if key && !too_large {
                self.awaiting_key = false;
                self.convergence_requested = false;
                self.push(packet);
                return Enqueue::Enqueued { skipped };
            }
            if gradual && !too_large {
                let started = !self.convergence_requested;
                self.convergence_requested = true;
                self.awaiting_key = false;
                self.push(packet);
                return Enqueue::Converging { skipped, started };
            }
            let started = !self.awaiting_key;
            self.awaiting_key = true;
            return Enqueue::AwaitingKey { started };
        }
        self.push(packet);
        Enqueue::Enqueued { skipped: 0 }
    }

    pub fn take_batch(&mut self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.packets.iter().map(|p| p.data.len() + 4).sum());
        for packet in self.packets.drain(..) {
            out.extend_from_slice(&(packet.data.len() as u32).to_le_bytes());
            out.extend_from_slice(&packet.data);
        }
        self.bytes = 0;
        self.convergence_requested = false;
        // Draining must NOT release an actual missing-reference fence.
        out
    }
}

// Test adapter appended to the verbatim frozen implementation above.
pub(super) fn with_limits(max_age: Duration, max_bytes: usize) -> VideoHandoff {
    VideoHandoff {
        max_age,
        max_bytes,
        ..VideoHandoff::default()
    }
}

pub(super) fn packet_charge(payload_len: usize, now: Instant) -> usize {
    Packet {
        data: vec![0; payload_len],
        at: now,
    }
    .charge()
}

pub(super) fn state(queue: &VideoHandoff) -> (usize, usize, bool, bool) {
    (
        queue.len(),
        queue.bytes,
        queue.awaiting_key,
        queue.convergence_requested,
    )
}

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::{RealtimeError, Result};

pub(crate) const HEADER: usize = 10;

pub(crate) struct Budget {
    limit: usize,
    used: AtomicUsize,
}

impl Budget {
    pub(crate) fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: AtomicUsize::new(0),
        })
    }

    pub(crate) fn reserve(self: &Arc<Self>, bytes: usize) -> Result<FrameLease> {
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|sum| *sum <= self.limit)
            })
            .map_err(|_| RealtimeError::LimitExceeded("retained frame bytes"))?;
        Ok(FrameLease {
            budget: self.clone(),
            bytes,
        })
    }
}

pub(crate) struct FrameLease {
    budget: Arc<Budget>,
    bytes: usize,
}

impl Drop for FrameLease {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

pub(crate) fn body_size(label_bytes: usize, payload_bytes: usize, ceiling: usize) -> Result<usize> {
    if label_bytes == 0 || u8::try_from(label_bytes).is_err() {
        return Err(RealtimeError::InvalidInput(
            "flow label must occupy 1..=255 bytes",
        ));
    }
    if u32::try_from(payload_bytes).is_err() {
        return Err(RealtimeError::LimitExceeded("payload wire width"));
    }
    let length = HEADER
        .checked_add(label_bytes)
        .and_then(|n| n.checked_add(payload_bytes))
        .ok_or(RealtimeError::LimitExceeded("binary body wire width"))?;
    if length > ceiling || u32::try_from(length).is_err() || length.checked_add(4).is_none() {
        return Err(RealtimeError::LimitExceeded("binary body bytes"));
    }
    Ok(length)
}

/// One owned inbound frame. Its aggregate budget lease remains held through
/// Drop, even when a caller retains this unit across later receive calls.
/// Application copies are separate caller-owned allocations.
pub struct ReceivedUnit {
    body: Vec<u8>,
    payload_start: usize,
    _lease: FrameLease,
}

impl ReceivedUnit {
    pub(crate) fn decode(body: Vec<u8>, lease: FrameLease) -> Result<Self> {
        let header = body
            .get(..HEADER)
            .ok_or(RealtimeError::InvalidResponse("short binary header"))?;
        let label_length = usize::from(header[0]);
        if label_length == 0 || header[1] > 1 {
            return Err(RealtimeError::InvalidResponse("binary label or marker"));
        }
        let payload_length =
            u32::from_le_bytes(header[6..10].try_into().expect("fixed header")) as usize;
        let payload_start = HEADER
            .checked_add(label_length)
            .ok_or(RealtimeError::InvalidResponse("binary lengths"))?;
        if payload_start.checked_add(payload_length) != Some(body.len()) {
            return Err(RealtimeError::InvalidResponse("binary lengths"));
        }
        Ok(Self {
            body,
            payload_start,
            _lease: lease,
        })
    }

    pub fn label(&self) -> &[u8] {
        &self.body[HEADER..self.payload_start]
    }
    pub fn payload(&self) -> &[u8] {
        &self.body[self.payload_start..]
    }
    pub fn marker(&self) -> bool {
        self.body[1] != 0
    }
    pub fn rtp_timestamp(&self) -> u32 {
        u32::from_le_bytes(self.body[2..6].try_into().expect("validated header"))
    }
}

impl fmt::Debug for ReceivedUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReceivedUnit")
            .field("label_bytes", &self.label().len())
            .field("payload_bytes", &self.payload().len())
            .field("marker", &self.marker())
            .field("rtp_timestamp", &self.rtp_timestamp())
            .finish()
    }
}

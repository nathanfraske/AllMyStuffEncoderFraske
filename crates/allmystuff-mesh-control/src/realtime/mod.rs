//! Optional, codec-independent realtime IPC for PR135/db7818e.
//!
//! Callers select this wire explicitly and keep application consent, route
//! selection, codecs, whole access-unit assembly and pacing above this module.
//! Neither a version nor an event capability approves an application or grants
//! access to a user's ledger. The shipping product's legacy guard is unchanged.
//!
//! An outbound unit is a whole codec unit, with a presentation duration rather
//! than an RTP timestamp. Inbound units preserve the upstream profile's marker
//! value (an entry-point bit for assembled H.264) and absolute RTP clock value.
//! There is no per-unit acknowledgment; a completed write proves only
//! local IPC progress. No handle reconnects or resolves a reused label.
//!
//! Use the explicit asynchronous close methods. Drop closes local sockets and
//! aborts owned work, but cannot acknowledge native retirement. In particular,
//! this upstream revision does not reliably observe a quiet event-socket EOF.
//! An unknown open capability or lost close acknowledgment therefore fences the
//! local owner and reports [`RealtimeError::CleanupUncertain`].

mod framing;
mod runtime;
mod state;
mod wire;

use std::fmt;
use std::time::Duration;

pub use framing::ReceivedUnit;
pub use runtime::{FlowTermination, InboundPipe, RealtimeFlow, RealtimeSession};

pub type Result<T> = std::result::Result<T, RealtimeError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowDirection {
    Inbound,
    Outbound,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RtpKind {
    Audio,
    Video,
}

/// An application-selected encoding family, not a codec implementation or an
/// SDP payload type. Values are forwarded without guessing or normalization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Encoding {
    pub rtp_kind: RtpKind,
    pub mime: String,
    pub clock_rate: u32,
    pub channels: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowSpec {
    pub network: String,
    pub peer: String,
    /// The exact UTF-8 spelling, with a wire width of 1..=255 bytes.
    pub label: String,
    pub direction: FlowDirection,
    pub encoding: Encoding,
}

/// Local resource and waiting limits supplied by the caller; no sample budget
/// or codec policy is silently selected by the transport.
#[derive(Clone, Debug)]
pub struct RealtimePolicy {
    pub max_flows: usize,
    pub max_inbound_pipes: usize,
    /// Inclusive binary BODY width: ten header bytes, label and payload. The
    /// outer four-byte prefix is additional, and counted by retained leases.
    pub max_frame_bytes: usize,
    /// Aggregate accounted frame bytes (body plus outer prefix), including
    /// returned ReceivedUnits until Drop. This is not allocator/RSS accounting
    /// and does not include copies independently made by the application.
    pub max_retained_bytes: usize,
    pub operation_timeout: Duration,
    pub io_timeout: Duration,
}

impl RealtimePolicy {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.max_flows == 0 || self.max_inbound_pipes == 0 {
            return Err(RealtimeError::InvalidPolicy("owner counts must be nonzero"));
        }
        if self.max_frame_bytes < 11
            || u32::try_from(self.max_frame_bytes).is_err()
            || self.max_frame_bytes.checked_add(4).is_none()
        {
            return Err(RealtimeError::InvalidPolicy(
                "binary body limit is not representable",
            ));
        }
        if self.max_retained_bytes < 15 {
            return Err(RealtimeError::InvalidPolicy(
                "retained budget cannot hold a minimum frame",
            ));
        }
        for deadline in [self.operation_timeout, self.io_timeout] {
            if deadline.is_zero() || std::time::Instant::now().checked_add(deadline).is_none() {
                return Err(RealtimeError::InvalidPolicy(
                    "deadlines must be finite and nonzero",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipeEnd {
    RemoteClosed,
    LocalClosed,
    SessionEnded,
    InvalidData,
    IoFailure,
}

/// Diagnostics deliberately contain no raw daemon responses or capabilities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RealtimeError {
    InvalidPolicy(&'static str),
    InvalidInput(&'static str),
    UnsupportedContract,
    StaleSession,
    SessionFenced,
    LimitExceeded(&'static str),
    Refused { code: Option<String> },
    InvalidResponse(&'static str),
    Transport,
    Timeout,
    PipeEnded(PipeEnd),
    CleanupUncertain,
}

impl fmt::Display for RealtimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPolicy(reason) => write!(f, "invalid realtime policy: {reason}"),
            Self::InvalidInput(reason) => write!(f, "invalid realtime input: {reason}"),
            Self::UnsupportedContract => {
                f.write_str("realtime requires the explicit candidate contract")
            }
            Self::StaleSession => {
                f.write_str("realtime session is stale or belongs to another client")
            }
            Self::SessionFenced => {
                f.write_str("realtime owner is fenced; close it before replacement")
            }
            Self::LimitExceeded(reason) => write!(f, "realtime resource limit: {reason}"),
            Self::Refused { code } => write!(f, "realtime operation refused (code: {code:?})"),
            Self::InvalidResponse(reason) => write!(f, "invalid realtime response: {reason}"),
            Self::Transport => f.write_str("realtime IPC failed"),
            Self::Timeout => f.write_str("realtime operation timed out"),
            Self::PipeEnded(reason) => write!(f, "realtime pipe ended: {reason:?}"),
            Self::CleanupUncertain => {
                f.write_str("realtime native retirement was not acknowledged")
            }
        }
    }
}

impl std::error::Error for RealtimeError {}

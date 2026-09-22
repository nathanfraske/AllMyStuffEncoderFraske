//! Thin adapters allowing the same observations beside both implementations.

use super::super::{CaptureSource, LevelStats, Playback, RouteAudio};
use allmystuff_session::AudioFrame;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

type AudioBridge = super::super::AudioBridge<super::TestStats>;

fn stats_log(line: String) {
    super::super::stats_log::<super::TestStats>(line);
}

fn metered<F>(
    route: &str,
    source: CaptureSource,
    callback: F,
) -> impl Fn(Vec<i16>, u32) + Send + Sync + 'static
where
    F: Fn(Vec<i16>, u32) + Send + Sync + 'static,
{
    super::super::metered::<super::TestStats, F>(route, source, callback)
}

fn observed_fill<T>(
    ring: &Mutex<VecDeque<i16>>,
    channels: usize,
    data: &mut [T],
    conv: impl Fn(i16) -> T,
) {
    fill!(ring, channels, data, conv);
}

// Compile the same observations against both sets of private model types.
// A re-export would make both sides exercise the same implementation.
#[allow(clippy::duplicate_mod)]
#[path = "io_harness.rs"]
pub(super) mod harness;

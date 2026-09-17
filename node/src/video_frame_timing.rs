//! Compatibility path for the reusable frame timing helpers.

#[cfg(feature = "host")]
pub(crate) use allmystuff_frame_timing::FrameCadence;
pub(crate) use allmystuff_frame_timing::{periodic_sample, send_breakdown, AssemblyClock};

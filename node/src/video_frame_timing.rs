//! Compatibility path for the reusable frame timing helpers.

#[cfg(feature = "host")]
pub(crate) use allmystuff_video::timing::FrameCadence;
pub(crate) use allmystuff_video::timing::{periodic_sample, send_breakdown, AssemblyClock};

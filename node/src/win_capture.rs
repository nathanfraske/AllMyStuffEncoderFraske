//! Compatibility path for Windows capture with node-owned desktop following.
#![cfg(all(windows, feature = "host"))]

pub use allmystuff_video::win_capture::*;

pub fn start(monitor_id: u32) -> Result<StartedDuplication, String> {
    allmystuff_video::win_capture::start::<crate::win_privilege::DesktopFollower>(monitor_id)
}

pub fn start_named(
    monitor_id: u32,
    stable_name: Option<&str>,
) -> Result<StartedDuplication, String> {
    allmystuff_video::win_capture::start_named::<crate::win_privilege::DesktopFollower>(
        monitor_id,
        stable_name,
    )
}

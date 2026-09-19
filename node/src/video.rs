//! Compatibility path for host video processing with node-owned desktop policy.

pub use allmystuff_video::video::*;

pub type VideoBridge =
    allmystuff_video::video::VideoBridge<crate::win_privilege::DesktopFollower>;

impl allmystuff_video::host::DesktopFollower for crate::win_privilege::DesktopFollower {
    fn new() -> Self {
        Self::new()
    }

    fn follow(&mut self) -> bool {
        self.follow()
    }

    fn desktop_name(&self) -> &str {
        self.desktop_name()
    }

    fn on_secure_desktop(&self) -> bool {
        self.on_secure_desktop()
    }
}

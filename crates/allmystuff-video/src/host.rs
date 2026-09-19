//! Application-owned desktop policy used by Windows capture.

/// Constructed and retained on the existing capture pump thread.
/// Implementations retain ownership of desktop permissions and handles.
pub trait DesktopFollower: 'static {
    fn new() -> Self;
    fn follow(&mut self) -> bool;
    fn desktop_name(&self) -> &str;
    fn on_secure_desktop(&self) -> bool;
}

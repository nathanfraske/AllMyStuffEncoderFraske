// on every desktop/server build, swapped for same-API no-op stubs (see
// `stubs/`) on a capture-less build — iOS, whose sandbox has no PTY, screen
// grab, input injection, or OS clipboard to offer. The stub keeps the
// module's exact public surface so `mesh.rs` (and everything else) compiles
// identically either way; only what a route *does* changes (capture routes
// report failure, inject/clipboard writes drop). Viewer planes —
// `video_decode`, the terminal output queues, opus decode — are NOT gated:
// watching other machines is the whole point of a capture-less node.
// Audio is gated separately (`audio-io`, included in `host`): it's the one
// capture plane iOS can genuinely run — cpal speaks CoreAudio there — so
// the phone builds it real while the rest stay stubs.
/// AMD AMF encode — the Radeon twin of `nvenc`, in progress (loader +
/// availability probe today; component vtables next). Runtime-loaded from
/// the Radeon driver's DLL; absent driver = absent rung, softly.
#[cfg(all(windows, feature = "host"))]
pub mod amf;
#[cfg(feature = "audio-io")]
pub mod audio;
#[cfg(not(feature = "audio-io"))]
#[path = "stubs/audio.rs"]
pub mod audio;
pub mod byte_queues;

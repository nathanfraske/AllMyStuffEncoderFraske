/// Whether per-stream dial-in stats log at info — mirrored so shared call
/// sites in `mesh.rs` compile; nothing here ever produces a stream to log.
pub fn stats_to_info() -> bool {
    false
}

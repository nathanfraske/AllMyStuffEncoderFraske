/// Whether the periodic pipeline stats print at info. Off by default —
/// steady-state runs stay quiet; set `ALLMYSTUFF_VIDEO_STATS=1` while
/// dialing performance in (without it the same lines land at debug, so
/// the `ALLMYSTUFF_GUI_LOG` filter can also reach them).
pub fn stats_to_info() -> bool {
    static ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        std::env::var("ALLMYSTUFF_VIDEO_STATS").is_ok_and(|v| !v.is_empty() && v != "0")
    });
    *ON
}

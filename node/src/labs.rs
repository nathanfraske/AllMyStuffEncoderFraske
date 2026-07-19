//! The Experimental ("Labs") tier gate — one choke point for every
//! out-of-the-box feature the field-trial arc adds
//! (`docs/fork/EXPERIMENTAL-ARC-PLAN-2026-07.md`).
//!
//! The contract that lets the GUI be DONE and all future Labs work be
//! pure pipeline: the Mode dropdown's Experimental toggle flips this
//! gate at runtime (via the `labs_set` control op), and every future
//! feature reads it as `if labs::on(Feature::Foo) { … } else { /*
//! today's shipped path, untouched */ }`. No feature ever needs a new
//! GUI control — the toggle already exists.
//!
//! Two layers, tri-state precedence (mirrors `ALLMYSTUFF_RATE_ADAPT`'s
//! proven pattern):
//!  1. **Tier** — `ALLMYSTUFF_EXPERIMENTAL=1` at boot, OR the GUI toggle
//!     at runtime. Off = every feature inert, the build behaves exactly
//!     like today (the tier is the outer wall).
//!  2. **Per-feature dial** — `ALLMYSTUFF_X_<FEATURE>` at boot, or the
//!     runtime override the GUI/ops set. Unset under an on tier = the
//!     feature's curated default (conservative: off until its arc phase
//!     graduates it). `=0` hard-off, `=1` on.
//!
//! Everything is best-effort and lock-free on the read path (an atomic
//! per feature): the encode/decode hot loops read a gate per frame the
//! same way they read `RouteRate.target`.

use std::sync::atomic::{AtomicU8, Ordering};

/// The tier's master runtime state. `0` = follow env (the boot default),
/// `1` = forced on, `2` = forced off — the GUI toggle writes 1/2.
static TIER: AtomicU8 = AtomicU8::new(0);

/// Every Labs feature — one variant per dial in the Experimental arc
/// plan (`docs/fork/EXPERIMENTAL-ARC-PLAN-2026-07.md` §1.1), so the gate is
/// complete and implementing a feature is filling its call site's
/// `if labs::on(Feature::X)` branch, never adding a gate. Adding a
/// genuinely new experiment = a variant here + a line in [`slot`] and
/// [`env_spec`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    /// T2.9 — damage-metadata pixel grouping.
    Damage,
    /// T2.6 — capture-clock paint pacing (GUI-side; the flag rides here
    /// so one gate governs the whole tier).
    PaintPace,
    /// T2.2 — NVENC sub-frame slice streaming.
    SubFrame,
    /// T2.5a — damage-QP emphasis.
    QpMap,
    /// T2.5b — adaptive slice grain.
    Grain,
    /// T2.7 — steady-state wave-period stretch (the loss-aware wave
    /// LENGTH already ships; this is the clean-link period stretch).
    WaveStretch,
    /// T2.8 — viewer zero-copy present.
    Present,
    /// T1.4 — LTR-anchored recovery.
    Ltr,
    /// T2.3 — arrival-side loss inference (gap-NACK) + the recovery matrix.
    GapNack,
    /// T2.4 — speculative rescue layer.
    Rescue,
    /// Fence/async encode submit chain.
    EncAsync,
}

/// One override slot per feature: `0` = follow env, `1` = on, `2` = off.
fn slot(f: Feature) -> &'static AtomicU8 {
    static DAMAGE: AtomicU8 = AtomicU8::new(0);
    static PAINT_PACE: AtomicU8 = AtomicU8::new(0);
    static SUBFRAME: AtomicU8 = AtomicU8::new(0);
    static QPMAP: AtomicU8 = AtomicU8::new(0);
    static GRAIN: AtomicU8 = AtomicU8::new(0);
    static WAVE_STRETCH: AtomicU8 = AtomicU8::new(0);
    static PRESENT: AtomicU8 = AtomicU8::new(0);
    static LTR: AtomicU8 = AtomicU8::new(0);
    static GAP_NACK: AtomicU8 = AtomicU8::new(0);
    static RESCUE: AtomicU8 = AtomicU8::new(0);
    static ENC_ASYNC: AtomicU8 = AtomicU8::new(0);
    match f {
        Feature::Damage => &DAMAGE,
        Feature::PaintPace => &PAINT_PACE,
        Feature::SubFrame => &SUBFRAME,
        Feature::QpMap => &QPMAP,
        Feature::Grain => &GRAIN,
        Feature::WaveStretch => &WAVE_STRETCH,
        Feature::Present => &PRESENT,
        Feature::Ltr => &LTR,
        Feature::GapNack => &GAP_NACK,
        Feature::Rescue => &RESCUE,
        Feature::EncAsync => &ENC_ASYNC,
    }
}

/// The feature's boot env var name and its curated default when the tier
/// is on but the dial is unset. Curated defaults are conservative — a
/// feature graduates to default-on only when its arc phase's exit
/// criteria pass; until then tier-on still needs the explicit dial.
fn env_spec(f: Feature) -> (&'static str, bool) {
    match f {
        Feature::Damage => ("ALLMYSTUFF_X_DAMAGE", false),
        Feature::PaintPace => ("ALLMYSTUFF_X_PAINT_PACE", false),
        Feature::SubFrame => ("ALLMYSTUFF_X_SUBFRAME", false),
        Feature::QpMap => ("ALLMYSTUFF_X_QPMAP", false),
        Feature::Grain => ("ALLMYSTUFF_X_GRAIN", false),
        Feature::WaveStretch => ("ALLMYSTUFF_X_WAVE_STRETCH", false),
        Feature::Present => ("ALLMYSTUFF_X_PRESENT", false),
        Feature::Ltr => ("ALLMYSTUFF_X_LTR", false),
        Feature::GapNack => ("ALLMYSTUFF_X_GAP_NACK", false),
        Feature::Rescue => ("ALLMYSTUFF_X_RESCUE", false),
        Feature::EncAsync => ("ALLMYSTUFF_X_ENC_ASYNC", false),
    }
}

fn env_on(key: &str) -> Option<bool> {
    std::env::var(key).ok().map(|v| {
        !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "off" | "false" | ""
        )
    })
}

/// Is the Experimental tier active right now? Runtime override wins over
/// the boot env; unset env = off (the safe wall).
pub fn tier() -> bool {
    match TIER.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => env_on("ALLMYSTUFF_EXPERIMENTAL").unwrap_or(false),
    }
}

/// Is `f` engaged? False whenever the tier is off — no feature acts
/// outside the tier, so a stray `X_` var can never change a stock box.
pub fn on(f: Feature) -> bool {
    if !tier() {
        return false;
    }
    match slot(f).load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let (key, default) = env_spec(f);
            env_on(key).unwrap_or(default)
        }
    }
}

/// Runtime tier toggle — the Mode dropdown's Experimental switch, via
/// the `labs_set` op. Logged so an experimental session is self-
/// describing in the field log (the plan's `labs state` line).
pub fn set_tier(active: bool) {
    TIER.store(if active { 1 } else { 2 }, Ordering::Relaxed);
    tracing::info!(
        "labs state: experimental tier {}",
        if active { "ON" } else { "off" }
    );
}

/// Runtime per-feature toggle (a Labs sheet row, when one lands). Named
/// by the wire strings the op passes so the GUI never hard-codes the
/// enum. Unknown names are ignored (forward-compatible with a newer GUI).
pub fn set_feature(name: &str, on: bool) {
    let f = match name {
        "damage" => Feature::Damage,
        "paint_pace" => Feature::PaintPace,
        "subframe" => Feature::SubFrame,
        "qpmap" => Feature::QpMap,
        "grain" => Feature::Grain,
        "wave_stretch" => Feature::WaveStretch,
        "present" => Feature::Present,
        "ltr" => Feature::Ltr,
        "gap_nack" => Feature::GapNack,
        "rescue" => Feature::Rescue,
        "enc_async" => Feature::EncAsync,
        _ => {
            tracing::debug!("labs_set: unknown feature {name}");
            return;
        }
    };
    slot(f).store(if on { 1 } else { 2 }, Ordering::Relaxed);
    tracing::info!(
        "labs state: feature {name} {}",
        if on { "on" } else { "off" }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tier is the outer wall: with it off, no feature engages no
    /// matter what its own dial says — the "off means off" field
    /// contract. Runtime override beats env; per-feature override works
    /// only inside an on tier.
    #[test]
    fn tier_gates_every_feature() {
        // Forced off: every feature off, even one forced on.
        set_tier(false);
        slot(Feature::Damage).store(1, Ordering::Relaxed);
        assert!(!on(Feature::Damage), "tier off overrides a feature-on");
        // Forced on: the forced feature engages, an unset one follows its
        // conservative default (off), a forced-off one stays off.
        set_tier(true);
        assert!(on(Feature::Damage), "tier on + feature on");
        assert!(
            !on(Feature::SubFrame),
            "unset feature = conservative default"
        );
        slot(Feature::Present).store(2, Ordering::Relaxed);
        assert!(
            !on(Feature::Present),
            "feature forced off inside an on tier"
        );
        // set_feature by wire name flips the same slot.
        set_feature("subframe", true);
        assert!(on(Feature::SubFrame), "set_feature engaged it");
        // Reset the process-global state so test order can't leak.
        set_tier(false);
        for f in [Feature::Damage, Feature::SubFrame, Feature::Present] {
            slot(f).store(0, Ordering::Relaxed);
        }
        TIER.store(0, Ordering::Relaxed);
    }
}

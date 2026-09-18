use allmystuff_video_pacing as pacing;
use std::time::{Duration, Instant};

#[path = "support/vectors.rs"]
mod vectors;

fn microseconds(values: &[u64]) -> Vec<Duration> {
    values.iter().copied().map(Duration::from_micros).collect()
}

#[test]
fn public_policy_vectors_preserve_floors_headroom_ceiling_and_overrides() {
    let rates = [
        8_000_000,
        16_000_000,
        30_000_000,
        25_000_000,
        32_000_000,
        30_000_000,
        40_000_000,
        80_000_000,
        4_294_967_295,
        8_000_000,
        8_000_000,
        8_000_000,
        17_000_000,
        18_446_744_073_709_000_000,
    ];
    let expected: Vec<_> = rates.into_iter().map(|rate| (rate, 98_304)).collect();
    assert_eq!(vectors::policy_trace(Instant::now()), expected);
}

#[test]
fn public_frame_vectors_keep_direction_override_and_ceiling_rules() {
    assert_eq!(
        vectors::frame_trace(Instant::now()),
        vec![
            (16_000_000, 98_304),
            (16_000_000, 98_304),
            (24_000_000, 98_304),
            (48_000_000, 98_304),
            (192_000_000, 98_304),
            (256_000_000, 98_304),
            (16_000_000, 98_304),
            (16_000_000, 98_304),
            (256_000_000, 73),
            (400_000_000, 73),
            (400_000_000, 73),
        ]
    );
}

#[test]
fn public_state_trace_keeps_debt_zero_reservations_and_idle_burst() {
    assert_eq!(
        vectors::debt_trace(Instant::now()),
        microseconds(&[0, 1, 6, 5, 0, 0, 4, 1])
    );
}

#[test]
fn public_state_trace_keeps_outstanding_debt_across_policy_changes() {
    assert_eq!(
        vectors::policy_change_trace(Instant::now()),
        microseconds(&[0, 8, 12, 10, 0, 0, 1, 2])
    );
}

#[test]
fn public_state_trace_preserves_zero_rate_and_rounding_boundaries() {
    assert_eq!(
        vectors::boundary_trace(Instant::now()),
        microseconds(&[
            0, 8_000_000, 16_000_000, 0, 8_000_000, 2_666_667, 5_333_334, 2_666_667, 1, 2,
        ])
    );
}

#[test]
fn public_state_trace_accepts_large_values_with_representable_deadlines() {
    assert_eq!(
        vectors::large_trace(Instant::now()),
        microseconds(&[1, 2, 0, 1, 0])
    );
}

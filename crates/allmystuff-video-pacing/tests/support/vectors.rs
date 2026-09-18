use super::pacing::{frame_policy, pace_policy, PacePolicy, PaceRouteState};
use std::time::{Duration, Instant};

pub fn policy_trace(_start: Instant) -> Vec<(u64, u64)> {
    [
        (false, true, 0, 0),
        (false, false, 0, 0),
        (false, true, 20_000_000, 0),
        (true, true, 20_000_000, 0),
        (false, false, 24_000_000, 0),
        (true, false, 24_000_000, 0),
        (false, true, 40_000_000, 0),
        (false, false, 80_000_000, 0),
        (true, false, u32::MAX, 0),
        (false, true, 1, 1),
        (true, false, 100_000_000, 7),
        (false, false, 0, 8),
        (false, true, 1, 17),
        (false, false, 0, 18_446_744_073_709),
    ]
    .into_iter()
    .map(|(game, wan, rate, limit)| {
        let policy = pace_policy(game, wan, rate, limit);
        (policy.drain_bps, policy.burst_bytes)
    })
    .collect()
}

pub fn frame_trace(_start: Instant) -> Vec<(u64, u64)> {
    let base = pace_policy(false, false, 8_000_000, 0);
    let high = PacePolicy {
        drain_bps: 400_000_000,
        burst_bytes: 73,
    };
    [
        (base, false, 0, 1000, 0),
        (base, false, 0, 100_000, 1),
        (base, false, 0, 100_000, 30),
        (base, false, 0, 100_000, 60),
        (base, false, 0, 100_000, 300),
        (base, false, 0, usize::MAX, u32::MAX),
        (base, true, 0, usize::MAX, 60),
        (base, false, 1, usize::MAX, 60),
        (high, false, 0, 0, 1),
        (high, true, 0, 0, 1),
        (high, false, 1, 0, 1),
    ]
    .into_iter()
    .map(|(base, wan, limit, bytes, fps)| {
        let policy = frame_policy(base, wan, limit, bytes, fps);
        (policy.drain_bps, policy.burst_bytes)
    })
    .collect()
}

fn reservations(
    start: Instant,
    initial: PacePolicy,
    steps: &[(u64, usize, PacePolicy)],
) -> Vec<Duration> {
    let mut bucket = PaceRouteState::full(start, initial);
    steps
        .iter()
        .map(|&(at_us, bytes, policy)| {
            bucket.reserve(start + Duration::from_micros(at_us), bytes, policy)
        })
        .collect()
}

pub fn debt_trace(start: Instant) -> Vec<Duration> {
    let policy = PacePolicy {
        drain_bps: 8_000_000,
        burst_bytes: 4,
    };
    reservations(
        start,
        policy,
        &[
            (0, 3, policy),
            (0, 2, policy),
            (0, 5, policy),
            (2, 1, policy),
            (2, 0, policy),
            (10, 2, policy),
            (20, 8, policy),
            (24, 1, policy),
        ],
    )
}

pub fn policy_change_trace(start: Instant) -> Vec<Duration> {
    let initial = PacePolicy {
        drain_bps: 8_000_000,
        burst_bytes: 8,
    };
    let faster = PacePolicy {
        drain_bps: 16_000_000,
        burst_bytes: 8,
    };
    let smaller = PacePolicy {
        drain_bps: 4_000_000,
        burst_bytes: 2,
    };
    reservations(
        start,
        initial,
        &[
            (0, 8, initial),
            (0, 8, initial),
            (0, 8, faster),
            (4, 1, smaller),
            (20, 1, smaller),
            (20, 1, initial),
            (20, 1, initial),
            (40, 3, smaller),
        ],
    )
}

pub fn boundary_trace(start: Instant) -> Vec<Duration> {
    let zero = PacePolicy {
        drain_bps: 0,
        burst_bytes: 0,
    };
    let small = PacePolicy {
        drain_bps: 3,
        burst_bytes: 0,
    };
    let sub_microsecond = PacePolicy {
        drain_bps: 16_000_000,
        burst_bytes: 0,
    };
    let mut trace = reservations(
        start,
        zero,
        &[
            (0, 0, zero),
            (0, 1, zero),
            (0, 1, zero),
            (20_000_000, 0, zero),
            (20_000_000, 1, zero),
        ],
    );
    trace.extend(reservations(
        start,
        small,
        &[(0, 1, small), (0, 1, small), (6_000_000, 1, small)],
    ));
    trace.extend(reservations(
        start,
        sub_microsecond,
        &[(0, 1, sub_microsecond), (0, 1, sub_microsecond)],
    ));
    trace
}

pub fn large_trace(start: Instant) -> Vec<Duration> {
    let fast = PacePolicy {
        drain_bps: u64::MAX,
        burst_bytes: 0,
    };
    let large = PacePolicy {
        drain_bps: u64::MAX,
        burst_bytes: 4_000_000_000,
    };
    let mut trace = reservations(
        start,
        fast,
        &[(0, 1_000_000_000, fast), (0, 1_000_000_000, fast)],
    );
    trace.extend(reservations(
        start,
        large,
        &[
            (0, 4_000_000_000, large),
            (0, 4_000_000_000, large),
            (1_000_000, 4_000_000_000, large),
        ],
    ));
    trace
}

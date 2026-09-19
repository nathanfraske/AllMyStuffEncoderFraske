#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuSequenceObservation {
    Accept,
    Gap,
    DropDuplicateOrStale,
}

/// Classify one route-local AU sequence without guessing from timestamps.
/// Clean entries are allowed to reset a sender that restarted in place;
/// duplicates/stale units never move the high-water mark backwards.
fn observe_au_sequence(
    previous: Option<u64>,
    incoming: u64,
    clean_entry: bool,
) -> AuSequenceObservation {
    let Some(previous) = previous else {
        return AuSequenceObservation::Accept;
    };
    if incoming == previous {
        return AuSequenceObservation::DropDuplicateOrStale;
    }
    let expected = previous.wrapping_add(1);
    if incoming == expected || clean_entry {
        return AuSequenceObservation::Accept;
    }
    if incoming.wrapping_sub(expected) < (1u64 << 63) {
        AuSequenceObservation::Gap
    } else {
        AuSequenceObservation::DropDuplicateOrStale
    }
}

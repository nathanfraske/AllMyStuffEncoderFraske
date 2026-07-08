//! The **Support ID** — the short number a customer reads to a technician.
//!
//! It is a deterministic function of the device's public key, so a technician
//! never needs a directory or a central server: their app sees a peer's
//! `device_id` at the signaling layer (before any connection) and computes
//! that peer's Support ID locally to match the number the customer gave.
//!
//! Format: **9 decimal digits** (numbers only — nothing to spell out or misread
//! over the phone), shown grouped as `NNN NNN NNN`. Derived by reducing the
//! first 64 bits of `SHA-256(device_id_bytes)` modulo 1e9. A billion values —
//! collisions are irrelevant because the Support ID is only a *lookup hint*: the
//! actual connection is still mutually authenticated by ed25519 at the mesh
//! layer, and the customer confirms the technician by name and by the 6-digit
//! verification code before approving.

use sha2::{Digest, Sha256};

/// Number of digits in a [`SupportId`].
pub const SUPPORT_ID_LEN: usize = 9;

/// A customer's Support ID — a 9-digit numeric code derived from their device
/// id. Cheap to clone; compares digit-for-digit via [`SupportId::matches`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SupportId(String);

impl SupportId {
    /// Derive the Support ID for a device from its `device_id` (the base32
    /// ed25519 public key MyOwnMesh puts on the wire). Any stable string id
    /// works; the same input always yields the same Support ID on every
    /// platform.
    pub fn from_device(device_id: &str) -> Self {
        SupportId(support_id_from_device(device_id))
    }

    /// The raw 9-digit code, e.g. `"123456789"`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Grouped for display, e.g. `"123 456 789"`.
    pub fn grouped(&self) -> String {
        format_support_id(&self.0)
    }

    /// Whether a user-typed string names this Support ID. Tolerant of the
    /// grouping spaces (or dashes) and of read-alikes a customer might voice —
    /// "oh" for zero, "el"/"eye" for one — so `123-456-789`, `123 456 789`, and
    /// `123456789` all match.
    pub fn matches(&self, input: &str) -> bool {
        normalize_input(input) == self.0
    }
}

impl std::fmt::Display for SupportId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Derive the 9-digit Support ID string for a device id. See [`SupportId`].
pub fn support_id_from_device(device_id: &str) -> String {
    let digest = Sha256::digest(device_id.as_bytes());
    // First 8 bytes = 64 bits, reduced modulo 1e9 to a 9-digit number. 2^64 is
    // ~1.8e10 × 1e9, so the modulo bias is negligible.
    let mut acc: u64 = 0;
    for &b in &digest[..8] {
        acc = (acc << 8) | u64::from(b);
    }
    let n = acc % 1_000_000_000;
    format!("{n:09}")
}

/// Group the digits for readability: `123456789` → `123 456 789`. Purely
/// cosmetic; [`normalize_input`] strips the spaces back out.
pub fn format_support_id(code: &str) -> String {
    let digits: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() == SUPPORT_ID_LEN {
        format!("{} {} {}", &digits[0..3], &digits[3..6], &digits[6..9])
    } else {
        code.to_string()
    }
}

/// Canonicalise user input for matching: keep only the digits (so spaces,
/// dashes, and pasted labels are tolerated), folding the read-alikes a customer
/// might voice — `O` → `0`, `I`/`L` → `1` — so a spoken "oh" or "eye" still
/// lands on the right number. Everything else is dropped.
pub fn normalize_input(input: &str) -> String {
    input
        .chars()
        .filter_map(|c| match c.to_ascii_uppercase() {
            'O' => Some('0'),
            'I' | 'L' => Some('1'),
            c @ '0'..='9' => Some(c),
            _ => None,
        })
        .collect()
}

/// Derive the per-customer MyOwnMesh `network_id` — the Silent mesh room — from
/// a number. The number is canonicalised with [`normalize_input`] (so a
/// technician may paste it grouped or with read-alike folds) and prefixed with
/// [`CEC_NETWORK_PREFIX`](crate::CEC_NETWORK_PREFIX). The result satisfies
/// MyOwnMesh's `normalize_network_id` (`[a-z0-9-_]`, length 3..=64), so it can
/// be joined verbatim. Because the id is a pure function of the number, the
/// customer and the technician independently derive the same room from the same
/// number — no directory, no server.
pub fn network_id_for_number(number: &str) -> String {
    format!("{}{}", crate::CEC_NETWORK_PREFIX, normalize_input(number))
}

/// The Silent mesh a device joins for itself:
/// `network_id_for_number(support_id_from_device(device_id))`.
pub fn network_id_for_device(device_id: &str) -> String {
    network_id_for_number(&support_id_from_device(device_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_deterministic_and_well_formed() {
        let dev = "k3m2pq7xabcdefghijklmnopqrstuvwxyz234567abcdefghijkl";
        let a = support_id_from_device(dev);
        let b = support_id_from_device(dev);
        assert_eq!(a, b, "same device -> same id");
        assert_eq!(a.len(), SUPPORT_ID_LEN);
        assert!(
            a.bytes().all(|c| c.is_ascii_digit()),
            "id {a} is digits only"
        );
    }

    #[test]
    fn different_devices_differ() {
        assert_ne!(
            support_id_from_device("device-alpha"),
            support_id_from_device("device-beta")
        );
    }

    #[test]
    fn grouping_round_trips_through_normalize() {
        let id = SupportId::from_device("some-device-id");
        let grouped = id.grouped();
        assert!(grouped.contains(' '), "grouped {grouped} has spaces");
        assert_eq!(normalize_input(&grouped), id.as_str());
    }

    #[test]
    fn matches_is_tolerant() {
        let id = SupportId::from_device("device-alpha");
        let raw = id.as_str().to_string();
        assert!(id.matches(&raw));
        assert!(id.matches(&format!("  {}  ", id.grouped())));
        // A dash-grouped form the technician might type still matches.
        let dashed = format!("{}-{}-{}", &raw[0..3], &raw[3..6], &raw[6..9]);
        assert!(id.matches(&dashed));
        assert!(!id.matches("000000000"));
    }

    #[test]
    fn normalize_keeps_digits_and_folds_read_alikes() {
        // A customer who reads O for zero and l/I for one still matches.
        assert_eq!(normalize_input("O1lI"), "0111");
        // Spaces, dashes, and stray letters are dropped.
        assert_eq!(normalize_input("123 456-789 abc"), "123456789");
    }

    #[test]
    fn network_id_is_derived_and_valid() {
        let id = SupportId::from_device("device-alpha");
        let net = network_id_for_number(id.as_str());
        assert!(net.starts_with("cec-"));
        // Grouped input resolves to the same room.
        assert_eq!(network_id_for_number(&id.grouped()), net);
        // Valid MyOwnMesh network_id shape: [a-z0-9-_], length 3..=64.
        assert!((3..=64).contains(&net.len()));
        assert!(net
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_')));
    }

    #[test]
    fn device_and_number_agree_on_the_room() {
        let dev = "device-beta";
        assert_eq!(
            network_id_for_device(dev),
            network_id_for_number(&support_id_from_device(dev))
        );
    }
}

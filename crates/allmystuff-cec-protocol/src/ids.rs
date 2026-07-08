//! The **Support ID** — the short number a customer reads to a technician.
//!
//! It is a deterministic function of the device's public key, so a technician
//! never needs a directory or a central server: their app sees a peer's
//! `device_id` at the signaling layer (before any connection) and computes
//! that peer's Support ID locally to match the number the customer gave.
//!
//! Format: 8 characters of **Crockford base32**
//! (`0123456789ABCDEFGHJKMNPQRSTVWXYZ` — no `I`, `L`, `O`, `U`, so it can't be
//! misread over the phone), upper-cased. Derived as the first 40 bits of
//! `SHA-256(device_id_bytes)`. 40 bits ≈ 1.1e12 values — collisions are
//! irrelevant because the Support ID is only a *lookup hint*: the actual
//! connection is still mutually authenticated by ed25519 at the mesh layer,
//! and the customer confirms the technician by name and by the 6-char
//! verification code before approving.

use sha2::{Digest, Sha256};

/// Number of characters in a [`SupportId`].
pub const SUPPORT_ID_LEN: usize = 8;

/// Crockford base32 alphabet: digits + upper-case letters minus `I L O U`.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A customer's Support ID — an 8-char Crockford-base32 code derived from
/// their device id. Cheap to clone; compares case-insensitively via
/// [`SupportId::matches`].
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

    /// The raw 8-char code, e.g. `"XY400SHD"`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Grouped for display, e.g. `"XY40-0SHD"`.
    pub fn grouped(&self) -> String {
        format_support_id(&self.0)
    }

    /// Whether a user-typed string names this Support ID. Tolerant of case,
    /// whitespace, and the grouping dash so a technician can paste `xy40-0shd`
    /// or `XY400SHD` interchangeably.
    pub fn matches(&self, input: &str) -> bool {
        normalize_input(input) == self.0
    }
}

impl std::fmt::Display for SupportId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Derive the 8-char Support ID string for a device id. See [`SupportId`].
pub fn support_id_from_device(device_id: &str) -> String {
    let digest = Sha256::digest(device_id.as_bytes());
    // First 5 bytes = 40 bits = exactly 8 base32 symbols.
    let mut acc: u64 = 0;
    for &b in &digest[..5] {
        acc = (acc << 8) | u64::from(b);
    }
    let mut out = String::with_capacity(SUPPORT_ID_LEN);
    for i in 0..SUPPORT_ID_LEN {
        let shift = 40 - 5 * (i + 1);
        let idx = ((acc >> shift) & 0x1f) as usize;
        out.push(CROCKFORD[idx] as char);
    }
    out
}

/// Insert a single dash at the midpoint for readability: `XY400SHD` →
/// `XY40-0SHD`. Purely cosmetic; [`normalize_input`] strips it back out.
pub fn format_support_id(code: &str) -> String {
    let up = code.to_ascii_uppercase();
    if up.len() == SUPPORT_ID_LEN {
        let (a, b) = up.split_at(SUPPORT_ID_LEN / 2);
        format!("{a}-{b}")
    } else {
        up
    }
}

/// Canonicalise user input for matching: upper-case, drop everything that
/// isn't a Crockford symbol (so spaces, dashes, and pasted labels are
/// tolerated). Also applies Crockford's read-alike folds (`I`/`L` → `1`,
/// `O` → `0`) so a customer reading "oh" for zero still matches.
pub fn normalize_input(input: &str) -> String {
    input
        .chars()
        .filter_map(|c| {
            let c = c.to_ascii_uppercase();
            match c {
                'O' => Some('0'),
                'I' | 'L' => Some('1'),
                'U' => None, // never emitted; treat as noise
                '0'..='9' | 'A'..='Z' => Some(c),
                _ => None,
            }
        })
        .collect()
}

/// Derive the per-customer MyOwnMesh `network_id` — the Silent mesh room — from
/// a number. The number is canonicalised with [`normalize_input`] (so a
/// technician may paste it grouped, lower-case, or with read-alike folds),
/// lower-cased, and prefixed with [`CEC_NETWORK_PREFIX`](crate::CEC_NETWORK_PREFIX).
/// The result satisfies MyOwnMesh's `normalize_network_id` (`[a-z0-9-_]`,
/// length 3..=64), so it can be joined verbatim. Because the id is a pure
/// function of the number, the customer and the technician independently derive
/// the same room from the same number — no directory, no server.
pub fn network_id_for_number(number: &str) -> String {
    format!(
        "{}{}",
        crate::CEC_NETWORK_PREFIX,
        normalize_input(number).to_ascii_lowercase()
    )
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
            a.bytes().all(|c| CROCKFORD.contains(&c)),
            "id {a} uses only Crockford symbols"
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
        assert!(grouped.contains('-'));
        assert_eq!(normalize_input(&grouped), id.as_str());
    }

    #[test]
    fn matches_is_tolerant() {
        let id = SupportId::from_device("device-alpha");
        let raw = id.as_str().to_string();
        assert!(id.matches(&raw));
        assert!(id.matches(&raw.to_lowercase()));
        assert!(id.matches(&format!("  {}  ", id.grouped())));
        assert!(!id.matches("00000000"));
    }

    #[test]
    fn normalize_folds_read_alikes() {
        // A customer who reads O for zero and l for one still matches.
        assert_eq!(normalize_input("O1lI"), "0111");
    }

    #[test]
    fn network_id_is_derived_and_valid() {
        let id = SupportId::from_device("device-alpha");
        let net = network_id_for_number(id.as_str());
        assert!(net.starts_with("cec-"));
        // Grouped / lower-case input resolves to the same room.
        assert_eq!(network_id_for_number(&id.grouped()), net);
        assert_eq!(network_id_for_number(&id.as_str().to_lowercase()), net);
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

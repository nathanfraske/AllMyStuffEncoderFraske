//! Device ownership — the persisted answer to "whose machine is this?"
//!
//! AllMyStuff's rule (the one the user asked for): you can't *flat-out take
//! ownership* of a device on the mesh. A claim only takes if the device is
//! in **claim mode** — started with `ALLMYSTUFF_CLAIMABLE=1`, or with its
//! owner having toggled "allow adoption" in its own UI. Otherwise the device
//! defines its own owner (whatever it has recorded, or nobody), and a peer
//! can look but not grab.
//!
//! Two pieces of state, treated differently on purpose:
//!
//!  * **owner** — *persisted* next to the mesh identity under `~/.myownmesh`
//!    (`MYOWNMESH_HOME`-overridable), so a device remembers who owns it
//!    across restarts exactly like the mesh remembers its keys.
//!  * **claim mode** — *not* persisted. It's a transient "I'm offering this
//!    device right now" state, re-asserted each start by the
//!    `ALLMYSTUFF_CLAIMABLE` flag, so a box never sits silently adoptable
//!    across reboots after you toggled it on once.

use std::path::PathBuf;

use allmystuff_graph::NodeId;
use allmystuff_protocol::{OwnedMember, OwnedRoster};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// The durable part of the record. The owner *and* the owned-fleet roster
/// survive a restart (only claim mode is transient). Additive +
/// `#[serde(default)]` so an older file (or none) still loads.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Persisted {
    #[serde(default)]
    owner: Option<String>,
    /// The shared key of the fleet this device belongs to, if any. Minted the
    /// first time this device claims another, or handed down by its owner when
    /// this device is itself adopted.
    #[serde(default)]
    fleet_key: Option<String>,
    /// Last version of the owned roster we hold (last-writer-wins on gossip).
    #[serde(default)]
    fleet_version: u64,
    /// The fleet's members — every co-owned device, in canonical-pubkey form.
    #[serde(default)]
    fleet_members: Vec<OwnedMember>,
}

/// Live state behind one lock, so a claim's check-and-set is atomic.
#[derive(Debug, Default)]
struct Inner {
    owner: Option<String>,
    claim_mode: bool,
    fleet_key: Option<String>,
    fleet_version: u64,
    fleet_members: Vec<OwnedMember>,
}

/// The live ownership store. Cheap to share behind an `Arc`.
pub struct Ownership {
    path: Option<PathBuf>,
    inner: Mutex<Inner>,
}

impl Ownership {
    /// Load the record from disk (or start blank). The start-time
    /// `ALLMYSTUFF_CLAIMABLE` flag seeds claim mode for this run, but only
    /// while the device is still unowned.
    pub fn load() -> Self {
        let path = store_path();
        let persisted = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<Persisted>(&s).ok())
            .unwrap_or_default();
        let inner = Inner {
            claim_mode: persisted.owner.is_none() && env_claim_flag(),
            owner: persisted.owner,
            fleet_key: persisted.fleet_key,
            fleet_version: persisted.fleet_version,
            fleet_members: persisted.fleet_members,
        };
        Ownership {
            path,
            inner: Mutex::new(inner),
        }
    }

    /// The recorded owner's node id, if any.
    pub fn owner(&self) -> Option<String> {
        self.inner.lock().owner.clone()
    }

    /// Whether this device is currently offering itself for adoption: no
    /// owner yet **and** claim mode is on. This is the *only* condition under
    /// which a claim is accepted.
    pub fn claimable(&self) -> bool {
        let i = self.inner.lock();
        i.owner.is_none() && i.claim_mode
    }

    /// Record (or clear) the owner. Recording one ends claim mode — an owned
    /// device is never claimable until its owner releases it. Returns whether
    /// the durable write succeeded.
    pub fn set_owner(&self, owner: Option<String>) -> bool {
        let mut i = self.inner.lock();
        i.owner = owner;
        if i.owner.is_some() {
            i.claim_mode = false;
        }
        persist(&self.path, &i)
    }

    /// Turn claim mode on or off at runtime (only meaningful while unowned).
    pub fn set_claim_mode(&self, on: bool) {
        let mut i = self.inner.lock();
        i.claim_mode = on && i.owner.is_none();
    }

    /// Accept a claim from `claimer` — but only if the device is currently
    /// claimable **and** the new owner can be durably recorded. Both the
    /// check and the set happen under one lock so a claim can't race another
    /// or be acknowledged without being persisted. Returns whether it took.
    pub fn try_accept_claim(&self, claimer: &str) -> bool {
        let mut i = self.inner.lock();
        if i.owner.is_some() || !i.claim_mode {
            return false;
        }
        i.owner = Some(claimer.to_string());
        i.claim_mode = false;
        if persist(&self.path, &i) {
            true
        } else {
            // Couldn't record it durably — roll back rather than pretend the
            // claim took (the peer would be told Claimed, then we'd come back
            // unowned after a restart).
            i.owner = None;
            i.claim_mode = true;
            false
        }
    }

    // ---- owned fleet (the gossiped "Owned" roster) ----------------------
    //
    // Claiming a device links the two machines under a shared **fleet key**.
    // The owner mints the key on its first claim; every adoption adds the new
    // device to the roster and hands the key down. All co-owned devices gossip
    // the [`OwnedRoster`] on `CHANNEL_OWNED` and converge by version, so the
    // fleet is the same set everywhere. For now this only groups devices
    // internally — a later edition links the key to other things.

    /// The fleet roster this device currently holds, if it belongs to a fleet.
    pub fn fleet(&self) -> Option<OwnedRoster> {
        let i = self.inner.lock();
        let key = i.fleet_key.clone()?;
        Some(OwnedRoster {
            key,
            version: i.fleet_version,
            members: i.fleet_members.clone(),
        })
    }

    /// Make sure this device has a fleet key, minting a fresh one the first
    /// time (e.g. when it claims its first device). Returns the key.
    pub fn ensure_fleet_key(&self) -> String {
        let mut i = self.inner.lock();
        if i.fleet_key.is_none() {
            i.fleet_key = Some(new_fleet_key());
            i.fleet_version = i.fleet_version.max(1);
            persist(&self.path, &i);
        }
        i.fleet_key.clone().unwrap_or_default()
    }

    /// Add or refresh a member of this device's fleet, bumping the version.
    /// Members are keyed by canonical pubkey so one machine never doubles up.
    /// Returns whether the roster actually changed.
    pub fn upsert_member(&self, device: &str, label: &str) -> bool {
        let mut i = self.inner.lock();
        let changed = upsert_member_into(&mut i.fleet_members, device, label);
        if changed {
            i.fleet_version += 1;
            persist(&self.path, &i);
        }
        changed
    }

    /// Merge an inbound fleet roster a peer gossiped. We adopt its key if we
    /// hold none; once we have a key we only merge rosters that share it (a
    /// foreign fleet's gossip is ignored). Members are unioned by canonical
    /// pubkey and the version converges upward.
    ///
    /// Returns whether the merge was **structural** — we adopted the key or
    /// gained a member — which is the signal to re-broadcast so the rest of
    /// the fleet converges. A label-only refresh is still saved but does *not*
    /// re-broadcast, so two peers that disagree on a member's label can't
    /// ping-pong gossip forever.
    pub fn merge_fleet(&self, incoming: &OwnedRoster) -> bool {
        if incoming.key.is_empty() {
            return false;
        }
        let mut i = self.inner.lock();
        // A foreign fleet's gossip (a key we don't share) is ignored once we
        // hold one. Done before any mutation so the borrow doesn't conflict.
        if let Some(k) = &i.fleet_key {
            if k != &incoming.key {
                return false;
            }
        }
        // Adopt the key if we hold none — that's a structural change.
        let mut structural = i.fleet_key.is_none();
        if structural {
            i.fleet_key = Some(incoming.key.clone());
        }
        let mut dirty = structural;
        for m in &incoming.members {
            let canon = pubkey_part(m.device.as_str());
            match i
                .fleet_members
                .iter()
                .position(|x| pubkey_part(x.device.as_str()) == canon)
            {
                Some(pos) => {
                    if !m.label.is_empty() && i.fleet_members[pos].label != m.label {
                        i.fleet_members[pos].label = m.label.clone();
                        dirty = true; // label refresh — saved, but not re-broadcast
                    }
                }
                None => {
                    i.fleet_members.push(OwnedMember {
                        device: NodeId::from(canon),
                        label: m.label.clone(),
                    });
                    structural = true;
                    dirty = true;
                }
            }
        }
        // Converge the version upward; a structural change makes ours strictly
        // newer so our next gossip out-ranks the copy we just merged.
        let mut target = incoming.version.max(i.fleet_version);
        if structural {
            target = target.max(i.fleet_version + 1);
        }
        if target != i.fleet_version {
            i.fleet_version = target;
            dirty = true;
        }
        if dirty {
            persist(&self.path, &i);
        }
        structural
    }
}

/// Add or refresh `device` in `members`, matching by canonical pubkey. The
/// newest non-empty label wins. Returns whether the list changed. Uses an
/// owned index (`position`) so the lookup borrow is released before the push.
fn upsert_member_into(members: &mut Vec<OwnedMember>, device: &str, label: &str) -> bool {
    let canon = pubkey_part(device);
    match members
        .iter()
        .position(|m| pubkey_part(m.device.as_str()) == canon)
    {
        Some(pos) => {
            if !label.is_empty() && members[pos].label != label {
                members[pos].label = label.to_string();
                true
            } else {
                false
            }
        }
        None => {
            members.push(OwnedMember {
                device: NodeId::from(canon),
                label: label.to_string(),
            });
            true
        }
    }
}

/// Mint a fresh fleet key: 32 bytes of system randomness, hex-encoded. This
/// is an opaque grouping secret today; it carries no structure other apps
/// rely on.
fn new_fleet_key() -> String {
    let mut bytes = [0u8; 32];
    // The system RNG not being available is catastrophic and vanishingly
    // unlikely; failing loudly beats minting a predictable fleet key.
    getrandom::getrandom(&mut bytes).expect("system RNG unavailable for fleet key");
    let mut s = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// The stable pubkey portion of a mesh device id — strip MyOwnMesh's trailing
/// 5-char display suffix (`-AB12C`). Mirrors `mesh::pubkey_part` so a device
/// in display form and bare form collapse to one fleet member.
fn pubkey_part(id: &str) -> &str {
    if let Some((body, suffix)) = id.rsplit_once('-') {
        if suffix.len() == 5 && suffix.chars().all(|c| c.is_ascii_alphanumeric()) {
            return body;
        }
    }
    id
}

/// Write the durable part. Returns whether it was saved (a missing home dir —
/// e.g. an ephemeral/test environment — counts as "nothing to persist, ok").
fn persist(path: &Option<PathBuf>, inner: &Inner) -> bool {
    let Some(path) = path else { return true };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let persisted = Persisted {
        owner: inner.owner.clone(),
        fleet_key: inner.fleet_key.clone(),
        fleet_version: inner.fleet_version,
        fleet_members: inner.fleet_members.clone(),
    };
    match serde_json::to_string_pretty(&persisted) {
        Ok(json) => std::fs::write(path, json).is_ok(),
        Err(_) => false,
    }
}

/// `~/.myownmesh/allmystuff-ownership.json`, honouring `MYOWNMESH_HOME` —
/// the same home the control socket and identity use.
fn store_path() -> Option<PathBuf> {
    let home = std::env::var_os("MYOWNMESH_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)?;
    Some(home.join(".myownmesh").join("allmystuff-ownership.json"))
}

/// The start-time claim flag: `ALLMYSTUFF_CLAIMABLE` set to a truthy value.
fn env_claim_flag() -> bool {
    std::env::var("ALLMYSTUFF_CLAIMABLE")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store with no backing file — every `persist` is a no-op "ok", so the
    /// fleet logic can be exercised without a home dir.
    fn memory() -> Ownership {
        Ownership {
            path: None,
            inner: Mutex::new(Inner::default()),
        }
    }

    #[test]
    fn pubkey_part_strips_the_display_suffix() {
        assert_eq!(pubkey_part("abcdef-AB12C"), "abcdef");
        // Not a 5-char alphanumeric suffix → left alone.
        assert_eq!(pubkey_part("abcdef-toolong"), "abcdef-toolong");
        assert_eq!(pubkey_part("bare"), "bare");
    }

    #[test]
    fn fleet_key_is_64_hex_chars_and_fresh_each_time() {
        let a = new_fleet_key();
        let b = new_fleet_key();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "two mints must not collide");
    }

    #[test]
    fn upsert_dedups_by_canonical_pubkey_and_refreshes_labels() {
        let mut members = Vec::new();
        assert!(upsert_member_into(&mut members, "k1-AB12C", "Laptop"));
        // Same machine via its bare pubkey: no new member, just maybe a label.
        assert!(!upsert_member_into(&mut members, "k1", "Laptop"));
        assert!(upsert_member_into(&mut members, "k1", "My laptop")); // label change
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].label, "My laptop");
    }

    #[test]
    fn claiming_builds_a_fleet_then_merges_converge() {
        // Owner mints a key and rosters itself + a claimed device.
        let owner = memory();
        let key = owner.ensure_fleet_key();
        assert!(!key.is_empty());
        assert!(owner.upsert_member("owner-AAAAA", "Owner"));
        assert!(owner.upsert_member("nuc-BBBBB", "Spare NUC"));
        let roster = owner.fleet().expect("owner has a fleet");
        assert_eq!(roster.members.len(), 2);

        // The claimed device starts blank, then merges the owner's gossip:
        // it adopts the key and the membership.
        let target = memory();
        assert!(target.merge_fleet(&roster));
        let t = target.fleet().unwrap();
        assert_eq!(t.key, key);
        assert_eq!(t.members.len(), 2);

        // A foreign fleet's gossip (different key) is ignored once we hold one.
        let foreign = OwnedRoster {
            key: "ffff".into(),
            version: 99,
            members: vec![OwnedMember {
                device: "intruder".into(),
                label: "Intruder".into(),
            }],
        };
        assert!(!target.merge_fleet(&foreign));
        assert_eq!(target.fleet().unwrap().members.len(), 2);
    }
}

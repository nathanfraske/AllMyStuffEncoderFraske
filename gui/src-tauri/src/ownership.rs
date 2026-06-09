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

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// The durable part of the record — only the owner survives a restart.
/// Additive + `#[serde(default)]` so an older file (or none) still loads.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Persisted {
    #[serde(default)]
    owner: Option<String>,
}

/// Live state behind one lock, so a claim's check-and-set is atomic.
#[derive(Debug, Default)]
struct Inner {
    owner: Option<String>,
    claim_mode: bool,
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

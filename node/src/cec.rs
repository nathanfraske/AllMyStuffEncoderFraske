//! CEC Support — the technician/customer state layered on the shared mesh
//! engine.
//!
//! CEC Support is AnyDesk-style remote help built on the very same [`Mesh`]
//! engine AllMyStuff already runs (presence, the route offer/accept handshake,
//! screen `display` + `input`, the media planes, and the per-frame
//! authorization gate). The only substitution is *trust*: where an ordinary
//! AllMyStuff route is gated on owner/fleet membership
//! ([`crate::mesh::Mesh::sender_may_control`]), a CEC route is gated on the
//! customer holding a **live consent grant** for the technician
//! ([`allmystuff_cec_consent`]) — so a revoke ("Forget this technician") bites
//! immediately, mid-session, exactly like AllMyStuff re-checks authorization
//! per frame.
//!
//! Two roles share this one struct:
//!  * a **customer** (the standalone CEC Support client, or any node hosting)
//!    fills [`CecInner::consent`] + [`CecInner::pending`] + `hosting`;
//!  * a **technician** (this AllMyStuff install, joined to a customer's secret
//!    Silent mesh) fills `agent_name` + [`CecInner::dialed`].
//!
//! Everything here is plain, lock-guarded state plus pure helpers, so the wire
//! contract ([`allmystuff_cec_protocol`]) and the enforcement store
//! ([`allmystuff_cec_consent`]) stay the single sources of truth — this module
//! only *bookkeeps* which technicians are pending/dialed and projects that into
//! JSON for the node-control surface and the `cec://*` events.
//!
//! [`Mesh`]: crate::mesh::Mesh

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use allmystuff_cec_consent::{capabilities_for, Capability, ConsentStore};
use allmystuff_cec_protocol::{
    format_support_id, network_id_for_device, network_id_for_number, support_id_from_device,
    ApprovalScope, Role,
};

/// Where the customer's consent store lives:
/// `~/.myownmesh/cec-consent.json`, honouring `MYOWNMESH_HOME` — the same home
/// the ownership store and control socket use. `None` (no home resolvable) runs
/// the store in memory.
pub fn consent_store_path() -> Option<PathBuf> {
    let home = std::env::var_os("MYOWNMESH_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)?;
    Some(home.join(".myownmesh").join("cec-consent.json"))
}

/// This machine's wall clock as Unix seconds — the injected `now` the consent
/// store enforces expiry against (it never reads the clock itself).
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One inbound technician connect-request awaiting the customer's 3-choice
/// prompt (customer side). Built from a [`ConnectControl::Request`] arriving on
/// the `cec.control` channel; surfaced verbatim in `cec_pending` and the
/// `cec://request` event.
///
/// [`ConnectControl::Request`]: allmystuff_cec_protocol::ConnectControl::Request
#[derive(Clone, Debug)]
pub struct PendingRequest {
    /// The technician's device id (as it arrived — display or bare).
    pub tech: String,
    /// The Agent Name the customer sees ("*so-and-so* is trying to connect").
    pub agent_name: String,
    /// Whether the technician asked for keyboard/mouse control (vs view-only).
    pub want_control: bool,
    /// The session id the technician minted for this attempt.
    pub session_id: String,
    /// A short, human-comparable code the customer can read back to confirm the
    /// technician out-of-band before approving.
    pub verification_code: String,
}

impl PendingRequest {
    fn to_value(&self) -> Value {
        json!({
            "tech": self.tech,
            "agent_name": self.agent_name,
            "want_control": self.want_control,
            "session_id": self.session_id,
            "verification_code": self.verification_code,
        })
    }
}

/// One customer this technician has dialed (technician side). Keyed in
/// [`CecInner::dialed`] by the customer's canonical (bare-pubkey) id. A dialed
/// customer is an ordinary mesh peer on the graph — the CEC tab lists these
/// from CEC state ([`Cec::dialed_list`]), it is not a graph grouping.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DialedCustomer {
    /// The customer's device id — empty for an attempt whose discovery never
    /// completed (the customer hasn't answered on this number yet).
    pub node: String,
    /// The support number the technician typed to reach them.
    pub number: String,
    /// Best-known label (the machine name, once presence lands).
    pub label: String,
    /// Best-known hostname, shown beside the label so the technician's card
    /// spells the same identity the customer's app shows. `default` so a
    /// directory persisted by an older build still loads.
    #[serde(default)]
    pub hostname: String,
    /// Whether the customer is currently reachable.
    pub online: bool,
    /// The Silent mesh (`network_id_for_number`) the customer was dialed on.
    pub network_id: String,
    /// Epoch seconds of the last time the technician actively used this
    /// connection — a fresh dial, or the console session going active. Surfaced
    /// to the CEC tab so a technician can spot (and clean up) connections gone
    /// stale while keeping the ones they've reached for recently.
    pub last_used: u64,
}

impl DialedCustomer {
    /// The `cec://peer` / `cec_dial` result shape.
    pub fn to_value(&self) -> Value {
        json!({
            "node": self.node,
            "number": self.number,
            "label": self.label,
            "hostname": self.hostname,
            "online": self.online,
            "last_used": self.last_used,
        })
    }
}

/// Load the persisted dialed-customer directory, keyed by each customer's
/// canonical (bare-pubkey) id. `online` is reset to `false` — reachability is
/// re-confirmed live (see the `cec_dialed` reconcile), never trusted from a
/// prior run. A missing or corrupt file loads empty; it never bricks the node.
fn load_dialed(path: Option<&PathBuf>) -> HashMap<String, DialedCustomer> {
    let Some(path) = path else {
        return HashMap::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let list: Vec<DialedCustomer> = serde_json::from_str(&text).unwrap_or_default();
    list.into_iter()
        .filter(|c| !number_digits(&c.number).is_empty())
        .map(|mut c| {
            c.online = false;
            (number_digits(&c.number), c)
        })
        .collect()
}

/// The CEC state a node carries — all behind one lock, since both the
/// node-control commands and the mesh's per-frame gate reach it from many
/// tasks. Cheap to hold: the enforcement (expiry, persistence) lives in the
/// [`ConsentStore`], not here.
pub struct Cec {
    inner: Mutex<CecInner>,
    /// Where the technician's dialed-customer directory is mirrored, so it
    /// survives a node restart. The customer's Silent mesh is persisted
    /// daemon-side; this keeps the technician's view of every machine they've
    /// serviced in step (independent of grant lifetime — an expired grant just
    /// re-prompts the customer on the next connect). `None` for an in-memory
    /// node (tests), where nothing is written.
    dialed_path: Option<PathBuf>,
}

struct CecInner {
    /// Which side this node is playing. Defaults to [`Role::Client`] (the CEC
    /// Support app's role); flips to [`Role::Technician`] on the first
    /// `cec_dial`.
    role: Role,
    /// Whether this node is currently hosting (a customer advertising itself on
    /// its own number-derived Silent mesh, listening for connect-requests).
    hosting: bool,
    /// This node's own support number (customer) — derived once the local id is
    /// known. Empty until then.
    number: String,
    /// The Silent mesh id this node is anchored to: a customer's own
    /// `network_id_for_device(me)`, or the technician's most-recently-dialed
    /// customer room.
    network_id: String,
    /// The technician's **Agent Name** — the name a customer sees in the prompt.
    /// Persisted GUI-side; mirrored here so an outbound connect-request carries
    /// it.
    agent_name: String,
    /// The customer's standing approvals — the enforcement store consulted on
    /// every privileged CEC frame.
    consent: ConsentStore,
    /// Inbound connect-requests awaiting the customer's decision.
    pending: Vec<PendingRequest>,
    /// Every client machine this technician has *attempted* (number digits →
    /// record) — a permanent directory row exists from the moment of the dial,
    /// whether or not the customer ever answered. `node` is filled in once
    /// discovery succeeds. Surfaced to the CEC tab via [`Cec::dialed_list`];
    /// rows leave only via the explicit forget/curate actions.
    dialed: HashMap<String, DialedCustomer>,
    /// Live session states by session id, for `cec://session`.
    sessions: HashMap<String, String>,
    /// Cancellation flag for the in-flight dial (one at a time — the GUI
    /// serializes dials). `begin_dial` mints a fresh flag; `cancel_dial` trips
    /// it; the discovery poll and the connect-request re-send loop both honor
    /// it, so "stop trying" actually stops everything being tried.
    dial_cancel: Option<Arc<AtomicBool>>,
    /// Customer: whether this node is currently asking for help on the global
    /// help mesh. In-memory only — a restart simply stops the beacon and the
    /// technicians' TTL caches age the entry out.
    asking_help: bool,
    /// Bumped on every asking-state transition, so the re-beacon loop can tell
    /// "still the same ask" from "cancelled and re-asked" and exactly one loop
    /// ever beacons.
    help_epoch: u64,
    /// Technician: the waiting customers heard on the global help mesh, keyed
    /// by the sender's canonical id. Entries live [`HELP_TTL_SECS`] past their
    /// last beacon; an `available: false` beacon (cancel / help arrived)
    /// removes one immediately.
    help_wanted: HashMap<String, HelpSeeker>,
}

/// One customer waiting on the global help mesh, as heard from their beacon.
#[derive(Clone, Debug)]
struct HelpSeeker {
    /// Their dialable support number — derived from the *authenticated* sender
    /// id, never read from the payload, so a beacon can't impersonate another
    /// number.
    number: String,
    /// Their machine label (cosmetic, from the beacon).
    label: String,
    /// Their machine hostname (cosmetic, from the beacon) — shown beside the
    /// label so the technician's card and the customer's waiting screen spell
    /// the same identity.
    hostname: String,
    /// Unix seconds we first heard this ask — the queue position.
    asked_at: u64,
    /// Unix seconds of the latest beacon — the TTL clock.
    last_seen: u64,
}

/// How long a technician keeps a help entry past its last beacon. The customer
/// re-beacons every [`HELP_BEACON_SECS`], so this tolerates a few missed beats
/// before a silent (crashed / offline) asker ages out.
pub const HELP_TTL_SECS: u64 = 90;
/// How often an asking customer re-beacons on the help mesh.
pub const HELP_BEACON_SECS: u64 = 20;

impl Cec {
    /// Build the CEC state, loading (or, with `None`, running an in-memory)
    /// consent store. The store path is a `consent.json` under the node's home
    /// — a corrupt or absent file loads empty (it never bricks the node), and
    /// only `ThreeHours`/`Forever` grants are ever written.
    pub fn new(consent_path: Option<PathBuf>) -> Self {
        // Mirror the dialed directory next to the consent store, under the same
        // node home. A `None` consent path (tests) means no persistence.
        let dialed_path = consent_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(|dir| dir.join("cec-dialed.json"));
        let consent = match consent_path {
            Some(p) => ConsentStore::load(p),
            None => ConsentStore::in_memory(),
        };
        let dialed = load_dialed(dialed_path.as_ref());
        Cec {
            dialed_path,
            inner: Mutex::new(CecInner {
                role: Role::Client,
                hosting: false,
                number: String::new(),
                network_id: String::new(),
                agent_name: String::new(),
                consent,
                pending: Vec::new(),
                dialed,
                sessions: HashMap::new(),
                dial_cancel: None,
                asking_help: false,
                help_epoch: 0,
                help_wanted: HashMap::new(),
            }),
        }
    }

    /// Mirror the dialed directory to disk. Best-effort: a write failure warns
    /// and is dropped (an in-memory list still beats bricking on a read-only
    /// disk). Called after every mutation of `dialed`.
    fn persist_dialed(&self, list: Vec<DialedCustomer>) {
        let Some(path) = &self.dialed_path else {
            return;
        };
        match serde_json::to_string_pretty(&list) {
            Ok(json) => {
                if let Err(e) = std::fs::write(path, json) {
                    tracing::warn!(
                        "couldn't persist CEC dialed customers to {}: {e}",
                        path.display()
                    );
                }
            }
            Err(e) => tracing::warn!("couldn't serialize CEC dialed customers: {e}"),
        }
    }

    // ---- status ---------------------------------------------------------

    /// The `cec_status` result: `{ number, network_id, role, hosting }`. When
    /// `me` is known and this node has no number yet, derive its own from it so
    /// a customer can read its code straight away.
    pub fn status(&self, me: Option<&str>) -> Value {
        let mut inner = self.inner.lock();
        if inner.number.is_empty() {
            if let Some(me) = me {
                inner.number = support_id_from_device(me);
                if inner.network_id.is_empty() {
                    inner.network_id = network_id_for_device(me);
                }
            }
        }
        json!({
            "number": inner.number,
            "network_id": inner.network_id,
            "role": role_str(inner.role),
            "hosting": inner.hosting,
            "asking_help": inner.asking_help,
        })
    }

    // ---- technician (Agent Name + dial bookkeeping) ---------------------

    /// The technician's Agent Name, for stamping an outbound connect-request.
    pub fn agent_name(&self) -> String {
        self.inner.lock().agent_name.clone()
    }

    /// Set (persist mirror of) the technician's Agent Name.
    pub fn set_agent_name(&self, name: String) {
        self.inner.lock().agent_name = name;
    }

    /// Note that this node is now acting as a technician (first dial), pinning
    /// the last-dialed customer room as its `network_id`.
    pub fn set_dialed_network(&self, network_id: String) {
        let mut inner = self.inner.lock();
        inner.role = Role::Technician;
        inner.network_id = network_id;
    }

    /// Record (or refresh) a dialed customer, keyed by canonical id. Returns the
    /// stored record for the `cec://peer` emit.
    pub fn record_dialed(
        &self,
        node: String,
        number: String,
        label: String,
        hostname: String,
        online: bool,
        network_id: String,
    ) -> DialedCustomer {
        let now = now_secs();
        let mut inner = self.inner.lock();
        let entry = inner
            .dialed
            .entry(number_digits(&number))
            .or_insert_with(|| DialedCustomer {
                node: node.clone(),
                number: number.clone(),
                label: label.clone(),
                hostname: hostname.clone(),
                online,
                network_id: network_id.clone(),
                last_used: now,
            });
        entry.node = node;
        entry.number = number;
        if !label.is_empty() {
            entry.label = label;
        }
        if !hostname.is_empty() {
            entry.hostname = hostname;
        }
        entry.online = online;
        entry.network_id = network_id;
        // A (re)dial is a fresh use — keep the stale-connection metric honest.
        entry.last_used = now;
        let record = entry.clone();
        let snapshot: Vec<DialedCustomer> = inner.dialed.values().cloned().collect();
        drop(inner);
        self.persist_dialed(snapshot);
        record
    }

    /// End every live session except `keep`, returning the ids that changed so
    /// the caller can emit their `cec://session` transitions. The customer flow
    /// is one live support session at a time: a technician's re-dial mints a
    /// fresh session id, and without this the old rows stack up in the
    /// customer's "viewing your screen" banner forever.
    pub fn end_other_sessions(&self, keep: &str) -> Vec<String> {
        let mut inner = self.inner.lock();
        let ended: Vec<String> = inner
            .sessions
            .iter()
            .filter(|(id, state)| id.as_str() != keep && state.as_str() == "active")
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ended {
            inner.sessions.insert(id.clone(), "ended".to_string());
        }
        ended
    }

    /// Mint the cancellation flag for a new dial, replacing any stale one.
    /// The returned flag is checked by every loop the dial runs.
    pub fn begin_dial(&self) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        self.inner.lock().dial_cancel = Some(flag.clone());
        flag
    }

    /// Trip the in-flight dial's cancellation flag ("stop trying"). Harmless
    /// when nothing is in flight — a completed dial's stale flag has no
    /// readers left.
    pub fn cancel_dial(&self) {
        if let Some(f) = &self.inner.lock().dial_cancel {
            f.store(true, Ordering::Relaxed);
        }
    }

    /// Record a dial *attempt* the moment the technician dials: the permanent
    /// directory row exists (and persists) before discovery, before approval,
    /// whether or not the customer ever answers. Re-dialing an existing number
    /// just refreshes its recency. The node id stays empty until discovery
    /// fills it via [`Cec::record_dialed`].
    pub fn record_attempt(&self, number: &str) -> DialedCustomer {
        let now = now_secs();
        let mut inner = self.inner.lock();
        let entry = inner
            .dialed
            .entry(number_digits(number))
            .or_insert_with(|| DialedCustomer {
                node: String::new(),
                number: number.to_string(),
                label: String::new(),
                hostname: String::new(),
                online: false,
                network_id: network_id_for_number(number),
                last_used: now,
            });
        entry.last_used = now;
        let record = entry.clone();
        let snapshot: Vec<DialedCustomer> = inner.dialed.values().cloned().collect();
        drop(inner);
        self.persist_dialed(snapshot);
        record
    }

    /// Mark a dialed customer online/offline (from presence), returning its
    /// updated record when the flag actually changed.
    pub fn set_customer_online(&self, canonical: &str, online: bool) -> Option<DialedCustomer> {
        // `online` is ephemeral — reconciled live and reset to false on load — so
        // this stays in memory; only the durable fields (record/last_used) are
        // ever written to disk.
        let mut inner = self.inner.lock();
        let c = inner
            .dialed
            .values_mut()
            .find(|c| !c.node.is_empty() && pubkey_part(&c.node) == canonical)?;
        if c.online == online {
            return None;
        }
        c.online = online;
        Some(c.clone())
    }

    /// Stamp a dialed customer as just-used (`last_used = now`) — called when the
    /// console session with them goes active, so the CEC tab's "last used"
    /// reflects real activity, not just the original dial. Returns the updated
    /// record (for a `cec://peer` re-emit); `None` for a customer we haven't
    /// dialed.
    pub fn touch_dialed(&self, canonical: &str) -> Option<DialedCustomer> {
        let mut inner = self.inner.lock();
        let c = inner
            .dialed
            .values_mut()
            .find(|c| !c.node.is_empty() && pubkey_part(&c.node) == canonical)?;
        c.last_used = now_secs();
        let record = c.clone();
        let snapshot: Vec<DialedCustomer> = inner.dialed.values().cloned().collect();
        drop(inner);
        self.persist_dialed(snapshot);
        Some(record)
    }

    /// Whether `canonical` is a customer this technician has dialed. Used by
    /// "Forget this node" to know a CEC customer needs its Silent room left too.
    pub fn is_dialed(&self, canonical: &str) -> bool {
        self.inner
            .lock()
            .dialed
            .values()
            .any(|c| !c.node.is_empty() && pubkey_part(&c.node) == canonical)
    }

    /// The customers this technician has dialed, projected for the CEC tab's
    /// "Active connections" list (`cec_dialed`) — the same `{ node, number,
    /// label, online }` shape as a `cec://peer` event. Each entry is an ordinary
    /// mesh peer on the graph; the tab reads them from here rather than from any
    /// graph grouping (there is none — the CEC mesh is Silent, with no roster).
    pub fn dialed_list(&self) -> Vec<Value> {
        self.inner
            .lock()
            .dialed
            .values()
            .map(DialedCustomer::to_value)
            .collect()
    }

    /// The dialed customers as owned records (with `network_id`), for the async
    /// `cec_dialed` projection that reconciles each one's live reachability
    /// against the daemon's peer set. [`dialed_list`] returns the UI shape;
    /// this keeps the fields that shape drops.
    pub fn dialed_records(&self) -> Vec<DialedCustomer> {
        self.inner.lock().dialed.values().cloned().collect()
    }

    /// The Silent mesh a dialed customer lives on, if any (for teardown).
    pub fn dialed_network(&self, canonical: &str) -> Option<String> {
        self.inner
            .lock()
            .dialed
            .values()
            .find(|c| !c.node.is_empty() && pubkey_part(&c.node) == canonical)
            .map(|c| c.network_id.clone())
    }

    /// Drop a customer this technician dialed (the CEC part of "Forget this
    /// node"). Returns `true` when one was actually removed.
    pub fn forget_dialed(&self, canonical: &str) -> bool {
        let mut inner = self.inner.lock();
        let key = inner
            .dialed
            .iter()
            .find(|(_, c)| !c.node.is_empty() && pubkey_part(&c.node) == canonical)
            .map(|(k, _)| k.clone());
        let removed = key.and_then(|k| inner.dialed.remove(&k)).is_some();
        if removed {
            let snapshot: Vec<DialedCustomer> = inner.dialed.values().cloned().collect();
            drop(inner);
            self.persist_dialed(snapshot);
        }
        removed
    }

    /// Remove a directory row by its support number — the curation path for an
    /// attempt row whose discovery never completed (it has no node id for the
    /// canonical-keyed [`Cec::forget_dialed`]). Returns the removed record so
    /// the caller can tear down its Silent room too.
    pub fn forget_number(&self, number: &str) -> Option<DialedCustomer> {
        let mut inner = self.inner.lock();
        let removed = inner.dialed.remove(&number_digits(number));
        if removed.is_some() {
            let snapshot: Vec<DialedCustomer> = inner.dialed.values().cloned().collect();
            drop(inner);
            self.persist_dialed(snapshot);
        }
        removed
    }

    // ---- customer (hosting + the 3-choice consent flow) -----------------

    /// Enter/leave hosting (a customer advertising on its own Silent mesh).
    /// Returns the resolved support number when entering.
    pub fn set_hosting(&self, hosting: bool, me: Option<&str>) -> String {
        let mut inner = self.inner.lock();
        inner.hosting = hosting;
        inner.role = Role::Client;
        if hosting {
            if let Some(me) = me {
                inner.number = support_id_from_device(me);
                inner.network_id = network_id_for_device(me);
            }
        }
        inner.number.clone()
    }

    pub fn is_hosting(&self) -> bool {
        self.inner.lock().hosting
    }

    /// Record an inbound technician connect-request (customer side), replacing
    /// any prior pending attempt from the same technician so a redial doesn't
    /// stack duplicates.
    pub fn record_pending(&self, req: PendingRequest) {
        let mut inner = self.inner.lock();
        let tech = pubkey_part(&req.tech).to_string();
        inner.pending.retain(|p| pubkey_part(&p.tech) != tech);
        inner.pending.push(req);
    }

    /// The customer's pending connect-requests, for `cec_pending`.
    pub fn pending(&self) -> Vec<Value> {
        self.inner
            .lock()
            .pending
            .iter()
            .map(PendingRequest::to_value)
            .collect()
    }

    /// Look up a pending request's Agent Name (kept with the grant so the
    /// customer recognises a "Forget this technician" entry later).
    pub fn pending_agent_name(&self, tech: &str) -> String {
        let inner = self.inner.lock();
        let key = pubkey_part(tech);
        inner
            .pending
            .iter()
            .find(|p| pubkey_part(&p.tech) == key)
            .map(|p| p.agent_name.clone())
            .unwrap_or_default()
    }

    /// Record the customer's approval of `tech` at `scope` in the consent
    /// store, dropping the matching pending request. A failed durable write
    /// (a `ThreeHours`/`Forever` grant that couldn't be saved) returns the
    /// error and records nothing — never a silent security downgrade.
    pub fn approve(
        &self,
        tech: &str,
        agent_name: &str,
        scope: ApprovalScope,
        want_control: bool,
    ) -> Result<(), String> {
        let mut inner = self.inner.lock();
        inner
            .consent
            .approve(
                tech,
                agent_name,
                capabilities_for(want_control),
                scope,
                now_secs(),
            )
            .map_err(|e| e.to_string())?;
        let key = pubkey_part(tech).to_string();
        inner.pending.retain(|p| pubkey_part(&p.tech) != key);
        Ok(())
    }

    /// Drop a pending request (a plain "Deny", no grant recorded).
    pub fn deny(&self, tech: &str) {
        let mut inner = self.inner.lock();
        let key = pubkey_part(tech).to_string();
        inner.pending.retain(|p| pubkey_part(&p.tech) != key);
    }

    /// Revoke every grant for `tech` ("Forget this technician"). Returns
    /// whether anything was actually removed (so the caller can skip a teardown
    /// that isn't needed). Also drops any pending request from them.
    pub fn revoke(&self, tech: &str) -> Result<bool, String> {
        let mut inner = self.inner.lock();
        let key = pubkey_part(tech).to_string();
        inner.pending.retain(|p| pubkey_part(&p.tech) != key);
        inner.consent.revoke(tech).map_err(|e| e.to_string())
    }

    /// The customer's live grants, for `cec_grants` and the `cec://grants`
    /// event.
    pub fn grants(&self) -> Vec<Value> {
        let inner = self.inner.lock();
        inner
            .consent
            .active_grants(now_secs())
            .into_iter()
            .map(|g| {
                json!({
                    "technician": g.technician,
                    "agent_name": g.agent_name,
                    "scope": scope_str(g.scope),
                    "granted_at": g.granted_at,
                    "expires_at": g.expires_at,
                    "control": g
                        .capabilities
                        .iter()
                        .any(|c| matches!(c, Capability::Control)),
                })
            })
            .collect()
    }

    /// The per-frame enforcement check: whether `tech` currently holds a live
    /// grant covering `cap`. Consulted from the mesh's `sender_may_drive`
    /// (Control) and the screen-offer screen (ScreenView), so a revoke bites
    /// the next frame. Reads the clock via [`now_secs`] so an expired grant
    /// stops mid-session with no bookkeeping tick.
    pub fn is_allowed(&self, tech: &str, cap: Capability) -> bool {
        self.inner.lock().consent.is_allowed(tech, cap, now_secs())
    }

    /// Whether this node has *any* CEC involvement with `tech` — a pending
    /// request, a live grant, or a dialed record. Lets the mesh treat a peer as
    /// a CEC technician (gate on consent) only when CEC actually applies, so the
    /// consent path never narrows an ordinary owner/fleet peer.
    pub fn knows_technician(&self, tech: &str) -> bool {
        let inner = self.inner.lock();
        let key = pubkey_part(tech);
        // Grant records count live **or lapsed** (`ConsentStore::known`): an
        // expired technician must stay recognized, so the screen-offer gate
        // keeps screening them (lapsed ≠ stranger) and a control refusal can
        // name the lapsed approval instead of blaming the fleet roster.
        inner.pending.iter().any(|p| pubkey_part(&p.tech) == key) || inner.consent.known(tech)
    }

    // ---- ask-for-help (customer beacon + technician cache) ---------------

    /// Flip the customer's asking-for-help state. Returns whether it actually
    /// changed (the callers' guard against double-withdrawals), bumping the
    /// epoch on every real transition so exactly one re-beacon loop survives.
    pub fn set_asking_help(&self, on: bool) -> bool {
        let mut inner = self.inner.lock();
        if inner.asking_help == on {
            return false;
        }
        inner.asking_help = on;
        inner.help_epoch += 1;
        true
    }

    /// Whether this customer is currently asking for help.
    pub fn asking_help(&self) -> bool {
        self.inner.lock().asking_help
    }

    /// The current asking-state epoch — the re-beacon loop's "is this still my
    /// ask" check.
    pub fn help_epoch(&self) -> u64 {
        self.inner.lock().help_epoch
    }

    /// Technician: record (or refresh) a waiting customer heard on the help
    /// mesh. `number` must come from the authenticated sender id, not the
    /// payload. Returns whether the *membership* changed (a fresh asker, or a
    /// changed label) — pure keep-alives refresh the TTL clock without
    /// spamming an event.
    pub fn record_help_beacon(
        &self,
        node: &str,
        number: &str,
        label: &str,
        hostname: &str,
    ) -> bool {
        let now = now_secs();
        let mut inner = self.inner.lock();
        let key = pubkey_part(node).to_string();
        match inner.help_wanted.get_mut(&key) {
            Some(s) => {
                s.last_seen = now;
                if s.label != label || s.hostname != hostname {
                    s.label = label.to_string();
                    s.hostname = hostname.to_string();
                    return true;
                }
                false
            }
            None => {
                inner.help_wanted.insert(
                    key,
                    HelpSeeker {
                        number: number.to_string(),
                        label: label.to_string(),
                        hostname: hostname.to_string(),
                        asked_at: now,
                        last_seen: now,
                    },
                );
                true
            }
        }
    }

    /// Technician: drop a waiting customer (their `available: false` withdrawal
    /// — cancelled, or help arrived). Returns whether anything was removed.
    pub fn remove_help_beacon(&self, node: &str) -> bool {
        let key = pubkey_part(node).to_string();
        self.inner.lock().help_wanted.remove(&key).is_some()
    }

    /// Drop every cached help beacon — the watch toggle's leave path, so the
    /// queue empties the moment a technician stops watching.
    pub fn clear_help(&self) {
        self.inner.lock().help_wanted.clear();
    }

    /// Whether this node has ever acted as a technician (its role flipped on
    /// the first dial). Guards the customer-side ask teardown on side-by-side
    /// machines: a technician's help-room watch must survive a co-resident
    /// customer app withdrawing its ask.
    pub fn is_technician(&self) -> bool {
        matches!(self.inner.lock().role, Role::Technician)
    }

    /// Technician: the customers currently waiting for help, longest-waiting
    /// first (it's a queue, not a feed). Prunes anything past its beacon TTL
    /// on the way out, so a crashed asker disappears without a withdrawal.
    pub fn help_list(&self) -> Vec<Value> {
        let now = now_secs();
        let mut inner = self.inner.lock();
        inner
            .help_wanted
            .retain(|_, s| s.last_seen.saturating_add(HELP_TTL_SECS) >= now);
        let mut list: Vec<(&String, &HelpSeeker)> = inner.help_wanted.iter().collect();
        list.sort_by_key(|(_, s)| s.asked_at);
        list.into_iter()
            .map(|(node, s)| {
                json!({
                    "node": node,
                    "number": s.number,
                    "label": s.label,
                    "hostname": s.hostname,
                    "asked_at": s.asked_at,
                    "last_seen": s.last_seen,
                })
            })
            .collect()
    }

    // ---- session state --------------------------------------------------

    /// Record a session's state, returning it for the `cec://session` emit.
    pub fn set_session(&self, session_id: &str, state: &str) {
        self.inner
            .lock()
            .sessions
            .insert(session_id.to_string(), state.to_string());
    }

    /// The last recorded state for a session (`requested` / `active` / `denied`
    /// / `ended`), if known. Lets the technician's dial loop stop re-sending the
    /// connect-request once the customer has answered.
    pub fn session_state(&self, session_id: &str) -> Option<String> {
        self.inner.lock().sessions.get(session_id).cloned()
    }

    /// Whether a pending connect-request is already recorded for `session_id`.
    /// The technician retransmits its Request every 2s until answered, so this
    /// lets the customer refresh the pending record on each beat *without*
    /// re-raising the approval prompt every time.
    pub fn has_pending_session(&self, session_id: &str) -> bool {
        self.inner
            .lock()
            .pending
            .iter()
            .any(|p| p.session_id == session_id)
    }

    /// The scope of the customer's live grant for `tech`, if any — used to
    /// re-send an Approve when a retransmitted Request shows the first one never
    /// reached the technician. `None` only when no grant is held; the technician
    /// ignores the scope on an Approve (it merely moves the session to active),
    /// so the caller can default it.
    pub fn active_scope_for(&self, tech: &str) -> Option<ApprovalScope> {
        let inner = self.inner.lock();
        let key = pubkey_part(tech);
        inner
            .consent
            .active_grants(now_secs())
            .into_iter()
            .find(|g| pubkey_part(&g.technician) == key)
            .map(|g| g.scope)
    }

    /// The scope of a **standing** (persistent) grant for `tech` — 3-hours or
    /// Forever only. This is the auto-approve check for a *new* session: an
    /// "Approve Once" covers exactly the session it was granted in, so it must
    /// never silently approve a reconnect — a fresh dial re-prompts instead.
    pub fn standing_scope_for(&self, tech: &str) -> Option<ApprovalScope> {
        let inner = self.inner.lock();
        let key = pubkey_part(tech);
        inner
            .consent
            .active_grants(now_secs())
            .into_iter()
            .find(|g| g.scope.persists() && pubkey_part(&g.technician) == key)
            .map(|g| g.scope)
    }

    /// Retire the in-memory "Approve Once" grant for `tech` — the session it
    /// covered ended, and Once must not outlive its session. Returns whether a
    /// grant was actually dropped (so the caller can re-emit the grant list).
    pub fn retire_once(&self, tech: &str) -> bool {
        self.inner.lock().consent.revoke_once(tech)
    }
}

/// The customer-facing scope word for a grant (the `cec_grants` shape) —
/// mirrors the wire `snake_case` (`once` / `three_hours` / `forever`).
fn scope_str(scope: ApprovalScope) -> &'static str {
    match scope {
        ApprovalScope::Once => "once",
        ApprovalScope::ThreeHours => "three_hours",
        ApprovalScope::Forever => "forever",
    }
}

/// The `cec_status` role word — `client` / `technician`.
fn role_str(role: Role) -> &'static str {
    match role {
        Role::Client => "client",
        Role::Technician => "technician",
    }
}

/// Map the node-control `scope` argument (`"once" | "three_hours" | "forever"`)
/// to an [`ApprovalScope`]. Unknown values are an error the dispatch surfaces.
pub fn parse_scope(s: &str) -> Result<ApprovalScope, String> {
    match s {
        "once" => Ok(ApprovalScope::Once),
        "three_hours" => Ok(ApprovalScope::ThreeHours),
        "forever" => Ok(ApprovalScope::Forever),
        other => Err(format!(
            "unknown approval scope '{other}' (want once | three_hours | forever)"
        )),
    }
}

/// A short, stable verification code for a connect attempt — the first 6
/// digits of the Support ID of the concatenated technician id and session id,
/// so both ends compute the same code to read back out-of-band.
pub fn verification_code(tech: &str, session_id: &str) -> String {
    // The raw string hash, NOT the device derivation: this input is
    // `tech:session`, and device-id canonicalisation must never touch it.
    let code = allmystuff_cec_protocol::support_id_from_string(&format!("{tech}:{session_id}"));
    code.chars().take(6).collect()
}

/// Grouped display of a support number, e.g. `123 456 789` (cosmetic; the node
/// derives rooms from the normalized form).
pub fn grouped_number(number: &str) -> String {
    format_support_id(number)
}

/// Build the daemon `NetworkAdd` config for a CEC Support **Silent** mesh named
/// after `number`. `Silent` auto-dials nobody and never gossips a roster — the
/// customer is merely *discoverable* inside its own number-derived room, and the
/// technician must `connect_peer` deliberately. The room is isolated by the
/// per-number `network_id` (`cec-<number>`) alone: technician and customer both
/// use the default signaling app-id, so they derive the same room handle and
/// meet with no env override.
pub fn silent_network_config(number: &str) -> (String, Value) {
    let network_id = network_id_for_number(number);
    let config = json!({
        "id": network_id,
        "network_id": network_id,
        "label": format!("CEC Support {}", grouped_number(number)),
        // The Silent kind the sibling myownmesh change adds: no auto-dial, no
        // roster gossip — peers are only *visible* until `connect_peer`.
        "kind": "silent",
        "auto_approve": true,
        "signaling": { "strategy": "nostr", "mdns": true },
    });
    (network_id, config)
}

/// Build the daemon config for the **global help mesh** — the one well-known
/// Silent room every CEC client shares
/// ([`HELP_NETWORK_ID`](allmystuff_cec_protocol::HELP_NETWORK_ID)). A customer
/// joins it only while asking for help and beacons a `SupportPresence` there;
/// technicians sit on it and list the beacons. Silent again: the room carries
/// *want*, never access — a session still goes through the customer's own
/// number mesh and the consent handshake.
pub fn help_network_config() -> (String, Value) {
    let network_id = allmystuff_cec_protocol::HELP_NETWORK_ID.to_string();
    let config = json!({
        "id": network_id,
        "network_id": network_id,
        "label": "CEC Support — asking for help",
        "kind": "silent",
        "auto_approve": true,
        "signaling": { "strategy": "nostr", "mdns": true },
    });
    (network_id, config)
}

/// Canonicalise a device id to its bare pubkey — the same `-XXXXX` display
/// suffix strip the consent store and the mesh use, so a technician isn't seen
/// as a new peer across a reconnect. Re-exported shape of
/// [`allmystuff_cec_consent::pubkey_part`].
/// The bare digits of a support number — the directory key. An attempt and a
/// completed dial share one row per number: the number is the stable identity
/// of a client machine (its node id is only learned once discovery succeeds).
pub fn number_digits(number: &str) -> String {
    number.chars().filter(|c| c.is_ascii_digit()).collect()
}

pub fn pubkey_part(id: &str) -> &str {
    allmystuff_cec_consent::pubkey_part(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "customerpubkeybase32aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TECH: &str = "techpubkeybase32bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn status_derives_number_and_room_from_me() {
        let cec = Cec::new(None);
        let v = cec.status(Some(ME));
        assert_eq!(v["number"], support_id_from_device(ME));
        assert_eq!(v["network_id"], network_id_for_device(ME));
        assert_eq!(v["role"], "client");
        assert_eq!(v["hosting"], false);
    }

    #[test]
    fn approve_then_gate_allows_control_and_revoke_bites() {
        let cec = Cec::new(None);
        cec.record_pending(PendingRequest {
            tech: TECH.into(),
            agent_name: "Alex at CEC".into(),
            want_control: true,
            session_id: "s1".into(),
            verification_code: verification_code(TECH, "s1"),
        });
        assert_eq!(cec.pending().len(), 1);
        assert!(!cec.is_allowed(TECH, Capability::Control));

        cec.approve(TECH, "Alex at CEC", ApprovalScope::Once, true)
            .unwrap();
        // The grant now gates control, and the pending request cleared.
        assert!(cec.is_allowed(TECH, Capability::Control));
        assert!(cec.is_allowed(TECH, Capability::ScreenView));
        assert!(cec.pending().is_empty());
        assert_eq!(cec.grants().len(), 1);

        // "Forget this technician" — the gate closes immediately.
        assert!(cec.revoke(TECH).unwrap());
        assert!(!cec.is_allowed(TECH, Capability::Control));
        assert!(cec.grants().is_empty());
    }

    #[test]
    fn view_only_grant_does_not_authorise_control() {
        let cec = Cec::new(None);
        cec.approve(TECH, "Alex", ApprovalScope::Forever, false)
            .unwrap();
        assert!(cec.is_allowed(TECH, Capability::ScreenView));
        assert!(!cec.is_allowed(TECH, Capability::Control));
    }

    #[test]
    fn dialed_customers_track_the_cec_group() {
        let cec = Cec::new(None);
        let canon = pubkey_part(TECH);
        // An attempt is directory-worthy on its own: the row exists (nodeless)
        // from the dial, and a completed discovery merges into the same row.
        let attempt = cec.record_attempt("123 456 789");
        assert!(attempt.node.is_empty());
        assert_eq!(cec.dialed_list().len(), 1);
        cec.record_dialed(
            TECH.into(),
            "123456789".into(),
            "Reception PC".into(),
            "RECEPTION-01".into(),
            true,
            "cec-123456789".into(),
        );
        assert_eq!(cec.dialed_list().len(), 1, "attempt and dial share the row");
        assert!(cec.is_dialed(canon));
        assert_eq!(cec.dialed_network(canon).as_deref(), Some("cec-123456789"));
        // The dial stamps a `last_used` the CEC tab renders as time-since — it's
        // present, non-zero, and a `touch` refreshes the record for a re-emit.
        let listed = cec.dialed_list();
        assert_eq!(listed.len(), 1);
        assert!(listed[0]["last_used"].as_u64().unwrap_or(0) > 0);
        assert!(cec.touch_dialed(canon).is_some());
        assert!(cec.touch_dialed("someone-we-never-dialed").is_none());
        assert!(cec.forget_dialed(canon));
        assert!(!cec.is_dialed(canon));
        // A nodeless attempt row is curated away by number.
        cec.record_attempt("987654321");
        assert_eq!(cec.dialed_list().len(), 1);
        assert!(cec.forget_number("987 654 321").is_some());
        assert!(cec.dialed_list().is_empty());
    }

    #[test]
    fn parse_scope_round_trips() {
        assert_eq!(parse_scope("once").unwrap(), ApprovalScope::Once);
        assert_eq!(
            parse_scope("three_hours").unwrap(),
            ApprovalScope::ThreeHours
        );
        assert_eq!(parse_scope("forever").unwrap(), ApprovalScope::Forever);
        assert!(parse_scope("someday").is_err());
    }

    #[test]
    fn verification_code_is_stable_and_short() {
        let a = verification_code(TECH, "s1");
        let b = verification_code(TECH, "s1");
        assert_eq!(a, b);
        assert_eq!(a.len(), 6);
        assert_ne!(a, verification_code(TECH, "s2"));
    }

    #[test]
    fn help_queue_records_dedupes_and_withdraws() {
        let cec = Cec::new(None);
        // First beacon: a new asker — membership changed.
        assert!(cec.record_help_beacon(ME, "123 456 789", "Reception PC", "RECEPTION-01"));
        // Keep-alive with the same identity: TTL refresh only, no event churn.
        assert!(!cec.record_help_beacon(ME, "123 456 789", "Reception PC", "RECEPTION-01"));
        // A renamed machine is worth re-announcing.
        assert!(cec.record_help_beacon(ME, "123 456 789", "Front desk", "RECEPTION-01"));
        // ...and so is a changed hostname.
        assert!(cec.record_help_beacon(ME, "123 456 789", "Front desk", "RECEPTION-02"));
        // Display-suffix and bare forms are the same asker.
        let display = format!("{ME}-AB12C");
        assert!(!cec.record_help_beacon(&display, "123 456 789", "Front desk", "RECEPTION-02"));
        let list = cec.help_list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["number"], "123 456 789");
        assert_eq!(list[0]["label"], "Front desk");
        assert_eq!(list[0]["hostname"], "RECEPTION-02");
        // Withdrawal (available:false / help arrived) empties the queue.
        assert!(cec.remove_help_beacon(&display));
        assert!(cec.help_list().is_empty());
        assert!(!cec.remove_help_beacon(ME), "second withdrawal is a no-op");
    }

    #[test]
    fn asking_help_transitions_bump_the_epoch_once() {
        let cec = Cec::new(None);
        assert!(!cec.asking_help());
        let e0 = cec.help_epoch();
        assert!(cec.set_asking_help(true));
        assert!(!cec.set_asking_help(true), "re-ask while asking is a no-op");
        let e1 = cec.help_epoch();
        assert_eq!(e1, e0 + 1, "one transition, one epoch bump");
        assert!(cec.set_asking_help(false));
        assert!(!cec.set_asking_help(false));
        assert_eq!(cec.help_epoch(), e1 + 1);
    }
}

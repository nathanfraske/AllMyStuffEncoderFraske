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
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
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
#[derive(Clone, Debug)]
pub struct DialedCustomer {
    /// The customer's device id (as dialed).
    pub node: String,
    /// The support number the technician typed to reach them.
    pub number: String,
    /// Best-known label (the machine name, once presence lands).
    pub label: String,
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
            "online": self.online,
            "last_used": self.last_used,
        })
    }
}

/// The CEC state a node carries — all behind one lock, since both the
/// node-control commands and the mesh's per-frame gate reach it from many
/// tasks. Cheap to hold: the enforcement (expiry, persistence) lives in the
/// [`ConsentStore`], not here.
pub struct Cec {
    inner: Mutex<CecInner>,
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
    /// Customers this technician has dialed (canonical id → record). Surfaced
    /// to the CEC tab via [`Cec::dialed_list`]; each is a normal graph peer.
    dialed: HashMap<String, DialedCustomer>,
    /// Live session states by session id, for `cec://session`.
    sessions: HashMap<String, String>,
}

impl Cec {
    /// Build the CEC state, loading (or, with `None`, running an in-memory)
    /// consent store. The store path is a `consent.json` under the node's home
    /// — a corrupt or absent file loads empty (it never bricks the node), and
    /// only `ThreeHours`/`Forever` grants are ever written.
    pub fn new(consent_path: Option<PathBuf>) -> Self {
        let consent = match consent_path {
            Some(p) => ConsentStore::load(p),
            None => ConsentStore::in_memory(),
        };
        Cec {
            inner: Mutex::new(CecInner {
                role: Role::Client,
                hosting: false,
                number: String::new(),
                network_id: String::new(),
                agent_name: String::new(),
                consent,
                pending: Vec::new(),
                dialed: HashMap::new(),
                sessions: HashMap::new(),
            }),
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
        canonical: &str,
        node: String,
        number: String,
        label: String,
        online: bool,
        network_id: String,
    ) -> DialedCustomer {
        let now = now_secs();
        let mut inner = self.inner.lock();
        let entry = inner
            .dialed
            .entry(canonical.to_string())
            .or_insert_with(|| DialedCustomer {
                node: node.clone(),
                number: number.clone(),
                label: label.clone(),
                online,
                network_id: network_id.clone(),
                last_used: now,
            });
        entry.node = node;
        entry.number = number;
        if !label.is_empty() {
            entry.label = label;
        }
        entry.online = online;
        entry.network_id = network_id;
        // A (re)dial is a fresh use — keep the stale-connection metric honest.
        entry.last_used = now;
        entry.clone()
    }

    /// Mark a dialed customer online/offline (from presence), returning its
    /// updated record when the flag actually changed.
    pub fn set_customer_online(&self, canonical: &str, online: bool) -> Option<DialedCustomer> {
        let mut inner = self.inner.lock();
        let c = inner.dialed.get_mut(canonical)?;
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
        let c = inner.dialed.get_mut(canonical)?;
        c.last_used = now_secs();
        Some(c.clone())
    }

    /// Whether `canonical` is a customer this technician has dialed. Used by
    /// "Forget this node" to know a CEC customer needs its Silent room left too.
    pub fn is_dialed(&self, canonical: &str) -> bool {
        self.inner.lock().dialed.contains_key(canonical)
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

    /// The Silent mesh a dialed customer lives on, if any (for teardown).
    pub fn dialed_network(&self, canonical: &str) -> Option<String> {
        self.inner
            .lock()
            .dialed
            .get(canonical)
            .map(|c| c.network_id.clone())
    }

    /// Drop a customer this technician dialed (the CEC part of "Forget this
    /// node"). Returns `true` when one was actually removed.
    pub fn forget_dialed(&self, canonical: &str) -> bool {
        self.inner.lock().dialed.remove(canonical).is_some()
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
        inner.pending.iter().any(|p| pubkey_part(&p.tech) == key)
            || inner
                .consent
                .active_grants(now_secs())
                .iter()
                .any(|g| pubkey_part(&g.technician) == key)
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
    let code = support_id_from_device(&format!("{tech}:{session_id}"));
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
/// technician must `connect_peer` deliberately. The signaling app-id is forked
/// ([`CEC_TRYSTERO_APP_ID`]) so CEC traffic never lands in an AllMyStuff room.
///
/// [`CEC_TRYSTERO_APP_ID`]: allmystuff_cec_protocol::CEC_TRYSTERO_APP_ID
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
        "app_id": allmystuff_cec_protocol::CEC_TRYSTERO_APP_ID,
        "signaling": { "strategy": "nostr", "mdns": true },
    });
    (network_id, config)
}

/// Canonicalise a device id to its bare pubkey — the same `-XXXXX` display
/// suffix strip the consent store and the mesh use, so a technician isn't seen
/// as a new peer across a reconnect. Re-exported shape of
/// [`allmystuff_cec_consent::pubkey_part`].
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
        cec.record_dialed(
            canon,
            TECH.into(),
            "123456789".into(),
            "Reception PC".into(),
            true,
            "cec-123456789".into(),
        );
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
}

//! Owned event registrations. A client coordinate is not an authority, and a
//! registration cannot be reused after its socket or connection generation ends.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use allmystuff_protocol::{ClientId, Response};
use anyhow::{anyhow, bail, Result};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::task::{AbortHandle, JoinHandle};

/// An explicit wire selection for the small event/channel adapter. The staged
/// candidate is never selected by Mesh or by the shipping request API. It is
/// source-qualified to PR 135/db7818e, not negotiated from a version string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EventContract {
    LegacyV0_3_21,
    #[allow(dead_code)] // Source-characterized only; there is no product selector.
    CandidateV1Db7818e,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionGeneration(u64);

/// Deliberately has no Display, Serialize, or public accessor. Only the bounded
/// channel encoder can borrow the daemon-issued secret. This is event-resource
/// authority, not approved-application identity or access to a user's ledger.
struct ClientCapability(String);

impl fmt::Debug for ClientCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

struct Slot {
    generation: ConnectionGeneration,
    current: Weak<RegistrationState>,
}

pub(super) struct EventSessions {
    slot: Mutex<Slot>,
    pub(super) subscribe: tokio::sync::Mutex<()>,
}

impl EventSessions {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            slot: Mutex::new(Slot {
                generation: ConnectionGeneration(0),
                current: Weak::new(),
            }),
            subscribe: tokio::sync::Mutex::new(()),
        })
    }

    pub(super) fn begin(&self) -> ConnectionGeneration {
        let mut slot = self.slot.lock();
        if let Some(previous) = slot.current.upgrade() {
            previous.invalidate();
        }
        slot.current = Weak::new();
        slot.generation = ConnectionGeneration(
            slot.generation
                .0
                .checked_add(1)
                .expect("event generation exhausted"),
        );
        slot.generation
    }

    pub(super) fn install(
        self: &Arc<Self>,
        contract: EventContract,
        generation: ConnectionGeneration,
        ack: Response,
    ) -> Result<EventRegistration> {
        // No remote ACK/error text is included in errors: it may contain C.
        if !ack.ok || ack.error.is_some() {
            bail!("daemon rejected event subscription");
        }
        let data = ack
            .data
            .as_ref()
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("event subscription ack has no data object"))?;
        let id_text = data
            .get("client_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("event subscription ack has no client_id"))?;
        let client_id: ClientId = id_text
            .parse()
            .map_err(|_| anyhow!("event subscription ack has an invalid client_id"))?;
        let capability = match contract {
            EventContract::LegacyV0_3_21 => None,
            EventContract::CandidateV1Db7818e => {
                if client_id.to_string() != id_text {
                    bail!("candidate event subscription client_id is not canonical");
                }
                if data.get("subscribed").and_then(Value::as_bool) != Some(true) {
                    bail!("candidate event subscription ack is not subscribed");
                }
                let value = data
                    .get("client_capability")
                    .and_then(Value::as_str)
                    .filter(|secret| !secret.is_empty())
                    .ok_or_else(|| {
                        anyhow!("candidate event subscription ack has no client capability")
                    })?;
                Some(ClientCapability(value.to_owned()))
            }
        };
        let state = Arc::new(RegistrationState {
            owner: Arc::downgrade(self),
            client_id,
            capability,
            contract,
            generation,
            active: AtomicBool::new(true),
            readers: Mutex::new(Vec::new()),
        });
        let mut slot = self.slot.lock();
        if slot.generation != generation {
            bail!("event subscription was superseded");
        }
        slot.current = Arc::downgrade(&state);
        Ok(EventRegistration { state })
    }

    pub(super) fn invalidate(&self) {
        if let Some(state) = self.slot.lock().current.upgrade() {
            state.invalidate();
        }
    }
}

struct RegistrationState {
    owner: Weak<EventSessions>,
    client_id: ClientId,
    capability: Option<ClientCapability>,
    contract: EventContract,
    generation: ConnectionGeneration,
    active: AtomicBool,
    readers: Mutex<Vec<AbortHandle>>,
}

impl RegistrationState {
    fn invalidate(&self) {
        self.active.store(false, Ordering::Release);
        for reader in self.readers.lock().drain(..) {
            reader.abort();
        }
    }
}

/// Cloneable registration evidence for one owned socket. Clones do not keep
/// the session alive: dropping the EventSession invalidates every clone.
#[derive(Clone)]
pub struct EventRegistration {
    state: Arc<RegistrationState>,
}

impl fmt::Debug for EventRegistration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventRegistration")
            .field("client_id", &self.client_id())
            .field("generation", &self.generation())
            .field("contract", &self.contract())
            .field("capability", &self.state.capability)
            .field("active", &self.is_active())
            .finish()
    }
}

impl EventRegistration {
    pub fn client_id(&self) -> ClientId {
        self.state.client_id
    }
    pub fn generation(&self) -> ConnectionGeneration {
        self.state.generation
    }
    pub fn is_active(&self) -> bool {
        self.state.active.load(Ordering::Acquire)
    }
    pub(super) fn contract(&self) -> EventContract {
        self.state.contract
    }
    pub(super) fn capability(&self) -> Option<&str> {
        self.state
            .capability
            .as_ref()
            .map(|secret| secret.0.as_str())
    }

    pub(super) fn require_current(&self, owner: &Arc<EventSessions>) -> Result<()> {
        let registered_owner = self
            .state
            .owner
            .upgrade()
            .ok_or_else(|| anyhow!("event registration owner has ended"))?;
        if !Arc::ptr_eq(owner, &registered_owner) || !self.is_active() {
            bail!("event registration is stale or belongs to another client");
        }
        let slot = owner.slot.lock();
        let matches = slot
            .current
            .upgrade()
            .is_some_and(|current| Arc::ptr_eq(&current, &self.state));
        if slot.generation != self.generation() || !matches || !self.is_active() {
            bail!("event registration is stale or belongs to another client");
        }
        Ok(())
    }

    pub(super) fn invalidate(&self) {
        self.state.invalidate();
    }

    pub(super) fn own_reader(&self, reader: AbortHandle) {
        let mut readers = self.state.readers.lock();
        if self.is_active() {
            readers.push(reader);
        } else {
            reader.abort();
        }
    }

    pub(super) fn reader_lifetime(&self) -> ReaderLifetime {
        ReaderLifetime(self.clone())
    }

    pub(super) fn redact_value(&self, value: &mut Value) {
        let Some(secret) = self.capability() else {
            return;
        };
        redact_value(value, secret);
    }

    pub(super) fn redact_response(&self, response: &mut Response) {
        if let Some(secret) = self.capability() {
            if let Some(error) = response.error.as_mut() {
                *error = error.replace(secret, "[redacted]");
            }
            if let Some(data) = response.data.as_mut() {
                redact_value(data, secret);
            }
        }
    }
}

fn redact_value(value: &mut Value, secret: &str) {
    match value {
        Value::String(text) => *text = text.replace(secret, "[redacted]"),
        Value::Array(values) => {
            for value in values {
                redact_value(value, secret);
            }
        }
        Value::Object(values) => {
            let original = std::mem::take(values);
            for (key, mut value) in original {
                redact_value(&mut value, secret);
                values.insert(key.replace(secret, "[redacted]"), value);
            }
        }
        _ => {}
    }
}

pub(super) struct ReaderLifetime(EventRegistration);

impl Drop for ReaderLifetime {
    fn drop(&mut self) {
        self.0.invalidate();
    }
}

/// Owns the event reader task and both socket halves. There is no detached
/// reader after Drop, receiver closure, stream EOF, cancellation or renewal.
#[must_use = "keep the event session alive while using its registration"]
pub struct EventSession {
    registration: EventRegistration,
    reader: Option<JoinHandle<()>>,
}

impl fmt::Debug for EventSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventSession")
            .field("registration", &self.registration)
            .finish()
    }
}

impl EventSession {
    pub(super) fn new(registration: EventRegistration, reader: JoinHandle<()>) -> Self {
        registration.own_reader(reader.abort_handle());
        Self {
            registration,
            reader: Some(reader),
        }
    }

    pub fn registration(&self) -> EventRegistration {
        self.registration.clone()
    }

    pub async fn close(mut self) {
        self.registration.invalidate();
        if let Some(reader) = self.reader.take() {
            let _ = reader.await;
        }
    }
}

impl Drop for EventSession {
    fn drop(&mut self) {
        self.registration.invalidate();
        if let Some(reader) = self.reader.take() {
            reader.abort();
        }
    }
}

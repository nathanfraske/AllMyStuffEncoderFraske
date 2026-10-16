use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::framing::Budget;
use super::wire::Pipe;
use super::{FlowSpec, PipeEnd, RealtimeError, RealtimePolicy, Result};
use crate::{ControlClient, EventRegistration};

pub(crate) type FlowKey = (String, String, String);
pub(crate) type PeerKey = (String, String);
pub(crate) type InboundSocket = Arc<Mutex<Option<Pipe>>>;

pub(crate) struct Secret(String);

impl Secret {
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }
    pub(crate) fn text(&self) -> &str {
        &self.0
    }
}

impl Clone for Secret {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Life {
    Opening,
    Open,
    Closing,
    Closed,
    Uncertain,
}

pub(crate) struct Record {
    pub(crate) life: Life,
    pub(crate) capability: Option<Secret>,
}

pub(crate) struct FlowState {
    pub(crate) spec: FlowSpec,
    pub(crate) record: Mutex<Record>,
    pub(crate) terminal: watch::Sender<Option<PipeEnd>>,
    pub(crate) monitor: Mutex<Option<JoinHandle<()>>>,
    pub(crate) writer: Mutex<Option<interprocess::local_socket::tokio::SendHalf>>,
}

impl FlowState {
    pub(crate) fn new(spec: FlowSpec) -> Arc<Self> {
        let (terminal, _) = watch::channel(None);
        Arc::new(Self {
            spec,
            record: Mutex::new(Record {
                life: Life::Opening,
                capability: None,
            }),
            terminal,
            monitor: Mutex::new(None),
            writer: Mutex::new(None),
        })
    }

    pub(crate) fn key(&self) -> FlowKey {
        (
            self.spec.network.clone(),
            self.spec.peer.clone(),
            self.spec.label.clone(),
        )
    }

    pub(crate) fn signal(&self, end: PipeEnd) {
        self.terminal.send_if_modified(|current| {
            if current.is_none() {
                *current = Some(end);
                true
            } else {
                false
            }
        });
    }

    pub(crate) fn abort_monitor(&self) {
        if let Some(task) = self.monitor.lock().as_ref() {
            task.abort();
        }
    }

    pub(crate) async fn join_monitor(&self) {
        let task = self.monitor.lock().take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
    }
}

pub(crate) struct Shared {
    pub(crate) client: Arc<ControlClient>,
    pub(crate) registration: EventRegistration,
    pub(crate) policy: RealtimePolicy,
    pub(crate) budget: Arc<Budget>,
    pub(crate) flows: Mutex<BTreeMap<FlowKey, Arc<FlowState>>>,
    pub(crate) incoming: Mutex<BTreeMap<PeerKey, InboundSocket>>,
    pub(crate) unknown: AtomicBool,
    pub(crate) fenced: AtomicBool,
    cancelled: watch::Sender<bool>,
}

impl Shared {
    pub(crate) fn new(
        client: Arc<ControlClient>,
        registration: EventRegistration,
        policy: RealtimePolicy,
    ) -> Arc<Self> {
        let budget = Budget::new(policy.max_retained_bytes);
        let (cancelled, _) = watch::channel(false);
        Arc::new(Self {
            client,
            registration,
            policy,
            budget,
            flows: Mutex::new(BTreeMap::new()),
            incoming: Mutex::new(BTreeMap::new()),
            unknown: AtomicBool::new(false),
            fenced: AtomicBool::new(false),
            cancelled,
        })
    }

    pub(crate) fn verify(&self) -> Result<()> {
        self.client
            .validate_registration(&self.registration)
            .map_err(|_| RealtimeError::StaleSession)?;
        if self.fenced.load(Ordering::Acquire) {
            return Err(RealtimeError::SessionFenced);
        }
        Ok(())
    }

    pub(crate) fn fence(&self, unknown: bool) {
        if unknown {
            self.unknown.store(true, Ordering::Release);
        }
        self.fenced.store(true, Ordering::Release);
        self.cancelled.send_replace(true);
        for flow in self.flows.lock().values() {
            flow.signal(PipeEnd::SessionEnded);
            // Custody is here, not in an idle external handle. Closing the
            // owner therefore releases both socket halves without more IO.
            flow.writer.lock().take();
            flow.abort_monitor();
        }
        for socket in self.incoming.lock().values() {
            socket.lock().take();
        }
    }

    pub(crate) fn end(&self) {
        self.fence(false);
    }

    pub(crate) async fn cancelled(&self) {
        let mut notice = self.cancelled.subscribe();
        loop {
            if *notice.borrow() || self.verify().is_err() {
                return;
            }
            if notice.changed().await.is_err() {
                return;
            }
        }
    }

    pub(crate) fn reserve_flow(&self, spec: FlowSpec) -> Result<Arc<FlowState>> {
        self.verify()?;
        let flow = FlowState::new(spec);
        let mut flows = self.flows.lock();
        if flows.contains_key(&flow.key()) {
            return Err(RealtimeError::InvalidInput(
                "flow label is still reserved for this peer",
            ));
        }
        if flows.len() >= self.policy.max_flows {
            return Err(RealtimeError::LimitExceeded("owned flow count"));
        }
        flows.insert(flow.key(), flow.clone());
        Ok(flow)
    }

    pub(crate) fn release_flow(&self, flow: &Arc<FlowState>) {
        let mut flows = self.flows.lock();
        if flows
            .get(&flow.key())
            .is_some_and(|current| Arc::ptr_eq(current, flow))
        {
            flows.remove(&flow.key());
        }
    }

    pub(crate) fn uncertain(&self, flow: &FlowState) {
        flow.record.lock().life = Life::Uncertain;
        flow.signal(PipeEnd::LocalClosed);
        self.fence(true);
    }
}

/// Constructed before spawning, so abort-before-first-poll also ends this owner.
pub(crate) struct RegistrationEnd(pub(crate) Weak<Shared>);

impl Drop for RegistrationEnd {
    fn drop(&mut self) {
        if let Some(shared) = self.0.upgrade() {
            shared.end();
        }
    }
}

pub(crate) struct MonitorEnd(pub(crate) Arc<FlowState>);
impl Drop for MonitorEnd {
    fn drop(&mut self) {
        self.0.signal(PipeEnd::LocalClosed);
    }
}

pub(crate) struct CloseGuard {
    shared: Arc<Shared>,
    flow: Arc<FlowState>,
    armed: bool,
}

impl CloseGuard {
    pub(crate) fn new(shared: Arc<Shared>, flow: Arc<FlowState>) -> Self {
        Self {
            shared,
            flow,
            armed: true,
        }
    }
    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CloseGuard {
    fn drop(&mut self) {
        if self.armed {
            self.shared.uncertain(&self.flow);
        }
    }
}

pub(crate) struct IoGuard {
    flow: Arc<FlowState>,
    armed: bool,
}

impl IoGuard {
    pub(crate) fn new(flow: Arc<FlowState>) -> Self {
        Self { flow, armed: true }
    }
    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for IoGuard {
    fn drop(&mut self) {
        if self.armed {
            self.flow.signal(PipeEnd::LocalClosed);
        }
    }
}

pub(crate) struct InboundClaim {
    shared: Weak<Shared>,
    pub(crate) key: PeerKey,
    pub(crate) socket: InboundSocket,
}

impl InboundClaim {
    pub(crate) fn reserve(shared: &Arc<Shared>, network: &str, peer: &str) -> Result<Self> {
        shared.verify()?;
        let key = (network.to_owned(), peer.to_owned());
        let mut incoming = shared.incoming.lock();
        if incoming.contains_key(&key) {
            return Err(RealtimeError::InvalidInput(
                "inbound peer pipe is already owned",
            ));
        }
        if incoming.len() >= shared.policy.max_inbound_pipes {
            return Err(RealtimeError::LimitExceeded("inbound pipe count"));
        }
        let socket = Arc::new(Mutex::new(None));
        incoming.insert(key.clone(), socket.clone());
        Ok(Self {
            shared: Arc::downgrade(shared),
            key,
            socket,
        })
    }

    pub(crate) fn install(&self, pipe: Pipe) -> Result<()> {
        let mut socket = self.socket.lock();
        let shared = self.shared.upgrade().ok_or(RealtimeError::StaleSession)?;
        // Fencing sets its flag before taking this custody lock, so either
        // installation refuses, or the ending owner takes the installed pipe.
        shared.verify()?;
        *socket = Some(pipe);
        Ok(())
    }
}

impl Drop for InboundClaim {
    fn drop(&mut self) {
        self.socket.lock().take();
        if let Some(shared) = self.shared.upgrade() {
            let mut incoming = shared.incoming.lock();
            if incoming
                .get(&self.key)
                .is_some_and(|socket| Arc::ptr_eq(socket, &self.socket))
            {
                incoming.remove(&self.key);
            }
        }
    }
}

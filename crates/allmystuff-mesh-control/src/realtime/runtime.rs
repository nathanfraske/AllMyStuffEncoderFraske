use std::fmt;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use super::framing::{body_size, ReceivedUnit, HEADER};
use super::state::{
    CloseGuard, FlowState, InboundClaim, InboundSocket, IoGuard, Life, MonitorEnd, RegistrationEnd,
    Secret, Shared,
};
use super::wire::{self, Pipe};
use super::{FlowDirection, FlowSpec, PipeEnd, RealtimeError, RealtimePolicy, Result};
use crate::{ConnectionGeneration, ControlClient, EventContract, EventRegistration, EventSession};

/// One event generation and its bounded realtime cleanup registry. The event
/// session is moved in, never reconstructed from an id or capability string.
#[must_use = "keep the owner alive and await close for acknowledged retirement"]
pub struct RealtimeSession {
    shared: Arc<Shared>,
    events: Option<EventSession>,
    watcher: Option<JoinHandle<()>>,
    pending: Option<JoinHandle<()>>,
}

impl RealtimeSession {
    /// Requires a running Tokio runtime. A caller deliberately transfers the
    /// event session's lifetime, including any registrations already using it.
    pub fn new(
        client: Arc<ControlClient>,
        events: EventSession,
        policy: RealtimePolicy,
    ) -> Result<Self> {
        policy.validate()?;
        let registration = events.registration();
        if registration.contract() != EventContract::CandidateV1Db7818e {
            return Err(RealtimeError::UnsupportedContract);
        }
        client
            .validate_registration(&registration)
            .map_err(|_| RealtimeError::StaleSession)?;
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| RealtimeError::InvalidInput("a running Tokio runtime is required"))?;
        let shared = Shared::new(client, registration, policy);
        let guard = RegistrationEnd(Arc::downgrade(&shared));
        let watcher = runtime.spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        });
        // One watcher per moved event session, not one accumulating handle per
        // send or per recycled flow. Every pipe observes this shared ending.
        shared.registration.own_reader(watcher.abort_handle());
        Ok(Self {
            shared,
            events: Some(events),
            watcher: Some(watcher),
            pending: None,
        })
    }

    pub fn registration(&self) -> EventRegistration {
        self.shared.registration.clone()
    }
    pub fn generation(&self) -> ConnectionGeneration {
        self.shared.registration.generation()
    }
    pub fn is_fenced(&self) -> bool {
        self.shared.fenced.load(Ordering::Acquire)
    }
    pub fn is_current(&self) -> bool {
        self.shared.verify().is_ok()
    }

    async fn join_pending(&mut self) -> Result<()> {
        // Await by reference: cancelling this await leaves the join owned here.
        let result = match self.pending.as_mut() {
            Some(task) => Some(task.await),
            None => None,
        };
        if let Some(result) = result {
            self.pending.take();
            if result.is_err() {
                self.shared.fence(true);
                return Err(RealtimeError::CleanupUncertain);
            }
        }
        Ok(())
    }

    /// Open exactly the caller's label and encoding. Outbound setup includes
    /// its dedicated pipe acknowledgment; inbound setup owns a flow only.
    ///
    /// A cancelled call fences this local owner. Its tracked worker completes
    /// bounded acknowledgment handling and closes a known F exactly once. An
    /// unknown F remains uncertain; dropping Events is not a native close ACK.
    pub async fn open_flow(&mut self, spec: FlowSpec) -> Result<RealtimeFlow> {
        self.join_pending().await?;
        self.shared.verify()?;
        wire::validate_spec(&spec)?;
        body_size(spec.label.len(), 0, self.shared.policy.max_frame_bytes)?;
        let line = wire::open_line(&self.shared, &spec)?;
        let flow = self.shared.reserve_flow(spec)?;
        let (reply, receive) = oneshot::channel();
        let shared = self.shared.clone();
        let record = flow.clone();
        self.pending = Some(tokio::spawn(async move {
            open_worker(shared, record, line, reply).await;
        }));
        let mut waiting = AwaitOpenGuard {
            shared: self.shared.clone(),
            armed: true,
        };
        let result = match receive.await {
            Ok(Ok(offer)) => offer.accept(),
            Ok(Err(error)) => Err(error),
            Err(_) => {
                self.shared.uncertain(&flow);
                Err(RealtimeError::CleanupUncertain)
            }
        };
        waiting.armed = false;
        result
    }

    /// Claim one exact peer session's inbound stream. That pipe carries every
    /// inbound label for the peer; labels in units confer no flow authority.
    pub async fn open_inbound(&mut self, network: &str, peer: &str) -> Result<InboundPipe> {
        self.join_pending().await?;
        self.shared.verify()?;
        if network.is_empty()
            || peer.is_empty()
            || network.len() > crate::STATUS_ACK_LIMIT
            || peer.len() > crate::STATUS_ACK_LIMIT
        {
            return Err(RealtimeError::InvalidInput("inbound peer metadata"));
        }
        let claim = InboundClaim::reserve(&self.shared, network, peer)?;
        let pipe = wire::bind_pipe(&self.shared, network, Some(peer), None).await?;
        claim.install(pipe)?;
        Ok(InboundPipe {
            shared: self.shared.clone(),
            network: network.to_owned(),
            peer: peer.to_owned(),
            socket: claim.socket.clone(),
            claim: Some(claim),
        })
    }

    /// Attempt every remaining exact known-F close, even after another fails.
    /// Then join owned setup/monitor work and close the local event reader.
    /// Unknown capabilities or unacknowledged closes always remain an error;
    /// local event closure does not prove server/native cleanup at this revision.
    pub async fn close(mut self) -> Result<()> {
        let mut first = self.join_pending().await.err();
        let flows: Vec<_> = self.shared.flows.lock().values().cloned().collect();
        for flow in flows {
            let life = flow.record.lock().life;
            let outcome = match life {
                Life::Open => close_exact(&self.shared, &flow).await,
                Life::Closed => Ok(()),
                Life::Opening | Life::Closing | Life::Uncertain => {
                    self.shared.fence(true);
                    flow.signal(PipeEnd::LocalClosed);
                    flow.join_monitor().await;
                    Err(RealtimeError::CleanupUncertain)
                }
            };
            if first.is_none() {
                first = outcome.err();
            }
        }
        self.shared.end();
        if let Some(events) = self.events.take() {
            events.close().await;
        }
        if let Some(watcher) = self.watcher.take() {
            watcher.abort();
            let _ = watcher.await;
        }
        if self.shared.unknown.load(Ordering::Acquire) {
            return Err(RealtimeError::CleanupUncertain);
        }
        match first {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Drop for RealtimeSession {
    fn drop(&mut self) {
        self.shared.end();
        if let Some(pending) = self.pending.take() {
            pending.abort();
        }
        if let Some(watcher) = self.watcher.take() {
            watcher.abort();
        }
        // Dropping EventSession aborts its reader. No async Drop worker is
        // spawned and no remote retirement is claimed by this synchronous path.
        self.events.take();
    }
}

impl fmt::Debug for RealtimeSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RealtimeSession")
            .field("generation", &self.generation())
            .field("fenced", &self.is_fenced())
            .finish()
    }
}

struct PendingGuard {
    shared: Arc<Shared>,
    flow: Arc<FlowState>,
    armed: bool,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self.armed {
            self.shared.uncertain(&self.flow);
        }
    }
}

struct AwaitOpenGuard {
    shared: Arc<Shared>,
    armed: bool,
}
impl Drop for AwaitOpenGuard {
    fn drop(&mut self) {
        if self.armed {
            self.shared.fence(false);
        }
    }
}

struct OpenOffer {
    flow: Option<RealtimeFlow>,
    decision: Option<oneshot::Sender<bool>>,
}

impl OpenOffer {
    fn accept(mut self) -> Result<RealtimeFlow> {
        let accepted = self
            .decision
            .take()
            .is_some_and(|reply| reply.send(true).is_ok());
        if !accepted {
            if let Some(flow) = &self.flow {
                flow.shared.uncertain(&flow.state);
            }
            return Err(RealtimeError::CleanupUncertain);
        }
        Ok(self.flow.take().expect("an offer owns one flow"))
    }
}

impl Drop for OpenOffer {
    fn drop(&mut self) {
        // If the oneshot held this offer when the caller cancelled, this path
        // still runs. The worker retains F until the decision is observed.
        self.flow.take();
        if let Some(reply) = self.decision.take() {
            let _ = reply.send(false);
        }
    }
}

async fn open_worker(
    shared: Arc<Shared>,
    flow: Arc<FlowState>,
    line: String,
    reply: oneshot::Sender<Result<OpenOffer>>,
) {
    let mut guard = PendingGuard {
        shared: shared.clone(),
        flow: flow.clone(),
        armed: true,
    };
    if reply.is_closed() {
        shared.fence(false);
        finish_unopened(&shared, &flow);
        guard.armed = false;
        return;
    }
    let prepared = prepare_open(&shared, &flow, &line).await;
    match prepared {
        Ok(()) => (),
        Err(error) => {
            if reply.is_closed() {
                shared.fence(false);
            }
            let cleanup = settle_failed_open(&shared, &flow).await;
            guard.armed = false;
            let error = if cleanup.is_err() {
                RealtimeError::CleanupUncertain
            } else {
                error
            };
            let _ = reply.send(Err(error));
            return;
        }
    }
    if reply.is_closed() || shared.verify().is_err() {
        shared.fence(false);
        let cleanup = settle_failed_open(&shared, &flow).await;
        guard.armed = false;
        let error = if cleanup.is_err() {
            RealtimeError::CleanupUncertain
        } else {
            RealtimeError::StaleSession
        };
        let _ = reply.send(Err(error));
        return;
    }
    let (decide, decision) = oneshot::channel();
    let offer = OpenOffer {
        flow: Some(RealtimeFlow {
            shared: shared.clone(),
            state: flow.clone(),
        }),
        decision: Some(decide),
    };
    let delivered = reply.send(Ok(offer));
    if let Err(offer) = delivered {
        drop(offer);
    }
    // The decision has an owner in the caller's offer. It is sent without an
    // await on acceptance, or by Drop when the caller/oneshot is discarded.
    // There is no timer race that can retire a successfully accepted offer.
    if !matches!(decision.await, Ok(true)) {
        shared.fence(false);
        let _ = settle_failed_open(&shared, &flow).await;
    }
    guard.armed = false;
}

fn finish_unopened(shared: &Shared, flow: &Arc<FlowState>) {
    flow.record.lock().life = Life::Closed;
    shared.release_flow(flow);
}

async fn prepare_open(shared: &Arc<Shared>, flow: &Arc<FlowState>, line: &str) -> Result<()> {
    let response = match wire::exchange(shared, line, true).await {
        Ok(response) => response,
        Err(failure) => {
            if failure.sent {
                shared.uncertain(flow);
                return Err(RealtimeError::CleanupUncertain);
            }
            return Err(failure.error);
        }
    };
    let capability = response
        .data
        .as_ref()
        .and_then(|data| data.get("flow_capability"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(|value| Secret::new(value.to_owned()));
    if let Some(capability) = capability.as_ref() {
        // A duplicate capability must not turn a malformed new ACK into an
        // instruction to close another still-owned flow.
        let duplicate = shared.flows.lock().values().any(|other| {
            !Arc::ptr_eq(other, flow)
                && other
                    .record
                    .lock()
                    .capability
                    .as_ref()
                    .is_some_and(|current| current.text() == capability.text())
        });
        if duplicate {
            shared.uncertain(flow);
            return Err(RealtimeError::CleanupUncertain);
        }
        let mut record = flow.record.lock();
        record.capability = Some(capability.clone());
        record.life = Life::Open;
    }
    if !response.ok {
        return Err(wire::refusal(shared, &response, capability.as_ref()));
    }
    if capability.is_none() {
        shared.uncertain(flow);
        return Err(RealtimeError::CleanupUncertain);
    }
    if response.error.is_some()
        || response
            .data
            .as_ref()
            .and_then(|data| data.get("flow_label"))
            .and_then(|value| value.as_str())
            != Some(flow.spec.label.as_str())
    {
        return Err(RealtimeError::InvalidResponse("flow acknowledgment"));
    }
    shared.verify()?;
    if flow.spec.direction == FlowDirection::Inbound {
        return Ok(());
    }
    let pipe = wire::bind_pipe(shared, &flow.spec.network, None, capability.as_ref()).await?;
    let Pipe { reader, writer } = pipe;
    {
        let mut custody = flow.writer.lock();
        shared.verify()?;
        *custody = Some(writer);
    }
    spawn_monitor(shared, flow, reader)
}

async fn settle_failed_open(shared: &Arc<Shared>, flow: &Arc<FlowState>) -> Result<()> {
    let life = flow.record.lock().life;
    match life {
        Life::Opening => {
            finish_unopened(shared, flow);
            Ok(())
        }
        Life::Open => close_exact(shared, flow).await,
        Life::Closed => Ok(()),
        Life::Closing | Life::Uncertain => Err(RealtimeError::CleanupUncertain),
    }
}

async fn close_exact(shared: &Arc<Shared>, flow: &Arc<FlowState>) -> Result<()> {
    let capability = {
        let mut record = flow.record.lock();
        if record.life == Life::Closed {
            return Ok(());
        }
        if record.life != Life::Open {
            return Err(RealtimeError::CleanupUncertain);
        }
        let capability = record
            .capability
            .clone()
            .ok_or(RealtimeError::CleanupUncertain)?;
        record.life = Life::Closing;
        capability
    };
    let mut guard = CloseGuard::new(shared.clone(), flow.clone());
    flow.signal(PipeEnd::LocalClosed);
    flow.writer.lock().take();
    flow.join_monitor().await;
    let outcome = async {
        let line = wire::close_line(shared, &capability)?;
        // Cleanup uses only the exact old id+C+F, including after local renewal.
        // It cannot resolve a successor by peer or label and is never retried.
        let response = wire::exchange(shared, &line, false)
            .await
            .map_err(|failure| failure.error)?;
        if !response.ok || response.error.is_some() {
            return Err(wire::refusal(shared, &response, Some(&capability)));
        }
        if response
            .data
            .as_ref()
            .and_then(|data| data.get("closed"))
            .and_then(|value| value.as_bool())
            != Some(true)
        {
            return Err(RealtimeError::InvalidResponse("close acknowledgment"));
        }
        Ok(())
    }
    .await;
    if outcome.is_ok() {
        let mut record = flow.record.lock();
        record.life = Life::Closed;
        record.capability = None;
        drop(record);
        shared.release_flow(flow);
    } else {
        shared.uncertain(flow);
    }
    guard.disarm();
    outcome
}

fn spawn_monitor(
    shared: &Arc<Shared>,
    flow: &Arc<FlowState>,
    mut reader: tokio::io::BufReader<interprocess::local_socket::tokio::RecvHalf>,
) -> Result<()> {
    let mut monitor_slot = flow.monitor.lock();
    shared.verify()?;
    let shared = shared.clone();
    let state = flow.clone();
    let guard = MonitorEnd(state.clone());
    let monitor = tokio::spawn(async move {
        let _guard = guard;
        let mut byte = [0u8; 1];
        let end = tokio::select! {
            biased;
            () = shared.cancelled() => PipeEnd::SessionEnded,
            result = reader.read(&mut byte) => match result {
                Ok(0) => PipeEnd::RemoteClosed,
                Ok(_) => PipeEnd::InvalidData,
                Err(_) => PipeEnd::IoFailure,
            },
        };
        state.signal(end);
    });
    *monitor_slot = Some(monitor);
    Ok(())
}

/// A flow-bound sender/retirement handle. It cannot be cloned or constructed
/// from a label or capability. Drop closes its local pipe; the owner retains
/// the exact F and reserved label until an awaited close acknowledges retirement.
#[must_use = "await close or close the owning realtime session"]
pub struct RealtimeFlow {
    shared: Arc<Shared>,
    state: Arc<FlowState>,
}

impl RealtimeFlow {
    pub fn spec(&self) -> &FlowSpec {
        &self.state.spec
    }
    pub fn generation(&self) -> ConnectionGeneration {
        self.shared.registration.generation()
    }
    pub fn termination(&self) -> FlowTermination {
        FlowTermination {
            receiver: self.state.terminal.subscribe(),
        }
    }

    /// Write one complete codec unit with no byte/clock/pacing transformation.
    /// Success means a complete LOCAL write, not remote admission or delivery.
    /// A local size/budget refusal preserves the pipe. Once IO begins, timeout,
    /// cancellation or partial IO permanently drops that socket.
    pub async fn send(&mut self, duration_us: u32, payload: &[u8]) -> Result<()> {
        self.shared.verify()?;
        if self.state.spec.direction != FlowDirection::Outbound {
            return Err(RealtimeError::InvalidInput("inbound flow cannot send"));
        }
        if self.state.record.lock().life != Life::Open {
            return Err(RealtimeError::PipeEnded(PipeEnd::LocalClosed));
        }
        let length = body_size(
            self.state.spec.label.len(),
            payload.len(),
            self.shared.policy.max_frame_bytes,
        )?;
        let _lease = self.shared.budget.reserve(length + 4)?;
        if let Some(end) = *self.state.terminal.borrow() {
            return Err(RealtimeError::PipeEnded(end));
        }
        let mut writer = self
            .state
            .writer
            .lock()
            .take()
            .ok_or(RealtimeError::PipeEnded(PipeEnd::LocalClosed))?;
        let mut guard = IoGuard::new(self.state.clone());
        let mut terminal = self.state.terminal.subscribe();
        let mut header = [0u8; HEADER];
        header[0] = self.state.spec.label.len() as u8;
        header[2..6].copy_from_slice(&duration_us.to_le_bytes());
        header[6..10].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        let result = tokio::select! {
            biased;
            () = self.shared.cancelled() => Err(RealtimeError::StaleSession),
            end = wait_terminal(&mut terminal) => Err(RealtimeError::PipeEnded(end)),
            outcome = tokio::time::timeout(self.shared.policy.io_timeout, async {
                writer.write_all(&(length as u32).to_le_bytes()).await?;
                writer.write_all(&header).await?;
                writer.write_all(self.state.spec.label.as_bytes()).await?;
                writer.write_all(payload).await?;
                writer.flush().await
            }) => match outcome {
                Ok(Ok(())) => Ok(()),
                Ok(Err(_)) => Err(RealtimeError::Transport),
                Err(_) => Err(RealtimeError::Timeout),
            },
        };
        if result.is_ok() {
            let mut custody = self.state.writer.lock();
            self.shared.verify()?;
            if let Some(end) = *self.state.terminal.borrow() {
                return Err(RealtimeError::PipeEnded(end));
            }
            *custody = Some(writer);
            guard.disarm();
        }
        result
    }

    /// Await this exact F's native retirement acknowledgment. A refused,
    /// cancelled or lost close is fenced and cannot be retried by label or F.
    pub async fn close(self) -> Result<()> {
        self.state.writer.lock().take();
        self.shared.verify()?;
        close_exact(&self.shared, &self.state).await
    }
}

impl Drop for RealtimeFlow {
    fn drop(&mut self) {
        self.state.writer.lock().take();
        self.state.signal(PipeEnd::LocalClosed);
        self.state.abort_monitor();
    }
}

impl fmt::Debug for RealtimeFlow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RealtimeFlow")
            .field("direction", &self.state.spec.direction)
            .field("generation", &self.generation())
            .finish()
    }
}

/// Cloneable EOF/terminal observation; cancelling a wait never closes a pipe.
/// It cannot report per-unit admission or distinguish invisible unit drops.
#[derive(Clone)]
pub struct FlowTermination {
    receiver: watch::Receiver<Option<PipeEnd>>,
}

impl FlowTermination {
    pub async fn wait(&mut self, timeout: Duration) -> Result<PipeEnd> {
        if timeout.is_zero() || std::time::Instant::now().checked_add(timeout).is_none() {
            return Err(RealtimeError::InvalidInput("terminal wait deadline"));
        }
        tokio::time::timeout(timeout, wait_terminal(&mut self.receiver))
            .await
            .map_err(|_| RealtimeError::Timeout)
    }
}

async fn wait_terminal(receiver: &mut watch::Receiver<Option<PipeEnd>>) -> PipeEnd {
    loop {
        let current = *receiver.borrow_and_update();
        if let Some(end) = current {
            return end;
        }
        if receiver.changed().await.is_err() {
            return PipeEnd::LocalClosed;
        }
    }
}

/// One network/peer session's raw incoming stream. A failed or cancelled read
/// drops its entire socket, so a partial prefix/body is never reused.
#[must_use = "keep the inbound pipe alive while receiving"]
pub struct InboundPipe {
    shared: Arc<Shared>,
    network: String,
    peer: String,
    socket: InboundSocket,
    claim: Option<InboundClaim>,
}

impl InboundPipe {
    pub fn network(&self) -> &str {
        &self.network
    }
    pub fn peer(&self) -> &str {
        &self.peer
    }
    pub fn generation(&self) -> ConnectionGeneration {
        self.shared.registration.generation()
    }

    pub async fn recv(&mut self) -> Result<ReceivedUnit> {
        self.shared.verify()?;
        let mut pipe = self
            .socket
            .lock()
            .take()
            .ok_or(RealtimeError::PipeEnded(PipeEnd::LocalClosed))?;
        let result = tokio::select! {
            biased;
            () = self.shared.cancelled() => Err(RealtimeError::StaleSession),
            outcome = tokio::time::timeout(self.shared.policy.io_timeout, async {
                let mut prefix = [0u8; 4];
                pipe.reader.read_exact(&mut prefix).await.map_err(|_| RealtimeError::PipeEnded(PipeEnd::RemoteClosed))?;
                let length = u32::from_le_bytes(prefix) as usize;
                if length < HEADER + 1 {
                    return Err(RealtimeError::InvalidResponse("short binary body"));
                }
                if length > self.shared.policy.max_frame_bytes {
                    return Err(RealtimeError::LimitExceeded("binary body bytes"));
                }
                let bytes = length.checked_add(4).ok_or(RealtimeError::LimitExceeded("binary frame wire width"))?;
                let lease = self.shared.budget.reserve(bytes)?;
                let mut body = Vec::new();
                body.try_reserve_exact(length).map_err(|_| RealtimeError::LimitExceeded("frame allocation"))?;
                body.resize(length, 0);
                pipe.reader.read_exact(&mut body).await.map_err(|_| RealtimeError::PipeEnded(PipeEnd::RemoteClosed))?;
                ReceivedUnit::decode(body, lease)
            }) => match outcome {
                Ok(result) => result,
                Err(_) => Err(RealtimeError::Timeout),
            },
        };
        if result.is_ok() {
            let mut custody = self.socket.lock();
            self.shared.verify()?;
            *custody = Some(pipe);
        }
        result
    }

    /// Release the local exact-session receive claim; this does not close the
    /// separately owned inbound flows or claim a native flow-close receipt.
    pub async fn close(mut self) -> Result<()> {
        self.socket.lock().take();
        self.claim.take();
        Ok(())
    }
}

impl fmt::Debug for InboundPipe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InboundPipe")
            .field("network", &self.network)
            .field("peer", &self.peer)
            .field("generation", &self.generation())
            .finish()
    }
}

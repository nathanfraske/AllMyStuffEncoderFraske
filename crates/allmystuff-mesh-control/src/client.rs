use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use interprocess::local_socket::tokio::prelude::*;
use tokio::io::BufReader;
use tokio::sync::mpsc;

use crate::contract::verify_wire_status;
use crate::session::EventSessions;
use crate::transport::{connect_endpoint, read_json_line, round_trip};
use crate::{
    Endpoint, EventContract, EventRegistration, EventSession, Request, Response,
    CANDIDATE_EVENT_LINE_LIMIT, CONTROL_TIMEOUT, STATUS_ACK_LIMIT,
};

/// Optional host policy applied before portable wire validation on the same
/// Status response. Its error is returned unchanged; it opens no other socket.
pub type StatusGuard = fn(&Response) -> Result<()>;

/// A raw Status observation. Only a native NotFound during connect is absence;
/// malformed, rejected, denied and timed-out endpoints remain present evidence.
#[derive(Clone, Debug)]
pub enum StatusProbe {
    Absent,
    Answered(Response),
    Unavailable(String),
}

/// Local control transport with an explicit endpoint and owned event slots.
/// There is no default address, daemon launch or automatic wire selection.
pub struct ControlClient {
    addr: Endpoint,
    events: Arc<EventSessions>,
}

impl Drop for ControlClient {
    fn drop(&mut self) {
        self.events.invalidate();
    }
}

impl ControlClient {
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            addr: endpoint,
            events: EventSessions::new(),
        }
    }

    /// Observe safe Status without interpreting application readiness.
    pub async fn probe_status(&self) -> StatusProbe {
        let stream = match self.connect().await {
            Ok(stream) => stream,
            Err(error) => {
                let missing = error.chain().any(|cause| {
                    cause
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|cause| cause.kind() == std::io::ErrorKind::NotFound)
                });
                return if missing {
                    StatusProbe::Absent
                } else {
                    StatusProbe::Unavailable("cannot connect to daemon endpoint".into())
                };
            }
        };
        let (reader, mut writer) = stream.split();
        let mut reader = BufReader::new(reader);
        match round_trip(
            &mut reader,
            &mut writer,
            "{\"op\":\"status\"}\n",
            CONTROL_TIMEOUT,
            Some(STATUS_ACK_LIMIT),
        )
        .await
        {
            Ok(response) => StatusProbe::Answered(response),
            Err(_) => StatusProbe::Unavailable("daemon Status could not be read".into()),
        }
    }

    /// Connect to the explicit endpoint for a host-owned adapter. The returned
    /// socket carries no verified product policy or forged registration.
    pub async fn connect(&self) -> Result<LocalSocketStream> {
        connect_endpoint(&self.addr).await
    }

    async fn check_on_connection<R, W>(
        reader: &mut R,
        writer: &mut W,
        contract: EventContract,
        guard: Option<StatusGuard>,
    ) -> Result<()>
    where
        R: tokio::io::AsyncBufRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin,
    {
        let status = round_trip(
            reader,
            writer,
            "{\"op\":\"status\"}\n",
            CONTROL_TIMEOUT,
            Some(STATUS_ACK_LIMIT),
        )
        .await?;
        if let Some(guard) = guard {
            guard(&status)?;
        }
        verify_wire_status(&status, contract)?;
        Ok(())
    }

    /// One-shot legacy command or safe diagnostic Status. Candidate operations
    /// are restricted to the explicit owned event/channel methods below.
    pub async fn request(&self, contract: EventContract, req: &Request) -> Result<Response> {
        self.request_with_timeout(contract, req, CONTROL_TIMEOUT)
            .await
    }

    pub async fn request_with_timeout(
        &self,
        contract: EventContract,
        req: &Request,
        read_timeout: Duration,
    ) -> Result<Response> {
        self.request_checked_inner(contract, req, read_timeout, None)
            .await
    }

    /// Apply the host guard on the actual command connection before delivering
    /// a protected request. Status itself remains an unguarded diagnostic.
    pub async fn request_checked(
        &self,
        contract: EventContract,
        req: &Request,
        read_timeout: Duration,
        guard: StatusGuard,
    ) -> Result<Response> {
        self.request_checked_inner(contract, req, read_timeout, Some(guard))
            .await
    }

    async fn request_checked_inner(
        &self,
        contract: EventContract,
        req: &Request,
        read_timeout: Duration,
        guard: Option<StatusGuard>,
    ) -> Result<Response> {
        if Self::request_client_id(req).is_some() || matches!(req, Request::EventsSubscribe) {
            bail!("session-bound request requires an owned event registration");
        }
        if contract == EventContract::CandidateV1Db7818e && !matches!(req, Request::Status) {
            bail!("candidate request is not part of the characterized event/channel adapter");
        }
        self.request_inner(contract, req, read_timeout, None, guard)
            .await
    }

    async fn request_inner(
        &self,
        contract: EventContract,
        req: &Request,
        read_timeout: Duration,
        registration: Option<&EventRegistration>,
        guard: Option<StatusGuard>,
    ) -> Result<Response> {
        if let Some(registration) = registration {
            self.validate_registration(registration)?;
        }
        let stream = self.connect().await?;
        let (reader, mut writer) = stream.split();
        let mut reader = BufReader::new(reader);
        if !matches!(req, Request::Status) {
            Self::check_on_connection(&mut reader, &mut writer, contract, guard).await?;
        }
        if let Some(registration) = registration {
            self.validate_registration(registration)?;
        }
        let line = serde_json::to_string(req)? + "\n";
        round_trip(
            &mut reader,
            &mut writer,
            &line,
            read_timeout,
            matches!(req, Request::Status).then_some(STATUS_ACK_LIMIT),
        )
        .await
    }

    /// Check owner, active socket and generation without exposing capability C.
    pub fn validate_registration(&self, registration: &EventRegistration) -> Result<()> {
        registration.require_current(&self.events)
    }

    fn request_client_id(req: &Request) -> Option<allmystuff_protocol::ClientId> {
        match req {
            Request::ChannelSubscribe { client_id, .. }
            | Request::ChannelUnsubscribe { client_id, .. }
            | Request::VideoSubscribe { client_id, .. }
            | Request::VideoUnsubscribe { client_id, .. }
            | Request::AudioSubscribe { client_id, .. }
            | Request::AudioUnsubscribe { client_id, .. }
            | Request::RpcRegister { client_id, .. }
            | Request::RpcUnregister { client_id, .. }
            | Request::MediaSourcePipe { client_id } => Some(*client_id),
            _ => None,
        }
    }

    pub async fn request_for_registration(
        &self,
        registration: &EventRegistration,
        req: &Request,
    ) -> Result<Response> {
        self.request_for_registration_inner(registration, req, None)
            .await
    }

    pub async fn request_for_registration_checked(
        &self,
        registration: &EventRegistration,
        req: &Request,
        guard: StatusGuard,
    ) -> Result<Response> {
        self.request_for_registration_inner(registration, req, Some(guard))
            .await
    }

    async fn request_for_registration_inner(
        &self,
        registration: &EventRegistration,
        req: &Request,
        guard: Option<StatusGuard>,
    ) -> Result<Response> {
        self.validate_registration(registration)?;
        if registration.contract() != EventContract::LegacyV0_3_21
            || Self::request_client_id(req) != Some(registration.client_id())
        {
            bail!("request is not part of this legacy event registration");
        }
        self.request_inner(
            EventContract::LegacyV0_3_21,
            req,
            CONTROL_TIMEOUT,
            Some(registration),
            guard,
        )
        .await
    }

    /// Subscribe an ordinary channel using the daemon-issued owned registration.
    /// Candidate encoding inserts C internally and redacts returned payloads.
    pub async fn subscribe_channel(
        &self,
        registration: &EventRegistration,
        network: &str,
        channel: &str,
    ) -> Result<Response> {
        self.subscribe_channel_inner(registration, network, channel, None)
            .await
    }

    /// Legacy host-policy variant; the guard runs on the actual subscription
    /// socket. It cannot activate the candidate wire as a product fallback.
    pub async fn subscribe_channel_checked(
        &self,
        registration: &EventRegistration,
        network: &str,
        channel: &str,
        guard: StatusGuard,
    ) -> Result<Response> {
        self.subscribe_channel_inner(registration, network, channel, Some(guard))
            .await
    }

    async fn subscribe_channel_inner(
        &self,
        registration: &EventRegistration,
        network: &str,
        channel: &str,
        guard: Option<StatusGuard>,
    ) -> Result<Response> {
        self.validate_registration(registration)?;
        match registration.contract() {
            EventContract::LegacyV0_3_21 => {
                self.request_for_registration_inner(
                    registration,
                    &Request::ChannelSubscribe {
                        client_id: registration.client_id(),
                        network: network.to_owned(),
                        channel: channel.to_owned(),
                    },
                    guard,
                )
                .await
            }
            EventContract::CandidateV1Db7818e => {
                if guard.is_some() {
                    bail!("checked host policy requires the legacy wire contract");
                }
                #[derive(serde::Serialize)]
                struct ChannelSubscribe<'a> {
                    op: &'static str,
                    client_id: allmystuff_protocol::ClientId,
                    client_capability: &'a str,
                    network: &'a str,
                    channel: &'a str,
                }
                let request = ChannelSubscribe {
                    op: "channel_subscribe",
                    client_id: registration.client_id(),
                    client_capability: registration.capability().ok_or_else(|| {
                        anyhow!("candidate registration has no client capability")
                    })?,
                    network,
                    channel,
                };
                let stream = self.connect().await?;
                let (reader, mut writer) = stream.split();
                let mut reader = BufReader::new(reader);
                self.validate_registration(registration)?;
                let line = serde_json::to_string(&request)? + "\n";
                let mut response = round_trip(
                    &mut reader,
                    &mut writer,
                    &line,
                    CONTROL_TIMEOUT,
                    Some(STATUS_ACK_LIMIT),
                )
                .await?;
                registration.redact_response(&mut response);
                Ok(response)
            }
        }
    }

    /// Open an owned event stream for a wire the caller explicitly selected.
    /// Legacy performs Status first; the characterized candidate uses its actual
    /// events-subscribe ACK and does not invent a negotiation exchange.
    pub async fn subscribe_events_for_contract(
        &self,
        contract: EventContract,
        tx: mpsc::Sender<serde_json::Value>,
    ) -> Result<EventSession> {
        self.subscribe_events_inner(contract, tx, None).await
    }

    /// Legacy host-policy variant; policy and events subscribe use one socket.
    pub async fn subscribe_events_checked(
        &self,
        contract: EventContract,
        tx: mpsc::Sender<serde_json::Value>,
        guard: StatusGuard,
    ) -> Result<EventSession> {
        self.subscribe_events_inner(contract, tx, Some(guard)).await
    }

    async fn subscribe_events_inner(
        &self,
        contract: EventContract,
        tx: mpsc::Sender<serde_json::Value>,
        guard: Option<StatusGuard>,
    ) -> Result<EventSession> {
        if contract == EventContract::CandidateV1Db7818e && guard.is_some() {
            bail!("checked host policy requires the legacy wire contract");
        }
        let _renewal = self.events.subscribe.lock().await;
        let generation = self.events.begin();
        let stream = self.connect().await?;
        let (reader, mut writer) = stream.split();
        let mut reader = BufReader::new(reader);
        if contract == EventContract::LegacyV0_3_21 {
            Self::check_on_connection(&mut reader, &mut writer, contract, guard).await?;
        }
        let ack = round_trip(
            &mut reader,
            &mut writer,
            "{\"op\":\"events_subscribe\"}\n",
            CONTROL_TIMEOUT,
            Some(STATUS_ACK_LIMIT),
        )
        .await?;
        let registration = self.events.install(contract, generation, ack)?;
        let reader_registration = registration.clone();
        // Construct outside the future so even an unpolled cancelled task
        // drops its lifetime guard and invalidates the registration.
        let lifetime = registration.reader_lifetime();
        let task = tokio::spawn(async move {
            let _lifetime = lifetime;
            let _writer_keepalive = writer;
            loop {
                let line = tokio::select! {
                    biased;
                    _ = tx.closed() => break,
                    line = read_json_line(&mut reader,
                        (contract == EventContract::CandidateV1Db7818e).then_some(CANDIDATE_EVENT_LINE_LIMIT)) => line,
                };
                let bytes = match line {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        tracing::debug!(
                            target: "allmystuff_node::control_client",
                            "daemon event stream ended or exceeded its adapter limit"
                        );
                        break;
                    }
                };
                let mut value: serde_json::Value = match serde_json::from_slice(&bytes) {
                    Ok(value) => value,
                    Err(_) => {
                        tracing::warn!(
                            target: "allmystuff_node::control_client",
                            "malformed daemon event JSON (payload omitted)"
                        );
                        continue;
                    }
                };
                if contract == EventContract::CandidateV1Db7818e {
                    let Some(channel) = Self::candidate_channel_inbound(value) else {
                        continue;
                    };
                    value = channel;
                    reader_registration.redact_value(&mut value);
                }
                if tx.send(value).await.is_err() {
                    break;
                }
            }
        });
        Ok(EventSession::new(registration, task))
    }

    fn candidate_channel_inbound(value: serde_json::Value) -> Option<serde_json::Value> {
        // PR135/db7818e ipc/wire.rs ChannelInbound: these are arrival network
        // and authenticated sender evidence, not application authorization.
        if value.get("kind")?.as_str()? != "channel_inbound" {
            return None;
        }
        Some(serde_json::json!({
            "kind": "channel_inbound",
            "network": value.get("network")?.as_str()?,
            "from": value.get("from")?.as_str()?,
            "channel": value.get("channel")?.as_str()?,
            "payload": value.get("payload")?,
        }))
    }
}

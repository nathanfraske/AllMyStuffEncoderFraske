use std::sync::Arc;

use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::tokio::{RecvHalf, SendHalf};
use serde::Serialize;
use tokio::io::{AsyncWriteExt, BufReader};

use super::state::{Secret, Shared};
use super::{FlowSpec, RealtimeError, Result};
use crate::contract::verify_wire_status;
use crate::transport::{read_json_line, round_trip};
use crate::{EventContract, Response, STATUS_ACK_LIMIT};

pub(crate) struct Pipe {
    pub(crate) reader: BufReader<RecvHalf>,
    pub(crate) writer: SendHalf,
}

pub(crate) struct CallFailure {
    pub(crate) error: RealtimeError,
    /// True once writing the protected request could have begun. An Open ACK
    /// failure after this point cannot be treated as proof no flow was created.
    pub(crate) sent: bool,
}

pub(crate) fn validate_spec(spec: &FlowSpec) -> Result<()> {
    if spec.label.is_empty() || spec.label.len() > 255 {
        return Err(RealtimeError::InvalidInput(
            "flow label must occupy 1..=255 bytes",
        ));
    }
    for field in [&spec.network, &spec.peer, &spec.encoding.mime] {
        if field.is_empty() || field.len() > STATUS_ACK_LIMIT {
            return Err(RealtimeError::InvalidInput(
                "flow metadata is empty or too large",
            ));
        }
    }
    Ok(())
}

fn encode(request: &impl Serialize) -> Result<String> {
    let mut line = serde_json::to_string(request)
        .map_err(|_| RealtimeError::InvalidInput("request encoding"))?;
    line.push('\n');
    if line.len() > STATUS_ACK_LIMIT {
        return Err(RealtimeError::LimitExceeded("control request bytes"));
    }
    Ok(line)
}

pub(crate) fn open_line(shared: &Shared, spec: &FlowSpec) -> Result<String> {
    #[derive(Serialize)]
    struct Open<'a> {
        op: &'static str,
        network: &'a str,
        peer: &'a str,
        flow_label: &'a str,
        client_id: crate::ClientId,
        client_capability: &'a str,
        direction: super::FlowDirection,
        rtp_kind: super::RtpKind,
        mime: &'a str,
        clock_rate: u32,
        channels: u16,
    }
    encode(&Open {
        op: "realtime_flow_open",
        network: &spec.network,
        peer: &spec.peer,
        flow_label: &spec.label,
        client_id: shared.registration.client_id(),
        client_capability: shared
            .registration
            .capability()
            .ok_or(RealtimeError::UnsupportedContract)?,
        direction: spec.direction,
        rtp_kind: spec.encoding.rtp_kind,
        mime: &spec.encoding.mime,
        clock_rate: spec.encoding.clock_rate,
        channels: spec.encoding.channels,
    })
}

pub(crate) fn close_line(shared: &Shared, capability: &Secret) -> Result<String> {
    #[derive(Serialize)]
    struct Close<'a> {
        op: &'static str,
        client_id: crate::ClientId,
        client_capability: &'a str,
        flow_capability: &'a str,
    }
    encode(&Close {
        op: "realtime_flow_close",
        client_id: shared.registration.client_id(),
        client_capability: shared
            .registration
            .capability()
            .ok_or(RealtimeError::UnsupportedContract)?,
        flow_capability: capability.text(),
    })
}

pub(crate) fn pipe_line(
    shared: &Shared,
    network: &str,
    peer: Option<&str>,
    capability: Option<&Secret>,
) -> Result<String> {
    #[derive(Serialize)]
    struct Bind<'a> {
        op: &'static str,
        direction: &'static str,
        network: &'a str,
        client_id: crate::ClientId,
        client_capability: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        peer: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        flow_capability: Option<&'a str>,
    }
    encode(&Bind {
        op: "realtime_pipe",
        direction: if capability.is_some() {
            "outbound"
        } else {
            "inbound"
        },
        network,
        client_id: shared.registration.client_id(),
        client_capability: shared
            .registration
            .capability()
            .ok_or(RealtimeError::UnsupportedContract)?,
        peer,
        flow_capability: capability.map(Secret::text),
    })
}

pub(crate) async fn connect_verified(shared: &Shared) -> Result<Pipe> {
    let stream = shared
        .client
        .connect()
        .await
        .map_err(|_| RealtimeError::Transport)?;
    let (reader, mut writer) = stream.split();
    let mut reader = BufReader::new(reader);
    let response = round_trip(
        &mut reader,
        &mut writer,
        "{\"op\":\"status\"}\n",
        shared.policy.operation_timeout,
        Some(STATUS_ACK_LIMIT),
    )
    .await
    .map_err(|_| RealtimeError::Transport)?;
    verify_wire_status(&response, EventContract::CandidateV1Db7818e)
        .map_err(|_| RealtimeError::UnsupportedContract)?;
    Ok(Pipe { reader, writer })
}

pub(crate) async fn exchange(
    shared: &Shared,
    line: &str,
    opening: bool,
) -> std::result::Result<Response, CallFailure> {
    let mut sent = false;
    let result = tokio::time::timeout(shared.policy.operation_timeout, async {
        let mut pipe = connect_verified(shared).await?;
        // Recheck ownership after Status but before a new protected open. Exact
        // cleanup uses its old capabilities deliberately and is handled below.
        if opening {
            shared.verify()?;
        }
        sent = true;
        write_read(&mut pipe, line).await
    })
    .await;
    match result {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(error)) => Err(CallFailure { error, sent }),
        Err(_) => Err(CallFailure {
            error: RealtimeError::Timeout,
            sent,
        }),
    }
}

async fn write_read(pipe: &mut Pipe, line: &str) -> Result<Response> {
    pipe.writer
        .write_all(line.as_bytes())
        .await
        .map_err(|_| RealtimeError::Transport)?;
    pipe.writer
        .flush()
        .await
        .map_err(|_| RealtimeError::Transport)?;
    let bytes = read_json_line(&mut pipe.reader, Some(STATUS_ACK_LIMIT))
        .await
        .map_err(|_| RealtimeError::InvalidResponse("control acknowledgment"))?;
    if bytes.last() != Some(&b'\n') {
        return Err(RealtimeError::InvalidResponse(
            "unterminated acknowledgment",
        ));
    }
    serde_json::from_slice(&bytes).map_err(|_| RealtimeError::InvalidResponse("control JSON"))
}

pub(crate) async fn bind_pipe(
    shared: &Arc<Shared>,
    network: &str,
    peer: Option<&str>,
    capability: Option<&Secret>,
) -> Result<Pipe> {
    let line = pipe_line(shared, network, peer, capability)?;
    tokio::time::timeout(shared.policy.operation_timeout, async {
        let mut pipe = connect_verified(shared).await?;
        shared.verify()?;
        let response = write_read(&mut pipe, &line).await?;
        if !response.ok || response.error.is_some() {
            return Err(refusal(shared, &response, capability));
        }
        if response
            .data
            .as_ref()
            .and_then(|data| data.get("realtime_pipe"))
            .and_then(|value| value.as_bool())
            != Some(true)
        {
            return Err(RealtimeError::InvalidResponse("pipe acknowledgment"));
        }
        shared.verify()?;
        Ok(pipe)
    })
    .await
    .map_err(|_| RealtimeError::Timeout)?
}

pub(crate) fn refusal(
    shared: &Shared,
    response: &Response,
    capability: Option<&Secret>,
) -> RealtimeError {
    // Never reflect daemon error text. Preserve a bounded refusal code after
    // removing both known secrets, including when the code itself echoes one.
    let code = response
        .data
        .as_ref()
        .and_then(|data| data.get("code"))
        .and_then(|value| value.as_str())
        .map(|code| {
            let mut text = code.to_owned();
            if let Some(secret) = shared.registration.capability() {
                text = text.replace(secret, "[redacted]");
            }
            if let Some(secret) = capability {
                text = text.replace(secret.text(), "[redacted]");
            }
            for flow in shared.flows.lock().values() {
                if let Some(secret) = flow.record.lock().capability.as_ref() {
                    text = text.replace(secret.text(), "[redacted]");
                }
            }
            text.chars().take(64).collect()
        });
    RealtimeError::Refused { code }
}

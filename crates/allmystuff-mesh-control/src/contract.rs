//! Pure Status evidence for explicitly selected control wires.
//!
//! Parsing does not select a wire, authenticate a device or authorize an
//! application. Missing media evidence is retained as missing; it is never
//! guessed from a version number or converted into a fallback decision.

use crate::{EventContract, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeEncoding {
    pub kind: String,
    pub mime: String,
    pub clock_rate: u32,
    pub channels: u16,
}

/// The candidate's actual advert, without any capacity or application promise.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeAdvert {
    pub supported: bool,
    pub encodings: Vec<RealtimeEncoding>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusEvidence {
    /// Exact reported text, including unknown versions and suffixes.
    pub version: String,
    pub device_id: String,
    pub joined_networks: Vec<String>,
    pub media_pipes: Option<bool>,
    pub realtime: Option<RealtimeAdvert>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusError {
    StatusRejected,
    MissingStatusData,
    MalformedStatus(&'static str),
    MissingVersion,
    UnsupportedWireVersion {
        expected: EventContract,
        reported: String,
    },
}

impl std::fmt::Display for StatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StatusRejected => write!(f, "MyOwnMesh rejected Status"),
            Self::MissingStatusData => write!(f, "MyOwnMesh Status has no data"),
            Self::MalformedStatus(field) => write!(f, "MyOwnMesh Status has an invalid {field}"),
            Self::MissingVersion => write!(f, "MyOwnMesh Status has no version"),
            Self::UnsupportedWireVersion { expected, reported } => {
                let reported: String = reported.chars().take(64).collect();
                write!(
                    f,
                    "MyOwnMesh version {reported:?} does not match selected {expected:?} wire"
                )
            }
        }
    }
}

impl std::error::Error for StatusError {}

fn data_object(response: &Response) -> Result<&Map<String, Value>, StatusError> {
    if !response.ok {
        return Err(StatusError::StatusRejected);
    }
    if response.error.is_some() {
        return Err(StatusError::MalformedStatus("response envelope"));
    }
    response
        .data
        .as_ref()
        .ok_or(StatusError::MissingStatusData)?
        .as_object()
        .ok_or(StatusError::MalformedStatus("data object"))
}

fn version(data: &Map<String, Value>) -> Result<&str, StatusError> {
    data.get("version")
        .ok_or(StatusError::MissingVersion)?
        .as_str()
        .ok_or(StatusError::MalformedStatus("version field"))
}

/// Read the actual envelope/common fields and any present media advert. Unknown
/// and blank version strings remain evidence; this function does not grant
/// compatibility. Call [`verify_wire_status`] for an explicit wire match.
pub fn parse_status_evidence(response: &Response) -> Result<StatusEvidence, StatusError> {
    let data = data_object(response)?;
    let version = version(data)?.to_owned();
    let device_id = data
        .get("device_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or(StatusError::MalformedStatus("device_id field"))?
        .to_owned();
    let joined_networks = data
        .get("joined_networks")
        .and_then(Value::as_array)
        .ok_or(StatusError::MalformedStatus("joined_networks field"))?
        .iter()
        .map(|network| {
            network
                .as_str()
                .map(str::to_owned)
                .ok_or(StatusError::MalformedStatus("joined_networks field"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let media_pipes = data
        .get("media_pipes")
        .map(|value| {
            value
                .as_bool()
                .ok_or(StatusError::MalformedStatus("media_pipes field"))
        })
        .transpose()?;
    let realtime = data
        .get("realtime")
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|_| StatusError::MalformedStatus("realtime field"))
        })
        .transpose()?;
    Ok(StatusEvidence {
        version,
        device_id,
        joined_networks,
        media_pipes,
        realtime,
    })
}

/// Verify evidence against a wire the caller explicitly chose. This supports
/// the released legacy wire and the source-characterized PR135/db7818e wire;
/// it is not an application's readiness, migration or deployment policy.
pub fn verify_wire_status(
    response: &Response,
    contract: EventContract,
) -> Result<StatusEvidence, StatusError> {
    let data = data_object(response)?;
    let reported = version(data)?;
    let expected = match contract {
        EventContract::LegacyV0_3_21 => "0.3.21",
        EventContract::CandidateV1Db7818e => "1.0.0",
    };
    let normalized = reported.trim();
    if normalized.strip_prefix('v').unwrap_or(normalized) != expected {
        return Err(StatusError::UnsupportedWireVersion {
            expected: contract,
            reported: reported.to_owned(),
        });
    }
    let evidence = parse_status_evidence(response)?;
    match contract {
        EventContract::LegacyV0_3_21 if evidence.media_pipes.is_none() => {
            Err(StatusError::MalformedStatus("media_pipes field"))
        }
        EventContract::CandidateV1Db7818e if evidence.realtime.is_none() => {
            Err(StatusError::MalformedStatus("realtime field"))
        }
        _ => Ok(evidence),
    }
}

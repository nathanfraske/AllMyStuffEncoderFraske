//! Pure policy for the daemon contract this AllMyStuff build can use.
//!
//! Versions and Status fields are protocol evidence, not authentication or
//! proof of a particular binary's Git revision. The staged v1 protocol is
//! characterized separately; it cannot activate the shipping application's
//! legacy media, governance or consent operations. A daemon client capability
//! does not enroll an approved application or grant access to a user's ledger.

use allmystuff_protocol::Response;

/// The shipping daemon protocol. Advancing the pin alone does not extend it.
pub const SUPPORTED_DAEMON_VERSION: &str = "0.3.21";
/// Source reviewed for the staged protocol, not an identity supplied by Status.
pub const REVIEWED_CANDIDATE_REV: &str = "db7818e09fedd98899490347b86ac9bc9f97b59b";

/// Capabilities within the explicitly supported legacy contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LegacyStatus {
    /// False selects the legacy JSON media path within this supported contract.
    pub media_pipes: bool,
}

/// Application protocol readiness, distinct from endpoint presence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContractReadiness {
    ReadyLegacy(LegacyStatus),
    Refused(ContractRefusal),
}

impl ContractReadiness {
    pub fn require_legacy(self) -> Result<LegacyStatus, ContractRefusal> {
        match self {
            Self::ReadyLegacy(status) => Ok(status),
            Self::Refused(reason) => Err(reason),
        }
    }
}

/// Only confirmed endpoint absence permits a daemon launch. A failed or
/// incompatible occupied endpoint is never evidence that its owner disappeared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndpointProbe {
    Absent,
    Answered(ContractReadiness),
    Unavailable(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContractRefusal {
    StatusRejected(Option<String>),
    MissingStatusData,
    MalformedStatus(&'static str),
    MissingVersion,
    MalformedVersion(String),
    UnsupportedVersion(String),
    /// The reported v1 version is recognized; its product integration is not
    /// complete. This does not prove the answering binary is the reviewed SHA.
    CandidateNotMigrated,
}

impl std::fmt::Display for ContractRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Keep the server error available to typed callers, but do not
            // echo arbitrary response contents into lifecycle diagnostics.
            Self::StatusRejected(_) => write!(f, "MyOwnMesh rejected Status"),
            Self::MissingStatusData => write!(f, "MyOwnMesh Status has no data"),
            Self::MalformedStatus(field) => {
                write!(f, "MyOwnMesh Status has an invalid {field}")
            }
            Self::MissingVersion => write!(f, "MyOwnMesh Status has no version"),
            Self::MalformedVersion(version) => {
                let version: String = version.chars().take(64).collect();
                write!(f, "MyOwnMesh reported an unsupported version format {version:?}")
            }
            Self::UnsupportedVersion(version) => {
                let version: String = version.chars().take(64).collect();
                write!(
                    f,
                    "MyOwnMesh version {version:?} is unsupported; this build supports v{SUPPORTED_DAEMON_VERSION}"
                )
            }
            Self::CandidateNotMigrated => write!(
                f,
                "MyOwnMesh v1.0.0 requires the AllMyStuff session, media, authority and state migration; product cutover is not enabled"
            ),
        }
    }
}

impl std::error::Error for ContractRefusal {}

/// Preserve the exact version token from the real CLI format. Other command
/// output is not version evidence; later lines are not a second negotiation.
pub fn parse_binary_version_output(output: &str) -> Option<&str> {
    let mut fields = output.lines().next()?.split_whitespace();
    if fields.next()? != "myownmesh" {
        return None;
    }
    let version = fields.next()?;
    fields.next().is_none().then_some(version)
}

/// Explicit version policy for a binary before `serve` is launched. Accept the
/// tag spelling as well as the version emitted by the released binary. Do not
/// erase prerelease/build suffixes or infer compatibility from numeric order.
pub fn assess_binary_version(version: &str) -> Result<(), ContractRefusal> {
    let version = version.trim();
    let bare = version.strip_prefix('v').unwrap_or(version);
    match bare {
        SUPPORTED_DAEMON_VERSION => Ok(()),
        "1.0.0" => Err(ContractRefusal::CandidateNotMigrated),
        _ => {
            let fields: Vec<_> = bare.split('.').collect();
            let canonical_numeric = fields.len() == 3
                && fields.iter().all(|field| {
                    !field.is_empty()
                        && field.bytes().all(|byte| byte.is_ascii_digit())
                        && (field.len() == 1 || !field.starts_with('0'))
                        && field.parse::<u64>().is_ok()
                });
            if canonical_numeric {
                Err(ContractRefusal::UnsupportedVersion(version.to_owned()))
            } else {
                Err(ContractRefusal::MalformedVersion(version.to_owned()))
            }
        }
    }
}

/// Assess the real Status envelope without performing I/O or mutating state.
/// The released v0.3.21 Status has version/device/network fields and a boolean
/// `media_pipes` field. Checking their shape does not authenticate the device.
/// The staged v1 Status uses `realtime: { supported, encodings }` instead; its
/// version is refused even if a peer adds legacy-looking media fields.
pub fn assess_status(response: &Response) -> ContractReadiness {
    let refused = |reason| ContractReadiness::Refused(reason);
    if !response.ok {
        return refused(ContractRefusal::StatusRejected(response.error.clone()));
    }
    if response.error.is_some() {
        return refused(ContractRefusal::MalformedStatus("response envelope"));
    }
    let Some(data) = response.data.as_ref() else {
        return refused(ContractRefusal::MissingStatusData);
    };
    let Some(data) = data.as_object() else {
        return refused(ContractRefusal::MalformedStatus("data object"));
    };
    let Some(version) = data.get("version") else {
        return refused(ContractRefusal::MissingVersion);
    };
    let Some(version) = version.as_str() else {
        return refused(ContractRefusal::MalformedStatus("version field"));
    };
    if let Err(reason) = assess_binary_version(version) {
        return refused(reason);
    }
    if !data
        .get("device_id")
        .and_then(|value| value.as_str())
        .is_some_and(|id| !id.is_empty())
    {
        return refused(ContractRefusal::MalformedStatus("device_id field"));
    }
    if !data
        .get("joined_networks")
        .and_then(|value| value.as_array())
        .is_some_and(|networks| networks.iter().all(|network| network.is_string()))
    {
        return refused(ContractRefusal::MalformedStatus("joined_networks field"));
    }
    let Some(media_pipes) = data.get("media_pipes").and_then(|value| value.as_bool()) else {
        return refused(ContractRefusal::MalformedStatus("media_pipes field"));
    };
    ContractReadiness::ReadyLegacy(LegacyStatus { media_pipes })
}

#[cfg(test)]
#[path = "mesh_contract_tests.rs"]
mod tests;

//! Keep the two established public error contracts at the client boundary.

use std::io;

pub(crate) enum Error {
    #[cfg(unix)]
    ResolveHome,
    SocketName(io::Error),
    Connect(io::Error),
    Serialize(serde_json::Error),
    WriteRequest(io::Error),
    ReadResponse(io::Error),
    ResponseClosed,
    JsonTag(u8),
    ParseResponse(serde_json::Error),
    Remote(String),
    BytesTag(u8),
    WriteSubscribe(io::Error),
    ReadAck(io::Error),
    AckClosed,
    AckTag,
    ParseAck(serde_json::Error),
    AckRejected(String),
}

impl Error {
    pub(crate) fn into_node(self) -> anyhow::Error {
        match self {
            #[cfg(unix)]
            Self::ResolveHome => {
                anyhow::anyhow!("resolve the ~/.myownmesh home for the node socket")
            }
            Self::SocketName(e) => anyhow::Error::new(e).context(crate::address::NAME_CONTEXT),
            Self::Connect(e) => anyhow::Error::new(e)
                .context("connect node socket — is `allmystuff-serve` running?"),
            Self::Serialize(e) => e.into(),
            Self::WriteRequest(e) => anyhow::Error::new(e).context("write node request"),
            Self::ReadResponse(e) => anyhow::Error::new(e).context("read node response"),
            Self::ResponseClosed => anyhow::anyhow!("node closed the connection without a response"),
            Self::JsonTag(tag) => {
                anyhow::anyhow!("node sent a {tag} frame where a JSON response was expected")
            }
            Self::ParseResponse(e) => anyhow::Error::new(e).context("parse node response"),
            Self::Remote(e) => anyhow::anyhow!(e),
            Self::BytesTag(tag) => {
                anyhow::anyhow!("node sent a {tag} frame where raw bytes were expected")
            }
            Self::WriteSubscribe(e) => anyhow::Error::new(e).context("write node subscribe"),
            Self::ReadAck(e) => anyhow::Error::new(e).context("read subscribe ack"),
            Self::AckClosed => {
                anyhow::anyhow!("node closed the connection before the subscribe ack")
            }
            Self::AckTag => anyhow::anyhow!("subscribe ack wasn't a JSON frame"),
            Self::ParseAck(e) => anyhow::Error::new(e).context("parse subscribe ack"),
            Self::AckRejected(e) => anyhow::anyhow!("subscribe rejected: {e}"),
        }
    }

    pub(crate) fn into_terminal(self) -> String {
        match self {
            #[cfg(unix)]
            Self::ResolveHome => {
                "couldn't resolve the ~/.myownmesh home for the node socket".to_string()
            }
            Self::SocketName(e) => format!("{}: {e}", crate::address::NAME_CONTEXT),
            Self::Connect(e) => format!("connect node socket: {e}"),
            Self::Serialize(e) => e.to_string(),
            Self::WriteRequest(e) => format!("write node request: {e}"),
            Self::ReadResponse(e) => format!("read node response: {e}"),
            Self::ResponseClosed => "node closed the connection without a response".to_string(),
            Self::JsonTag(tag) => format!("node sent a {tag} frame where JSON was expected"),
            Self::ParseResponse(e) => format!("parse node response: {e}"),
            Self::Remote(e) => e,
            Self::BytesTag(tag) => format!("node sent a {tag} frame where bytes were expected"),
            Self::WriteSubscribe(e) => format!("write node subscribe: {e}"),
            Self::ReadAck(e) => format!("read subscribe ack: {e}"),
            Self::AckClosed => "node closed the connection before the subscribe ack".to_string(),
            Self::AckTag => "subscribe ack wasn't a JSON frame".to_string(),
            Self::ParseAck(e) => format!("parse subscribe ack: {e}"),
            Self::AckRejected(e) => format!("subscribe rejected: {e}"),
        }
    }
}

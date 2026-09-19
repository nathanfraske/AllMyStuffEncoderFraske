use interprocess::local_socket::tokio::prelude::*;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::address::{self, SocketAddr};
use crate::error::Error;
use crate::{
    read_frame, write_frame, NodeEvent, NodeRequest, WireResponse, SUBSCRIBE_EVENTS, TAG_BYTES,
    TAG_EVENT, TAG_JSON, TAG_RESTART,
};

#[derive(Clone, Copy)]
pub(crate) enum EventPolicy {
    Node,
    Terminal,
}

pub(crate) struct Client {
    addr: SocketAddr,
}

impl Client {
    pub(crate) fn new() -> Result<Self, Error> {
        Ok(Self {
            addr: address::resolve()?,
        })
    }

    #[cfg(test)]
    pub(crate) fn at_address(addr: SocketAddr) -> Self {
        Self { addr }
    }

    pub(crate) async fn connect(&self) -> Result<LocalSocketStream, Error> {
        let name = self.addr.raw_name().map_err(Error::SocketName)?;
        LocalSocketStream::connect(name)
            .await
            .map_err(Error::Connect)
    }

    pub(crate) async fn probe() -> bool {
        let Ok(client) = Self::new() else {
            return false;
        };
        client.connect().await.is_ok()
    }

    async fn round_trip(&self, cmd: &str, args: Value) -> Result<(u8, Vec<u8>), Error> {
        let stream = self.connect().await?;
        let (mut reader, mut writer) = stream.split();
        let body = serde_json::to_vec(&NodeRequest {
            cmd: cmd.to_string(),
            args,
        })
        .map_err(Error::Serialize)?;
        write_frame(&mut writer, TAG_JSON, &body)
            .await
            .map_err(Error::WriteRequest)?;
        read_frame(&mut reader)
            .await
            .map_err(Error::ReadResponse)?
            .ok_or(Error::ResponseClosed)
    }

    pub(crate) async fn request(&self, cmd: &str, args: Value) -> Result<Value, Error> {
        let (tag, payload) = self.round_trip(cmd, args).await?;
        if tag != TAG_JSON {
            return Err(Error::JsonTag(tag));
        }
        let resp: WireResponse = serde_json::from_slice(&payload).map_err(Error::ParseResponse)?;
        if resp.ok {
            Ok(resp.result)
        } else {
            Err(Error::Remote(
                resp.error.unwrap_or_else(|| "(no error)".into()),
            ))
        }
    }

    pub(crate) async fn request_bytes(&self, cmd: &str, args: Value) -> Result<Vec<u8>, Error> {
        let (tag, payload) = self.round_trip(cmd, args).await?;
        match tag {
            TAG_BYTES => Ok(payload),
            TAG_JSON => {
                let resp: WireResponse =
                    serde_json::from_slice(&payload).map_err(Error::ParseResponse)?;
                Err(Error::Remote(resp.error.unwrap_or_else(|| {
                    "node returned JSON where bytes were expected".into()
                })))
            }
            other => Err(Error::BytesTag(other)),
        }
    }

    pub(crate) async fn subscribe_events(
        &self,
        tx: mpsc::Sender<NodeEvent>,
        policy: EventPolicy,
    ) -> Result<(), Error> {
        let stream = self.connect().await?;
        let (mut reader, mut writer) = stream.split();
        let body = serde_json::to_vec(&NodeRequest {
            cmd: SUBSCRIBE_EVENTS.to_string(),
            args: Value::Null,
        })
        .map_err(Error::Serialize)?;
        write_frame(&mut writer, TAG_JSON, &body)
            .await
            .map_err(Error::WriteSubscribe)?;

        let (tag, payload) = read_frame(&mut reader)
            .await
            .map_err(Error::ReadAck)?
            .ok_or(Error::AckClosed)?;
        if tag != TAG_JSON {
            return Err(Error::AckTag);
        }
        let ack: WireResponse = serde_json::from_slice(&payload).map_err(Error::ParseAck)?;
        if !ack.ok {
            return Err(Error::AckRejected(
                ack.error.unwrap_or_else(|| "(no error)".into()),
            ));
        }

        tokio::spawn(async move {
            // Keep the writer half alive for the read loop's lifetime.
            let _writer_keepalive = writer;
            loop {
                match read_frame(&mut reader).await {
                    Ok(Some((TAG_EVENT, body))) => {
                        match serde_json::from_slice::<NodeEvent>(&body) {
                            Ok(ev) => {
                                if tx.send(ev).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                if matches!(policy, EventPolicy::Node) {
                                    tracing::warn!(target: "allmystuff_node::node_control", "malformed node event: {e}");
                                }
                            }
                        }
                    }
                    Ok(Some((TAG_RESTART, _))) => {
                        let _ = tx.send(NodeEvent::Restart).await;
                        break;
                    }
                    Ok(Some((tag, _))) => {
                        if matches!(policy, EventPolicy::Node) {
                            tracing::warn!(target: "allmystuff_node::node_control", "unexpected node event frame tag {tag}");
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        if matches!(policy, EventPolicy::Node) {
                            tracing::warn!(target: "allmystuff_node::node_control", "node event stream read failed: {e}");
                        }
                        break;
                    }
                }
            }
        });
        Ok(())
    }
}

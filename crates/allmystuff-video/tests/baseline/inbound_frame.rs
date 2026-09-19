/// One decoded inbound media frame (from a peer, arriving at this client).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundFrame {
    pub kind: u8,
    /// Video keyframe flag (always false for audio).
    pub key: bool,
    pub stream: u8,
    pub rtp_timestamp: u32,
    /// Sending peer (display id, as the old `from` field carried).
    pub from: String,
    pub data: Vec<u8>,
}

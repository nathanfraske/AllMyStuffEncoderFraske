//! Compatibility adapter for the node's video IPC packet envelope.

pub(crate) use allmystuff_video::handoff::Enqueue;
pub(crate) type VideoHandoff = allmystuff_video::handoff::VideoHandoff<NodePacketPolicy>;

pub(crate) struct NodePacketPolicy;

impl allmystuff_video::handoff::HandoffPolicy for NodePacketPolicy {
    const PREFIX_BYTES: usize = 4;

    fn key(data: &[u8]) -> bool {
        data.first() == Some(&2) && data.get(1) == Some(&1)
    }

    fn append_to_batch(out: &mut Vec<u8>, data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
    }
}

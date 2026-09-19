//! Compatibility path preserving the node's single-allocation RGBA IPC output.

pub use allmystuff_video::codec::Au;
pub(crate) use allmystuff_video::codec::{is_decode_entry, sniff_codec, AuCodec};
pub use allmystuff_video::video_decode::DecoderPreference;

pub type DecodeBridge = allmystuff_video::video_decode::DecodeBridge<NodeFrameOutput>;

/// Node-local IPC layout; decoding writes directly into the final payload.
pub struct NodeFrameOutput;

impl allmystuff_video::output::DecodeOutput for NodeFrameOutput {
    type Frame = Vec<u8>;

    fn allocate(ts_us: u64, w: u32, h: u32) -> Vec<u8> {
        let len = (w as usize) * (h as usize) * 4;
        let mut out = crate::mesh::video_ipc_header(3, 0, [w, h, 0, 0], ts_us, len);
        out.resize(crate::mesh::VIDEO_IPC_HEADER_LEN + len, 0);
        out
    }

    fn rgba_mut(packet: &mut Vec<u8>) -> &mut [u8] {
        &mut packet[crate::mesh::VIDEO_IPC_HEADER_LEN..]
    }
}

//! Test-only application envelope adapters for the preserved private tests.
//! Nothing here is a production output policy or a real desktop follower.

#[cfg(feature = "decode")]
mod ipc {
    include!("../baseline/ipc_header.rs");
}

#[cfg(feature = "decode")]
pub(crate) use ipc::VIDEO_IPC_HEADER_LEN;

#[cfg(feature = "decode")]
pub(crate) struct LegacyOutput;

#[cfg(feature = "decode")]
impl crate::decode::DecodeOutput for LegacyOutput {
    type Frame = Vec<u8>;

    fn allocate(ts_us: u64, w: u32, h: u32) -> Self::Frame {
        // Exact node raw_ipc_packet arithmetic, allocation and resize order.
        let len = (w as usize) * (h as usize) * 4;
        let mut out = ipc::video_ipc_header(3, 0, [w, h, 0, 0], ts_us, len);
        out.resize(VIDEO_IPC_HEADER_LEN + len, 0);
        out
    }

    fn rgba_mut(frame: &mut Self::Frame) -> &mut [u8] {
        &mut frame[VIDEO_IPC_HEADER_LEN..]
    }
}

pub(crate) struct LegacyHandoffPolicy;

impl crate::handoff::HandoffPolicy for LegacyHandoffPolicy {
    const PREFIX_BYTES: usize = 4;

    fn key(data: &[u8]) -> bool {
        data.first() == Some(&2) && data.get(1) == Some(&1)
    }

    fn append_to_batch(out: &mut Vec<u8>, data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
    }
}

#[cfg(feature = "host")]
pub(crate) struct TestDesktopFollower;

#[cfg(feature = "host")]
impl crate::host::DesktopFollower for TestDesktopFollower {
    fn new() -> Self {
        Self
    }

    fn follow(&mut self) -> bool {
        false
    }

    fn desktop_name(&self) -> &str {
        "isolated-test-desktop"
    }

    fn on_secure_desktop(&self) -> bool {
        false
    }
}

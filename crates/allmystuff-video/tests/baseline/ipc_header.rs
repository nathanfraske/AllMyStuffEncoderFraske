pub(crate) const VIDEO_IPC_HEADER_LEN: usize = 28;

pub(crate) fn video_ipc_header(
    kind: u8,
    flags: u8,
    dims: [u32; 4],
    tail: u64,
    payload_len: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(VIDEO_IPC_HEADER_LEN + payload_len);
    out.push(kind);
    out.push(flags);
    out.extend_from_slice(&[0u8; 2]);
    for d in dims {
        out.extend_from_slice(&d.to_le_bytes());
    }
    out.extend_from_slice(&tail.to_le_bytes());
    out
}

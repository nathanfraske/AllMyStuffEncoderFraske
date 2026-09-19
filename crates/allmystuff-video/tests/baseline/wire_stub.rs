const PACED_AU_MARKER_UUID: &[u8; 16] = b"AMS-PACED-AU-V1!";

pub(crate) fn paced_au_marker(chunks: usize) -> Vec<u8> {
    let chunks = u16::try_from(chunks).unwrap_or(u16::MAX);
    let mut out = Vec::with_capacity(26);
    out.extend_from_slice(&[0, 0, 0, 1, 0x06, 0x05, 18]);
    out.extend_from_slice(PACED_AU_MARKER_UUID);
    out.extend_from_slice(&chunks.to_le_bytes());
    out.push(0x80);
    out
}

pub(crate) fn paced_au_marker_count(data: &[u8]) -> Option<usize> {
    if data.len() != 26
        || data[..7] != [0, 0, 0, 1, 0x06, 0x05, 18]
        || &data[7..23] != PACED_AU_MARKER_UUID
        || data[25] != 0x80
    {
        return None;
    }
    Some(u16::from_le_bytes([data[23], data[24]]) as usize)
}

/// Mirror of the real module's paced splitter — pure byte logic, kept
/// identical (both codecs, glue rules, byte-exact concatenation).
pub(crate) fn split_annexb_paced(data: &[u8], max_chunk: usize) -> Vec<std::ops::Range<usize>> {
    let mut nals: Vec<(usize, u8)> = Vec::new();
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 {
            let hdr = if data[i + 2] == 1 {
                i + 3
            } else if i + 4 <= data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                i + 4
            } else {
                i += 1;
                continue;
            };
            if hdr < data.len() {
                nals.push((i, data[hdr]));
            }
            i = hdr + 1;
        } else {
            i += 1;
        }
    }
    // Exact parameter-set bytes — see the real module's comment: a masked
    // type test collides with H.264's 0x41 referenced P slice.
    let hevc = nals.iter().any(|&(_, b)| matches!(b, 0x40 | 0x42 | 0x44));
    let is_slice = |b: u8| {
        if hevc {
            b & 0x80 == 0 && ((b >> 1) & 0x3F) <= 21
        } else {
            matches!(b & 0x1F, 1 | 5)
        }
    };
    let mut unit_starts: Vec<usize> = Vec::new();
    let mut pending: Option<usize> = None;
    for &(off, b) in &nals {
        if is_slice(b) {
            unit_starts.push(pending.take().unwrap_or(off));
        } else if pending.is_none() {
            pending = Some(off);
        }
    }
    if unit_starts.len() < 2 {
        return std::iter::once(0..data.len()).collect();
    }
    unit_starts[0] = 0;
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    let mut chunk_start = 0usize;
    let mut chunk_end = 0usize;
    for (k, &s) in unit_starts.iter().enumerate() {
        let e = unit_starts.get(k + 1).copied().unwrap_or(data.len());
        if chunk_end > chunk_start && e - chunk_start > max_chunk {
            out.push(chunk_start..chunk_end);
            chunk_start = s;
        }
        chunk_end = e;
    }
    out.push(chunk_start..chunk_end);
    out
}

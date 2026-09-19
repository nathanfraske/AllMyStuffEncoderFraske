const PACED_AU_MARKER_UUID: &[u8; 16] = b"AMS-PACED-AU-V1!";

/// Build the explicit end marker for one paced access unit. The two-byte
/// fragment count lets ingress distinguish a complete train from a train that
/// lost a whole media sample (RTP reassembly can detect packet holes inside a
/// sample, but not the absence of a sample it never knew existed).
pub(crate) fn paced_au_marker(chunks: usize) -> Vec<u8> {
    let chunks = u16::try_from(chunks).unwrap_or(u16::MAX);
    let mut out = Vec::with_capacity(26);
    out.extend_from_slice(&[0, 0, 0, 1, 0x06]); // SEI NAL
    out.extend_from_slice(&[0x05, 18]); // user_data_unregistered, 16-byte UUID + u16 count
    out.extend_from_slice(PACED_AU_MARKER_UUID);
    out.extend_from_slice(&chunks.to_le_bytes());
    out.push(0x80); // rbsp_trailing_bits
    out
}

/// Parse a v1 paced-access-unit closing marker. Exact shape matching avoids
/// mistaking an encoder's ordinary user-data SEI for transport framing.
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

/// Split one Annex-B access unit into paced chunks of at most `max_chunk`
/// bytes, cutting **only** at slice-NAL boundaries and gluing any
/// parameter-set/SEI run to the slice that follows it — a chunk never
/// strands a slice from its headers. The ranges concatenate back to one
/// complete access unit; they are not generally independent pictures
/// (WebCodecs/NVDEC require receiver-side AU completion). Speaks both codecs:
/// an AU carrying HEVC parameter sets
/// (VPS/SPS/PPS — byte values no H.264 key unit leads with) cuts at HEVC
/// VCL NALs; anything else cuts at H.264 slice types 1/5, which for a
/// paramless HEVC delta AU means "no cut" — correct, just unpaced, and
/// the keyframe walls the pacer exists for always carry their parameter
/// sets. Returns contiguous ranges that concatenate back to `data`
/// byte-identically; a unit whose single slice exceeds the cap stays
/// whole (slice granularity is the floor — the encoder-side slice count
/// is what makes real splits exist).
///
/// **AV1 (future):** AV1 has no Annex-B start codes — it's OBUs with
/// leb128 sizes — so this walk finds no cut points and returns the whole
/// AU as one chunk. That is the correct SAFE fallback (AV1 rides unpaced,
/// exactly like a paramless HEVC delta), so nothing breaks when AV1
/// lands; but the pacer's burst-shaping is then off for AV1. The AV1-
/// aware seam is here: an `obu_split` branch that cuts at tile-group /
/// frame OBU boundaries (the AV1 analog of slice NALs) — walk OBU
/// headers via leb128 like `sniff_av1_obu` in `video_decode.rs`, group
/// to `max_chunk`.
pub(crate) fn split_annexb_paced(data: &[u8], max_chunk: usize) -> Vec<std::ops::Range<usize>> {
    // Walk the start codes (00 00 01 and 00 00 00 01), recording each
    // NAL's offset and header byte.
    // SIMD-anchored: memchr sweeps for 0x01 at cache speed and the
    // look-behind confirms the 00 00 prefix — the pacer runs this over
    // every AU (a lossless IDR is ~1.4 MB), and the old byte-stepping
    // loop paid a branch per byte. Equivalent to the forward scan: a
    // start code's own bytes can never satisfy another match's [0,0]
    // look-behind, and a run of ≥3 zeros anchors at p−3 exactly where
    // the forward scan entered its 4-byte arm.
    let nals = crate::video_wire::annexb_nals(data);
    // Exact parameter-set bytes (0x40/0x42/0x44 = VPS/SPS/PPS, layer 0)
    // — a masked type test would also match 0x41, the H.264 referenced
    // P-slice byte, and misread whole H.264 streams as HEVC.
    let hevc = nals.iter().any(|&(_, b)| matches!(b, 0x40 | 0x42 | 0x44));
    let is_slice = |b: u8| {
        if hevc {
            b & 0x80 == 0 && ((b >> 1) & 0x3F) <= 21 // any VCL NAL
        } else {
            matches!(b & 0x1F, 1 | 5)
        }
    };
    // One decodable unit per slice NAL, absorbing the non-slice run
    // before it; anything before the first slice belongs to the first
    // unit, anything after the last slice's data to the last.
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
    // Greedy pack: extend the current chunk unit by unit; cut when the
    // next extension would overflow the cap (an oversized single unit
    // still ships whole).
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

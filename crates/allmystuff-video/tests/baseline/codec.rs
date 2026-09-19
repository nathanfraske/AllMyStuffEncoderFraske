/// Which codec an access unit opens with. H.264/HEVC are judged from an
/// identifying NAL in a self-describing unit (the decode entries both encoders
/// emit with repeated VPS/SPS/PPS); harmless AUD/SEI prefixes are skipped.
/// AV1 has no Annex-B start codes — it's judged from a leading sequence-header
/// OBU instead ([`sniff_codec`]). Delta units return `None`: the stream's codec
/// is a property carried key-to-key, not re-judged per frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AuCodec {
    H264,
    Hevc,
    /// AV1 (OBU bitstream). **Decode is a STUB** — see [`Av1Rung`]: the
    /// sniff and dispatch seams exist so implementing AV1 is filling the
    /// rung bodies, not hunting for the branch points. No encoder emits
    /// AV1 yet, so this arm is dormant scaffolding.
    Av1,
}

pub(crate) fn sniff_codec(data: &[u8]) -> Option<AuCodec> {
    let nals = crate::video_wire::annexb_nals(data);
    if nals.is_empty() {
        // No start code anywhere — not H.264/HEVC. AV1 access units carry no
        // start codes, so only this case is allowed into the OBU check.
        return sniff_av1_obu(data);
    }
    // Exact bytes, not masked types: HEVC VPS/SPS/PPS at layer 0 are
    // precisely 0x40/0x42/0x44. A masked `(b>>1)&0x3F == 32` test would
    // also catch 0x41 — an H.264 P slice with nal_ref_idc 2, the byte
    // most delta AUs lead with — and flip a healthy H.264 stream's
    // decoder on every frame. (Caught in review; the byte-exact match is
    // collision-free because H.264 types 0/2/4 never lead an AU.)
    for (_, header) in nals {
        match header {
            // VPS · SPS · PPS, plus IDR_W_RADL (0x26): an H.264 SEI would
            // share that byte only with nal_ref_idc=1, which the H.264 spec
            // forbids for SEI — so a conformant 0x26 is HEVC. IDR_N_LP
            // (0x28) stays out because it collides with a legal H.264 PPS;
            // our senders repeat parameter sets on clean entries.
            0x40 | 0x42 | 0x44 | 0x26 => return Some(AuCodec::Hevc),
            _ if matches!(header & 0x1f, 5 | 7 | 8) => return Some(AuCodec::H264),
            _ => {}
        }
    }
    None
}

/// Whether this access unit is self-describing decoder entry. The media
/// lane's `key` bit is H.264-shaped, so HEVC/AV1 parameter-set-led entries
/// must be recognized from their bytes before a receiver decides to hold
/// first-frame media.
pub(crate) fn is_decode_entry(data: &[u8]) -> bool {
    sniff_codec(data).is_some()
}

/// AV1 codec detection from a start-code-less AU — the OBU-aware seam.
/// An AV1 key access unit leads with a **sequence header OBU** (our
/// encoders emit it on every key frame, the AV1 analog of repeated
/// SPS/PPS). The low-overhead OBU header first byte is
/// `forbidden(1)=0 | type(4) | extension(1) | has_size(1) |
/// reserved(1)=0`; a leading temporal-delimiter (type 2) then
/// sequence-header (type 1) is the conformant key opening. Conservative
/// on purpose: only a genuine seq-header-led opening returns `Av1`, so a
/// truncated/odd H.264 chunk that reached here (no start code found)
/// stays `None` rather than being misread. Delta AUs (no seq header)
/// return `None` — codec carries from the key, like the Annex-B path.
///
/// STUB status: correct enough to route the stream to [`Av1Rung`], which
/// then reports "not yet implemented". Full OBU parsing lives in the
/// decoder, not here — this only names the codec.
fn sniff_av1_obu(data: &[u8]) -> Option<AuCodec> {
    /// One OBU header at `data[at]`: `(obu_type, next_offset)` when the
    /// header (and its optional leb128 size) parse; `None` past the end.
    fn obu_at(data: &[u8], at: usize) -> Option<(u8, usize)> {
        let hdr = *data.get(at)?;
        if hdr & 0x80 != 0 {
            return None; // forbidden bit set — not a valid OBU
        }
        let obu_type = (hdr >> 3) & 0x0f;
        let has_ext = hdr & 0x04 != 0;
        let has_size = hdr & 0x02 != 0;
        let mut p = at + 1 + usize::from(has_ext);
        if has_size {
            // leb128 size — skip it to reach the next OBU. Accumulate in
            // u64 (not usize): the final iteration shifts by 49, which
            // overflows a 32-bit usize on riscv32/armv7 (panic in debug,
            // masked-wrong in release) — u64 is valid on every target, and
            // `checked_add` keeps the 32-bit pointer add from wrapping.
            let mut size = 0u64;
            for shift in 0..8u32 {
                let byte = *data.get(p)?;
                p += 1;
                size |= u64::from(byte & 0x7f) << (shift * 7);
                if byte & 0x80 == 0 {
                    break;
                }
            }
            p = p.checked_add(usize::try_from(size).ok()?)?;
        }
        Some((obu_type, p))
    }
    // OBU_TEMPORAL_DELIMITER = 2, OBU_SEQUENCE_HEADER = 1. A key AU opens
    // with a seq header, optionally behind a temporal delimiter.
    let (t0, next) = obu_at(data, 0)?;
    if t0 == 1 {
        return Some(AuCodec::Av1); // seq-header-led
    }
    if t0 == 2 {
        if let Some((t1, _)) = obu_at(data, next) {
            if t1 == 1 {
                return Some(AuCodec::Av1); // temporal-delimiter then seq header
            }
        }
    }
    None
}

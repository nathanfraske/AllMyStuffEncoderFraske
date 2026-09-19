/// One event of a terminal route's stream — the byte-level conversation
/// between an xterm.js viewer and the PTY a host spawned for it. Bytes are
/// opaque to the wire (the emulator and the PTY speak VT between
/// themselves); the frame just carries them, plus the two control events a
/// session needs: the viewer resizing its emulator, and the host reporting
/// the shell's end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermFrame {
    /// Tag for demuxing off the shared media channel. Always `"term"`.
    pub t: MediaTagTerm,
    pub route: String,
    pub seq: u64,
    #[serde(flatten)]
    pub event: TermEvent,
}

/// What happened on the terminal route. `Data` flows both ways (keystrokes
/// up, PTY output down); `Resize` only viewer → host; `Exit` only host →
/// viewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TermEvent {
    /// Raw PTY bytes, base64 on the wire (the daemon channel is JSON).
    Data {
        #[serde(with = "bytes_b64")]
        bytes: Vec<u8>,
    },
    /// The viewer's emulator was resized; the host resizes the PTY so the
    /// shell relays out at the right dimensions.
    Resize { cols: u16, rows: u16 },
    /// The shell ended. `None` = killed / no status to report.
    Exit {
        #[serde(default)]
        code: Option<i32>,
    },
    /// A terminal event a newer build introduced. Ignored rather than
    /// failing the whole frame.
    #[serde(other)]
    Unknown,
}

impl TermFrame {
    pub fn new(route: impl Into<String>, seq: u64, event: TermEvent) -> Self {
        TermFrame {
            t: MediaTagTerm::Term,
            route: route.into(),
            seq,
            event,
        }
    }

    /// Split `bytes` into channel-sized [`TermEvent::Data`] frames — each
    /// carries at most `max_bytes` so the full JSON message (base64 +
    /// envelope) stays under the transport's ceiling. Sequence numbers
    /// increment per piece starting at `first_seq`; an empty payload still
    /// yields one (empty) frame so a write is never silently dropped.
    pub fn data_frames(
        route: &str,
        first_seq: u64,
        bytes: &[u8],
        max_bytes: usize,
    ) -> Vec<TermFrame> {
        let max = max_bytes.max(1);
        if bytes.len() <= max {
            return vec![TermFrame::new(
                route,
                first_seq,
                TermEvent::Data {
                    bytes: bytes.to_vec(),
                },
            )];
        }
        bytes
            .chunks(max)
            .enumerate()
            .map(|(i, piece)| {
                TermFrame::new(
                    route,
                    first_seq + i as u64,
                    TermEvent::Data {
                        bytes: piece.to_vec(),
                    },
                )
            })
            .collect()
    }
}

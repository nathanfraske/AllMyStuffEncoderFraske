/// Everything that can arrive on the media channel, demuxed by the `t`
/// tag (no tag = audio, the original frame shape).
#[derive(Debug, Clone, PartialEq)]
pub enum MediaPayload {
    Audio(AudioFrame),
    Video(VideoFrame),
    VideoStatus(VideoStatusFrame),
    Input(InputEvent),
    Terminal(TermFrame),
    File(FileFrame),
    Clipboard(ClipboardFrame),
    Site(SiteFrame),
}

impl MediaPayload {
    /// Decode a media-channel payload. `None` for frames we don't
    /// understand (e.g. a newer peer's new kind) — drop, never error.
    pub fn decode(payload: serde_json::Value) -> Option<MediaPayload> {
        match payload.get("t").and_then(|t| t.as_str()) {
            Some("video") => serde_json::from_value(payload)
                .ok()
                .map(MediaPayload::Video),
            Some("vstat") => serde_json::from_value(payload)
                .ok()
                .map(MediaPayload::VideoStatus),
            Some("input") => serde_json::from_value(payload)
                .ok()
                .map(MediaPayload::Input),
            Some("term") => serde_json::from_value(payload)
                .ok()
                .map(MediaPayload::Terminal),
            Some("file") => serde_json::from_value(payload).ok().map(MediaPayload::File),
            Some("clip") => serde_json::from_value(payload)
                .ok()
                .map(MediaPayload::Clipboard),
            Some("site") => serde_json::from_value(payload).ok().map(MediaPayload::Site),
            Some(_) => None,
            None => serde_json::from_value(payload)
                .ok()
                .map(MediaPayload::Audio),
        }
    }

    /// The route id the frame belongs to, whatever its kind.
    pub fn route(&self) -> &str {
        match self {
            MediaPayload::Audio(f) => &f.route,
            MediaPayload::Video(f) => &f.route,
            MediaPayload::VideoStatus(f) => &f.route,
            MediaPayload::Input(f) => &f.route,
            MediaPayload::Terminal(f) => &f.route,
            MediaPayload::File(f) => &f.route,
            MediaPayload::Clipboard(f) => &f.route,
            MediaPayload::Site(f) => &f.route,
        }
    }
}

// Unit-variant tags so the structs serialize with a literal `"t":"…"`

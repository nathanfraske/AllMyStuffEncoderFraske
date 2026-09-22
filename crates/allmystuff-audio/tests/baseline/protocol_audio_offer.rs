/// Lifecycle of a single cross-node route. The sourcing side offers; the
/// other side accepts to start media flowing, or rejects with a reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RouteControl {
    /// "I'd like to connect this." Carries the full route so the receiver
    /// can show exactly what's being asked and check it against its own
    /// catalog before accepting.
    Offer {
        route: Route,
        /// Video transports the *offerer* can consume for a display
        /// route, best first (today: `"h264"` — the mesh's RTP track
        /// lane). The accepting side — the machine whose screen will
        /// stream — picks the best one it can produce, falling back to
        /// MJPEG over the media channel when the list is empty or
        /// nothing matches. Absent on v0.1.x offers (`default`) and
        /// ignored by v0.1.x receivers: both skews degrade to MJPEG,
        /// never to a broken stream.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        video: Vec<String>,
        /// Audio transports the *offerer* can consume for an audio
        /// route, best first (today: `"opus"` — the mesh's RTP audio
        /// lane). Same degradation contract as `video`: absent or
        /// unrecognized on either side means PCM frames over the media
        /// channel, never a broken stream. Only meaningful when the
        /// offerer is the route's sink (the console's listen leg).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        audio: Vec<String>,

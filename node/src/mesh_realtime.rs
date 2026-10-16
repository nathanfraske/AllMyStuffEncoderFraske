//! Opt-in H.264/Opus media for existing application-approved live routes.
//!
//! The application keeps its route state and consent checks. A current-context
//! callback samples that state before and after each I/O operation, including
//! the application's route generation so a same-id replacement cannot inherit
//! its predecessor's flow. This module creates no application grant or registry.
//!
//! The caller retains the owning [`RealtimeSession`] during setup and media I/O.
//! After success, failure or cancellation it awaits route close and owner close.
//! Local socket closure and a successful write do not establish remote delivery
//! or retirement. Uncertain upstream cleanup remains an explicit error.

use std::fmt;
use std::sync::Arc;

use allmystuff_graph::MediaKind;
use allmystuff_mesh_control::realtime::{
    Encoding, FlowDirection, FlowSpec, FlowTermination, InboundPipe, RealtimeError, RealtimeFlow,
    RealtimeSession, ReceivedUnit, RtpKind,
};
use allmystuff_session::{LiveRoute, Session};

use crate::mesh::{node_of, pubkey_part, route_sinks_on, route_sources_on};

pub type Result<T> = std::result::Result<T, AvRouteError>;

/// Application opt-ins carried by the existing route negotiation fields. The
/// normal node advertises neither capability as part of this tooling slice.
pub const REALTIME_H264_CAPABILITY: &str = "h264-realtime-v1";
pub const REALTIME_OPUS_CAPABILITY: &str = "opus-realtime-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvDirection {
    Send,
    Receive,
}

/// A snapshot of two routes already approved by the application's existing
/// catalog/session path. `generation` is the application's current route
/// lifetime, advanced on either route's teardown, replacement or approval loss.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvRouteContext {
    local: String,
    peer: String,
    video: LiveRoute,
    audio: LiveRoute,
    generation: u64,
    direction: AvDirection,
}

impl AvRouteContext {
    /// Bind the approved session's two active routes. An embedding application
    /// still rechecks its current catalog, room and consent policy in the
    /// context callback; a Session snapshot alone does not issue authority.
    pub fn from_session(
        session: &Session,
        video_route_id: &str,
        audio_route_id: &str,
        generation: u64,
    ) -> Result<Self> {
        let video = session
            .route(video_route_id)
            .ok_or(AvRouteError::InvalidRoutes(
                "video route is absent from the application session",
            ))?;
        let audio = session
            .route(audio_route_id)
            .ok_or(AvRouteError::InvalidRoutes(
                "audio route is absent from the application session",
            ))?;
        if !video.is_active() || !audio.is_active() {
            return Err(AvRouteError::InvalidRoutes("both routes must be active"));
        }
        if !matches!(video.route.media, MediaKind::Display | MediaKind::Video)
            || audio.route.media != MediaKind::Audio
            || video.route.id == audio.route.id
        {
            return Err(AvRouteError::InvalidRoutes(
                "expected distinct video and audio routes",
            ));
        }
        if !video
            .video
            .iter()
            .any(|value| value == REALTIME_H264_CAPABILITY)
            || !audio
                .audio
                .iter()
                .any(|value| value == REALTIME_OPUS_CAPABILITY)
        {
            return Err(AvRouteError::InvalidRoutes(
                "both application routes must explicitly select realtime media",
            ));
        }
        let local = pubkey_part(session.me().as_str());
        let peer = pubkey_part(video.peer.as_str());
        if local.is_empty()
            || peer.is_empty()
            || local == peer
            || peer != pubkey_part(audio.peer.as_str())
        {
            return Err(AvRouteError::InvalidRoutes(
                "both routes must name one remote peer",
            ));
        }
        let direction = if route_sources_on(&video.route, local)
            && route_sources_on(&audio.route, local)
            && pubkey_part(&node_of(video.route.to.as_str())) == peer
            && pubkey_part(&node_of(audio.route.to.as_str())) == peer
        {
            AvDirection::Send
        } else if route_sinks_on(&video.route, local)
            && route_sinks_on(&audio.route, local)
            && pubkey_part(&node_of(video.route.from.as_str())) == peer
            && pubkey_part(&node_of(audio.route.from.as_str())) == peer
        {
            AvDirection::Receive
        } else {
            return Err(AvRouteError::InvalidRoutes(
                "route endpoints do not form one AV direction",
            ));
        };
        Ok(Self {
            local: local.to_owned(),
            peer: peer.to_owned(),
            video: video.clone(),
            audio: audio.clone(),
            generation,
            direction,
        })
    }

    pub fn direction(&self) -> AvDirection {
        self.direction
    }
    pub fn peer(&self) -> &str {
        &self.peer
    }
    pub fn video_route_id(&self) -> &str {
        &self.video.route.id
    }
    pub fn audio_route_id(&self) -> &str {
        &self.audio.route.id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn live_routes(&self) -> [&LiveRoute; 2] {
        [&self.video, &self.audio]
    }
}

/// Return the current approved session snapshot and its application epoch.
/// Recheck the application's current catalog/room/consent policy and return
/// `None` when approval ends. The callback must not retain a session lock while
/// returning; the facade never holds application locks across I/O.
pub type AvContextSource = Arc<dyn Fn() -> Option<AvRouteContext> + Send + Sync>;

pub struct AvRouteConfig {
    pub network: String,
    pub video_label: String,
    pub audio_label: String,
    pub video_encoding: Encoding,
    pub audio_encoding: Encoding,
}

impl AvRouteConfig {
    fn validate(&self) -> Result<()> {
        if self.network.is_empty() {
            return Err(AvRouteError::InvalidConfig(
                "an explicit network is required",
            ));
        }
        if self.video_label.is_empty()
            || self.video_label.len() > 255
            || self.audio_label.is_empty()
            || self.audio_label.len() > 255
            || self.video_label == self.audio_label
        {
            return Err(AvRouteError::InvalidConfig(
                "distinct flow labels must contain 1..255 UTF-8 bytes",
            ));
        }
        if !matches!(&self.video_encoding.rtp_kind, RtpKind::Video)
            || !self.video_encoding.mime.eq_ignore_ascii_case("video/H264")
            || self.video_encoding.clock_rate != 90_000
            || self.video_encoding.channels != 0
        {
            return Err(AvRouteError::InvalidConfig(
                "video requires the selected H264 90 kHz encoding",
            ));
        }
        // The RTP family is Opus/48000/2 even for mono PCM and mono packets
        // (RFC7587 section 7). It is independent of our mono software decoder.
        if !matches!(&self.audio_encoding.rtp_kind, RtpKind::Audio)
            || !self.audio_encoding.mime.eq_ignore_ascii_case("audio/opus")
            || self.audio_encoding.clock_rate != crate::audio::OPUS_RATE
            || self.audio_encoding.channels != 2
        {
            return Err(AvRouteError::InvalidConfig(
                "audio requires the selected Opus 48000/2 RTP family",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct CloseFailures {
    pub inbound: Option<RealtimeError>,
    pub video: Option<RealtimeError>,
    pub audio: Option<RealtimeError>,
}

impl CloseFailures {
    fn is_empty(&self) -> bool {
        self.inbound.is_none() && self.video.is_none() && self.audio.is_none()
    }
}

#[derive(Debug)]
pub enum AvRouteError {
    InvalidRoutes(&'static str),
    InvalidConfig(&'static str),
    InvalidUnit(&'static str),
    ApprovalEnded,
    ApprovalEndedWithCleanup(CloseFailures),
    WrongDirection,
    UnknownFlowLabel,
    Realtime(RealtimeError),
    OpenFailed {
        cause: RealtimeError,
        cleanup: CloseFailures,
    },
    CloseFailed(CloseFailures),
}

impl fmt::Display for AvRouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRoutes(reason)
            | Self::InvalidConfig(reason)
            | Self::InvalidUnit(reason) => f.write_str(reason),
            Self::ApprovalEnded => f.write_str("application route approval or generation ended"),
            Self::ApprovalEndedWithCleanup(_) => f.write_str(
                "application route approval ended; owned cleanup was not fully acknowledged",
            ),
            Self::WrongDirection => {
                f.write_str("operation does not match the approved route direction")
            }
            Self::UnknownFlowLabel => f.write_str("inbound flow is outside the bound AV route"),
            Self::Realtime(error) => write!(f, "realtime media: {error}"),
            Self::OpenFailed { .. } => {
                f.write_str("realtime AV setup failed; owned cleanup was not fully acknowledged")
            }
            Self::CloseFailed(_) => f.write_str("realtime AV close was not fully acknowledged"),
        }
    }
}

impl std::error::Error for AvRouteError {}

impl From<RealtimeError> for AvRouteError {
    fn from(error: RealtimeError) -> Self {
        Self::Realtime(error)
    }
}

fn ensure_current(current: &AvContextSource, bound: &AvRouteContext) -> Result<()> {
    match current() {
        Some(snapshot) if snapshot == *bound => Ok(()),
        _ => Err(AvRouteError::ApprovalEnded),
    }
}

/// Two explicitly selected flows belonging to existing live application routes.
/// Keep the external realtime owner alive, and await its close even if route
/// setup or an I/O future is cancelled. It owns the pending ACK/cleanup state.
pub struct RealtimeAvRoute {
    context: AvRouteContext,
    current: AvContextSource,
    video: RealtimeFlow,
    audio: RealtimeFlow,
    inbound: Option<InboundPipe>,
    video_label: String,
    audio_label: String,
}

impl RealtimeAvRoute {
    pub async fn open(
        owner: &mut RealtimeSession,
        current: AvContextSource,
        config: AvRouteConfig,
    ) -> Result<Self> {
        config.validate()?;
        let context = current().ok_or(AvRouteError::ApprovalEnded)?;
        if !owner.is_current() {
            return Err(RealtimeError::StaleSession.into());
        }
        let direction = || match context.direction {
            AvDirection::Send => FlowDirection::Outbound,
            AvDirection::Receive => FlowDirection::Inbound,
        };
        let video_label = config.video_label.clone();
        let audio_label = config.audio_label.clone();
        let video = owner
            .open_flow(FlowSpec {
                network: config.network.clone(),
                peer: context.peer.clone(),
                label: config.video_label,
                direction: direction(),
                encoding: config.video_encoding,
            })
            .await?;
        if ensure_current(&current, &context).is_err() {
            return Err(approval_error(
                close_resources(None, Some(video), None).await,
            ));
        }
        // Cancellation drops handles into the retained owner's cleanup registry.
        // It cannot falsely report acknowledged upstream retirement.
        let audio = match owner
            .open_flow(FlowSpec {
                network: config.network.clone(),
                peer: context.peer.clone(),
                label: config.audio_label,
                direction: direction(),
                encoding: config.audio_encoding,
            })
            .await
        {
            Ok(flow) => flow,
            Err(cause) => {
                let cleanup = close_resources(None, Some(video), None).await;
                return Err(open_error(cause, cleanup));
            }
        };
        if ensure_current(&current, &context).is_err() {
            return Err(approval_error(
                close_resources(None, Some(video), Some(audio)).await,
            ));
        }
        let inbound = if context.direction == AvDirection::Receive {
            match owner.open_inbound(&config.network, &context.peer).await {
                Ok(pipe) => Some(pipe),
                Err(cause) => {
                    let cleanup = close_resources(None, Some(video), Some(audio)).await;
                    return Err(open_error(cause, cleanup));
                }
            }
        } else {
            None
        };
        let route = Self {
            context,
            current,
            video,
            audio,
            inbound,
            video_label,
            audio_label,
        };
        if ensure_current(&route.current, &route.context).is_err() {
            return Err(approval_error(route.cleanup().await));
        }
        Ok(route)
    }

    pub fn context(&self) -> &AvRouteContext {
        &self.context
    }

    /// Observe this exact pipe's termination without sending another unit or
    /// treating local write completion as peer-delivery evidence.
    pub fn video_termination(&self) -> FlowTermination {
        self.video.termination()
    }

    pub fn audio_termination(&self) -> FlowTermination {
        self.audio.termination()
    }

    /// Borrow independent send handles so the application can schedule audio
    /// while a video operation is pending. Pacing and recovery remain caller policy.
    pub fn split_senders(&mut self) -> Result<(VideoSender<'_>, AudioSender<'_>)> {
        ensure_current(&self.current, &self.context)?;
        if self.context.direction != AvDirection::Send {
            return Err(AvRouteError::WrongDirection);
        }
        Ok((
            VideoSender {
                flow: &mut self.video,
                current: &self.current,
                bound: &self.context,
            },
            AudioSender {
                flow: &mut self.audio,
                current: &self.current,
                bound: &self.context,
            },
        ))
    }

    pub async fn send_video(&mut self, duration_us: u64, payload: &[u8]) -> Result<()> {
        let (mut video, _) = self.split_senders()?;
        video.send(duration_us, payload).await
    }

    pub async fn send_audio(&mut self, duration_us: u64, payload: &[u8]) -> Result<()> {
        let (_, mut audio) = self.split_senders()?;
        audio.send(duration_us, payload).await
    }

    pub async fn recv(&mut self) -> Result<ReceivedAvUnit> {
        ensure_current(&self.current, &self.context)?;
        let pipe = self.inbound.as_mut().ok_or(AvRouteError::WrongDirection)?;
        let unit = pipe.recv().await?;
        ensure_current(&self.current, &self.context)?;
        if unit.label() == self.video_label.as_bytes() {
            Ok(ReceivedAvUnit::Video(ReceivedVideo(unit)))
        } else if unit.label() == self.audio_label.as_bytes() {
            Ok(ReceivedAvUnit::Audio(ReceivedAudio(unit)))
        } else {
            Err(AvRouteError::UnknownFlowLabel)
        }
    }

    /// Close every owned pipe/flow, including after application approval ends.
    /// The caller still awaits the retained realtime owner's close afterwards.
    pub async fn close(self) -> Result<()> {
        let failures = self.cleanup().await;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(AvRouteError::CloseFailed(failures))
        }
    }

    async fn cleanup(self) -> CloseFailures {
        close_resources(self.inbound, Some(self.video), Some(self.audio)).await
    }
}

fn approval_error(cleanup: CloseFailures) -> AvRouteError {
    if cleanup.is_empty() {
        AvRouteError::ApprovalEnded
    } else {
        AvRouteError::ApprovalEndedWithCleanup(cleanup)
    }
}

fn open_error(cause: RealtimeError, cleanup: CloseFailures) -> AvRouteError {
    if cleanup.is_empty() {
        AvRouteError::Realtime(cause)
    } else {
        AvRouteError::OpenFailed { cause, cleanup }
    }
}

async fn close_resources(
    inbound: Option<InboundPipe>,
    video: Option<RealtimeFlow>,
    audio: Option<RealtimeFlow>,
) -> CloseFailures {
    // An earlier failure must not skip closure of the other known flow.
    let inbound = match inbound {
        Some(pipe) => pipe.close().await.err(),
        None => None,
    };
    let video = match video {
        Some(flow) => flow.close().await.err(),
        None => None,
    };
    let audio = match audio {
        Some(flow) => flow.close().await.err(),
        None => None,
    };
    CloseFailures {
        inbound,
        video,
        audio,
    }
}

pub struct VideoSender<'a> {
    flow: &'a mut RealtimeFlow,
    current: &'a AvContextSource,
    bound: &'a AvRouteContext,
}

impl VideoSender<'_> {
    /// Send one complete H.264 Annex-B AU before the legacy fragment pacer.
    /// Successful completion means a local write, without a delivery ACK.
    pub async fn send(&mut self, duration_us: u64, payload: &[u8]) -> Result<()> {
        ensure_current(self.current, self.bound)?;
        let duration = u32::try_from(duration_us).map_err(|_| {
            AvRouteError::InvalidUnit("video duration exceeds the selected wire field")
        })?;
        if duration == 0 || payload.is_empty() {
            return Err(AvRouteError::InvalidUnit(
                "video requires a nonempty whole AU and nonzero duration",
            ));
        }
        self.flow.send(duration, payload).await?;
        ensure_current(self.current, self.bound)
    }
}

pub struct AudioSender<'a> {
    flow: &'a mut RealtimeFlow,
    current: &'a AvContextSource,
    bound: &'a AvRouteContext,
}

impl AudioSender<'_> {
    /// Send one complete packet from the application's mono 20 ms Opus codec.
    pub async fn send(&mut self, duration_us: u64, payload: &[u8]) -> Result<()> {
        ensure_current(self.current, self.bound)?;
        if duration_us != crate::audio::OPUS_FRAME_US || payload.is_empty() {
            return Err(AvRouteError::InvalidUnit(
                "audio requires one nonempty mono 20 ms Opus packet",
            ));
        }
        self.flow
            .send(crate::audio::OPUS_FRAME_US as u32, payload)
            .await?;
        ensure_current(self.current, self.bound)
    }
}

pub enum ReceivedAvUnit {
    Video(ReceivedVideo),
    Audio(ReceivedAudio),
}

/// Retains the adapter's body lease through decoding or application handoff.
pub struct ReceivedVideo(ReceivedUnit);

impl ReceivedVideo {
    pub fn payload(&self) -> &[u8] {
        self.0.payload()
    }
    pub fn rtp_timestamp(&self) -> u32 {
        self.0.rtp_timestamp()
    }
    /// Preserve upstream semantics: the assembled H264 path reports its IDR
    /// entry-point bit here, rather than the original RTP packet marker.
    pub fn marker(&self) -> bool {
        self.0.marker()
    }
    /// Application-derived decode entry, never an invented wire field.
    pub fn is_decode_entry(&self) -> bool {
        crate::video_decode::is_decode_entry(self.payload())
    }
}

pub struct ReceivedAudio(ReceivedUnit);

impl ReceivedAudio {
    pub fn payload(&self) -> &[u8] {
        self.0.payload()
    }
    pub fn rtp_timestamp(&self) -> u32 {
        self.0.rtp_timestamp()
    }
    pub fn marker(&self) -> bool {
        self.0.marker()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use allmystuff_graph::{Capability, Catalog, Flow, MeshNode, NodeKind, Relationship, Route};
    use allmystuff_protocol::{ControlMessage, RouteControl};

    fn approved_route(from: &str, to: &str, media: MediaKind) -> Route {
        let mut catalog = Catalog::new();
        for node in ["local", "remote", "other"] {
            catalog.nodes.push(MeshNode {
                id: node.into(),
                label: node.into(),
                kind: NodeKind::Machine,
                relationship: Relationship::Mine,
                online: true,
            });
        }
        catalog.capabilities.push(Capability::new(
            node_of(from),
            from,
            "source",
            media,
            Flow::Source,
            "fixture",
        ));
        catalog.capabilities.push(Capability::new(
            node_of(to),
            to,
            "sink",
            media,
            Flow::Sink,
            "fixture",
        ));
        catalog.propose_route(&from.into(), &to.into()).unwrap()
    }

    fn accept(session: &mut Session, id: &str, peer: &str) {
        session.handle(
            peer.into(),
            ControlMessage::Route(RouteControl::Accept {
                route_id: id.into(),
                session: None,
                paced_video: false,
            }),
        );
    }

    fn offer_pair(audio_from: &str, audio_to: &str, audio_peer: &str) -> (Session, Route, Route) {
        let video = approved_route("local:screen", "remote:screen", MediaKind::Display);
        let audio = approved_route(audio_from, audio_to, MediaKind::Audio);
        let mut session = Session::new("local");
        session.offer(
            video.clone(),
            "remote",
            vec![REALTIME_H264_CAPABILITY.into()],
            vec![],
        );
        session.offer(
            audio.clone(),
            audio_peer,
            vec![],
            vec![REALTIME_OPUS_CAPABILITY.into()],
        );
        (session, video, audio)
    }

    #[test]
    fn realtime_context_requires_two_accepted_explicit_routes() {
        let (mut session, video, audio) = offer_pair("local:audio", "remote:audio", "remote");
        accept(&mut session, &video.id, "remote");
        assert!(AvRouteContext::from_session(&session, &video.id, &audio.id, 1).is_err());
        accept(&mut session, &audio.id, "remote");
        let context = AvRouteContext::from_session(&session, &video.id, &audio.id, 1).unwrap();
        assert_eq!(context.direction(), AvDirection::Send);
        assert_eq!(context.peer(), "remote");

        // Existing legacy route offers cannot accidentally activate this path.
        session.offer(audio.clone(), "remote", vec![], vec!["opus".into()]);
        accept(&mut session, &audio.id, "remote");
        assert!(AvRouteContext::from_session(&session, &video.id, &audio.id, 1).is_err());
        assert!(AvRouteContext::from_session(&session, &video.id, &video.id, 1).is_err());
    }

    #[test]
    fn unrelated_peer_and_crossed_direction_cannot_share_an_av_binding() {
        for (from, to, peer) in [
            ("local:audio", "other:audio", "other"),
            ("remote:audio", "local:audio", "remote"),
        ] {
            let (mut session, video, audio) = offer_pair(from, to, peer);
            accept(&mut session, &video.id, "remote");
            accept(&mut session, &audio.id, peer);
            assert!(AvRouteContext::from_session(&session, &video.id, &audio.id, 1).is_err());
        }
    }

    #[test]
    fn teardown_and_same_id_replacement_end_the_bound_application_epoch() {
        let (mut session, video, audio) = offer_pair("local:audio", "remote:audio", "remote");
        accept(&mut session, &video.id, "remote");
        accept(&mut session, &audio.id, "remote");
        let bound = AvRouteContext::from_session(&session, &video.id, &audio.id, 7).unwrap();
        let state = Arc::new(parking_lot::Mutex::new((session, 7u64)));
        let sampled = state.clone();
        let video_id = video.id.clone();
        let audio_id = audio.id.clone();
        let current: AvContextSource = Arc::new(move || {
            let state = sampled.lock();
            AvRouteContext::from_session(&state.0, &video_id, &audio_id, state.1).ok()
        });
        assert!(ensure_current(&current, &bound).is_ok());
        {
            let mut state = state.lock();
            let _ = state.0.teardown(&video.id);
        }
        assert!(matches!(
            ensure_current(&current, &bound),
            Err(AvRouteError::ApprovalEnded)
        ));
        {
            let mut state = state.lock();
            state.1 += 1;
            state.0.offer(
                video.clone(),
                "remote",
                vec![REALTIME_H264_CAPABILITY.into()],
                vec![],
            );
            accept(&mut state.0, &video.id, "remote");
        }
        assert!(current().is_some());
        assert!(matches!(
            ensure_current(&current, &bound),
            Err(AvRouteError::ApprovalEnded)
        ));
    }

    #[test]
    fn opus_rtp_family_is_independent_of_mono_software_pcm() {
        let mut config = AvRouteConfig {
            network: "explicit-test-network".into(),
            video_label: "video".into(),
            audio_label: "audio".into(),
            video_encoding: Encoding {
                rtp_kind: RtpKind::Video,
                mime: "video/H264".into(),
                clock_rate: 90_000,
                channels: 0,
            },
            audio_encoding: Encoding {
                rtp_kind: RtpKind::Audio,
                mime: "audio/opus".into(),
                clock_rate: 48_000,
                channels: 2,
            },
        };
        assert!(config.validate().is_ok());
        config.audio_encoding.channels = 1;
        assert!(matches!(
            config.validate(),
            Err(AvRouteError::InvalidConfig(_))
        ));
    }
}

//! Ignored, device-free consumer of the opt-in node AV facade.
//!
//! The Python controller owns both private daemons. The generator needs no
//! endpoint and runs the existing software codecs only inside this node test.

#[path = "support/mesh_realtime_codec.rs"]
mod codec;
#[path = "../../crates/allmystuff-mesh-control/tests/support/paired.rs"]
pub mod paired;

use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use allmystuff_graph::{
    Capability, Catalog, Flow, MediaKind, MeshNode, NodeKind, Relationship, Route,
};
use allmystuff_mesh_control::realtime::{PipeEnd, RealtimeError};
use allmystuff_node::mesh_realtime::{
    AudioSender, AvContextSource, AvRouteConfig, AvRouteContext, AvRouteError, RealtimeAvRoute,
    ReceivedAvUnit, VideoSender, REALTIME_H264_CAPABILITY, REALTIME_OPUS_CAPABILITY,
};
use allmystuff_protocol::{ControlMessage, RouteControl};
use allmystuff_session::{Effect, Session};
use allmystuff_video::metadata::{
    insert_au_identity_marker, peek_au_identity_marker, AuIdentity, AuRecovery,
};
use parking_lot::Mutex;
use serde_json::{json, Value};

use paired::{Context, Unit};

fn run(work: impl Future<Output = ()>, deadline: Duration) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(deadline, work)
                .await
                .expect("actual consumer pair absolute deadline");
        });
}

fn new_file(path: &Path, bytes: &[u8]) {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).expect("create new private vector file");
    file.write_all(bytes).unwrap();
    file.flush().unwrap();
}

fn identity(index: usize) -> AuIdentity {
    AuIdentity {
        sequence: 1_000 + index as u64,
        recovery: AuRecovery::Reset,
    }
}

#[test]
#[ignore = "explicit NEW AMS_MESH_PAIRED_VECTOR_OUTPUT under a protected parent; no daemon needed"]
fn prepare_realtime_pair_vectors() {
    let output = PathBuf::from(paired::required("AMS_MESH_PAIRED_VECTOR_OUTPUT"));
    assert!(output.is_absolute());
    let parent = output.parent().expect("explicit protected parent");
    let metadata = std::fs::symlink_metadata(parent).unwrap();
    assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        assert_eq!(
            metadata.permissions().mode() & 0o077,
            0,
            "parent must be private to the test account"
        );
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&output)
            .unwrap();
    }
    #[cfg(not(unix))]
    std::fs::create_dir(&output).unwrap();

    let mut fixture = codec::generated_media();
    let mut video = Vec::new();
    let mut audio = Vec::new();
    let mut video_reference = Vec::new();
    for (index, unit) in fixture.video.iter_mut().enumerate() {
        assert!(peek_au_identity_marker(&unit.data).is_none());
        insert_au_identity_marker(&mut unit.data, identity(index), false);
        assert_eq!(peek_au_identity_marker(&unit.data), Some(identity(index)));
        assert!(codec::annex_b_nal_payloads(&unit.data)
            .iter()
            .any(|nal| nal[0] & 0x1f == 6));
        let transported = paired::expected_h264_payloads(&unit.data);
        let file = format!("video-{index}.h264");
        new_file(&output.join(&file), &unit.data);
        video.push(
            json!({"file":file, "sha256":paired::digest(&unit.data), "bytes":unit.data.len(),
                          "duration_us":unit.duration_us, "ts_us":unit.ts_us}),
        );
        video_reference.push(json!({"width":unit.width, "height":unit.height,
            "encoder_key_not_wire_flag":unit.key, "sequence":identity(index).sequence,
            "recovery":"reset", "ams_identity_sei":true,
            "source_nal_types":codec::annex_b_nal_payloads(&unit.data).iter().map(|nal| nal[0] & 0x1f).collect::<Vec<_>>(),
            "transported_nal_types":transported.iter().map(|nal| nal[0] & 0x1f).collect::<Vec<_>>(),
            "parameter_pairs_fit_pinned_mtu":true}));
    }
    for (index, unit) in fixture.audio.iter().enumerate() {
        let file = format!("audio-{index}.opus");
        new_file(&output.join(&file), &unit.data);
        audio.push(
            json!({"file":file, "sha256":paired::digest(&unit.data), "bytes":unit.data.len(),
                          "duration_us":unit.duration_us, "ts_us":unit.ts_us}),
        );
    }
    // Prove that this process's actual encoded, SEI-marked reference decodes.
    // The separate pair test must still decode the received units itself.
    codec::assert_decoded_video(
        &fixture
            .video
            .iter()
            .map(|unit| unit.data.clone())
            .collect::<Vec<_>>(),
    );
    codec::assert_decoded_audio(
        &fixture
            .audio
            .iter()
            .map(|unit| unit.data.clone())
            .collect::<Vec<_>>(),
    );
    let manifest = json!({"version":1, "video":video, "audio":audio,
        "software_reference":{"video":video_reference,
          "audio":{"pcm_channels":1,"rtp_family_channels":2,"rate":48000,"samples_per_packet":960,
                   "source_tone_hz":1000,"source_peak":10000},
          "video_backend":"source OpenH264", "audio_backend":"software Opus",
          "decode_checks":"three ordered 64x64 quadrant pictures; six 960-sample mono tone packets",
          "hardware_capture_display_playback":false}});
    let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    new_file(&output.join("vectors.json"), &bytes);
    println!(
        "MESH_CONTROL_VECTORS {}",
        json!({"manifest":output.join("vectors.json"),
        "sha256":paired::digest(&bytes),"video_units":3,"audio_packets":6,
        "reference_software_decode":true,"hardware":false})
    );
}

struct ApprovedState {
    session: Session,
    epoch: u64,
    enabled: bool,
}

fn approved_route(catalog: &mut Catalog, from: &str, to: &str, video: bool) -> Route {
    let media = if video {
        MediaKind::Display
    } else {
        MediaKind::Audio
    };
    let source = format!(
        "{from}:{}",
        if video {
            "screen-source"
        } else {
            "audio-source"
        }
    );
    let sink = format!("{to}:{}", if video { "screen-sink" } else { "audio-sink" });
    catalog.capabilities.push(Capability::new(
        from,
        source.as_str(),
        "source",
        media,
        Flow::Source,
        "owned fixture",
    ));
    catalog.capabilities.push(Capability::new(
        to,
        sink.as_str(),
        "sink",
        media,
        Flow::Sink,
        "owned fixture",
    ));
    catalog
        .propose_route(&source.into(), &sink.into())
        .expect("catalog-approved fixture route")
}

fn approve_pair(
    a_id: &str,
    b_id: &str,
    epoch: u64,
) -> (
    Arc<Mutex<ApprovedState>>,
    Arc<Mutex<ApprovedState>>,
    String,
    String,
) {
    let mut catalog = Catalog::new();
    for public_id in [a_id, b_id] {
        catalog.nodes.push(MeshNode {
            id: public_id.into(),
            label: "owned fixture peer".into(),
            kind: NodeKind::Machine,
            relationship: Relationship::Mine,
            online: true,
        });
    }
    let video = approved_route(&mut catalog, a_id, b_id, true);
    let audio = approved_route(&mut catalog, a_id, b_id, false);
    let mut a = Session::new(a_id);
    let mut b = Session::new(b_id);
    // This isolated process owner explicitly approves only the two synthetic
    // routes. No daemon capability grants product permissions or ledger access.
    b.auto_accept = true;
    for (route, video, audio) in [
        (
            video.clone(),
            vec![REALTIME_H264_CAPABILITY.to_owned()],
            vec![],
        ),
        (
            audio.clone(),
            vec![],
            vec![REALTIME_OPUS_CAPABILITY.to_owned()],
        ),
    ] {
        let offer = a.offer(route.clone(), b_id, video, audio);
        let effects = b.handle(a_id.into(), offer);
        let accepts: Vec<_> = effects
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::Send {
                    peer,
                    message:
                        ControlMessage::Route(RouteControl::Accept {
                            route_id,
                            session,
                            paced_video,
                        }),
                } => {
                    assert_eq!(peer.as_str(), a_id);
                    assert_eq!(route_id, route.id);
                    Some(ControlMessage::Route(RouteControl::Accept {
                        route_id,
                        session,
                        paced_video,
                    }))
                }
                _ => None,
            })
            .collect();
        assert_eq!(accepts.len(), 1, "real pure Session Offer/Accept exchange");
        for accept in accepts {
            a.handle(b_id.into(), accept);
        }
        assert!(a.route(&route.id).unwrap().is_active());
        assert!(b.route(&route.id).unwrap().is_active());
    }
    (
        Arc::new(Mutex::new(ApprovedState {
            session: a,
            epoch,
            enabled: true,
        })),
        Arc::new(Mutex::new(ApprovedState {
            session: b,
            epoch,
            enabled: true,
        })),
        video.id,
        audio.id,
    )
}

fn current(
    state: Arc<Mutex<ApprovedState>>,
    video_id: String,
    audio_id: String,
) -> AvContextSource {
    Arc::new(move || {
        let state = state.lock();
        if !state.enabled {
            return None;
        }
        AvRouteContext::from_session(&state.session, &video_id, &audio_id, state.epoch).ok()
    })
}

fn config(context: &Context, prefix: &str) -> AvRouteConfig {
    AvRouteConfig {
        network: context.network.clone(),
        video_label: format!("{prefix}/video"),
        audio_label: format!("{prefix}/audio"),
        video_encoding: paired::encoding(true),
        audio_encoding: paired::encoding(false),
    }
}

async fn send_video(sender: &mut VideoSender<'_>, units: &[Unit]) {
    for unit in units {
        sender
            .send(u64::from(unit.duration_us), &unit.data)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_micros(u64::from(unit.duration_us))).await;
    }
}

async fn send_audio(sender: &mut AudioSender<'_>, units: &[Unit]) {
    for unit in units {
        sender
            .send(u64::from(unit.duration_us), &unit.data)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_micros(u64::from(unit.duration_us))).await;
    }
}

async fn receive(context: &Context, route: &mut RealtimeAvRoute, prefix: &str) -> Value {
    let mut video = Vec::new();
    let mut audio = Vec::new();
    let mut observations = Vec::new();
    let mut video_prior = None;
    let mut audio_prior = None;
    while video.len() < context.video.len() || audio.len() < context.audio.len() {
        match route.recv().await.unwrap() {
            ReceivedAvUnit::Video(unit) => {
                assert!(video.len() < context.video.len(), "extra video AU");
                let expected = &context.video[video.len()];
                paired::check_received(expected, unit.payload(), unit.marker(), true);
                assert_eq!(
                    peek_au_identity_marker(unit.payload()),
                    Some(identity(video.len())),
                    "AMS AU identity/order survived RTP"
                );
                assert_eq!(
                    unit.is_decode_entry(),
                    unit.marker(),
                    "caller-derived H264 IDR entry agrees with assembled marker"
                );
                paired::check_delta(
                    video_prior,
                    unit.rtp_timestamp(),
                    expected.duration_us,
                    90000,
                );
                video_prior = Some(unit.rtp_timestamp());
                observations.push(paired::observation(
                    expected,
                    unit.payload(),
                    unit.rtp_timestamp(),
                    unit.marker(),
                    &format!("{prefix}/video"),
                ));
                video.push(unit.payload().to_vec());
            }
            ReceivedAvUnit::Audio(unit) => {
                assert!(audio.len() < context.audio.len(), "extra Opus packet");
                let expected = &context.audio[audio.len()];
                paired::check_received(expected, unit.payload(), unit.marker(), false);
                paired::check_delta(
                    audio_prior,
                    unit.rtp_timestamp(),
                    expected.duration_us,
                    48000,
                );
                audio_prior = Some(unit.rtp_timestamp());
                observations.push(paired::observation(
                    expected,
                    unit.payload(),
                    unit.rtp_timestamp(),
                    unit.marker(),
                    &format!("{prefix}/audio"),
                ));
                audio.push(unit.payload().to_vec());
            }
        }
    }
    // Each wrapper's retained lease drops after extracting this bounded copy.
    // Codec verification belongs only to this node validation executable.
    codec::assert_decoded_video(&video);
    codec::assert_decoded_audio(&audio);
    json!({"video_units":video.len(),"audio_packets":audio.len(),"units":observations,
           "ams_au_identity_order":true,"received_software_decode":true,"hardware":false})
}

async fn transfer(
    context: &Context,
    source: &mut RealtimeAvRoute,
    sink: &mut RealtimeAvRoute,
    prefix: &str,
) -> Value {
    let (mut video, mut audio) = source.split_senders().unwrap();
    let sending = async {
        let ((), ()) = tokio::join!(
            send_video(&mut video, &context.video),
            send_audio(&mut audio, &context.audio)
        );
    };
    let ((), receipt) = tokio::join!(sending, receive(context, sink, prefix));
    receipt
}

fn expected_retirement_error(error: RealtimeError) -> Value {
    match error {
        RealtimeError::Refused { code } => {
            json!({"kind":"refused_consumed_close_not_retried", "code":code})
        }
        RealtimeError::StaleSession => json!({"kind":"locally_stale_no_close_write"}),
        RealtimeError::SessionFenced => json!({"kind":"locally_fenced_no_close_write"}),
        unexpected => panic!(
            "unexpected retirement failure after the intentional peer replacement: {unexpected}"
        ),
    }
}

fn route_retirement(result: allmystuff_node::mesh_realtime::Result<()>) -> Value {
    match result {
        Ok(()) => json!({"kind":"all_exact_closes_acknowledged"}),
        Err(AvRouteError::CloseFailed(failures)) => {
            assert!(
                failures.inbound.is_none(),
                "local inbound close must still complete"
            );
            json!({"kind":"not_all_native_retirement_acknowledged",
                   "video":failures.video.map(expected_retirement_error),
                   "audio":failures.audio.map(expected_retirement_error)})
        }
        Err(unexpected) => panic!("unexpected route close after peer replacement: {unexpected}"),
    }
}

fn owner_retirement(result: allmystuff_mesh_control::realtime::Result<()>) -> &'static str {
    match result {
        Ok(()) => "acknowledged",
        Err(RealtimeError::CleanupUncertain) => {
            "cleanup_uncertain_after_intentional_peer_replacement"
        }
        Err(unexpected) => panic!("unexpected owner close after peer replacement: {unexpected}"),
    }
}

#[test]
#[ignore = "requires checksum-pinned private pair and mesh-control-paired.py"]
fn paired_endpoint_ready() {
    let context = Context::load();
    run(paired::endpoint_ready(&context), Duration::from_secs(35));
}

#[test]
#[ignore = "requires checksum-pinned private pair, valid vectors and mesh-control-paired.py"]
fn isolated_realtime_consumer_pair() {
    let context = Context::load();
    run(
        async {
            let a_id = paired::ready(&context.a, &context.network).await;
            let b_id = paired::ready(&context.b, &context.network).await;
            assert_ne!(a_id, b_id);
            paired::connect(&context, &b_id).await;
            let mut a = context.session(&context.a).await;
            let mut b = context.session(&context.b).await;
            let generation = a.session.generation();
            let (state_a, state_b, video, audio) = approve_pair(&a_id, &b_id, 1);
            let mut sink = RealtimeAvRoute::open(
                &mut b.session,
                current(state_b.clone(), video.clone(), audio.clone()),
                config(&context, "consumer"),
            )
            .await
            .unwrap();
            let mut source = RealtimeAvRoute::open(
                &mut a.session,
                current(state_a.clone(), video.clone(), audio.clone()),
                config(&context, "consumer"),
            )
            .await
            .unwrap();
            let initial = transfer(&context, &mut source, &mut sink, "consumer").await;
            let initial_path = paired::selected_paths(&context, &a_id, &b_id).await;
            let mut video_termination = source.video_termination();
            let mut audio_termination = source.audio_termination();
            context.restart_marker();
            loop {
                if context.root.join("restart-complete").exists() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert_eq!(
                std::fs::read_to_string(context.root.join("restart-complete")).unwrap(),
                context.owner
            );
            assert_eq!(
                paired::ready(&context.b, &context.network).await,
                b_id,
                "owned B keeps its private identity"
            );
            let reconnected = paired::raw(
                &context.a,
                json!({"op":"network_reconnect","network":context.network,"peer":b_id}),
                context.realtime_policy().operation_timeout,
            )
            .await;
            assert!(reconnected.ok);
            paired::connect(&context, &b_id).await;
            // Exact old F cannot resolve to the successor peer session. One small
            // old unit prompts the server's source-defined SessionNotCurrent path;
            // a complete local write is not evidence of acceptance or delivery.
            let old_video_write = source
                .send_video(
                    u64::from(context.video[0].duration_us),
                    &context.video[0].data,
                )
                .await;
            let old_audio_write = source
                .send_audio(
                    u64::from(context.audio[0].duration_us),
                    &context.audio[0].data,
                )
                .await;
            for outcome in [&old_video_write, &old_audio_write] {
                assert!(
                    !matches!(outcome, Err(AvRouteError::Realtime(RealtimeError::Timeout))),
                    "timeout cannot establish stale-pipe refusal"
                );
            }
            let video_end = video_termination
                .wait(context.realtime_policy().operation_timeout)
                .await
                .unwrap();
            let audio_end = audio_termination
                .wait(context.realtime_policy().operation_timeout)
                .await
                .unwrap();
            assert!(matches!(
                video_end,
                PipeEnd::RemoteClosed | PipeEnd::SessionEnded
            ));
            assert!(matches!(
                audio_end,
                PipeEnd::RemoteClosed | PipeEnd::SessionEnded
            ));
            state_a.lock().enabled = false;
            assert!(
                matches!(source.split_senders(), Err(AvRouteError::ApprovalEnded)),
                "revoked app context must refuse locally before any media write"
            );
            {
                let mut state = state_a.lock();
                state.enabled = true;
                state.epoch += 1;
            }
            assert!(
                matches!(source.split_senders(), Err(AvRouteError::ApprovalEnded)),
                "same routes with a successor app epoch cannot revive old handles"
            );
            let old_source_close = route_retirement(source.close().await);
            let old_sink_close = route_retirement(sink.close().await);
            let old_a_close = owner_retirement(a.close().await);
            let old_b_close = owner_retirement(b.close().await);
            let mut a = context.session(&context.a).await;
            let mut b = context.session(&context.b).await;
            assert_ne!(a.session.generation(), generation);
            // This is an explicit two-sided setup prerequisite after the old lifetime checks.
            // Local ACTIVE still does not reserve the following inbound claim.
            let replacement_b_timeout = context.realtime_policy().operation_timeout;
            let replacement_b_wait_ms = u64::try_from(replacement_b_timeout.as_millis())
                .unwrap()
                .checked_sub(1000)
                .unwrap();
            let replacement_b_connected = paired::raw(
                &context.b,
                json!({"op":"network_connect_peer", "network":&context.network,
                       "peer":&a_id, "pin":false, "wait_ms":replacement_b_wait_ms}),
                replacement_b_timeout,
            )
            .await;
            assert!(
                replacement_b_connected.ok
                    && replacement_b_connected
                        .data
                        .as_ref()
                        .and_then(|data| data.get("active"))
                        .and_then(Value::as_bool)
                        == Some(true),
                "one explicit replacement-B setup dial must reach ACTIVE; ok={}; active_shape={}; error_family={}",
                replacement_b_connected.ok,
                match replacement_b_connected.data.as_ref().and_then(|data| data.get("active")) {
                    None => "absent",
                    Some(Value::Bool(false)) => "false",
                    Some(Value::Bool(true)) => "true",
                    Some(_) => "not_bool",
                },
                match replacement_b_connected.error.as_deref() {
                    None => "absent",
                    Some(error) if error.starts_with("unknown network:") => "unknown_network",
                    Some(_) => "other",
                }
            );
            let (state_a, state_b, video, audio) = approve_pair(&a_id, &b_id, 2);
            let mut sink = RealtimeAvRoute::open(
                &mut b.session,
                current(state_b, video.clone(), audio.clone()),
                config(&context, "consumer"),
            )
            .await
            .unwrap();
            let mut source = RealtimeAvRoute::open(
                &mut a.session,
                current(state_a, video, audio),
                config(&context, "consumer"),
            )
            .await
            .unwrap();
            let after_restart = transfer(&context, &mut source, &mut sink, "consumer").await;
            let after_restart_path = paired::selected_paths(&context, &a_id, &b_id).await;
            source.close().await.unwrap();
            sink.close().await.unwrap();
            a.close().await.unwrap();
            b.close().await.unwrap();
            paired::emit(
                json!({"scenario":"consumer","initial":initial,"after_owned_peer_restart":after_restart,
          "initial_path":initial_path,"after_restart_path":after_restart_path,
          "application_context_revocation":true,"same_id_epoch_refusal":true,"renewed_event_generation":true,
          "old_video_pipe_end":format!("{video_end:?}"),"old_audio_pipe_end":format!("{audio_end:?}"),
          "old_video_write_is_not_delivery":old_video_write.is_ok(),"old_audio_write_is_not_delivery":old_audio_write.is_ok(),
          "old_source_close":old_source_close,"old_sink_close":old_sink_close,
          "old_owner_a_close":old_a_close,"old_owner_b_close":old_b_close,
          "payload_delivery":true,"received_software_decode":true,"hardware_capture_display_playback":false,
          "native_cleanup_proof":"owned daemon shutdown receipt, not event socket EOF"}),
            );
        },
        context.timeout(),
    );
}

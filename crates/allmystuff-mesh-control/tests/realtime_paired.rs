//! Ignored actual candidate pair. mesh-control-paired.py owns both processes.

#[path = "support/paired.rs"]
pub mod paired;

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use allmystuff_mesh_control::realtime::{
    FlowDirection, InboundPipe, PipeEnd, RealtimeError, RealtimeFlow, RealtimeSession,
};
use serde_json::{json, Value};

use paired::{Context, Unit};

// Only static phase names, source/received hashes and bounded numeric metadata.
// Never log a capability, raw response, received label spelling or media bytes.
fn diagnostic(phase: &'static str, event: &'static str, details: Value) {
    println!(
        "MESH_CONTROL_PAIRED_DIAGNOSTIC {}",
        json!({"phase":phase, "event":event, "details":details})
    );
}

fn run(work: impl Future<Output = ()>, deadline: Duration) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(deadline, work)
                .await
                .expect("actual pair absolute deadline");
        });
}

async fn flows(
    context: &Context,
    source: &mut RealtimeSession,
    sink: &mut RealtimeSession,
    source_id: &str,
    sink_id: &str,
    prefix: &str,
    video: bool,
) -> (RealtimeFlow, RealtimeFlow) {
    let label = format!("{prefix}/{}", if video { "video" } else { "audio" });
    let inbound = sink
        .open_flow(context.flow(source_id, &label, FlowDirection::Inbound, video))
        .await
        .unwrap();
    let outbound = source
        .open_flow(context.flow(sink_id, &label, FlowDirection::Outbound, video))
        .await
        .unwrap();
    (outbound, inbound)
}

async fn receive(
    context: &Context,
    pipe: &mut InboundPipe,
    prefix: &str,
    phase: &'static str,
    video: bool,
    audio: bool,
) -> Value {
    let video_label = format!("{prefix}/video");
    let audio_label = format!("{prefix}/audio");
    let mut vi = 0;
    let mut ai = 0;
    let mut video_prior = None;
    let mut audio_prior = None;
    let mut observations = Vec::new();
    let video_count = if video { context.video.len() } else { 0 };
    let audio_count = if audio { context.audio.len() } else { 0 };
    while vi < video_count || ai < audio_count {
        let unit = pipe.recv().await.unwrap();
        let diagnostic_video = unit.label() == video_label.as_bytes();
        let diagnostic_index = if diagnostic_video { vi } else { ai };
        let diagnostic_expected = if diagnostic_video {
            context.video.get(vi)
        } else {
            context.audio.get(ai)
        };
        diagnostic(
            phase,
            "receive_before_checks",
            json!({
                "kind":if diagnostic_video { "video" } else { "audio_or_unrecognized_label" },
                "expected_index":diagnostic_index, "prior_units_verified":observations.len(),
                "expected_bytes":diagnostic_expected.map(|unit| unit.data.len()),
                "expected_sha256":diagnostic_expected.map(|unit| unit.sha256.as_str()),
                "received_bytes":unit.payload().len(), "received_sha256":paired::digest(unit.payload()),
                "received_label_bytes":unit.label().len(),
                "label_matches_expected":unit.label() == if diagnostic_video { video_label.as_bytes() } else { audio_label.as_bytes() },
                "rtp_timestamp":unit.rtp_timestamp(), "marker":unit.marker(),
                "prior_rtp_timestamp":if diagnostic_video { video_prior } else { audio_prior }
            }),
        );
        let (expected, is_video, previous) = if unit.label() == video_label.as_bytes() {
            assert!(vi < video_count, "unexpected/extra video on actual pair");
            let expected = &context.video[vi];
            vi += 1;
            (expected, true, &mut video_prior)
        } else {
            assert_eq!(
                unit.label(),
                audio_label.as_bytes(),
                "unexpected binary flow label"
            );
            assert!(ai < audio_count, "unexpected/extra audio on actual pair");
            let expected = &context.audio[ai];
            ai += 1;
            (expected, false, &mut audio_prior)
        };
        paired::check_received(expected, unit.payload(), unit.marker(), is_video);
        paired::check_delta(
            *previous,
            unit.rtp_timestamp(),
            expected.duration_us,
            if is_video { 90000 } else { 48000 },
        );
        *previous = Some(unit.rtp_timestamp());
        observations.push(paired::observation(
            expected,
            unit.payload(),
            unit.rtp_timestamp(),
            unit.marker(),
            if is_video { &video_label } else { &audio_label },
        ));
        // The leased ReceivedUnit drops each iteration. These bounded JSON
        // observations contain hashes/metadata, not retained media bodies.
    }
    json!({"video_units":vi,"audio_packets":ai,"units":observations})
}

async fn send_units(
    phase: &'static str,
    kind: &'static str,
    flow: &mut RealtimeFlow,
    units: &[Unit],
) {
    for (index, unit) in units.iter().enumerate() {
        diagnostic(
            phase,
            "send_before_local_write",
            json!({"kind":kind, "index":index,
            "bytes":unit.data.len(), "sha256":unit.sha256, "duration_us":unit.duration_us}),
        );
        flow.send(unit.duration_us, &unit.data).await.unwrap();
        diagnostic(
            phase,
            "local_write_complete_not_delivery",
            json!({"kind":kind, "index":index}),
        );
        tokio::time::sleep(Duration::from_micros(u64::from(unit.duration_us))).await;
    }
}

async fn batch(
    context: &Context,
    pipe: &mut InboundPipe,
    prefix: &str,
    phase: &'static str,
    video: &mut RealtimeFlow,
    audio: &mut RealtimeFlow,
) -> Value {
    diagnostic(
        phase,
        "phase_begin",
        json!({"expected_video_units":context.video.len(), "expected_audio_packets":context.audio.len()}),
    );
    let sending = async {
        let ((), ()) = tokio::join!(
            send_units(phase, "video", video, &context.video),
            send_units(phase, "audio", audio, &context.audio)
        );
    };
    let ((), received) = tokio::join!(sending, receive(context, pipe, prefix, phase, true, true));
    diagnostic(
        phase,
        "phase_complete",
        json!({"video_units":received["video_units"], "audio_packets":received["audio_packets"]}),
    );
    received
}

async fn audio_batch(
    context: &Context,
    pipe: &mut InboundPipe,
    prefix: &str,
    phase: &'static str,
    audio: &mut RealtimeFlow,
) -> Value {
    diagnostic(
        phase,
        "phase_begin",
        json!({"expected_video_units":0, "expected_audio_packets":context.audio.len()}),
    );
    let ((), received) = tokio::join!(
        send_units(phase, "audio", audio, &context.audio),
        receive(context, pipe, prefix, phase, false, true)
    );
    diagnostic(
        phase,
        "phase_complete",
        json!({"video_units":received["video_units"], "audio_packets":received["audio_packets"]}),
    );
    received
}

fn cleanup_result(result: allmystuff_mesh_control::realtime::Result<()>) -> &'static str {
    match result {
        Ok(()) => "acknowledged",
        Err(RealtimeError::CleanupUncertain) => {
            "cleanup_uncertain_after_intentional_session_replacement"
        }
        Err(error) => panic!("unexpected cleanup result: {error}"),
    }
}

#[test]
#[ignore = "requires checksum-pinned private pair, valid codec vectors and mesh-control-paired.py"]
fn paired_endpoint_ready() {
    let context = Context::load();
    run(paired::endpoint_ready(&context), Duration::from_secs(35));
}

#[test]
#[ignore = "requires checksum-pinned private pair, valid codec vectors and mesh-control-paired.py"]
fn isolated_realtime_pair() {
    let context = Context::load();
    run(
        async {
            let a_id = paired::ready(&context.a, &context.network).await;
            let b_id = paired::ready(&context.b, &context.network).await;
            assert_ne!(
                a_id, b_id,
                "each private daemon must own a distinct identity"
            );
            paired::connect(&context, &b_id).await;
            let mut a = context.session(&context.a).await;
            let mut b = context.session(&context.b).await;
            let old_generation = a.session.registration().generation();
            let mut inbound_b = b
                .session
                .open_inbound(&context.network, &a_id)
                .await
                .unwrap();
            let (mut video, video_in) = flows(
                &context,
                &mut a.session,
                &mut b.session,
                &a_id,
                &b_id,
                "forward",
                true,
            )
            .await;
            let (mut audio, audio_in) = flows(
                &context,
                &mut a.session,
                &mut b.session,
                &a_id,
                &b_id,
                "forward",
                false,
            )
            .await;
            let initial = batch(
                &context,
                &mut inbound_b,
                "forward",
                "initial_forward",
                &mut video,
                &mut audio,
            )
            .await;
            let initial_path = paired::selected_paths(&context, &a_id, &b_id).await;
            video.close().await.unwrap();
            video_in.close().await.unwrap();
            let audio_after_video_close = audio_batch(
                &context,
                &mut inbound_b,
                "forward",
                "audio_after_video_close",
                &mut audio,
            )
            .await;

            let mut unsupported = context.flow(&b_id, "unsupported", FlowDirection::Outbound, true);
            unsupported.encoding.clock_rate = 90001;
            let refusal_code = match a.session.open_flow(unsupported).await {
                Err(RealtimeError::Refused { code: Some(code) }) => code,
                Err(error) => {
                    panic!("expected source-coded unsupported encoding refusal, got {error}")
                }
                Ok(flow) => {
                    flow.close().await.unwrap();
                    panic!("unregistered codec family was admitted");
                }
            };
            assert_eq!(
                refusal_code, "provider_configuration_invalid",
                "exact registered-family refusal from db7818e"
            );

            let (mut pressure, pressure_in) = flows(
                &context,
                &mut a.session,
                &mut b.session,
                &a_id,
                &b_id,
                "pressure",
                true,
            )
            .await;
            let mut termination = pressure.termination();
            let oversized = vec![
                0_u8;
                usize::try_from(paired::number(&context.policy, "ipc_frame_bytes"))
                    .unwrap()
            ];
            let write_result = pressure
                .send(context.video[0].duration_us, &oversized)
                .await;
            assert!(
                !matches!(write_result, Err(RealtimeError::Timeout)),
                "a local timeout is not daemon refusal evidence"
            );
            let end = termination
                .wait(context.realtime_policy().io_timeout)
                .await
                .unwrap();
            assert!(
                matches!(end, PipeEnd::RemoteClosed),
                "require actual remote pipe termination; observed {end:?}"
            );
            assert!(
                a.session.registration().is_active(),
                "one refused binary frame must not revoke the event generation"
            );
            pressure.close().await.unwrap();
            pressure_in.close().await.unwrap();
            let audio_after_pressure = audio_batch(
                &context,
                &mut inbound_b,
                "forward",
                "audio_after_pressure",
                &mut audio,
            )
            .await;
            audio.close().await.unwrap();
            audio_in.close().await.unwrap();
            inbound_b.close().await.unwrap();

            let mut inbound_a = a
                .session
                .open_inbound(&context.network, &b_id)
                .await
                .unwrap();
            let (mut reverse_video, reverse_video_in) = flows(
                &context,
                &mut b.session,
                &mut a.session,
                &b_id,
                &a_id,
                "reverse",
                true,
            )
            .await;
            let (mut reverse_audio, reverse_audio_in) = flows(
                &context,
                &mut b.session,
                &mut a.session,
                &b_id,
                &a_id,
                "reverse",
                false,
            )
            .await;
            let reverse = batch(
                &context,
                &mut inbound_a,
                "reverse",
                "reverse",
                &mut reverse_video,
                &mut reverse_audio,
            )
            .await;
            // Poll the real idle read once, proving cancellation after it has taken
            // its socket, rather than dropping an unpolled future.
            {
                let mut pending = pin!(inbound_a.recv());
                std::future::poll_fn(|cx| match pending.as_mut().poll(cx) {
                    Poll::Pending => Poll::Ready(()),
                    Poll::Ready(_) => panic!("expected the drained real inbound stream to be idle"),
                })
                .await;
            }
            assert!(
                inbound_a.recv().await.is_err(),
                "cancelled framed read must not reuse its socket"
            );
            inbound_a.close().await.unwrap();
            reverse_video.close().await.unwrap();
            reverse_video_in.close().await.unwrap();
            reverse_audio.close().await.unwrap();
            reverse_audio_in.close().await.unwrap();

            let (mut stale, stale_in) = flows(
                &context,
                &mut a.session,
                &mut b.session,
                &a_id,
                &b_id,
                "reconnect",
                true,
            )
            .await;
            let mut stale_end = stale.termination();
            context.restart_marker();
            // Stable identity and explicit reconnect are read/issued only after B
            // has actually restarted; the controller publishes the complete marker.
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
            assert_eq!(paired::ready(&context.b, &context.network).await, b_id);
            let reconnected = paired::raw(
                &context.a,
                json!({"op":"network_reconnect", "network":context.network,
                                     "peer":b_id}),
                context.realtime_policy().operation_timeout,
            )
            .await;
            assert!(reconnected.ok, "one exact reconnect request must answer");
            paired::connect(&context, &b_id).await;
            // A successful local stale write is not acceptance; exact-session
            // refusal must be followed by observed pipe termination.
            let _stale_write = stale
                .send(context.video[0].duration_us, &context.video[0].data)
                .await;
            let stale_terminal = stale_end
                .wait(context.realtime_policy().operation_timeout)
                .await
                .unwrap();
            assert!(matches!(
                stale_terminal,
                PipeEnd::RemoteClosed | PipeEnd::SessionEnded
            ));

            let mut fresh_a = context.session(&Arc::clone(&context.a)).await;
            let mut fresh_b = context.session(&Arc::clone(&context.b)).await;
            assert_ne!(fresh_a.session.registration().generation(), old_generation);
            let mut fresh_inbound = fresh_b
                .session
                .open_inbound(&context.network, &a_id)
                .await
                .unwrap();
            let (mut new_video, new_video_in) = flows(
                &context,
                &mut fresh_a.session,
                &mut fresh_b.session,
                &a_id,
                &b_id,
                "reconnect",
                true,
            )
            .await;
            let (mut new_audio, new_audio_in) = flows(
                &context,
                &mut fresh_a.session,
                &mut fresh_b.session,
                &a_id,
                &b_id,
                "reconnect",
                false,
            )
            .await;
            assert!(
                stale
                    .send(context.video[0].duration_us, &context.video[0].data)
                    .await
                    .is_err(),
                "old local generation cannot write to its successor"
            );
            assert!(
                matches!(stale.close().await, Err(RealtimeError::StaleSession)),
                "old public handle must refuse before a protected close write"
            );
            assert!(
                matches!(stale_in.close().await, Err(RealtimeError::StaleSession)),
                "old inbound flow handle is also locally stale"
            );
            let old_a_close = cleanup_result(a.close().await);
            let old_b_close = cleanup_result(b.close().await);
            let after_restart = batch(
                &context,
                &mut fresh_inbound,
                "reconnect",
                "after_owned_peer_restart",
                &mut new_video,
                &mut new_audio,
            )
            .await;
            new_video.close().await.unwrap();
            new_video_in.close().await.unwrap();
            new_audio.close().await.unwrap();
            new_audio_in.close().await.unwrap();
            fresh_inbound.close().await.unwrap();
            let after_restart_path = paired::selected_paths(&context, &a_id, &b_id).await;
            fresh_a.close().await.unwrap();
            fresh_b.close().await.unwrap();
            paired::emit(json!({"scenario":"transport", "codec_decode":false,
            "initial_path":initial_path,"after_restart_path":after_restart_path,
            "initial":initial, "audio_after_video_close":audio_after_video_close,
            "audio_after_remote_frame_refusal":audio_after_pressure, "reverse":reverse,
            "after_owned_peer_restart":after_restart, "unsupported_encoding_refusal_code":refusal_code,
            "pressure":"owner_selected_daemon_frame_ceiling_remote_pipe_eof",
            "cancelled_polled_inbound":true, "generation_renewed":true,
            "old_flow_close":"locally_stale_not_native_retirement", "old_inbound_close":"locally_stale_not_native_retirement",
            "old_owner_a_close":old_a_close, "old_owner_b_close":old_b_close,
            "native_cleanup_proof":"owned daemon shutdown receipt, not event socket EOF"}));
        },
        context.timeout(),
    );
}

//! Public shape, disabled behavior and frozen AudioFrame wire literals.

#[cfg(feature = "codec")]
#[rustfmt::skip]
#[path = "baseline/audio_stub.rs"]
mod original;

#[test]
fn clock_frame_duration_and_capture_variants_keep_the_original_values() {
    assert_eq!(allmystuff_audio::OPUS_RATE, 48000);
    assert_eq!(allmystuff_audio::OPUS_FRAME_SAMPLES, 960);
    assert_eq!(allmystuff_audio::OPUS_FRAME_US, 20000);
    assert_eq!(format!("{:?}", allmystuff_audio::CaptureSource::Mic), "Mic");
    assert_eq!(
        format!("{:?}", allmystuff_audio::CaptureSource::System),
        "System"
    );
}

#[cfg(feature = "codec")]
mod codec_contracts {
    use super::original;
    use allmystuff_audio::{disabled, AudioFrame, CaptureSource};
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    fn pcm(value: &Value) -> Vec<i16> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_i64().unwrap() as i16)
            .collect()
    }

    #[test]
    fn disabled_encoder_refusal_keeps_the_original_text() {
        let expected = "audio capture is not built into this node (no `host` feature)";
        assert_eq!(original::OpusStream::new().err().unwrap(), expected);
        assert_eq!(disabled::OpusStream::new().err().unwrap(), expected);
        assert_eq!(original::OPUS_RATE, allmystuff_audio::OPUS_RATE);
        assert_eq!(
            original::OPUS_FRAME_SAMPLES,
            allmystuff_audio::OPUS_FRAME_SAMPLES
        );
        assert_eq!(original::OPUS_FRAME_US, allmystuff_audio::OPUS_FRAME_US);
    }

    #[test]
    fn explicitly_disabled_encoder_push_remains_inert() {
        let mut old = original::OpusStream {};
        let mut new = disabled::OpusStream {};
        for rate in [0, 24000, 48000] {
            old.push(&[1; 1920], rate, |_| panic!("original disabled emission"));
            new.push(&[1; 1920], rate, |_| panic!("extracted disabled emission"));
        }
    }

    struct DropNotice(Arc<AtomicUsize>);
    impl Drop for DropNotice {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn disabled_capture_drops_its_callback_without_invocation_or_resource_state() {
        let old = original::AudioBridge::new();
        let new = disabled::AudioBridge::default();
        let drops = Arc::new(AtomicUsize::new(0));
        for (old_source, new_source) in [
            (original::CaptureSource::Mic, CaptureSource::Mic),
            (original::CaptureSource::System, CaptureSource::System),
        ] {
            let notice = DropNotice(drops.clone());
            old.start_capture("r".into(), old_source, move |_, _| {
                let _ = &notice;
                panic!("original disabled callback");
            });
            let notice = DropNotice(drops.clone());
            new.start_capture("r".into(), new_source, move |_, _| {
                let _ = &notice;
                panic!("extracted disabled callback");
            });
            assert!(!old.is_running("r"));
            assert!(!new.is_running("r"));
        }
        assert_eq!(drops.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn explicit_disabled_bridge_stays_inert_even_with_io_feature_unification() {
        let old = original::AudioBridge::default();
        let new = disabled::AudioBridge::new();
        for id in ["", "r", "r\0é"] {
            old.start_playback(id.into());
            new.start_playback(id.into());
            let frame = AudioFrame::new("different", u64::MAX, 0, 0, vec![i16::MIN, i16::MAX]);
            old.feed(id, &frame);
            new.feed(id, &frame);
            assert!(!old.is_running(id));
            assert!(!new.is_running(id));
            old.stop(id);
            new.stop(id);
        }
        old.stop_all();
        new.stop_all();
    }

    type LogEvents = Arc<Mutex<Vec<(String, String, String)>>>;

    #[derive(Clone)]
    struct Events(LogEvents);

    #[derive(Default)]
    struct Message(String);

    impl tracing::field::Visit for Message {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0 = format!("{value:?}");
            }
        }
    }

    impl tracing::Subscriber for Events {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut message = Message::default();
            event.record(&mut message);
            self.0.lock().unwrap().push((
                event.metadata().target().to_string(),
                event.metadata().level().to_string(),
                message.0,
            ));
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[test]
    fn disabled_logs_keep_the_original_messages_levels_and_node_target() {
        let old_events = Arc::new(Mutex::new(Vec::new()));
        let new_events = Arc::new(Mutex::new(Vec::new()));
        tracing::subscriber::with_default(Events(old_events.clone()), || {
            let old = original::AudioBridge::new();
            old.start_capture("r".into(), original::CaptureSource::Mic, |_, _| {});
            old.start_playback("r".into());
        });
        tracing::subscriber::with_default(Events(new_events.clone()), || {
            let new = disabled::AudioBridge::new();
            new.start_capture("r".into(), CaptureSource::Mic, |_, _| {});
            new.start_playback("r".into());
        });
        let old = old_events.lock().unwrap();
        let new = new_events.lock().unwrap();
        assert_eq!(new.len(), 2);
        assert_eq!(old.len(), 2);
        let expected = [
            "audio capture for r unavailable: capture-less build",
            "audio playback for r unavailable: capture-less build",
        ];
        for (index, ((_, old_level, old_message), (target, level, message))) in
            old.iter().zip(new.iter()).enumerate()
        {
            assert_eq!(old_level, "INFO");
            assert_eq!(level, old_level);
            assert_eq!(message, old_message);
            assert_eq!(message, expected[index]);
            assert_eq!(target, "allmystuff_node::audio");
        }
    }

    #[test]
    fn frame_reexport_retains_the_canonical_session_type() {
        fn session_consumer(frame: allmystuff_session::AudioFrame) -> AudioFrame {
            frame
        }
        let frame = AudioFrame::new("r", 1, 48000, 1, vec![3]);
        assert_eq!(session_consumer(frame.clone()), frame);
    }

    #[test]
    fn frame_json_matches_independent_little_endian_and_field_order_literals() {
        let rows: Value = serde_json::from_str(include_str!("baseline/wire_vectors.json")).unwrap();
        for row in rows["serialized_frames"].as_array().unwrap() {
            let value = &row["frame"];
            let frame = AudioFrame::new(
                value["route"].as_str().unwrap(),
                value["seq"].as_u64().unwrap(),
                value["sample_rate"].as_u64().unwrap() as u32,
                value["channels"].as_u64().unwrap() as u16,
                pcm(&value["pcm"]),
            );
            let text = row["json"].as_str().unwrap();
            assert_eq!(serde_json::to_string(&frame).unwrap(), text);
            assert_eq!(serde_json::from_str::<AudioFrame>(text).unwrap(), frame);
            assert_eq!(
                frame.frame_count(),
                row["frame_count"].as_u64().unwrap() as usize
            );
        }
    }

    #[test]
    fn frame_decode_retains_odd_byte_discard_and_zero_channel_shape() {
        let rows: Value = serde_json::from_str(include_str!("baseline/wire_vectors.json")).unwrap();
        for row in rows["odd_byte_inputs"].as_array().unwrap() {
            let frame: AudioFrame = serde_json::from_value(json!({
                "route": "r", "seq": 0, "sample_rate": 0, "channels": 0,
                "pcm": row["base64"], "unknown": "ignored"
            }))
            .unwrap();
            assert_eq!(frame.pcm, pcm(&row["expected_pcm"]));
            assert_eq!(frame.frame_count(), frame.pcm.len());
        }
    }

    #[test]
    fn frame_requires_original_fields_and_rejects_wrong_pcm_representation() {
        let full = json!({"route": "r", "seq": 0, "sample_rate": 48000, "channels": 1, "pcm": ""});
        for field in ["route", "seq", "sample_rate", "channels", "pcm"] {
            let mut missing = full.clone();
            missing.as_object_mut().unwrap().remove(field);
            let error = serde_json::from_value::<AudioFrame>(missing)
                .unwrap_err()
                .to_string();
            assert_eq!(error, format!("missing field `{field}`"));
        }
        for wrong in [json!([1, 2]), json!(false), json!(null)] {
            let mut value = full.clone();
            value["pcm"] = wrong;
            let error = serde_json::from_value::<AudioFrame>(value)
                .unwrap_err()
                .to_string();
            assert!(error.contains("expected a string"), "{error}");
        }
        for invalid in ["!", "A", "A===", "AQ"] {
            let mut value = full.clone();
            value["pcm"] = json!(invalid);
            assert!(
                serde_json::from_value::<AudioFrame>(value).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn untagged_and_nonstring_media_tags_keep_audio_demux_behavior() {
        use allmystuff_session::MediaPayload;
        let frame = AudioFrame::new("r", 7, 0, 0, vec![1, -1, 2]);
        let full = serde_json::to_value(&frame).unwrap();
        for tag in [None, Some(json!(null)), Some(json!(7))] {
            let mut value = full.clone();
            if let Some(tag) = tag {
                value["t"] = tag;
            }
            assert!(
                matches!(MediaPayload::decode(value), Some(MediaPayload::Audio(decoded)) if decoded == frame)
            );
        }
        let mut unknown = full;
        unknown["t"] = json!("unknown");
        assert!(MediaPayload::decode(unknown).is_none());
    }
}

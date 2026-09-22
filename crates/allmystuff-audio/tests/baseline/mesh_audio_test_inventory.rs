    #[test]
    fn advertised_capabilities_audio_requires_audio_io() {
        let full = advertised_capabilities_fixture();
        let mut filtered = full.clone();
        Mesh::filter_advertised_capabilities(&mut filtered);
        let audio: Vec<_> = filtered
            .iter()
            .filter(|c| c.media == MediaKind::Audio)
            .collect();
        if cfg!(feature = "audio-io") {
            assert_eq!(
                audio
                    .iter()
                    .map(|c| (c.id.as_str(), c.flow))
                    .collect::<Vec<_>>(),
                vec![
                    ("fixture:system-audio", Flow::Duplex),
                    ("fixture:mic:1", Flow::Source),
                    ("fixture:spk:1", Flow::Sink),
                ]
            );
            assert_eq!(
                audio,
                full.iter()
                    .filter(|c| c.media == MediaKind::Audio)
                    .collect::<Vec<_>>()
            );
        } else {
            assert!(audio.is_empty(), "the audio stub cannot capture or play");
        }
    }
    #[test]
    fn system_audio_routes_capture_the_machines_own_output() {
        // The synthetic `system-audio` capability = "what this machine
        // plays" — its routes loop the output back…
        let system = term_route("me:system-audio", "them:system-audio", MediaKind::Audio);
        assert_eq!(audio_capture_source(&system), CaptureSource::System);

        // …while a scanned input device (and anything unrecognized,
        // including a bare node id) captures the mic, exactly as before.
        let mic = term_route("me:mic:array-1", "them:system-audio", MediaKind::Audio);
        assert_eq!(audio_capture_source(&mic), CaptureSource::Mic);
        let bare = term_route("me", "them:system-audio", MediaKind::Audio);
        assert_eq!(audio_capture_source(&bare), CaptureSource::Mic);
    }

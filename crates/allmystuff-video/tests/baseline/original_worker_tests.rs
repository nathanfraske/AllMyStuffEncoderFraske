#[cfg(test)]
mod tests {
    use super::*;

    /// The codec sniff's three-way branch, including the AV1 OBU seam:
    /// H.264/HEVC key units are detected from their start-code-led NAL
    /// byte, an AV1 sequence-header OBU (start-code-less) is detected as
    /// AV1, and — critically — an H.264/HEVC stream is NEVER misread as
    /// AV1 (the OBU branch only fires when no start code exists at all).
    #[test]
    fn sniff_routes_h264_hevc_and_av1_obu() {
        // H.264 IDR (00 00 01 65): type 5.
        assert_eq!(sniff_codec(&[0, 0, 1, 0x65, 0x88]), Some(AuCodec::H264));
        // H.264 SPS (67), PPS (68).
        assert_eq!(sniff_codec(&[0, 0, 1, 0x67, 0x42]), Some(AuCodec::H264));
        // HEVC VPS (0x40), SPS (0x42), PPS (0x44).
        assert_eq!(sniff_codec(&[0, 0, 1, 0x40, 0x01]), Some(AuCodec::Hevc));
        assert_eq!(sniff_codec(&[0, 0, 1, 0x42, 0x01]), Some(AuCodec::Hevc));
        // Prefix metadata does not obscure the parameter set that actually
        // identifies the stream.
        assert_eq!(
            sniff_codec(&[0, 0, 1, 0x09, 0xf0, 0, 0, 1, 0x06, 1, 0, 0, 1, 0x67, 0x42]),
            Some(AuCodec::H264)
        );
        assert_eq!(
            sniff_codec(&[0, 0, 1, 0x46, 0x01, 0, 0, 1, 0x4e, 0x01, 0, 0, 1, 0x40, 0x01]),
            Some(AuCodec::Hevc)
        );
        // An H.264 delta P-slice (0x41) leads no key AU → None (codec
        // carries from the key) and NEVER trips the AV1 branch.
        assert_eq!(sniff_codec(&[0, 0, 1, 0x41, 0x9a]), None);
        // AV1 sequence-header OBU: has_size set, type 1 → obu byte 0x0a
        // (000 0001 0 1: type=1, ext=0, has_size=1), leb128 size 3, payload.
        let seq = [0x0a, 0x03, 0x00, 0x00, 0x00];
        assert_eq!(sniff_codec(&seq), Some(AuCodec::Av1));
        // AV1 temporal delimiter (type 2, obu 0x12) then seq header.
        let td_seq = [0x12, 0x00, 0x0a, 0x03, 0x00, 0x00, 0x00];
        assert_eq!(sniff_codec(&td_seq), Some(AuCodec::Av1));
        // An AV1 delta (a lone frame OBU type 6, obu 0x32) is not a key —
        // no seq header → None, codec carries from the key.
        assert_eq!(sniff_codec(&[0x32, 0x02, 0x10, 0x00]), None);
        // Random start-code-less bytes that aren't a valid OBU opening
        // stay None (the forbidden bit / wrong type guards).
        assert_eq!(sniff_codec(&[0xff, 0xff, 0xff, 0xff]), None);
    }

    /// A decoder thread can return after a callback failure or panic. Its
    /// sender used to remain in the route map forever, making every future
    /// display feed hit `Disconnected` without recreating the decoder.
    #[test]
    fn disconnected_decoder_route_restarts_on_next_feed() {
        let bridge = DecodeBridge::new();
        let (dead_tx, dead_rx) = mpsc::sync_channel(1);
        drop(dead_rx);
        bridge.routes.lock().insert(
            "dead-route".into(),
            RouteDecode {
                tx: dead_tx,
                preference: DecoderPreference::Automatic,
                need_key: Arc::new(AtomicBool::new(false)),
                stop: Arc::new(AtomicBool::new(false)),
                thread: None,
            },
        );

        let (glitch_tx, glitch_rx) = mpsc::channel();
        bridge.feed(
            "dead-route",
            DecoderPreference::Automatic,
            Au {
                ts_us: 1,
                key: false,
                data: vec![0, 0, 1, 0x41, 0x9a],
            },
            |_| {},
            move |lost| {
                let _ = glitch_tx.send(lost);
            },
        );

        assert_eq!(
            glitch_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            None,
            "a restarted delta stream asks for a fresh key"
        );
        assert!(bridge.is_running("dead-route"));
        bridge.stop("dead-route");
        assert!(!bridge.is_running("dead-route"));
    }

    /// Enabling native decode while a route is already flowing commonly makes
    /// the first AU a delta. The decoder deliberately waits for a key, so the
    /// bridge must actively request one even though this is a first start (not
    /// merely resurrection of a disconnected worker). This is essential for
    /// Game/GDR streams, whose normal GOP has no periodic IDR.
    #[test]
    fn fresh_decoder_started_on_delta_requests_key() {
        let bridge = DecodeBridge::new();
        let (glitch_tx, glitch_rx) = mpsc::channel();
        bridge.feed(
            "fresh-delta-route",
            DecoderPreference::Automatic,
            Au {
                ts_us: 1,
                key: false,
                data: vec![0, 0, 1, 0x41, 0x9a],
            },
            |_| {},
            move |lost| {
                let _ = glitch_tx.send(lost);
            },
        );

        assert_eq!(
            glitch_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            None,
            "a newly-created decoder starting on a delta asks for a fresh key"
        );
        assert!(bridge.is_running("fresh-delta-route"));
        bridge.stop("fresh-delta-route");
    }

    #[test]
    fn local_decoder_preference_is_strict_and_defaults_to_automatic() {
        assert_eq!(
            DecoderPreference::parse(None).unwrap(),
            DecoderPreference::Automatic
        );
        assert_eq!(
            DecoderPreference::parse(Some("automatic")).unwrap(),
            DecoderPreference::Automatic
        );
        assert_eq!(
            DecoderPreference::parse(Some("software")).unwrap(),
            DecoderPreference::Software
        );
        assert!(DecoderPreference::Software.requires_software());
        assert!(DecoderPreference::parse(Some("nvdec")).is_err());
    }

    #[test]
    fn changing_local_decoder_preference_replaces_the_route_worker() {
        let bridge = DecodeBridge::new();
        let delta = || Au {
            ts_us: 1,
            key: false,
            data: vec![0, 0, 0, 1, 0x41, 0x9a],
        };

        bridge.feed(
            "preference-route",
            DecoderPreference::Automatic,
            delta(),
            |_| {},
            |_| {},
        );
        assert_eq!(
            bridge
                .routes
                .lock()
                .get("preference-route")
                .map(|route| route.preference),
            Some(DecoderPreference::Automatic)
        );

        bridge.feed(
            "preference-route",
            DecoderPreference::Software,
            delta(),
            |_| {},
            |_| {},
        );
        assert_eq!(
            bridge
                .routes
                .lock()
                .get("preference-route")
                .map(|route| route.preference),
            Some(DecoderPreference::Software)
        );
        bridge.stop("preference-route");
    }

    #[cfg(all(windows, feature = "host"))]
    #[test]
    fn zero_output_failure_demotes_h264_route_until_restart() {
        let mut policy = H264RuntimePolicy::default();
        assert!(!policy.requires_software());
        policy.note_zero_output_failure();
        assert!(
            policy.requires_software(),
            "a decoder that accepts the zero-output limit must not reopen NVDEC"
        );
        policy.reset();
        assert!(
            !policy.requires_software(),
            "a new route generation gets the automatic hardware ladder again"
        );
    }

    #[cfg(all(windows, feature = "host"))]
    #[test]
    fn repeated_nvdec_delta_failure_demotes_until_route_restart() {
        let mut policy = H264RuntimePolicy::default();
        assert!(!policy.requires_software());
        assert_eq!(policy.note_delta_failure(), 1);
        assert!(
            !policy.requires_software(),
            "one fresh NVDEC retry is allowed"
        );
        policy.note_delta_success();
        assert!(
            !policy.requires_software(),
            "a healthy dependent picture clears the strike"
        );
        assert_eq!(policy.note_delta_failure(), 1);
        assert_eq!(policy.note_delta_failure(), 2);
        assert!(policy.requires_software());
        policy.reset();
        assert!(
            !policy.requires_software(),
            "a new route gets the hardware ladder again"
        );
    }

    #[cfg(all(windows, feature = "host"))]
    #[test]
    fn hevc_runtime_failure_demotes_unless_nvdec_is_pinned() {
        let mut policy = HevcRuntimePolicy::default();
        assert!(policy.demote_from_nvdec(false));
        assert!(policy.prefer_dxva);

        policy.reset();
        assert!(!policy.prefer_dxva);

        assert!(!policy.demote_from_nvdec(true));
        assert!(
            !policy.prefer_dxva,
            "an explicit NVDEC A/B pin must preserve the selected rung"
        );
    }

    /// Encode a couple of frames with the encoder the send side uses, feed
    /// them through the bridge, and check real RGBA frames come out — the
    /// whole loop the two ends of a route rely on, no hardware involved.
    #[test]
    fn decodes_what_the_encoder_produces() {
        use openh264::encoder::Encoder;
        use openh264::formats::{RgbSliceU8, YUVBuffer};

        let mut enc = Encoder::with_api_config(
            openh264::OpenH264API::from_source(),
            openh264::encoder::EncoderConfig::new(),
        )
        .expect("encoder");
        let bridge = DecodeBridge::new();
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();

        for shade in [40u8, 200u8] {
            let rgb = vec![shade; 64 * 64 * 3];
            let yuv = YUVBuffer::from_rgb8_source(RgbSliceU8::new(&rgb, (64, 64)));
            let stream = enc.encode(&yuv).expect("encode");
            let data = stream.to_vec();
            if data.is_empty() {
                continue;
            }
            let tx = tx.clone();
            bridge.feed(
                "r1",
                DecoderPreference::Automatic,
                Au {
                    ts_us: 0,
                    key: shade == 40, // first unit out of a fresh encoder is the IDR
                    data,
                },
                move |packet| {
                    let _ = tx.send(packet);
                },
                |_| {},
            );
        }

        let packet = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("a decoded frame");
        assert_eq!(packet[0], 3, "kind 3 = raw RGBA");
        let w = u32::from_le_bytes(packet[4..8].try_into().unwrap());
        let h = u32::from_le_bytes(packet[8..12].try_into().unwrap());
        assert_eq!((w, h), (64, 64));
        assert_eq!(
            packet.len(),
            crate::mesh::VIDEO_IPC_HEADER_LEN + 64 * 64 * 4
        );
        // Alpha is opaque all the way through (the canvas blits it as-is).
        assert_eq!(packet[crate::mesh::VIDEO_IPC_HEADER_LEN + 3], 255);
        assert!(bridge.is_running("r1"));
        bridge.stop("r1");
        assert!(!bridge.is_running("r1"));
    }

    /// A retune can change display geometry without changing the route ID.
    /// NVDEC deliberately rejects that on an existing session; the bridge must
    /// rebuild hardware and retry the same key instead of permanently
    /// demoting or waiting for another IDR.
    #[test]
    fn h264_route_rebuilds_across_resolution_change() {
        use openh264::encoder::Encoder;
        use openh264::formats::{RgbSliceU8, YUVBuffer};

        let bridge = DecodeBridge::new();
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        for (seq, (w, h)) in [(1u64, (64usize, 64usize)), (2, (96usize, 80usize))] {
            let mut enc = Encoder::with_api_config(
                openh264::OpenH264API::from_source(),
                openh264::encoder::EncoderConfig::new(),
            )
            .expect("encoder");
            let rgb = vec![(seq * 70) as u8; w * h * 3];
            let yuv = YUVBuffer::from_rgb8_source(RgbSliceU8::new(&rgb, (w, h)));
            let data = enc.encode(&yuv).expect("encode key").to_vec();
            let sink = tx.clone();
            bridge.feed(
                "resize-route",
                DecoderPreference::Automatic,
                Au {
                    ts_us: seq * 20_000,
                    key: true,
                    data,
                },
                move |packet| {
                    let _ = sink.send(packet);
                },
                |_| {},
            );
        }

        let mut dims = Vec::new();
        for _ in 0..2 {
            let packet = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("frame after each geometry");
            dims.push((
                u32::from_le_bytes(packet[4..8].try_into().unwrap()),
                u32::from_le_bytes(packet[8..12].try_into().unwrap()),
            ));
        }
        assert_eq!(dims, [(64, 64), (96, 80)]);
        bridge.stop("resize-route");
    }

    /// HEVC through the whole bridge on the real hardware rungs: NVENC
    /// lossless AUs fed with `key: false` on purpose — the daemon's key
    /// flag is H.264-shaped and must never be load-bearing for HEVC; the
    /// sniff carries the entry. 640×360 codes with CTB padding (384
    /// rows), so the display crop is exercised too. Skips (passing)
    /// without the NVIDIA rungs.
    #[cfg(all(windows, feature = "host"))]
    #[test]
    fn hevc_stream_decodes_through_bridge() {
        let paced = crate::video::paced_slices_enabled();
        let (w, h) = (640u32, 360u32);
        let (wu, hu) = (w as usize, h as usize);
        let mut gpu = match crate::gpu_pipeline::GpuConvert::new(w, h, w, h) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: GPU convert unavailable: {e}");
                return;
            }
        };
        let mut enc =
            match crate::nvenc::NvencH264::open_lossless_hevc_on_device(&gpu.device(), w, h, 60) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("SKIP: NVENC HEVC unavailable: {e}");
                    return;
                }
            };
        // Availability probe, held open through the test: paying cuInit
        // here keeps the bridge thread's lazy open fast, the same warm
        // state a live viewer reaches after its first session.
        let _warm = match crate::nvdec::NvdecHevc::open() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("SKIP: NVDEC unavailable: {e}");
                return;
            }
        };
        let bridge = DecodeBridge::new();
        let got = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let mut bgra = vec![0u8; wu * hu * 4];
        let tex = gpu.bgra_texture_from(&bgra, w, h).expect("tex");
        let mut saw_multi = false;
        for i in 0..30u64 {
            for (j, v) in bgra.iter_mut().enumerate() {
                *v = ((j as u64).wrapping_add(i * 11) % 251) as u8;
            }
            gpu.update_bgra(&tex, &bgra, w, h);
            let (slot, nv12) = gpu.convert(&tex).expect("convert").expect("slot");
            // Periodic IDRs, like the live stream's adaptive cadence: if
            // the bridge's bounded queue ever dumps (slow first open),
            // the stream carries its own re-entry points.
            let out = enc
                .encode_texture(&nv12, i.is_multiple_of(10))
                .expect("encode");
            gpu.release(slot);
            for (d, _) in out.units {
                let chunks = crate::video::split_annexb_paced(&d, crate::video::PACE_SLICE_BYTES);
                saw_multi |= chunks.len() > 1;
                // Mesh ingress owns paced-fragment validation/reassembly; the
                // native bridge's contract is one complete AU again.
                let sink = got.clone();
                bridge.feed(
                    "route-hevc",
                    DecoderPreference::Automatic,
                    Au {
                        ts_us: i * 16_667,
                        key: false,
                        data: d,
                    },
                    move |p| sink.lock().push(p),
                    |_| {},
                );
            }
            // Stay under the bounded queue — the decoder runs ~1 ms/frame.
            std::thread::sleep(Duration::from_millis(4));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while got.lock().len() < 30 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        bridge.stop("route-hevc");
        let packets = got.lock();
        if paced {
            assert!(saw_multi, "NVENC emitted a paced multi-sample HEVC AU");
        }
        // ≥18: allows one dumped queue window at start-up (≤12 units)
        // healed by the next periodic key — the bridge's designed
        // recovery — while still proving a sustained decoded stream.
        assert!(packets.len() >= 18, "decoded packets: {}", packets.len());
        let expect = crate::mesh::VIDEO_IPC_HEADER_LEN + wu * hu * 4;
        assert!(packets.iter().all(|p| p.len() == expect), "packet shape");
        let pw = u32::from_le_bytes(packets[0][4..8].try_into().unwrap());
        let ph = u32::from_le_bytes(packets[0][8..12].try_into().unwrap());
        assert_eq!((pw, ph), (w, h), "display-cropped dimensions");
        let body = &packets[5][crate::mesh::VIDEO_IPC_HEADER_LEN..];
        assert!(body.iter().any(|&b| b > 8), "pixels arrived");
    }
}

    /// The OpenH264-specific incremental-feed property on a REAL bitstream: an encoder
    /// with a slice cap emits multi-slice units, the splitter cuts them, and
    /// OpenH264 can consume them incrementally. Production ingress does not
    /// depend on that decoder quirk: the v1 marker/count contract reassembles
    /// every decoder's input to a complete access unit in `mesh.rs`.
    #[test]
    fn openh264_accepts_paced_slice_chunks_incrementally() {
        use openh264::encoder::{
            BitRate, Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType,
        };
        let (w, h) = (640usize, 480usize);
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(8_000_000))
            .max_frame_rate(FrameRate::from_hz(30.0))
            .max_slice_len(4 * 1024);
        let mut enc =
            Encoder::with_api_config(openh264::OpenH264API::from_source(), config).expect("enc");
        let mut dec = openh264::decoder::Decoder::with_api_config(
            openh264::OpenH264API::from_source(),
            openh264::decoder::DecoderConfig::new(),
        )
        .expect("dec");
        let mut yuv = vec![128u8; w * h + 2 * ((w / 2) * (h / 2))];
        let mut saw_multi = false;
        let mut decoded = 0u32;
        let mut on_last_chunk = 0u32;
        for i in 0..10u32 {
            // Encodable-but-busy content (marching stripes + texture) so
            // rate control never frame-skips, while slices still fill to
            // the cap. (Pure noise at this bitrate makes openh264 skip
            // alternate frames entirely — a rate-control artifact that
            // says nothing about chunked feeding.)
            for (j, v) in yuv[..w * h].iter_mut().enumerate() {
                let row = j / w;
                let stripe = (row as u32 + i * 3) % 32 < 16;
                let texture = ((j as u32).wrapping_mul(31) >> 3) % 48;
                *v = if stripe {
                    170 + (texture as u8 / 2)
                } else {
                    40 + texture as u8
                };
            }
            let mut au = enc
                .encode(&I420Frame { buf: &yuv, w, h })
                .expect("encode")
                .to_vec();
            // Model a rolling update: an older receiver does not strip the
            // identity SEI and gives it to OpenH264. Being valid codec metadata
            // must keep that path decodable.
            crate::video_wire::insert_au_identity_marker(
                &mut au,
                au_identity(u64::from(i), crate::video_wire::AuRecovery::Reset),
                false,
            );
            let chunks = split_annexb_paced(&au, 4 * 1024);
            let rebuilt: Vec<u8> = chunks
                .iter()
                .flat_map(|r| au[r.clone()].iter().copied())
                .collect();
            assert_eq!(rebuilt, au, "real-bitstream partition is byte-exact");
            if chunks.len() > 1 {
                saw_multi = true;
            }
            let last = chunks.len() - 1;
            for (ci, r) in chunks.into_iter().enumerate() {
                if dec
                    .decode(&au[r])
                    .expect("each paced chunk decodes cleanly")
                    .is_some()
                {
                    decoded += 1;
                    if ci == last {
                        on_last_chunk += 1;
                    }
                }
            }
        }
        assert!(saw_multi, "the slice cap produced multi-chunk units");
        assert!(
            decoded >= 9,
            "pictures completed across chunk-by-chunk feeding ({decoded}/10)"
        );
        // The zero-added-latency property: openh264 completes a picture by
        // macroblock accounting, so it surfaces on the SAME frame's final
        // chunk — never held for the next AU. This is what lets the live
        // viewer decode paced chunks as they arrive, no coalescing stage.
        assert!(
            on_last_chunk >= decoded.saturating_sub(1),
            "pictures surface on their own frame's last chunk ({on_last_chunk}/{decoded})"
        );
    }

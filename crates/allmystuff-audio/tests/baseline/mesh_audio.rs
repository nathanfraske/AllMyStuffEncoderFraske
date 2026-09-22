/// One captured-audio packet headed for the forwarder, in whichever
/// shape its route negotiated.
enum AudioOut {
    /// A PCM frame for `CHANNEL_MEDIA` — the floor every peer speaks.
    Channel(String, AudioFrame),
    /// One encoded Opus frame for the daemon's audio track lane.
    Lane {
        peer: String,
        route: String,
        data: Vec<u8>,
    },
}
        // Shallow queues both: at most a few frames in flight, so a slow
        // link sheds load by dropping captures rather than growing latency.
        // Audio's 8 buffers are ~160 ms of slack.
        let (audio_out, audio_rx) = mpsc::channel::<AudioOut>(8);
        let (video_out, video_rx) = mpsc::channel::<VideoOut>(4);
    /// Spawn the media forwarders that drain captured frames out to peers on
    /// the media channel, both bounded (see the field docs). Send failures are
    /// *surfaced* (rate-limited): a silently-dying media plane is exactly the
    /// "connected but nothing arrives" mystery.
    ///
    /// Called from [`Mesh::start`] rather than [`Mesh::new`] so the tasks land
    /// on the runtime `start` runs on — `new` is built in the GUI's sync Tauri
    /// `setup`, where `tokio::spawn` would panic with "no reactor running".
    /// Idempotent: the receivers are taken once, so a second call is a no-op.
    fn spawn_media_forwarders(self: &Arc<Self>) {
        if let Some(mut audio_rx) = self.audio_rx.lock().take() {
            let mesh = self.clone();
            crate::spawn(async move {
                let mut last_warn = std::time::Instant::now() - WARN_EVERY;
                while let Some(out) = audio_rx.recv().await {
                    let (peer, result) = match out {
                        AudioOut::Channel(peer, frame) => {
                            let Ok(payload) = serde_json::to_value(&frame) else {
                                continue;
                            };
                            let r = mesh.send_media_value(&peer, payload).await;
                            (peer, r)
                        }
                        AudioOut::Lane { peer, route, data } => {
                            // Same lane discipline as video: drop rather than
                            // ship on lane 0 when the route has no current lane
                            // (torn down, or past the audio lane pool), which
                            // would otherwise play one stream's audio on
                            // another's route.
                            match mesh.audio_lane(&route, &peer, true) {
                                Some(lane) => {
                                    let r = mesh.send_audio_track(&peer, &route, lane, data).await;
                                    (peer, r)
                                }
                                None => {
                                    if mesh.diag_ok(&format!("nolane-a:{route}")) {
                                        tracing::debug!(
                                            "no audio lane for {route} right now; dropping Opus frame"
                                        );
                                    }
                                    (peer, Ok(()))
                                }
                            }
                        }
                    };
                    if let Err(e) = result {
                        if last_warn.elapsed() >= WARN_EVERY {
                            last_warn = std::time::Instant::now();
                            tracing::warn!("audio frame to {} failed: {e}", short_id(&peer));
                        }
                    }
                }
            });
    /// Send one encoded Opus frame to `peer` over the daemon's audio track
    /// lane — binary media pipe when supported, else legacy base64.
    async fn send_audio_track(
        &self,
        peer: &str,
        route_id: &str,
        lane: u8,
        data: Vec<u8>,
    ) -> Result<(), String> {
        let Some(network) = self.network_for_route(route_id, peer) else {
            return Err("no shared network".into());
        };
        let result = if self.daemon_media_pipes.load(Ordering::SeqCst) {
            self.media_audio_track_pipe
                .send_audio(
                    &network,
                    pubkey_part(peer),
                    lane,
                    crate::audio::OPUS_FRAME_US,
                    &data,
                )
                .await
                .map_err(|e| e.to_string())
        } else {
            use base64::Engine as _;
            self.media_pipe
                .send(&Request::AudioSend {
                    network: network.clone(),
                    peer: pubkey_part(peer).to_string(),
                    stream: lane,
                    duration_us: crate::audio::OPUS_FRAME_US,
                    data: base64::engine::general_purpose::STANDARD.encode(&data),
                })
                .await
                .map_err(|e| e.to_string())
        };
        if result.is_err() {
            self.route_networks
                .lock()
                .release_if_network(route_id, &network);
        }
        result
    }
            CHANNEL_MEDIA => {
                let Some(media) = MediaPayload::decode(payload) else {
                    return;
                };
                match media {
                    MediaPayload::Audio(frame) => self.audio.feed(&frame.route, &frame),
    /// Because this now touches only *this* route's decoder, it is safe on
    /// a route that is still live — the positional close is what used to
    /// make that hazardous (it would have hit the top-ranked neighbour's
    /// lane, not this route's).
    fn release_audio_decoder(&self, route_id: &str) {
        self.audio_decoders.lock().remove(route_id);
    }

    /// One Opus frame arrived on a peer's audio lane `stream`. It belongs
    /// to whichever of our routes maps to that lane (the lane-th Opus route
    /// from this peer in sorted order — [`Self::audio_route_for_lane`]),
    /// gated exactly like every other media frame (route live, sinks here,
    /// sender is the route's peer) — then decodes straight into the
    /// route's playback ring.
    fn handle_audio_inbound(self: &Arc<Self>, from: &str, stream: u8, data: Vec<u8>) {
        let Some(route_id) = self.audio_route_for_lane(from, stream) else {
            // The audio twin of the video lane's "no route for it" warn
            // (rate-limited the same way): Opus arriving with nowhere to
            // decode it is the caller-hears-nothing drop, and it used to be
            // a DEBUG whisper while the room sat silent.
            if self.diag_ok(&format!("audio-lane:{}:{stream}", pubkey_part(from))) {
                tracing::warn!(
                    "Opus frames arriving from {} on lane {stream} but no route maps to it — dropped (caller hears nothing)",
                    short_id(from)
                );
            }
            self.nack_dead_lane(from, "audio", stream);
            return;
        };
        self.clear_dead_lane(from, "audio", stream);
        if !self.inbound_media_ok(&route_id, from, MediaKind::Audio) {
            tracing::debug!("audio frame for {route_id} refused (route not live here)");
            self.nack_dead_route(from, &route_id);
            return;
        }
        // Up to 120 ms per packet is legal Opus; ours are 20 ms.
        let mut pcm = vec![0i16; crate::audio::OPUS_FRAME_SAMPLES * 6];
        let decoded = {
            let mut decoders = self.audio_decoders.lock();
            let dec = match decoders.entry(route_id.clone()) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(v) => {
                    match opus::Decoder::new(crate::audio::OPUS_RATE, opus::Channels::Mono) {
                        Ok(d) => v.insert(d),
                        Err(e) => {
                            tracing::warn!("opus decoder for {route_id} failed: {e}");
                            return;
                        }
                    }
                }
            };
            match dec.decode(&data, &mut pcm, false) {
                Ok(n) => n,
                Err(e) => {
                    // One bad frame costs 20 ms; the next stands alone.
                    tracing::debug!("opus decode for {route_id} failed: {e}");
                    return;
                }
            }
        };
        pcm.truncate(decoded);
        let frame = AudioFrame::new(route_id.clone(), 0, crate::audio::OPUS_RATE, 1, pcm);
        self.audio.feed(&route_id, &frame);
    }
            self.route_networks.lock().release(&route_id);
            return Ok(());
        }
        if msg.is_some() {
            tracing::info!("local route teardown committing for {route_id}");
        }
        self.audio.stop(&route_id);
        // Cancel any bounded H.264 admission wait before VideoBridge::stop
        // joins the capture thread. Lane ownership remains intact until the
        // normal release below; this is only the generation fence.
        self.video_route_generations.lock().retire(&route_id);
        self.video.stop(&route_id);
        self.video_watchers.lock().remove(&route_id);
        self.release_video_lanes(&route_id);
        self.release_audio_decoder(&route_id);
        self.terminal.stop(&route_id);
                    tracing::info!(
                        "session StopMedia committing for {id} (route state {stop_state})"
                    );
                    self.audio.stop(&id);
                    // See the local-disconnect twin: invalidate callbacks
                    // before joining a codec thread that may be waiting for
                    // bounded sender admission.
                    self.video_route_generations.lock().retire(&id);
                    self.video.stop(&id);
                    self.video_watchers.lock().remove(&id);
                    self.release_video_lanes(&id);
                    self.release_audio_decoder(&id);
                    // A control route ending mid-chord must not leave this
    /// Begin carrying media for a now-active route. Audio, display (screen
    /// streaming), video (camera streaming), and input (remote control)
    /// are wired; storage still shows active without a transport, and the
    /// log says so.
    fn start_media(self: &Arc<Self>, route: &Route) {
        let Some(me) = self.local_node_id() else {
            return;
        };
        // Compare endpoints to ourselves *canonically* — the route's ids carry
        // the UI's display suffix while `me` is the bare node id. Without this
        // a loopback (e.g. a local terminal) matches neither the loopback arm
        // nor the host/viewer arms, and nothing starts. The bare ids only feed
        // `== me` checks, log labels, and the peer arg to the capture starts
        // (which the routing layer canonicalises again), so normalising them
        // here is safe.
        let me = pubkey_part(&me).to_string();
        let from_node = pubkey_part(&node_of(route.from.as_str())).to_string();
        let to_node = pubkey_part(&node_of(route.to.as_str())).to_string();

        // The activating Offer/Accept normally pinned this exact route while
        // its control send was daemon-confirmed. Keep a lazy fallback for
        // restored/older session shapes, but do it once before any capture,
        // terminal, file, or input pump can emit its first frame.
        let route_peer = if from_node == me {
            Some(to_node.as_str())
        } else if to_node == me {
            Some(from_node.as_str())
        } else {
            None
        };
        if let Some(peer) = route_peer.filter(|peer| *peer != me.as_str()) {
            let _ = self.network_for_route(&route.id, peer);
        }

        match route.media {
            MediaKind::Audio => {
                // We source: capture what the routed capability names — the
                // machine's own playback for the synthetic `system-audio`,
                // the default mic for a scanned input device — and stream
                // it to the sink. Transport: the offer said what the sink
                // can consume — Opus on the daemon's audio track lane when
                // both stacks carry it and this peer's lane is free, PCM
                // frames over the media channel otherwise (the floor).
                if from_node == me {
                    let source = audio_capture_source(route);
                    let accepts_opus = self
                        .state
                        .lock()
                        .session
                        .as_ref()
                        .and_then(|s| s.route(&route.id))
                        .map(|r| r.audio.iter().any(|a| a == "opus"))
                        .unwrap_or(false);
                    let lane = accepts_opus && self.audio_lane(&route.id, &to_node, true).is_some();
                    tracing::info!(
                        "route {} active — streaming {} to {} ({})",
                        route.id,
                        match source {
                            CaptureSource::System => "system audio",
                            CaptureSource::Mic => "mic audio",
                        },
                        short_id(&to_node),
                        if lane { "Opus lane" } else { "PCM channel" }
                    );
                    let peer = to_node.clone();
                    let tx = self.audio_out.clone();
                    let encoder = if lane {
                        match crate::audio::OpusStream::new() {
                            Ok(enc) => Some(parking_lot::Mutex::new(enc)),
                            Err(e) => {
                                tracing::warn!(
                                    "opus encoder for {} failed ({e}) — falling back to PCM frames",
                                    route.id
                                );
                                // Safe on a live route now that this only
                                // drops the route's own decoder — it used to
                                // also close a lane by rank, which on a route
                                // that keeps its place in the peer's Opus
                                // order would have hit the top-ranked
                                // neighbour's lane instead of this one's.
                                self.release_audio_decoder(&route.id);
                                None
                            }
                        }
                    } else {
                        None
                    };
                    let rid = route.id.clone();
                    let seq = Arc::new(AtomicU64::new(0));
                    self.audio
                        .start_capture(route.id.clone(), source, move |pcm, rate| {
                            // try_send everywhere: a full queue drops this
                            // buffer; the next one carries fresher sound.
                            if let Some(enc) = &encoder {
                                enc.lock().push(&pcm, rate, |data| {
                                    let _ = tx.try_send(AudioOut::Lane {
                                        peer: peer.clone(),
                                        route: rid.clone(),
                                        data,
                                    });
                                });
                            } else {
                                let s = seq.fetch_add(1, Ordering::Relaxed);
                                let frame = AudioFrame::new(rid.clone(), s, rate, 1, pcm);
                                let _ = tx.try_send(AudioOut::Channel(peer.clone(), frame));
                            }
                        });
                }
                // We sink: play inbound frames for this route. Inbound Opus
                // lane samples find their route on demand
                // ([`Self::audio_route_for_lane`]) — the peer maps each
                // active-codec route to a lane by sorted position the same
                // way we do, so no claim is recorded here (the sender may
                // still pick PCM, in which case the lane simply never sees a
                // frame).
                if to_node == me {
                    tracing::info!(
                        "route {} active — playing audio from {}",
                        route.id,
                        short_id(&from_node)
                    );
                    self.audio.start_playback(route.id.clone());
                }
            }
    /// contract general, then omit endpoints whose implementation is not built
    /// into this node. Viewer/controller endpoints remain usable without `host`;
    /// audio capture and playback both require `audio-io`.
    fn advertised_capabilities(
        inv: &allmystuff_inventory::Inventory,
        node: &allmystuff_graph::NodeId,
    ) -> Vec<allmystuff_graph::Capability> {
        let mut caps =
            allmystuff_bridge::capabilities_with_screens(inv, node, &crate::video::extra_screens());
        Self::filter_advertised_capabilities(&mut caps);
        caps
    }

    /// Pure profile filtering, shared by initial presence and inventory updates.
    /// Input sources can drive a remote even when local injection is stubbed.
    fn filter_advertised_capabilities(caps: &mut Vec<Capability>) {
        caps.retain(|c| {
            let needs_host = matches!(
                (c.media, c.flow, c.origin.as_str()),
                (MediaKind::Display, Flow::Source, "screen")
                    | (MediaKind::Video, Flow::Source, "camera")
                    | (MediaKind::Input, Flow::Sink, "control")
                    | (MediaKind::Clipboard, Flow::Duplex, "clipboard")
            );
            (cfg!(feature = "host") || !needs_host)
                && (cfg!(feature = "audio-io") || c.media != MediaKind::Audio)
        });
    }

    fn sorted_media_routes(&self, peer: &str, outbound: bool, codec: &str) -> Vec<String> {
        let Some(me) = self.local_node_id() else {
            return Vec::new();
        };
        let mp = pubkey_part(&me).to_string();
        let pc = pubkey_part(peer).to_string();
        let st = self.state.lock();
        let Some(session) = st.session.as_ref() else {
            return Vec::new();
        };
        let mut ids: Vec<String> = session
            .active_routes()
            .filter(|r| {
                let codecs = if codec == "opus" { &r.audio } else { &r.video };
                codecs.iter().any(|c| c == codec) && {
                    let src = pubkey_part(node_of(r.route.from.as_str()).as_str()).to_string();
                    let dst = pubkey_part(node_of(r.route.to.as_str()).as_str()).to_string();
                    if outbound {
                        src == mp && dst == pc
                    } else {
                        src == pc && dst == mp
                    }
                }
            })
            .map(|r| r.route.id.clone())
            .collect();
        ids.sort_unstable();
        ids
    }

    /// The media-lane pool size we and `peer` can both use for video: 0 when the
    /// The audio twin of [`Self::effective_video_lanes`], gated on the audio lane.
    fn effective_audio_lanes(&self, peer: &str) -> u8 {
        if !self.daemon_audio.load(Ordering::SeqCst) {
            return 0;
        }
        if self.peer_supports_lanes(peer) {
            self.daemon_lanes.load(Ordering::SeqCst).max(1)
        } else {
            1
        }
    }
    /// The audio twin of [`Self::video_lane`] (Opus on the audio lane).
    fn audio_lane(&self, route_id: &str, peer: &str, outbound: bool) -> Option<u8> {
        let cap = self.effective_audio_lanes(peer);
        if cap == 0 {
            return None;
        }
        let idx = self
            .sorted_media_routes(peer, outbound, "opus")
            .iter()
            .position(|id| id == route_id)?;
        (idx < cap as usize).then_some(idx as u8)
    }
    /// The audio twin of [`Self::video_route_for_lane`].
    fn audio_route_for_lane(&self, peer: &str, lane: u8) -> Option<String> {
        self.sorted_media_routes(peer, false, "opus")
            .into_iter()
            .nth(lane as usize)
    }
    /// Whether an inbound media frame is acceptable: its route is one this
    /// session knows, is live, carries `media`, sinks on this machine, and
    /// the daemon-authenticated sender is the route's peer.
    fn inbound_media_ok(&self, route_id: &str, sender: &str, media: MediaKind) -> bool {
        let Some(me) = self.local_node_id() else {
            return false;
        };
        let st = self.state.lock();
        let Some(r) = st.session.as_ref().and_then(|s| s.route(route_id)) else {
            return false;
        };
        r.is_active()
            && r.route.media == media
            && route_sinks_on(&r.route, &me)
            && pubkey_part(r.peer.as_str()) == pubkey_part(sender)
    }
/// What an audio route this machine sources should capture: the synthetic
/// `system-audio` capability advertises "what this machine plays", so it
/// captures the machine's own output (loopback); every other audio source
/// is a scanned input device — the default mic in v1. Pure, so the rule
/// that decides between "your room" and "your sound" is unit-testable.
fn audio_capture_source(route: &Route) -> CaptureSource {
    match route.from.as_str().split_once(':') {
        Some((_, "system-audio")) => CaptureSource::System,
        _ => CaptureSource::Mic,
    }
}

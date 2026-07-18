//! Control-channel messages a phone sends *beyond* opening a media / terminal /
//! files route — the pieces of the desktop's control surface a
//! viewer/controller phone needs to reach parity with the current model:
//!
//! * **fleet-machine admin** ([`AppControl`]) — tell one of your own machines to
//!   upgrade, restart its app, or reboot the whole device. "Reboot the wedged
//!   server from your phone" is one of the most compelling mobile cases, and it
//!   rides the same owner/fleet gate the desktop uses.
//! * **KVM curation** ([`KvmControl`]) — point a KVM appliance at a target,
//!   detach it, or walk it on/off a mesh, plus the recognition helpers
//!   ([`is_kvm`], [`kvm_web_site`]) a phone needs to render "this is a KVM" and
//!   open its web UI.
//! * **per-route video negotiation** ([`RouteControl`]) — the levers a *viewer*
//!   drives: [`tune`] (cap resolution / bitrate / fps — the one a phone on
//!   cellular leans on hardest), [`refresh_video`] (force a clean decode entry
//!   after the decoder lost its place), and [`video_feedback`] (report decode
//!   health back to the streamer).
//! * **the shared-shell picker** ([`list_terminal_sessions`]) — ask a host which
//!   shells are already open so the phone can *attach* (tmux-style multi-attach)
//!   instead of always minting a new one; pair it with
//!   [`crate::connect::offer_terminal`]'s `attach` argument.
//! * **fleet-site management** ([`SiteControl`]) — list and re-expose a co-owned
//!   machine's sites.
//!
//! Every builder returns a [`ControlMessage`] ready to publish on
//! [`CHANNEL_CONTROL`](allmystuff_protocol::CHANNEL_CONTROL) to one peer with
//! [`MeshClient::send_control`](crate::transport::MeshClient::send_control). The
//! wire shapes match `amst` and the desktop GUI byte-for-byte — the tests here
//! pin the `t` / `kind` tags and field names, so a drift from the daemon's
//! contract fails `cargo test` rather than silently going unanswered on the
//! wire. Anything the *host* sends back (a `Tune` echo, a
//! [`RouteControl::TerminalSessions`] answer, a [`RouteControl::VideoLane`]
//! binding) arrives already typed through [`crate::transport::classify`].

use allmystuff_graph::NodeId;
use allmystuff_protocol::{
    AppControl, ControlMessage, KvmControl, NodeProfile, RouteControl, SiteAdvert, SiteControl,
    FEATURE_KVM,
};
use std::collections::BTreeMap;

// ---- app-level fleet-machine admin -----------------------------------------
//
// Sent to one of *your own* machines (its peer id). The receiver enforces that
// the sender is its owner or a fleet co-member before acting — the same rule
// that gates a terminal or a remote-control session — so a stranger on the mesh
// can never drive it. Each command confirms by the machine's next presence
// advert (a new version after an upgrade, presence dropping and returning after
// a reboot), exactly as a claim confirms by re-advertising its owner.

/// "Update yourself and restart." For a fleet machine running an AllMyStuff
/// older than the channel's latest release ([`NodeProfile::version`]).
pub fn app_upgrade() -> ControlMessage {
    ControlMessage::App(AppControl::Upgrade)
}

/// "Restart your AllMyStuff app." Relaunch the node onto the same build — the
/// recovery step heavier than a reconnect but lighter than an upgrade.
pub fn app_restart() -> ControlMessage {
    ControlMessage::App(AppControl::Restart)
}

/// "Reboot the machine you run on." The heaviest recovery lever — the whole OS
/// goes down and back, for the wedge an app relaunch can't clear.
pub fn app_restart_device() -> ControlMessage {
    ControlMessage::App(AppControl::RestartDevice)
}

// ---- KVM curation ----------------------------------------------------------
//
// Sent to the KVM appliance peer itself. Authorized exactly like a terminal or
// a site route (owner / fleet co-member); the KVM applies the change, persists
// it, and re-advertises presence with the new [`NodeProfile::kvm`] — that
// presence is the authoritative confirmation.

/// "Point this KVM at `target`." Binds the appliance to the machine it
/// physically controls; `label` is the target's display name at attach time so
/// the KVM can rename itself `KVM-<label>` (cosmetic — empty is fine and is
/// omitted on the wire).
pub fn kvm_attach(target: &NodeId, label: impl Into<String>) -> ControlMessage {
    ControlMessage::Kvm(KvmControl::Attach {
        node: target.clone(),
        label: label.into(),
    })
}

/// "This KVM no longer represents anything." Clears the binding — the
/// confirm-gated action, since detaching strips a machine of its out-of-band
/// screen/keyboard.
pub fn kvm_detach() -> ControlMessage {
    ControlMessage::Kvm(KvmControl::Detach)
}

/// "Join this mesh too." Adds `network_id` to the KVM's mesh memberships — the
/// fleet-owner tool for walking a device onto another venue.
pub fn kvm_mesh_add(network_id: impl Into<String>) -> ControlMessage {
    ControlMessage::Kvm(KvmControl::MeshAdd {
        network_id: network_id.into(),
    })
}

/// "Leave this mesh." Removes `network_id` from the KVM's memberships.
pub fn kvm_mesh_remove(network_id: impl Into<String>) -> ControlMessage {
    ControlMessage::Kvm(KvmControl::MeshRemove {
        network_id: network_id.into(),
    })
}

/// Does this peer advertise itself as a KVM appliance? True when it carries a
/// [`KvmAdvert`](allmystuff_protocol::KvmAdvert) or lists [`FEATURE_KVM`] — the
/// signal a phone uses to render KVM affordances (its target binding, an "Open
/// KVM" button) instead of an ordinary node card.
pub fn is_kvm(profile: &NodeProfile) -> bool {
    profile.kvm.is_some() || profile.features.iter().any(|f| f == FEATURE_KVM)
}

/// The site serving a KVM's own web UI, for the "Open KVM" button. Prefers the
/// id the KVM named in [`KvmAdvert::web`](allmystuff_protocol::KvmAdvert::web),
/// falling back to the node's first web-scheme site — exactly the desktop's
/// resolution order. `None` when the peer exposes no web site to open.
pub fn kvm_web_site(profile: &NodeProfile) -> Option<&SiteAdvert> {
    let named = profile
        .kvm
        .as_ref()
        .map(|k| k.web.as_str())
        .filter(|w| !w.is_empty());
    if let Some(id) = named {
        if let Some(site) = profile.sites.iter().find(|s| s.id == id) {
            return Some(site);
        }
    }
    profile.sites.iter().find(|s| s.is_web())
}

// ---- per-route video negotiation the viewer drives -------------------------

/// "Give me a clean decode entry *now*." The viewer's decoder lost its place (a
/// decode error, a rebuilt pipeline) and shouldn't sit out the rest of the
/// periodic IDR interval; the streaming side forces an IDR on its next capture.
/// Follow a [`VideoDecoder::refresh`](crate::media::VideoDecoder::refresh) with
/// this.
pub fn refresh_video(route_id: impl Into<String>) -> ControlMessage {
    ControlMessage::Route(RouteControl::Refresh {
        route_id: route_id.into(),
    })
}

/// "Stream with these settings." A viewer's quality picks for a display route it
/// consumes; the streaming side restarts its capture with the overrides. `None`
/// on a field leaves it automatic (the streamer's own budget). This is the lever
/// a phone leans on hardest — a small screen on cellular data asks for a modest
/// `max_edge` / `bitrate` the desktop never would.
///
/// * `max_edge` — longest output edge in pixels (e.g. 1280); `None` = native.
/// * `bitrate` — H.264 target in bits/second; `None` = pixel-budgeted.
/// * `fps` — capture-rate ceiling; `None` = the streamer's default.
pub fn tune(
    route_id: impl Into<String>,
    max_edge: Option<u32>,
    bitrate: Option<u32>,
    fps: Option<u32>,
) -> ControlMessage {
    ControlMessage::Route(RouteControl::Tune {
        route_id: route_id.into(),
        max_edge,
        bitrate,
        fps,
        game: false,
        mode: None,
    })
}

/// "Here's how your stream is actually arriving." The viewer reports its decode
/// health back to the streamer so it can adapt. All counters are *since the last
/// report*: frames actually rendered per second, decode failures, and how deep
/// the decode queue is backed up (0 = keeping up).
pub fn video_feedback(
    route_id: impl Into<String>,
    recv_fps: u32,
    decode_fails: u32,
    queue_depth: u32,
) -> ControlMessage {
    ControlMessage::Route(RouteControl::VideoFeedback {
        route_id: route_id.into(),
        recv_fps,
        decode_fails,
        queue_depth,
    })
}

/// "List your open terminal sessions." Ask a host which shells it already has
/// running, so the phone's picker can offer to *attach* to one (via
/// [`offer_terminal`](crate::connect::offer_terminal)'s `attach` argument)
/// instead of always spawning a new shell. The host answers with a
/// [`RouteControl::TerminalSessions`] the phone reads off
/// [`classify`](crate::transport::classify).
pub fn list_terminal_sessions() -> ControlMessage {
    ControlMessage::Route(RouteControl::TerminalSessionsRequest)
}

// ---- presence + fleet-site management --------------------------------------

/// "Re-send me your presence." A per-node refresh — the target re-announces its
/// [`NodeProfile`] so the phone re-learns it on the spot (the guaranteed
/// round-trip behind a pull-to-refresh on one device). Rate-limit these per
/// target.
pub fn profile_request() -> ControlMessage {
    ControlMessage::ProfileRequest
}

/// "List your sites." Ask a co-owned machine what it's listening on and what it
/// currently exposes, to manage it from the phone. The host answers with a
/// [`SiteControl::Sites`] off [`classify`](crate::transport::classify).
pub fn site_list() -> ControlMessage {
    ControlMessage::Site(SiteControl::List)
}

/// "Advertise exactly these." Set the exposed map (site id → advertised name)
/// for a co-owned machine to publish. Applied only from the owner/fleet; the
/// machine persists it and re-broadcasts presence.
pub fn site_set_exposed(exposed: BTreeMap<String, String>) -> ControlMessage {
    ControlMessage::Site(SiteControl::SetExposed { exposed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use allmystuff_protocol::{KvmAdvert, SiteAdvert};

    fn v(msg: &ControlMessage) -> serde_json::Value {
        serde_json::to_value(msg).unwrap()
    }

    /// A control message must round-trip through JSON like every other peer's,
    /// and unknown-to-us shapes must never be minted here.
    fn round_trips(msg: &ControlMessage) {
        let json = serde_json::to_string(msg).unwrap();
        let back: ControlMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(*msg, back);
    }

    #[test]
    fn app_admin_carries_the_app_tag_and_kind() {
        for (msg, kind) in [
            (app_upgrade(), "upgrade"),
            (app_restart(), "restart"),
            (app_restart_device(), "restart_device"),
        ] {
            let j = v(&msg);
            assert_eq!(j["t"], "app");
            assert_eq!(j["kind"], kind);
            round_trips(&msg);
        }
    }

    #[test]
    fn kvm_attach_names_the_target_and_optional_label() {
        let target = NodeId::from("den-tower");

        // With a label: both fields present under the kvm/attach tags.
        let j = v(&kvm_attach(&target, "den-tower"));
        assert_eq!(j["t"], "kvm");
        assert_eq!(j["kind"], "attach");
        assert_eq!(j["node"], "den-tower");
        assert_eq!(j["label"], "den-tower");

        // Empty label is omitted (skip_serializing_if), matching the desktop.
        let j = v(&kvm_attach(&target, ""));
        assert_eq!(j["node"], "den-tower");
        assert!(j.get("label").is_none() || j["label"].is_null());

        round_trips(&kvm_attach(&target, "den-tower"));
    }

    #[test]
    fn kvm_detach_and_mesh_edits_match_the_wire() {
        let j = v(&kvm_detach());
        assert_eq!(j["t"], "kvm");
        assert_eq!(j["kind"], "detach");

        let j = v(&kvm_mesh_add("cec-customer-abcde"));
        assert_eq!(j["kind"], "mesh_add");
        assert_eq!(j["network_id"], "cec-customer-abcde");

        let j = v(&kvm_mesh_remove("lab-mesh"));
        assert_eq!(j["kind"], "mesh_remove");
        assert_eq!(j["network_id"], "lab-mesh");
    }

    fn kvm_profile(web: &str, sites: Vec<SiteAdvert>, feature_only: bool) -> NodeProfile {
        let mut p = NodeProfile {
            protocol: 1,
            node: NodeId::from("kvm-1"),
            label: "Den KVM".into(),
            hostname: String::new(),
            summary: Default::default(),
            capabilities: Vec::new(),
            owner: None,
            claimable: false,
            boot: 0,
            features: Vec::new(),
            sites,
            version: String::new(),
            fleet_name: String::new(),
            fleet_owner: String::new(),
            kvm: if feature_only {
                None
            } else {
                Some(KvmAdvert {
                    web: web.into(),
                    ..Default::default()
                })
            },
            sent_at: 0,
        };
        if feature_only {
            p.features.push(FEATURE_KVM.to_string());
        }
        p
    }

    fn web_site(id: &str, scheme: &str) -> SiteAdvert {
        SiteAdvert {
            id: id.into(),
            label: "Web".into(),
            port: 80,
            scheme: scheme.into(),
            loopback: true,
        }
    }

    #[test]
    fn is_kvm_recognizes_advert_and_feature_flag() {
        // A KvmAdvert alone is enough.
        assert!(is_kvm(&kvm_profile("tcp:80", vec![], false)));
        // So is the FEATURE_KVM tag alone (advert not yet populated).
        assert!(is_kvm(&kvm_profile("", vec![], true)));
        // An ordinary machine is not a KVM.
        let plain = kvm_profile("", vec![], false);
        let mut plain = plain;
        plain.kvm = None;
        assert!(!is_kvm(&plain));
    }

    #[test]
    fn kvm_web_site_prefers_the_named_id_then_falls_back_to_first_web() {
        // Named id wins even when another web site sorts first.
        let sites = vec![web_site("tcp:8080", "http"), web_site("tcp:443", "https")];
        let p = kvm_profile("tcp:443", sites, false);
        assert_eq!(kvm_web_site(&p).unwrap().id, "tcp:443");

        // No named id → first web-scheme site.
        let sites = vec![
            web_site("tcp:22", "ssh"), // not web
            web_site("tcp:8080", "http"),
        ];
        let p = kvm_profile("", sites, false);
        assert_eq!(kvm_web_site(&p).unwrap().id, "tcp:8080");

        // Nothing web to open.
        let p = kvm_profile("", vec![web_site("tcp:22", "ssh")], false);
        assert!(kvm_web_site(&p).is_none());
    }

    #[test]
    fn video_negotiation_rides_the_route_tag() {
        let j = v(&refresh_video("route:desk:screen→phone:display-in"));
        assert_eq!(j["t"], "route");
        assert_eq!(j["kind"], "refresh");
        assert_eq!(j["route_id"], "route:desk:screen→phone:display-in");

        // Tune with a cellular-friendly cap; unset fields are omitted.
        let j = v(&tune("route:r", Some(1280), Some(1_500_000), None));
        assert_eq!(j["kind"], "tune");
        assert_eq!(j["max_edge"], 1280);
        assert_eq!(j["bitrate"], 1_500_000);
        assert!(j.get("fps").is_none() || j["fps"].is_null());

        // All-automatic tune carries only the route id.
        let j = v(&tune("route:r", None, None, None));
        assert!(j.get("max_edge").is_none() || j["max_edge"].is_null());

        let j = v(&video_feedback("route:r", 24, 3, 1));
        assert_eq!(j["kind"], "video_feedback");
        assert_eq!(j["recv_fps"], 24);
        assert_eq!(j["decode_fails"], 3);
        assert_eq!(j["queue_depth"], 1);

        let j = v(&list_terminal_sessions());
        assert_eq!(j["kind"], "terminal_sessions_request");
    }

    #[test]
    fn profile_request_and_site_management_shapes() {
        let j = v(&profile_request());
        assert_eq!(j["t"], "profile_request");

        let j = v(&site_list());
        assert_eq!(j["t"], "site");
        assert_eq!(j["kind"], "list");

        let mut exposed = BTreeMap::new();
        exposed.insert("tcp:8080".to_string(), "Grafana".to_string());
        let j = v(&site_set_exposed(exposed));
        assert_eq!(j["kind"], "set_exposed");
        assert_eq!(j["exposed"]["tcp:8080"], "Grafana");
    }

    #[test]
    fn every_builder_round_trips() {
        round_trips(&app_upgrade());
        round_trips(&kvm_detach());
        round_trips(&kvm_mesh_add("m"));
        round_trips(&refresh_video("r"));
        round_trips(&tune("r", Some(1920), None, Some(30)));
        round_trips(&video_feedback("r", 30, 0, 0));
        round_trips(&list_terminal_sessions());
        round_trips(&profile_request());
        round_trips(&site_list());
    }
}

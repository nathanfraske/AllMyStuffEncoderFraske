//! # allmystuff-graph
//!
//! The model behind the AllMyStuff screen: a graph of **nodes** (your
//! machines and the people you share with), the **capabilities** each one
//! exposes (this mic, that display, this computer's screen), the
//! **routes** that wire one capability to another across the mesh, and the
//! **groups** that bundle a set of capabilities so you can move them as a
//! unit.
//!
//! On top of that sits the part that makes it safe for normal people: the
//! **relationship + grant** model. MyOwnMesh underneath proves *identity*
//! (who a peer cryptographically is). This crate never surfaces keys — it
//! answers the only question a user actually has: *is this mine, or am I
//! sharing with someone, and for what?* Every route is checked against
//! that. See [`Relationship`] and [`Catalog::authorize`].
//!
//! ```
//! use allmystuff_graph::*;
//!
//! let mut cat = Catalog::new();
//! cat.nodes.push(MeshNode::this("My laptop"));
//! cat.nodes.push(MeshNode {
//!     id: "desk-pc".into(),
//!     label: "Desk PC".into(),
//!     kind: NodeKind::Machine,
//!     relationship: Relationship::Mine, // a device I own
//!     online: true,
//! });
//! cat.capabilities.push(Capability::new(
//!     NodeId::this(), "this:mic", "Laptop mic", MediaKind::Audio, Flow::Source, "microphone",
//! ));
//! cat.capabilities.push(Capability::new(
//!     "desk-pc", "desk-pc:system-in", "Desk PC audio in", MediaKind::Audio, Flow::Sink, "system",
//! ));
//!
//! // Both are mine, so this just works — no grant ceremony.
//! let route = cat.propose_route(&"this:mic".into(), &"desk-pc:system-in".into()).unwrap();
//! assert_eq!(route.media, MediaKind::Audio);
//! ```

mod authz;
mod catalog;
mod model;

pub use authz::{describe_action, describe_grant, Denied, GrantRequest};
pub use catalog::{Catalog, ConnectError};
pub use model::*;

#[cfg(test)]
mod tests {
    use super::*;

    // ---- fixtures -----------------------------------------------------

    fn alex() -> Person {
        Person {
            id: "person:alex".into(),
            name: "Alex".into(),
        }
    }

    /// A catalog with: this laptop (mine), a desk PC I own, and Alex's
    /// laptop (shared, no grants yet). Each machine carries the synthetic
    /// "screen" (Display source) + "control" (Input sink) + "system" audio
    /// duplex capabilities the bridge would mint, plus a physical display
    /// and mic on *this* node.
    fn fixture() -> Catalog {
        let mut cat = Catalog::new();
        cat.nodes.push(MeshNode::this("My laptop"));
        cat.nodes.push(MeshNode {
            id: "desk".into(),
            label: "Desk PC".into(),
            kind: NodeKind::Machine,
            relationship: Relationship::Mine,
            online: true,
        });
        cat.nodes.push(MeshNode {
            id: "alex".into(),
            label: "Alex's laptop".into(),
            kind: NodeKind::Machine,
            relationship: Relationship::Shared(Share {
                person: alex(),
                grants: vec![],
            }),
            online: true,
        });

        for node in ["this", "desk", "alex"] {
            cat.capabilities.push(Capability::new(
                node,
                format!("{node}:screen"),
                "Screen",
                MediaKind::Display,
                Flow::Source,
                "screen",
            ));
            cat.capabilities.push(Capability::new(
                node,
                format!("{node}:control"),
                "Keyboard & mouse control",
                MediaKind::Input,
                Flow::Sink,
                "control",
            ));
            cat.capabilities.push(Capability::new(
                node,
                format!("{node}:system"),
                "System audio",
                MediaKind::Audio,
                Flow::Duplex,
                "system",
            ));
        }

        // Physical peripherals on this laptop.
        cat.capabilities.push(Capability::new(
            "this",
            "this:display",
            "Built-in display",
            MediaKind::Display,
            Flow::Sink,
            "display",
        ));
        cat.capabilities.push(Capability::new(
            "this",
            "this:mic",
            "Mic array",
            MediaKind::Audio,
            Flow::Source,
            "microphone",
        ));
        cat.capabilities.push(Capability::new(
            "this",
            "this:keyboard",
            "Keyboard",
            MediaKind::Input,
            Flow::Source,
            "keyboard",
        ));
        cat.capabilities.push(Capability::new(
            "this",
            "this:speaker",
            "Speakers",
            MediaKind::Audio,
            Flow::Sink,
            "speaker",
        ));
        cat
    }

    // ---- routing basics ----------------------------------------------

    #[test]
    fn mine_to_mine_connects_without_grants() {
        let cat = fixture();
        let r = cat
            .propose_route(&"this:mic".into(), &"desk:system".into())
            .expect("two of my own devices connect freely");
        assert_eq!(r.media, MediaKind::Audio);
        assert_eq!(r.from, "this:mic".into());
    }

    #[test]
    fn media_mismatch_is_rejected() {
        let cat = fixture();
        // A mic (audio) can't feed a display sink.
        let err = cat
            .propose_route(&"this:mic".into(), &"this:display".into())
            .unwrap_err();
        assert!(matches!(err, ConnectError::MediaMismatch { .. }), "{err:?}");
    }

    #[test]
    fn wrong_flow_is_rejected() {
        let cat = fixture();
        // A display *sink* can't be a source.
        let err = cat
            .propose_route(&"this:display".into(), &"desk:screen".into())
            .unwrap_err();
        assert!(matches!(err, ConnectError::WrongFlow { .. }), "{err:?}");
    }

    #[test]
    fn self_loop_is_rejected() {
        let cat = fixture();
        let err = cat
            .propose_route(&"this:mic".into(), &"this:mic".into())
            .unwrap_err();
        assert_eq!(err, ConnectError::SelfLoop);
    }

    // ---- authorization -----------------------------------------------

    #[test]
    fn sharing_to_a_person_is_denied_without_a_grant() {
        let mut cat = fixture();
        cat.capabilities.push(Capability::new(
            "alex",
            "alex:display",
            "Alex's monitor",
            MediaKind::Display,
            Flow::Sink,
            "display",
        ));
        // Cast my screen to Alex's display — Alex's endpoint is a sink
        // (Consume), and there's no grant.
        let err = cat
            .propose_route(&"this:screen".into(), &"alex:display".into())
            .unwrap_err();
        match err {
            ConnectError::Denied(d) => {
                assert_eq!(d.person_name, "Alex");
                assert_eq!(d.role, GrantRole::Consume);
                assert_eq!(d.media, MediaKind::Display);
            }
            other => panic!("expected Denied, got {other:?}"),
        }
    }

    #[test]
    fn a_matching_grant_authorizes_the_share() {
        let mut cat = fixture();
        // Need a display sink on Alex's side.
        cat.capabilities.push(Capability::new(
            "alex",
            "alex:display",
            "Alex's monitor",
            MediaKind::Display,
            Flow::Sink,
            "display",
        ));
        // Grant Alex "may receive my display."
        grant(
            &mut cat,
            "alex",
            Grant {
                id: "g1".into(),
                media: MediaKind::Display,
                role: GrantRole::Consume,
                capability: None,
                label: "Receive your screen".into(),
            },
        );
        let r = cat
            .propose_route(&"this:screen".into(), &"alex:display".into())
            .expect("grant authorizes the cast");
        assert_eq!(r.media, MediaKind::Display);
    }

    #[test]
    fn grant_direction_matters() {
        let mut cat = fixture();
        cat.capabilities.push(Capability::new(
            "alex",
            "alex:cam",
            "Alex's webcam",
            MediaKind::Video,
            Flow::Source,
            "camera",
        ));
        cat.capabilities.push(Capability::new(
            "this",
            "this:videowin",
            "Video window",
            MediaKind::Video,
            Flow::Sink,
            "screen",
        ));
        // A "Consume" grant does NOT let Alex be a source.
        grant(
            &mut cat,
            "alex",
            Grant {
                id: "g-consume".into(),
                media: MediaKind::Video,
                role: GrantRole::Consume,
                capability: None,
                label: String::new(),
            },
        );
        let err = cat
            .propose_route(&"alex:cam".into(), &"this:videowin".into())
            .unwrap_err();
        assert!(matches!(err, ConnectError::Denied(_)));

        // A "Provide" grant does.
        grant(
            &mut cat,
            "alex",
            Grant {
                id: "g-provide".into(),
                media: MediaKind::Video,
                role: GrantRole::Provide,
                capability: None,
                label: String::new(),
            },
        );
        cat.propose_route(&"alex:cam".into(), &"this:videowin".into())
            .expect("provide grant lets Alex send video");
    }

    #[test]
    fn grant_can_be_pinned_to_one_capability() {
        let mut cat = fixture();
        cat.capabilities.push(Capability::new(
            "alex",
            "alex:spkA",
            "Kitchen speaker",
            MediaKind::Audio,
            Flow::Sink,
            "speaker",
        ));
        cat.capabilities.push(Capability::new(
            "alex",
            "alex:spkB",
            "Bedroom speaker",
            MediaKind::Audio,
            Flow::Sink,
            "speaker",
        ));
        // Grant pinned to the kitchen speaker only.
        grant(
            &mut cat,
            "alex",
            Grant {
                id: "g-kitchen".into(),
                media: MediaKind::Audio,
                role: GrantRole::Consume,
                capability: Some("alex:spkA".into()),
                label: String::new(),
            },
        );
        cat.propose_route(&"this:mic".into(), &"alex:spkA".into())
            .expect("kitchen is granted");
        let err = cat
            .propose_route(&"this:mic".into(), &"alex:spkB".into())
            .unwrap_err();
        assert!(
            matches!(err, ConnectError::Denied(_)),
            "bedroom is not granted"
        );
    }

    #[test]
    fn required_grants_describes_the_one_tap_fix() {
        let mut cat = fixture();
        cat.capabilities.push(Capability::new(
            "alex",
            "alex:display",
            "Alex's monitor",
            MediaKind::Display,
            Flow::Sink,
            "display",
        ));
        let reqs = cat.required_grants(&"this:screen".into(), &"alex:display".into());
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].person_name, "Alex");
        assert_eq!(reqs[0].role, GrantRole::Consume);
        assert_eq!(reqs[0].capability, Some("alex:display".into()));
        assert_eq!(reqs[0].description, "Receive your display");
    }

    // ---- groups: the RDC bundle --------------------------------------

    #[test]
    fn rdc_group_fans_out_to_the_right_routes() {
        let cat = {
            let mut c = fixture();
            c.groups.push(Group {
                id: "rdc".into(),
                name: "My desk".into(),
                node: NodeId::this(),
                // Monitor (sink), keyboard (source), mic (source),
                // speakers (sink) — the classic remote-desktop terminal.
                members: vec![
                    "this:display".into(),
                    "this:keyboard".into(),
                    "this:mic".into(),
                    "this:speaker".into(),
                ],
            });
            c
        };

        let routes = cat
            .connect_group("rdc", &"desk".into())
            .expect("connects to my desk PC");

        // Expect: desk screen → my display, my keyboard → desk control,
        // my mic → desk system audio, desk system audio → my speakers.
        let has = |from: &str, to: &str| {
            routes
                .iter()
                .any(|r| r.from == from.into() && r.to == to.into())
        };
        assert!(
            has("desk:screen", "this:display"),
            "remote screen drives my monitor"
        );
        assert!(
            has("this:keyboard", "desk:control"),
            "my keyboard controls the PC"
        );
        assert!(has("this:mic", "desk:system"), "my mic feeds the PC");
        assert!(
            has("desk:system", "this:speaker"),
            "the PC's audio reaches my speakers"
        );
        assert!(routes.iter().all(|r| r.group.as_deref() == Some("rdc")));
    }

    #[test]
    fn group_routing_prefers_the_default_device() {
        // A target with two speakers and no synthetic system-audio endpoint:
        // a group's audio source should land on the one marked default.
        let mut cat = Catalog::new();
        cat.nodes.push(MeshNode::this("My laptop"));
        cat.nodes.push(MeshNode {
            id: "box".into(),
            label: "Box".into(),
            kind: NodeKind::Machine,
            relationship: Relationship::Mine,
            online: true,
        });
        cat.capabilities.push(Capability::new(
            "this",
            "this:mic",
            "Mic",
            MediaKind::Audio,
            Flow::Source,
            "microphone",
        ));
        // The default is the id-*later* speaker, so only default-preference
        // (not id order, which would pick spkA) can explain the choice.
        cat.capabilities.push(Capability::new(
            "box",
            "box:spkA",
            "Aux speaker",
            MediaKind::Audio,
            Flow::Sink,
            "speaker",
        ));
        cat.capabilities.push(
            Capability::new(
                "box",
                "box:spkZ",
                "Main speaker",
                MediaKind::Audio,
                Flow::Sink,
                "speaker",
            )
            .as_default(true),
        );
        cat.groups.push(Group {
            id: "g".into(),
            name: "Audio".into(),
            node: NodeId::this(),
            members: vec!["this:mic".into()],
        });
        let routes = cat.connect_group("g", &"box".into()).expect("connects");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].to, "box:spkZ".into());
    }

    #[test]
    fn group_connect_aborts_when_a_leg_would_breach_a_share() {
        // A group on a shared person's node, pointed at my desk PC, must
        // not silently connect the authorized legs while dropping the
        // denied one — it aborts.
        let mut cat = fixture();
        cat.groups.push(Group {
            id: "alex-desk".into(),
            name: "Alex's desk".into(),
            node: "alex".into(),
            members: vec!["alex:screen".into()], // Display source on Alex
        });
        // My desk PC has a display sink to receive it.
        cat.capabilities.push(Capability::new(
            "desk",
            "desk:display",
            "Desk monitor",
            MediaKind::Display,
            Flow::Sink,
            "display",
        ));
        let err = cat.connect_group("alex-desk", &"desk".into()).unwrap_err();
        assert!(matches!(err, ConnectError::Denied(_)), "{err:?}");
    }

    // ---- serde --------------------------------------------------------

    #[test]
    fn catalog_round_trips_through_json() {
        let cat = fixture();
        let json = serde_json::to_string(&cat).unwrap();
        let back: Catalog = serde_json::from_str(&json).unwrap();
        assert_eq!(cat, back);

        // Relationship is internally tagged so TS can switch on `kind`.
        assert!(json.contains(r#""kind":"mine""#));
        assert!(json.contains(r#""kind":"shared""#));
        // Ids serialise as bare strings, not wrapper objects.
        assert!(json.contains(r#""id":"this""#));
    }

    // ---- helpers ------------------------------------------------------

    fn grant(cat: &mut Catalog, node: &str, g: Grant) {
        let node = cat
            .nodes
            .iter_mut()
            .find(|n| n.id == node.into())
            .expect("node exists");
        if let Relationship::Shared(share) = &mut node.relationship {
            share.grants.push(g);
        } else {
            panic!("node is not shared");
        }
    }
}

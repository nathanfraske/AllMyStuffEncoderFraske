//! # allmystuff-bridge
//!
//! The seam between hardware and graph: turn a scanned [`Inventory`] into
//! the routable [`Capability`] nodes the graph wires together. Shared by
//! the headless CLI and the Tauri backend so "what this machine exposes"
//! is computed in exactly one place.
//!
//! Two kinds of capability come out of a machine:
//!
//!  * **Physical** — one per real device the scan found. A mic is an audio
//!    *source*, a speaker an audio *sink*, a camera a video source, a
//!    monitor a display sink, a keyboard an input source, a volume a duplex
//!    storage endpoint.
//!  * **Synthetic** — a small fixed set that represents "the machine
//!    itself" so whole-computer flows work: its **screen** (a display
//!    source you can cast or use as a remote desktop), **control** (an
//!    input sink that lets a remote keyboard/mouse drive it), and
//!    **system audio** (a duplex endpoint for its mixer). These are what an
//!    RDC group lands on at the far end.
//!
//! This is the only place hardware vocabulary meets graph vocabulary, so
//! it's small and thoroughly tested.

use allmystuff_graph::{Capability, Flow, MediaKind, NodeId};
use allmystuff_inventory::{InputKind, Inventory};
use allmystuff_protocol::InventorySummary;

/// Build every capability `node` should expose, given its scan.
pub fn capabilities_from_inventory(inv: &Inventory, node: &NodeId) -> Vec<Capability> {
    let mut caps = Vec::new();
    let n = node.as_str();
    let mk = |id: String, label: String, media, flow, origin: &str| {
        Capability::new(node.clone(), id, label, media, flow, origin.to_string())
    };

    // ---- synthetic "the machine itself" -----------------------------
    caps.push(mk(
        format!("{n}:screen"),
        "Screen".into(),
        MediaKind::Display,
        Flow::Source,
        "screen",
    ));
    caps.push(mk(
        format!("{n}:control"),
        "Keyboard & mouse control".into(),
        MediaKind::Input,
        Flow::Sink,
        "control",
    ));
    caps.push(mk(
        format!("{n}:system-audio"),
        "System audio".into(),
        MediaKind::Audio,
        Flow::Duplex,
        "system",
    ));

    // ---- physical devices -------------------------------------------
    for m in &inv.microphones {
        let label = if m.is_array() {
            format!("{} (array)", m.name)
        } else {
            m.name.clone()
        };
        caps.push(mk(
            qualify(n, &m.id),
            label,
            MediaKind::Audio,
            Flow::Source,
            "microphone",
        ));
    }
    for s in &inv.speakers {
        caps.push(mk(
            qualify(n, &s.id),
            s.name.clone(),
            MediaKind::Audio,
            Flow::Sink,
            "speaker",
        ));
    }
    for c in &inv.cameras {
        caps.push(mk(
            qualify(n, &c.id),
            c.name.clone(),
            MediaKind::Video,
            Flow::Source,
            "camera",
        ));
    }
    for d in &inv.displays {
        // Only connected monitors are wireable sinks.
        if !d.connected {
            continue;
        }
        caps.push(mk(
            qualify(n, &d.id),
            d.name.clone(),
            MediaKind::Display,
            Flow::Sink,
            "display",
        ));
    }
    for i in &inv.inputs {
        // Keyboards/mice/etc. are input *sources* (they produce events).
        let origin = match i.kind {
            InputKind::Keyboard => "keyboard",
            InputKind::Mouse => "mouse",
            InputKind::Touchpad => "touchpad",
            InputKind::Touchscreen => "touchscreen",
            InputKind::Gamepad => "gamepad",
            InputKind::Tablet => "tablet",
            InputKind::Other => "input",
        };
        caps.push(mk(
            qualify(n, &i.id),
            i.name.clone(),
            MediaKind::Input,
            Flow::Source,
            origin,
        ));
    }
    for v in &inv.storage {
        // Skip the tiny pseudo-volumes; share real ones both ways.
        if v.total_bytes < 1 << 30 {
            continue;
        }
        caps.push(mk(
            qualify(n, &v.id),
            v.name.clone(),
            MediaKind::Storage,
            Flow::Duplex,
            "storage",
        ));
    }

    caps
}

/// A thumbnail of the machine for the presence advert / node card.
pub fn node_summary(inv: &Inventory) -> InventorySummary {
    InventorySummary {
        os: match &inv.host.os_version {
            Some(v) => format!("{} {}", inv.host.os, v),
            None => inv.host.os.clone(),
        },
        cpu: inv.cpu.brand.clone(),
        ram_bytes: inv.memory.total_bytes,
        device_count: inv.device_count() as u32,
    }
}

/// Namespace a device id under its node so the same inventory id on two
/// machines stays distinct on the graph.
fn qualify(node: &str, device_id: &str) -> String {
    format!("{node}:{device_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use allmystuff_inventory::*;

    fn empty_inventory() -> Inventory {
        Inventory {
            scanned_at: 0,
            host: HostInfo {
                hostname: "box".into(),
                os: "linux".into(),
                os_version: None,
                kernel_version: None,
                arch: "x86_64".into(),
                board: None,
                soc: None,
                uptime_secs: 0,
            },
            cpu: Cpu {
                brand: "Test CPU".into(),
                vendor: None,
                physical_cores: Some(4),
                logical_cores: 8,
                max_mhz: None,
            },
            memory: Memory {
                total_bytes: 16 << 30,
                available_bytes: 8 << 30,
                swap_total_bytes: 0,
                swap_used_bytes: 0,
            },
            gpus: vec![],
            storage: vec![],
            networks: vec![],
            displays: vec![],
            microphones: vec![],
            speakers: vec![],
            cameras: vec![],
            inputs: vec![],
            usb: vec![],
        }
    }

    #[test]
    fn every_machine_exposes_screen_control_and_system_audio() {
        let inv = empty_inventory();
        let caps = capabilities_from_inventory(&inv, &NodeId::this());
        let by_origin = |o: &str| caps.iter().find(|c| c.origin == o).cloned();

        let screen = by_origin("screen").expect("screen");
        assert_eq!(screen.media, MediaKind::Display);
        assert_eq!(screen.flow, Flow::Source);
        assert_eq!(screen.id.as_str(), "this:screen");

        let control = by_origin("control").expect("control");
        assert_eq!(
            (control.media, control.flow),
            (MediaKind::Input, Flow::Sink)
        );

        let sys = by_origin("system").expect("system audio");
        assert_eq!((sys.media, sys.flow), (MediaKind::Audio, Flow::Duplex));
    }

    #[test]
    fn devices_map_to_the_right_media_and_flow() {
        let mut inv = empty_inventory();
        inv.microphones.push(AudioDevice {
            id: "mic:1:0".into(),
            name: "ReSpeaker".into(),
            direction: AudioDirection::Input,
            channels: Some(4),
            card: Some("1".into()),
        });
        inv.speakers.push(AudioDevice {
            id: "spk:0:0".into(),
            name: "Speakers".into(),
            direction: AudioDirection::Output,
            channels: None,
            card: Some("0".into()),
        });
        inv.cameras.push(Camera {
            id: "cam:video0".into(),
            name: "Webcam".into(),
            path: None,
        });
        inv.displays.push(Display {
            id: "display:HDMI-A-1".into(),
            name: "Monitor".into(),
            connector: "HDMI-A-1".into(),
            connected: true,
            width_px: Some(1920),
            height_px: Some(1080),
            internal: false,
        });
        inv.inputs.push(InputDevice {
            id: "input:input1".into(),
            name: "Keyboard".into(),
            kind: InputKind::Keyboard,
        });

        let caps = capabilities_from_inventory(&inv, &NodeId::this());
        let find = |origin: &str| caps.iter().find(|c| c.origin == origin).unwrap();

        assert_eq!(
            (find("microphone").media, find("microphone").flow),
            (MediaKind::Audio, Flow::Source)
        );
        // The array tag rides through to the friendly label.
        assert!(find("microphone").label.contains("array"));
        assert_eq!(
            (find("speaker").media, find("speaker").flow),
            (MediaKind::Audio, Flow::Sink)
        );
        assert_eq!(
            (find("camera").media, find("camera").flow),
            (MediaKind::Video, Flow::Source)
        );
        assert_eq!(
            (find("display").media, find("display").flow),
            (MediaKind::Display, Flow::Sink)
        );
        assert_eq!(
            (find("keyboard").media, find("keyboard").flow),
            (MediaKind::Input, Flow::Source)
        );

        // Ids are namespaced under the node.
        assert_eq!(find("microphone").id.as_str(), "this:mic:1:0");
    }

    #[test]
    fn disconnected_displays_are_not_wireable() {
        let mut inv = empty_inventory();
        inv.displays.push(Display {
            id: "display:DP-3".into(),
            name: "Nothing".into(),
            connector: "DP-3".into(),
            connected: false,
            width_px: None,
            height_px: None,
            internal: false,
        });
        let caps = capabilities_from_inventory(&inv, &NodeId::this());
        assert!(caps.iter().all(|c| c.origin != "display"));
    }

    #[test]
    fn summary_reflects_the_scan() {
        let inv = empty_inventory();
        let s = node_summary(&inv);
        assert_eq!(s.cpu, "Test CPU");
        assert_eq!(s.ram_bytes, 16 << 30);
        assert_eq!(s.os, "linux");
        assert!(s.device_count >= 2);
    }
}

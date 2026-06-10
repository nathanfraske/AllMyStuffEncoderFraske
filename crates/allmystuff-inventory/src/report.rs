//! Human-readable rendering of an [`Inventory`] for the CLI (`allmystuff
//! scan`). Pure formatting — no probing — so it's trivially testable and
//! reusable anywhere a text dump is handy (logs, `--text` output, bug
//! reports).

use std::fmt::Write as _;

use crate::types::*;

/// Render a full inventory as an aligned, sectioned text report.
pub fn render(inv: &Inventory) -> String {
    let mut s = String::new();
    let h = &inv.host;

    let title = format!("  {}  ", h.hostname);
    let bar = "─".repeat(title.chars().count());
    let _ = writeln!(s, "┌{bar}┐");
    let _ = writeln!(s, "│{title}│");
    let _ = writeln!(s, "└{bar}┘");

    let os_line = match &h.os_version {
        Some(v) => format!("{} {}", h.os, v),
        None => h.os.clone(),
    };
    let _ = writeln!(s, "  {}  ·  {}", os_line, h.arch);
    if let Some(board) = h.board.as_ref().or(h.soc.as_ref()) {
        let _ = writeln!(s, "  {board}");
    }
    let _ = writeln!(
        s,
        "  up {}  ·  {} devices",
        human_duration(h.uptime_secs),
        inv.device_count()
    );
    let _ = writeln!(s);

    // Compute.
    section(&mut s, "Compute");
    let cores = match inv.cpu.physical_cores {
        Some(p) => format!("{p}c / {}t", inv.cpu.logical_cores),
        None => format!("{}t", inv.cpu.logical_cores),
    };
    let mhz = inv
        .cpu
        .max_mhz
        .map(|m| format!(" @ {:.2} GHz", m as f64 / 1000.0))
        .unwrap_or_default();
    item(&mut s, "cpu", &format!("{} ({cores}){mhz}", inv.cpu.brand));
    item(
        &mut s,
        "ram",
        &format!(
            "{} ({} free)",
            human_bytes(inv.memory.total_bytes),
            human_bytes(inv.memory.available_bytes)
        ),
    );
    for g in &inv.gpus {
        let vram = g
            .vram_bytes
            .map(|b| format!(" · {} VRAM", human_bytes(b)))
            .unwrap_or_default();
        item(
            &mut s,
            "gpu",
            &format!("{} [{}]{vram}", g.name, kind_str(g.kind)),
        );
    }

    // Storage.
    if !inv.storage.is_empty() {
        section(&mut s, "Storage");
        for d in &inv.storage {
            let used = d.total_bytes.saturating_sub(d.available_bytes);
            let pct = if d.total_bytes > 0 {
                (used as f64 / d.total_bytes as f64 * 100.0).round() as u64
            } else {
                0
            };
            let mount = d.mount_point.as_deref().unwrap_or("");
            item(
                &mut s,
                disk_kind_str(d.kind),
                &format!(
                    "{} — {} of {} used ({pct}%) {mount}",
                    d.name,
                    human_bytes(used),
                    human_bytes(d.total_bytes)
                ),
            );
        }
    }

    // Network.
    if !inv.networks.is_empty() {
        section(&mut s, "Network");
        for n in &inv.networks {
            let state = if n.up { "up" } else { "down" };
            let speed = n
                .speed_mbps
                .map(|m| format!(" · {m} Mbps"))
                .unwrap_or_default();
            let addrs = {
                let mut a = n.ipv4.clone();
                a.extend(n.ipv6.iter().cloned());
                if a.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", a.join(", "))
                }
            };
            item(
                &mut s,
                net_kind_str(n.kind),
                &format!("{} [{state}]{speed}{addrs}", n.name),
            );
        }
    }

    // Displays.
    if !inv.displays.is_empty() {
        section(&mut s, "Displays");
        for d in &inv.displays {
            let res = match (d.width_px, d.height_px) {
                (Some(w), Some(h)) => format!(" {w}×{h}"),
                _ => String::new(),
            };
            let tag = if d.internal { " (built-in)" } else { "" };
            let status = if d.connected { "" } else { " — disconnected" };
            let def = default_tag(d.default);
            item(
                &mut s,
                "screen",
                &format!("{}{res}{tag}{status}{def}", d.name),
            );
        }
    }

    // Audio.
    if !inv.microphones.is_empty() || !inv.speakers.is_empty() {
        section(&mut s, "Audio");
        for m in &inv.microphones {
            let ch = m.channels.map(|c| format!(" · {c}ch")).unwrap_or_default();
            let array = if m.is_array() { "  ⟨array⟩" } else { "" };
            item(
                &mut s,
                "mic",
                &format!("{}{ch}{array}{}", m.name, default_tag(m.default)),
            );
        }
        for sp in &inv.speakers {
            item(
                &mut s,
                "speaker",
                &format!("{}{}", sp.name, default_tag(sp.default)),
            );
        }
    }

    // Cameras.
    if !inv.cameras.is_empty() {
        section(&mut s, "Cameras");
        for c in &inv.cameras {
            let path = c
                .path
                .as_deref()
                .map(|p| format!("  {p}"))
                .unwrap_or_default();
            item(
                &mut s,
                "camera",
                &format!("{}{path}{}", c.name, default_tag(c.default)),
            );
        }
    }

    // Input devices. One line per *physical* device; a unit that exposes
    // several HID interfaces says how many were folded in.
    if !inv.inputs.is_empty() {
        section(&mut s, "Input");
        for d in &inv.inputs {
            let detail = if d.endpoints > 1 {
                format!("{}  ({} endpoints)", d.name, d.endpoints)
            } else {
                d.name.clone()
            };
            item(&mut s, input_kind_str(d.kind), &detail);
        }
    }

    // Other USB.
    if !inv.usb.is_empty() {
        section(&mut s, "USB");
        for u in &inv.usb {
            let class = u
                .class
                .as_deref()
                .map(|c| format!("  [{c}]"))
                .unwrap_or_default();
            item(
                &mut s,
                "usb",
                &format!("{} ({}:{}){class}", u.name, u.vendor_id, u.product_id),
            );
        }
    }

    s
}

fn section(s: &mut String, name: &str) {
    let _ = writeln!(s, "▓ {name}");
}

/// Trailing "  ★ default" marker for the category default, else nothing.
fn default_tag(is_default: bool) -> &'static str {
    if is_default {
        "  ★ default"
    } else {
        ""
    }
}

fn item(s: &mut String, tag: &str, body: &str) {
    let _ = writeln!(s, "    {tag:<8} {body}");
}

fn kind_str(k: GpuKind) -> &'static str {
    match k {
        GpuKind::Discrete => "discrete",
        GpuKind::Integrated => "integrated",
        GpuKind::Unknown => "gpu",
    }
}

fn disk_kind_str(k: DiskKind) -> &'static str {
    match k {
        DiskKind::Ssd => "ssd",
        DiskKind::Hdd => "hdd",
        DiskKind::Removable => "usb",
        DiskKind::Unknown => "disk",
    }
}

fn net_kind_str(k: NetKind) -> &'static str {
    match k {
        NetKind::Ethernet => "eth",
        NetKind::Wifi => "wifi",
        NetKind::Loopback => "lo",
        NetKind::Virtual => "virt",
        NetKind::Cellular => "cell",
        NetKind::Bluetooth => "bt",
        NetKind::Unknown => "net",
    }
}

fn input_kind_str(k: InputKind) -> &'static str {
    match k {
        InputKind::Keyboard => "keyboard",
        InputKind::Mouse => "mouse",
        InputKind::Touchpad => "touchpad",
        InputKind::Touchscreen => "touch",
        InputKind::Gamepad => "gamepad",
        InputKind::Tablet => "tablet",
        InputKind::Other => "input",
    }
}

/// Binary-prefixed byte formatting — `16.0 GiB`, `931.5 GiB`, `512 MiB`.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes == 0 {
        return "0 B".into();
    }
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn human_duration(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_uses_binary_prefixes() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(16 * 1024 * 1024 * 1024), "16.0 GiB");
        // Large values drop the decimal once past 100 of a unit.
        assert_eq!(human_bytes(931 * 1024 * 1024 * 1024), "931 GiB");
    }

    #[test]
    fn human_duration_scales() {
        assert_eq!(human_duration(90), "1m");
        assert_eq!(human_duration(3_700), "1h 1m");
        assert_eq!(human_duration(200_000), "2d 7h");
    }

    #[test]
    fn render_includes_host_and_compute() {
        let inv = Inventory {
            scanned_at: 0,
            host: HostInfo {
                hostname: "demo-box".into(),
                os: "linux".into(),
                os_version: Some("6.1".into()),
                kernel_version: None,
                arch: "x86_64".into(),
                board: Some("Acme Laptop".into()),
                soc: None,
                uptime_secs: 3_700,
            },
            cpu: Cpu {
                brand: "Test CPU".into(),
                vendor: Some("GenuineIntel".into()),
                physical_cores: Some(4),
                logical_cores: 8,
                max_mhz: Some(3200),
            },
            memory: Memory {
                total_bytes: 16 * 1024 * 1024 * 1024,
                available_bytes: 8 * 1024 * 1024 * 1024,
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
        };
        let out = render(&inv);
        assert!(out.contains("demo-box"));
        assert!(out.contains("Test CPU"));
        assert!(out.contains("16.0 GiB"));
        assert!(out.contains("Acme Laptop"));
    }
}

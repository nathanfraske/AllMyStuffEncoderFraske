//! macOS device probing via `system_profiler -json`.
//!
//! Linux (`linux.rs`) is the reference; this is the macOS implementation of
//! the same collector surface. The host basics (CPU/memory/storage/network)
//! come from `sysinfo` cross-platform; everything here is the richer device
//! classes. Each probe is defensive — a missing data type or a shape change
//! degrades to "nothing here" rather than a panic.

#![cfg(target_os = "macos")]

use std::process::Command;

use crate::types::*;

fn system_profiler(data_type: &str) -> Option<serde_json::Value> {
    let out = Command::new("system_profiler")
        .args([data_type, "-json", "-detailLevel", "mini"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

fn sysctl(key: &str) -> Option<String> {
    let out = Command::new("sysctl").args(["-n", key]).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Friendly board label — "MacBook Pro · Apple M2".
pub fn board_label() -> Option<String> {
    let hw = system_profiler("SPHardwareDataType")?;
    let item = hw["SPHardwareDataType"].as_array()?.first()?;
    let name = item["machine_name"]
        .as_str()
        .or_else(|| item["machine_model"].as_str());
    let chip = item["chip_type"]
        .as_str()
        .or_else(|| item["cpu_type"].as_str());
    match (name, chip) {
        (Some(n), Some(c)) => Some(format!("{n} · {c}")),
        (Some(n), None) => Some(n.to_string()),
        _ => sysctl("hw.model"),
    }
}

/// Just the product / model name — the Mac's machine name ("MacBook Pro"),
/// without the ` · Apple M2` chip suffix `board_label` adds. Falls back to
/// the raw `hw.model` code ("Mac14,7") when the friendly name is absent.
pub fn product_label() -> Option<String> {
    let hw = system_profiler("SPHardwareDataType")?;
    let item = hw["SPHardwareDataType"].as_array()?.first()?;
    item["machine_name"]
        .as_str()
        .or_else(|| item["machine_model"].as_str())
        .map(str::to_string)
        .or_else(|| sysctl("hw.model"))
}

/// Apple-silicon chip label for the `soc` field ("Apple M2"). `None` on
/// Intel Macs.
pub fn soc_label() -> Option<String> {
    let hw = system_profiler("SPHardwareDataType")?;
    let chip = hw["SPHardwareDataType"].as_array()?.first()?["chip_type"].as_str()?;
    chip.to_lowercase()
        .contains("apple")
        .then(|| chip.to_string())
}

pub fn collect_gpus() -> Vec<Gpu> {
    let Some(v) = system_profiler("SPDisplaysDataType") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(items) = v["SPDisplaysDataType"].as_array() {
        for (i, gpu) in items.iter().enumerate() {
            let name = gpu["sppci_model"]
                .as_str()
                .or_else(|| gpu["_name"].as_str())
                .unwrap_or("Apple GPU")
                .to_string();
            let vendor = match gpu["spdisplays_vendor"].as_str() {
                Some(s) if s.to_lowercase().contains("nvidia") => GpuVendor::Nvidia,
                Some(s) if s.to_lowercase().contains("amd") || s.to_lowercase().contains("ati") => {
                    GpuVendor::Amd
                }
                Some(s) if s.to_lowercase().contains("intel") => GpuVendor::Intel,
                _ => GpuVendor::Apple,
            };
            // VRAM: "spdisplays_vram" / "_vram_shared" like "8 GB".
            let vram_bytes = gpu["spdisplays_vram"]
                .as_str()
                .or_else(|| gpu["spdisplays_vram_shared"].as_str())
                .and_then(parse_size_to_bytes);
            out.push(Gpu {
                id: format!("gpu:{i}"),
                name,
                vendor,
                vram_bytes,
                kind: if vendor == GpuVendor::Apple {
                    GpuKind::Integrated
                } else if vram_bytes.is_some() {
                    GpuKind::Discrete
                } else {
                    GpuKind::Unknown
                },
                driver: Some("Metal".into()),
            });
        }
    }
    out
}

pub fn collect_displays() -> Vec<Display> {
    let Some(v) = system_profiler("SPDisplaysDataType") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut idx = 0;
    if let Some(gpus) = v["SPDisplaysDataType"].as_array() {
        for gpu in gpus {
            let Some(screens) = gpu["spdisplays_ndrvs"].as_array() else {
                continue;
            };
            for s in screens {
                let name = s["_name"].as_str().unwrap_or("Display").to_string();
                let conn = s["spdisplays_connection_type"].as_str().unwrap_or("");
                let internal = conn.contains("internal") || conn.contains("Internal");
                let (w, h) = s["_spdisplays_resolution"]
                    .as_str()
                    .or_else(|| s["spdisplays_resolution"].as_str())
                    .and_then(parse_resolution)
                    .unzip();
                out.push(Display {
                    id: format!("display:{idx}"),
                    name,
                    connector: conn.to_string(),
                    connected: true,
                    width_px: w,
                    height_px: h,
                    internal,
                    default: false,
                });
                idx += 1;
            }
        }
    }
    out
}

pub fn collect_audio() -> (Vec<AudioDevice>, Vec<AudioDevice>) {
    let Some(v) = system_profiler("SPAudioDataType") else {
        return (Vec::new(), Vec::new());
    };
    let (mut mics, mut speakers) = (Vec::new(), Vec::new());
    if let Some(items) = v["SPAudioDataType"].as_array() {
        for item in items {
            let Some(devs) = item["_items"].as_array() else {
                continue;
            };
            for (i, d) in devs.iter().enumerate() {
                let name = d["_name"].as_str().unwrap_or("Audio device").to_string();
                let in_ch = d["coreaudio_device_input"]
                    .as_u64()
                    .or_else(|| d["coreaudio_input_source"].as_u64());
                let out_ch = d["coreaudio_device_output"].as_u64();
                if let Some(ch) = in_ch.filter(|&c| c > 0) {
                    mics.push(AudioDevice {
                        id: format!("mic:{i}"),
                        name: name.clone(),
                        direction: AudioDirection::Input,
                        channels: Some(ch as u32),
                        card: None,
                        default: false,
                    });
                }
                if let Some(ch) = out_ch.filter(|&c| c > 0) {
                    speakers.push(AudioDevice {
                        id: format!("spk:{i}"),
                        name,
                        direction: AudioDirection::Output,
                        channels: Some(ch as u32),
                        card: None,
                        default: false,
                    });
                }
            }
        }
    }
    (mics, speakers)
}

pub fn collect_cameras() -> Vec<Camera> {
    let Some(v) = system_profiler("SPCameraDataType") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(items) = v["SPCameraDataType"].as_array() {
        for (i, c) in items.iter().enumerate() {
            out.push(Camera {
                id: format!("cam:{i}"),
                name: c["_name"].as_str().unwrap_or("Camera").to_string(),
                path: None,
                default: false,
            });
        }
    }
    out
}

pub fn collect_usb() -> Vec<UsbDevice> {
    let Some(v) = system_profiler("SPUSBDataType") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(buses) = v["SPUSBDataType"].as_array() {
        for bus in buses {
            walk_usb(bus, &mut out);
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// USB items nest under `_items`; recurse, emitting any node that carries a
/// vendor/product id (hubs and the root bus don't).
fn walk_usb(node: &serde_json::Value, out: &mut Vec<UsbDevice>) {
    let vid = node["vendor_id"].as_str().map(clean_hex_id);
    let pid = node["product_id"].as_str().map(clean_hex_id);
    if let (Some(vid), Some(pid)) = (vid, pid) {
        let name = node["_name"].as_str().unwrap_or("USB device").to_string();
        let manufacturer = node["manufacturer"].as_str().map(str::to_string);
        out.push(UsbDevice {
            id: format!("usb:{vid}:{pid}"),
            name,
            vendor_id: vid,
            product_id: pid,
            manufacturer,
            class: None,
        });
    }
    if let Some(children) = node["_items"].as_array() {
        for c in children {
            walk_usb(c, out);
        }
    }
}

// macOS keyboards/mice are HID (IOKit) rather than something
// `system_profiler` lists cleanly; external USB ones surface in the USB
// list above, and the synthetic per-machine "control" capability covers
// driving the Mac itself. Built-in HID enumeration is a follow-up.
pub fn collect_inputs() -> Vec<InputDevice> {
    Vec::new()
}

// ---- parsing helpers (pure) ------------------------------------------

/// `"2560 x 1440"` → `(2560, 1440)`.
fn parse_resolution(s: &str) -> Option<(u32, u32)> {
    let s = s.split('@').next().unwrap_or(s);
    let (w, h) = s.split_once(['x', 'X'])?;
    Some((
        w.replace(' ', "").parse().ok()?,
        h.split_whitespace().next()?.parse().ok()?,
    ))
}

/// `"8 GB"` / `"1536 MB"` → bytes.
fn parse_size_to_bytes(s: &str) -> Option<u64> {
    let mut it = s.split_whitespace();
    let n: f64 = it.next()?.parse().ok()?;
    let mult = match it.next().map(|u| u.to_lowercase()).as_deref() {
        Some("gb") => 1024u64.pow(3),
        Some("mb") => 1024u64.pow(2),
        Some("kb") => 1024,
        _ => 1,
    };
    Some((n * mult as f64) as u64)
}

/// system_profiler vendor/product ids look like `"0x05ac"`; normalise to
/// 4-hex-digit lowercase.
fn clean_hex_id(s: &str) -> String {
    let t = s.trim().trim_start_matches("0x");
    format!("{:0>4}", t.to_lowercase())
}

/// Enumerate the TCP ports this machine is listening on, via `lsof` (there's
/// no `/proc/net/tcp` on macOS). `-n -P` skip DNS/port-name lookups; `-iTCP
/// -sTCP:LISTEN` selects listening TCP sockets. Run as the user, lsof sees
/// the user's own servers (a dev server, a database) without elevation.
/// Degrades to an empty list if lsof isn't there or finds nothing.
pub fn collect_listening() -> Vec<ListeningService> {
    let Ok(out) = Command::new("lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN"])
        .output()
    else {
        return Vec::new();
    };
    // lsof exits non-zero when nothing matches; parse whatever it printed.
    let text = String::from_utf8_lossy(&out.stdout);
    crate::listening::services_from_lsof(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_resolution() {
        assert_eq!(parse_resolution("2560 x 1440"), Some((2560, 1440)));
        assert_eq!(
            parse_resolution("3840 x 2160 @ 60.00Hz"),
            Some((3840, 2160))
        );
        assert_eq!(parse_resolution("nope"), None);
    }

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size_to_bytes("8 GB"), Some(8 * 1024u64.pow(3)));
        assert_eq!(parse_size_to_bytes("1536 MB"), Some(1536 * 1024u64.pow(2)));
    }

    #[test]
    fn cleans_hex_ids() {
        assert_eq!(clean_hex_id("0x05ac"), "05ac");
        assert_eq!(clean_hex_id("46d"), "046d");
    }
}

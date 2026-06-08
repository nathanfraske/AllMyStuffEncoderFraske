//! macOS device probing.
//!
//! Linux is the reference implementation (`linux.rs`); this module
//! mirrors its collector surface using `system_profiler -json` and is
//! compiled only on macOS. The host basics (CPU/memory/storage/network)
//! come from `sysinfo` cross-platform, so what's left here is the richer
//! display/audio/camera classes. Collectors that aren't wired yet return
//! empty rather than guess — the scan still produces a complete,
//! correctly-typed `Inventory`.

#![cfg(target_os = "macos")]

use std::process::Command;

use crate::types::*;

fn system_profiler(data_type: &str) -> Option<serde_json::Value> {
    let out = Command::new("system_profiler")
        .args([data_type, "-json"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
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
                .unwrap_or("Apple GPU")
                .to_string();
            out.push(Gpu {
                id: format!("gpu:{i}"),
                name,
                vendor: GpuVendor::Apple,
                vram_bytes: None,
                kind: GpuKind::Integrated,
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
    if let Some(gpus) = v["SPDisplaysDataType"].as_array() {
        for gpu in gpus {
            let Some(screens) = gpu["spdisplays_ndrvs"].as_array() else {
                continue;
            };
            for (i, s) in screens.iter().enumerate() {
                let name = s["_name"].as_str().unwrap_or("Display").to_string();
                let internal = s["spdisplays_connection_type"]
                    .as_str()
                    .map(|c| c.contains("internal"))
                    .unwrap_or(false);
                out.push(Display {
                    id: format!("display:{i}:{name}"),
                    name: name.clone(),
                    connector: name,
                    connected: true,
                    width_px: None,
                    height_px: None,
                    internal,
                });
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
            if let Some(devs) = item["_items"].as_array() {
                for (i, d) in devs.iter().enumerate() {
                    let name = d["_name"].as_str().unwrap_or("Audio").to_string();
                    let in_ch = d["coreaudio_device_input"].as_u64();
                    let out_ch = d["coreaudio_device_output"].as_u64();
                    if let Some(ch) = in_ch {
                        if ch > 0 {
                            mics.push(AudioDevice {
                                id: format!("mic:{i}:{name}"),
                                name: name.clone(),
                                direction: AudioDirection::Input,
                                channels: Some(ch as u32),
                                card: None,
                            });
                        }
                    }
                    if out_ch.map(|c| c > 0).unwrap_or(false) {
                        speakers.push(AudioDevice {
                            id: format!("spk:{i}:{name}"),
                            name,
                            direction: AudioDirection::Output,
                            channels: out_ch.map(|c| c as u32),
                            card: None,
                        });
                    }
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
            let name = c["_name"].as_str().unwrap_or("Camera").to_string();
            out.push(Camera {
                id: format!("cam:{i}:{name}"),
                name,
                path: None,
            });
        }
    }
    out
}

// USB + input enumeration on macOS go through IOKit; not yet wired.
// Returning empty keeps the scan well-formed.
pub fn collect_inputs() -> Vec<InputDevice> {
    Vec::new()
}

pub fn collect_usb() -> Vec<UsbDevice> {
    Vec::new()
}

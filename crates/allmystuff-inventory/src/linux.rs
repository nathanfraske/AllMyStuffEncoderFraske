//! Linux device probing via `/proc` and `/sys`.
//!
//! Two halves on purpose:
//!
//!  * **`collect_*`** functions read the live filesystem. They are
//!    defensive to a fault — every missing file, permission error, or
//!    malformed line degrades to "nothing here" rather than a panic, so
//!    the scan returns *something useful* inside a locked-down container
//!    (where most of `/sys/class/*` simply isn't mounted) exactly as it
//!    does on a fully-loaded desktop.
//!  * **`parse_*` / `edid_*`** functions are pure: bytes/str in, typed
//!    data out. They carry the unit tests, so the fiddly bit-twiddling
//!    (EDID timing descriptors, ALSA stream channel counts, input-device
//!    handler classification) is verified against real-world fixtures
//!    without needing the hardware present. Same split MyOwnLLM uses for
//!    its `/proc/meminfo` and `df` parsers.

#![cfg(target_os = "linux")]

use std::fs;
use std::path::Path;

use crate::types::*;

// =======================================================================
// host: board + SoC
// =======================================================================

/// Friendly board label from DMI — "Dell Inc. XPS 15 9520". `None`
/// inside VMs / containers where DMI is stripped.
pub fn board_label() -> Option<String> {
    let vendor = read_trim("/sys/class/dmi/id/sys_vendor");
    let product = read_trim("/sys/class/dmi/id/product_name");
    parse_dmi_board(vendor.as_deref(), product.as_deref())
}

pub fn soc_label() -> Option<String> {
    if let Ok(raw) = fs::read("/proc/device-tree/model") {
        if let Some(label) = parse_device_tree_model(&raw) {
            return Some(label);
        }
    }
    if let Ok(content) = fs::read_to_string("/proc/cpuinfo") {
        if let Some(label) = parse_cpuinfo_soc(&content) {
            return Some(label);
        }
    }
    None
}

fn parse_dmi_board(vendor: Option<&str>, product: Option<&str>) -> Option<String> {
    let bad = |s: &str| {
        let l = s.to_lowercase();
        s.is_empty()
            || l.contains("to be filled")
            || l.contains("system manufacturer")
            || l.contains("default string")
            || l == "none"
    };
    let vendor = vendor.filter(|v| !bad(v));
    let product = product.filter(|p| !bad(p))?;
    Some(match vendor {
        Some(v) if !product.starts_with(v) => format!("{v} {product}"),
        _ => product.to_string(),
    })
}

/// `/proc/device-tree/model` — NUL-terminated string on Pi / ARM SBCs.
fn parse_device_tree_model(raw: &[u8]) -> Option<String> {
    let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
    let s = std::str::from_utf8(raw.get(..end)?).ok()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// ARM kernels emit `Model : Raspberry Pi 5 Model B …` and/or
/// `Hardware : BCM2712`. Prefer the human-friendly Model line. x86
/// cpuinfo has neither, so this returns `None` there.
fn parse_cpuinfo_soc(content: &str) -> Option<String> {
    let mut hardware = None;
    for line in content.lines() {
        let (k, v) = match line.split_once(':') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => continue,
        };
        match k {
            "Model" if !v.is_empty() => return Some(v.to_string()),
            "Hardware" if !v.is_empty() => hardware = Some(v.to_string()),
            _ => {}
        }
    }
    hardware
}

/// CPU marketing string from `/proc/cpuinfo` (`model name`). sysinfo
/// usually has this, but some minimal VMs blank the sysinfo field while
/// keeping the cpuinfo line, so we keep a fallback.
pub fn cpu_brand_fallback() -> Option<String> {
    let content = fs::read_to_string("/proc/cpuinfo").ok()?;
    for line in content.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim() == "model name" && !v.trim().is_empty() {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

// =======================================================================
// displays: /sys/class/drm + EDID
// =======================================================================

pub fn collect_displays() -> Vec<Display> {
    let mut out = Vec::new();
    let dir = match fs::read_dir("/sys/class/drm") {
        Ok(d) => d,
        Err(_) => return out,
    };
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Connector dirs look like `card0-HDMI-A-1`; bare `card0` is the
        // GPU itself (handled by the GPU probe).
        if !name.contains('-') {
            continue;
        }
        let path = entry.path();
        let status = read_trim(path.join("status")).unwrap_or_default();
        let connected = status == "connected";
        let (connector, internal) = parse_drm_connector(&name);

        // Resolution: prefer EDID's preferred timing, fall back to the
        // first line of `modes`.
        let mut width = None;
        let mut height = None;
        let mut monitor_name = None;
        if let Ok(edid) = fs::read(path.join("edid")) {
            if let Some(info) = edid_parse(&edid) {
                monitor_name = info.monitor_name.or(info.manufacturer);
                if let Some((w, h)) = info.preferred {
                    width = Some(w);
                    height = Some(h);
                }
            }
        }
        if width.is_none() {
            if let Some(modes) = read_trim(path.join("modes")) {
                if let Some((w, h)) = parse_drm_mode(modes.lines().next().unwrap_or("")) {
                    width = Some(w);
                    height = Some(h);
                }
            }
        }

        out.push(Display {
            id: format!("display:{connector}"),
            name: monitor_name.unwrap_or_else(|| connector.clone()),
            connector,
            connected,
            width_px: width,
            height_px: height,
            internal,
        });
    }
    out.sort_by(|a, b| a.connector.cmp(&b.connector));
    out
}

/// `card0-HDMI-A-1` → (`HDMI-A-1`, false). Internal panels (eDP / LVDS /
/// DSI) are the built-in laptop screen.
fn parse_drm_connector(dir_name: &str) -> (String, bool) {
    let connector = dir_name
        .split_once('-')
        .map(|(_, rest)| rest.to_string())
        .unwrap_or_else(|| dir_name.to_string());
    let upper = connector.to_uppercase();
    let internal =
        upper.starts_with("EDP") || upper.starts_with("LVDS") || upper.starts_with("DSI");
    (connector, internal)
}

/// First entry of a drm `modes` file: `1920x1080`.
fn parse_drm_mode(line: &str) -> Option<(u32, u32)> {
    let (w, h) = line.trim().split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// Decoded fields from the 128-byte EDID base block.
#[derive(Debug, Default, PartialEq, Eq)]
struct EdidInfo {
    manufacturer: Option<String>,
    monitor_name: Option<String>,
    preferred: Option<(u32, u32)>,
}

/// Parse the EDID base block. Validates the fixed 8-byte header, then
/// reads the PNP manufacturer id, the first detailed-timing descriptor
/// (the preferred resolution), and the monitor-name descriptor (tag
/// 0xFC). Anything malformed yields `None`/missing rather than a panic —
/// EDIDs in the wild are frequently truncated or zero-padded.
fn edid_parse(edid: &[u8]) -> Option<EdidInfo> {
    const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
    if edid.len() < 128 || edid[..8] != HEADER {
        return None;
    }
    let mut info = EdidInfo {
        manufacturer: edid_manufacturer(edid),
        ..Default::default()
    };
    // Four 18-byte descriptors at 54/72/90/108. The first is the
    // preferred detailed timing; a descriptor whose first two bytes are
    // zero is a "display descriptor" carrying text instead.
    for base in [54usize, 72, 90, 108] {
        let d = &edid[base..base + 18];
        if d[0] == 0 && d[1] == 0 {
            // Display descriptor. Tag at byte 3; 0xFC = monitor name.
            if d[3] == 0xFC {
                let text: String = d[5..18]
                    .iter()
                    .take_while(|&&b| b != 0x0A)
                    .map(|&b| b as char)
                    .collect();
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    info.monitor_name = Some(trimmed.to_string());
                }
            }
        } else if info.preferred.is_none() {
            // Detailed timing: 12-bit active pixel counts split across
            // bytes 2/4 (horizontal) and 5/7 (vertical).
            let h = ((d[4] as u32 & 0xF0) << 4) | d[2] as u32;
            let v = ((d[7] as u32 & 0xF0) << 4) | d[5] as u32;
            if h > 0 && v > 0 {
                info.preferred = Some((h, v));
            }
        }
    }
    Some(info)
}

/// Three-letter PNP vendor id packed into EDID bytes 8-9 (five bits per
/// letter, `1 = 'A'`). "DEL", "SAM", "GSM"…
fn edid_manufacturer(edid: &[u8]) -> Option<String> {
    let b8 = edid.get(8).copied()? as u16;
    let b9 = edid.get(9).copied()? as u16;
    let packed = (b8 << 8) | b9;
    let letter = |shift: u16| -> Option<char> {
        let v = ((packed >> shift) & 0x1F) as u8;
        (1..=26).contains(&v).then(|| (b'A' + v - 1) as char)
    };
    let s: String = [letter(10)?, letter(5)?, letter(0)?].iter().collect();
    Some(s)
}

// =======================================================================
// audio: /proc/asound
// =======================================================================

pub fn collect_audio() -> (Vec<AudioDevice>, Vec<AudioDevice>) {
    let mut mics = Vec::new();
    let mut speakers = Vec::new();

    let cards = fs::read_to_string("/proc/asound/cards")
        .map(|s| parse_alsa_cards(&s))
        .unwrap_or_default();
    let card_name = |idx: u32| -> Option<String> {
        cards
            .iter()
            .find(|c| c.index == idx)
            .map(|c| c.name.clone())
    };

    let pcm = match fs::read_to_string("/proc/asound/pcm") {
        Ok(s) => s,
        Err(_) => return (mics, speakers),
    };
    for line in pcm.lines() {
        let Some(p) = parse_alsa_pcm_line(line) else {
            continue;
        };
        let name = card_name(p.card).unwrap_or(p.name);
        if p.capture {
            let channels = alsa_capture_channels(p.card, p.device);
            mics.push(AudioDevice {
                id: format!("mic:{}:{}", p.card, p.device),
                name: name.clone(),
                direction: AudioDirection::Input,
                channels,
                card: Some(p.card.to_string()),
            });
        }
        if p.playback {
            speakers.push(AudioDevice {
                id: format!("spk:{}:{}", p.card, p.device),
                name,
                direction: AudioDirection::Output,
                channels: None,
                card: Some(p.card.to_string()),
            });
        }
    }
    (mics, speakers)
}

/// Channel count for a USB-Audio capture endpoint, read from
/// `/proc/asound/card<N>/stream<dev>`. Only present (and only meaningful)
/// for USB gadgets — which is exactly the consumer mic-array case
/// (ReSpeaker, conference pucks). HDA codecs don't expose this statically,
/// so those stay `None`.
fn alsa_capture_channels(card: u32, dev: u32) -> Option<u32> {
    let text = fs::read_to_string(format!("/proc/asound/card{card}/stream{dev}")).ok()?;
    parse_alsa_stream_capture_channels(&text)
}

struct AlsaCard {
    index: u32,
    name: String,
}

/// `/proc/asound/cards`:
/// ` 1 [USB    ]: USB-Audio - ReSpeaker 4 Mic Array`
fn parse_alsa_cards(text: &str) -> Vec<AlsaCard> {
    let mut out = Vec::new();
    for line in text.lines() {
        // Card header lines start with the index; the following
        // indented line is a longer description we ignore.
        let trimmed = line.trim_start();
        if trimmed.starts_with(char::is_numeric) && line.contains(']') && line.contains(':') {
            if let Some(index) = trimmed
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
            {
                // Friendly name is everything after the final ` - `, or
                // after `]:` if there's no driver dash.
                let name = line
                    .rsplit_once(" - ")
                    .map(|(_, n)| n.trim())
                    .or_else(|| line.split_once("]:").map(|(_, n)| n.trim()))
                    .unwrap_or("")
                    .to_string();
                out.push(AlsaCard { index, name });
            }
        }
    }
    out
}

struct PcmInfo {
    card: u32,
    device: u32,
    name: String,
    playback: bool,
    capture: bool,
}

/// `/proc/asound/pcm`:
/// `01-00: USB Audio : USB Audio : capture 1`
fn parse_alsa_pcm_line(line: &str) -> Option<PcmInfo> {
    let (id, rest) = line.split_once(':')?;
    let (card, device) = id.trim().split_once('-')?;
    let fields: Vec<&str> = rest.split(':').map(str::trim).collect();
    Some(PcmInfo {
        card: card.parse().ok()?,
        device: device.parse().ok()?,
        name: fields.first().copied().unwrap_or("audio").to_string(),
        playback: fields.iter().any(|f| f.starts_with("playback")),
        capture: fields.iter().any(|f| f.starts_with("capture")),
    })
}

/// Pull `Channels: N` from the `Capture:` section of a USB-Audio stream
/// descriptor. Stops at the next top-level section so a 2-ch playback
/// block doesn't get mistaken for the capture count.
fn parse_alsa_stream_capture_channels(text: &str) -> Option<u32> {
    let mut in_capture = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("Capture:") {
            in_capture = true;
            continue;
        }
        if t.starts_with("Playback:") {
            in_capture = false;
            continue;
        }
        if in_capture {
            if let Some(rest) = t.strip_prefix("Channels:") {
                return rest.trim().parse().ok();
            }
        }
    }
    None
}

// =======================================================================
// cameras: /sys/class/video4linux
// =======================================================================

pub fn collect_cameras() -> Vec<Camera> {
    let mut out = Vec::new();
    let dir = match fs::read_dir("/sys/class/video4linux") {
        Ok(d) => d,
        Err(_) => return out,
    };
    for entry in dir.flatten() {
        let sysname = entry.file_name().to_string_lossy().to_string();
        // Many capture chips expose several /dev/videoN nodes (capture +
        // metadata). Keep the first per device by reading `index` == 0
        // when available; otherwise list them all rather than risk
        // dropping a real camera.
        if let Some(idx) = read_trim(entry.path().join("device/index")) {
            if idx != "0" {
                continue;
            }
        }
        let name = read_trim(entry.path().join("name")).unwrap_or_else(|| sysname.clone());
        out.push(Camera {
            id: format!("cam:{sysname}"),
            name,
            path: Some(format!("/dev/{sysname}")),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

// =======================================================================
// input: /proc/bus/input/devices
// =======================================================================

pub fn collect_inputs() -> Vec<InputDevice> {
    let text = match fs::read_to_string("/proc/bus/input/devices") {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    parse_input_devices(&text)
        .into_iter()
        // Drop the kernel's virtual/system pseudo-inputs (power button,
        // lid switch, PC speaker…) — they're not "things you plugged in."
        .filter(|d| !is_system_input(&d.name))
        .collect()
}

fn is_system_input(name: &str) -> bool {
    let l = name.to_lowercase();
    [
        "power button",
        "sleep button",
        "lid switch",
        "video bus",
        "pc speaker",
        "hda ",
        "hdmi",
        "gpio",
    ]
    .iter()
    .any(|s| l.contains(s))
}

/// Parse the blank-line-separated device blocks of
/// `/proc/bus/input/devices`, classifying each from its `Name=` and
/// `Handlers=` lines (with `js*` → gamepad, `kbd` → keyboard, `mouse` →
/// mouse/touchpad). Returns every device; the live collector applies the
/// system-input denylist.
fn parse_input_devices(text: &str) -> Vec<InputDevice> {
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        let mut name = None;
        let mut handlers = "";
        let mut sysfs = "";
        for line in block.lines() {
            if let Some(rest) = line.strip_prefix("N: Name=") {
                name = Some(rest.trim().trim_matches('"').to_string());
            } else if let Some(rest) = line.strip_prefix("H: Handlers=") {
                handlers = rest.trim();
            } else if let Some(rest) = line.strip_prefix("S: Sysfs=") {
                sysfs = rest.trim();
            }
        }
        let Some(name) = name else { continue };
        let kind = classify_input(&name, handlers);
        // Stable id from the sysfs leaf when present, else the slugged
        // name, so two identically-named devices don't collide.
        let leaf = sysfs.rsplit('/').next().unwrap_or("");
        let id = if leaf.is_empty() {
            format!("input:{}", slug(&name))
        } else {
            format!("input:{leaf}")
        };
        out.push(InputDevice { id, name, kind });
    }
    out
}

fn classify_input(name: &str, handlers: &str) -> InputKind {
    let n = name.to_lowercase();
    let h = handlers.to_lowercase();
    if h.split_whitespace().any(|t| t.starts_with("js")) {
        return InputKind::Gamepad;
    }
    if n.contains("wacom") || n.contains("tablet") || n.contains("pen") {
        return InputKind::Tablet;
    }
    if n.contains("touchscreen") {
        return InputKind::Touchscreen;
    }
    if h.contains("kbd") {
        return InputKind::Keyboard;
    }
    if h.contains("mouse") {
        if n.contains("touchpad") || n.contains("trackpad") {
            return InputKind::Touchpad;
        }
        return InputKind::Mouse;
    }
    InputKind::Other
}

// =======================================================================
// usb: /sys/bus/usb/devices
// =======================================================================

pub fn collect_usb() -> Vec<UsbDevice> {
    let mut out = Vec::new();
    let dir = match fs::read_dir("/sys/bus/usb/devices") {
        Ok(d) => d,
        Err(_) => return out,
    };
    for entry in dir.flatten() {
        let path = entry.path();
        // Real devices have idVendor/idProduct; root hubs and interface
        // nodes (`1-1:1.0`) don't.
        let (Some(vid), Some(pid)) = (
            read_trim(path.join("idVendor")),
            read_trim(path.join("idProduct")),
        ) else {
            continue;
        };
        let manufacturer = read_trim(path.join("manufacturer"));
        let product = read_trim(path.join("product"));
        let class = read_trim(path.join("bDeviceClass"))
            .and_then(|c| usb_class_label(&c).map(str::to_string));
        // Skip the host's own root hubs — vid 1d6b is "Linux Foundation".
        if vid == "1d6b" {
            continue;
        }
        let name = product
            .clone()
            .or_else(|| manufacturer.clone())
            .unwrap_or_else(|| format!("USB {vid}:{pid}"));
        out.push(UsbDevice {
            id: format!("usb:{vid}:{pid}"),
            name,
            vendor_id: vid,
            product_id: pid,
            manufacturer,
            class,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// Friendly label for a USB `bDeviceClass` byte. `00` means "see the
/// interface descriptors", which we don't crawl, so it maps to `None`.
fn usb_class_label(hex: &str) -> Option<&'static str> {
    match hex.trim().to_lowercase().as_str() {
        "01" => Some("Audio"),
        "02" => Some("Communications"),
        "03" => Some("HID"),
        "05" => Some("Physical"),
        "06" => Some("Imaging"),
        "07" => Some("Printer"),
        "08" => Some("Mass Storage"),
        "09" => Some("Hub"),
        "0a" => Some("Data"),
        "0b" => Some("Smart Card"),
        "0e" => Some("Video"),
        "dc" => Some("Diagnostic"),
        "e0" => Some("Wireless"),
        "ef" => Some("Misc"),
        "fe" => Some("Application"),
        "ff" => Some("Vendor Specific"),
        _ => None,
    }
}

// =======================================================================
// gpu: /sys/class/drm/card*/device
// =======================================================================

pub fn collect_gpus() -> Vec<Gpu> {
    let mut out = Vec::new();
    let dir = match fs::read_dir("/sys/class/drm") {
        Ok(d) => d,
        Err(_) => return out,
    };
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Bare `cardN` (no connector suffix) is the GPU device node.
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let dev = entry.path().join("device");
        let Some(vendor_hex) = read_trim(dev.join("vendor")) else {
            continue;
        };
        let vendor = match vendor_hex.to_lowercase().as_str() {
            "0x10de" => GpuVendor::Nvidia,
            "0x1002" | "0x1022" => GpuVendor::Amd,
            "0x8086" => GpuVendor::Intel,
            _ => GpuVendor::Other,
        };
        let driver = fs::read_link(dev.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().to_string()));
        // amdgpu exposes total VRAM in bytes; NVIDIA needs NVML/nvidia-smi
        // (handled in the common probe), Intel is integrated.
        let vram_bytes = read_trim(dev.join("mem_info_vram_total")).and_then(|s| s.parse().ok());
        let kind = match vendor {
            GpuVendor::Intel => GpuKind::Integrated,
            _ if vram_bytes.is_some() => GpuKind::Discrete,
            _ => GpuKind::Unknown,
        };
        out.push(Gpu {
            id: format!("gpu:{name}"),
            name: gpu_vendor_name(vendor),
            vendor,
            vram_bytes,
            kind,
            driver,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn gpu_vendor_name(v: GpuVendor) -> String {
    match v {
        GpuVendor::Nvidia => "NVIDIA GPU",
        GpuVendor::Amd => "AMD GPU",
        GpuVendor::Intel => "Intel Graphics",
        GpuVendor::Apple => "Apple GPU",
        GpuVendor::Other => "GPU",
    }
    .to_string()
}

// =======================================================================
// network: enrich sysinfo with /sys/class/net link type + speed
// =======================================================================

/// Classify an interface and read its link speed from sysfs. The common
/// (sysinfo) probe already has the name + MAC + addresses; this fills the
/// Linux-only `kind`/`up`/`speed` detail.
pub fn net_detail(iface: &str) -> (NetKind, bool, Option<u64>) {
    let base = Path::new("/sys/class/net").join(iface);
    let kind = if iface == "lo" {
        NetKind::Loopback
    } else if base.join("wireless").exists() || base.join("phy80211").exists() {
        NetKind::Wifi
    } else if base.join("tun_flags").exists()
        || iface.starts_with("docker")
        || iface.starts_with("veth")
        || iface.starts_with("br-")
        || iface.starts_with("virbr")
    {
        NetKind::Virtual
    } else if read_trim(base.join("type")).as_deref() == Some("1") {
        // ARPHRD_ETHER. Could still be Wi-Fi, but the wireless dir check
        // above already caught those.
        NetKind::Ethernet
    } else {
        NetKind::Unknown
    };
    let up = read_trim(base.join("operstate")).as_deref() == Some("up");
    let speed = read_trim(base.join("speed"))
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|&s| s > 0)
        .map(|s| s as u64);
    (kind, up, speed)
}

// =======================================================================
// helpers
// =======================================================================

fn read_trim(path: impl AsRef<Path>) -> Option<String> {
    let s = fs::read_to_string(path).ok()?;
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

// =======================================================================
// tests — pure parsers against real-world fixtures
// =======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dmi_board_combines_vendor_and_product() {
        assert_eq!(
            parse_dmi_board(Some("Dell Inc."), Some("XPS 15 9520")).as_deref(),
            Some("Dell Inc. XPS 15 9520")
        );
    }

    #[test]
    fn dmi_board_rejects_placeholder_strings() {
        assert_eq!(
            parse_dmi_board(Some("System manufacturer"), Some("To be filled by O.E.M.")),
            None
        );
        assert_eq!(parse_dmi_board(None, None), None);
    }

    #[test]
    fn dmi_board_skips_redundant_vendor_prefix() {
        // Some products already embed the vendor; don't double it.
        assert_eq!(
            parse_dmi_board(Some("ASUS"), Some("ASUS TUF Gaming")).as_deref(),
            Some("ASUS TUF Gaming")
        );
    }

    #[test]
    fn device_tree_model_trims_nul() {
        assert_eq!(
            parse_device_tree_model(b"Raspberry Pi 5 Model B Rev 1.0\0").as_deref(),
            Some("Raspberry Pi 5 Model B Rev 1.0")
        );
        assert_eq!(parse_device_tree_model(b"\0"), None);
    }

    #[test]
    fn cpuinfo_soc_prefers_model_over_hardware() {
        let s = "processor : 0\nHardware : BCM2712\nModel : Raspberry Pi 5 Model B Rev 1.0\n";
        assert_eq!(
            parse_cpuinfo_soc(s).as_deref(),
            Some("Raspberry Pi 5 Model B Rev 1.0")
        );
    }

    #[test]
    fn cpuinfo_soc_none_on_x86() {
        let s = "processor : 0\nvendor_id : GenuineIntel\nmodel name : Intel Core i7\n";
        assert_eq!(parse_cpuinfo_soc(s), None);
    }

    #[test]
    fn drm_connector_classifies_internal_panel() {
        assert_eq!(parse_drm_connector("card0-eDP-1"), ("eDP-1".into(), true));
        assert_eq!(
            parse_drm_connector("card0-HDMI-A-1"),
            ("HDMI-A-1".into(), false)
        );
        assert_eq!(parse_drm_connector("card1-DP-2"), ("DP-2".into(), false));
    }

    #[test]
    fn drm_mode_parses_resolution() {
        assert_eq!(parse_drm_mode("3840x2160"), Some((3840, 2160)));
        assert_eq!(parse_drm_mode("garbage"), None);
    }

    /// Build a minimal-but-valid EDID base block: header, a "DEL"
    /// manufacturer, a 2560x1440 preferred detailed timing at offset 54,
    /// and a monitor-name descriptor ("U2720Q") at offset 72.
    fn sample_edid() -> Vec<u8> {
        let mut e = vec![0u8; 128];
        e[..8].copy_from_slice(&[0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0]);
        // "DEL" → D=4,E=5,L=12 → packed 0b00100_00101_01100 = 0x10AC.
        e[8] = 0x10;
        e[9] = 0xAC;
        // Preferred timing @54: h_active 2560 = 0xA00 → low 0x00, hi nibble 0xA.
        // v_active 1440 = 0x5A0 → low 0xA0, hi nibble 0x5.
        e[54] = 0x01; // non-zero so it's read as a detailed timing
        e[54 + 2] = 0x00; // h low
        e[54 + 4] = 0xA0; // h high nibble = A
        e[54 + 5] = 0xA0; // v low
        e[54 + 7] = 0x50; // v high nibble = 5
                          // Monitor-name descriptor @72.
        e[72 + 3] = 0xFC;
        let nm = b"U2720Q\n";
        e[72 + 5..72 + 5 + nm.len()].copy_from_slice(nm);
        e
    }

    #[test]
    fn edid_decodes_manufacturer_name_and_resolution() {
        let info = edid_parse(&sample_edid()).expect("valid edid");
        assert_eq!(info.manufacturer.as_deref(), Some("DEL"));
        assert_eq!(info.monitor_name.as_deref(), Some("U2720Q"));
        assert_eq!(info.preferred, Some((2560, 1440)));
    }

    #[test]
    fn edid_rejects_bad_header() {
        assert_eq!(edid_parse(&[0u8; 128]), None);
        assert_eq!(edid_parse(&[0xFF; 4]), None);
    }

    #[test]
    fn alsa_cards_parses_index_and_name() {
        let s = " 0 [PCH            ]: HDA-Intel - HDA Intel PCH\n\
                  HDA Intel PCH at 0xed340000 irq 145\n \
                  1 [Array          ]: USB-Audio - ReSpeaker 4 Mic Array\n";
        let cards = parse_alsa_cards(s);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].index, 0);
        assert_eq!(cards[0].name, "HDA Intel PCH");
        assert_eq!(cards[1].name, "ReSpeaker 4 Mic Array");
    }

    #[test]
    fn alsa_pcm_line_reads_direction() {
        let p =
            parse_alsa_pcm_line("00-00: ALC256 Analog : ALC256 Analog : playback 1 : capture 1")
                .unwrap();
        assert_eq!((p.card, p.device), (0, 0));
        assert!(p.playback && p.capture);

        let c = parse_alsa_pcm_line("01-00: USB Audio : USB Audio : capture 1").unwrap();
        assert!(c.capture && !c.playback);

        assert!(parse_alsa_pcm_line("not a pcm line").is_none());
    }

    #[test]
    fn alsa_stream_reads_capture_channels_not_playback() {
        let s = "ReSpeaker 4 Mic Array\n\
                 \n  Playback:\n    Channels: 2\n\
                 \n  Capture:\n    Status: Stop\n    Format: S16_LE\n    Channels: 4\n";
        assert_eq!(parse_alsa_stream_capture_channels(s), Some(4));
    }

    #[test]
    fn input_classification_covers_the_common_peripherals() {
        let text = "I: Bus=0011 Vendor=0001 Product=0001\n\
                    N: Name=\"AT Translated Set 2 keyboard\"\n\
                    S: Sysfs=/devices/platform/i8042/serio0/input/input1\n\
                    H: Handlers=sysrq kbd event1 \n\
                    \n\
                    I: Bus=0003 Vendor=046d Product=c52b\n\
                    N: Name=\"Logitech USB Receiver Mouse\"\n\
                    S: Sysfs=/devices/pci0000:00/usb1/1-1/input/input5\n\
                    H: Handlers=mouse0 event5 \n\
                    \n\
                    I: Bus=0018 Vendor=06cb Product=7f28\n\
                    N: Name=\"SYNA8004:00 06CB:CD8B Touchpad\"\n\
                    S: Sysfs=/devices/pci0000:00/i2c/input/input12\n\
                    H: Handlers=mouse1 event12 \n\
                    \n\
                    I: Bus=0003 Vendor=045e Product=028e\n\
                    N: Name=\"Microsoft X-Box 360 pad\"\n\
                    S: Sysfs=/devices/pci0000:00/usb2/2-1/input/input20\n\
                    H: Handlers=event20 js0 \n";
        let devs = parse_input_devices(text);
        assert_eq!(devs.len(), 4);
        assert_eq!(devs[0].kind, InputKind::Keyboard);
        assert_eq!(devs[0].id, "input:input1");
        assert_eq!(devs[1].kind, InputKind::Mouse);
        assert_eq!(devs[2].kind, InputKind::Touchpad);
        assert_eq!(devs[3].kind, InputKind::Gamepad);
    }

    #[test]
    fn system_inputs_are_filtered() {
        assert!(is_system_input("Power Button"));
        assert!(is_system_input("Lid Switch"));
        assert!(!is_system_input("Logitech USB Receiver"));
    }

    #[test]
    fn usb_class_labels() {
        assert_eq!(usb_class_label("0e"), Some("Video"));
        assert_eq!(usb_class_label("08"), Some("Mass Storage"));
        assert_eq!(usb_class_label("00"), None);
    }
}

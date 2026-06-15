//! Windows device probing via PowerShell + CIM (`Get-CimInstance`).
//!
//! Linux (`linux.rs`) is the reference; this is the Windows implementation
//! of the same collector surface. Host basics (CPU/memory/storage/network)
//! come from `sysinfo`; everything here queries CIM and parses the JSON.
//! Each probe is defensive — a failed query or a shape change degrades to
//! "nothing here" rather than a panic.

#![cfg(target_os = "windows")]

use std::os::windows::process::CommandExt as _;
use std::process::Command;

use crate::types::*;

/// Each console-subsystem child of a windowless (GUI-subsystem) parent
/// gets a fresh visible console on Windows — one flashing window per
/// probe when the app scans. CREATE_NO_WINDOW runs the child with no
/// console window; `.output()` pipes stdio, so nothing else changes.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Run a PowerShell snippet that ends in `ConvertTo-Json` and parse the
/// result. `ConvertTo-Json` emits a bare object for a single row and an
/// array for many; [`as_rows`] normalises both.
fn ps_json(script: &str) -> Option<serde_json::Value> {
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

fn as_rows(v: serde_json::Value) -> Vec<serde_json::Value> {
    match v {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Null => Vec::new(),
        other => vec![other],
    }
}

fn rows(script: &str) -> Vec<serde_json::Value> {
    ps_json(script).map(as_rows).unwrap_or_default()
}

fn s(v: &serde_json::Value, key: &str) -> Option<String> {
    v[key]
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn board_label() -> Option<String> {
    let v = ps_json(
        "Get-CimInstance Win32_ComputerSystem | Select-Object Manufacturer,Model | ConvertTo-Json -Compress",
    )?;
    let vendor = s(&v, "Manufacturer");
    let model = s(&v, "Model")?;
    Some(match vendor {
        Some(vn) if !model.starts_with(&vn) => format!("{vn} {model}"),
        _ => model,
    })
}

pub fn soc_label() -> Option<String> {
    None
}

pub fn collect_gpus() -> Vec<Gpu> {
    rows("Get-CimInstance Win32_VideoController | Select-Object Name,AdapterRAM,DriverVersion | ConvertTo-Json -Compress")
        .into_iter()
        .enumerate()
        .filter_map(|(i, v)| {
            let name = s(&v, "Name")?;
            let lname = name.to_lowercase();
            let vendor = if lname.contains("nvidia") {
                GpuVendor::Nvidia
            } else if lname.contains("amd") || lname.contains("radeon") {
                GpuVendor::Amd
            } else if lname.contains("intel") {
                GpuVendor::Intel
            } else {
                GpuVendor::Other
            };
            // AdapterRAM is a uint32 and wraps for >4 GB cards; treat 0 /
            // missing as unknown rather than wrong.
            let vram_bytes = v["AdapterRAM"].as_u64().filter(|&b| b > 0);
            Some(Gpu {
                id: format!("gpu:{i}"),
                name,
                vendor,
                vram_bytes,
                kind: if vendor == GpuVendor::Intel {
                    GpuKind::Integrated
                } else if vram_bytes.is_some() {
                    GpuKind::Discrete
                } else {
                    GpuKind::Unknown
                },
                driver: s(&v, "DriverVersion"),
            })
        })
        .collect()
}

pub fn collect_displays() -> Vec<Display> {
    // Decode the uint16 UserFriendlyName array from WmiMonitorID in
    // PowerShell, then carry the primary resolution from the video
    // controller (per-monitor native resolution needs the EDID timing
    // block, a follow-up).
    let script = r#"
$res = Get-CimInstance Win32_VideoController |
    Where-Object { $_.CurrentHorizontalResolution } |
    Select-Object -First 1
Get-CimInstance -Namespace root\wmi -ClassName WmiMonitorID -ErrorAction SilentlyContinue | ForEach-Object {
    $name = -join ($_.UserFriendlyName | Where-Object { $_ -ne 0 } | ForEach-Object { [char]$_ })
    [pscustomobject]@{
        Name = $name
        Instance = $_.InstanceName
        Width = $res.CurrentHorizontalResolution
        Height = $res.CurrentVerticalResolution
    }
} | ConvertTo-Json -Compress
"#;
    rows(script)
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            let name = s(&v, "Name").unwrap_or_else(|| format!("Display {i}"));
            let connector = s(&v, "Instance").unwrap_or_default();
            let internal = connector.to_uppercase().contains("LCD")
                || name.to_lowercase().contains("internal");
            Display {
                id: format!("display:{i}"),
                name,
                connector,
                connected: true,
                width_px: v["Width"].as_u64().map(|w| w as u32),
                height_px: v["Height"].as_u64().map(|h| h as u32),
                internal,
                default: false,
            }
        })
        .collect()
}

pub fn collect_audio() -> (Vec<AudioDevice>, Vec<AudioDevice>) {
    let (mut mics, mut speakers) = (Vec::new(), Vec::new());
    let endpoints = rows(
        "Get-CimInstance Win32_PnPEntity -Filter \"PNPClass='AudioEndpoint'\" | Select-Object Name,DeviceID | ConvertTo-Json -Compress",
    );
    for (i, v) in endpoints.into_iter().enumerate() {
        let Some(name) = s(&v, "Name") else { continue };
        let l = name.to_lowercase();
        let is_input = l.contains("microphone")
            || l.contains("mic ")
            || l.contains("line in")
            || l.contains("capture")
            || l.contains("input");
        let dev = AudioDevice {
            id: format!("{}:{i}", if is_input { "mic" } else { "spk" }),
            name,
            direction: if is_input {
                AudioDirection::Input
            } else {
                AudioDirection::Output
            },
            channels: None,
            card: s(&v, "DeviceID"),
            default: false,
        };
        if is_input {
            mics.push(dev);
        } else {
            speakers.push(dev);
        }
    }
    (mics, speakers)
}

pub fn collect_cameras() -> Vec<Camera> {
    // Webcams register under PNPClass 'Camera' on most modern drivers but
    // 'Image' on plenty of others (UVC devices especially) — query both.
    // 'Image' also covers scanners, so those rows only count when the name
    // says camera; 'Camera'-class rows are taken at their word.
    rows("Get-CimInstance Win32_PnPEntity -Filter \"PNPClass='Camera' OR PNPClass='Image'\" | Select-Object Name,PNPClass | ConvertTo-Json -Compress")
        .into_iter()
        .filter(|v| {
            let class = s(v, "PNPClass").unwrap_or_default();
            if class.eq_ignore_ascii_case("camera") {
                return true;
            }
            let name = s(v, "Name").unwrap_or_default().to_lowercase();
            name.contains("cam") || name.contains("video")
        })
        .enumerate()
        .filter_map(|(i, v)| {
            Some(Camera {
                id: format!("cam:{i}"),
                name: s(&v, "Name")?,
                path: None,
                default: false,
            })
        })
        .collect()
}

pub fn collect_inputs() -> Vec<InputDevice> {
    // One physical device registers a WMI row per HID interface ("HID
    // Keyboard Device" three times over); merge them by the PnP VID:PID.
    // WMI rows carry no stable per-port path, so two identical units of
    // one model merge too — an accepted trade for a readable list (these
    // entries are display-only input sources).
    let mut raw = Vec::new();
    for (i, v) in rows("Get-CimInstance Win32_Keyboard | Select-Object Name,Description,PNPDeviceID | ConvertTo-Json -Compress")
        .into_iter()
        .enumerate()
    {
        let name = s(&v, "Name").or_else(|| s(&v, "Description")).unwrap_or_else(|| "Keyboard".into());
        raw.push(crate::dedupe::RawInput {
            group: s(&v, "PNPDeviceID").as_deref().and_then(pnp_vid_pid),
            fallback_id: format!("input:kbd:{i}"),
            name,
            kind: InputKind::Keyboard,
        });
    }
    for (i, v) in rows("Get-CimInstance Win32_PointingDevice | Select-Object Name,Description,PNPDeviceID | ConvertTo-Json -Compress")
        .into_iter()
        .enumerate()
    {
        let name = s(&v, "Name").or_else(|| s(&v, "Description")).unwrap_or_else(|| "Pointer".into());
        let l = name.to_lowercase();
        let kind = if l.contains("touchpad") || l.contains("trackpad") {
            InputKind::Touchpad
        } else {
            InputKind::Mouse
        };
        raw.push(crate::dedupe::RawInput {
            group: s(&v, "PNPDeviceID").as_deref().and_then(pnp_vid_pid),
            fallback_id: format!("input:pt:{i}"),
            name,
            kind,
        });
    }
    crate::dedupe::merge_inputs(raw)
}

/// `HID\VID_046D&PID_C52B&MI_01\8&2f662e1&0&0000` → `046d:c52b` — the
/// physical unit's identity, shared by all its interfaces.
fn pnp_vid_pid(id: &str) -> Option<String> {
    let (vid, pid) = parse_usb_id(id)?;
    Some(format!("{vid}:{pid}"))
}

pub fn collect_usb() -> Vec<UsbDevice> {
    let mut out = Vec::new();
    for v in rows(
        "Get-CimInstance Win32_PnPEntity -Filter \"DeviceID like 'USB\\\\VID_%'\" | Select-Object Name,Manufacturer,DeviceID | ConvertTo-Json -Compress",
    ) {
        let Some(device_id) = s(&v, "DeviceID") else { continue };
        let Some((vid, pid)) = parse_usb_id(&device_id) else { continue };
        // Skip Microsoft/host root entries that aren't really peripherals.
        let name = s(&v, "Name").unwrap_or_else(|| format!("USB {vid}:{pid}"));
        out.push(UsbDevice {
            id: format!("usb:{vid}:{pid}"),
            name,
            vendor_id: vid,
            product_id: pid,
            manufacturer: s(&v, "Manufacturer"),
            class: None,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// `USB\VID_046D&PID_C52B\...` → (`046d`, `c52b`).
fn parse_usb_id(device_id: &str) -> Option<(String, String)> {
    let up = device_id.to_uppercase();
    let vid = up.split("VID_").nth(1)?.get(..4)?.to_lowercase();
    let pid = up.split("PID_").nth(1)?.get(..4)?.to_lowercase();
    (vid.chars().all(|c| c.is_ascii_hexdigit()) && pid.chars().all(|c| c.is_ascii_hexdigit()))
        .then_some((vid, pid))
}

/// Enumerate the TCP ports this machine is listening on, via
/// `Get-NetTCPConnection -State Listen`, tagged with each socket's owning
/// process name (cosmetic — `Get-Process` is SilentlyContinue, so a PID we
/// can't open just leaves the name blank). Degrades to an empty list when
/// PowerShell isn't available or nothing is listening.
pub fn collect_listening() -> Vec<ListeningService> {
    let script = "Get-NetTCPConnection -State Listen | ForEach-Object { \
        [PSCustomObject]@{ LocalAddress = $_.LocalAddress; LocalPort = $_.LocalPort; \
        Process = (Get-Process -Id $_.OwningProcess -ErrorAction SilentlyContinue).ProcessName } } \
        | ConvertTo-Json -Compress";
    crate::listening::services_from_nettcp_rows(&rows(script))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usb_device_id() {
        assert_eq!(
            parse_usb_id("USB\\VID_046D&PID_C52B\\5&1234"),
            Some(("046d".into(), "c52b".into()))
        );
        assert_eq!(parse_usb_id("HID\\nope"), None);
    }

    #[test]
    fn pnp_id_yields_the_units_group_key() {
        // The MI_xx interface suffix differs per endpoint; the key doesn't.
        assert_eq!(
            pnp_vid_pid("HID\\VID_046D&PID_C52B&MI_00\\8&2f662e1&0&0000").as_deref(),
            Some("046d:c52b")
        );
        assert_eq!(
            pnp_vid_pid("HID\\VID_046D&PID_C52B&MI_01\\9&aaaa&0&0000").as_deref(),
            Some("046d:c52b")
        );
        assert_eq!(pnp_vid_pid("ACPI\\PNP0303\\4&1ab2c3d&0"), None);
    }

    #[test]
    fn normalises_single_and_array_rows() {
        assert_eq!(as_rows(serde_json::json!({"Name": "a"})).len(), 1);
        assert_eq!(
            as_rows(serde_json::json!([{"Name": "a"}, {"Name": "b"}])).len(),
            2
        );
        assert_eq!(as_rows(serde_json::Value::Null).len(), 0);
    }
}

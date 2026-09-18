use allmystuff_inventory_model::*;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

const FULL: &str = include_str!("fixtures/full.json");
const LEGACY: &str = include_str!("fixtures/legacy.json");
const COLLECTIONS: &[&str] = &[
    "gpus",
    "storage",
    "networks",
    "displays",
    "microphones",
    "speakers",
    "cameras",
    "inputs",
    "usb",
    "listening",
    "temps",
];

#[test]
fn missing_collections_preserve_the_minimal_snapshot() {
    let mut fixture: Value = serde_json::from_str(LEGACY).unwrap();
    for field in COLLECTIONS {
        fixture.as_object_mut().unwrap().remove(*field);
    }
    let inventory: Inventory = serde_json::from_value(fixture).unwrap();
    let serialized = serde_json::to_value(&inventory).unwrap();
    for field in COLLECTIONS {
        assert_eq!(serialized[*field], json!([]), "{field}");
    }
    assert_eq!(inventory.device_count(), 2);
    assert_eq!(inventory.scanned_at, 1_700_000_000);
    assert_eq!(inventory.cpu.logical_cores, 8);
}

#[test]
fn legacy_devices_keep_missing_option_and_additive_field_defaults() {
    let inventory: Inventory = serde_json::from_str(LEGACY).unwrap();
    assert!(inventory.host.os_version.is_none());
    assert!(inventory.host.kernel_version.is_none());
    assert!(inventory.host.board.is_none());
    assert!(inventory.host.product.is_none());
    assert!(inventory.host.soc.is_none());
    assert!(inventory.cpu.vendor.is_none());
    assert!(inventory.cpu.physical_cores.is_none());
    assert!(inventory.cpu.max_mhz.is_none());
    assert!(inventory.gpus[0].vram_bytes.is_none());
    assert!(inventory.gpus[0].driver.is_none());
    assert!(inventory.storage[0].mount_point.is_none());
    assert!(inventory.storage[0].filesystem.is_none());
    assert!(inventory.networks[0].mac.is_none());
    assert!(inventory.networks[0].speed_mbps.is_none());
    assert!(inventory.networks[0].ipv4.is_empty());
    assert!(inventory.networks[0].ipv6.is_empty());
    assert!(inventory.displays[0].width_px.is_none());
    assert!(inventory.displays[0].height_px.is_none());
    assert!(!inventory.displays[0].default);
    for device in [&inventory.microphones[0], &inventory.speakers[0]] {
        assert!(device.channels.is_none());
        assert!(device.card.is_none());
        assert!(!device.default);
    }
    assert!(inventory.cameras[0].path.is_none());
    assert!(!inventory.cameras[0].default);
    assert_eq!(inventory.inputs[0].endpoints, 1);
    assert!(inventory.usb[0].manufacturer.is_none());
    assert!(inventory.usb[0].class.is_none());
    let service = &inventory.listening[0];
    assert!(service.scheme.is_empty());
    assert!(!service.loopback);
    assert!(service.process.is_empty());
    assert!(service.title.is_empty());
    let serialized = serde_json::to_value(&inventory).unwrap();
    assert!(serialized.get("future_snapshot_field").is_none());
    assert!(serialized["inputs"][0].get("future_device_field").is_none());
    assert_eq!(inventory.device_count(), 11);
}

#[test]
fn populated_snapshot_round_trips_without_wire_shape_changes() {
    let fixture: Value = serde_json::from_str(FULL).unwrap();
    let inventory: Inventory = serde_json::from_value(fixture.clone()).unwrap();
    assert_eq!(serde_json::to_value(&inventory).unwrap(), fixture);
    assert_eq!(inventory.inputs[0].endpoints, 3);
    // Nine device records plus CPU/memory; listening services and temperatures
    // are present in this fixture but do not increase the device count.
    assert_eq!(inventory.device_count(), 11);
    assert!(inventory.microphones[0].is_array());
    assert!(!inventory.speakers[0].is_array());
}

#[test]
fn required_fields_and_explicit_zero_remain_distinct_from_defaults() {
    let fixture: Value = serde_json::from_str(FULL).unwrap();
    for field in ["scanned_at", "host", "cpu", "memory"] {
        let mut missing = fixture.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<Inventory>(missing).is_err(),
            "{field}"
        );
    }
    for field in COLLECTIONS {
        let mut null = fixture.clone();
        null[*field] = Value::Null;
        assert!(
            serde_json::from_value::<Inventory>(null).is_err(),
            "{field}"
        );
    }
    let mut input = fixture["inputs"][0].clone();
    input["endpoints"] = json!(0);
    let device: InputDevice = serde_json::from_value(input).unwrap();
    assert_eq!(device.endpoints, 0);
}

fn enum_tokens<T>(cases: &[(T, &str)])
where
    T: Copy + std::fmt::Debug + PartialEq + Serialize + DeserializeOwned,
{
    for &(value, token) in cases {
        assert_eq!(serde_json::to_value(value).unwrap(), json!(token));
        assert_eq!(serde_json::from_value::<T>(json!(token)).unwrap(), value);
    }
    assert!(serde_json::from_value::<T>(json!("future_unknown_kind")).is_err());
}

#[test]
fn device_enum_tokens_remain_compatible() {
    enum_tokens(&[
        (GpuVendor::Nvidia, "nvidia"),
        (GpuVendor::Amd, "amd"),
        (GpuVendor::Intel, "intel"),
        (GpuVendor::Apple, "apple"),
        (GpuVendor::Other, "other"),
    ]);
    enum_tokens(&[
        (GpuKind::Discrete, "discrete"),
        (GpuKind::Integrated, "integrated"),
        (GpuKind::Unknown, "unknown"),
    ]);
    enum_tokens(&[
        (DiskKind::Ssd, "ssd"),
        (DiskKind::Hdd, "hdd"),
        (DiskKind::Removable, "removable"),
        (DiskKind::Unknown, "unknown"),
    ]);
    enum_tokens(&[
        (NetKind::Ethernet, "ethernet"),
        (NetKind::Wifi, "wifi"),
        (NetKind::Loopback, "loopback"),
        (NetKind::Virtual, "virtual"),
        (NetKind::Cellular, "cellular"),
        (NetKind::Bluetooth, "bluetooth"),
        (NetKind::Unknown, "unknown"),
    ]);
    enum_tokens(&[
        (AudioDirection::Input, "input"),
        (AudioDirection::Output, "output"),
    ]);
    enum_tokens(&[
        (InputKind::Keyboard, "keyboard"),
        (InputKind::Mouse, "mouse"),
        (InputKind::Touchpad, "touchpad"),
        (InputKind::Touchscreen, "touchscreen"),
        (InputKind::Gamepad, "gamepad"),
        (InputKind::Tablet, "tablet"),
        (InputKind::Other, "other"),
    ]);
}

#[test]
fn array_detection_preserves_direction_and_channel_boundaries() {
    let inventory: Inventory = serde_json::from_str(FULL).unwrap();
    let mut device = inventory.microphones[0].clone();
    for (channels, input_is_array) in [
        (None, false),
        (Some(0), false),
        (Some(3), false),
        (Some(4), true),
        (Some(u32::MAX), true),
    ] {
        device.channels = channels;
        device.direction = AudioDirection::Input;
        assert_eq!(device.is_array(), input_is_array);
        device.direction = AudioDirection::Output;
        assert!(!device.is_array());
    }
}

#[test]
fn service_tokens_labels_schemes_and_web_flags_remain_compatible() {
    for (kind, token, label, scheme, web) in [
        (ServiceKind::Http, "http", "HTTP", "http", true),
        (ServiceKind::Https, "https", "HTTPS", "https", true),
        (ServiceKind::Ssh, "ssh", "SSH", "ssh", false),
        (
            ServiceKind::Postgres,
            "postgres",
            "PostgreSQL",
            "postgres",
            false,
        ),
        (ServiceKind::Mysql, "mysql", "MySQL", "mysql", false),
        (ServiceKind::Redis, "redis", "Redis", "redis", false),
        (ServiceKind::Mongodb, "mongodb", "MongoDB", "mongodb", false),
        (ServiceKind::Vnc, "vnc", "VNC", "vnc", false),
        (ServiceKind::Rdp, "rdp", "RDP", "rdp", false),
        (ServiceKind::Smb, "smb", "SMB", "smb", false),
        (ServiceKind::Ftp, "ftp", "FTP", "ftp", false),
        (ServiceKind::Smtp, "smtp", "SMTP", "smtp", false),
        (ServiceKind::Dns, "dns", "DNS", "", false),
        (ServiceKind::Other, "other", "TCP service", "", false),
    ] {
        enum_tokens(&[(kind, token)]);
        assert_eq!(kind.label(), label);
        assert_eq!(kind.scheme(), scheme);
        assert_eq!(kind.is_web(), web);
    }
}

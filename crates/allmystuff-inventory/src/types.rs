//! The data model a scan produces.
//!
//! Every device the scanner finds becomes a typed record with a
//! **stable `id`** — derived from durable attributes (a MAC address, a
//! drm connector, a USB `vid:pid:serial`) rather than an enumeration
//! index — so the graph layer can pin a route to "this exact mic" and
//! have it survive a reboot or a replug. Friendly `name`s are for the
//! human; `id`s are for the wire.
//!
//! Shapes are deliberately `serde`-clean and additive: new fields are
//! `#[serde(default)]` so an older config snapshot or an older peer
//! still deserialises. This mirrors the discipline in
//! `MyOwnLLM`'s `HardwareProfile`.

use serde::{Deserialize, Serialize};

/// A full snapshot of one machine's hardware and attached devices.
///
/// This is what `allmystuff_inventory::scan()` returns and what the
/// desktop app hands to the graph as "this node's stuff."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inventory {
    /// Unix seconds the scan completed. Lets the UI show "scanned 2m
    /// ago" and lets a peer reason about staleness of a shared
    /// snapshot.
    pub scanned_at: u64,
    pub host: HostInfo,
    pub cpu: Cpu,
    pub memory: Memory,
    #[serde(default)]
    pub gpus: Vec<Gpu>,
    #[serde(default)]
    pub storage: Vec<StorageVolume>,
    #[serde(default)]
    pub networks: Vec<NetworkInterface>,
    #[serde(default)]
    pub displays: Vec<Display>,
    /// Capture devices — microphones, line-in, loopback. The `channels`
    /// field is how we surface "this is a 4+ mic array" (a conference
    /// puck, a ReSpeaker, a laptop's beam-forming array).
    #[serde(default)]
    pub microphones: Vec<AudioDevice>,
    /// Playback devices — speakers, headphones, HDMI audio.
    #[serde(default)]
    pub speakers: Vec<AudioDevice>,
    #[serde(default)]
    pub cameras: Vec<Camera>,
    /// Keyboards, mice, touchpads, game controllers, touchscreens.
    #[serde(default)]
    pub inputs: Vec<InputDevice>,
    /// Everything else identifiable on the USB bus that didn't slot
    /// into a richer category above (printers, yubikeys, dongles…).
    #[serde(default)]
    pub usb: Vec<UsbDevice>,
    /// TCP services this machine is **listening on** — the ports a scan
    /// found bound and accepting. The bridge turns these into the "sites"
    /// peers can reach through AllMyStuff's reverse proxy. Like every other
    /// probe this degrades to an empty list rather than failing, and it's
    /// `#[serde(default)]` so an older snapshot/peer without the field still
    /// decodes.
    #[serde(default)]
    pub listening: Vec<ListeningService>,
    /// Temperature sensors the OS exposes, in °C. Strictly what the platform
    /// reports (hwmon on Linux, SMC on macOS, ACPI thermal zones on Windows) —
    /// many consumer Windows boards expose nothing without a vendor driver,
    /// so an empty list is the common case there, and UIs should hide the
    /// section rather than show a blank. `#[serde(default)]` so an older
    /// snapshot/peer without the field still decodes.
    #[serde(default)]
    pub temps: Vec<TempSensor>,
}

/// One temperature reading, labelled by its source sensor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TempSensor {
    /// Sensor label as the platform names it, e.g. `"coretemp Package id 0"`
    /// or `"ACPI\\ThermalZone\\TZ00_0"`.
    pub label: String,
    /// Degrees Celsius.
    pub celsius: f32,
}

impl Inventory {
    /// Total number of individually-addressable devices across every
    /// category — the headline "you have N things" number for the UI.
    pub fn device_count(&self) -> usize {
        self.gpus.len()
            + self.storage.len()
            + self.networks.len()
            + self.displays.len()
            + self.microphones.len()
            + self.speakers.len()
            + self.cameras.len()
            + self.inputs.len()
            + self.usb.len()
            // CPU + memory are always-present singletons.
            + 2
    }
}

// ---- host -------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostInfo {
    pub hostname: String,
    /// `linux` / `macos` / `windows` — `std::env::consts::OS`.
    pub os: String,
    pub os_version: Option<String>,
    pub kernel_version: Option<String>,
    /// `x86_64`, `aarch64`, … — what the running binary was built for.
    pub arch: String,
    /// Friendly board / vendor label from DMI (`Dell Inc. XPS 15`) when
    /// available on x86 desktops/laptops.
    pub board: Option<String>,
    /// Just the product / model name — the *second* DMI field, without the
    /// manufacturer prefix that `board` carries (`XPS 15`, not `Dell Inc.
    /// XPS 15`). This is the field that identifies which machine you're
    /// looking at (a maker name doesn't), so it's what surfaces to a CEC
    /// technician. `None` when DMI reports no usable product string.
    pub product: Option<String>,
    /// Single-board-computer / SoC label when identifiable — e.g.
    /// "Raspberry Pi 5 Model B". `None` on most x86 and Macs. Same
    /// detection MyOwnLLM uses to right-size its model pick.
    pub soc: Option<String>,
    pub uptime_secs: u64,
}

// ---- cpu --------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cpu {
    /// Marketing string — "Intel(R) Core(TM) i7-1185G7", "Apple M2".
    pub brand: String,
    pub vendor: Option<String>,
    pub physical_cores: Option<usize>,
    pub logical_cores: usize,
    /// Nominal / max frequency in MHz when the OS reports it.
    pub max_mhz: Option<u64>,
}

// ---- memory -----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
}

// ---- gpu --------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gpu {
    pub id: String,
    pub name: String,
    pub vendor: GpuVendor,
    /// Dedicated video memory in bytes when discoverable (NVML, amdgpu
    /// sysfs, Apple unified memory). `None` for most integrated GPUs.
    pub vram_bytes: Option<u64>,
    pub kind: GpuKind,
    pub driver: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuKind {
    Discrete,
    Integrated,
    Unknown,
}

// ---- storage ----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageVolume {
    pub id: String,
    pub name: String,
    pub mount_point: Option<String>,
    pub filesystem: Option<String>,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub removable: bool,
    pub kind: DiskKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskKind {
    Ssd,
    Hdd,
    Removable,
    Unknown,
}

// ---- network ----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkInterface {
    pub id: String,
    pub name: String,
    pub mac: Option<String>,
    pub kind: NetKind,
    pub up: bool,
    /// Link speed in Mbit/s when the driver reports it (`/sys/class/net/
    /// <if>/speed`). `None` on Wi-Fi/virtual where it's not meaningful.
    pub speed_mbps: Option<u64>,
    #[serde(default)]
    pub ipv4: Vec<String>,
    #[serde(default)]
    pub ipv6: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetKind {
    Ethernet,
    Wifi,
    Loopback,
    Virtual,
    Cellular,
    Bluetooth,
    Unknown,
}

// ---- display ----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Display {
    pub id: String,
    /// Monitor model name from EDID when we can read it ("DELL U2720Q"),
    /// otherwise the connector name.
    pub name: String,
    /// drm connector — "eDP-1", "HDMI-A-1", "DP-2".
    pub connector: String,
    pub connected: bool,
    /// Native / preferred resolution in pixels.
    pub width_px: Option<u32>,
    pub height_px: Option<u32>,
    /// `true` for built-in panels (eDP / LVDS / DSI) — a laptop screen
    /// rather than an external monitor.
    pub internal: bool,
    /// `true` for the machine's **current default** display in this
    /// category — the primary screen the OS would drive first. Exactly one
    /// connected display is marked per scan (the built-in panel when there
    /// is one, else the first connected output). `#[serde(default)]` so an
    /// older snapshot/peer without the field still decodes.
    #[serde(default)]
    pub default: bool,
}

// ---- audio ------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub direction: AudioDirection,
    /// Max channel count when discoverable. `>= 4` on a capture device
    /// is the tell for a microphone *array* (beam-forming pucks,
    /// conference mics, dev boards).
    pub channels: Option<u32>,
    /// ALSA card index / CoreAudio uid / WASAPI id — for debugging and
    /// stable id derivation.
    pub card: Option<String>,
    /// `true` for the machine's **current default** device of this
    /// direction — the mic the OS captures from / the speaker it plays to
    /// by default. Exactly one input and one output are marked per scan
    /// (the default ALSA card on Linux; the system default elsewhere).
    /// A desktop sound server can route elsewhere at runtime, but this is
    /// the default the scan can see. `#[serde(default)]` for older peers.
    #[serde(default)]
    pub default: bool,
}

impl AudioDevice {
    /// A capture device with four or more channels is treated as a mic
    /// *array* — the UI badges it and the graph can offer "use the
    /// whole array" vs "use one element."
    pub fn is_array(&self) -> bool {
        matches!(self.direction, AudioDirection::Input) && self.channels.is_some_and(|c| c >= 4)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioDirection {
    Input,
    Output,
}

// ---- camera -----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Camera {
    pub id: String,
    pub name: String,
    /// Device node on Unix (`/dev/video0`). Informational on other OSes.
    pub path: Option<String>,
    /// `true` for the machine's **current default** camera — the first
    /// capture node the OS would open. `#[serde(default)]` for older peers.
    #[serde(default)]
    pub default: bool,
}

// ---- input ------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputDevice {
    pub id: String,
    pub name: String,
    pub kind: InputKind,
    /// How many OS-level input endpoints (HID interfaces) merged into this
    /// one physical device — a gaming mouse or a unifying receiver exposes
    /// several. `1` for a plain device; defaulted so older snapshots and
    /// peers still decode.
    #[serde(default = "default_endpoints")]
    pub endpoints: u32,
}

fn default_endpoints() -> u32 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    Keyboard,
    Mouse,
    Touchpad,
    Touchscreen,
    Gamepad,
    Tablet,
    Other,
}

// ---- usb --------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsbDevice {
    pub id: String,
    pub name: String,
    /// 4-hex-digit USB vendor id (`"046d"`).
    pub vendor_id: String,
    /// 4-hex-digit USB product id (`"085e"`).
    pub product_id: String,
    pub manufacturer: Option<String>,
    /// Friendly USB device-class label ("Audio", "Video", "HID",
    /// "Mass Storage") derived from `bDeviceClass`.
    pub class: Option<String>,
}

// ---- listening services (the "sites" source) --------------------------

/// A TCP service this machine is listening on — one bound, accepting port.
///
/// Found by the scan (passive: which ports are in `LISTEN`) and optionally
/// refined by an active banner probe (what's actually behind the port).
/// AllMyStuff's reverse proxy can carry any of these across the mesh, so a
/// service that only ever bound to `127.0.0.1` (a local dev server, a
/// self-hosted app) becomes reachable from another of your machines without
/// the owner re-binding it to the LAN.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListeningService {
    /// Stable id derived from the transport + port — `tcp:8080`. Survives
    /// rescans (and a service restarting) so a mapped site keeps its
    /// identity.
    pub id: String,
    /// Friendly label — the classified service name ("HTTP", "PostgreSQL")
    /// or "Port 8080" when nothing matched.
    pub name: String,
    /// The listening port.
    pub port: u16,
    /// Best guess at what speaks behind the port: from the well-known-port
    /// table, then refined by an active banner probe when one was run.
    pub kind: ServiceKind,
    /// URL scheme a browser or client tool would reach it with — "http",
    /// "https", "ssh", "postgres", … — or empty when there's no sensible
    /// one (a bare TCP service the proxy still tunnels).
    #[serde(default)]
    pub scheme: String,
    /// `true` when the socket is bound only to loopback (127.0.0.1 / ::1),
    /// so it's reachable from this machine alone. The prime reverse-proxy
    /// case: not on the LAN, but the mesh can carry it. `false` means it's
    /// bound to a routable interface (already reachable on the network it
    /// sits on).
    #[serde(default)]
    pub loopback: bool,
    /// Best-effort owning process name (resolved from the socket inode via
    /// `/proc/<pid>` on Linux), purely so the human recognises the service.
    /// Empty when it couldn't be resolved (the common case without elevated
    /// permissions) — never load-bearing.
    #[serde(default)]
    pub process: String,
    /// The HTML `<title>` of the page served here, when the active probe
    /// could fetch one (an `http` site — `https` needs TLS the probe
    /// doesn't carry). The UI offers it as the default name when exposing
    /// the site, so "My Grafana" beats "Port 3000". Empty when there's none.
    #[serde(default)]
    pub title: String,
}

/// What a listening port is speaking — a best-effort classification, never
/// trusted for routing (the proxy tunnels raw bytes regardless). `Other` is
/// a TCP service we couldn't name; it's still a perfectly good site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind {
    Http,
    Https,
    Ssh,
    Postgres,
    Mysql,
    Redis,
    Mongodb,
    Vnc,
    Rdp,
    Smb,
    Ftp,
    Smtp,
    Dns,
    /// A named-but-uncategorised or unknown TCP service.
    Other,
}

impl ServiceKind {
    /// The friendly service name shown in the UI.
    pub fn label(self) -> &'static str {
        match self {
            ServiceKind::Http => "HTTP",
            ServiceKind::Https => "HTTPS",
            ServiceKind::Ssh => "SSH",
            ServiceKind::Postgres => "PostgreSQL",
            ServiceKind::Mysql => "MySQL",
            ServiceKind::Redis => "Redis",
            ServiceKind::Mongodb => "MongoDB",
            ServiceKind::Vnc => "VNC",
            ServiceKind::Rdp => "RDP",
            ServiceKind::Smb => "SMB",
            ServiceKind::Ftp => "FTP",
            ServiceKind::Smtp => "SMTP",
            ServiceKind::Dns => "DNS",
            ServiceKind::Other => "TCP service",
        }
    }

    /// The URL scheme a client would use, or `""` for services with no
    /// sensible scheme (the proxy still tunnels them as raw TCP).
    pub fn scheme(self) -> &'static str {
        match self {
            ServiceKind::Http => "http",
            ServiceKind::Https => "https",
            ServiceKind::Ssh => "ssh",
            ServiceKind::Postgres => "postgres",
            ServiceKind::Mysql => "mysql",
            ServiceKind::Redis => "redis",
            ServiceKind::Mongodb => "mongodb",
            ServiceKind::Vnc => "vnc",
            ServiceKind::Rdp => "rdp",
            ServiceKind::Smb => "smb",
            ServiceKind::Ftp => "ftp",
            ServiceKind::Smtp => "smtp",
            ServiceKind::Dns => "",
            ServiceKind::Other => "",
        }
    }

    /// `true` for a web service — the UI offers "open in browser" and the
    /// proxy can hand back an `http(s)://` URL. `Other` is treated as web
    /// only once a probe upgrades it.
    pub fn is_web(self) -> bool {
        matches!(self, ServiceKind::Http | ServiceKind::Https)
    }
}

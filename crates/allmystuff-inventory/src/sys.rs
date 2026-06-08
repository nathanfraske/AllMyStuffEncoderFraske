//! Cross-platform host basics via `sysinfo` — CPU, memory, storage,
//! networks, and the OS/uptime fields. The richer per-device classes
//! (displays, audio, cameras, input, USB) are platform-specific and live
//! in `linux.rs` / `macos.rs` / `windows.rs`.

use std::collections::BTreeMap;

use sysinfo::{Disks, Networks, System};

use crate::types::*;

pub fn host_info() -> HostInfo {
    let arch = std::env::consts::ARCH.to_string();

    #[cfg(target_os = "linux")]
    let (board, soc) = (crate::linux::board_label(), crate::linux::soc_label());
    #[cfg(not(target_os = "linux"))]
    let (board, soc) = (None, None);

    HostInfo {
        hostname: System::host_name().unwrap_or_else(|| "this device".into()),
        os: std::env::consts::OS.to_string(),
        os_version: System::os_version(),
        kernel_version: System::kernel_version(),
        arch,
        board,
        soc,
        uptime_secs: System::uptime(),
    }
}

pub fn cpu(sys: &System) -> Cpu {
    let cpus = sys.cpus();
    let first = cpus.first();

    // sysinfo sometimes blanks the brand inside minimal VMs; fall back to
    // the raw cpuinfo line on Linux before giving up.
    let brand = first
        .map(|c| c.brand().trim().to_string())
        .filter(|b| !b.is_empty());
    #[cfg(target_os = "linux")]
    let brand = brand.or_else(crate::linux::cpu_brand_fallback);
    let brand = brand.unwrap_or_else(|| "Unknown CPU".into());

    let vendor = first
        .map(|c| c.vendor_id().trim().to_string())
        .filter(|v| !v.is_empty());

    let max_mhz = first.map(|c| c.frequency()).filter(|&f| f > 0);

    Cpu {
        brand,
        vendor,
        physical_cores: System::physical_core_count(),
        logical_cores: cpus.len().max(1),
        max_mhz,
    }
}

pub fn memory(sys: &System) -> Memory {
    Memory {
        total_bytes: sys.total_memory(),
        available_bytes: sys.available_memory(),
        swap_total_bytes: sys.total_swap(),
        swap_used_bytes: sys.used_swap(),
    }
}

pub fn storage() -> Vec<StorageVolume> {
    let disks = Disks::new_with_refreshed_list();
    // Dedupe by mount point — bind mounts and the like surface the same
    // volume several times.
    let mut by_mount: BTreeMap<String, StorageVolume> = BTreeMap::new();
    for d in &disks {
        let mount = d.mount_point().to_string_lossy().to_string();
        // Skip the usual pseudo / read-only system mounts that aren't
        // "storage you'd share."
        if is_pseudo_mount(&mount) {
            continue;
        }
        let name = d.name().to_string_lossy().to_string();
        let kind = if d.is_removable() {
            DiskKind::Removable
        } else {
            match d.kind() {
                sysinfo::DiskKind::SSD => DiskKind::Ssd,
                sysinfo::DiskKind::HDD => DiskKind::Hdd,
                _ => DiskKind::Unknown,
            }
        };
        let vol = StorageVolume {
            id: format!("disk:{mount}"),
            name: if name.is_empty() { mount.clone() } else { name },
            mount_point: Some(mount.clone()),
            filesystem: Some(d.file_system().to_string_lossy().to_string())
                .filter(|s| !s.is_empty()),
            total_bytes: d.total_space(),
            available_bytes: d.available_space(),
            removable: d.is_removable(),
            kind,
        };
        by_mount.insert(mount, vol);
    }
    by_mount.into_values().collect()
}

fn is_pseudo_mount(mount: &str) -> bool {
    mount.starts_with("/proc")
        || mount.starts_with("/sys")
        || mount.starts_with("/dev")
        || mount.starts_with("/run")
        || mount.starts_with("/snap")
        || mount == "/boot/efi"
}

pub fn networks() -> Vec<NetworkInterface> {
    let nets = Networks::new_with_refreshed_list();
    let mut out = Vec::new();
    for (name, data) in &nets {
        let mac = {
            let m = data.mac_address().to_string();
            (m != "00:00:00:00:00:00").then_some(m)
        };

        // IP addresses (sysinfo ≥ 0.31 surfaces these per interface).
        let mut ipv4 = Vec::new();
        let mut ipv6 = Vec::new();
        for ipn in data.ip_networks() {
            match ipn.addr {
                std::net::IpAddr::V4(a) => ipv4.push(a.to_string()),
                std::net::IpAddr::V6(a) => ipv6.push(a.to_string()),
            }
        }

        #[cfg(target_os = "linux")]
        let (kind, up, speed_mbps) = crate::linux::net_detail(name);
        #[cfg(not(target_os = "linux"))]
        let (kind, up, speed_mbps) = (
            if name == "lo" || name.starts_with("lo") {
                NetKind::Loopback
            } else {
                NetKind::Unknown
            },
            true,
            None,
        );

        out.push(NetworkInterface {
            id: format!("net:{name}"),
            name: name.clone(),
            mac,
            kind,
            up,
            speed_mbps,
            ipv4,
            ipv6,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

//! The field-test telemetry line: every 5 s, one greppable INFO line
//! with the numbers remote optimization needs and the per-route dial-ins
//! can't see — process + system CPU, per-engine GPU utilization
//! (3D · video-encode · video-decode · copy), and dedicated VRAM.
//!
//! The GPU counters are WDDM's own (`\GPU Engine(*)\Utilization
//! Percentage` via PDH), which means the *same line on NVIDIA, AMD, and
//! Intel* — the property that makes a 9060 XT log directly comparable
//! to this box's. Engine busy vs our own ms/frame answers the questions
//! a latency number alone can't: is the encode engine saturated or
//! waiting, did clocks settle (engines busy but slow), is the copy
//! engine the ceiling.
//!
//! One sampler thread per process, started by `serve` next to the cwd
//! logger; every failure is soft (a box without the counters logs one
//! warning and the thread ends). Cost: one PDH collect per 5 s.
//! `ALLMYSTUFF_TELEMETRY=0` disables.

#![cfg(windows)]

use windows::core::PCWSTR;
use windows::Win32::Foundation::{FILETIME, HANDLE};
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes, GetSystemTimes};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn ft_100ns(ft: FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

/// Sum a wildcard counter's formatted array, bucketing GPU-engine
/// instances by `engtype_*` substring. Returns (3d, encode, decode,
/// copy) percentages, or the flat sum for non-engine counters.
unsafe fn read_engine_counter(counter: PDH_HCOUNTER) -> Option<(f64, f64, f64, f64)> {
    let mut buf_len = 0u32;
    let mut count = 0u32;
    // Size call: PDH_MORE_DATA expected.
    let _ = PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut buf_len, &mut count, None);
    if buf_len == 0 {
        return None;
    }
    let mut buf = vec![0u8; buf_len as usize];
    let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
    let status = PdhGetFormattedCounterArrayW(
        counter,
        PDH_FMT_DOUBLE,
        &mut buf_len,
        &mut count,
        Some(items),
    );
    if status != 0 {
        return None;
    }
    let (mut d3, mut enc, mut dec, mut copy) = (0.0, 0.0, 0.0, 0.0);
    for i in 0..count as usize {
        let item = &*items.add(i);
        let name = {
            let mut p = item.szName.0;
            let mut s = String::new();
            while !p.is_null() && *p != 0 {
                s.push(char::from_u32(u32::from(*p)).unwrap_or('?'));
                p = p.add(1);
            }
            s
        };
        let v = item.FmtValue.Anonymous.doubleValue;
        if name.contains("engtype_3D") {
            d3 += v;
        } else if name.contains("engtype_VideoEncode") {
            enc += v;
        } else if name.contains("engtype_VideoDecode") {
            dec += v;
        } else if name.contains("engtype_Copy") {
            copy += v;
        } else {
            // Non-engine wildcard counters (VRAM) land here: the caller
            // reads the flat sum across all four buckets.
            d3 += v;
        }
    }
    Some((d3, enc, dec, copy))
}

/// One line describing the attached monitors: name, mode, refresh,
/// desktop position, primary marker — the multi-monitor test's ground
/// truth, logged at start and again whenever the topology changes
/// (hotplug, resolution switch, rotation).
fn monitors_line() -> String {
    use windows_sys::Win32::Graphics::Gdi::{
        EnumDisplayDevicesW, EnumDisplaySettingsW, DEVMODEW, DISPLAY_DEVICEW, ENUM_CURRENT_SETTINGS,
    };
    let mut parts = Vec::new();
    unsafe {
        for i in 0..16u32 {
            let mut dev: DISPLAY_DEVICEW = std::mem::zeroed();
            dev.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
            if EnumDisplayDevicesW(std::ptr::null(), i, &mut dev, 0) == 0 {
                break;
            }
            const ACTIVE: u32 = 0x1;
            const PRIMARY: u32 = 0x4;
            if dev.StateFlags & ACTIVE == 0 {
                continue;
            }
            let name_end = dev.DeviceName.iter().position(|&c| c == 0).unwrap_or(32);
            let name = String::from_utf16_lossy(&dev.DeviceName[..name_end]);
            let mut mode: DEVMODEW = std::mem::zeroed();
            mode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
            if EnumDisplaySettingsW(dev.DeviceName.as_ptr(), ENUM_CURRENT_SETTINGS, &mut mode) != 0
            {
                let pos = mode.Anonymous1.Anonymous2.dmPosition;
                parts.push(format!(
                    "{name} {}x{}@{} at ({},{}){}",
                    mode.dmPelsWidth,
                    mode.dmPelsHeight,
                    mode.dmDisplayFrequency,
                    pos.x,
                    pos.y,
                    if dev.StateFlags & PRIMARY != 0 {
                        " primary"
                    } else {
                        ""
                    }
                ));
            } else {
                parts.push(name);
            }
        }
    }
    format!("monitors: {}", parts.join(" · "))
}

/// Whether the sampler should run. A field/prototype build carries the
/// `field-telemetry` feature and defaults ON (unless `ALLMYSTUFF_TELEMETRY`
/// is explicitly off); a stamped PROD build compiles without the feature
/// and defaults OFF, so a released binary is silent unless an operator sets
/// `ALLMYSTUFF_TELEMETRY=1` for on-demand debugging. The env dial always
/// wins over the build stamp in both directions.
fn telemetry_enabled() -> bool {
    match std::env::var("ALLMYSTUFF_TELEMETRY") {
        Ok(v) if matches!(v.trim(), "0" | "off" | "false") => false,
        Ok(_) => true, // any explicit non-off value opts in, any build
        Err(_) => cfg!(feature = "field-telemetry"), // unset → the build stamp decides
    }
}

/// Start the sampler; returns quietly if disabled or unavailable.
pub fn start() {
    if !telemetry_enabled() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("telemetry".into())
        .spawn(|| unsafe {
            let mut query = PDH_HQUERY::default();
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
                tracing::warn!("telemetry: PDH unavailable — GPU counters off");
                return;
            }
            let mut engine = PDH_HCOUNTER::default();
            let path = wide("\\GPU Engine(*)\\Utilization Percentage");
            let have_gpu =
                PdhAddEnglishCounterW(query, PCWSTR(path.as_ptr()), 0, &mut engine) == 0;
            let mut vram = PDH_HCOUNTER::default();
            let vpath = wide("\\GPU Adapter Memory(*)\\Dedicated Usage");
            let have_vram = PdhAddEnglishCounterW(query, PCWSTR(vpath.as_ptr()), 0, &mut vram) == 0;
            if !have_gpu {
                tracing::warn!("telemetry: GPU Engine counters unavailable on this box");
            }
            let _ = PdhCollectQueryData(query);
            // CPU baselines.
            let proc_handle: HANDLE = GetCurrentProcess();
            let zero = FILETIME::default();
            let (mut c0, mut e0, mut k0, mut u0) = (zero, zero, zero, zero);
            let _ = GetProcessTimes(proc_handle, &mut c0, &mut e0, &mut k0, &mut u0);
            let (mut si0, mut sk0, mut su0) = (zero, zero, zero);
            let _ = GetSystemTimes(Some(&mut si0), Some(&mut sk0), Some(&mut su0));
            let mut last_proc = ft_100ns(k0) + ft_100ns(u0);
            let (mut last_idle, mut last_sys) =
                (ft_100ns(si0), ft_100ns(sk0) + ft_100ns(su0));
            let cores = std::thread::available_parallelism()
                .map(|n| n.get() as f64)
                .unwrap_or(1.0);
            tracing::info!("{}", monitors_line());
            let mut last_monitors = monitors_line();
            // Per-media-thread CPU accounting: last (kernel+user) 100 ns
            // per registered handle.
            let mut thread_last: std::collections::HashMap<isize, u64> =
                std::collections::HashMap::new();
            // 1 Hz default — the cadence clock transitions and burst
            // stalls actually happen at; `ALLMYSTUFF_TELEMETRY_SECS`
            // stretches it for long soaks.
            let period = std::env::var("ALLMYSTUFF_TELEMETRY_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1u64)
                .clamp(1, 60);
            loop {
                std::thread::sleep(std::time::Duration::from_secs(period));
                let _ = PdhCollectQueryData(query);
                let (mut ct, mut et, mut kt, mut ut) = (zero, zero, zero, zero);
                let _ = GetProcessTimes(proc_handle, &mut ct, &mut et, &mut kt, &mut ut);
                let (mut si, mut sk, mut su) = (zero, zero, zero);
                let _ = GetSystemTimes(Some(&mut si), Some(&mut sk), Some(&mut su));
                let proc_now = ft_100ns(kt) + ft_100ns(ut);
                let (idle_now, sys_now) = (ft_100ns(si), ft_100ns(sk) + ft_100ns(su));
                let sys_delta = sys_now.saturating_sub(last_sys).max(1) as f64;
                let proc_pct =
                    (proc_now.saturating_sub(last_proc)) as f64 / sys_delta * 100.0 * cores;
                let total_pct = (1.0
                    - idle_now.saturating_sub(last_idle) as f64 / sys_delta)
                    .max(0.0)
                    * 100.0;
                (last_proc, last_idle, last_sys) = (proc_now, idle_now, sys_now);
                let gpu = if have_gpu {
                    read_engine_counter(engine)
                } else {
                    None
                };
                let vram_mb = if have_vram {
                    read_engine_counter(vram).map(|(a, b, c, d)| a + b + c + d)
                } else {
                    None
                };
                // Per-thread busy% for every registered media thread —
                // this is the CPU side of "busy vs wait": which stage of
                // the pipeline is actually burning cycles. Dead threads
                // fall out of the registry (handle closed).
                let mut thread_bits = String::new();
                if let Ok(mut reg) = crate::os_perf::MEDIA_THREADS.lock() {
                    reg.retain(|(name, handle)| {
                        let h = *handle as windows_sys::Win32::Foundation::HANDLE;
                        let zero = windows_sys::Win32::Foundation::FILETIME {
                            dwLowDateTime: 0,
                            dwHighDateTime: 0,
                        };
                        let (mut c, mut e, mut k, mut u) = (zero, zero, zero, zero);
                        if windows_sys::Win32::System::Threading::GetThreadTimes(
                            h, &mut c, &mut e, &mut k, &mut u,
                        ) == 0
                        {
                            let _ = windows_sys::Win32::Foundation::CloseHandle(h);
                            thread_last.remove(handle);
                            return false;
                        }
                        // Thread exited but the handle is alive: exit
                        // time set → retire it.
                        if e.dwLowDateTime != 0 || e.dwHighDateTime != 0 {
                            let _ = windows_sys::Win32::Foundation::CloseHandle(h);
                            thread_last.remove(handle);
                            return false;
                        }
                        let now = ((u64::from(k.dwHighDateTime) << 32)
                            | u64::from(k.dwLowDateTime))
                            + ((u64::from(u.dwHighDateTime) << 32) | u64::from(u.dwLowDateTime));
                        let prev = thread_last.insert(*handle, now).unwrap_or(now);
                        let pct = (now.saturating_sub(prev)) as f64 / sys_delta * 100.0 * cores;
                        if pct >= 0.5 {
                            use std::fmt::Write as _;
                            let _ = write!(thread_bits, " {name} {pct:.0}%");
                        }
                        true
                    });
                }
                let threads = if thread_bits.is_empty() {
                    String::new()
                } else {
                    format!(" · threads:{thread_bits}")
                };
                match gpu {
                    Some((d3, enc, dec, copy)) => tracing::info!(
                        "telemetry: cpu proc {proc_pct:.0}% total {total_pct:.0}% · gpu 3d {d3:.0}% enc {enc:.0}% dec {dec:.0}% copy {copy:.0}%{}{threads}",
                        vram_mb
                            .map(|v| format!(" · vram {:.0} MB", v / 1_048_576.0))
                            .unwrap_or_default()
                    ),
                    None => tracing::info!(
                        "telemetry: cpu proc {proc_pct:.0}% total {total_pct:.0}%{threads}"
                    ),
                }
                let monitors = monitors_line();
                if monitors != last_monitors {
                    tracing::info!("{monitors} (topology changed)");
                    last_monitors = monitors;
                }
            }
        });
}

#[cfg(test)]
mod tests {
    /// Live PDH proof: start the sampler under a visible subscriber and
    /// let two lines land — verifies the counter path on whatever GPU
    /// this box carries (the whole point is that the same line works on
    /// the next box's vendor). Run:
    /// `cargo test --release -- --ignored telemetry_smoke --nocapture --test-threads=1`
    #[test]
    #[ignore = "diagnostic — prints live telemetry for ~11 s"]
    fn telemetry_smoke() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        super::start();
        std::thread::sleep(std::time::Duration::from_millis(11_500));
    }
}

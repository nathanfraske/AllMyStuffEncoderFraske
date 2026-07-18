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

/// Start the sampler; returns quietly if disabled or unavailable.
pub fn start() {
    if std::env::var("ALLMYSTUFF_TELEMETRY")
        .map(|v| matches!(v.trim(), "0" | "off" | "false"))
        .unwrap_or(false)
    {
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
            loop {
                std::thread::sleep(std::time::Duration::from_secs(5));
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
                match gpu {
                    Some((d3, enc, dec, copy)) => tracing::info!(
                        "telemetry: cpu proc {proc_pct:.0}% total {total_pct:.0}% · gpu 3d {d3:.0}% enc {enc:.0}% dec {dec:.0}% copy {copy:.0}%{}",
                        vram_mb
                            .map(|v| format!(" · vram {:.0} MB", v / 1_048_576.0))
                            .unwrap_or_default()
                    ),
                    None => tracing::info!(
                        "telemetry: cpu proc {proc_pct:.0}% total {total_pct:.0}%"
                    ),
                }
            }
        });
}

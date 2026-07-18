//! Small OS performance levers for the media-plane threads.
//!
//! Windows-focused today: the capture/encode loops pace themselves with
//! `thread::sleep`, whose quantum is the process's timer resolution — the
//! system default is ~15.6 ms, a hard fps ceiling for a 60 fps budget loop
//! and dead time in the async-MFT poll. Desktops often *appear* fine only
//! because some other app (typically a browser) holds the resolution at
//! 1 ms; a headless or idle host loses that by luck of what else is
//! running. Holding it ourselves while a stream is live removes the luck.
//!
//! Same story for scheduling: the capture/encode and input-injection
//! threads exist precisely for the moments the machine is loaded, and at
//! normal priority they degrade in lockstep with the load they're trying
//! to stream through. A single step above normal keeps them responsive
//! without starving anything (well below the audio/realtime classes).
//!
//! macOS gets the QoS-class arm of the same idea: on Apple silicon there is
//! **no thread-affinity API** — a thread's QoS class is the only mechanism
//! that steers it onto performance vs efficiency cores (and drives timer
//! coalescing and I/O throttling). An untagged worker thread can sit on
//! E-cores while the P-cores idle. The timer guard is Windows-only (macOS
//! timers aren't quantized the same way); everything is a silent no-op on
//! the remaining platforms.

/// Holds the OS timer resolution at 1 ms for as long as any guard lives.
/// winmm refcounts `timeBeginPeriod`/`timeEndPeriod` process-wide, so
/// nested guards are fine and drop order is free.
pub(crate) struct TimerResolutionGuard;

impl TimerResolutionGuard {
    pub(crate) fn hold() -> TimerResolutionGuard {
        #[cfg(windows)]
        unsafe {
            let _ = windows_sys::Win32::Media::timeBeginPeriod(1);
        }
        TimerResolutionGuard
    }
}

impl Drop for TimerResolutionGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            let _ = windows_sys::Win32::Media::timeEndPeriod(1);
        }
    }
}

/// Boost the current thread for media work — called at the top of each
/// capture/encode thread and the input-injector thread. Three levers, all
/// best-effort:
///
///  1. **Priority** one step above normal — under load the stream must not
///     degrade in lockstep with the load it exists to carry.
///  2. **EcoQoS opt-out** — Windows 11 can classify a background process's
///     threads as "efficiency" work and park them on E-cores at low clocks
///     (the headless node is exactly the shape that gets tagged). The
///     power-throttling opt-out declares this thread latency-sensitive.
///  3. **Performance-core preference** on hybrid CPUs (Intel P+E) — a CPU-set
///     hint listing the highest-efficiency-class cores. An encode/convert
///     pass that fits a 16 ms budget on a P-core can blow it on a
///     downclocked E-core; the hint keeps the media threads where the
///     budget holds while the scheduler retains the right to overrule
///     (CPU sets are a preference, unlike hard affinity — nothing starves
///     if a game owns the P-cores; our raised priority arbitrates).
/// Media threads registered for the telemetry line's per-thread CPU
/// split: (label, real thread handle). Every media thread passes through
/// [`boost_media_thread`], which makes this the one free hook.
#[cfg(windows)]
pub(crate) static MEDIA_THREADS: std::sync::Mutex<Vec<(String, isize)>> =
    std::sync::Mutex::new(Vec::new());

pub(crate) fn boost_media_thread() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::DUPLICATE_SAME_ACCESS;
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcess, GetCurrentThread, SetThreadInformation, SetThreadPriority,
            SetThreadSelectedCpuSets, ThreadPowerThrottling,
            THREAD_POWER_THROTTLING_CURRENT_VERSION, THREAD_POWER_THROTTLING_EXECUTION_SPEED,
            THREAD_POWER_THROTTLING_STATE, THREAD_PRIORITY_ABOVE_NORMAL,
        };
        let thread = GetCurrentThread();
        // Register for the telemetry per-thread CPU split — the pseudo
        // handle only means "me", so duplicate a real one.
        let mut real: windows_sys::Win32::Foundation::HANDLE = std::ptr::null_mut();
        if windows_sys::Win32::Foundation::DuplicateHandle(
            GetCurrentProcess(),
            thread,
            GetCurrentProcess(),
            &mut real,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        ) != 0
        {
            let name = std::thread::current().name().unwrap_or("media").to_string();
            if let Ok(mut reg) = MEDIA_THREADS.lock() {
                reg.push((name, real as isize));
            }
        }
        let _ = SetThreadPriority(thread, THREAD_PRIORITY_ABOVE_NORMAL);
        // ControlMask names the knob, StateMask=0 turns throttling OFF —
        // i.e. "never EcoQoS this thread".
        let throttle = THREAD_POWER_THROTTLING_STATE {
            Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: 0,
        };
        let _ = SetThreadInformation(
            thread,
            ThreadPowerThrottling,
            &throttle as *const THREAD_POWER_THROTTLING_STATE as *const core::ffi::c_void,
            core::mem::size_of::<THREAD_POWER_THROTTLING_STATE>() as u32,
        );
        let p_cores = performance_core_ids();
        if !p_cores.is_empty() {
            let _ = SetThreadSelectedCpuSets(thread, p_cores.as_ptr(), p_cores.len() as u32);
        }
    }
    #[cfg(target_os = "macos")]
    unsafe {
        // USER_INTERACTIVE is the P-core / no-coalescing tier; the relative
        // priority offset stays 0. Apple's guidance reserves this tier for
        // work the user is actively perceiving — a live stream's capture,
        // encode, and input-injection threads are exactly that.
        let _ = libc::pthread_set_qos_class_self_np(libc::QOS_CLASS_USER_INTERACTIVE, 0);
    }
}

/// CPU-set ids of the "performance" cores — the highest efficiency class
/// the machine reports. Empty on homogeneous CPUs (nothing to prefer) and
/// on query failure. Queried once per process.
#[cfg(windows)]
fn performance_core_ids() -> &'static [u32] {
    use std::sync::LazyLock;
    static IDS: LazyLock<Vec<u32>> = LazyLock::new(|| unsafe {
        use windows_sys::Win32::System::SystemInformation::{
            GetSystemCpuSetInformation, CPU_SET_INFORMATION_TYPE, SYSTEM_CPU_SET_INFORMATION,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        const CPU_SET_INFORMATION: CPU_SET_INFORMATION_TYPE = 0; // CpuSetInformation
        let mut needed = 0u32;
        let _ = GetSystemCpuSetInformation(
            core::ptr::null_mut(),
            0,
            &mut needed,
            GetCurrentProcess(),
            0,
        );
        if needed == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u8; needed as usize];
        if GetSystemCpuSetInformation(
            buf.as_mut_ptr() as *mut SYSTEM_CPU_SET_INFORMATION,
            needed,
            &mut needed,
            GetCurrentProcess(),
            0,
        ) == 0
        {
            return Vec::new();
        }
        // The buffer is a packed run of variable-size records; walk by each
        // record's own Size.
        let mut sets: Vec<(u32, u8)> = Vec::new();
        let mut at = 0usize;
        while at + core::mem::size_of::<u32>() * 2 <= needed as usize {
            let info = &*(buf.as_ptr().add(at) as *const SYSTEM_CPU_SET_INFORMATION);
            if info.Size == 0 {
                break;
            }
            if info.Type == CPU_SET_INFORMATION {
                let cpu = &info.Anonymous.CpuSet;
                sets.push((cpu.Id, cpu.EfficiencyClass));
            }
            at += info.Size as usize;
        }
        let max_class = sets.iter().map(|&(_, c)| c).max().unwrap_or(0);
        let min_class = sets.iter().map(|&(_, c)| c).min().unwrap_or(0);
        if max_class == min_class {
            return Vec::new(); // homogeneous — nothing to prefer
        }
        let total = sets.len();
        let ids: Vec<u32> = sets
            .into_iter()
            .filter(|&(_, c)| c == max_class)
            .map(|(id, _)| id)
            .collect();
        tracing::info!(
            "hybrid CPU: media threads prefer the {} performance cores (of {total} logical)",
            ids.len()
        );
        ids
    });
    &IDS
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn boost_raises_priority_and_prefers_p_cores_on_hybrid() {
        std::thread::spawn(|| {
            use windows_sys::Win32::System::Threading::{
                GetCurrentThread, GetThreadPriority, GetThreadSelectedCpuSets,
                THREAD_PRIORITY_ABOVE_NORMAL,
            };
            boost_media_thread();
            let got = unsafe { GetThreadPriority(GetCurrentThread()) };
            assert_eq!(got, THREAD_PRIORITY_ABOVE_NORMAL);
            // On a hybrid CPU the thread's selected CPU sets must be exactly
            // the performance cores; on homogeneous machines none are set.
            let expect = performance_core_ids().len() as u32;
            let mut count = 0u32;
            let ok = unsafe {
                GetThreadSelectedCpuSets(GetCurrentThread(), core::ptr::null_mut(), 0, &mut count)
            };
            assert_ne!(ok, 0, "GetThreadSelectedCpuSets failed");
            assert_eq!(count, expect, "selected CPU sets mirror the P-core list");
        })
        .join()
        .expect("boost thread");
    }

    #[test]
    fn timer_guard_is_balanced_and_reentrant() {
        // Two nested guards, dropped out of order — winmm refcounts, so the
        // only observable contract is "no panic, no error"; the sleep bench
        // (`bench_sleep_granularity`) shows the actual resolution effect.
        let a = TimerResolutionGuard::hold();
        let b = TimerResolutionGuard::hold();
        drop(a);
        drop(b);
    }
}

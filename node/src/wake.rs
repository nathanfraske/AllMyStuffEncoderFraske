//! Keep the display awake while this machine hosts a screen stream —
//! and **force it back on** when it's already dark.
//!
//! A remote console session *is* active use, but the OS can't see it:
//! no local input arrives, the idle timer runs out, and the display
//! sleeps — at which point the damage-driven capture backends simply
//! stop producing frames (and a deep-sleeping DisplayPort monitor drops
//! off the desktop entirely, taking the capture target with it).
//!
//! Two halves, because the OS treats them as different problems:
//!
//! **Prevention** — a hosted display route holds a [`DisplayAwake`]
//! guard for its lifetime:
//!
//!  * **Windows** — a keeper thread holds
//!    `SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED |
//!    ES_SYSTEM_REQUIRED)`. The state is per-thread, and capture threads
//!    come and go per route, so one dedicated thread owns it and exits
//!    (clearing the state) when the last guard drops.
//!  * **macOS** — IOKit power assertions (`NoDisplaySleepAssertion` +
//!    `PreventUserIdleSystemSleep`), released with the guard.
//!  * **Linux** — `org.freedesktop.ScreenSaver.Inhibit` over the session
//!    bus (GNOME's `org.gnome.SessionManager` as the fallback), the
//!    standard "a video is playing" inhibitor.
//!
//! **Waking** — [`force_display_on`]. A held inhibitor does *not*
//! re-light a panel that's already off, and a tiny injected mouse move
//! is exactly the class of synthetic input modern display power
//! managers filter (Windows especially: software jiggles don't relight
//! the panel; a real TeamViewer-style login does, because it fires the
//! strong activity signals). So the pulse uses each OS's *documented*
//! force-on calls, plus a synthetic key tap (F15 — exists on no
//! mainstream keyboard) that survives the filtering injected mouse
//! motion doesn't:
//!
//!  * **Windows** — a one-shot `SetThreadExecutionState(ES_DISPLAY_REQUIRED)`
//!    (no `ES_CONTINUOUS`: the documented "reset the display idle timer"
//!    pulse that re-lights an idle-darkened panel) plus a
//!    `WM_SYSCOMMAND`/`SC_MONITORPOWER(-1)` broadcast — the explicit
//!    "monitor on" message.
//!  * **macOS** — `IOPMAssertionDeclareUserActivity(kIOPMUserActiveLocal)`,
//!    *the* public "a user is here, light the display now" call.
//!  * **Linux** — DPMS `ForceLevel(On)` over xcb when an X display is
//!    reachable (X11 and XWayland), plus `SimulateUserActivity` on the
//!    freedesktop and GNOME screensaver buses and a logind
//!    `SetIdleHint(false)` — all best-effort.
//!
//! Pulses are rate-limited process-wide (one per few seconds), so the
//! capture loop can pulse on every frameless tick and the input
//! injector on every inbound click/keystroke without spamming the OS.
//! The injector only pulses **while a hosted stream is dark**
//! ([`force_display_on_if_dark`]) — when frames flow, real injected
//! input already counts as activity, and phantom F15 taps during active
//! typing would be noise.
//!
//! The honest limit: a **locked Windows console** gives user-session
//! processes no way to relight the secure desktop (that takes a SYSTEM
//! service, the TeamViewer/RustDesk architecture) — on such a box the
//! viewer at least gets told via the in-band capture status.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

static AWAKE: Mutex<AwakeState> = Mutex::new(AwakeState {
    holders: 0,
    backend: None,
});

struct AwakeState {
    holders: usize,
    backend: Option<platform::Hold>,
}

/// RAII guard: the display is kept awake while any of these is alive.
pub struct DisplayAwake(());

impl DisplayAwake {
    pub fn hold(reason: &str) -> DisplayAwake {
        let mut st = AWAKE.lock();
        st.holders += 1;
        if st.holders == 1 {
            match platform::acquire(reason) {
                Ok(hold) => {
                    st.backend = Some(hold);
                    tracing::info!("display keep-awake held ({reason})");
                }
                Err(e) => {
                    // The guard still counts; only the OS-level hold is
                    // missing. The capture status keeps the viewer honest.
                    tracing::warn!("display keep-awake unavailable: {e}");
                }
            }
        }
        DisplayAwake(())
    }
}

impl Drop for DisplayAwake {
    fn drop(&mut self) {
        let mut st = AWAKE.lock();
        st.holders = st.holders.saturating_sub(1);
        if st.holders == 0 {
            if let Some(hold) = st.backend.take() {
                platform::release(hold);
                tracing::info!("display keep-awake released");
            }
        }
    }
}

/// Whether some hosted display stream is currently dark (no frames /
/// failing grabs) — set by the capture loops' status reporting, read by
/// the input injector so a viewer's click can relight the panel.
static STREAM_DARK: AtomicBool = AtomicBool::new(false);

pub fn set_stream_dark(dark: bool) {
    STREAM_DARK.store(dark, Ordering::Relaxed);
}

/// Minimum spacing between OS wake pulses — callers pulse freely (every
/// frameless capture tick, every inbound click); this gate makes that
/// one OS-visible pulse per window.
const PULSE_EVERY: Duration = Duration::from_secs(3);

static LAST_PULSE: Mutex<Option<Instant>> = Mutex::new(None);

fn pulse_due() -> bool {
    let mut last = LAST_PULSE.lock();
    let now = Instant::now();
    match *last {
        Some(t) if now.duration_since(t) < PULSE_EVERY => false,
        _ => {
            *last = Some(now);
            true
        }
    }
}

/// Force the display on, now — the strong calls, not just a wiggle.
/// Safe to call hot (rate-limited internally); everything inside is
/// best-effort and logged at debug on refusal.
pub fn force_display_on() {
    if !pulse_due() {
        return;
    }
    platform::force_display_on();
    synthetic_user_activity();
}

/// The input injector's hook: pulse only while a hosted stream is dark.
/// A viewer wiggling and clicking at a black console is exactly the
/// "remote login wakes the machine" moment — when frames already flow,
/// the real injected input is activity enough.
pub fn force_display_on_if_dark() {
    if STREAM_DARK.load(Ordering::Relaxed) {
        force_display_on();
    }
}

/// A synthetic key tap + tiny mouse wiggle. The F15 *key* event is the
/// load-bearing half — display power managers honor injected key
/// presses where they filter small injected mouse motion — and F15
/// exists on no mainstream keyboard, so the focused app sees nothing it
/// reacts to.
fn synthetic_user_activity() {
    use enigo::{Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
    match Enigo::new(&Settings::default()) {
        Ok(mut enigo) => {
            let _ = enigo.key(Key::F15, Direction::Click);
            let _ = enigo.move_mouse(1, 0, Coordinate::Rel);
            let _ = enigo.move_mouse(-1, 0, Coordinate::Rel);
        }
        Err(e) => tracing::debug!("synthetic user activity unavailable: {e}"),
    }
}

#[cfg(windows)]
mod platform {
    /// The keeper thread's handle: dropping a message into the channel
    /// tells it to clear the execution state and exit.
    pub struct Hold(std::sync::mpsc::Sender<()>);

    pub fn acquire(_reason: &str) -> Result<Hold, String> {
        use windows_sys::Win32::System::Power::{
            SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
        };
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        std::thread::Builder::new()
            .name("display-keepawake".into())
            .spawn(move || {
                let ok = unsafe {
                    SetThreadExecutionState(
                        ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED,
                    )
                };
                if ok == 0 {
                    tracing::warn!("SetThreadExecutionState refused the keep-awake");
                }
                // Parks until release (or until every sender is gone, which
                // is the same thing).
                let _ = rx.recv();
                unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
            })
            .map_err(|e| e.to_string())?;
        Ok(Hold(tx))
    }

    pub fn release(hold: Hold) {
        let _ = hold.0.send(());
    }

    pub fn force_display_on() {
        use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_DISPLAY_REQUIRED};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            PostMessageW, HWND_BROADCAST, SC_MONITORPOWER, WM_SYSCOMMAND,
        };
        // One-shot, *without* ES_CONTINUOUS: the documented "reset the
        // display idle timer" pulse, which re-lights a panel the idle
        // timer turned off (the continuous hold only prevents).
        unsafe { SetThreadExecutionState(ES_DISPLAY_REQUIRED) };
        // And the explicit monitor-power message: lParam -1 = on. This
        // reaches the power manager even when injected input wouldn't.
        unsafe {
            PostMessageW(
                HWND_BROADCAST,
                WM_SYSCOMMAND,
                SC_MONITORPOWER as usize,
                -1isize,
            )
        };
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};

    type IOPMAssertionID = u32;
    const IOPM_ASSERTION_LEVEL_ON: u32 = 255;
    /// `kIOPMUserActiveLocal` — count as a *local* user so the display
    /// actually lights (`Remote` deliberately leaves it dark).
    const IOPM_USER_ACTIVE_LOCAL: u32 = 0;

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: CFStringRef,
            assertion_level: u32,
            assertion_name: CFStringRef,
            assertion_id: *mut IOPMAssertionID,
        ) -> i32;
        fn IOPMAssertionRelease(assertion_id: IOPMAssertionID) -> i32;
        fn IOPMAssertionDeclareUserActivity(
            assertion_name: CFStringRef,
            user_type: u32,
            assertion_id: *mut IOPMAssertionID,
        ) -> i32;
    }

    pub struct Hold(Vec<IOPMAssertionID>);

    pub fn acquire(reason: &str) -> Result<Hold, String> {
        let name = CFString::new(&format!("AllMyStuff: {reason}"));
        let mut ids = Vec::new();
        for kind in ["NoDisplaySleepAssertion", "PreventUserIdleSystemSleep"] {
            let kind = CFString::new(kind);
            let mut id: IOPMAssertionID = 0;
            let status = unsafe {
                IOPMAssertionCreateWithName(
                    kind.as_concrete_TypeRef(),
                    IOPM_ASSERTION_LEVEL_ON,
                    name.as_concrete_TypeRef(),
                    &mut id,
                )
            };
            if status == 0 {
                ids.push(id);
            }
        }
        if ids.is_empty() {
            return Err("IOPMAssertionCreateWithName refused both assertions".into());
        }
        Ok(Hold(ids))
    }

    pub fn release(hold: Hold) {
        for id in hold.0 {
            unsafe { IOPMAssertionRelease(id) };
        }
    }

    pub fn force_display_on() {
        // The public "a user is here — light the display now" call; the
        // system retires the assertion by itself after its activity
        // window, so there's nothing to release.
        let name = CFString::new("AllMyStuff remote session activity");
        let mut id: IOPMAssertionID = 0;
        let status = unsafe {
            IOPMAssertionDeclareUserActivity(
                name.as_concrete_TypeRef(),
                IOPM_USER_ACTIVE_LOCAL,
                &mut id,
            )
        };
        if status != 0 {
            tracing::debug!("IOPMAssertionDeclareUserActivity refused: {status}");
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    /// Which inhibition service granted, with the cookie to hand back.
    /// The connection stays alive with the hold — dropping it would let
    /// some services expire the inhibition.
    pub struct Hold {
        conn: zbus::blocking::Connection,
        service: Service,
        cookie: u32,
    }

    #[derive(Clone, Copy)]
    enum Service {
        ScreenSaver,
        GnomeSession,
    }

    pub fn acquire(reason: &str) -> Result<Hold, String> {
        let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
        // The freedesktop ScreenSaver interface is what video players use;
        // KDE and GNOME both serve it. GNOME's SessionManager is the
        // fallback for setups where only the GNOME name is on the bus.
        match inhibit_screensaver(&conn, reason) {
            Ok(cookie) => {
                return Ok(Hold {
                    conn,
                    service: Service::ScreenSaver,
                    cookie,
                })
            }
            Err(e) => tracing::debug!("org.freedesktop.ScreenSaver inhibit: {e}"),
        }
        match inhibit_gnome(&conn, reason) {
            Ok(cookie) => Ok(Hold {
                conn,
                service: Service::GnomeSession,
                cookie,
            }),
            Err(e) => Err(format!("no inhibition service answered (gnome: {e})")),
        }
    }

    pub fn release(hold: Hold) {
        let result = match hold.service {
            Service::ScreenSaver => proxy(
                &hold.conn,
                "org.freedesktop.ScreenSaver",
                "/org/freedesktop/ScreenSaver",
                "org.freedesktop.ScreenSaver",
            )
            .and_then(|p| p.call::<_, _, ()>("UnInhibit", &(hold.cookie))),
            Service::GnomeSession => proxy(
                &hold.conn,
                "org.gnome.SessionManager",
                "/org/gnome/SessionManager",
                "org.gnome.SessionManager",
            )
            .and_then(|p| p.call::<_, _, ()>("Uninhibit", &(hold.cookie))),
        };
        if let Err(e) = result {
            tracing::debug!("display keep-awake uninhibit: {e}");
        }
    }

    pub fn force_display_on() {
        // X11 and XWayland: DPMS force-on is the direct, documented call.
        if std::env::var_os("DISPLAY").is_some() {
            x11_dpms_on();
        }
        dbus_user_activity();
    }

    fn x11_dpms_on() {
        let connected = xcb::Connection::connect_with_extensions(
            None,
            &[],
            // Optional: a server without DPMS just skips this leg.
            &[xcb::Extension::Dpms],
        );
        let (conn, _) = match connected {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("DPMS wake: no X connection: {e}");
                return;
            }
        };
        if !conn.active_extensions().any(|e| e == xcb::Extension::Dpms) {
            return;
        }
        let cookie = conn.send_request_checked(&xcb::dpms::ForceLevel {
            power_level: xcb::dpms::DpmsMode::On,
        });
        if let Err(e) = conn.check_request(cookie) {
            tracing::debug!("DPMS ForceLevel(On): {e}");
        }
    }

    /// "A user is active" on every bus that listens: the freedesktop and
    /// GNOME screensaver interfaces, and logind's idle hint.
    fn dbus_user_activity() {
        if let Ok(conn) = zbus::blocking::Connection::session() {
            for (dest, path, iface) in [
                (
                    "org.freedesktop.ScreenSaver",
                    "/org/freedesktop/ScreenSaver",
                    "org.freedesktop.ScreenSaver",
                ),
                (
                    "org.gnome.ScreenSaver",
                    "/org/gnome/ScreenSaver",
                    "org.gnome.ScreenSaver",
                ),
            ] {
                if let Ok(p) = proxy(&conn, dest, path, iface) {
                    let _ = p.call_method("SimulateUserActivity", &());
                }
            }
        }
        if let Ok(sys) = zbus::blocking::Connection::system() {
            if let Ok(p) = proxy(
                &sys,
                "org.freedesktop.login1",
                "/org/freedesktop/login1/session/auto",
                "org.freedesktop.login1.Session",
            ) {
                let _ = p.call_method("SetIdleHint", &(false));
            }
        }
    }

    fn proxy<'a>(
        conn: &zbus::blocking::Connection,
        dest: &'a str,
        path: &'a str,
        iface: &'a str,
    ) -> zbus::Result<zbus::blocking::Proxy<'a>> {
        zbus::blocking::Proxy::new(conn, dest, path, iface)
    }

    fn inhibit_screensaver(conn: &zbus::blocking::Connection, reason: &str) -> zbus::Result<u32> {
        proxy(
            conn,
            "org.freedesktop.ScreenSaver",
            "/org/freedesktop/ScreenSaver",
            "org.freedesktop.ScreenSaver",
        )?
        .call("Inhibit", &("AllMyStuff", reason))
    }

    fn inhibit_gnome(conn: &zbus::blocking::Connection, reason: &str) -> zbus::Result<u32> {
        // Flags: 4 = inhibit suspend, 8 = inhibit idle.
        proxy(
            conn,
            "org.gnome.SessionManager",
            "/org/gnome/SessionManager",
            "org.gnome.SessionManager",
        )?
        .call("Inhibit", &("AllMyStuff", 0u32, reason, 12u32))
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod platform {
    pub struct Hold(());

    pub fn acquire(_reason: &str) -> Result<Hold, String> {
        Err("no keep-awake backend on this platform".into())
    }

    pub fn release(_hold: Hold) {}

    pub fn force_display_on() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guards_refcount_to_one_os_hold() {
        // Two overlapping guards must come and go without underflow —
        // the OS backend may legitimately fail in CI (no session bus, no
        // display), which is exactly the degraded path users without an
        // inhibition service get.
        let a = DisplayAwake::hold("test a");
        let b = DisplayAwake::hold("test b");
        assert_eq!(AWAKE.lock().holders, 2);
        drop(a);
        assert_eq!(AWAKE.lock().holders, 1);
        drop(b);
        assert_eq!(AWAKE.lock().holders, 0);
        assert!(
            AWAKE.lock().backend.is_none(),
            "hold released with last guard"
        );
    }

    #[test]
    fn pulses_are_rate_limited() {
        // Only this test touches the pulse gate; the first call in the
        // process opens the window, the immediate second is swallowed.
        assert!(pulse_due());
        assert!(!pulse_due());
    }

    #[test]
    fn dark_flag_gates_the_input_pulse() {
        set_stream_dark(false);
        assert!(!STREAM_DARK.load(Ordering::Relaxed));
        set_stream_dark(true);
        assert!(STREAM_DARK.load(Ordering::Relaxed));
        set_stream_dark(false);
    }
}

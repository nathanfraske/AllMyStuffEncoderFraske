//! Keep the display awake while this machine hosts a screen stream.
//!
//! A remote console session *is* active use, but the OS can't see it:
//! no local input arrives, the idle timer runs out, and the display
//! sleeps — at which point the damage-driven capture backends simply
//! stop producing frames (and a deep-sleeping DisplayPort monitor drops
//! off the desktop entirely, taking the capture target with it). So a
//! hosted display route holds a [`DisplayAwake`] guard for its lifetime:
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
//! Guards are refcounted process-wide: the OS-level hold is acquired when
//! the first route starts and released when the last one ends. Everything
//! is best-effort — a desktop with no inhibition service just keeps the
//! old behaviour, and the in-band capture status (`vstat`) tells the
//! viewer when the display went away regardless.
//!
//! [`nudge_display`] is the other half: holding the display *awake*
//! doesn't *wake* one that's already asleep when the route starts, but a
//! synthetic 1-px mouse wiggle does, on every platform — the same
//! `enigo` machinery the control plane injects with.

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

/// Wake a display that's already asleep: a relative 1-px mouse move out
/// and back, which every OS treats as user activity. Best-effort — a
/// session that can't inject (no permission, no seat) just skips it.
pub fn nudge_display() {
    use enigo::{Coordinate, Enigo, Mouse, Settings};
    match Enigo::new(&Settings::default()) {
        Ok(mut enigo) => {
            let _ = enigo.move_mouse(1, 0, Coordinate::Rel);
            let _ = enigo.move_mouse(-1, 0, Coordinate::Rel);
        }
        Err(e) => tracing::debug!("display wake nudge unavailable: {e}"),
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
}

#[cfg(target_os = "macos")]
mod platform {
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};

    type IOPMAssertionID = u32;
    const IOPM_ASSERTION_LEVEL_ON: u32 = 255;

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: CFStringRef,
            assertion_level: u32,
            assertion_name: CFStringRef,
            assertion_id: *mut IOPMAssertionID,
        ) -> i32;
        fn IOPMAssertionRelease(assertion_id: IOPMAssertionID) -> i32;
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
}

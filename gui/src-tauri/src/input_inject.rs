//! The input media plane's *sink* side: apply a remote console's keyboard
//! and mouse events to this machine, via `enigo` (SendInput on Windows,
//! CoreGraphics on macOS, X11 on Linux).
//!
//! Mouse coordinates arrive normalized 0..1 over the sender's *view of one
//! of our screens* (the paired display stream) plus which screen that is —
//! the `screen` field of [`InputAction::MouseMove`], absent for the
//! primary. Injection resolves that monitor's rectangle in the global
//! desktop space and lands the cursor inside it, so control follows the
//! screen the console is showing. The two ends still never negotiate
//! resolutions — exactly the piKVM model, one monitor at a time.
//!
//! On Windows the absolute move is raised by hand (`SendInput` with
//! `MOUSEEVENTF_VIRTUALDESK`): enigo's absolute coordinates normalize
//! against the primary monitor only, which physically can't express a
//! point on a second screen. macOS and X11 take global coordinates
//! through enigo directly.
//!
//! Keys arrive as the DOM `KeyboardEvent.key` value: printable characters
//! inject as themselves (so the *typist's* layout wins), named keys map
//! through a fixed table.
//!
//! `Enigo` isn't `Send`, so one dedicated thread owns it and drains a
//! channel of [`InputAction`]s; the mesh just calls [`Injector::apply`]
//! after its route/ownership gates pass. The thread starts lazily on the
//! first event and dies with the app.

use std::sync::mpsc;

#[cfg(not(windows))]
use enigo::Coordinate;
use enigo::{Axis, Button, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use parking_lot::Mutex;

use allmystuff_session::InputAction;

#[derive(Default)]
pub struct Injector {
    tx: Mutex<Option<mpsc::Sender<InputAction>>>,
}

impl Injector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue one event for injection. Starts the injector thread on first
    /// use; if the platform refuses (no display server, missing
    /// permissions) the failure is logged once and events are dropped.
    pub fn apply(&self, action: InputAction) {
        let mut tx = self.tx.lock();
        if tx.is_none() {
            let (sender, rx) = mpsc::channel::<InputAction>();
            std::thread::spawn(move || run_injector(rx));
            *tx = Some(sender);
        }
        if let Some(t) = tx.as_ref() {
            if t.send(action).is_err() {
                // The thread ended (platform refused); allow a retry on the
                // next event rather than wedging forever.
                *tx = None;
            }
        }
    }
}

fn run_injector(rx: mpsc::Receiver<InputAction>) {
    let mut enigo = match Enigo::new(&Settings::default()) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("input injection unavailable on this machine: {e}");
            return;
        }
    };
    let mut screens = ScreenMap::load();
    while let Ok(action) = rx.recv() {
        // A viewer clicking or typing at a *dark* console is the "remote
        // login wakes the machine" moment — relight the panel so the
        // stream they're driving blind comes back. No-op while frames
        // flow (rate-limited and gated inside).
        if matches!(
            action,
            InputAction::MouseButton { .. } | InputAction::Key { .. }
        ) {
            crate::wake::force_display_on_if_dark();
        }
        let result = match action {
            InputAction::MouseMove { x, y, screen } => {
                let rect = screens.resolve(screen);
                let (gx, gy) = rect.denorm(x, y);
                move_mouse_global(&mut enigo, gx, gy)
            }
            InputAction::MouseButton { button, down } => match dom_button(button) {
                Some(b) => enigo.button(b, direction(down)).map_err(|e| e.to_string()),
                None => Ok(()),
            },
            InputAction::Wheel { dx, dy } => {
                let mut r = Ok(());
                if dy.abs() >= 0.5 {
                    r = enigo
                        .scroll(dy.round() as i32, Axis::Vertical)
                        .map_err(|e| e.to_string());
                }
                if r.is_ok() && dx.abs() >= 0.5 {
                    r = enigo
                        .scroll(dx.round() as i32, Axis::Horizontal)
                        .map_err(|e| e.to_string());
                }
                r
            }
            InputAction::Key { ref key, down } => match map_key(key) {
                Some(k) => enigo.key(k, direction(down)).map_err(|e| e.to_string()),
                None => Ok(()),
            },
        };
        if let Err(e) = result {
            tracing::debug!("input injection event failed: {e}");
        }
    }
}

/// Land the cursor on a global desktop coordinate (any monitor).
#[cfg(not(windows))]
fn move_mouse_global(enigo: &mut Enigo, gx: i32, gy: i32) -> Result<(), String> {
    // CoreGraphics and X11 take global coordinates as-is.
    enigo
        .move_mouse(gx, gy, Coordinate::Abs)
        .map_err(|e| e.to_string())
}

/// Land the cursor on a global desktop coordinate (any monitor).
///
/// Raised by hand because enigo's `Coordinate::Abs` normalizes against the
/// primary monitor without `MOUSEEVENTF_VIRTUALDESK` — a point on a second
/// screen is simply unreachable through it. With the flag, 0..65535 spans
/// the whole virtual desktop.
#[cfg(windows)]
fn move_mouse_global(_enigo: &mut Enigo, gx: i32, gy: i32) -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE,
        MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    unsafe {
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = i64::from(GetSystemMetrics(SM_CXVIRTUALSCREEN)).max(2);
        let vh = i64::from(GetSystemMetrics(SM_CYVIRTUALSCREEN)).max(2);
        // Pixel → the API's 0..65535 span of the virtual desktop, rounding
        // at the pixel center.
        let nx =
            ((i64::from(gx) - i64::from(vx)).clamp(0, vw - 1) * 65535 + (vw - 1) / 2) / (vw - 1);
        let ny =
            ((i64::from(gy) - i64::from(vy)).clamp(0, vh - 1) * 65535 + (vh - 1) / 2) / (vh - 1);
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: nx as i32,
                    dy: ny as i32,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        if SendInput(&[input], std::mem::size_of::<INPUT>() as i32) == 1 {
            Ok(())
        } else {
            Err("SendInput refused the move".into())
        }
    }
}

fn direction(down: bool) -> Direction {
    if down {
        Direction::Press
    } else {
        Direction::Release
    }
}

/// One monitor's rectangle in the global desktop space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScreenRect {
    id: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    primary: bool,
}

impl ScreenRect {
    /// Normalized 0..1 over this screen → a global pixel inside it.
    fn denorm(&self, nx: f64, ny: f64) -> (i32, i32) {
        let px = (nx.clamp(0.0, 1.0) * (self.w.max(1) - 1) as f64).round() as i32;
        let py = (ny.clamp(0.0, 1.0) * (self.h.max(1) - 1) as f64).round() as i32;
        (self.x + px, self.y + py)
    }
}

/// The monitor layout, refreshed when an event names a screen we don't
/// know (hotplug since the last look).
struct ScreenMap {
    rects: Vec<ScreenRect>,
}

impl ScreenMap {
    fn load() -> Self {
        ScreenMap {
            rects: query_screens(),
        }
    }

    /// The rectangle for `screen` — the primary when unnamed, a re-query
    /// then the primary when the named one is gone. The fallback rect (no
    /// monitors readable at all) keeps the old primary-screen behaviour.
    fn resolve(&mut self, screen: Option<u32>) -> ScreenRect {
        if let Some(id) = screen {
            if let Some(r) = self.rects.iter().find(|r| r.id == id) {
                return *r;
            }
            self.rects = query_screens();
            if let Some(r) = self.rects.iter().find(|r| r.id == id) {
                return *r;
            }
            tracing::debug!("input names screen {id} but it isn't attached — using the primary");
        }
        self.primary()
    }

    fn primary(&self) -> ScreenRect {
        self.rects
            .iter()
            .find(|r| r.primary)
            .or_else(|| self.rects.first())
            .copied()
            .unwrap_or(ScreenRect {
                id: 0,
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
                primary: true,
            })
    }
}

/// Every attached monitor's global rect, via the same enumeration the
/// capture side uses — so the ids in `screen:<id>` capabilities resolve
/// back to the same physical screens here.
fn query_screens() -> Vec<ScreenRect> {
    let Ok(monitors) = xcap::Monitor::all() else {
        return Vec::new();
    };
    monitors
        .iter()
        .filter_map(|m| {
            Some(ScreenRect {
                id: m.id().ok()?,
                x: m.x().unwrap_or(0),
                y: m.y().unwrap_or(0),
                w: m.width().unwrap_or(0).max(1),
                h: m.height().unwrap_or(0).max(1),
                primary: m.is_primary().unwrap_or(false),
            })
        })
        .collect()
}

/// DOM `MouseEvent.button` → enigo button. 3/4 (browser back/forward)
/// don't inject — they'd navigate the *remote*'s browser focus oddly.
fn dom_button(button: u8) -> Option<Button> {
    match button {
        0 => Some(Button::Left),
        1 => Some(Button::Middle),
        2 => Some(Button::Right),
        _ => None,
    }
}

/// DOM `KeyboardEvent.key` → enigo key. A single-char value is a
/// printable character and injects as itself (typist's layout wins);
/// named keys map explicitly. `None` = a key we deliberately don't carry
/// (media keys, IME composition, lock keys we can't faithfully mirror).
fn map_key(key: &str) -> Option<Key> {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(Key::Unicode(c));
    }
    Some(match key {
        "Enter" => Key::Return,
        "Escape" => Key::Escape,
        "Backspace" => Key::Backspace,
        "Tab" => Key::Tab,
        "Delete" => Key::Delete,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "ArrowUp" => Key::UpArrow,
        "ArrowDown" => Key::DownArrow,
        "ArrowLeft" => Key::LeftArrow,
        "ArrowRight" => Key::RightArrow,
        "Shift" => Key::Shift,
        "Control" => Key::Control,
        "Alt" => Key::Alt,
        "Meta" => Key::Meta,
        "CapsLock" => Key::CapsLock,
        "F1" => Key::F1,
        "F2" => Key::F2,
        "F3" => Key::F3,
        "F4" => Key::F4,
        "F5" => Key::F5,
        "F6" => Key::F6,
        "F7" => Key::F7,
        "F8" => Key::F8,
        "F9" => Key::F9,
        "F10" => Key::F10,
        "F11" => Key::F11,
        "F12" => Key::F12,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_keys_inject_as_unicode() {
        assert_eq!(map_key("a"), Some(Key::Unicode('a')));
        assert_eq!(map_key("?"), Some(Key::Unicode('?')));
        // Layout-resolved characters carry through untranslated.
        assert_eq!(map_key("ü"), Some(Key::Unicode('ü')));
    }

    #[test]
    fn named_keys_map_and_unknown_ones_drop() {
        assert_eq!(map_key("Enter"), Some(Key::Return));
        assert_eq!(map_key("ArrowLeft"), Some(Key::LeftArrow));
        assert_eq!(map_key("MediaPlayPause"), None);
    }

    #[test]
    fn coordinates_denormalize_onto_the_named_screen() {
        // A 1920×1080 primary with a 2560×1440 screen to its right.
        let right = ScreenRect {
            id: 7,
            x: 1920,
            y: 0,
            w: 2560,
            h: 1440,
            primary: false,
        };
        assert_eq!(right.denorm(0.0, 0.0), (1920, 0));
        assert_eq!(right.denorm(1.0, 1.0), (1920 + 2559, 1439));
        let (cx, cy) = right.denorm(0.5, 0.5);
        assert_eq!((cx, cy), (1920 + 1280, 720));
        // Out-of-range input clamps inside the screen, not off the edge.
        assert_eq!(right.denorm(7.0, -3.0), (1920 + 2559, 0));
    }

    #[test]
    fn resolve_prefers_the_named_screen_and_falls_back_to_primary() {
        let primary = ScreenRect {
            id: 1,
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
            primary: true,
        };
        let right = ScreenRect {
            id: 7,
            x: 1920,
            y: 0,
            w: 2560,
            h: 1440,
            primary: false,
        };
        let mut map = ScreenMap {
            rects: vec![primary, right],
        };
        assert_eq!(map.resolve(Some(7)), right);
        assert_eq!(map.resolve(None), primary);
        // An unknown id re-queries (nothing here in CI) and lands on the
        // primary of whatever the map now holds.
        let mut map = ScreenMap {
            rects: vec![primary, right],
        };
        let fallback = map.resolve(Some(99));
        assert!(fallback.primary);
    }

    #[test]
    fn only_real_mouse_buttons_inject() {
        assert_eq!(dom_button(0), Some(Button::Left));
        assert_eq!(dom_button(2), Some(Button::Right));
        assert_eq!(dom_button(4), None);
    }
}

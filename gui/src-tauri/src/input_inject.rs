//! The input media plane's *sink* side: apply a remote console's keyboard
//! and mouse events to this machine, via `enigo` (SendInput on Windows,
//! CoreGraphics on macOS, X11 on Linux).
//!
//! Mouse coordinates arrive normalized 0..1 over the sender's *view of our
//! screen* (the paired display stream), so injection multiplies by our own
//! primary-screen size — the two ends never negotiate resolutions, exactly
//! the piKVM model. Keys arrive as the DOM `KeyboardEvent.key` value:
//! printable characters inject as themselves (so the *typist's* layout
//! wins), named keys map through a fixed table.
//!
//! `Enigo` isn't `Send`, so one dedicated thread owns it and drains a
//! channel of [`InputAction`]s; the mesh just calls [`Injector::apply`]
//! after its route/ownership gates pass. The thread starts lazily on the
//! first event and dies with the app.

use std::sync::mpsc;

use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
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
    // Our primary screen, for denormalizing mouse coordinates. Queried
    // once — a mid-session resolution change just lands slightly off until
    // the route is rewired (v1, called out honestly).
    let (sw, sh) = enigo.main_display().unwrap_or((1920, 1080));
    while let Ok(action) = rx.recv() {
        let result = match action {
            InputAction::MouseMove { x, y } => {
                enigo.move_mouse(denorm(x, sw), denorm(y, sh), Coordinate::Abs)
            }
            InputAction::MouseButton { button, down } => match dom_button(button) {
                Some(b) => enigo.button(b, direction(down)),
                None => Ok(()),
            },
            InputAction::Wheel { dx, dy } => {
                let mut r = Ok(());
                if dy.abs() >= 0.5 {
                    r = enigo.scroll(dy.round() as i32, Axis::Vertical);
                }
                if r.is_ok() && dx.abs() >= 0.5 {
                    r = enigo.scroll(dx.round() as i32, Axis::Horizontal);
                }
                r
            }
            InputAction::Key { ref key, down } => match map_key(key) {
                Some(k) => enigo.key(k, direction(down)),
                None => Ok(()),
            },
        };
        if let Err(e) = result {
            tracing::debug!("input injection event failed: {e}");
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

fn denorm(v: f64, size: i32) -> i32 {
    (v.clamp(0.0, 1.0) * size.max(1) as f64).round() as i32
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
    fn coordinates_denormalize_onto_the_screen() {
        assert_eq!(denorm(0.0, 1920), 0);
        assert_eq!(denorm(0.5, 1920), 960);
        assert_eq!(denorm(1.0, 1080), 1080);
        // Out-of-range input clamps instead of flying off-screen.
        assert_eq!(denorm(7.0, 1920), 1920);
    }

    #[test]
    fn only_real_mouse_buttons_inject() {
        assert_eq!(dom_button(0), Some(Button::Left));
        assert_eq!(dom_button(2), Some(Button::Right));
        assert_eq!(dom_button(4), None);
    }
}

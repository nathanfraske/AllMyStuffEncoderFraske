//! Collapse one *physical* input device's many kernel/OS endpoints into a
//! single inventory entry.
//!
//! Modern keyboards and mice register several HID interfaces — a gaming
//! mouse typically shows up as "X", "X Keyboard", and "X Consumer
//! Control"; a wireless unifying receiver as four or five entries. Listing
//! each endpoint reads as duplicated junk in the UI, so the per-platform
//! collectors hand their raw endpoints here with a **group key** that
//! identifies the physical unit (Linux: `vendor:product` + the `Phys=`
//! port path; Windows: the PnP `VID:PID`), and one merged device comes
//! out per key:
//!
//!  * **name** — the shortest endpoint name (the common base: "Logitech
//!    USB Receiver", not "… Consumer Control"), suffixed `(keyboard +
//!    mouse)` when one unit genuinely serves both.
//!  * **kind** — the most specific endpoint kind (a gaming mouse whose
//!    extra macro-keys endpoint registers as a keyboard is still a mouse).
//!  * **endpoints** — how many were merged, kept on the record so nothing
//!    is hidden, just folded.
//!
//! Endpoints with no group key (virtual devices without identity) pass
//! through untouched. Pure and platform-independent, so the policy is
//! unit-tested once and identical everywhere.

use crate::types::{InputDevice, InputKind};

/// One raw endpoint as a platform collector found it.
pub struct RawInput {
    pub name: String,
    pub kind: InputKind,
    /// Identity of the physical unit this endpoint belongs to. Endpoints
    /// sharing a key merge; `None` means "can't tell — keep it as-is".
    pub group: Option<String>,
    /// The id to use when the endpoint stands alone.
    pub fallback_id: String,
}

/// Merge raw endpoints into one entry per physical device, preserving
/// first-seen order.
pub fn merge_inputs(raw: Vec<RawInput>) -> Vec<InputDevice> {
    // (key, members) in first-seen order; ungrouped endpoints get a unique
    // synthetic key so they pass through one-to-one.
    let mut order: Vec<String> = Vec::new();
    let mut members: std::collections::HashMap<String, Vec<RawInput>> =
        std::collections::HashMap::new();
    for (i, r) in raw.into_iter().enumerate() {
        let key = match &r.group {
            Some(g) => format!("g:{g}"),
            None => format!("solo:{i}"),
        };
        members.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            Vec::new()
        });
        members.get_mut(&key).expect("just inserted").push(r);
    }

    order
        .into_iter()
        .filter_map(|key| {
            let group = members.remove(&key)?;
            let endpoints = group.len() as u32;
            // The shortest name is the common base the others suffix
            // ("Logitech USB Receiver" vs "… Consumer Control"), and the
            // endpoint *carrying* the base name is the unit's primary
            // function — on a gaming mouse that's the mouse endpoint (its
            // keyboard endpoints are macro plumbing), on a receiver it's
            // the keyboard.
            let base = group
                .iter()
                .map(|r| r.name.trim())
                .min_by_key(|n| n.len())
                .unwrap_or("")
                .to_string();
            let base_kind = group
                .iter()
                .filter(|r| r.name.trim() == base)
                .map(|r| r.kind)
                .min_by_key(|k| specificity(*k))
                .unwrap_or(InputKind::Other);
            let kind = if base_kind != InputKind::Other {
                base_kind
            } else {
                group
                    .iter()
                    .map(|r| r.kind)
                    .min_by_key(|k| specificity(*k))
                    .unwrap_or(InputKind::Other)
            };
            let name = match combined_suffix(kind, &group) {
                Some(suffix) => format!("{base} ({suffix})"),
                None => base,
            };
            let id = match group[0].group.as_deref() {
                Some(g) => format!("input:{}", slug(g)),
                None => group[0].fallback_id.clone(),
            };
            Some(InputDevice {
                id,
                name,
                kind,
                endpoints,
            })
        })
        .collect()
}

/// Lower = more specific. A unit is named by its most specific endpoint:
/// the keyboard interface on a mouse is plumbing, the mouse on a keyboard
/// combo is the point.
fn specificity(k: InputKind) -> u8 {
    match k {
        InputKind::Tablet => 0,
        InputKind::Touchscreen => 1,
        InputKind::Gamepad => 2,
        InputKind::Touchpad => 3,
        InputKind::Mouse => 4,
        InputKind::Keyboard => 5,
        InputKind::Other => 6,
    }
}

/// When one physical unit genuinely serves both typing and pointing (a
/// unifying receiver, a keyboard with a built-in touchpad), say so. A
/// *pointer-first* unit with auxiliary keyboard endpoints (a gaming
/// mouse's macro keys) stays plainly named — those endpoints are
/// plumbing, not a keyboard you'd reach for.
fn combined_suffix(unit_kind: InputKind, group: &[RawInput]) -> Option<&'static str> {
    if unit_kind != InputKind::Keyboard {
        return None;
    }
    if group.iter().any(|r| r.kind == InputKind::Mouse) {
        Some("keyboard + mouse")
    } else if group.iter().any(|r| r.kind == InputKind::Touchpad) {
        Some("keyboard + touchpad")
    } else {
        None
    }
}

/// Lowercased alphanumerics, everything else `-` — a stable id segment.
pub fn slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(name: &str, kind: InputKind, group: Option<&str>) -> RawInput {
        RawInput {
            name: name.into(),
            kind,
            group: group.map(str::to_string),
            fallback_id: format!("input:{}", slug(name)),
        }
    }

    #[test]
    fn a_gaming_mouse_collapses_to_one_plain_mouse() {
        // Razer-style: the unit registers a mouse + two keyboard-ish
        // endpoints for macros/media. One entry, kind mouse, no combo
        // suffix (the keyboard endpoints are plumbing), count kept.
        let merged = merge_inputs(vec![
            raw(
                "Razer Razer Viper",
                InputKind::Mouse,
                Some("1532:0078:usb-1"),
            ),
            raw(
                "Razer Razer Viper Keyboard",
                InputKind::Keyboard,
                Some("1532:0078:usb-1"),
            ),
            raw(
                "Razer Razer Viper Consumer Control",
                InputKind::Keyboard,
                Some("1532:0078:usb-1"),
            ),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].kind, InputKind::Mouse);
        assert_eq!(merged[0].endpoints, 3);
        assert_eq!(merged[0].name, "Razer Razer Viper");
        assert_eq!(merged[0].id, "input:1532-0078-usb-1");
    }

    #[test]
    fn a_unifying_receiver_reads_as_one_combo_device() {
        // The base-named endpoint is a keyboard and a real mouse rides the
        // same receiver → a keyboard-kind unit that says it's both.
        let merged = merge_inputs(vec![
            raw(
                "Logitech USB Receiver",
                InputKind::Keyboard,
                Some("046d:c52b:usb-2"),
            ),
            raw(
                "Logitech USB Receiver Mouse",
                InputKind::Mouse,
                Some("046d:c52b:usb-2"),
            ),
            raw(
                "Logitech USB Receiver Consumer Control",
                InputKind::Keyboard,
                Some("046d:c52b:usb-2"),
            ),
            raw(
                "Logitech USB Receiver System Control",
                InputKind::Keyboard,
                Some("046d:c52b:usb-2"),
            ),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].name, "Logitech USB Receiver (keyboard + mouse)");
        assert_eq!(
            merged[0].kind,
            InputKind::Keyboard,
            "the base-named endpoint names the unit"
        );
        assert_eq!(merged[0].endpoints, 4);
    }

    #[test]
    fn distinct_units_of_the_same_model_stay_separate() {
        // Two identical mice on different ports → different group keys.
        let merged = merge_inputs(vec![
            raw(
                "USB Optical Mouse",
                InputKind::Mouse,
                Some("046d:c077:usb-2"),
            ),
            raw(
                "USB Optical Mouse",
                InputKind::Mouse,
                Some("046d:c077:usb-3"),
            ),
        ]);
        assert_eq!(merged.len(), 2);
        assert!(merged[0].id != merged[1].id);
        assert_eq!(merged[0].endpoints, 1);
    }

    #[test]
    fn plain_and_ungrouped_devices_pass_through() {
        let merged = merge_inputs(vec![
            raw(
                "AT Translated Set 2 keyboard",
                InputKind::Keyboard,
                Some("0001:0001:isa0060"),
            ),
            raw("Virtual uinput pen", InputKind::Tablet, None),
        ]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].name, "AT Translated Set 2 keyboard");
        assert_eq!(merged[0].kind, InputKind::Keyboard);
        assert_eq!(merged[1].id, "input:virtual-uinput-pen");
        assert_eq!(merged[1].endpoints, 1);
    }

    #[test]
    fn keyboard_with_touchpad_says_so() {
        let merged = merge_inputs(vec![
            raw("Combo K400", InputKind::Keyboard, Some("046d:404d:usb-1")),
            raw(
                "Combo K400 Touchpad",
                InputKind::Touchpad,
                Some("046d:404d:usb-1"),
            ),
        ]);
        assert_eq!(merged[0].name, "Combo K400 (keyboard + touchpad)");
        assert_eq!(merged[0].kind, InputKind::Keyboard);
    }
}

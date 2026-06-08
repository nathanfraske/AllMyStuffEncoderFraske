//! Windows device probing.
//!
//! Linux is the reference implementation (`linux.rs`); on Windows the
//! host basics come from `sysinfo` and the device classes here are
//! scaffolded against PowerShell/CIM (`Get-CimInstance`). Collectors that
//! aren't wired yet return empty so the scan still yields a complete,
//! correctly-typed `Inventory`. Full WMI/SetupAPI enumeration is a
//! follow-up.

#![cfg(target_os = "windows")]

use crate::types::*;

pub fn collect_gpus() -> Vec<Gpu> {
    Vec::new()
}

pub fn collect_displays() -> Vec<Display> {
    Vec::new()
}

pub fn collect_audio() -> (Vec<AudioDevice>, Vec<AudioDevice>) {
    (Vec::new(), Vec::new())
}

pub fn collect_cameras() -> Vec<Camera> {
    Vec::new()
}

pub fn collect_inputs() -> Vec<InputDevice> {
    Vec::new()
}

pub fn collect_usb() -> Vec<UsbDevice> {
    Vec::new()
}

//! Windows virtual-key codes: names for the UI, parsing, and "press a key"
//! capture for rebinding.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

pub fn down(vk: i32) -> bool {
    // High bit set = key currently down.
    unsafe { GetAsyncKeyState(vk) < 0 }
}

const NAMED: &[(i32, &str)] = &[
    (0x01, "mouse1"),
    (0x02, "mouse2"),
    (0x04, "mouse3"),
    (0x05, "mouse4"),
    (0x06, "mouse5"),
    (0x08, "backspace"),
    (0x09, "tab"),
    (0x0D, "enter"),
    (0x10, "shift"),
    (0x11, "ctrl"),
    (0x12, "alt"),
    (0x14, "capslock"),
    (0x1B, "esc"),
    (0x20, "space"),
    (0x21, "pageup"),
    (0x22, "pagedown"),
    (0x23, "end"),
    (0x24, "home"),
    (0x25, "left"),
    (0x26, "up"),
    (0x27, "right"),
    (0x28, "down"),
    (0x2D, "insert"),
    (0x2E, "delete"),
    (0xA0, "lshift"),
    (0xA1, "rshift"),
    (0xA2, "lctrl"),
    (0xA3, "rctrl"),
    (0xA4, "lalt"),
    (0xA5, "ralt"),
    (0xC0, "`"),
];

pub fn name(vk: i32) -> String {
    if let Some((_, n)) = NAMED.iter().find(|(v, _)| *v == vk) {
        return (*n).into();
    }
    match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        0x60..=0x69 => format!("num{}", vk - 0x60),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        _ => format!("0x{vk:02X}"),
    }
}

pub fn parse(s: &str) -> Option<i32> {
    let lower = s.trim().to_ascii_lowercase();
    if let Some((vk, _)) = NAMED.iter().find(|(_, n)| *n == lower) {
        return Some(*vk);
    }
    if let Some(hex) = lower.strip_prefix("0x") {
        return i32::from_str_radix(hex, 16).ok().filter(|v| (1..255).contains(v));
    }
    if let Some(n) = lower.strip_prefix("num").and_then(|n| n.parse::<i32>().ok()) {
        return (0..=9).contains(&n).then_some(0x60 + n);
    }
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<i32>().ok()) {
        return (1..=24).contains(&n).then_some(0x6F + n);
    }
    match lower.as_bytes() {
        [c] if c.is_ascii_alphanumeric() => Some(c.to_ascii_uppercase() as i32),
        _ => None,
    }
}

static CAPTURING: AtomicBool = AtomicBool::new(false);
/// 0 while waiting, -1 when cancelled with Esc, else the captured key.
static CAPTURED: AtomicI32 = AtomicI32::new(0);
/// Keys held when capture started must be released first.
static ARMED: AtomicBool = AtomicBool::new(false);

pub fn start_capture() {
    CAPTURED.store(0, Ordering::Relaxed);
    ARMED.store(false, Ordering::Relaxed);
    CAPTURING.store(true, Ordering::Relaxed);
}

pub fn captured() -> i32 {
    CAPTURED.load(Ordering::Relaxed)
}

/// Polled by the audio thread.
pub fn poll_capture() {
    if !CAPTURING.load(Ordering::Relaxed) {
        return;
    }
    // Left click is how you press the UI button, so it can't be the key.
    let pressed = (0x02..0xFF).find(|&vk| down(vk));
    if !ARMED.load(Ordering::Relaxed) {
        if pressed.is_none() && !down(0x01) {
            ARMED.store(true, Ordering::Relaxed);
        }
        return;
    }
    if let Some(vk) = pressed {
        CAPTURED.store(if vk == 0x1B { -1 } else { vk }, Ordering::Relaxed);
        CAPTURING.store(false, Ordering::Relaxed);
    }
}

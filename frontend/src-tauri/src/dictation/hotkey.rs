//! macOS hold-to-talk via CGEvent tap (not tauri-plugin-global-shortcut).
//! Honors configured hold / hands-free shortcut strings (Fn remains default).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    HoldStart,
    HoldEnd,
    Cancel,
    HandsFreeToggle,
}

/// Parsed shortcut configuration shared with the tap thread.
#[derive(Debug, Clone)]
pub struct HotkeyConfig {
    /// When true, hold uses the Fn / SecondaryFn flag (default).
    pub hold_is_fn: bool,
    /// Non-Fn hold: keycode + modifiers that must be held.
    pub hold_chord: Option<Chord>,
    /// Hands-free toggle chord.
    pub hands_free: Chord,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            hold_is_fn: true,
            hold_chord: None,
            hands_free: Chord {
                keycode: Some(49), // Space
                ctrl: true,
                shift: true,
                alt: false,
                cmd: false,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    pub keycode: Option<i64>,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub cmd: bool,
}

impl Chord {
    pub fn matches_flags_and_key(&self, flags: ModifierFlags, keycode: i64) -> bool {
        if let Some(k) = self.keycode {
            if keycode != k {
                return false;
            }
        } else {
            return false;
        }
        flags.ctrl == self.ctrl
            && flags.shift == self.shift
            && flags.alt == self.alt
            && flags.cmd == self.cmd
    }

    pub fn modifiers_held(&self, flags: ModifierFlags) -> bool {
        (!self.ctrl || flags.ctrl)
            && (!self.shift || flags.shift)
            && (!self.alt || flags.alt)
            && (!self.cmd || flags.cmd)
            && (self.ctrl || self.shift || self.alt || self.cmd || self.keycode.is_some())
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ModifierFlags {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub cmd: bool,
    pub fn_key: bool,
}

/// Parse strings like `fn`, `ctrl+shift+space`, `cmd+shift+d`.
pub fn parse_shortcut(raw: &str) -> Result<ParsedShortcut, String> {
    let s = raw.trim().to_lowercase();
    if s.is_empty() {
        return Err("empty shortcut".into());
    }
    if s == "fn" || s == "globe" {
        return Ok(ParsedShortcut::FnHold);
    }

    let mut ctrl = false;
    let mut shift = false;
    let mut alt = false;
    let mut cmd = false;
    let mut keycode: Option<i64> = None;

    for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match part {
            "ctrl" | "control" | "control_l" | "control_r" => ctrl = true,
            "shift" | "shift_l" | "shift_r" => shift = true,
            "alt" | "option" | "opt" => alt = true,
            "cmd" | "command" | "super" | "meta" => cmd = true,
            "fn" | "globe" => {
                return Err("combine fn alone as hold_shortcut=fn".into());
            }
            "space" => keycode = Some(49),
            "esc" | "escape" => keycode = Some(53),
            "return" | "enter" => keycode = Some(36),
            "tab" => keycode = Some(48),
            other if other.len() == 1 => {
                let ch = other.chars().next().unwrap();
                keycode = Some(keycode_for_char(ch).ok_or_else(|| {
                    format!("unsupported key in shortcut: {other}")
                })?);
            }
            other => {
                return Err(format!("unsupported shortcut token: {other}"));
            }
        }
    }

    let keycode = keycode.ok_or_else(|| "shortcut missing key".to_string())?;
    Ok(ParsedShortcut::Chord(Chord {
        keycode: Some(keycode),
        ctrl,
        shift,
        alt,
        cmd,
    }))
}

pub enum ParsedShortcut {
    FnHold,
    Chord(Chord),
}

/// US ANSI keycodes for a–z / 0–9 (sufficient for settings chords).
fn keycode_for_char(ch: char) -> Option<i64> {
    match ch {
        'a' => Some(0),
        'b' => Some(11),
        'c' => Some(8),
        'd' => Some(2),
        'e' => Some(14),
        'f' => Some(3),
        'g' => Some(5),
        'h' => Some(4),
        'i' => Some(34),
        'j' => Some(38),
        'k' => Some(40),
        'l' => Some(37),
        'm' => Some(46),
        'n' => Some(45),
        'o' => Some(31),
        'p' => Some(35),
        'q' => Some(12),
        'r' => Some(15),
        's' => Some(1),
        't' => Some(17),
        'u' => Some(32),
        'v' => Some(9),
        'w' => Some(13),
        'x' => Some(7),
        'y' => Some(16),
        'z' => Some(6),
        '0' => Some(29),
        '1' => Some(18),
        '2' => Some(19),
        '3' => Some(20),
        '4' => Some(21),
        '5' => Some(23),
        '6' => Some(22),
        '7' => Some(26),
        '8' => Some(28),
        '9' => Some(25),
        _ => None,
    }
}

pub fn config_from_settings(hold: &str, hands_free: &str) -> HotkeyConfig {
    let mut cfg = HotkeyConfig::default();
    match parse_shortcut(hold) {
        Ok(ParsedShortcut::FnHold) => {
            cfg.hold_is_fn = true;
            cfg.hold_chord = None;
        }
        Ok(ParsedShortcut::Chord(c)) => {
            cfg.hold_is_fn = false;
            cfg.hold_chord = Some(c);
        }
        Err(e) => {
            log::warn!("Invalid hold_shortcut '{hold}' ({e}); defaulting to fn");
            cfg.hold_is_fn = true;
            cfg.hold_chord = None;
        }
    }
    match parse_shortcut(hands_free) {
        Ok(ParsedShortcut::Chord(c)) => cfg.hands_free = c,
        Ok(ParsedShortcut::FnHold) => {
            log::warn!("hands_free_shortcut cannot be fn; keeping default");
        }
        Err(e) => {
            log::warn!("Invalid hands_free_shortcut '{hands_free}' ({e}); keeping default");
        }
    }
    cfg
}

pub struct HotkeyWatcher {
    stop: Arc<AtomicBool>,
    config: Arc<RwLock<HotkeyConfig>>,
    join: Option<thread::JoinHandle<()>>,
}

impl HotkeyWatcher {
    pub fn start(
        tx: mpsc::UnboundedSender<HotkeyEvent>,
        config: HotkeyConfig,
    ) -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        {
            let stop = Arc::new(AtomicBool::new(false));
            let config = Arc::new(RwLock::new(config));
            let stop_t = stop.clone();
            let config_t = config.clone();
            let join = thread::Builder::new()
                .name("dictation-hotkey".into())
                .spawn(move || macos::run_tap(tx, stop_t, config_t))
                .map_err(|e| e.to_string())?;
            Ok(Self {
                stop,
                config,
                join: Some(join),
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (tx, config);
            Err("Dictation hotkeys require macOS".into())
        }
    }

    pub fn update_config(&self, config: HotkeyConfig) {
        if let Ok(mut guard) = self.config.write() {
            *guard = config;
        }
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        #[cfg(target_os = "macos")]
        {
            macos::wake_runloop();
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use core_foundation::base::TCFType;
    use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop, CFRunLoopRef};
    use core_graphics::event::{
        CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions,
        CGEventTapPlacement, CGEventType, EventField, KeyCode,
    };
    use std::sync::Mutex;

    static RUNLOOP: Mutex<Option<usize>> = Mutex::new(None);

    pub fn wake_runloop() {
        if let Ok(guard) = RUNLOOP.lock() {
            if let Some(ptr) = *guard {
                unsafe {
                    let rl = ptr as CFRunLoopRef;
                    core_foundation::runloop::CFRunLoopStop(rl);
                }
            }
        }
    }

    pub fn run_tap(
        tx: mpsc::UnboundedSender<HotkeyEvent>,
        stop: Arc<AtomicBool>,
        config: Arc<RwLock<HotkeyConfig>>,
    ) {
        let holding = Arc::new(AtomicBool::new(false));
        let holding_cb = holding.clone();
        let tx_cb = tx.clone();
        let stop_cb = stop.clone();
        let config_cb = config.clone();

        let tap = match CGEventTap::new(
            CGEventTapLocation::HID,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            vec![
                CGEventType::FlagsChanged,
                CGEventType::KeyDown,
                CGEventType::KeyUp,
            ],
            move |_proxy, etype, event| {
                if stop_cb.load(Ordering::SeqCst) {
                    return Some(event.clone());
                }

                let cfg = config_cb.read().ok().map(|c| c.clone()).unwrap_or_default();
                let flags = flags_from_event(&event);
                let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);

                match etype {
                    CGEventType::FlagsChanged => {
                        if cfg.hold_is_fn {
                            handle_fn_flags(&event, flags, &holding_cb, &tx_cb);
                        }
                    }
                    CGEventType::KeyDown => {
                        if keycode == KeyCode::ESCAPE as i64 {
                            let _ = tx_cb.send(HotkeyEvent::Cancel);
                        }

                        if cfg.hands_free.matches_flags_and_key(flags, keycode) {
                            let _ = tx_cb.send(HotkeyEvent::HandsFreeToggle);
                        }

                        if !cfg.hold_is_fn {
                            if let Some(ref chord) = cfg.hold_chord {
                                if chord.matches_flags_and_key(flags, keycode)
                                    && !holding_cb.load(Ordering::SeqCst)
                                {
                                    holding_cb.store(true, Ordering::SeqCst);
                                    let _ = tx_cb.send(HotkeyEvent::HoldStart);
                                }
                            }
                        }
                    }
                    CGEventType::KeyUp => {
                        if !cfg.hold_is_fn {
                            if let Some(ref chord) = cfg.hold_chord {
                                if chord.keycode == Some(keycode)
                                    && holding_cb.load(Ordering::SeqCst)
                                {
                                    holding_cb.store(false, Ordering::SeqCst);
                                    let _ = tx_cb.send(HotkeyEvent::HoldEnd);
                                }
                            }
                        }
                    }
                    _ => {}
                }
                Some(event.clone())
            },
        ) {
            Ok(t) => t,
            Err(()) => {
                log::error!(
                    "Failed to create CGEvent tap for dictation (Input Monitoring permission?)"
                );
                return;
            }
        };

        unsafe {
            let current = CFRunLoop::get_current();
            if let Ok(mut guard) = RUNLOOP.lock() {
                *guard = Some(current.as_concrete_TypeRef() as usize);
            }
            let loop_source = tap
                .mach_port
                .create_runloop_source(0)
                .expect("dictation event tap runloop source");
            current.add_source(&loop_source, kCFRunLoopCommonModes);
            tap.enable();
            log::info!("Dictation hotkey CGEvent tap enabled");
            CFRunLoop::run_current();
            if let Ok(mut guard) = RUNLOOP.lock() {
                *guard = None;
            }
        }
        log::info!("Dictation hotkey runloop exited");
    }

    fn flags_from_event(event: &CGEvent) -> ModifierFlags {
        let flags = event.get_flags();
        ModifierFlags {
            ctrl: flags.contains(CGEventFlags::CGEventFlagControl),
            shift: flags.contains(CGEventFlags::CGEventFlagShift),
            alt: flags.contains(CGEventFlags::CGEventFlagAlternate),
            cmd: flags.contains(CGEventFlags::CGEventFlagCommand),
            fn_key: flags.contains(CGEventFlags::CGEventFlagSecondaryFn),
        }
    }

    fn handle_fn_flags(
        event: &CGEvent,
        flags: ModifierFlags,
        holding: &AtomicBool,
        tx: &mpsc::UnboundedSender<HotkeyEvent>,
    ) {
        let fn_down = flags.fn_key;
        let was = holding.load(Ordering::SeqCst);
        let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
        let is_fn_key = keycode == KeyCode::FUNCTION as i64;

        if (is_fn_key || fn_down != was) && fn_down && !was {
            holding.store(true, Ordering::SeqCst);
            let _ = tx.send(HotkeyEvent::HoldStart);
        } else if was && !fn_down {
            holding.store(false, Ordering::SeqCst);
            let _ = tx.send(HotkeyEvent::HoldEnd);
        }
    }
}

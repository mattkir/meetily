//! Dictation permission checks: Microphone, Accessibility, Input Monitoring.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictationPermissions {
    pub microphone: bool,
    pub accessibility: bool,
    pub input_monitoring: bool,
}

impl DictationPermissions {
    pub fn all_granted(&self) -> bool {
        self.microphone && self.accessibility && self.input_monitoring
    }
}

pub fn check_permissions() -> DictationPermissions {
    DictationPermissions {
        microphone: check_microphone(),
        accessibility: check_accessibility(false),
        input_monitoring: check_input_monitoring(),
    }
}

fn check_microphone() -> bool {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    match host.default_input_device() {
        Some(device) => device.default_input_config().is_ok(),
        None => false,
    }
}

fn check_accessibility(prompt: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        crate::dictation::insert::is_accessibility_trusted(prompt)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = prompt;
        true
    }
}

/// Input Monitoring: whether a CGEvent tap can be created.
/// Creating a listen-only tap fails (returns null) without the TCC grant.
fn check_input_monitoring() -> bool {
    #[cfg(target_os = "macos")]
    {
        use core_graphics::event::{
            CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
        };
        // Probe with ListenOnly + a cheap event type; success means TCC allows taps.
        match CGEventTap::new(
            CGEventTapLocation::HID,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            vec![CGEventType::FlagsChanged],
            |_p, _t, ev| Some(ev.clone()),
        ) {
            Ok(_tap) => true,
            Err(()) => false,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

pub fn request_accessibility_prompt() -> bool {
    check_accessibility(true)
}

/// Open System Settings panes for the three dictation permissions.
#[cfg(target_os = "macos")]
pub fn open_privacy_pane(pane: &str) -> Result<(), String> {
    let url = match pane {
        "microphone" => "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone",
        "accessibility" => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
        }
        "input_monitoring" | "listen_event" => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent"
        }
        _ => {
            return Err(format!("Unknown privacy pane: {pane}"));
        }
    };
    std::process::Command::new("open")
        .arg(url)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn open_privacy_pane(_pane: &str) -> Result<(), String> {
    Ok(())
}

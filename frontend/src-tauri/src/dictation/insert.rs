//! Insert dictation text into the focused app.
//! Prefer Accessibility AXValue + selected range; fall back to pasteboard + Cmd+V.
//! Capture paste target on Fn-down; never steal focus via HUD; never log pasted content.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

/// Snapshot of the paste destination captured on hold-start.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InsertTarget {
    pub pid: i32,
    pub app_name: String,
}

/// Insert `text` into the previously captured target (or current focus if none).
pub fn insert_text(text: &str) -> Result<InsertMethod> {
    insert_text_to_target(text, None)
}

pub fn insert_text_to_target(text: &str, target: Option<&InsertTarget>) -> Result<InsertMethod> {
    if text.is_empty() {
        return Err(anyhow!("empty insert"));
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(t) = target {
            if !macos::frontmost_matches(t) {
                // Try to restore the captured app without activating Meetily.
                if let Err(e) = macos::activate_pid(t.pid) {
                    log::warn!("Failed to restore paste target pid={}: {e}", t.pid);
                }
                // Brief yield for AX focus settle.
                std::thread::sleep(std::time::Duration::from_millis(40));
                if !macos::frontmost_matches(t) {
                    return Err(anyhow!(
                        "Paste target lost (expected {} pid={})",
                        t.app_name,
                        t.pid
                    ));
                }
            }
        }

        match macos::insert_via_accessibility(text) {
            Ok(()) => {
                if let Some(t) = target {
                    if !macos::frontmost_matches(t) {
                        return Err(anyhow!(
                            "Frontmost app changed after Accessibility insert"
                        ));
                    }
                }
                log::info!(
                    "Dictation insert via Accessibility (chars={})",
                    text.chars().count()
                );
                return Ok(InsertMethod::Accessibility);
            }
            Err(e) => {
                log::warn!(
                    "Accessibility insert failed ({e}); trying clipboard Cmd+V fallback"
                );
            }
        }

        macos::insert_via_clipboard_paste(text)?;
        if let Some(t) = target {
            if !macos::frontmost_matches(t) {
                return Err(anyhow!(
                    "Frontmost app changed after clipboard paste"
                ));
            }
        }
        log::info!(
            "Dictation insert via clipboard paste (chars={})",
            text.chars().count()
        );
        return Ok(InsertMethod::ClipboardPaste);
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (text, target);
        Err(anyhow!("Dictation insert is only implemented on macOS"))
    }
}

/// Capture the frontmost app (+ focused element availability) on Fn-down.
pub fn capture_insert_target() -> Result<InsertTarget> {
    #[cfg(target_os = "macos")]
    {
        macos::capture_frontmost()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(anyhow!("Dictation insert is only implemented on macOS"))
    }
}

pub fn verify_insert_target(target: &InsertTarget) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::frontmost_matches(target)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = target;
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertMethod {
    Accessibility,
    ClipboardPaste,
}

/// Copy text to the system clipboard without pasting. Never logs content.
pub fn copy_text(text: &str) -> Result<()> {
    if text.is_empty() {
        return Err(anyhow!("empty copy"));
    }

    #[cfg(target_os = "macos")]
    {
        macos::write_pasteboard(text)?;
        log::info!(
            "Dictation copied to clipboard (chars={})",
            text.chars().count()
        );
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = text;
        Err(anyhow!("Dictation clipboard copy is only implemented on macOS"))
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;
    use core_graphics::event::{
        CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode, KeyCode,
    };
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use objc::{class, msg_send, runtime::Object, sel, sel_impl};
    use std::ffi::c_void;
    use std::ptr;
    use std::thread;
    use std::time::Duration;

    #[link(name = "AppKit", kind = "framework")]
    extern "C" {}

    type AXUIElementRef = *mut c_void;
    type AXError = i32;
    type AXValueRef = *mut c_void;
    const K_AX_ERROR_SUCCESS: AXError = 0;
    // kAXValueTypeCFRange = 4
    const K_AX_VALUE_CF_RANGE: u32 = 4;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CFRange {
        location: isize,
        length: isize,
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
        fn AXUIElementCreateSystemWide() -> AXUIElementRef;
        fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: *const c_void,
            value: *mut *const c_void,
        ) -> AXError;
        fn AXUIElementSetAttributeValue(
            element: AXUIElementRef,
            attribute: *const c_void,
            value: *const c_void,
        ) -> AXError;
        fn AXValueCreate(value_type: u32, value_ptr: *const c_void) -> AXValueRef;
        fn AXValueGetValue(value: AXValueRef, value_type: u32, value_ptr: *mut c_void) -> bool;
        fn CFRelease(cf: *const c_void);
    }

    pub fn is_accessibility_trusted(prompt: bool) -> bool {
        unsafe {
            if !prompt {
                return AXIsProcessTrusted();
            }
            let key = CFString::new("AXTrustedCheckOptionPrompt");
            let value = CFBoolean::true_value();
            let opts = CFDictionary::from_CFType_pairs(&[(
                key.as_CFType(),
                value.as_CFType(),
            )]);
            AXIsProcessTrustedWithOptions(opts.as_concrete_TypeRef() as *const c_void)
        }
    }

    pub fn capture_frontmost() -> Result<InsertTarget> {
        unsafe {
            let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
            if workspace.is_null() {
                return Err(anyhow!("NSWorkspace unavailable"));
            }
            let app: *mut Object = msg_send![workspace, frontmostApplication];
            if app.is_null() {
                return Err(anyhow!("No frontmost application"));
            }
            let pid: i32 = msg_send![app, processIdentifier];
            let name_obj: *mut Object = msg_send![app, localizedName];
            let app_name = if name_obj.is_null() {
                format!("pid-{pid}")
            } else {
                nsstring_to_rust(name_obj).unwrap_or_else(|| format!("pid-{pid}"))
            };

            // Probe that a focused element exists (best-effort).
            let _ = focused_element_for_pid(pid);

            Ok(InsertTarget { pid, app_name })
        }
    }

    pub fn frontmost_matches(target: &InsertTarget) -> bool {
        unsafe {
            let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
            if workspace.is_null() {
                return false;
            }
            let app: *mut Object = msg_send![workspace, frontmostApplication];
            if app.is_null() {
                return false;
            }
            let pid: i32 = msg_send![app, processIdentifier];
            pid == target.pid
        }
    }

    pub fn activate_pid(pid: i32) -> Result<()> {
        unsafe {
            let app: *mut Object = msg_send![
                class!(NSRunningApplication),
                runningApplicationWithProcessIdentifier: pid
            ];
            if app.is_null() {
                return Err(anyhow!("NSRunningApplication for pid {pid} not found"));
            }
            // NSApplicationActivateIgnoringOtherApps = 1 << 1 = 2
            let ok: bool = msg_send![app, activateWithOptions: 2u64];
            if !ok {
                return Err(anyhow!("activateWithOptions failed for pid {pid}"));
            }
            Ok(())
        }
    }

    unsafe fn nsstring_to_rust(ns: *mut Object) -> Option<String> {
        if ns.is_null() {
            return None;
        }
        let utf8: *const i8 = msg_send![ns, UTF8String];
        if utf8.is_null() {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr(utf8)
                .to_string_lossy()
                .into_owned(),
        )
    }

    unsafe fn focused_element_for_pid(pid: i32) -> Result<*const c_void> {
        let app_el = AXUIElementCreateApplication(pid);
        if app_el.is_null() {
            return Err(anyhow!("AXUIElementCreateApplication failed"));
        }
        let focused_attr = CFString::new("AXFocusedUIElement");
        let mut focused: *const c_void = ptr::null();
        let err = AXUIElementCopyAttributeValue(
            app_el,
            focused_attr.as_concrete_TypeRef() as *const c_void,
            &mut focused,
        );
        CFRelease(app_el);
        if err != K_AX_ERROR_SUCCESS || focused.is_null() {
            // Fall back to system-wide focused element.
            return focused_element_system_wide();
        }
        Ok(focused)
    }

    unsafe fn focused_element_system_wide() -> Result<*const c_void> {
        let system = AXUIElementCreateSystemWide();
        if system.is_null() {
            return Err(anyhow!("AXUIElementCreateSystemWide failed"));
        }
        let focused_attr = CFString::new("AXFocusedUIElement");
        let mut focused: *const c_void = ptr::null();
        let err = AXUIElementCopyAttributeValue(
            system,
            focused_attr.as_concrete_TypeRef() as *const c_void,
            &mut focused,
        );
        CFRelease(system);
        if err != K_AX_ERROR_SUCCESS || focused.is_null() {
            return Err(anyhow!("No focused UI element (AX error {err})"));
        }
        Ok(focused)
    }

    pub fn insert_via_accessibility(text: &str) -> Result<()> {
        if !is_accessibility_trusted(false) {
            return Err(anyhow!("Accessibility permission not granted"));
        }

        unsafe {
            let focused = focused_element_system_wide()?;

            // Prefer AXValue + selected range splice when available.
            if insert_via_ax_value(focused as AXUIElementRef, text).is_ok() {
                CFRelease(focused);
                return Ok(());
            }

            let cf_text = CFString::new(text);
            let selected_attr = CFString::new("AXSelectedText");
            let set_err = AXUIElementSetAttributeValue(
                focused as AXUIElementRef,
                selected_attr.as_concrete_TypeRef() as *const c_void,
                cf_text.as_concrete_TypeRef() as *const c_void,
            );
            CFRelease(focused);

            if set_err != K_AX_ERROR_SUCCESS {
                return Err(anyhow!("AXSelectedText set failed ({set_err})"));
            }
            Ok(())
        }
    }

    unsafe fn insert_via_ax_value(element: AXUIElementRef, text: &str) -> Result<()> {
        let value_attr = CFString::new("AXValue");
        let mut value_ptr: *const c_void = ptr::null();
        let get_err = AXUIElementCopyAttributeValue(
            element,
            value_attr.as_concrete_TypeRef() as *const c_void,
            &mut value_ptr,
        );
        if get_err != K_AX_ERROR_SUCCESS || value_ptr.is_null() {
            return Err(anyhow!("AXValue get failed ({get_err})"));
        }

        let current = cfstring_to_rust(value_ptr).unwrap_or_default();
        CFRelease(value_ptr);

        let mut range = CFRange {
            location: current.chars().count() as isize,
            length: 0,
        };

        let range_attr = CFString::new("AXSelectedTextRange");
        let mut range_ptr: *const c_void = ptr::null();
        if AXUIElementCopyAttributeValue(
            element,
            range_attr.as_concrete_TypeRef() as *const c_void,
            &mut range_ptr,
        ) == K_AX_ERROR_SUCCESS
            && !range_ptr.is_null()
        {
            let mut got = CFRange {
                location: 0,
                length: 0,
            };
            if AXValueGetValue(
                range_ptr as AXValueRef,
                K_AX_VALUE_CF_RANGE,
                &mut got as *mut CFRange as *mut c_void,
            ) {
                range = got;
            }
            CFRelease(range_ptr);
        }

        let chars: Vec<char> = current.chars().collect();
        let start = (range.location as usize).min(chars.len());
        let end = (start + range.length.max(0) as usize).min(chars.len());
        let mut new_chars = Vec::with_capacity(chars.len() + text.chars().count());
        new_chars.extend_from_slice(&chars[..start]);
        new_chars.extend(text.chars());
        new_chars.extend_from_slice(&chars[end..]);
        let new_value: String = new_chars.into_iter().collect();

        let cf_new = CFString::new(&new_value);
        let set_err = AXUIElementSetAttributeValue(
            element,
            value_attr.as_concrete_TypeRef() as *const c_void,
            cf_new.as_concrete_TypeRef() as *const c_void,
        );
        if set_err != K_AX_ERROR_SUCCESS {
            return Err(anyhow!("AXValue set failed ({set_err})"));
        }

        // Place caret after inserted text.
        let caret = CFRange {
            location: (start + text.chars().count()) as isize,
            length: 0,
        };
        let ax_range = AXValueCreate(
            K_AX_VALUE_CF_RANGE,
            &caret as *const CFRange as *const c_void,
        );
        if !ax_range.is_null() {
            let _ = AXUIElementSetAttributeValue(
                element,
                range_attr.as_concrete_TypeRef() as *const c_void,
                ax_range,
            );
            CFRelease(ax_range);
        }
        Ok(())
    }

    unsafe fn cfstring_to_rust(cf: *const c_void) -> Option<String> {
        let ns: *mut Object = cf as *mut Object;
        nsstring_to_rust(ns)
    }

    pub fn insert_via_clipboard_paste(text: &str) -> Result<()> {
        let previous = read_pasteboard();
        write_pasteboard(text)?;

        // Synthetic Cmd+V
        post_key(KeyCode::CHARACTER_V, true, true)?;
        post_key(KeyCode::CHARACTER_V, false, true)?;

        // Restore previous pasteboard after paste settles (tighter than 350ms).
        if let Some(prev) = previous {
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(120));
                let _ = write_pasteboard(&prev);
            });
        }
        Ok(())
    }

    trait KeyCodeExt {
        const CHARACTER_V: CGKeyCode;
    }
    impl KeyCodeExt for KeyCode {
        const CHARACTER_V: CGKeyCode = 0x09;
    }

    fn post_key(keycode: CGKeyCode, key_down: bool, with_cmd: bool) -> Result<()> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| anyhow!("CGEventSource failed"))?;
        let event = CGEvent::new_keyboard_event(source, keycode, key_down)
            .map_err(|_| anyhow!("CGEvent keyboard failed"))?;
        if with_cmd {
            event.set_flags(CGEventFlags::CGEventFlagCommand);
        }
        event.post(CGEventTapLocation::HID);
        Ok(())
    }

    pub fn write_pasteboard(text: &str) -> Result<()> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new("pbcopy")
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| anyhow!("pbcopy: {e}"))?;
        {
            let stdin = child
                .stdin
                .as_mut()
                .ok_or_else(|| anyhow!("pbcopy stdin"))?;
            stdin
                .write_all(text.as_bytes())
                .map_err(|e| anyhow!("pbcopy write: {e}"))?;
        }
        let status = child.wait().map_err(|e| anyhow!("pbcopy wait: {e}"))?;
        if !status.success() {
            return Err(anyhow!("pbcopy failed"));
        }
        Ok(())
    }

    fn read_pasteboard() -> Option<String> {
        let output = std::process::Command::new("pbpaste").output().ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()
    }
}

#[cfg(target_os = "macos")]
pub use macos::is_accessibility_trusted;

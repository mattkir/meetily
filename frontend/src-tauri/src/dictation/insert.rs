//! Insert dictation text into the focused app.
//! Prefer a verified Accessibility write on native text fields; fall back to
//! pasteboard + HID Cmd+V for web/Electron/Excel editors and when AX cannot be verified.
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

/// Bundle IDs where Accessibility AXValue writes are unreliable (false success).
pub const UNRELIABLE_AX_BUNDLE_IDS: &[&str] = &["com.microsoft.Excel"];

/// Pure routing decision: when true, skip Accessibility and use clipboard paste.
/// Used by macOS insert and unit-tested without live AX calls.
pub fn prefer_clipboard_paste(
    role: Option<&str>,
    has_web_area_ancestor: bool,
    is_electron_app: bool,
    bundle_id: Option<&str>,
) -> bool {
    if is_electron_app || has_web_area_ancestor {
        return true;
    }
    if bundle_id.is_some_and(is_unreliable_ax_bundle_id) {
        return true;
    }
    !is_native_text_role(role)
}

pub fn is_unreliable_ax_bundle_id(bundle_id: &str) -> bool {
    UNRELIABLE_AX_BUNDLE_IDS
        .iter()
        .any(|id| bundle_id.eq_ignore_ascii_case(id))
}

pub fn is_native_text_role(role: Option<&str>) -> bool {
    matches!(
        role,
        Some("AXTextField") | Some("AXTextArea") | Some("AXComboBox")
    )
}

/// True when `bundle_path` (or a parent directory) contains Electron Framework.
/// Covers both the main app and helpers like `Cursor Helper (Renderer).app`.
pub fn path_looks_like_electron(bundle_path: &std::path::Path) -> bool {
    let direct = bundle_path
        .join("Contents")
        .join("Frameworks")
        .join("Electron Framework.framework");
    if direct.exists() {
        return true;
    }
    // Helper apps live under Contents/Frameworks/ beside Electron Framework.
    for ancestor in bundle_path.ancestors().take(6) {
        let sibling = ancestor.join("Electron Framework.framework");
        if sibling.exists() {
            return true;
        }
        let nested = ancestor
            .join("Contents")
            .join("Frameworks")
            .join("Electron Framework.framework");
        if nested.exists() {
            return true;
        }
    }
    false
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

        match macos::insert_via_accessibility(text, target) {
            Ok(role) => {
                if let Some(t) = target {
                    if !macos::frontmost_matches(t) {
                        return Err(anyhow!(
                            "Frontmost app changed after Accessibility insert"
                        ));
                    }
                    log::info!(
                        "Dictation insert via Accessibility app={} pid={} role={} chars={}",
                        t.app_name,
                        t.pid,
                        role.as_deref().unwrap_or("unknown"),
                        text.chars().count()
                    );
                } else {
                    log::info!(
                        "Dictation insert via Accessibility role={} chars={}",
                        role.as_deref().unwrap_or("unknown"),
                        text.chars().count()
                    );
                }
                return Ok(InsertMethod::Accessibility);
            }
            Err(e) => {
                log::warn!(
                    "Accessibility insert skipped/failed ({e}); trying clipboard Cmd+V fallback"
                );
            }
        }

        // HID paste while the captured target stays frontmost (no CGEventPostToPid).
        macos::insert_via_clipboard_paste(text)?;
        if let Some(t) = target {
            if !macos::frontmost_matches(t) {
                return Err(anyhow!(
                    "Frontmost app changed after clipboard paste"
                ));
            }
            log::info!(
                "Dictation insert via clipboard paste app={} pid={} chars={}",
                t.app_name,
                t.pid,
                text.chars().count()
            );
        } else {
            log::info!(
                "Dictation insert via clipboard paste chars={}",
                text.chars().count()
            );
        }
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

/// Splice `insert` into `current` at a UTF-16 CFRange (Accessibility index units).
/// Returns the new string and the UTF-16 caret location after the insert.
pub fn splice_utf16(current: &str, insert: &str, location: isize, length: isize) -> (String, isize) {
    let utf16: Vec<u16> = current.encode_utf16().collect();
    let insert_utf16: Vec<u16> = insert.encode_utf16().collect();
    let start = (location.max(0) as usize).min(utf16.len());
    let end = (start + length.max(0) as usize).min(utf16.len());
    let mut out = Vec::with_capacity(utf16.len() - (end - start) + insert_utf16.len());
    out.extend_from_slice(&utf16[..start]);
    out.extend_from_slice(&insert_utf16);
    out.extend_from_slice(&utf16[end..]);
    let caret = (start + insert_utf16.len()) as isize;
    (String::from_utf16_lossy(&out), caret)
}

/// True when `inserted` appears at the UTF-16 range that was written.
pub fn readback_contains_insert(
    after: &str,
    inserted: &str,
    utf16_start: isize,
) -> bool {
    if inserted.is_empty() {
        return true;
    }
    let after_utf16: Vec<u16> = after.encode_utf16().collect();
    let insert_utf16: Vec<u16> = inserted.encode_utf16().collect();
    let start = utf16_start.max(0) as usize;
    if start + insert_utf16.len() > after_utf16.len() {
        return false;
    }
    &after_utf16[start..start + insert_utf16.len()] == insert_utf16.as_slice()
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
    use std::path::PathBuf;
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
    const KEYCODE_LEFT_COMMAND: CGKeyCode = 0x37;
    const PASTEBOARD_RESTORE_MS: u64 = 300;
    const KEY_EVENT_GAP_MS: u64 = 8;
    const MAX_PARENT_WALK: usize = 32;

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
        fn AXUIElementIsAttributeSettable(
            element: AXUIElementRef,
            attribute: *const c_void,
            settable: *mut u8,
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

    pub fn frontmost_pid() -> Option<i32> {
        unsafe {
            let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
            if workspace.is_null() {
                return None;
            }
            let app: *mut Object = msg_send![workspace, frontmostApplication];
            if app.is_null() {
                return None;
            }
            let pid: i32 = msg_send![app, processIdentifier];
            Some(pid)
        }
    }

    pub fn frontmost_matches(target: &InsertTarget) -> bool {
        frontmost_pid() == Some(target.pid)
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

    unsafe fn copy_ax_string_attr(element: AXUIElementRef, attr: &str) -> Option<String> {
        let attr_cf = CFString::new(attr);
        let mut value: *const c_void = ptr::null();
        let err = AXUIElementCopyAttributeValue(
            element,
            attr_cf.as_concrete_TypeRef() as *const c_void,
            &mut value,
        );
        if err != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }
        let s = cfstring_to_rust(value);
        CFRelease(value);
        s
    }

    unsafe fn has_web_area_ancestor(element: AXUIElementRef) -> bool {
        let parent_attr = CFString::new("AXParent");
        let mut current: *const c_void = element as *const c_void;
        // Do not release the original focused element here.
        let mut owned: Option<*const c_void> = None;
        for _ in 0..MAX_PARENT_WALK {
            let role = copy_ax_string_attr(current as AXUIElementRef, "AXRole");
            if role.as_deref() == Some("AXWebArea") {
                if let Some(p) = owned {
                    CFRelease(p);
                }
                return true;
            }
            let mut parent: *const c_void = ptr::null();
            let err = AXUIElementCopyAttributeValue(
                current as AXUIElementRef,
                parent_attr.as_concrete_TypeRef() as *const c_void,
                &mut parent,
            );
            if let Some(p) = owned {
                CFRelease(p);
            }
            if err != K_AX_ERROR_SUCCESS || parent.is_null() {
                return false;
            }
            current = parent;
            owned = Some(parent);
        }
        if let Some(p) = owned {
            CFRelease(p);
        }
        false
    }

    fn running_app_for_pid(pid: i32) -> Option<*mut Object> {
        unsafe {
            let app: *mut Object = msg_send![
                class!(NSRunningApplication),
                runningApplicationWithProcessIdentifier: pid
            ];
            if app.is_null() {
                None
            } else {
                Some(app)
            }
        }
    }

    fn bundle_path_for_pid(pid: i32) -> Option<PathBuf> {
        unsafe {
            let app = running_app_for_pid(pid)?;
            let url: *mut Object = msg_send![app, bundleURL];
            if url.is_null() {
                return None;
            }
            let path_ns: *mut Object = msg_send![url, path];
            let path = nsstring_to_rust(path_ns)?;
            Some(PathBuf::from(path))
        }
    }

    fn bundle_id_for_pid(pid: i32) -> Option<String> {
        unsafe {
            let app = running_app_for_pid(pid)?;
            let bid: *mut Object = msg_send![app, bundleIdentifier];
            nsstring_to_rust(bid)
        }
    }

    pub fn is_electron_app(pid: i32) -> bool {
        bundle_path_for_pid(pid)
            .map(|p| path_looks_like_electron(&p))
            .unwrap_or(false)
    }

    /// Verified Accessibility insert. Returns the focused role on success.
    pub fn insert_via_accessibility(
        text: &str,
        target: Option<&InsertTarget>,
    ) -> Result<Option<String>> {
        if !is_accessibility_trusted(false) {
            return Err(anyhow!("Accessibility permission not granted"));
        }

        unsafe {
            let focused = if let Some(t) = target {
                focused_element_for_pid(t.pid)?
            } else {
                focused_element_system_wide()?
            };

            let role = copy_ax_string_attr(focused as AXUIElementRef, "AXRole");
            let web_area = has_web_area_ancestor(focused as AXUIElementRef);
            let pid_for_checks = target.map(|t| t.pid).or_else(frontmost_pid);
            let electron = pid_for_checks
                .map(is_electron_app)
                .unwrap_or(false);
            let bundle_id = pid_for_checks.and_then(bundle_id_for_pid);

            if prefer_clipboard_paste(
                role.as_deref(),
                web_area,
                electron,
                bundle_id.as_deref(),
            ) {
                let reason = if electron {
                    "electron app"
                } else if bundle_id
                    .as_deref()
                    .is_some_and(is_unreliable_ax_bundle_id)
                {
                    "unreliable AX bundle"
                } else if web_area {
                    "AXWebArea ancestor"
                } else {
                    "non-native text role"
                };
                log::info!(
                    "Dictation skipping Accessibility ({reason}) role={} bundle={:?} app={}",
                    role.as_deref().unwrap_or("unknown"),
                    bundle_id,
                    target.map(|t| t.app_name.as_str()).unwrap_or("current")
                );
                CFRelease(focused);
                return Err(anyhow!("skip Accessibility: {reason}"));
            }

            match insert_via_ax_value(focused as AXUIElementRef, text) {
                Ok(()) => {
                    CFRelease(focused);
                    Ok(role)
                }
                Err(e) => {
                    CFRelease(focused);
                    Err(e)
                }
            }
        }
    }

    unsafe fn attribute_is_settable(element: AXUIElementRef, attr: &str) -> bool {
        let attr_cf = CFString::new(attr);
        let mut settable: u8 = 0;
        let err = AXUIElementIsAttributeSettable(
            element,
            attr_cf.as_concrete_TypeRef() as *const c_void,
            &mut settable,
        );
        err == K_AX_ERROR_SUCCESS && settable != 0
    }

    unsafe fn insert_via_ax_value(element: AXUIElementRef, text: &str) -> Result<()> {
        if !attribute_is_settable(element, "AXValue") {
            return Err(anyhow!("AXValue not settable"));
        }

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
            location: current.encode_utf16().count() as isize,
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

        let insert_start = range.location.max(0);
        let (new_value, caret_loc) = splice_utf16(current.as_str(), text, range.location, range.length);

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
            location: caret_loc,
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

        // Read back and require the inserted text at the write range.
        let mut after_ptr: *const c_void = ptr::null();
        let after_err = AXUIElementCopyAttributeValue(
            element,
            value_attr.as_concrete_TypeRef() as *const c_void,
            &mut after_ptr,
        );
        if after_err != K_AX_ERROR_SUCCESS || after_ptr.is_null() {
            return Err(anyhow!("AXValue readback failed ({after_err})"));
        }
        let after = cfstring_to_rust(after_ptr).unwrap_or_default();
        CFRelease(after_ptr);
        if !readback_contains_insert(&after, text, insert_start) {
            return Err(anyhow!("AXValue readback did not contain inserted text"));
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

        post_cmd_v_hid()?;

        // Restore previous pasteboard after paste settles.
        if let Some(prev) = previous {
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(PASTEBOARD_RESTORE_MS));
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

    /// Full Command+V chord via HID (not CGEventPostToPid). One event source;
    /// Command flag on Command-down and both V events; short gaps between posts.
    fn post_cmd_v_hid() -> Result<()> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| anyhow!("CGEventSource failed"))?;

        post_key_hid(&source, KEYCODE_LEFT_COMMAND, true, true)?;
        thread::sleep(Duration::from_millis(KEY_EVENT_GAP_MS));
        post_key_hid(&source, KeyCode::CHARACTER_V, true, true)?;
        thread::sleep(Duration::from_millis(KEY_EVENT_GAP_MS));
        post_key_hid(&source, KeyCode::CHARACTER_V, false, true)?;
        thread::sleep(Duration::from_millis(KEY_EVENT_GAP_MS));
        post_key_hid(&source, KEYCODE_LEFT_COMMAND, false, false)?;
        Ok(())
    }

    fn post_key_hid(
        source: &CGEventSource,
        keycode: CGKeyCode,
        key_down: bool,
        with_cmd: bool,
    ) -> Result<()> {
        let event = CGEvent::new_keyboard_event(source.clone(), keycode, key_down)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn prefer_paste_for_electron() {
        assert!(prefer_clipboard_paste(
            Some("AXTextArea"),
            false,
            true,
            None
        ));
    }

    #[test]
    fn prefer_paste_for_web_area_ancestor() {
        assert!(prefer_clipboard_paste(
            Some("AXTextArea"),
            true,
            false,
            None
        ));
    }

    #[test]
    fn prefer_paste_for_excel_bundle() {
        assert!(prefer_clipboard_paste(
            Some("AXTextArea"),
            false,
            false,
            Some("com.microsoft.Excel")
        ));
        assert!(is_unreliable_ax_bundle_id("com.microsoft.Excel"));
    }

    #[test]
    fn prefer_ax_for_native_text_roles() {
        assert!(!prefer_clipboard_paste(
            Some("AXTextField"),
            false,
            false,
            None
        ));
        assert!(!prefer_clipboard_paste(
            Some("AXTextArea"),
            false,
            false,
            None
        ));
        assert!(!prefer_clipboard_paste(
            Some("AXComboBox"),
            false,
            false,
            None
        ));
    }

    #[test]
    fn prefer_paste_for_unknown_role() {
        assert!(prefer_clipboard_paste(Some("AXButton"), false, false, None));
        assert!(prefer_clipboard_paste(None, false, false, None));
    }

    #[test]
    fn electron_helper_path_detected() {
        // Simulate Cursor Helper sitting beside Electron Framework.
        let tmp = std::env::temp_dir().join(format!(
            "meetily-electron-test-{}",
            std::process::id()
        ));
        let frameworks = tmp.join("Contents").join("Frameworks");
        let helper = frameworks.join("Cursor Helper (Renderer).app");
        let electron = frameworks.join("Electron Framework.framework");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&helper).unwrap();
        std::fs::create_dir_all(&electron).unwrap();
        assert!(path_looks_like_electron(&helper));
        assert!(path_looks_like_electron(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);

        // Non-electron path.
        let other = PathBuf::from("/Applications/TextEdit.app");
        // May or may not exist; without Electron Framework it must be false.
        if other.exists() {
            assert!(!path_looks_like_electron(&other));
        }
    }

    #[test]
    fn splice_utf16_ascii() {
        let (out, caret) = splice_utf16("hello", "XX", 2, 0);
        assert_eq!(out, "heXXllo");
        assert_eq!(caret, 4);
    }

    #[test]
    fn splice_utf16_replaces_selection() {
        // location 2, length 3 replaces "cde" → "ab" + "Z" + "f"
        let (out, caret) = splice_utf16("abcdef", "Z", 2, 3);
        assert_eq!(out, "abZf");
        assert_eq!(caret, 3);
    }

    #[test]
    fn splice_utf16_emoji_indexes() {
        // "a👍b" — thumbs-up is two UTF-16 code units.
        let current = "a👍b";
        let (out, caret) = splice_utf16(current, "X", 3, 0); // after emoji
        assert_eq!(out, "a👍Xb");
        assert_eq!(caret, 4);
    }

    #[test]
    fn readback_checks_utf16_range() {
        assert!(readback_contains_insert("heXXllo", "XX", 2));
        assert!(!readback_contains_insert("hello", "XX", 2));
        assert!(readback_contains_insert("a👍Xb", "X", 3));
    }
}

//! Tiny always-on-top HUD webview for Listening / Processing / Error.
//! Never activates or steals key focus from the paste target.

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};

pub const HUD_LABEL: &str = "dictation-hud";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HudPayload {
    pub phase: String,
    pub message: String,
    pub partial: Option<String>,
    /// Optional latency summary for self-test / debug (no transcript text).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<LatencyHudMetrics>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencyHudMetrics {
    pub grace_ms: u64,
    pub resample_ms: u64,
    pub ipc_ms: u64,
    pub asr_ms: u64,
    pub polish_ms: u64,
    pub insert_ms: u64,
    pub e2e_ms: u64,
}

pub fn ensure_hud_window<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    if app.get_webview_window(HUD_LABEL).is_some() {
        return Ok(());
    }

    let url = WebviewUrl::App("/dictation-hud".into());
    let builder = WebviewWindowBuilder::new(app, HUD_LABEL, url)
        .title("Dictation")
        .inner_size(320.0, 88.0)
        .resizable(false)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .focused(false)
        .focusable(false)
        .transparent(true);

    builder
        .build()
        .map_err(|e| format!("Failed to create dictation HUD: {e}"))?;

    log::info!("Dictation HUD window created (non-activating)");
    Ok(())
}

pub fn show_hud<R: Runtime>(app: &AppHandle<R>, payload: HudPayload) {
    if let Err(e) = ensure_hud_window(app) {
        log::warn!("{e}");
        return;
    }

    if let Some(win) = app.get_webview_window(HUD_LABEL) {
        // Center near top of primary display — show without focusing.
        if let Ok(Some(monitor)) = app.primary_monitor() {
            let size = monitor.size();
            let scale = monitor.scale_factor();
            let width = 320.0;
            let x = (size.width as f64 / scale - width) / 2.0;
            let y = 48.0;
            let _ = win.set_position(tauri::Position::Logical(tauri::LogicalPosition { x, y }));
        }
        // Never call set_focus — HUD must not steal key focus from paste target.
        let _ = win.show();
        let _ = app.emit("dictation-hud", &payload);
    }
}

pub fn hide_hud<R: Runtime>(app: &AppHandle<R>) {
    if let Some(win) = app.get_webview_window(HUD_LABEL) {
        let _ = win.hide();
    }
    let _ = app.emit(
        "dictation-hud",
        &HudPayload {
            phase: "idle".into(),
            message: String::new(),
            partial: None,
            metrics: None,
        },
    );
}

pub fn emit_status<R: Runtime>(app: &AppHandle<R>, phase: &str, message: &str) {
    let _ = app.emit(
        "dictation-status",
        &HudPayload {
            phase: phase.into(),
            message: message.into(),
            partial: None,
            metrics: None,
        },
    );
}

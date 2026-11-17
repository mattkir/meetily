//! Tauri commands for the dictation agent.

use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Runtime, State};

use super::asr;
use super::autostart;
use super::history::{self, HistoryEntry};
use super::hud;
use super::insert;
use super::permissions::{self, DictationPermissions};
use super::session::{DictationAgent, DictationStatus, LatencyMetrics, SelfTestResult};
use super::settings::{self, DictationEngine, DictationSettings};
use super::DAILY_READY_E2E_MS;

pub type DictationState = Arc<DictationAgent>;

#[tauri::command]
pub async fn dictation_get_status(
    state: State<'_, DictationState>,
) -> Result<DictationStatus, String> {
    Ok(state.status().await)
}

#[tauri::command]
pub async fn dictation_get_settings(
    state: State<'_, DictationState>,
) -> Result<DictationSettings, String> {
    Ok(state.settings_snapshot().await)
}

#[tauri::command]
pub async fn dictation_set_settings<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, DictationState>,
    settings: DictationSettings,
) -> Result<(), String> {
    settings::save_settings(&app, &settings)?;
    let engine = settings.engine;
    state.update_settings(settings).await;
    // Warm the newly selected engine in the background.
    let warm_state: DictationState = Arc::clone(state.inner());
    tauri::async_runtime::spawn(async move {
        if let Err(e) = warm_state.warm_asr(engine).await {
            log::warn!("Dictation ASR re-warm after settings: {e}");
        }
    });
    crate::tray::refresh_dictation_status(&app).await;
    Ok(())
}

#[tauri::command]
pub async fn dictation_start_agent<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, DictationState>,
) -> Result<(), String> {
    state.start(app).await
}

#[tauri::command]
pub async fn dictation_stop_agent(state: State<'_, DictationState>) -> Result<(), String> {
    state.stop().await;
    Ok(())
}

#[tauri::command]
pub async fn dictation_get_permissions() -> Result<DictationPermissions, String> {
    Ok(
        tauri::async_runtime::spawn_blocking(permissions::check_permissions)
            .await
            .map_err(|e| e.to_string())?,
    )
}

#[tauri::command]
pub async fn dictation_request_accessibility() -> Result<bool, String> {
    Ok(
        tauri::async_runtime::spawn_blocking(|| permissions::request_accessibility_prompt())
            .await
            .map_err(|e| e.to_string())?,
    )
}

#[tauri::command]
pub async fn dictation_open_permission_settings(pane: String) -> Result<(), String> {
    permissions::open_privacy_pane(&pane)
}

#[tauri::command]
pub async fn dictation_trigger_microphone_permission() -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(|| {
        crate::audio::devices::discovery::trigger_audio_permission().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn dictation_get_history<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Vec<HistoryEntry>, String> {
    Ok(history::load_history(&app))
}

#[tauri::command]
pub async fn dictation_clear_history<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    history::clear_history(&app)
}

#[tauri::command]
pub async fn dictation_remove_history_entry<R: Runtime>(
    app: AppHandle<R>,
    id: String,
) -> Result<bool, String> {
    history::remove_entry(&app, &id)
}

/// Re-insert a prior local history entry at the focused caret (retry paste).
#[tauri::command]
pub async fn dictation_retry_history_insert<R: Runtime>(
    app: AppHandle<R>,
    id: String,
) -> Result<String, String> {
    let entry =
        history::get_entry(&app, &id).ok_or_else(|| "History entry not found".to_string())?;
    let text = entry.text.clone();
    let method = tauri::async_runtime::spawn_blocking(move || insert::insert_text(&text))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let method_label = match method {
        insert::InsertMethod::Accessibility => "accessibility",
        insert::InsertMethod::ClipboardPaste => "clipboard",
    };
    log::info!(
        "Dictation history retry id={} method={} chars={}",
        id,
        method_label,
        entry.text.chars().count()
    );
    Ok(method_label.to_string())
}

/// Copy a history entry to the clipboard (no paste, no cloud).
#[tauri::command]
pub async fn dictation_copy_history_entry<R: Runtime>(
    app: AppHandle<R>,
    id: String,
) -> Result<(), String> {
    let entry =
        history::get_entry(&app, &id).ok_or_else(|| "History entry not found".to_string())?;
    let text = entry.text.clone();
    tauri::async_runtime::spawn_blocking(move || insert::copy_text(&text))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    log::info!(
        "Dictation history copy id={} chars={}",
        id,
        entry.text.chars().count()
    );
    Ok(())
}

/// Exercise AX / clipboard insert without ASR (verification helper).
#[tauri::command]
pub async fn dictation_test_insert(text: Option<String>) -> Result<String, String> {
    let payload = text.unwrap_or_else(|| "Meetily dictation paste OK".to_string());
    let method = tauri::async_runtime::spawn_blocking(move || insert::insert_text(&payload))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    Ok(format!("{method:?}"))
}

/// Warm short self-test with latency buckets; gates daily_ready on e2e &lt; 300ms.
#[tauri::command]
pub async fn dictation_run_self_test<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, DictationState>,
    include_insert: Option<bool>,
) -> Result<SelfTestResult, String> {
    state
        .run_self_test(&app, include_insert.unwrap_or(false))
        .await
}

#[tauri::command]
pub async fn dictation_asr_protocol_info() -> Result<String, String> {
    Ok(asr::protocol_schema_doc().to_string())
}

#[tauri::command]
pub async fn dictation_helper_available(state: State<'_, DictationState>) -> Result<bool, String> {
    Ok(state.asr_has_helper())
}

#[tauri::command]
pub async fn dictation_launch_at_login_status() -> Result<bool, String> {
    Ok(autostart::is_launch_at_login_enabled())
}

#[tauri::command]
pub async fn dictation_set_launch_at_login(enabled: bool) -> Result<(), String> {
    autostart::set_launch_at_login(enabled).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn dictation_show_hud<R: Runtime>(
    app: AppHandle<R>,
    phase: String,
    message: String,
) -> Result<(), String> {
    hud::show_hud(
        &app,
        hud::HudPayload {
            phase,
            message,
            partial: None,
            metrics: None,
        },
    );
    Ok(())
}

#[tauri::command]
pub async fn dictation_hide_hud<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    hud::hide_hud(&app);
    Ok(())
}

/// JSON written by `--dictation-self-test` (metrics only; no transcript text).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProductSelfTestReport {
    ok: bool,
    daily_ready: bool,
    helper_healthy: bool,
    engine: DictationEngine,
    grace_ms: u64,
    resample_ms: u64,
    ipc_ms: u64,
    asr_ms: u64,
    polish_ms: u64,
    insert_ms: u64,
    e2e_ms: u64,
    message: String,
    daily_ready_budget_ms: u64,
}

fn resolve_self_test_out_path() -> PathBuf {
    if let Ok(p) = std::env::var("MEETILY_SELF_TEST_OUT") {
        return PathBuf::from(p);
    }
    let mut dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for _ in 0..8 {
        let logs = dir.join("tools").join("dictation-bench").join("logs");
        let bench = dir.join("tools").join("dictation-bench");
        if logs.is_dir() || bench.is_dir() {
            let _ = std::fs::create_dir_all(&logs);
            return logs.join("product-self-test.json");
        }
        if !dir.pop() {
            break;
        }
    }
    PathBuf::from("product-self-test.json")
}

/// One-shot CLI: wait for warm ASR, run product self-test, write JSON, exit.
///
/// Exit 0 when `e2e_ms < DAILY_READY_E2E_MS` and helper is healthy; exit 2 otherwise;
/// exit 1 on hard failures (helper missing, warm timeout, I/O).
pub async fn run_cli_dictation_self_test<R: Runtime>(app: AppHandle<R>, state: DictationState) {
    const WARM_TIMEOUT: Duration = Duration::from_secs(30);
    let out_path = resolve_self_test_out_path();
    log::info!(
        "Dictation CLI self-test starting; output={}",
        out_path.display()
    );

    let deadline = tokio::time::Instant::now() + WARM_TIMEOUT;
    loop {
        let status = state.status().await;
        if status.helper_ready && status.warm_ready {
            break;
        }
        if !state.asr_has_helper() {
            let msg = "dictation-helper missing — cannot self-test";
            log::error!("{msg}");
            let _ = write_self_test_failure(&out_path, msg, &state).await;
            app.exit(1);
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            let msg = "timed out waiting for dictation ASR warm (30s)";
            log::error!("{msg}");
            let _ = write_self_test_failure(&out_path, msg, &state).await;
            app.exit(1);
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Ensure Idle before self-test (agent may still be settling).
    tokio::time::sleep(Duration::from_millis(300)).await;

    match state.run_self_test(&app, false).await {
        Ok(result) => {
            let engine = state.settings_snapshot().await.engine;
            let helper_healthy = state.status().await.helper_ready;
            let report = ProductSelfTestReport {
                ok: result.ok,
                daily_ready: result.daily_ready && helper_healthy,
                helper_healthy,
                engine,
                grace_ms: result.metrics.grace_ms,
                resample_ms: result.metrics.resample_ms,
                ipc_ms: result.metrics.ipc_ms,
                asr_ms: result.metrics.asr_ms,
                polish_ms: result.metrics.polish_ms,
                insert_ms: result.metrics.insert_ms,
                e2e_ms: result.metrics.e2e_ms,
                message: result.message,
                daily_ready_budget_ms: DAILY_READY_E2E_MS,
            };
            if let Err(e) = write_report(&out_path, &report) {
                log::error!("Failed to write self-test JSON: {e}");
                app.exit(1);
                return;
            }
            log::info!(
                "Dictation CLI self-test wrote {} e2e_ms={} daily_ready={}",
                out_path.display(),
                report.e2e_ms,
                report.daily_ready
            );
            let code = if report.daily_ready && report.ok {
                0
            } else {
                2
            };
            app.exit(code);
        }
        Err(e) => {
            log::error!("Dictation CLI self-test failed: {e}");
            let _ = write_self_test_failure(&out_path, &e, &state).await;
            app.exit(1);
        }
    }
}

async fn write_self_test_failure(
    path: &PathBuf,
    message: &str,
    state: &DictationState,
) -> Result<(), String> {
    let engine = state.settings_snapshot().await.engine;
    let helper_healthy = state.status().await.helper_ready;
    let empty = LatencyMetrics::default();
    let report = ProductSelfTestReport {
        ok: false,
        daily_ready: false,
        helper_healthy,
        engine,
        grace_ms: empty.grace_ms,
        resample_ms: empty.resample_ms,
        ipc_ms: empty.ipc_ms,
        asr_ms: empty.asr_ms,
        polish_ms: empty.polish_ms,
        insert_ms: empty.insert_ms,
        e2e_ms: empty.e2e_ms,
        message: message.to_string(),
        daily_ready_budget_ms: DAILY_READY_E2E_MS,
    };
    write_report(path, &report)
}

fn write_report(path: &PathBuf, report: &ProductSelfTestReport) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(report).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())?;
    Ok(())
}

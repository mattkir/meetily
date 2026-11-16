//! Dictation session state machine.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::{mpsc, Mutex, RwLock};

use super::asr::AsrClient;
use super::capture::MicCapture;
use super::history;
use super::hotkey::{self, HotkeyEvent, HotkeyWatcher};
use super::hud::{self, HudPayload, LatencyHudMetrics};
use super::insert::{self, InsertTarget};
use super::polish::{self, PolishPath};
use super::settings::{DictationEngine, DictationSettings};
use super::{ASR_SAMPLE_RATE, END_OF_SPEECH_GRACE_MS, DAILY_READY_E2E_MS};

/// Warm short utterance e2e budget for daily-ready gate.
pub const STREAM_CHUNK_MS: u64 = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DictationPhase {
    Idle,
    Listening,
    Processing,
    Error,
    BlockedMeeting,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LatencyMetrics {
    pub grace_ms: u64,
    pub resample_ms: u64,
    pub ipc_ms: u64,
    pub asr_ms: u64,
    pub polish_ms: u64,
    pub insert_ms: u64,
    pub e2e_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictationStatus {
    pub phase: DictationPhase,
    pub message: String,
    pub enabled: bool,
    pub helper_ready: bool,
    pub warm_ready: bool,
    pub daily_ready: bool,
    pub hands_free: bool,
    pub last_metrics: Option<LatencyMetrics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelfTestResult {
    pub ok: bool,
    pub daily_ready: bool,
    pub message: String,
    pub metrics: LatencyMetrics,
}

pub struct DictationAgent {
    settings: RwLock<DictationSettings>,
    phase: RwLock<DictationPhase>,
    message: RwLock<String>,
    hands_free: RwLock<bool>,
    asr: AsrClient,
    capture: Mutex<Option<MicCapture>>,
    insert_target: Mutex<Option<InsertTarget>>,
    /// Key-up / finalize start instant for e2e timing.
    release_at: Mutex<Option<Instant>>,
    stream_task_cancel: Mutex<Option<Arc<AtomicBool>>>,
    hotkey_tx: Mutex<Option<mpsc::UnboundedSender<HotkeyEvent>>>,
    hotkey_watcher: Mutex<Option<HotkeyWatcher>>,
    running: RwLock<bool>,
    warm_ready: AtomicBool,
    last_metrics: RwLock<Option<LatencyMetrics>>,
    last_short_e2e_ms: RwLock<Option<u64>>,
}

impl DictationAgent {
    pub fn new(settings: DictationSettings) -> Self {
        Self {
            settings: RwLock::new(settings),
            phase: RwLock::new(DictationPhase::Idle),
            message: RwLock::new(String::new()),
            hands_free: RwLock::new(false),
            asr: AsrClient::new(),
            capture: Mutex::new(None),
            insert_target: Mutex::new(None),
            release_at: Mutex::new(None),
            stream_task_cancel: Mutex::new(None),
            hotkey_tx: Mutex::new(None),
            hotkey_watcher: Mutex::new(None),
            running: RwLock::new(false),
            warm_ready: AtomicBool::new(false),
            last_metrics: RwLock::new(None),
            last_short_e2e_ms: RwLock::new(None),
        }
    }

    pub async fn status(&self) -> DictationStatus {
        let warm = self.warm_ready.load(Ordering::SeqCst) && self.asr.is_healthy();
        let short = *self.last_short_e2e_ms.read().await;
        let daily = warm && short.map(|ms| ms < DAILY_READY_E2E_MS).unwrap_or(false);
        DictationStatus {
            phase: *self.phase.read().await,
            message: self.message.read().await.clone(),
            enabled: self.settings.read().await.enabled,
            helper_ready: self.asr.has_helper() && self.asr.is_healthy(),
            warm_ready: warm,
            daily_ready: daily,
            hands_free: *self.hands_free.read().await,
            last_metrics: self.last_metrics.read().await.clone(),
        }
    }

    pub fn asr_has_helper(&self) -> bool {
        self.asr.has_helper()
    }

    pub async fn settings_snapshot(&self) -> DictationSettings {
        self.settings.read().await.clone()
    }

    pub async fn update_settings(&self, settings: DictationSettings) {
        let previous = self.settings.read().await.clone();
        let engine_changed = previous.engine != settings.engine;
        let shortcuts_changed = previous.hold_shortcut != settings.hold_shortcut
            || previous.hands_free_shortcut != settings.hands_free_shortcut;
        let login_changed = previous.launch_at_login != settings.launch_at_login;

        *self.settings.write().await = settings.clone();

        if engine_changed {
            self.asr.invalidate().await;
            self.warm_ready.store(false, Ordering::SeqCst);
        }

        if shortcuts_changed {
            if let Some(watcher) = self.hotkey_watcher.lock().await.as_ref() {
                watcher.update_config(hotkey::config_from_settings(
                    &settings.hold_shortcut,
                    &settings.hands_free_shortcut,
                ));
            }
        }

        if login_changed {
            if let Err(e) = super::autostart::set_launch_at_login(settings.launch_at_login) {
                log::warn!("Launch at login update failed: {e}");
            }
        }
    }

    /// Warm (or re-warm) the sidecar for `engine` without starting hotkeys.
    pub async fn warm_asr(&self, engine: DictationEngine) -> Result<(), String> {
        self.asr
            .ensure_ready(engine)
            .await
            .map_err(|e| e.to_string())?;
        self.warm_ready.store(true, Ordering::SeqCst);
        Ok(())
    }

    pub async fn start<R: Runtime>(self: &Arc<Self>, app: AppHandle<R>) -> Result<(), String> {
        if *self.running.read().await {
            return Ok(());
        }

        let settings = self.settings.read().await.clone();
        let hk = hotkey::config_from_settings(&settings.hold_shortcut, &settings.hands_free_shortcut);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let watcher = HotkeyWatcher::start(tx.clone(), hk).map_err(|e| e)?;
        *self.hotkey_tx.lock().await = Some(tx);
        *self.hotkey_watcher.lock().await = Some(watcher);
        *self.running.write().await = true;

        // Sync login item with stored preference.
        if let Err(e) = super::autostart::set_launch_at_login(settings.launch_at_login) {
            log::warn!("Launch at login sync: {e}");
        }

        let _ = hud::ensure_hud_window(&app);
        let agent = Arc::clone(self);
        let app_handle = app.clone();

        tauri::async_runtime::spawn(async move {
            log::info!("Dictation agent event loop started");
            while let Some(event) = rx.recv().await {
                if let Err(e) = DictationAgent::handle_event_arc(&agent, &app_handle, event).await {
                    log::warn!("Dictation event error: {e}");
                    agent
                        .set_phase(&app_handle, DictationPhase::Error, &e)
                        .await;
                    hud::show_hud(
                        &app_handle,
                        HudPayload {
                            phase: "error".into(),
                            message: e,
                            partial: None,
                            metrics: None,
                        },
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(1600)).await;
                    agent
                        .set_phase(&app_handle, DictationPhase::Idle, "")
                        .await;
                    hud::hide_hud(&app_handle);
                }
            }
            log::info!("Dictation agent event loop ended");
        });

        // Warm ASR — Ready only after warm succeeds (fail-closed without helper).
        let engine = settings.engine;
        match self.asr.ensure_ready(engine).await {
            Ok(()) => {
                self.warm_ready.store(true, Ordering::SeqCst);
                self.set_phase(&app, DictationPhase::Idle, "Ready").await;

                // ASR-only self-test so daily-ready is set without a Settings click.
                // Skip when CLI already owns the same test (`--dictation-self-test`).
                let skip_cli = super::autostart::wants_dictation_self_test();
                let perms = super::permissions::check_permissions();
                let should_auto_self_test =
                    settings.enabled && perms.all_granted() && !skip_cli;
                if should_auto_self_test {
                    match self.run_self_test(&app, false).await {
                        Ok(result) => {
                            log::info!(
                                "Dictation startup self-test ok daily_ready={} e2e_ms={}",
                                result.daily_ready,
                                result.metrics.e2e_ms
                            );
                            self.set_phase(
                                &app,
                                DictationPhase::Idle,
                                &result.message,
                            )
                            .await;
                        }
                        Err(e) => {
                            log::warn!(
                                "Dictation startup self-test failed ({e}); dictation remains usable"
                            );
                        }
                    }
                } else if !skip_cli {
                    log::info!(
                        "Dictation startup self-test skipped enabled={} mic={} accessibility={} input_monitoring={}",
                        settings.enabled,
                        perms.microphone,
                        perms.accessibility,
                        perms.input_monitoring
                    );
                }
            }
            Err(e) => {
                self.warm_ready.store(false, Ordering::SeqCst);
                log::warn!("Dictation ASR warm failed: {e}");
                self.set_phase(
                    &app,
                    DictationPhase::Idle,
                    "Not ready — helper warm required",
                )
                .await;
            }
        }

        crate::tray::refresh_dictation_status(&app).await;
        Ok(())
    }

    pub async fn stop(&self) {
        *self.running.write().await = false;
        self.cancel_stream_task().await;
        if let Some(watcher) = self.hotkey_watcher.lock().await.take() {
            watcher.stop();
        }
        *self.hotkey_tx.lock().await = None;
        if let Some(cap) = self.capture.lock().await.take() {
            cap.cancel();
        }
        self.asr.stream_abort().await;
        self.asr.shutdown().await;
        self.warm_ready.store(false, Ordering::SeqCst);
    }

    async fn handle_event_arc<R: Runtime>(
        agent: &Arc<Self>,
        app: &AppHandle<R>,
        event: HotkeyEvent,
    ) -> Result<(), String> {
        if !agent.settings.read().await.enabled {
            return Ok(());
        }

        match event {
            HotkeyEvent::HoldStart => agent.begin_listen(app, false).await,
            HotkeyEvent::HoldEnd => agent.finish_listen(app).await,
            HotkeyEvent::Cancel => agent.cancel(app).await,
            HotkeyEvent::HandsFreeToggle => agent.toggle_hands_free(app).await,
        }
    }

    async fn meeting_blocks(&self) -> bool {
        let allow = self.settings.read().await.allow_during_meeting;
        if allow {
            return false;
        }
        crate::audio::recording_commands::is_recording().await
    }

    async fn begin_listen<R: Runtime>(
        self: &Arc<Self>,
        app: &AppHandle<R>,
        from_hands_free: bool,
    ) -> Result<(), String> {
        let phase = *self.phase.read().await;
        if matches!(
            phase,
            DictationPhase::Listening | DictationPhase::Processing
        ) {
            return Ok(());
        }

        if !self.asr.has_helper() || !self.warm_ready.load(Ordering::SeqCst) {
            return Err(
                "Dictation not ready — dictation-helper must be installed and warmed".into(),
            );
        }
        if let Err(e) = self.asr.require_healthy() {
            return Err(e.to_string());
        }

        if self.meeting_blocks().await {
            self.set_phase(
                app,
                DictationPhase::BlockedMeeting,
                "Meeting recording in progress",
            )
            .await;
            hud::show_hud(
                app,
                HudPayload {
                    phase: "blocked".into(),
                    message: "Dictation blocked while meeting records".into(),
                    partial: None,
                    metrics: None,
                },
            );
            crate::tray::refresh_dictation_status(app).await;
            tokio::time::sleep(std::time::Duration::from_millis(1400)).await;
            self.set_phase(app, DictationPhase::Idle, "").await;
            hud::hide_hud(app);
            return Ok(());
        }

        let perms = super::permissions::check_permissions();
        if !perms.microphone {
            return Err("Microphone permission required".into());
        }
        if !perms.accessibility {
            return Err("Accessibility permission required for paste".into());
        }
        if !perms.input_monitoring && !from_hands_free {
            log::warn!("Input Monitoring may be incomplete");
        }

        // Capture paste target before HUD shows (Fn-down).
        let target = tauri::async_runtime::spawn_blocking(insert::capture_insert_target)
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        *self.insert_target.lock().await = Some(target);

        let capture = MicCapture::start().map_err(|e| e.to_string())?;
        *self.capture.lock().await = Some(capture);

        let engine = self.settings.read().await.engine;
        let app_for_partial = app.clone();
        self.asr
            .stream_start(engine, move |partial| {
                hud::show_hud(
                    &app_for_partial,
                    HudPayload {
                        phase: "listening".into(),
                        message: "Listening…".into(),
                        partial: Some(partial),
                        metrics: None,
                    },
                );
            })
            .await
            .map_err(|e| e.to_string())?;

        self.set_phase(app, DictationPhase::Listening, "Listening…")
            .await;
        hud::show_hud(
            app,
            HudPayload {
                phase: "listening".into(),
                message: if from_hands_free {
                    "Hands-free listening".into()
                } else {
                    "Listening…".into()
                },
                partial: None,
                metrics: None,
            },
        );
        crate::tray::refresh_dictation_status(app).await;

        self.after_begin_listen(app.clone(), from_hands_free).await;
        Ok(())
    }

    async fn cancel_stream_task(&self) {
        if let Some(c) = self.stream_task_cancel.lock().await.take() {
            c.store(true, Ordering::SeqCst);
        }
    }

    async fn finish_listen<R: Runtime>(&self, app: &AppHandle<R>) -> Result<(), String> {
        if *self.phase.read().await != DictationPhase::Listening {
            return Ok(());
        }
        if *self.hands_free.read().await {
            return Ok(());
        }
        *self.release_at.lock().await = Some(Instant::now());
        self.process_utterance(app).await
    }

    async fn process_utterance<R: Runtime>(&self, app: &AppHandle<R>) -> Result<(), String> {
        self.cancel_stream_task().await;
        self.set_phase(app, DictationPhase::Processing, "Processing…")
            .await;
        hud::show_hud(
            app,
            HudPayload {
                phase: "processing".into(),
                message: "Processing…".into(),
                partial: None,
                metrics: None,
            },
        );
        crate::tray::refresh_dictation_status(app).await;

        if let Err(e) = self.asr.require_healthy() {
            self.asr.stream_abort().await;
            *self.insert_target.lock().await = None;
            return Err(e.to_string());
        }

        let release_at = self
            .release_at
            .lock()
            .await
            .take()
            .unwrap_or_else(Instant::now);
        let grace_ms = END_OF_SPEECH_GRACE_MS;

        let capture = self
            .capture
            .lock()
            .await
            .take()
            .ok_or_else(|| "No active capture".to_string())?;

        // Drain any not-yet-streamed audio before finish, push final stream chunks.
        let undrained = capture.drain_new().unwrap_or_default();
        if !undrained.is_empty() {
            let app_p = app.clone();
            let _ = self
                .asr
                .stream_audio(&undrained, move |partial| {
                    hud::show_hud(
                        &app_p,
                        HudPayload {
                            phase: "processing".into(),
                            message: "Processing…".into(),
                            partial: Some(partial),
                            metrics: None,
                        },
                    );
                })
                .await;
        }

        let (samples, resample_ms) = tauri::async_runtime::spawn_blocking(move || {
            capture.finish(grace_ms)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;

        // Trailing samples after last drain — finish returns full buffer; send only
        // the grace-period delta by comparing lengths is hard. Send empty trailing
        // and rely on already-streamed audio + finalize buffer in helper.
        // For correctness: finalize with empty trailing if we streamed; helper has buffer.
        // Also push any samples beyond what was streamed — we lost the drained cursor
        // when we took ownership. Re-send last ~grace window as trailing.
        let trailing_samples = {
            let n = ((grace_ms as f64 / 1000.0) * ASR_SAMPLE_RATE as f64) as usize + 160;
            if samples.len() > n {
                samples[samples.len() - n..].to_vec()
            } else {
                samples.clone()
            }
        };

        let app_for_partial = app.clone();
        let (text, asr_timing) = self
            .asr
            .stream_finalize(&trailing_samples, move |partial| {
                hud::show_hud(
                    &app_for_partial,
                    HudPayload {
                        phase: "processing".into(),
                        message: "Processing…".into(),
                        partial: Some(partial),
                        metrics: None,
                    },
                );
            })
            .await
            .map_err(|e| e.to_string())?;

        let duration_ms = (samples.len() as f64 / ASR_SAMPLE_RATE as f64 * 1000.0) as u64;
        let settings = self.settings.read().await.clone();

        let polish_start = Instant::now();
        let polished = polish::polish_utterance(app, &text, duration_ms, &settings).await;
        let polish_ms = polish_start.elapsed().as_millis() as u64;
        let cleaned = polished.text;

        if cleaned.is_empty() {
            *self.insert_target.lock().await = None;
            self.set_phase(app, DictationPhase::Idle, "No speech").await;
            hud::hide_hud(app);
            crate::tray::refresh_dictation_status(app).await;
            return Ok(());
        }

        let path_label = match polished.path {
            PolishPath::Off => "off",
            PolishPath::Heuristic => "heuristic",
            PolishPath::Llm => "llm",
        };
        log::info!(
            "Dictation polished via {path_label} heuristic_ms={} llm_ms={:?} duration_ms={duration_ms} chars={}",
            polished.heuristic_ms,
            polished.llm_ms,
            cleaned.chars().count()
        );

        let target = self.insert_target.lock().await.clone();
        let insert_start = Instant::now();
        let method = tauri::async_runtime::spawn_blocking({
            let cleaned = cleaned.clone();
            let target = target.clone();
            move || insert::insert_text_to_target(&cleaned, target.as_ref())
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| {
            // Surface paste-target-lost as HUD error (fail closed).
            e.to_string()
        })?;
        let insert_ms = insert_start.elapsed().as_millis() as u64;
        *self.insert_target.lock().await = None;

        let e2e_ms = release_at.elapsed().as_millis() as u64;
        let metrics = LatencyMetrics {
            grace_ms,
            resample_ms,
            ipc_ms: asr_timing.ipc_ms,
            asr_ms: asr_timing.asr_ms,
            polish_ms,
            insert_ms,
            e2e_ms,
        };
        self.record_metrics(&metrics, duration_ms).await;

        log::info!(
            "Dictation latency grace_ms={} resample_ms={} ipc_ms={} asr_ms={} polish_ms={} insert_ms={} e2e_ms={} chars={}",
            metrics.grace_ms,
            metrics.resample_ms,
            metrics.ipc_ms,
            metrics.asr_ms,
            metrics.polish_ms,
            metrics.insert_ms,
            metrics.e2e_ms,
            cleaned.chars().count()
        );

        let method_label = match method {
            insert::InsertMethod::Accessibility => "accessibility",
            insert::InsertMethod::ClipboardPaste => "clipboard",
        };

        let engine_label = format!("{:?}", settings.engine);
        let _ = history::append_entry(
            app,
            cleaned,
            &engine_label,
            duration_ms,
            method_label,
        );

        hud::show_hud(
            app,
            HudPayload {
                phase: "processing".into(),
                message: format!("Done · {}ms", metrics.e2e_ms),
                partial: None,
                metrics: Some(LatencyHudMetrics {
                    grace_ms: metrics.grace_ms,
                    resample_ms: metrics.resample_ms,
                    ipc_ms: metrics.ipc_ms,
                    asr_ms: metrics.asr_ms,
                    polish_ms: metrics.polish_ms,
                    insert_ms: metrics.insert_ms,
                    e2e_ms: metrics.e2e_ms,
                }),
            },
        );
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;

        self.set_phase(app, DictationPhase::Idle, "Inserted").await;
        hud::hide_hud(app);
        crate::tray::refresh_dictation_status(app).await;
        let _ = app.emit("dictation-complete", &metrics);
        Ok(())
    }

    async fn record_metrics(&self, metrics: &LatencyMetrics, audio_duration_ms: u64) {
        *self.last_metrics.write().await = Some(metrics.clone());
        // Short utterance: under ~2.5s of audio qualifies for daily-ready gate.
        if audio_duration_ms <= 2_500 {
            *self.last_short_e2e_ms.write().await = Some(metrics.e2e_ms);
        }
    }

    /// Settings self-test: warm + short synthetic PCM through stream path + insert probe.
    /// Does not log transcript text. Gates daily_ready on e2e_ms < DAILY_READY_E2E_MS.
    pub async fn run_self_test<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        include_insert: bool,
    ) -> Result<SelfTestResult, String> {
        if *self.phase.read().await != DictationPhase::Idle {
            return Err("Dictation busy — finish or cancel first".into());
        }
        if !self.asr.has_helper() {
            return Err("dictation-helper missing — cannot self-test".into());
        }

        let engine = self.settings.read().await.engine;
        let warm_start = Instant::now();
        self.asr
            .ensure_ready(engine)
            .await
            .map_err(|e| e.to_string())?;
        self.warm_ready.store(true, Ordering::SeqCst);
        let _warm_ms = warm_start.elapsed().as_millis() as u64;

        let target = if include_insert {
            Some(
                tauri::async_runtime::spawn_blocking(insert::capture_insert_target)
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };

        let release_at = Instant::now();
        let grace_ms = END_OF_SPEECH_GRACE_MS;

        // ~0.4s of low-level noise so engines return quickly (no private audio).
        let synthetic: Vec<f32> = (0..6400)
            .map(|i| (i as f32 * 0.02).sin() * 0.02)
            .collect();

        let app_p = app.clone();
        self.asr
            .stream_start(engine, move |_| {})
            .await
            .map_err(|e| e.to_string())?;

        // Chunked stream to exercise live path.
        for chunk in synthetic.chunks(1920) {
            let app_c = app_p.clone();
            self.asr
                .stream_audio(chunk, move |partial| {
                    let _ = partial;
                    let _ = &app_c;
                })
                .await
                .map_err(|e| e.to_string())?;
        }

        tokio::time::sleep(std::time::Duration::from_millis(grace_ms)).await;

        let (text, asr_timing) = self
            .asr
            .stream_finalize(&[], |_| {})
            .await
            .map_err(|e| e.to_string())?;

        let settings = self.settings.read().await.clone();
        let polish_start = Instant::now();
        // Polish a fixed short phrase when ASR returns empty (silence).
        let to_polish = if text.trim().is_empty() {
            "self test".to_string()
        } else {
            text
        };
        let polished = polish::polish_utterance(app, &to_polish, 400, &settings).await;
        let polish_ms = polish_start.elapsed().as_millis() as u64;

        let insert_ms = if include_insert {
            let insert_start = Instant::now();
            let probe = "·";
            let target = target.clone();
            tauri::async_runtime::spawn_blocking(move || {
                insert::insert_text_to_target(probe, target.as_ref())
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
            insert_start.elapsed().as_millis() as u64
        } else {
            0
        };

        let e2e_ms = release_at.elapsed().as_millis() as u64;
        let metrics = LatencyMetrics {
            grace_ms,
            resample_ms: 0,
            ipc_ms: asr_timing.ipc_ms,
            asr_ms: asr_timing.asr_ms,
            polish_ms,
            insert_ms,
            e2e_ms,
        };
        self.record_metrics(&metrics, 400).await;

        let daily_ready = self.warm_ready.load(Ordering::SeqCst)
            && metrics.e2e_ms < DAILY_READY_E2E_MS;
        let message = if daily_ready {
            format!(
                "Daily ready · warm short e2e {}ms (<{}ms)",
                metrics.e2e_ms, DAILY_READY_E2E_MS
            )
        } else {
            format!(
                "Not daily-ready · e2e {}ms (need <{}ms after warm)",
                metrics.e2e_ms, DAILY_READY_E2E_MS
            )
        };

        log::info!(
            "Dictation self-test grace_ms={} ipc_ms={} asr_ms={} polish_ms={} insert_ms={} e2e_ms={} daily_ready={}",
            metrics.grace_ms,
            metrics.ipc_ms,
            metrics.asr_ms,
            metrics.polish_ms,
            metrics.insert_ms,
            metrics.e2e_ms,
            daily_ready
        );

        let _ = polished;
        Ok(SelfTestResult {
            ok: true,
            daily_ready,
            message,
            metrics,
        })
    }

    async fn cancel<R: Runtime>(&self, app: &AppHandle<R>) -> Result<(), String> {
        let phase = *self.phase.read().await;
        if !matches!(
            phase,
            DictationPhase::Listening | DictationPhase::Processing | DictationPhase::BlockedMeeting
        ) {
            if *self.hands_free.read().await {
                *self.hands_free.write().await = false;
            }
            return Ok(());
        }

        self.cancel_stream_task().await;
        self.asr.stream_abort().await;
        if let Some(cap) = self.capture.lock().await.take() {
            cap.cancel();
        }
        *self.insert_target.lock().await = None;
        *self.release_at.lock().await = None;
        *self.hands_free.write().await = false;
        self.set_phase(app, DictationPhase::Idle, "Cancelled").await;
        hud::hide_hud(app);
        crate::tray::refresh_dictation_status(app).await;
        Ok(())
    }

    async fn toggle_hands_free<R: Runtime>(
        self: &Arc<Self>,
        app: &AppHandle<R>,
    ) -> Result<(), String> {
        let currently = *self.hands_free.read().await;
        if currently {
            *self.hands_free.write().await = false;
            if *self.phase.read().await == DictationPhase::Listening {
                *self.release_at.lock().await = Some(Instant::now());
                return self.process_utterance(app).await;
            }
            self.set_phase(app, DictationPhase::Idle, "").await;
            hud::hide_hud(app);
            return Ok(());
        }

        *self.hands_free.write().await = true;
        log::info!("Dictation hands-free toggled on");
        self.begin_listen(app, true).await
    }

    async fn set_phase<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        phase: DictationPhase,
        message: &str,
    ) {
        *self.phase.write().await = phase;
        *self.message.write().await = message.to_string();
        hud::emit_status(
            app,
            match phase {
                DictationPhase::Idle => "idle",
                DictationPhase::Listening => "listening",
                DictationPhase::Processing => "processing",
                DictationPhase::Error => "error",
                DictationPhase::BlockedMeeting => "blocked",
            },
            message,
        );
    }
}

impl DictationAgent {
    /// Start background stream pump + hands-free watcher (needs Arc).
    pub async fn after_begin_listen<R: Runtime>(self: &Arc<Self>, app: AppHandle<R>, hands_free: bool) {
        self.start_stream_pump(app.clone()).await;
        if hands_free {
            self.start_hands_free_watch(app).await;
        }
    }

    async fn start_stream_pump<R: Runtime>(self: &Arc<Self>, app: AppHandle<R>) {
        self.cancel_stream_task().await;
        let cancel = Arc::new(AtomicBool::new(false));
        *self.stream_task_cancel.lock().await = Some(cancel.clone());
        let agent = Arc::clone(self);

        tauri::async_runtime::spawn(async move {
            while !cancel.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(STREAM_CHUNK_MS)).await;
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                if *agent.phase.read().await != DictationPhase::Listening {
                    break;
                }
                let chunk = {
                    let guard = agent.capture.lock().await;
                    match guard.as_ref() {
                        Some(cap) => cap.drain_new().unwrap_or_default(),
                        None => break,
                    }
                };
                if chunk.is_empty() {
                    continue;
                }
                let app_c = app.clone();
                if let Err(e) = agent
                    .asr
                    .stream_audio(&chunk, move |partial| {
                        hud::show_hud(
                            &app_c,
                            HudPayload {
                                phase: "listening".into(),
                                message: "Listening…".into(),
                                partial: Some(partial),
                                metrics: None,
                            },
                        );
                    })
                    .await
                {
                    log::warn!("stream_audio pump error: {e}");
                    break;
                }
            }
        });
    }

    async fn start_hands_free_watch<R: Runtime>(self: &Arc<Self>, app: AppHandle<R>) {
        let agent = Arc::clone(self);
        let settings = self.settings.read().await.clone();
        let silence_ms = settings.hands_free_silence_ms.max(400);
        let max_ms = settings.hands_free_max_ms.max(5_000);
        let started = Instant::now();
        let mut heard_speech = false;
        let mut silence_since: Option<Instant> = None;
        let window = (ASR_SAMPLE_RATE as usize) / 5; // 200ms
        let speech_threshold = 0.02f32;

        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if !*agent.hands_free.read().await {
                    break;
                }
                if *agent.phase.read().await != DictationPhase::Listening {
                    break;
                }
                if started.elapsed().as_millis() as u64 >= max_ms {
                    log::info!("Hands-free max duration reached ({max_ms}ms)");
                    *agent.release_at.lock().await = Some(Instant::now());
                    let _ = agent.process_utterance(&app).await;
                    *agent.hands_free.write().await = false;
                    break;
                }

                let rms = {
                    let guard = agent.capture.lock().await;
                    match guard.as_ref() {
                        Some(cap) => cap.recent_rms(window),
                        None => break,
                    }
                };

                if rms >= speech_threshold {
                    heard_speech = true;
                    silence_since = None;
                } else if heard_speech {
                    let since = silence_since.get_or_insert_with(Instant::now);
                    if since.elapsed().as_millis() as u64 >= silence_ms {
                        log::info!("Hands-free silence end ({silence_ms}ms)");
                        *agent.release_at.lock().await = Some(Instant::now());
                        let _ = agent.process_utterance(&app).await;
                        *agent.hands_free.write().await = false;
                        break;
                    }
                }
            }
        });
    }
}

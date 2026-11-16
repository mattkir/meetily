use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tauri::{AppHandle, Manager, Runtime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DictationEngine {
    /// Apple SpeechTranscriber via dictation-helper (Phase 2 default).
    Apple,
    /// FluidAudio Parakeet v3 Core ML (Phase 2 opt-in).
    ParakeetV3Coreml,
}

impl Default for DictationEngine {
    fn default() -> Self {
        Self::Apple
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolishMode {
    /// Heuristic cleanup + dictionary + snippets only (default latency path).
    Heuristic,
    /// Always run LLM polish after heuristics.
    AlwaysLlm,
    /// LLM polish only for long utterances (~9s+).
    LongOnly,
    /// Pass ASR text through unchanged (trim only).
    Off,
}

impl Default for PolishMode {
    fn default() -> Self {
        Self::Heuristic
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictationSettings {
    pub enabled: bool,
    /// Primary hold-to-talk: `fn` (CGEvent) or a chord string like `ctrl+shift+d`.
    pub hold_shortcut: String,
    /// Hands-free toggle shortcut.
    pub hands_free_shortcut: String,
    pub engine: DictationEngine,
    pub polish_mode: PolishMode,
    /// Optional llama-helper model name (e.g. `qwen3.5:4b`). Empty → default/ready model.
    #[serde(default)]
    pub polish_model: Option<String>,
    /// Simple string → string replacements applied after ASR heuristics.
    pub dictionary: HashMap<String, String>,
    /// Spoken trigger → expansion (e.g. "email sig" → signature block).
    #[serde(default)]
    pub snippets: HashMap<String, String>,
    /// When true, allow dictation while a meeting is recording (default false).
    pub allow_during_meeting: bool,
    /// Register macOS login item (SMAppService) so Meetily starts at login.
    #[serde(default)]
    pub launch_at_login: bool,
    /// Hands-free: end utterance after this many ms of silence (after speech detected).
    #[serde(default = "default_silence_end_ms")]
    pub hands_free_silence_ms: u64,
    /// Hands-free: hard cap on listening duration.
    #[serde(default = "default_max_duration_ms")]
    pub hands_free_max_ms: u64,
}

fn default_silence_end_ms() -> u64 {
    1_200
}

fn default_max_duration_ms() -> u64 {
    60_000
}

impl Default for DictationSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            hold_shortcut: "fn".to_string(),
            hands_free_shortcut: "ctrl+shift+space".to_string(),
            engine: DictationEngine::Apple,
            polish_mode: PolishMode::Heuristic,
            polish_model: None,
            dictionary: HashMap::new(),
            snippets: HashMap::new(),
            allow_during_meeting: false,
            launch_at_login: false,
            hands_free_silence_ms: default_silence_end_ms(),
            hands_free_max_ms: default_max_duration_ms(),
        }
    }
}

fn settings_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app data dir: {e}"))?;
    Ok(dir.join("dictation_settings.json"))
}

pub fn load_settings<R: Runtime>(app: &AppHandle<R>) -> DictationSettings {
    let path = match settings_path(app) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("Dictation settings path unavailable: {e}");
            return DictationSettings::default();
        }
    };

    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            log::warn!("Corrupt dictation settings ({e}); using defaults");
            DictationSettings::default()
        }),
        Err(_) => DictationSettings::default(),
    }
}

pub fn save_settings<R: Runtime>(
    app: &AppHandle<R>,
    settings: &DictationSettings,
) -> Result<(), String> {
    let path = settings_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let raw = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, raw).map_err(|e| e.to_string())
}

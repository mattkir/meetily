//! Always-on local dictation agent (Wispr-style hold-to-talk).
//!
//! Session shell, Fn hotkey, mic-only capture, AX/clipboard insert,
//! heuristic-first text polish (dictionary/snippets; optional LLM for long
//! utterances), local history, HUD/tray status, permissions, meeting conflict
//! policy, and `dictation-helper` ASR sidecar (Apple default; Parakeet Core ML opt-in).

pub mod asr;
pub mod autostart;
pub mod capture;
pub mod commands;
pub mod history;
pub mod hotkey;
pub mod hud;
pub mod insert;
pub mod permissions;
pub mod polish;
pub mod session;
pub mod settings;

pub use commands::DictationState;
pub use session::{DictationPhase, DictationStatus, LatencyMetrics, SelfTestResult};
pub use settings::{DictationEngine, DictationSettings, PolishMode};

/// End-of-speech grace after key-up (dictation path; not meeting VAD redemption).
pub const END_OF_SPEECH_GRACE_MS: u64 = 120;

/// Warm short utterance release→caret budget for the daily-ready gate.
pub const DAILY_READY_E2E_MS: u64 = 300;

/// Target sample rate for ASR IPC (PCM f32 mono).
pub const ASR_SAMPLE_RATE: u32 = 16_000;

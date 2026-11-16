/**
 * Default model names for meeting transcription engines.
 * IMPORTANT: Keep in sync with Rust constants in src-tauri/src/config.rs
 *
 * Roles (Phase 4):
 * - Meeting live STT default: Parakeet ONNX int8 (in-process). Dictation uses
 *   Apple / Parakeet Core ML via dictation-helper — not these meeting providers.
 * - Whisper large-v3-turbo: recommended Whisper choice for multilingual, import,
 *   and retranscription — not the dictation hot path.
 */

/**
 * Default Whisper model when the Whisper provider is selected or used for batch.
 * Multilingual / import / retranscribe path.
 */
export const DEFAULT_WHISPER_MODEL = 'large-v3-turbo';

/**
 * Default Parakeet model for meeting live transcription when no preference is set.
 * Quantized ONNX (CPU); downloadable via Rust parakeet_engine catalog.
 */
export const DEFAULT_PARAKEET_MODEL = 'parakeet-tdt-0.6b-v3-int8';

/**
 * Model defaults by provider type
 */
export const MODEL_DEFAULTS = {
  whisper: DEFAULT_WHISPER_MODEL,
  localWhisper: DEFAULT_WHISPER_MODEL,
  parakeet: DEFAULT_PARAKEET_MODEL,
} as const;

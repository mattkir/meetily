//! Heuristic-first dictation text polish.
//!
//! Default path: post_processor cleanup + dictionary + snippets (&lt;10ms).
//! Optional llama-helper polish only for long utterances or AlwaysLlm.
//! Never logs transcript bodies.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use tauri::{AppHandle, Manager, Runtime};

use super::settings::{DictationSettings, PolishMode};

/// Utterance duration (ms) above which LongOnly may invoke the LLM.
pub const LONG_UTTERANCE_MS: u64 = 9_000;

const DEFAULT_POLISH_MODEL: &str = "qwen3.5:4b";

const LLM_SYSTEM_PROMPT: &str = concat!(
    "You clean up dictated speech for pasting into a text field. ",
    "Fix grammar, punctuation, and filler words. ",
    "Preserve the speaker's meaning and wording where possible. ",
    "Do not add greetings, commentary, or quotes. ",
    "Output only the cleaned text."
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolishPath {
    Off,
    Heuristic,
    Llm,
}

#[derive(Debug, Clone)]
pub struct PolishResult {
    pub text: String,
    pub path: PolishPath,
    pub heuristic_ms: u64,
    pub llm_ms: Option<u64>,
}

/// Apply polish according to settings. Short default path is heuristic-only.
pub async fn polish_utterance<R: Runtime>(
    app: &AppHandle<R>,
    raw: &str,
    duration_ms: u64,
    settings: &DictationSettings,
) -> PolishResult {
    match settings.polish_mode {
        PolishMode::Off => PolishResult {
            text: raw.trim().to_string(),
            path: PolishPath::Off,
            heuristic_ms: 0,
            llm_ms: None,
        },
        PolishMode::Heuristic => {
            let (text, heuristic_ms) = heuristic_pipeline(raw, settings);
            PolishResult {
                text,
                path: PolishPath::Heuristic,
                heuristic_ms,
                llm_ms: None,
            }
        }
        PolishMode::LongOnly | PolishMode::AlwaysLlm => {
            let (heuristic_text, heuristic_ms) = heuristic_pipeline(raw, settings);
            let want_llm = matches!(settings.polish_mode, PolishMode::AlwaysLlm)
                || duration_ms >= LONG_UTTERANCE_MS;

            if !want_llm {
                return PolishResult {
                    text: heuristic_text,
                    path: PolishPath::Heuristic,
                    heuristic_ms,
                    llm_ms: None,
                };
            }

            match llm_polish(app, &heuristic_text, settings).await {
                Ok((text, llm_ms)) => PolishResult {
                    text,
                    path: PolishPath::Llm,
                    heuristic_ms,
                    llm_ms: Some(llm_ms),
                },
                Err(e) => {
                    log::warn!(
                        "Dictation LLM polish skipped ({e}); using heuristic (chars={})",
                        heuristic_text.chars().count()
                    );
                    PolishResult {
                        text: heuristic_text,
                        path: PolishPath::Heuristic,
                        heuristic_ms,
                        llm_ms: None,
                    }
                }
            }
        }
    }
}

fn heuristic_pipeline(raw: &str, settings: &DictationSettings) -> (String, u64) {
    let start = Instant::now();
    let cleaned = crate::audio::post_processor::cleanup_for_dictation(raw);
    let with_dict = apply_dictionary(&cleaned, &settings.dictionary);
    let with_snippets = apply_snippets(&with_dict, &settings.snippets);
    let ms = start.elapsed().as_millis() as u64;
    if ms > 10 {
        log::warn!("Dictation heuristic polish slow: {ms}ms (chars={})", with_snippets.chars().count());
    } else {
        log::debug!(
            "Dictation heuristic polish {}ms chars={}",
            ms,
            with_snippets.chars().count()
        );
    }
    (with_snippets, ms)
}

/// Case-insensitive whole-word (and multi-word phrase) replacements.
pub fn apply_dictionary(text: &str, dictionary: &HashMap<String, String>) -> String {
    if dictionary.is_empty() || text.is_empty() {
        return text.to_string();
    }

    // Longer keys first so multi-word phrases win over substrings.
    let mut entries: Vec<(&String, &String)> = dictionary.iter().collect();
    entries.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

    let mut out = text.to_string();
    for (from, to) in entries {
        if from.is_empty() {
            continue;
        }
        out = replace_all_ci_bounded(&out, from, to);
    }
    out
}

/// Expand snippet triggers: exact utterance match, then in-text phrase replace.
pub fn apply_snippets(text: &str, snippets: &HashMap<String, String>) -> String {
    if snippets.is_empty() || text.is_empty() {
        return text.to_string();
    }

    let trimmed = text.trim();
    let trimmed_lower = trimmed.to_lowercase();
    for (trigger, expansion) in snippets {
        if trigger.is_empty() {
            continue;
        }
        if trimmed_lower == trigger.to_lowercase() {
            return expansion.clone();
        }
    }

    let mut entries: Vec<(&String, &String)> = snippets.iter().collect();
    entries.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

    let mut out = text.to_string();
    for (trigger, expansion) in entries {
        if trigger.is_empty() {
            continue;
        }
        out = replace_all_ci_bounded(&out, trigger, expansion);
    }
    out
}

fn replace_all_ci_bounded(haystack: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() || haystack.is_empty() {
        return haystack.to_string();
    }

    let hay_lower = haystack.to_lowercase();
    let needle_lower = needle.to_lowercase();
    let needle_len = needle_lower.len();
    let mut out = String::with_capacity(haystack.len());
    let mut cursor = 0usize;

    while let Some(rel) = hay_lower[cursor..].find(&needle_lower) {
        let start = cursor + rel;
        let end = start + needle_len;

        let before_ok = if start == 0 {
            true
        } else {
            haystack[..start]
                .chars()
                .last()
                .map(|c| !is_word_char(c))
                .unwrap_or(true)
        };
        let after_ok = if end >= haystack.len() {
            true
        } else {
            haystack[end..]
                .chars()
                .next()
                .map(|c| !is_word_char(c))
                .unwrap_or(true)
        };

        out.push_str(&haystack[cursor..start]);
        if before_ok && after_ok {
            out.push_str(replacement);
        } else {
            out.push_str(&haystack[start..end]);
        }
        cursor = end;
    }
    out.push_str(&haystack[cursor..]);
    out
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '\''
}

async fn llm_polish<R: Runtime>(
    app: &AppHandle<R>,
    text: &str,
    settings: &DictationSettings,
) -> Result<(String, u64), String> {
    if text.trim().is_empty() {
        return Ok((String::new(), 0));
    }

    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app data dir: {e}"))?;

    let model = settings
        .polish_model
        .as_deref()
        .filter(|m| !m.is_empty())
        .unwrap_or(DEFAULT_POLISH_MODEL);

    // Prefer a ready model: configured → recommended → first ready.
    let model = resolve_polish_model(&app_data_dir, model).await?;

    let start = Instant::now();
    let polished = crate::summary::summary_engine::generate_with_builtin(
        &app_data_dir,
        &model,
        LLM_SYSTEM_PROMPT,
        text,
        None,
    )
    .await
    .map_err(|e| e.to_string())?;
    let llm_ms = start.elapsed().as_millis() as u64;

    let cleaned = polished.trim().trim_matches('"').to_string();
    if cleaned.is_empty() {
        return Err("LLM returned empty polish".into());
    }

    log::info!(
        "Dictation LLM polish ok model={} {}ms chars={}",
        model,
        llm_ms,
        cleaned.chars().count()
    );
    Ok((cleaned, llm_ms))
}

async fn resolve_polish_model(app_data_dir: &PathBuf, preferred: &str) -> Result<String, String> {
    use crate::summary::summary_engine::models::{get_available_models, get_model_path};

    let ready = |name: &str| -> bool {
        get_model_path(app_data_dir, name)
            .ok()
            .map(|p| p.exists())
            .unwrap_or(false)
    };

    if ready(preferred) {
        return Ok(preferred.to_string());
    }

    if let Ok(rec) =
        crate::summary::summary_engine::commands::get_recommended_summary_model_for_current_system()
    {
        if ready(rec) {
            log::info!(
                "Dictation polish falling back to recommended model {} (preferred {} not ready)",
                rec,
                preferred
            );
            return Ok(rec.to_string());
        }
    }

    for def in get_available_models() {
        if ready(&def.name) {
            log::info!(
                "Dictation polish falling back to ready model {} (preferred {} not ready)",
                def.name,
                preferred
            );
            return Ok(def.name);
        }
    }

    Err(format!(
        "No local summary model ready for polish (wanted {preferred}). Download one in Summary settings."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_replaces_phrases_longest_first() {
        let mut dict = HashMap::new();
        dict.insert("meet ily".into(), "Meetily".into());
        dict.insert("meet".into(), "gather".into());
        let out = apply_dictionary("please open meet ily now", &dict);
        assert!(out.contains("Meetily"));
        assert!(!out.to_lowercase().contains("meet ily"));
    }

    #[test]
    fn snippet_exact_utterance_expands() {
        let mut snippets = HashMap::new();
        snippets.insert("email sig".into(), "Best regards,\nMatt".into());
        assert_eq!(
            apply_snippets("email sig", &snippets),
            "Best regards,\nMatt"
        );
        assert_eq!(
            apply_snippets("Email Sig", &snippets),
            "Best regards,\nMatt"
        );
    }

    #[test]
    fn heuristic_strips_fillers_without_llm() {
        let settings = DictationSettings {
            polish_mode: PolishMode::Heuristic,
            ..Default::default()
        };
        let (text, ms) = heuristic_pipeline("um hello there", &settings);
        assert!(ms < 50, "heuristic should be fast, got {ms}ms");
        assert!(!text.to_lowercase().split_whitespace().any(|w| w == "um"));
        assert!(text.to_lowercase().contains("hello"));
    }
}

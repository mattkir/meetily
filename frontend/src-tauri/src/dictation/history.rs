//! Local dictation history (metadata only in logs; text kept on disk privately).

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Manager, Runtime};
use uuid::Uuid;

const MAX_ENTRIES: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: String,
    pub created_at: DateTime<Local>,
    pub text: String,
    pub engine: String,
    pub duration_ms: u64,
    pub insert_method: String,
}

fn history_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app data dir: {e}"))?;
    Ok(dir.join("dictation_history.json"))
}

pub fn load_history<R: Runtime>(app: &AppHandle<R>) -> Vec<HistoryEntry> {
    let path = match history_path(app) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

pub fn append_entry<R: Runtime>(
    app: &AppHandle<R>,
    text: String,
    engine: &str,
    duration_ms: u64,
    insert_method: &str,
) -> Result<HistoryEntry, String> {
    let mut entries = load_history(app);
    let entry = HistoryEntry {
        id: Uuid::new_v4().to_string(),
        created_at: Local::now(),
        text,
        engine: engine.to_string(),
        duration_ms,
        insert_method: insert_method.to_string(),
    };
    entries.insert(0, entry.clone());
    entries.truncate(MAX_ENTRIES);
    save_history(app, &entries)?;
    // Privacy: log length only, never content
    log::info!(
        "Dictation history appended id={} chars={} method={}",
        entry.id,
        entry.text.chars().count(),
        insert_method
    );
    Ok(entry)
}

fn save_history<R: Runtime>(app: &AppHandle<R>, entries: &[HistoryEntry]) -> Result<(), String> {
    let path = history_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let raw = serde_json::to_string_pretty(entries).map_err(|e| e.to_string())?;
    std::fs::write(path, raw).map_err(|e| e.to_string())
}

pub fn clear_history<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    save_history(app, &[])
}

pub fn get_entry<R: Runtime>(app: &AppHandle<R>, id: &str) -> Option<HistoryEntry> {
    load_history(app).into_iter().find(|e| e.id == id)
}

/// Delete one history entry by id. Returns true if an entry was removed.
pub fn remove_entry<R: Runtime>(app: &AppHandle<R>, id: &str) -> Result<bool, String> {
    let mut entries = load_history(app);
    let before = entries.len();
    entries.retain(|e| e.id != id);
    if entries.len() == before {
        return Ok(false);
    }
    save_history(app, &entries)?;
    log::info!("Dictation history removed id={id}");
    Ok(true)
}

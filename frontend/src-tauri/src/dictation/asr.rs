//! Dictation ASR sidecar protocol (`dictation-helper`).
//!
//! Line-delimited JSON on stdin/stdout. Audio is base64 PCM; responses must
//! never be written to Meetily logs as full transcript content.
//!
//! Full wire schema: [`PROTOCOL.md`](./PROTOCOL.md).

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use super::settings::DictationEngine;
use super::ASR_SAMPLE_RATE;

/// Wire protocol commands sent to dictation-helper.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum AsrRequest {
    Load {
        id: String,
        engine: String,
    },
    Warm {
        id: String,
    },
    Transcribe {
        id: String,
        format: String,
        sample_rate: u32,
        pcm_b64: String,
    },
    StreamStart {
        id: String,
    },
    StreamAudio {
        id: String,
        format: String,
        sample_rate: u32,
        pcm_b64: String,
    },
    StreamFinalize {
        id: String,
        format: String,
        sample_rate: u32,
        pcm_b64: String,
    },
    Unload {
        id: String,
    },
    Ping {
        id: String,
    },
}

/// Wire protocol events from dictation-helper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsrResponse {
    pub id: String,
    pub ok: bool,
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub partial: Option<bool>,
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Timing for a single ASR IPC exchange (no transcript text).
#[derive(Debug, Clone, Default)]
pub struct AsrTiming {
    pub ipc_ms: u64,
    pub asr_ms: u64,
}

fn engine_wire_name(engine: DictationEngine) -> &'static str {
    match engine {
        DictationEngine::Apple => "apple",
        DictationEngine::ParakeetV3Coreml => "parakeet-v3-coreml",
    }
}

/// Resolve `dictation-helper` the same way llama-helper is staged.
pub fn resolve_helper_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("MEETILY_DICTATION_HELPER") {
        let p = PathBuf::from(path);
        if p.exists() {
            return Some(p);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in candidate_names() {
                let candidate = dir.join(&name);
                if candidate.exists() {
                    return Some(candidate);
                }
                let nested = dir.join("binaries").join(&name);
                if nested.exists() {
                    return Some(nested);
                }
            }
        }
    }

    // Dev fallbacks relative to src-tauri
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for name in candidate_names() {
        let candidate = manifest.join("binaries").join(&name);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    None
}

fn candidate_names() -> Vec<String> {
    let triple = std::env::var("TARGET").unwrap_or_else(|_| {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            "aarch64-apple-darwin".to_string()
        }
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        {
            "x86_64-apple-darwin".to_string()
        }
        #[cfg(not(target_os = "macos"))]
        {
            "unknown".to_string()
        }
    });
    vec![
        format!("dictation-helper-{}", triple),
        "dictation-helper".to_string(),
    ]
}

/// Client for the Swift sidecar. Fail-closed when helper is missing/unhealthy.
pub struct AsrClient {
    helper_path: Option<PathBuf>,
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<ChildStdin>>,
    stdout: Mutex<Option<BufReader<ChildStdout>>>,
    healthy: AtomicBool,
    loaded_engine: Mutex<Option<DictationEngine>>,
    next_id: Mutex<u64>,
    /// Active streaming request id (same id for start/audio/finalize).
    stream_id: Mutex<Option<String>>,
}

impl AsrClient {
    pub fn new() -> Self {
        let helper_path = resolve_helper_path();
        if helper_path.is_some() {
            log::info!("dictation-helper found at {:?}", helper_path);
        } else {
            log::warn!("dictation-helper not found; dictation insert is disabled (fail-closed)");
        }
        Self {
            helper_path,
            child: Mutex::new(None),
            stdin: Mutex::new(None),
            stdout: Mutex::new(None),
            healthy: AtomicBool::new(false),
            loaded_engine: Mutex::new(None),
            next_id: Mutex::new(1),
            stream_id: Mutex::new(None),
        }
    }

    pub fn has_helper(&self) -> bool {
        self.helper_path.is_some()
    }

    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::SeqCst)
    }

    /// Require a live helper before any ASR / insert path.
    pub fn require_healthy(&self) -> Result<()> {
        if self.helper_path.is_none() {
            return Err(anyhow!(
                "dictation-helper is not installed; dictation is unavailable"
            ));
        }
        if !self.is_healthy() {
            return Err(anyhow!("dictation-helper is not ready; warm load required"));
        }
        Ok(())
    }

    /// Force a re-load/warm on the next [`Self::ensure_ready`] (e.g. engine change).
    pub async fn invalidate(&self) {
        self.healthy.store(false, Ordering::SeqCst);
        *self.loaded_engine.lock().await = None;
        *self.stream_id.lock().await = None;
    }

    async fn next_id(&self) -> String {
        let mut n = self.next_id.lock().await;
        let id = *n;
        *n += 1;
        id.to_string()
    }

    /// Ensure helper is running and warmed for `engine`. Fails closed if missing.
    pub async fn ensure_ready(&self, engine: DictationEngine) -> Result<()> {
        if self.helper_path.is_none() {
            self.healthy.store(false, Ordering::SeqCst);
            return Err(anyhow!(
                "dictation-helper is not installed; cannot warm ASR"
            ));
        }

        let loaded = *self.loaded_engine.lock().await;
        if self.is_healthy() && loaded == Some(engine) {
            return Ok(());
        }

        self.spawn().await?;
        let load_id = self.next_id().await;
        let resp = self
            .roundtrip(AsrRequest::Load {
                id: load_id,
                engine: engine_wire_name(engine).to_string(),
            })
            .await?;
        if !resp.ok {
            self.healthy.store(false, Ordering::SeqCst);
            return Err(anyhow!(
                "ASR load failed: {}",
                resp.error.unwrap_or_default()
            ));
        }

        let warm_id = self.next_id().await;
        let resp = self.roundtrip(AsrRequest::Warm { id: warm_id }).await?;
        if !resp.ok {
            self.healthy.store(false, Ordering::SeqCst);
            return Err(anyhow!(
                "ASR warm failed: {}",
                resp.error.unwrap_or_default()
            ));
        }

        *self.loaded_engine.lock().await = Some(engine);
        self.healthy.store(true, Ordering::SeqCst);
        log::info!(
            "dictation-helper warmed engine={}",
            engine_wire_name(engine)
        );
        Ok(())
    }

    async fn spawn(&self) -> Result<()> {
        let path = self
            .helper_path
            .as_ref()
            .ok_or_else(|| anyhow!("no helper"))?;

        {
            let mut child_lock = self.child.lock().await;
            if let Some(mut existing) = child_lock.take() {
                let _ = existing.kill().await;
            }
        }

        let mut child = Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("spawn dictation-helper at {}", path.display()))?;

        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;

        *self.stdin.lock().await = Some(stdin);
        *self.stdout.lock().await = Some(BufReader::new(stdout));
        *self.child.lock().await = Some(child);
        *self.stream_id.lock().await = None;
        self.healthy.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn write_request(&self, request: &AsrRequest) -> Result<()> {
        let line = serde_json::to_string(request)?;
        let mut stdin_lock = self.stdin.lock().await;
        let stdin = stdin_lock
            .as_mut()
            .ok_or_else(|| anyhow!("ASR stdin closed"))?;
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn read_response(&self) -> Result<AsrResponse> {
        let mut stdout_lock = self.stdout.lock().await;
        let reader = stdout_lock
            .as_mut()
            .ok_or_else(|| anyhow!("ASR stdout closed"))?;
        let mut response_line = String::new();
        reader.read_line(&mut response_line).await?;
        if response_line.trim().is_empty() {
            return Err(anyhow!("ASR stdout closed"));
        }
        let resp: AsrResponse =
            serde_json::from_str(response_line.trim()).context("parse ASR response")?;
        Ok(resp)
    }

    async fn roundtrip(&self, request: AsrRequest) -> Result<AsrResponse> {
        self.write_request(&request).await?;
        self.read_response().await
    }

    /// Begin a streaming utterance. Returns the stream request id.
    pub async fn stream_start<F>(
        &self,
        engine: DictationEngine,
        mut on_partial: F,
    ) -> Result<String>
    where
        F: FnMut(String),
    {
        let _ = &mut on_partial;
        self.ensure_ready(engine).await?;
        self.require_healthy()?;

        let id = self.next_id().await;
        self.write_request(&AsrRequest::StreamStart { id: id.clone() })
            .await?;

        let resp = self.read_response().await?;
        if resp.id != id {
            return Err(anyhow!("stream_start id mismatch"));
        }
        if !resp.ok {
            return Err(anyhow!(
                "stream_start failed: {}",
                resp.error.unwrap_or_else(|| "unknown".into())
            ));
        }
        *self.stream_id.lock().await = Some(id.clone());
        Ok(id)
    }

    /// Push a PCM chunk during hold. Invokes `on_partial` for live HUD updates.
    pub async fn stream_audio<F>(&self, samples: &[f32], mut on_partial: F) -> Result<()>
    where
        F: FnMut(String),
    {
        let id = self
            .stream_id
            .lock()
            .await
            .clone()
            .ok_or_else(|| anyhow!("no active stream"))?;
        if samples.is_empty() {
            return Ok(());
        }

        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let pcm_b64 = encode_base64(&bytes);
        self.write_request(&AsrRequest::StreamAudio {
            id: id.clone(),
            format: "f32le".to_string(),
            sample_rate: ASR_SAMPLE_RATE,
            pcm_b64,
        })
        .await?;

        // Consume partials until acked / error.
        loop {
            let resp = self.read_response().await?;
            if resp.id != id {
                log::warn!(
                    "dictation-helper stream_audio id mismatch (got {}, expected {})",
                    resp.id,
                    id
                );
                continue;
            }
            if !resp.ok {
                return Err(anyhow!(
                    "stream_audio failed: {}",
                    resp.error.unwrap_or_else(|| "unknown".into())
                ));
            }
            let event = resp.event.as_deref().unwrap_or("");
            match event {
                "partial" => {
                    if let Some(text) = resp.text {
                        if !text.is_empty() {
                            on_partial(text);
                        }
                    }
                }
                "acked" => return Ok(()),
                other => {
                    if resp.partial == Some(true) {
                        if let Some(text) = resp.text {
                            if !text.is_empty() {
                                on_partial(text);
                            }
                        }
                        continue;
                    }
                    if other.is_empty() {
                        return Ok(());
                    }
                    log::warn!("unexpected ASR event during stream_audio: {other}");
                    return Ok(());
                }
            }
        }
    }

    /// Finalize the stream after key-up (+ optional trailing PCM). Returns final text + timing.
    pub async fn stream_finalize<F>(
        &self,
        trailing: &[f32],
        mut on_partial: F,
    ) -> Result<(String, AsrTiming)>
    where
        F: FnMut(String),
    {
        let id = self
            .stream_id
            .lock()
            .await
            .take()
            .ok_or_else(|| anyhow!("no active stream"))?;

        let bytes: Vec<u8> = trailing.iter().flat_map(|s| s.to_le_bytes()).collect();
        let pcm_b64 = encode_base64(&bytes);

        let ipc_start = Instant::now();
        self.write_request(&AsrRequest::StreamFinalize {
            id: id.clone(),
            format: "f32le".to_string(),
            sample_rate: ASR_SAMPLE_RATE,
            pcm_b64,
        })
        .await?;
        let ipc_ms = ipc_start.elapsed().as_millis() as u64;

        let asr_start = Instant::now();
        loop {
            let resp = self.read_response().await?;
            if resp.id != id {
                log::warn!(
                    "dictation-helper stream_finalize id mismatch (got {}, expected {})",
                    resp.id,
                    id
                );
                continue;
            }
            if !resp.ok {
                return Err(anyhow!(
                    "stream_finalize failed: {}",
                    resp.error.unwrap_or_else(|| "unknown".into())
                ));
            }

            let event = resp.event.as_deref().unwrap_or("");
            match event {
                "partial" => {
                    if let Some(text) = resp.text {
                        if !text.is_empty() {
                            on_partial(text);
                        }
                    }
                }
                "final" => {
                    let text = resp.text.unwrap_or_default();
                    let asr_ms = asr_start.elapsed().as_millis() as u64;
                    log::info!("dictation ASR final chars={}", text.chars().count());
                    return Ok((text, AsrTiming { ipc_ms, asr_ms }));
                }
                other => {
                    if resp.partial == Some(true) {
                        if let Some(text) = resp.text {
                            if !text.is_empty() {
                                on_partial(text);
                            }
                        }
                        continue;
                    }
                    if resp.partial == Some(false) || other.is_empty() {
                        let text = resp.text.unwrap_or_default();
                        let asr_ms = asr_start.elapsed().as_millis() as u64;
                        log::info!("dictation ASR final chars={}", text.chars().count());
                        return Ok((text, AsrTiming { ipc_ms, asr_ms }));
                    }
                    log::warn!("unexpected ASR event while finalizing: {other}");
                }
            }
        }
    }

    /// Abort an in-flight stream without finalize (cancel path).
    pub async fn stream_abort(&self) {
        *self.stream_id.lock().await = None;
    }

    /// Batch transcribe mono f32 PCM at 16 kHz. Never logs transcript text.
    pub async fn transcribe<F>(
        &self,
        samples: &[f32],
        engine: DictationEngine,
        mut on_partial: F,
    ) -> Result<(String, AsrTiming)>
    where
        F: FnMut(String),
    {
        self.ensure_ready(engine).await?;
        self.require_healthy()?;

        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let pcm_b64 = encode_base64(&bytes);
        let id = self.next_id().await;

        let ipc_start = Instant::now();
        self.write_request(&AsrRequest::Transcribe {
            id: id.clone(),
            format: "f32le".to_string(),
            sample_rate: ASR_SAMPLE_RATE,
            pcm_b64,
        })
        .await?;
        let ipc_ms = ipc_start.elapsed().as_millis() as u64;

        let asr_start = Instant::now();
        loop {
            let resp = self.read_response().await?;
            if resp.id != id {
                log::warn!(
                    "dictation-helper response id mismatch (got {}, expected {})",
                    resp.id,
                    id
                );
                continue;
            }
            if !resp.ok {
                return Err(anyhow!(
                    "transcribe failed: {}",
                    resp.error.unwrap_or_else(|| "unknown".into())
                ));
            }

            let event = resp.event.as_deref().unwrap_or("");
            match event {
                "partial" => {
                    if let Some(text) = resp.text {
                        if !text.is_empty() {
                            on_partial(text);
                        }
                    }
                }
                "final" => {
                    let text = resp.text.unwrap_or_default();
                    let asr_ms = asr_start.elapsed().as_millis() as u64;
                    log::info!("dictation ASR final chars={}", text.chars().count());
                    return Ok((text, AsrTiming { ipc_ms, asr_ms }));
                }
                other => {
                    if resp.partial == Some(true) {
                        if let Some(text) = resp.text {
                            if !text.is_empty() {
                                on_partial(text);
                            }
                        }
                        continue;
                    }
                    if resp.partial == Some(false) || other.is_empty() {
                        let text = resp.text.unwrap_or_default();
                        let asr_ms = asr_start.elapsed().as_millis() as u64;
                        log::info!("dictation ASR final chars={}", text.chars().count());
                        return Ok((text, AsrTiming { ipc_ms, asr_ms }));
                    }
                    log::warn!("unexpected ASR event while transcribing: {other}");
                }
            }
        }
    }

    pub async fn shutdown(&self) {
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.kill().await;
        }
        *self.stdin.lock().await = None;
        *self.stdout.lock().await = None;
        *self.loaded_engine.lock().await = None;
        *self.stream_id.lock().await = None;
        self.healthy.store(false, Ordering::SeqCst);
    }

    /// Lightweight health check via ping (after warm).
    pub async fn ping(&self) -> Result<()> {
        if self.helper_path.is_none() {
            return Err(anyhow!("no helper"));
        }
        let id = self.next_id().await;
        let resp = self.roundtrip(AsrRequest::Ping { id }).await?;
        if !resp.ok {
            self.healthy.store(false, Ordering::SeqCst);
            return Err(anyhow!("ping failed"));
        }
        Ok(())
    }
}

impl Default for AsrClient {
    fn default() -> Self {
        Self::new()
    }
}

fn encode_base64(data: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Documented protocol schema for implementers / tests.
pub fn protocol_schema_doc() -> &'static str {
    r#"dictation-helper protocol (JSON lines, stdin/stdout):
  → {"cmd":"load","id":"...","engine":"apple"|"parakeet-v3-coreml"}
  → {"cmd":"warm","id":"..."}
  → {"cmd":"transcribe","id":"...","format":"f32le","sample_rate":16000,"pcm_b64":"..."}
  → {"cmd":"stream_start","id":"..."}
  → {"cmd":"stream_audio","id":"...","format":"f32le","sample_rate":16000,"pcm_b64":"..."}
  → {"cmd":"stream_finalize","id":"...","format":"f32le","sample_rate":16000,"pcm_b64":"..."}
  → {"cmd":"unload","id":"..."}
  → {"cmd":"ping","id":"..."}
  ← {"id":"...","ok":true,"event":"loaded"|"ready"|"stream_ready"|"acked"|"partial"|"final"|"unloaded"|"pong","text?":"...","partial?":bool,"engine?":"..."}
  ← {"id":"...","ok":false,"error":"..."}
Audio: mono f32 little-endian @ 16 kHz. Never log full transcript text. Fail-closed without helper."#
}

#[allow(dead_code)]
pub fn helper_exists_at(path: &Path) -> bool {
    path.exists()
}

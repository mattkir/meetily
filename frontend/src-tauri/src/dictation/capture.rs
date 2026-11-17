//! Mic-only capture for dictation (never mixes system audio).
//! Supports streaming drain of resampled PCM while holding Fn.
//!
//! On macOS, capture uses AVAudioEngine + voice processing so Bluetooth
//! headsets (AirPods) enter HFP and deliver real mic audio. If that stream
//! stays digitally silent, fall back to the built-in mic via cpal.

use anyhow::{anyhow, Result};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::ASR_SAMPLE_RATE;

const RESAMPLE_CHUNK: usize = 1024;
/// Peak below this is treated as digital silence (Bluetooth HFP stub frames, etc.).
pub const DIGITAL_SILENCE_PEAK: f32 = 1e-5;
/// Wait this long on a silent non-built-in input before switching to the built-in mic.
pub const SILENCE_ABANDON_MS: u64 = 400;

/// Result of a finished dictation capture (no PCM content in logs).
#[derive(Debug, Clone)]
pub struct CaptureFinish {
    pub samples: Vec<f32>,
    pub resample_ms: u64,
    pub device_name: String,
    pub peak: f32,
    /// True when capture switched devices mid-hold (ASR should be restarted cleanly).
    pub restarted_on_fallback: bool,
}

/// Pure decision: abandon the current input for the built-in mic.
pub fn should_abandon_silent_input(peak: f32, elapsed_ms: u64, already_builtin: bool) -> bool {
    !already_builtin && elapsed_ms >= SILENCE_ABANDON_MS && peak < DIGITAL_SILENCE_PEAK
}

/// Collect microphone audio until `stop` is set, streaming resampled PCM @ 16 kHz.
pub struct MicCapture {
    stop: Arc<AtomicBool>,
    /// Resampled mono f32 @ 16 kHz (grows during capture).
    samples: Arc<Mutex<Vec<f32>>>,
    /// How many resampled samples have been drained for streaming.
    drained: Arc<Mutex<usize>>,
    /// Wall time spent in the final resample flush (set on finish).
    resample_ms: Arc<Mutex<u64>>,
    /// Absolute peak amplitude observed on the active device (f32 bits).
    peak_bits: Arc<AtomicU32>,
    /// Name of the device that produced the final buffer.
    device_name: Arc<Mutex<String>>,
    /// Set when capture abandoned a silent input for the built-in mic mid-hold.
    restarted_on_fallback: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<Result<()>>>,
}

impl MicCapture {
    pub fn start() -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(Mutex::new(Vec::new()));
        let drained = Arc::new(Mutex::new(0usize));
        let resample_ms = Arc::new(Mutex::new(0u64));
        let peak_bits = Arc::new(AtomicU32::new(0f32.to_bits()));
        let device_name = Arc::new(Mutex::new(String::new()));
        let restarted_on_fallback = Arc::new(AtomicBool::new(false));

        let stop_t = stop.clone();
        let samples_t = samples.clone();
        let drained_t = drained.clone();
        let resample_ms_t = resample_ms.clone();
        let peak_t = peak_bits.clone();
        let name_t = device_name.clone();
        let restarted_t = restarted_on_fallback.clone();

        let join = thread::Builder::new()
            .name("dictation-mic".into())
            .spawn(move || {
                run_capture(
                    stop_t,
                    samples_t,
                    drained_t,
                    resample_ms_t,
                    peak_t,
                    name_t,
                    restarted_t,
                )
            })?;

        Ok(Self {
            stop,
            samples,
            drained,
            resample_ms,
            peak_bits,
            device_name,
            restarted_on_fallback,
            join: Some(join),
        })
    }

    /// Take newly available resampled samples since the last drain (streaming).
    pub fn drain_new(&self) -> Result<Vec<f32>> {
        let buf = self.samples.lock().map_err(|_| anyhow!("samples lock"))?;
        let mut drained = self.drained.lock().map_err(|_| anyhow!("drained lock"))?;
        Ok(take_undrained(&buf, &mut drained))
    }

    /// Approximate RMS of recent resampled audio (hands-free silence detection).
    pub fn recent_rms(&self, window_samples: usize) -> f32 {
        let Ok(buf) = self.samples.lock() else {
            return 0.0;
        };
        if buf.is_empty() {
            return 0.0;
        }
        let start = buf.len().saturating_sub(window_samples);
        let slice = &buf[start..];
        if slice.is_empty() {
            return 0.0;
        }
        let sum_sq: f32 = slice.iter().map(|s| s * s).sum();
        (sum_sq / slice.len() as f32).sqrt()
    }

    pub fn sample_count(&self) -> usize {
        self.samples.lock().map(|b| b.len()).unwrap_or(0)
    }

    /// Signal stop; after `grace_ms` the capture thread exits and returns all PCM @ 16 kHz.
    pub fn finish(mut self, grace_ms: u64) -> Result<CaptureFinish> {
        // Allow trailing speech after key-up (80–150 ms budget).
        thread::sleep(Duration::from_millis(grace_ms));
        self.stop.store(true, Ordering::SeqCst);

        let join = self
            .join
            .take()
            .ok_or_else(|| anyhow!("capture thread missing"))?;
        join.join()
            .map_err(|_| anyhow!("capture thread panicked"))??;

        let resample_ms = *self
            .resample_ms
            .lock()
            .map_err(|_| anyhow!("resample_ms lock"))?;
        let pcm = self
            .samples
            .lock()
            .map_err(|_| anyhow!("samples lock"))?
            .clone();
        let device_name = self
            .device_name
            .lock()
            .map_err(|_| anyhow!("device_name lock"))?
            .clone();
        let peak = f32::from_bits(self.peak_bits.load(Ordering::SeqCst));
        let restarted_on_fallback = self.restarted_on_fallback.load(Ordering::SeqCst);
        log::info!(
            "Dictation capture finished: device={} samples={} (~{:.0} ms @ 16kHz) peak={:.6} resample_ms={} fallback={}",
            if device_name.is_empty() {
                "unknown"
            } else {
                &device_name
            },
            pcm.len(),
            pcm.len() as f32 / ASR_SAMPLE_RATE as f32 * 1000.0,
            peak,
            resample_ms,
            restarted_on_fallback
        );
        Ok(CaptureFinish {
            samples: pcm,
            resample_ms,
            device_name,
            peak,
            restarted_on_fallback,
        })
    }

    /// Drain only samples not yet streamed (after finish, prefer full buffer).
    pub fn undrained_tail(&self) -> Result<Vec<f32>> {
        self.drain_new()
    }

    pub fn cancel(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn run_capture(
    stop: Arc<AtomicBool>,
    out: Arc<Mutex<Vec<f32>>>,
    drained: Arc<Mutex<usize>>,
    resample_ms: Arc<Mutex<u64>>,
    peak_bits: Arc<AtomicU32>,
    device_name: Arc<Mutex<String>>,
    restarted_on_fallback: Arc<AtomicBool>,
) -> Result<()> {
    let default_name = default_input_name()?;
    let builtin_name = crate::audio::devices::find_builtin_input_device()
        .ok()
        .flatten()
        .map(|d| d.name);
    let using_builtin = builtin_name
        .as_ref()
        .is_some_and(|b| names_match(&default_name, b));

    if let Ok(mut guard) = device_name.lock() {
        *guard = default_name.clone();
    }

    let started = Instant::now();
    let mut total_resample_ms = 0u64;

    #[cfg(target_os = "macos")]
    {
        peak_bits.store(0f32.to_bits(), Ordering::SeqCst);
        let outcome = macos::capture_via_avaudio(
            &default_name,
            stop.clone(),
            out.clone(),
            peak_bits.clone(),
            using_builtin,
            started,
        )?;
        total_resample_ms = total_resample_ms.saturating_add(outcome.resample_ms);

        match outcome.action {
            CaptureAction::Stopped => {
                if let Ok(mut ms) = resample_ms.lock() {
                    *ms = total_resample_ms;
                }
                return Ok(());
            }
            CaptureAction::AbandonForBuiltin => {
                let Some(ref builtin) = builtin_name else {
                    log::warn!(
                        "Dictation input '{}' stayed silent; no built-in mic to fall back to",
                        default_name
                    );
                    if let Ok(mut ms) = resample_ms.lock() {
                        *ms = total_resample_ms;
                    }
                    return Ok(());
                };
                log::warn!(
                    "Dictation input '{}' digitally silent after {}ms (peak={:.6}); switching to built-in '{}'",
                    default_name,
                    started.elapsed().as_millis(),
                    f32::from_bits(peak_bits.load(Ordering::SeqCst)),
                    builtin
                );
                clear_output_for_fallback(&out, &drained);
                restarted_on_fallback.store(true, Ordering::SeqCst);
                if let Ok(mut guard) = device_name.lock() {
                    *guard = builtin.clone();
                }

                peak_bits.store(0f32.to_bits(), Ordering::SeqCst);
                let fallback = cpal_fallback::capture_named_device(
                    builtin, stop, out, peak_bits, true, started, true,
                )?;
                total_resample_ms = total_resample_ms.saturating_add(fallback.resample_ms);
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = drained;
        let _ = restarted_on_fallback;
        peak_bits.store(0f32.to_bits(), Ordering::SeqCst);
        let outcome = cpal_fallback::capture_named_device(
            &default_name,
            stop,
            out,
            peak_bits,
            using_builtin,
            started,
            true,
        )?;
        total_resample_ms = total_resample_ms.saturating_add(outcome.resample_ms);
        let _ = outcome.action;
    }

    if let Ok(mut ms) = resample_ms.lock() {
        *ms = total_resample_ms;
    }
    Ok(())
}

enum CaptureAction {
    Stopped,
    AbandonForBuiltin,
}

struct DeviceCaptureOutcome {
    action: CaptureAction,
    resample_ms: u64,
}

fn default_input_name() -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        use cidre::core_audio::hardware::System;
        if let Ok(device) = System::default_input_device() {
            if let Ok(name) = device.name() {
                return Ok(name.to_string());
            }
        }
    }
    cpal_fallback::default_input_name()
}

fn names_match(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn update_peak(peak_bits: &AtomicU32, samples: &[f32]) {
    let mut local_peak = 0.0f32;
    for &s in samples {
        let a = s.abs();
        if a > local_peak {
            local_peak = a;
        }
    }
    if local_peak <= 0.0 {
        return;
    }
    loop {
        let current = peak_bits.load(Ordering::SeqCst);
        let current_f = f32::from_bits(current);
        if local_peak <= current_f {
            break;
        }
        if peak_bits
            .compare_exchange(
                current,
                local_peak.to_bits(),
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            break;
        }
    }
}

fn flush_raw_to_out(
    raw: &Arc<Mutex<Vec<f32>>>,
    raw_cursor: &mut usize,
    resampler: Option<&mut SincFixedIn<f32>>,
    input_rate: u32,
    out: &Arc<Mutex<Vec<f32>>>,
) -> Result<u64> {
    let t0 = Instant::now();
    let raw_buf = raw.lock().map_err(|_| anyhow!("raw lock"))?.clone();

    if input_rate == ASR_SAMPLE_RATE {
        if *raw_cursor < raw_buf.len() {
            let slice = &raw_buf[*raw_cursor..];
            if let Ok(mut out_lock) = out.lock() {
                out_lock.extend_from_slice(slice);
            }
            *raw_cursor = raw_buf.len();
        }
        return Ok(t0.elapsed().as_millis() as u64);
    }

    let Some(resampler) = resampler else {
        return Ok(0);
    };

    while *raw_cursor + RESAMPLE_CHUNK <= raw_buf.len() {
        let chunk = vec![raw_buf[*raw_cursor..*raw_cursor + RESAMPLE_CHUNK].to_vec()];
        let processed = resampler.process(&chunk, None)?;
        if let Ok(mut out_lock) = out.lock() {
            out_lock.extend_from_slice(&processed[0]);
        }
        *raw_cursor += RESAMPLE_CHUNK;
    }
    Ok(t0.elapsed().as_millis() as u64)
}

/// Samples not yet consumed. Caller must hold the buffer and cursor together.
fn take_undrained(buf: &[f32], drained: &mut usize) -> Vec<f32> {
    if *drained >= buf.len() {
        return Vec::new();
    }
    let chunk = buf[*drained..].to_vec();
    *drained = buf.len();
    chunk
}

/// Drop resampled PCM and the drain cursor before either lock is released.
fn clear_output_for_fallback(out: &Mutex<Vec<f32>>, drained: &Mutex<usize>) {
    let mut buf = out.lock().unwrap_or_else(|err| err.into_inner());
    let mut cursor = drained.lock().unwrap_or_else(|err| err.into_inner());
    buf.clear();
    *cursor = 0;
    // `into_inner` keeps the poison flag. Later flushes and `finish` use `lock()`.
    out.clear_poison();
    drained.clear_poison();
}

fn append_out(out: &Mutex<Vec<f32>>, samples: &[f32]) {
    if samples.is_empty() {
        return;
    }
    if let Ok(mut out_lock) = out.lock() {
        out_lock.extend_from_slice(samples);
    }
}

fn snapshot_unconsumed(raw: &Mutex<Vec<f32>>, raw_cursor: usize) -> Option<(Vec<f32>, usize)> {
    let raw_buf = raw.lock().ok()?;
    let end = raw_buf.len();
    let pending = if raw_cursor >= end {
        Vec::new()
    } else {
        raw_buf[raw_cursor..].to_vec()
    };
    Some((pending, end))
}

fn proportional_samples(sample_count: usize, input_rate: u32) -> usize {
    if sample_count == 0 || input_rate == 0 {
        return 0;
    }
    ((sample_count as f64) * ASR_SAMPLE_RATE as f64 / input_rate as f64).ceil() as usize
}

/// Feed complete blocks. Returns how many input samples were consumed.
fn push_full_blocks(
    samples: &[f32],
    resampler: &mut SincFixedIn<f32>,
    out: &Arc<Mutex<Vec<f32>>>,
) -> Option<usize> {
    let mut offset = 0usize;
    while samples.len().saturating_sub(offset) >= RESAMPLE_CHUNK {
        let block = samples[offset..offset + RESAMPLE_CHUNK].to_vec();
        let processed = resampler.process(&vec![block], None).ok()?;
        if let Some(produced) = processed.first() {
            append_out(out, produced);
        }
        offset += RESAMPLE_CHUNK;
    }
    Some(offset)
}

/// Zero-pad `rest` once. If the keep window runs past that block, flush only the overflow.
fn append_padded_tail(
    rest: &[f32],
    raw_cursor: usize,
    saw_full_block: bool,
    resampler: &mut SincFixedIn<f32>,
    input_rate: u32,
    out: &Arc<Mutex<Vec<f32>>>,
) -> bool {
    if rest.is_empty() && raw_cursor == 0 && !saw_full_block {
        return true;
    }
    let n = rest.len().min(RESAMPLE_CHUNK);
    let mut padded = vec![0.0f32; RESAMPLE_CHUNK];
    if n > 0 {
        padded[..n].copy_from_slice(&rest[..n]);
    }
    let delay = resampler.output_delay();
    let Ok(processed) = resampler.process(&vec![padded], None) else {
        return false;
    };
    let Some(produced) = processed.first() else {
        return true;
    };
    let wanted = delay.saturating_add(proportional_samples(n, input_rate));
    let skip = if raw_cursor == 0 && !saw_full_block {
        delay.min(produced.len())
    } else {
        0
    };
    let first_end = wanted.min(produced.len());
    if first_end > skip {
        append_out(out, &produced[skip..first_end]);
    }
    if wanted > produced.len() {
        let overflow = wanted - produced.len();
        let Ok(more) = resampler.process(&vec![vec![0.0f32; RESAMPLE_CHUNK]], None) else {
            return false;
        };
        if let Some(extra) = more.first() {
            let take = overflow.min(extra.len());
            append_out(out, &extra[..take]);
        }
    }
    true
}

fn flush_resampler_tail(
    raw: &Arc<Mutex<Vec<f32>>>,
    raw_cursor: usize,
    resampler: Option<&mut SincFixedIn<f32>>,
    input_rate: u32,
    out: &Arc<Mutex<Vec<f32>>>,
) -> Result<()> {
    let Some((pending, _)) = snapshot_unconsumed(raw, raw_cursor) else {
        return Ok(());
    };

    let Some(r) = resampler else {
        append_out(out, &pending);
        return Ok(());
    };

    let Some(consumed) = push_full_blocks(&pending, r, out) else {
        return Ok(());
    };
    append_padded_tail(
        &pending[consumed..],
        raw_cursor,
        consumed > 0,
        r,
        input_rate,
        out,
    );
    Ok(())
}

/// Flush full blocks, then one padded tail after any sample that arrived during the copy.
fn flush_tail_with_one_retry(
    raw: &Arc<Mutex<Vec<f32>>>,
    raw_cursor: usize,
    resampler: Option<&mut SincFixedIn<f32>>,
    input_rate: u32,
    out: &Arc<Mutex<Vec<f32>>>,
    mut before_retry_check: impl FnMut(),
) -> Result<u64> {
    let t0 = Instant::now();
    let Some((pending, snapshot_end)) = snapshot_unconsumed(raw, raw_cursor) else {
        return Ok(t0.elapsed().as_millis() as u64);
    };

    let Some(r) = resampler else {
        append_out(out, &pending);
        before_retry_check();
        let len_now = raw.lock().map(|buf| buf.len()).unwrap_or(snapshot_end);
        if len_now > snapshot_end {
            if let Some((suffix, _)) = snapshot_unconsumed(raw, snapshot_end) {
                append_out(out, &suffix);
            }
        }
        return Ok(t0.elapsed().as_millis() as u64);
    };

    let Some(consumed) = push_full_blocks(&pending, r, out) else {
        return Ok(t0.elapsed().as_millis() as u64);
    };
    let mut rest = pending[consumed..].to_vec();
    let mut saw_full_block = consumed > 0;

    before_retry_check();
    let len_now = raw.lock().map(|buf| buf.len()).unwrap_or(snapshot_end);
    if len_now > snapshot_end {
        if let Some((suffix, _)) = snapshot_unconsumed(raw, snapshot_end) {
            rest.extend_from_slice(&suffix);
            let Some(suffix_consumed) = push_full_blocks(&rest, r, out) else {
                return Ok(t0.elapsed().as_millis() as u64);
            };
            saw_full_block |= suffix_consumed > 0;
            rest = rest[suffix_consumed..].to_vec();
        }
    }

    append_padded_tail(&rest, raw_cursor, saw_full_block, r, input_rate, out);
    Ok(t0.elapsed().as_millis() as u64)
}

fn make_resampler(input_rate: u32) -> Result<SincFixedIn<f32>> {
    let params = SincInterpolationParameters {
        sinc_len: 64,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 16,
        window: WindowFunction::BlackmanHarris2,
    };
    Ok(SincFixedIn::<f32>::new(
        ASR_SAMPLE_RATE as f64 / input_rate as f64,
        2.0,
        params,
        RESAMPLE_CHUNK,
        1,
    )?)
}

fn push_downmix(dst: &mut Vec<f32>, interleaved: &[f32], channels: usize) {
    if channels <= 1 {
        dst.extend_from_slice(interleaved);
        return;
    }
    for frame in interleaved.chunks(channels) {
        let sum: f32 = frame.iter().sum();
        dst.push(sum / channels as f32);
    }
}

fn push_planar_downmix(dst: &mut Vec<f32>, channels: &[&[f32]]) {
    if channels.is_empty() {
        return;
    }
    let n = channels[0].len();
    let ch = channels.len() as f32;
    for i in 0..n {
        let mut sum = 0.0f32;
        for c in channels {
            if i < c.len() {
                sum += c[i];
            }
        }
        dst.push(sum / ch);
    }
}

/// Shared resample loop used by AVAudioEngine and cpal paths.
fn pump_until_stop(
    stop: &AtomicBool,
    raw: &Arc<Mutex<Vec<f32>>>,
    out: &Arc<Mutex<Vec<f32>>>,
    peak_bits: &AtomicU32,
    input_rate: u32,
    using_builtin: bool,
    started: Instant,
    already_abandoned: bool,
    mut on_stop: impl FnMut(),
) -> Result<DeviceCaptureOutcome> {
    let mut resampler = if input_rate == ASR_SAMPLE_RATE {
        None
    } else {
        Some(make_resampler(input_rate)?)
    };
    let mut raw_cursor = 0usize;
    let mut total_resample_ms = 0u64;

    while !stop.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(20));
        let flushed = flush_raw_to_out(raw, &mut raw_cursor, resampler.as_mut(), input_rate, out)?;
        total_resample_ms = total_resample_ms.saturating_add(flushed);

        if !already_abandoned {
            let peak = f32::from_bits(peak_bits.load(Ordering::SeqCst));
            let elapsed_ms = started.elapsed().as_millis() as u64;
            if should_abandon_silent_input(peak, elapsed_ms, using_builtin) {
                on_stop();
                return Ok(DeviceCaptureOutcome {
                    action: CaptureAction::AbandonForBuiltin,
                    resample_ms: total_resample_ms,
                });
            }
        }
    }

    on_stop();
    let deadline = Instant::now() + Duration::from_millis(15);
    let mut seen_len: Option<usize> = None;
    loop {
        let len = raw.lock().map(|buf| buf.len()).unwrap_or(0);
        let flushed = flush_raw_to_out(raw, &mut raw_cursor, resampler.as_mut(), input_rate, out)?;
        total_resample_ms = total_resample_ms.saturating_add(flushed);
        let unchanged = seen_len == Some(len);
        seen_len = Some(len);
        if unchanged || Instant::now() >= deadline {
            break;
        }
        let pause =
            Duration::from_millis(1).min(deadline.saturating_duration_since(Instant::now()));
        if pause.is_zero() {
            break;
        }
        thread::sleep(pause);
    }
    let tail_ms =
        flush_tail_with_one_retry(raw, raw_cursor, resampler.as_mut(), input_rate, out, || {})?;
    total_resample_ms = total_resample_ms.saturating_add(tail_ms);

    Ok(DeviceCaptureOutcome {
        action: CaptureAction::Stopped,
        resample_ms: total_resample_ms,
    })
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use cidre::av;

    struct FireOnce<F: FnOnce()> {
        action: Option<F>,
    }

    impl<F: FnOnce()> FireOnce<F> {
        fn new(action: F) -> Self {
            Self {
                action: Some(action),
            }
        }

        fn fire(&mut self) {
            if let Some(action) = self.action.take() {
                action();
            }
        }
    }

    impl<F: FnOnce()> Drop for FireOnce<F> {
        fn drop(&mut self) {
            self.fire();
        }
    }

    pub fn capture_via_avaudio(
        device_name: &str,
        stop: Arc<AtomicBool>,
        out: Arc<Mutex<Vec<f32>>>,
        peak_bits: Arc<AtomicU32>,
        using_builtin: bool,
        started: Instant,
    ) -> Result<DeviceCaptureOutcome> {
        let mut engine = av::audio::Engine::new();
        let mut input = engine.input_node();

        match input.set_vp_enabled(true) {
            Ok(()) => log::info!("Dictation AVAudioEngine voice processing enabled"),
            Err(e) => log::warn!(
                "Dictation AVAudioEngine voice processing unavailable ({e}); continuing without it"
            ),
        }

        let format = input.output_format_for_bus(0);
        let channels = format.channel_count() as usize;
        let input_rate = format.absd().sample_rate.round() as u32;
        if input_rate == 0 {
            return Err(anyhow!("AVAudioEngine reported zero sample rate"));
        }

        let mixer = engine.main_mixer_node().retained();
        engine.connect_node_to_node(&input, &mixer, Some(&format));
        engine.main_mixer_node_mut().set_output_volume(0.0);

        let raw: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let raw_cb = raw.clone();
        let peak_cb = peak_bits.clone();

        input
            .install_tap_on_bus(0, 1024, None, move |buf, _| {
                let ch_count = buf.format().channel_count() as usize;
                let stride = buf.stride();
                if stride > 1 || ch_count <= 1 {
                    if let Some(data) = buf.data_f32_at(0) {
                        update_peak(&peak_cb, data);
                        if let Ok(mut dst) = raw_cb.lock() {
                            push_downmix(
                                &mut dst,
                                data,
                                if stride > 1 { ch_count.max(1) } else { 1 },
                            );
                        }
                    }
                    return;
                }

                // Planar multi-channel.
                let mut planes: Vec<&[f32]> = Vec::with_capacity(ch_count);
                for c in 0..ch_count {
                    if let Some(plane) = buf.data_f32_at(c) {
                        planes.push(plane);
                    }
                }
                if planes.is_empty() {
                    return;
                }
                let mut mixed = Vec::with_capacity(planes[0].len());
                push_planar_downmix(&mut mixed, &planes);
                update_peak(&peak_cb, &mixed);
                if let Ok(mut dst) = raw_cb.lock() {
                    dst.extend_from_slice(&mixed);
                }
            })
            .map_err(|e| anyhow!("AVAudioEngine installTap failed: {e}"))?;

        engine
            .start()
            .map_err(|e| anyhow!("AVAudioEngine start failed: {e}"))?;

        log::info!(
            "Dictation mic-only capture: backend=AVAudioEngine device={} rate={} ch={}",
            device_name,
            input_rate,
            channels
        );

        let mut teardown = FireOnce::new(|| {
            engine.stop();
            // Tear down so Bluetooth can leave HFP / headset mode.
            unsafe {
                input.remove_tap_on_bus_throws(0);
            }
        });
        let outcome = pump_until_stop(
            &stop,
            &raw,
            &out,
            &peak_bits,
            input_rate,
            using_builtin,
            started,
            false,
            || teardown.fire(),
        );
        // `on_stop` already fired this on the success path. Drop fires it if
        // the pump returns early or panics.
        teardown.fire();
        outcome
    }
}

mod cpal_fallback {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{Device, SampleFormat, Stream, StreamConfig};

    pub fn default_input_name() -> Result<String> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow!("No default microphone"))?;
        Ok(device.name().unwrap_or_else(|_| "default".into()))
    }

    pub fn capture_named_device(
        name: &str,
        stop: Arc<AtomicBool>,
        out: Arc<Mutex<Vec<f32>>>,
        peak_bits: Arc<AtomicU32>,
        using_builtin: bool,
        started: Instant,
        already_abandoned: bool,
    ) -> Result<DeviceCaptureOutcome> {
        let device = find_input_device_by_name(name)?
            .ok_or_else(|| anyhow!("Microphone '{name}' not found"))?;
        capture_on_device(
            &device,
            name,
            stop,
            out,
            peak_bits,
            using_builtin,
            started,
            already_abandoned,
        )
    }

    fn find_input_device_by_name(name: &str) -> Result<Option<Device>> {
        let host = cpal::default_host();
        for device in host.input_devices()? {
            if let Ok(device_name) = device.name() {
                if names_match(&device_name, name) {
                    return Ok(Some(device));
                }
            }
        }
        Ok(None)
    }

    fn capture_on_device(
        device: &Device,
        name: &str,
        stop: Arc<AtomicBool>,
        out: Arc<Mutex<Vec<f32>>>,
        peak_bits: Arc<AtomicU32>,
        using_builtin: bool,
        started: Instant,
        already_abandoned: bool,
    ) -> Result<DeviceCaptureOutcome> {
        let supported = device.default_input_config()?;
        let sample_format = supported.sample_format();
        let channels = supported.channels() as usize;
        let input_rate = supported.sample_rate().0;
        let config: StreamConfig = supported.clone().into();

        log::info!(
            "Dictation mic-only capture: backend=cpal device={} rate={} ch={} format={:?}",
            name,
            input_rate,
            channels,
            sample_format
        );

        let raw: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let stream = build_input_stream(
            device,
            &config,
            sample_format,
            channels,
            raw.clone(),
            peak_bits.clone(),
        )?;
        stream.play()?;
        let mut stream = Some(stream);

        let outcome = pump_until_stop(
            &stop,
            &raw,
            &out,
            &peak_bits,
            input_rate,
            using_builtin,
            started,
            already_abandoned,
            || {
                drop(stream.take());
            },
        );
        // If the pump fails before `on_stop`, dropping the stream stops capture.
        drop(stream);
        outcome
    }

    fn build_input_stream(
        device: &Device,
        config: &StreamConfig,
        sample_format: SampleFormat,
        channels: usize,
        raw: Arc<Mutex<Vec<f32>>>,
        peak_bits: Arc<AtomicU32>,
    ) -> Result<Stream> {
        match sample_format {
            SampleFormat::F32 => {
                let raw_cb = raw;
                let peak_cb = peak_bits;
                Ok(device.build_input_stream(
                    config,
                    move |data: &[f32], _| {
                        update_peak(&peak_cb, data);
                        if let Ok(mut buf) = raw_cb.lock() {
                            push_downmix(&mut buf, data, channels);
                        }
                    },
                    |err| log::warn!("Dictation mic stream error: {err}"),
                    None,
                )?)
            }
            SampleFormat::I16 => {
                let raw_cb = raw;
                let peak_cb = peak_bits;
                Ok(device.build_input_stream(
                    config,
                    move |data: &[i16], _| {
                        let f: Vec<f32> = data.iter().map(|s| *s as f32 / 32768.0).collect();
                        update_peak(&peak_cb, &f);
                        if let Ok(mut buf) = raw_cb.lock() {
                            push_downmix(&mut buf, &f, channels);
                        }
                    },
                    |err| log::warn!("Dictation mic stream error: {err}"),
                    None,
                )?)
            }
            other => Err(anyhow!("Unsupported mic sample format: {other:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    #[test]
    fn abandon_silent_non_builtin_after_threshold() {
        assert!(should_abandon_silent_input(0.0, SILENCE_ABANDON_MS, false));
        assert!(should_abandon_silent_input(
            DIGITAL_SILENCE_PEAK * 0.5,
            SILENCE_ABANDON_MS + 50,
            false
        ));
    }

    #[test]
    fn keep_silent_builtin() {
        assert!(!should_abandon_silent_input(
            0.0,
            SILENCE_ABANDON_MS * 2,
            true
        ));
    }

    #[test]
    fn keep_live_non_builtin() {
        assert!(!should_abandon_silent_input(
            0.01,
            SILENCE_ABANDON_MS * 2,
            false
        ));
    }

    #[test]
    fn wait_before_abandon() {
        assert!(!should_abandon_silent_input(
            0.0,
            SILENCE_ABANDON_MS - 1,
            false
        ));
    }

    fn resampled_tail_len(sample_count: usize, input_rate: u32) -> usize {
        let mut resampler = make_resampler(input_rate).expect("resampler");
        let raw = Arc::new(Mutex::new(vec![0.2f32; sample_count]));
        let out = Arc::new(Mutex::new(Vec::new()));
        flush_resampler_tail(&raw, 0, Some(&mut resampler), input_rate, &out).expect("tail flush");
        let len = out.lock().expect("out lock").len();
        len
    }

    #[test]
    fn exact_chunk_tail_keeps_sinc_delay_only() {
        let mut baseline = make_resampler(48_000).expect("resampler");
        let processed = baseline
            .process(&vec![vec![0.2f32; RESAMPLE_CHUNK]], None)
            .expect("process");
        let full_len = processed[0].len();
        let delay = baseline.output_delay();
        let via_tail = resampled_tail_len(RESAMPLE_CHUNK, 48_000);
        let extra = via_tail - full_len;
        assert!(
            extra >= delay,
            "delay flush missing: extra={extra} delay={delay} full={full_len} tail={via_tail}"
        );
        assert!(
            extra <= delay.saturating_add(2),
            "tail kept the zero pad: extra={extra} delay={delay}"
        );
        assert!(extra < RESAMPLE_CHUNK / 2);
    }

    #[test]
    fn partial_tail_keeps_remainder_not_padded_chunk() {
        let one_chunk = resampled_tail_len(RESAMPLE_CHUNK, 48_000);
        let with_remainder = resampled_tail_len(RESAMPLE_CHUNK + 100, 48_000);
        let added = with_remainder - one_chunk;
        let expected = ((100_f64) * ASR_SAMPLE_RATE as f64 / 48_000_f64).ceil() as usize;
        assert!(
            added >= expected,
            "remainder was dropped: added={added} expected={expected}"
        );
        assert!(
            added <= expected + 2,
            "remainder kept a padded chunk: added={added} expected={expected}"
        );
    }

    #[test]
    fn near_full_remainder_flushes_past_one_block() {
        let input_rate = 48_000;
        let count = RESAMPLE_CHUNK - 1;
        let mut baseline = make_resampler(input_rate).expect("resampler");
        let mut padded = vec![0.2f32; count];
        padded.resize(RESAMPLE_CHUNK, 0.0);
        let processed = baseline.process(&vec![padded], None).expect("process");
        let one_len = processed[0].len();
        let delay = baseline.output_delay();
        let expected = proportional_samples(count, input_rate);
        assert!(
            delay + expected > one_len,
            "this input should overflow one block: delay={delay} expected={expected} one={one_len}"
        );
        let via = resampled_tail_len(count, input_rate);
        assert!(
            via + 2 >= expected,
            "near-full tail was capped at one block: via={via} expected={expected} one={one_len}"
        );
    }

    #[test]
    fn retry_matches_single_flush_of_combined_audio() {
        let input_rate = 48_000;
        let together = resampled_tail_len(160, input_rate);
        let raw = Arc::new(Mutex::new(vec![0.2f32; 100]));
        let out = Arc::new(Mutex::new(Vec::new()));
        let mut resampler = make_resampler(input_rate).expect("resampler");
        let raw_for_append = Arc::clone(&raw);
        flush_tail_with_one_retry(&raw, 0, Some(&mut resampler), input_rate, &out, || {
            raw_for_append
                .lock()
                .expect("raw lock")
                .extend(std::iter::repeat(0.2).take(60));
        })
        .expect("retry flush");
        let split = out.lock().expect("out lock").len();
        assert!(
            (split as i32 - together as i32).abs() <= 2,
            "retry diverged from one flush: split={split} together={together}"
        );
    }

    #[test]
    fn native_rate_tail_uses_raw_cursor_not_output_length() {
        let raw_samples: Vec<f32> = (0..80).map(|i| i as f32).collect();
        let raw = Arc::new(Mutex::new(raw_samples.clone()));
        let preexisting: Vec<f32> = (0..50).map(|i| -((i + 1) as f32)).collect();
        let out = Arc::new(Mutex::new(preexisting.clone()));
        flush_resampler_tail(&raw, 10, None, ASR_SAMPLE_RATE, &out).expect("tail flush");
        let got = out.lock().expect("out lock").clone();
        let mut expected = preexisting;
        expected.extend_from_slice(&raw_samples[10..]);
        assert_eq!(got, expected);
    }

    #[test]
    fn tail_retry_includes_samples_appended_after_snapshot() {
        let raw = Arc::new(Mutex::new(vec![0.25f32; 30]));
        let out = Arc::new(Mutex::new(Vec::new()));
        let suffix = [0.5f32; 7];
        let raw_for_append = Arc::clone(&raw);
        flush_tail_with_one_retry(&raw, 0, None, ASR_SAMPLE_RATE, &out, || {
            raw_for_append
                .lock()
                .expect("raw lock")
                .extend_from_slice(&suffix);
        })
        .expect("retry flush");
        let got = out.lock().expect("out lock").clone();
        let mut expected = vec![0.25f32; 30];
        expected.extend_from_slice(&suffix);
        assert_eq!(got, expected);
    }

    #[test]
    fn fallback_reset_and_drain_never_see_stale_cursor() {
        let out = Arc::new(Mutex::new(vec![0.1f32; 8_000]));
        let drained = Arc::new(Mutex::new(4_000usize));
        let start = Arc::new(AtomicBool::new(false));

        let producer = {
            let out = Arc::clone(&out);
            let drained = Arc::clone(&drained);
            let start = Arc::clone(&start);
            thread::spawn(move || {
                while !start.load(Ordering::Acquire) {
                    thread::yield_now();
                }
                for i in 0..10_000 {
                    if i % 2 == 0 {
                        clear_output_for_fallback(&out, &drained);
                    }
                    if let Ok(mut buf) = out.lock() {
                        buf.extend(std::iter::repeat(0.25).take(64));
                    }
                }
            })
        };

        let consumer = {
            let out = Arc::clone(&out);
            let drained = Arc::clone(&drained);
            let start = Arc::clone(&start);
            thread::spawn(move || {
                while !start.load(Ordering::Acquire) {
                    thread::yield_now();
                }
                for _ in 0..10_000 {
                    let buf = out.lock().expect("out lock");
                    let mut cursor = drained.lock().expect("drained lock");
                    assert!(
                        *cursor <= buf.len(),
                        "drained {} exceeds len {}",
                        *cursor,
                        buf.len()
                    );
                    let _chunk = take_undrained(&buf, &mut cursor);
                }
            })
        };

        start.store(true, Ordering::Release);
        producer.join().expect("producer");
        consumer.join().expect("consumer");

        clear_output_for_fallback(&out, &drained);
        assert!(out.lock().expect("out lock").is_empty());
        assert_eq!(*drained.lock().expect("drained lock"), 0);
    }

    #[test]
    fn fallback_reset_clears_poison() {
        let out = Arc::new(Mutex::new(vec![1.0f32; 4]));
        let drained = Arc::new(Mutex::new(3usize));
        let out_thread = Arc::clone(&out);
        let joined = thread::spawn(move || {
            let _guard = out_thread.lock().expect("out lock");
            panic!("poison output buffer");
        })
        .join();
        assert!(joined.is_err());
        clear_output_for_fallback(&out, &drained);
        assert!(out.lock().is_ok());
        assert!(out.lock().expect("out lock").is_empty());
        assert_eq!(*drained.lock().expect("drained lock"), 0);
    }
}

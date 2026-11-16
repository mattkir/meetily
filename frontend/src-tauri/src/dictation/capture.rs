//! Mic-only capture for dictation (never mixes system audio).
//! Supports streaming drain of resampled PCM while holding Fn.

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::ASR_SAMPLE_RATE;

const RESAMPLE_CHUNK: usize = 1024;

/// Collect microphone audio until `stop` is set, streaming resampled PCM @ 16 kHz.
pub struct MicCapture {
    stop: Arc<AtomicBool>,
    /// Resampled mono f32 @ 16 kHz (grows during capture).
    samples: Arc<Mutex<Vec<f32>>>,
    /// How many resampled samples have been drained for streaming.
    drained: Arc<Mutex<usize>>,
    /// Wall time spent in the final resample flush (set on finish).
    resample_ms: Arc<Mutex<u64>>,
    join: Option<thread::JoinHandle<Result<()>>>,
}

impl MicCapture {
    pub fn start() -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(Mutex::new(Vec::new()));
        let drained = Arc::new(Mutex::new(0usize));
        let resample_ms = Arc::new(Mutex::new(0u64));

        let stop_t = stop.clone();
        let samples_t = samples.clone();
        let resample_ms_t = resample_ms.clone();

        let join = thread::Builder::new()
            .name("dictation-mic".into())
            .spawn(move || run_capture(stop_t, samples_t, resample_ms_t))?;

        Ok(Self {
            stop,
            samples,
            drained,
            resample_ms,
            join: Some(join),
        })
    }

    /// Take newly available resampled samples since the last drain (streaming).
    pub fn drain_new(&self) -> Result<Vec<f32>> {
        let buf = self
            .samples
            .lock()
            .map_err(|_| anyhow!("samples lock"))?;
        let mut drained = self
            .drained
            .lock()
            .map_err(|_| anyhow!("drained lock"))?;
        if *drained >= buf.len() {
            return Ok(Vec::new());
        }
        let chunk = buf[*drained..].to_vec();
        *drained = buf.len();
        Ok(chunk)
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
    pub fn finish(mut self, grace_ms: u64) -> Result<(Vec<f32>, u64)> {
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
        log::info!(
            "Dictation capture finished: {} samples (~{:.0} ms @ 16kHz) resample_ms={}",
            pcm.len(),
            pcm.len() as f32 / ASR_SAMPLE_RATE as f32 * 1000.0,
            resample_ms
        );
        Ok((pcm, resample_ms))
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
    resample_ms: Arc<Mutex<u64>>,
) -> Result<()> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("No default microphone"))?;

    let supported = device.default_input_config()?;
    let sample_format = supported.sample_format();
    let channels = supported.channels() as usize;
    let input_rate = supported.sample_rate().0;

    let config: StreamConfig = supported.clone().into();
    log::info!(
        "Dictation mic-only capture: rate={} ch={} format={:?}",
        input_rate,
        channels,
        sample_format
    );

    let raw: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let raw_cb = raw.clone();

    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _| {
                if let Ok(mut buf) = raw_cb.lock() {
                    push_downmix(&mut buf, data, channels);
                }
            },
            |err| log::warn!("Dictation mic stream error: {err}"),
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _| {
                if let Ok(mut buf) = raw_cb.lock() {
                    let f: Vec<f32> = data.iter().map(|s| *s as f32 / 32768.0).collect();
                    push_downmix(&mut buf, &f, channels);
                }
            },
            |err| log::warn!("Dictation mic stream error: {err}"),
            None,
        )?,
        other => return Err(anyhow!("Unsupported mic sample format: {other:?}")),
    };

    stream.play()?;

    let mut resampler = if input_rate == ASR_SAMPLE_RATE {
        None
    } else {
        Some(make_resampler(input_rate)?)
    };
    let mut raw_cursor = 0usize;
    let mut total_resample_ms = 0u64;

    while !stop.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(20));
        let flushed = flush_raw_to_out(
            &raw,
            &mut raw_cursor,
            resampler.as_mut(),
            input_rate,
            &out,
        )?;
        total_resample_ms = total_resample_ms.saturating_add(flushed);
    }

    // Drop stream to flush hardware buffer, then final resample.
    drop(stream);
    thread::sleep(Duration::from_millis(15));
    let flushed = flush_raw_to_out(
        &raw,
        &mut raw_cursor,
        resampler.as_mut(),
        input_rate,
        &out,
    )?;
    total_resample_ms = total_resample_ms.saturating_add(flushed);

    // Pad remainder through resampler.
    if let Some(ref mut r) = resampler {
        let t0 = Instant::now();
        if let Ok(raw_buf) = raw.lock() {
            if raw_cursor < raw_buf.len() {
                let mut tail = vec![0.0f32; RESAMPLE_CHUNK];
                let rem = &raw_buf[raw_cursor..];
                tail[..rem.len()].copy_from_slice(rem);
                let chunk = vec![tail];
                if let Ok(processed) = r.process(&chunk, None) {
                    let keep = ((rem.len() as f64) * ASR_SAMPLE_RATE as f64 / input_rate as f64)
                        .ceil() as usize;
                    if let Ok(mut out_lock) = out.lock() {
                        out_lock.extend_from_slice(&processed[0][..keep.min(processed[0].len())]);
                    }
                }
            }
        }
        total_resample_ms = total_resample_ms.saturating_add(t0.elapsed().as_millis() as u64);
    } else if let Ok(raw_buf) = raw.lock() {
        // Same rate: copy any remainder not yet mirrored.
        if let Ok(mut out_lock) = out.lock() {
            let already = out_lock.len();
            if already < raw_buf.len() {
                out_lock.extend_from_slice(&raw_buf[already..]);
            }
        }
    }

    if let Ok(mut ms) = resample_ms.lock() {
        *ms = total_resample_ms;
    }
    Ok(())
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

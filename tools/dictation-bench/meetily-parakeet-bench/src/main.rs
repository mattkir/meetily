mod model;

use hound::WavReader;
use model::ParakeetModel;
use std::env;
use std::path::PathBuf;
use std::time::Instant;

fn load_wav_f32(path: &str) -> Result<Vec<f32>, String> {
    let mut reader = WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    if spec.sample_rate != 16_000 {
        return Err(format!(
            "{}: expected 16 kHz, got {}",
            path, spec.sample_rate
        ));
    }
    let samples: Result<Vec<f32>, _> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect(),
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / i16::MAX as f32))
            .collect(),
    };
    let mut samples = samples.map_err(|e| e.to_string())?;
    if spec.channels > 1 {
        let ch = spec.channels as usize;
        samples = samples
            .chunks(ch)
            .map(|frame| frame.iter().sum::<f32>() / ch as f32)
            .collect();
    }
    Ok(samples)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let idx = ((p / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn main() -> Result<(), String> {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() {
        return Err(
            "Usage: meetily-parakeet-bench --model-dir DIR [--runs N] <wav>...".into(),
        );
    }

    let mut model_dir = None;
    let mut runs = 5usize;
    let mut files = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--model-dir" && i + 1 < args.len() {
            model_dir = Some(PathBuf::from(&args[i + 1]));
            i += 2;
        } else if args[i] == "--runs" && i + 1 < args.len() {
            runs = args[i + 1].parse().map_err(|e| format!("{e}"))?;
            i += 2;
        } else {
            files.push(args[i].clone());
            i += 1;
        }
    }
    let model_dir = model_dir.ok_or("missing --model-dir")?;
    if files.is_empty() {
        return Err("no wav files".into());
    }

    eprintln!("Loading Meetily-compatible Parakeet CPU ONNX from {}", model_dir.display());
    let load_start = Instant::now();
    let mut model = ParakeetModel::new(&model_dir, true).map_err(|e| e.to_string())?;
    eprintln!("Model load: {:.1} ms", load_start.elapsed().as_secs_f64() * 1000.0);

    // Discarded warm-up on first file.
    let warm_samples = load_wav_f32(&files[0])?;
    let _ = model
        .transcribe_samples(warm_samples)
        .map_err(|e| e.to_string())?;

    let mut warm_ms = Vec::new();
    for path in &files {
        let samples = load_wav_f32(path)?;
        for run in 0..runs {
            let audio = samples.clone();
            let start = Instant::now();
            let result = model.transcribe_samples(audio).map_err(|e| e.to_string())?;
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            warm_ms.push(ms);
            println!(
                "PARAKEET_CPU\t{}\trun={}\twarm=yes\t{:.1}ms\t{}",
                PathBuf::from(path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(path),
                run + 1,
                ms,
                result.text.replace('\t', " ").trim()
            );
        }
    }

    warm_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "PARAKEET_CPU_SUMMARY\twarm_n={}\tp50_ms={:.1}\tmin_ms={:.1}\tmax_ms={:.1}",
        warm_ms.len(),
        percentile(&warm_ms, 50.0),
        warm_ms.first().copied().unwrap_or(f64::NAN),
        warm_ms.last().copied().unwrap_or(f64::NAN)
    );
    Ok(())
}

import Foundation
import FluidAudio

struct TimedResult {
    let file: String
    let text: String
    let latencyMs: Double
}

func defaultModelDir() -> URL {
    FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent(
            "Library/Application Support/FluidAudio/Models/parakeet-tdt-0.6b-v3-coreml"
        )
}

func loadModels(modelDir: URL) async throws -> AsrModels {
    if AsrModels.modelsExist(at: modelDir) {
        fputs("Loading Parakeet v3 Core ML from \(modelDir.path)\n", stderr)
        return try await AsrModels.load(
            from: modelDir,
            configuration: AsrModels.defaultConfiguration(),
            version: .v3
        )
    }
    fputs("Local Core ML bundle incomplete; calling downloadAndLoad(version: .v3)\n", stderr)
    return try await AsrModels.downloadAndLoad(version: .v3)
}

func run(paths: [String], modelDir: URL, runs: Int) async throws {
    let models = try await loadModels(modelDir: modelDir)
    let asr = AsrManager(config: .default)
    try await asr.loadModels(models)

    var decoderState = TdtDecoderState.make()

    // Discarded warm-up so timed runs are warm.
    if let first = paths.first {
        decoderState = TdtDecoderState.make()
        _ = try await asr.transcribe(URL(fileURLWithPath: first), decoderState: &decoderState)
    }

    var all: [TimedResult] = []
    for path in paths {
        let url = URL(fileURLWithPath: path)
        for i in 0..<runs {
            decoderState = TdtDecoderState.make()
            let started = ContinuousClock.now
            let result = try await asr.transcribe(url, decoderState: &decoderState)
            let elapsed = started.duration(to: .now)
            let ms = Double(elapsed.components.seconds) * 1000.0
                + Double(elapsed.components.attoseconds) / 1e15
            let timed = TimedResult(
                file: url.lastPathComponent,
                text: result.text.trimmingCharacters(in: .whitespacesAndNewlines),
                latencyMs: ms
            )
            all.append(timed)
            print(
                String(
                    format: "FLUID\t%@\trun=%d\twarm=yes\t%.1fms\t%@",
                    timed.file,
                    i + 1,
                    timed.latencyMs,
                    timed.text.replacingOccurrences(of: "\t", with: " ")
                )
            )
        }
    }

    let warm = all.map(\.latencyMs).sorted()
    if !warm.isEmpty {
        let p50 = warm[warm.count / 2]
        print(
            String(
                format: "FLUID_SUMMARY\twarm_n=%d\tp50_ms=%.1f\tmin_ms=%.1f\tmax_ms=%.1f",
                warm.count,
                p50,
                warm.first!,
                warm.last!
            )
        )
    }

    await asr.cleanup()
}

let args = Array(CommandLine.arguments.dropFirst())
guard !args.isEmpty else {
    fputs("Usage: FluidAudioProbe [--model-dir PATH] [--runs 5] <wav> [wav...]\n", stderr)
    exit(2)
}

var modelDir = defaultModelDir()
var runs = 5
var files: [String] = []
var i = 0
while i < args.count {
    let arg = args[i]
    if arg == "--model-dir", i + 1 < args.count {
        modelDir = URL(fileURLWithPath: args[i + 1], isDirectory: true)
        i += 2
    } else if arg == "--runs", i + 1 < args.count {
        runs = Int(args[i + 1]) ?? 5
        i += 2
    } else {
        files.append(arg)
        i += 1
    }
}

do {
    try await run(paths: files, modelDir: modelDir, runs: runs)
} catch {
    fputs("ERROR: \(error)\n", stderr)
    exit(1)
}

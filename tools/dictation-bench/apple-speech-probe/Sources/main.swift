import AVFoundation
import Foundation
import Speech

@available(macOS 26.0, *)
struct TimedResult: Codable {
    let file: String
    let text: String
    let latencyMs: Double
    let warm: Bool
}

@available(macOS 26.0, *)
enum ProbeError: Error, CustomStringConvertible {
    case unavailable
    case unsupportedLocale(String)
    case noAudioFormat
    case analyzeFailed(String)

    var description: String {
        switch self {
        case .unavailable:
            return "SpeechTranscriber.isAvailable == false"
        case .unsupportedLocale(let id):
            return "No supported SpeechTranscriber locale for \(id)"
        case .noAudioFormat:
            return "SpeechAnalyzer.bestAvailableAudioFormat returned nil"
        case .analyzeFailed(let message):
            return message
        }
    }
}

@available(macOS 26.0, *)
func prepareTranscriber(locale: Locale) async throws -> SpeechTranscriber {
    guard SpeechTranscriber.isAvailable else {
        throw ProbeError.unavailable
    }
    guard let supported = await SpeechTranscriber.supportedLocale(equivalentTo: locale) else {
        throw ProbeError.unsupportedLocale(locale.identifier)
    }

    let transcriber = SpeechTranscriber(locale: supported, preset: .transcription)
    if let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) {
        fputs("Installing SpeechTranscriber assets for \(supported.identifier)...\n", stderr)
        try await request.downloadAndInstall()
    }
    return transcriber
}

@available(macOS 26.0, *)
func transcribeFile(url: URL, locale: Locale, warm: Bool) async throws -> TimedResult {
    let transcriber = try await prepareTranscriber(locale: locale)

    async let collected: AttributedString = {
        var text = AttributedString()
        for try await result in transcriber.results {
            text += result.text
        }
        return text
    }()

    let analyzer = SpeechAnalyzer(modules: [transcriber])
    let file = try AVAudioFile(forReading: url)

    let started = ContinuousClock.now
    if let last = try await analyzer.analyzeSequence(from: file) {
        try await analyzer.finalizeAndFinish(through: last)
    } else {
        await analyzer.cancelAndFinishNow()
    }
    let elapsed = started.duration(to: .now)
    let text = String((try await collected).characters)
        .trimmingCharacters(in: .whitespacesAndNewlines)

    let ms = Double(elapsed.components.seconds) * 1000.0
        + Double(elapsed.components.attoseconds) / 1e15
    return TimedResult(file: url.lastPathComponent, text: text, latencyMs: ms, warm: warm)
}

@available(macOS 26.0, *)
func run(paths: [String], localeIdentifier: String, runs: Int) async throws {
    let locale = Locale(identifier: localeIdentifier)

    // One cold prep / asset install outside timed loop when possible.
    _ = try await prepareTranscriber(locale: locale)

    var all: [TimedResult] = []
    for path in paths {
        let url = URL(fileURLWithPath: path)
        for i in 0..<runs {
            let result = try await transcribeFile(url: url, locale: locale, warm: i > 0)
            all.append(result)
            print(
                String(
                    format: "APPLE\t%@\trun=%d\twarm=%@\t%.1fms\t%@",
                    result.file,
                    i + 1,
                    result.warm ? "yes" : "no",
                    result.latencyMs,
                    result.text.replacingOccurrences(of: "\t", with: " ")
                )
            )
        }
    }

    let warm = all.filter(\.warm).map(\.latencyMs).sorted()
    if !warm.isEmpty {
        let p50 = warm[warm.count / 2]
        print(String(format: "APPLE_SUMMARY\twarm_n=%d\tp50_ms=%.1f\tmin_ms=%.1f\tmax_ms=%.1f",
                     warm.count, p50, warm.first!, warm.last!))
    }
}

let args = Array(CommandLine.arguments.dropFirst())
guard !args.isEmpty else {
    fputs(
        """
        Usage: AppleSpeechProbe [--locale en-US] [--runs 5] <wav> [wav...]
        """,
        stderr
    )
    exit(2)
}

var locale = "en-US"
var runs = 5
var files: [String] = []
var i = 0
while i < args.count {
    let arg = args[i]
    if arg == "--locale", i + 1 < args.count {
        locale = args[i + 1]
        i += 2
    } else if arg == "--runs", i + 1 < args.count {
        runs = Int(args[i + 1]) ?? 5
        i += 2
    } else {
        files.append(arg)
        i += 1
    }
}

if #available(macOS 26.0, *) {
    do {
        try await run(paths: files, localeIdentifier: locale, runs: runs)
    } catch {
        fputs("ERROR: \(error)\n", stderr)
        exit(1)
    }
} else {
    fputs("SpeechTranscriber requires macOS 26+\n", stderr)
    exit(1)
}

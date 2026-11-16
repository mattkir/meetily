import AVFoundation
import Foundation
import Speech

/// Apple SpeechTranscriber / SpeechAnalyzer engine (default).
@available(macOS 26.0, *)
actor AppleEngine {
    private var locale: Locale = Locale(identifier: "en-US")
    private var warmed = false
    private var liveStream: LiveStream?

    private struct LiveStream {
        let requestId: String
        let writer: ResponseWriter
        let continuation: AsyncStream<AnalyzerInput>.Continuation
        let collectTask: Task<String, Error>
        let analyzerTask: Task<Void, Never>
        let analyzer: SpeechAnalyzer
        let transcriber: SpeechTranscriber
        let targetFormat: AVAudioFormat
    }

    func load() async throws {
        guard SpeechTranscriber.isAvailable else {
            throw ProtocolError.engine("SpeechTranscriber.isAvailable == false")
        }
        guard let supported = await SpeechTranscriber.supportedLocale(equivalentTo: locale)
        else {
            throw ProtocolError.engine("No supported SpeechTranscriber locale for \(locale.identifier)")
        }
        locale = supported
        let transcriber = makeTranscriber()
        if let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber])
        {
            Log.info("installing SpeechTranscriber assets for \(supported.identifier)")
            try await request.downloadAndInstall()
        }
        Log.info("apple engine loaded locale=\(supported.identifier)")
    }

    func warm() async throws {
        let transcriber = makeTranscriber()
        let options = SpeechAnalyzer.Options(
            priority: .userInitiated,
            modelRetention: .processLifetime
        )
        let analyzer = SpeechAnalyzer(modules: [transcriber], options: options)
        _ = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber])
        let silence = [Float](repeating: 0, count: Int(AudioCodec.targetSampleRate * 0.25))
        _ = try await analyze(
            samples: silence,
            requestId: nil,
            writer: nil,
            analyzer: analyzer,
            transcriber: transcriber
        )
        warmed = true
        Log.info("apple engine warm complete")
    }

    func transcribe(
        samples: [Float],
        requestId: String,
        writer: ResponseWriter
    ) async throws -> String {
        let transcriber = makeTranscriber()
        let options = SpeechAnalyzer.Options(
            priority: .userInitiated,
            modelRetention: warmed ? .processLifetime : .lingering
        )
        let analyzer = SpeechAnalyzer(modules: [transcriber], options: options)
        return try await analyze(
            samples: samples,
            requestId: requestId,
            writer: writer,
            analyzer: analyzer,
            transcriber: transcriber
        )
    }

    /// Begin a live hold-to-talk stream that emits partials as PCM arrives.
    func streamStart(requestId: String, writer: ResponseWriter) async throws {
        if liveStream != nil {
            await streamCancel()
        }

        let transcriber = makeTranscriber()
        let options = SpeechAnalyzer.Options(
            priority: .userInitiated,
            modelRetention: warmed ? .processLifetime : .lingering
        )
        let analyzer = SpeechAnalyzer(modules: [transcriber], options: options)
        let targetFormat =
            await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber])
            ?? AVAudioFormat(
                commonFormat: .pcmFormatFloat32,
                sampleRate: AudioCodec.targetSampleRate,
                channels: 1,
                interleaved: false
            )!

        let (inputStream, continuation) = AsyncStream.makeStream(
            of: AnalyzerInput.self,
            bufferingPolicy: .bufferingNewest(8)
        )

        let collectTask = Task { () -> String in
            var finalText = AttributedString()
            var lastPartial = ""
            for try await result in transcriber.results {
                let chunk = String(result.text.characters)
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                if result.isFinal {
                    finalText += result.text
                } else if !chunk.isEmpty, chunk != lastPartial {
                    lastPartial = chunk
                    await writer.emit(.partial(id: requestId, text: chunk))
                }
            }
            let text = String(finalText.characters)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            if text.isEmpty {
                return lastPartial
            }
            return text
        }

        // Start analyzer consumption in the background while PCM chunks arrive.
        let analyzerTask = Task {
            do {
                if let last = try await analyzer.analyzeSequence(inputStream) {
                    try await analyzer.finalizeAndFinish(through: last)
                } else {
                    await analyzer.cancelAndFinishNow()
                }
            } catch {
                Log.info("apple live analyzer ended: \(error.localizedDescription)")
            }
        }

        liveStream = LiveStream(
            requestId: requestId,
            writer: writer,
            continuation: continuation,
            collectTask: collectTask,
            analyzerTask: analyzerTask,
            analyzer: analyzer,
            transcriber: transcriber,
            targetFormat: targetFormat
        )
        Log.info("apple stream started id=\(requestId)")
    }

    func streamAudio(samples: [Float]) async throws {
        guard let stream = liveStream else { throw ProtocolError.noActiveStream }
        guard !samples.isEmpty else { return }
        let sourceBuffer = try AudioCodec.makePCMBuffer(samples: samples)
        let inputBuffer = try AudioCodec.convert(buffer: sourceBuffer, to: stream.targetFormat)
        stream.continuation.yield(AnalyzerInput(buffer: inputBuffer))
    }

    func streamFinalize(trailing: [Float]) async throws -> String {
        guard let stream = liveStream else { throw ProtocolError.noActiveStream }
        if !trailing.isEmpty {
            try await streamAudio(samples: trailing)
        }
        stream.continuation.finish()
        liveStream = nil

        do {
            await stream.analyzerTask.value
            let text = try await stream.collectTask.value
            Log.info("apple stream final chars=\(text.count)")
            return text
        } catch {
            await stream.analyzer.cancelAndFinishNow()
            throw ProtocolError.engine("SpeechAnalyzer stream failed: \(error.localizedDescription)")
        }
    }

    func streamCancel() async {
        guard let stream = liveStream else { return }
        stream.continuation.finish()
        stream.collectTask.cancel()
        stream.analyzerTask.cancel()
        await stream.analyzer.cancelAndFinishNow()
        liveStream = nil
        Log.info("apple stream cancelled")
    }

    func unload() async {
        await streamCancel()
        warmed = false
        Log.info("apple engine unloaded")
    }

    private func makeTranscriber() -> SpeechTranscriber {
        SpeechTranscriber(
            locale: locale,
            transcriptionOptions: [],
            reportingOptions: [.volatileResults, .fastResults],
            attributeOptions: []
        )
    }

    private func analyze(
        samples: [Float],
        requestId: String?,
        writer: ResponseWriter?,
        analyzer: SpeechAnalyzer,
        transcriber: SpeechTranscriber
    ) async throws -> String {
        guard !samples.isEmpty else { return "" }

        let sourceBuffer = try AudioCodec.makePCMBuffer(samples: samples)
        let targetFormat =
            await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber])
            ?? sourceBuffer.format
        let inputBuffer = try AudioCodec.convert(buffer: sourceBuffer, to: targetFormat)

        let collectTask = Task { () -> String in
            var finalText = AttributedString()
            var lastPartial = ""
            for try await result in transcriber.results {
                let chunk = String(result.text.characters)
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                if result.isFinal {
                    finalText += result.text
                } else if !chunk.isEmpty, chunk != lastPartial {
                    lastPartial = chunk
                    if let requestId, let writer {
                        await writer.emit(.partial(id: requestId, text: chunk))
                    }
                }
            }
            let text = String(finalText.characters)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            if text.isEmpty {
                return lastPartial
            }
            return text
        }

        do {
            if let last = try await analyzer.analyzeSequence(
                AsyncStream<AnalyzerInput> { continuation in
                    continuation.yield(AnalyzerInput(buffer: inputBuffer))
                    continuation.finish()
                }
            ) {
                try await analyzer.finalizeAndFinish(through: last)
            } else {
                await analyzer.cancelAndFinishNow()
            }
        } catch {
            collectTask.cancel()
            throw ProtocolError.engine("SpeechAnalyzer failed: \(error.localizedDescription)")
        }

        return try await collectTask.value
    }
}

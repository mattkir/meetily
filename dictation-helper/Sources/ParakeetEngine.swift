import FluidAudio
import Foundation

/// FluidAudio Parakeet TDT v3 Core ML engine (opt-in).
actor ParakeetEngine {
    private var manager: AsrManager?
    private var modelDir: URL

    init() {
        self.modelDir = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(
                "Library/Application Support/FluidAudio/Models/parakeet-tdt-0.6b-v3-coreml",
                isDirectory: true
            )
        if let override = ProcessInfo.processInfo.environment["MEETILY_PARAKEET_COREML_DIR"],
            !override.isEmpty
        {
            self.modelDir = URL(fileURLWithPath: override, isDirectory: true)
        }
    }

    func load() async throws {
        Log.info("parakeet loading from \(modelDir.path)")
        let models: AsrModels
        if AsrModels.modelsExist(at: modelDir) {
            models = try await AsrModels.load(
                from: modelDir,
                configuration: AsrModels.defaultConfiguration(),
                version: .v3
            )
        } else {
            Log.info("local Core ML bundle incomplete; downloadAndLoad(version: .v3)")
            models = try await AsrModels.downloadAndLoad(version: .v3)
        }
        let asr = AsrManager(config: .default)
        try await asr.loadModels(models)
        manager = asr
        Log.info("parakeet engine loaded")
    }

    func warm() async throws {
        guard let asr = manager else { throw ProtocolError.notLoaded }
        var decoderState = TdtDecoderState.make(decoderLayers: await asr.decoderLayerCount)
        // Short silence warm (≥0.3s FluidAudio minimum) — discard result; do not emit transcript.
        let silence = [Float](repeating: 0, count: Int(AudioCodec.targetSampleRate * 0.35))
        _ = try await asr.transcribe(silence, decoderState: &decoderState)
        Log.info("parakeet engine warm complete")
    }

    func transcribe(samples: [Float]) async throws -> String {
        guard let asr = manager else { throw ProtocolError.notLoaded }
        guard !samples.isEmpty else { return "" }
        var decoderState = TdtDecoderState.make(decoderLayers: await asr.decoderLayerCount)
        let result = try await asr.transcribe(samples, decoderState: &decoderState)
        return result.text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    func unload() async {
        if let asr = manager {
            await asr.cleanup()
        }
        manager = nil
        Log.info("parakeet engine unloaded")
    }
}

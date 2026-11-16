import Foundation

/// Owns the currently loaded engine and routes load/warm/transcribe/stream/unload.
actor EngineSession {
    private var kind: EngineKind?
    private var apple: AppleEngine?
    private var parakeet: ParakeetEngine?
    private var streamId: String?
    private var parakeetBuffer: [Float] = []

    var loadedEngine: EngineKind? { kind }

    func load(engineName: String) async throws -> EngineKind {
        guard let next = EngineKind(rawValue: engineName) else {
            throw ProtocolError.unsupportedEngine(engineName)
        }

        if kind == next {
            return next
        }

        try await unloadInternal()

        switch next {
        case .apple:
            let engine = AppleEngine()
            try await engine.load()
            apple = engine
        case .parakeetV3Coreml:
            let engine = ParakeetEngine()
            try await engine.load()
            parakeet = engine
        }

        kind = next
        return next
    }

    func warm() async throws {
        guard let kind else { throw ProtocolError.notLoaded }
        switch kind {
        case .apple:
            guard let apple else { throw ProtocolError.notLoaded }
            try await apple.warm()
        case .parakeetV3Coreml:
            guard let parakeet else { throw ProtocolError.notLoaded }
            try await parakeet.warm()
        }
    }

    func transcribe(
        samples: [Float],
        requestId: String,
        writer: ResponseWriter
    ) async throws -> String {
        guard let kind else { throw ProtocolError.notLoaded }
        switch kind {
        case .apple:
            guard let apple else { throw ProtocolError.notLoaded }
            return try await apple.transcribe(
                samples: samples,
                requestId: requestId,
                writer: writer
            )
        case .parakeetV3Coreml:
            guard let parakeet else { throw ProtocolError.notLoaded }
            // Parakeet batch path has no volatile partials; emit final only.
            return try await parakeet.transcribe(samples: samples)
        }
    }

    func streamStart(requestId: String, writer: ResponseWriter) async throws {
        guard let kind else { throw ProtocolError.notLoaded }
        streamId = requestId
        parakeetBuffer.removeAll(keepingCapacity: true)
        switch kind {
        case .apple:
            guard let apple else { throw ProtocolError.notLoaded }
            try await apple.streamStart(requestId: requestId, writer: writer)
        case .parakeetV3Coreml:
            // Buffer-only until finalize (no live partials).
            break
        }
    }

    func streamAudio(samples: [Float]) async throws {
        guard let kind, streamId != nil else { throw ProtocolError.noActiveStream }
        switch kind {
        case .apple:
            guard let apple else { throw ProtocolError.notLoaded }
            try await apple.streamAudio(samples: samples)
        case .parakeetV3Coreml:
            parakeetBuffer.append(contentsOf: samples)
        }
    }

    func streamFinalize(trailing: [Float]) async throws -> String {
        guard let kind, streamId != nil else { throw ProtocolError.noActiveStream }
        defer {
            streamId = nil
            parakeetBuffer.removeAll(keepingCapacity: false)
        }
        switch kind {
        case .apple:
            guard let apple else { throw ProtocolError.notLoaded }
            return try await apple.streamFinalize(trailing: trailing)
        case .parakeetV3Coreml:
            guard let parakeet else { throw ProtocolError.notLoaded }
            var all = parakeetBuffer
            all.append(contentsOf: trailing)
            return try await parakeet.transcribe(samples: all)
        }
    }

    func streamCancel() async {
        streamId = nil
        parakeetBuffer.removeAll(keepingCapacity: false)
        if let apple {
            await apple.streamCancel()
        }
    }

    func unload() async throws {
        try await unloadInternal()
    }

    private func unloadInternal() async throws {
        await streamCancel()
        if let apple {
            await apple.unload()
        }
        if let parakeet {
            await parakeet.unload()
        }
        apple = nil
        parakeet = nil
        kind = nil
    }
}

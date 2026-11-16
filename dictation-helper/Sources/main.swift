import Foundation

/// dictation-helper: line-delimited JSON ASR sidecar for Meetily.
/// Never logs full transcript text.

let writer = ResponseWriter()
let session = EngineSession()

Log.info("starting (stdin/stdout JSON lines)")

while let line = readLine(strippingNewline: true) {
    if line.isEmpty { continue }
    let command = parseCommand(line: line)

    switch command {
    case .load(let cmd):
        do {
            let engine = try await session.load(engineName: cmd.engine)
            await writer.emit(.ok(id: cmd.id, event: "loaded", engine: engine.rawValue))
            Log.info("loaded engine=\(engine.rawValue)")
        } catch {
            await writer.emit(.fail(id: cmd.id, error: "\(error)"))
            Log.info("load failed: \(error)")
        }

    case .warm(let cmd):
        do {
            try await session.warm()
            await writer.emit(.ok(id: cmd.id, event: "ready"))
            Log.info("warm ready")
        } catch {
            await writer.emit(.fail(id: cmd.id, error: "\(error)"))
            Log.info("warm failed: \(error)")
        }

    case .transcribe(let cmd):
        do {
            guard cmd.format == "f32le" else {
                throw ProtocolError.invalidAudio("format must be f32le")
            }
            let samples = try AudioCodec.decodeF32leBase64(cmd.pcmB64, sampleRate: cmd.sampleRate)
            Log.info("transcribe samples=\(samples.count) id=\(cmd.id)")
            let text = try await session.transcribe(
                samples: samples,
                requestId: cmd.id,
                writer: writer
            )
            await writer.emit(.final(id: cmd.id, text: text))
            Log.info("transcribe final chars=\(text.count) id=\(cmd.id)")
        } catch {
            await writer.emit(.fail(id: cmd.id, error: "\(error)"))
            Log.info("transcribe failed id=\(cmd.id): \(error)")
        }

    case .streamStart(let cmd):
        do {
            try await session.streamStart(requestId: cmd.id, writer: writer)
            await writer.emit(.ok(id: cmd.id, event: "stream_ready"))
            Log.info("stream_start id=\(cmd.id)")
        } catch {
            await writer.emit(.fail(id: cmd.id, error: "\(error)"))
            Log.info("stream_start failed: \(error)")
        }

    case .streamAudio(let cmd):
        do {
            guard cmd.format == "f32le" else {
                throw ProtocolError.invalidAudio("format must be f32le")
            }
            let samples = try AudioCodec.decodeF32leBase64(cmd.pcmB64, sampleRate: cmd.sampleRate)
            try await session.streamAudio(samples: samples)
            await writer.emit(.ok(id: cmd.id, event: "acked"))
        } catch {
            await writer.emit(.fail(id: cmd.id, error: "\(error)"))
            Log.info("stream_audio failed id=\(cmd.id): \(error)")
        }

    case .streamFinalize(let cmd):
        do {
            guard cmd.format == "f32le" else {
                throw ProtocolError.invalidAudio("format must be f32le")
            }
            let trailing: [Float]
            if cmd.pcmB64.isEmpty {
                trailing = []
            } else {
                trailing = try AudioCodec.decodeF32leBase64(cmd.pcmB64, sampleRate: cmd.sampleRate)
            }
            Log.info("stream_finalize trailing=\(trailing.count) id=\(cmd.id)")
            let text = try await session.streamFinalize(trailing: trailing)
            await writer.emit(.final(id: cmd.id, text: text))
            Log.info("stream_finalize final chars=\(text.count) id=\(cmd.id)")
        } catch {
            await writer.emit(.fail(id: cmd.id, error: "\(error)"))
            Log.info("stream_finalize failed id=\(cmd.id): \(error)")
        }

    case .unload(let cmd):
        do {
            try await session.unload()
            await writer.emit(.ok(id: cmd.id, event: "unloaded"))
            Log.info("unloaded")
        } catch {
            await writer.emit(.fail(id: cmd.id, error: "\(error)"))
        }

    case .ping(let cmd):
        await writer.emit(.ok(id: cmd.id, event: "pong"))

    case .unknown(let id, let raw):
        let responseId = id ?? "0"
        await writer.emit(.fail(id: responseId, error: "unknown cmd: \(raw)"))
        Log.info("unknown command")
    }
}

Log.info("stdin closed; exiting")
try? await session.unload()

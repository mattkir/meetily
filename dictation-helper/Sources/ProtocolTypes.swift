import Foundation

/// Wire protocol types for Meetily ↔ dictation-helper (see PROTOCOL.md).

enum EngineKind: String, Codable, Sendable {
    case apple
    case parakeetV3Coreml = "parakeet-v3-coreml"
}

struct LoadCommand: Decodable {
    let id: String
    let engine: String
}

struct IdOnlyCommand: Decodable {
    let id: String
}

struct TranscribeCommand: Decodable {
    let id: String
    let format: String
    let sampleRate: UInt32
    let pcmB64: String

    enum CodingKeys: String, CodingKey {
        case id, format
        case sampleRate = "sample_rate"
        case pcmB64 = "pcm_b64"
    }
}

struct StreamAudioCommand: Decodable {
    let id: String
    let format: String
    let sampleRate: UInt32
    let pcmB64: String

    enum CodingKeys: String, CodingKey {
        case id, format
        case sampleRate = "sample_rate"
        case pcmB64 = "pcm_b64"
    }
}

enum IncomingCommand {
    case load(LoadCommand)
    case warm(IdOnlyCommand)
    case transcribe(TranscribeCommand)
    case streamStart(IdOnlyCommand)
    case streamAudio(StreamAudioCommand)
    case streamFinalize(StreamAudioCommand)
    case unload(IdOnlyCommand)
    case ping(IdOnlyCommand)
    case unknown(id: String?, raw: String)
}

struct OutgoingResponse: Encodable {
    let id: String
    let ok: Bool
    var event: String?
    var text: String?
    var partial: Bool?
    var engine: String?
    var error: String?

    static func ok(id: String, event: String, engine: String? = nil) -> OutgoingResponse {
        OutgoingResponse(
            id: id,
            ok: true,
            event: event,
            text: nil,
            partial: nil,
            engine: engine,
            error: nil
        )
    }

    static func partial(id: String, text: String) -> OutgoingResponse {
        OutgoingResponse(
            id: id,
            ok: true,
            event: "partial",
            text: text,
            partial: true,
            engine: nil,
            error: nil
        )
    }

    static func final(id: String, text: String) -> OutgoingResponse {
        OutgoingResponse(
            id: id,
            ok: true,
            event: "final",
            text: text,
            partial: false,
            engine: nil,
            error: nil
        )
    }

    static func fail(id: String, error: String) -> OutgoingResponse {
        OutgoingResponse(
            id: id,
            ok: false,
            event: nil,
            text: nil,
            partial: nil,
            engine: nil,
            error: error
        )
    }
}

enum ProtocolError: Error, CustomStringConvertible {
    case missingId
    case unknownCommand(String)
    case unsupportedEngine(String)
    case invalidAudio(String)
    case notLoaded
    case noActiveStream
    case engine(String)

    var description: String {
        switch self {
        case .missingId:
            return "missing id"
        case .unknownCommand(let cmd):
            return "unknown cmd: \(cmd)"
        case .unsupportedEngine(let name):
            return "unsupported engine: \(name)"
        case .invalidAudio(let reason):
            return "invalid audio: \(reason)"
        case .notLoaded:
            return "no engine loaded"
        case .noActiveStream:
            return "no active stream"
        case .engine(let message):
            return message
        }
    }
}

func parseCommand(line: String) -> IncomingCommand {
    guard let data = line.data(using: .utf8),
          let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
          let cmd = obj["cmd"] as? String
    else {
        return .unknown(id: nil, raw: line)
    }

    let id = obj["id"] as? String
    let decoder = JSONDecoder()

    do {
        switch cmd {
        case "load":
            return .load(try decoder.decode(LoadCommand.self, from: data))
        case "warm":
            return .warm(try decoder.decode(IdOnlyCommand.self, from: data))
        case "transcribe":
            return .transcribe(try decoder.decode(TranscribeCommand.self, from: data))
        case "stream_start":
            return .streamStart(try decoder.decode(IdOnlyCommand.self, from: data))
        case "stream_audio":
            return .streamAudio(try decoder.decode(StreamAudioCommand.self, from: data))
        case "stream_finalize":
            return .streamFinalize(try decoder.decode(StreamAudioCommand.self, from: data))
        case "unload":
            return .unload(try decoder.decode(IdOnlyCommand.self, from: data))
        case "ping":
            return .ping(try decoder.decode(IdOnlyCommand.self, from: data))
        default:
            return .unknown(id: id, raw: cmd)
        }
    } catch {
        return .unknown(id: id, raw: cmd)
    }
}

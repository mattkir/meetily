import Foundation

/// Serializes JSON responses to stdout. Never logs transcript text to stderr.
actor ResponseWriter {
    private let encoder: JSONEncoder = {
        let enc = JSONEncoder()
        enc.outputFormatting = []
        return enc
    }()

    func emit(_ response: OutgoingResponse) {
        do {
            let data = try encoder.encode(response)
            FileHandle.standardOutput.write(data)
            FileHandle.standardOutput.write(Data([0x0A]))
            fflush(stdout)
        } catch {
            // Last-resort machine-readable failure without leaking request text.
            let fallback =
                #"{"id":"\#(response.id)","ok":false,"error":"encode failed"}"# + "\n"
            if let bytes = fallback.data(using: .utf8) {
                FileHandle.standardOutput.write(bytes)
                fflush(stdout)
            }
        }
    }
}

enum Log {
    /// Diagnostic only — never pass transcript content.
    static func info(_ message: String) {
        fputs("dictation-helper: \(message)\n", stderr)
        fflush(stderr)
    }
}

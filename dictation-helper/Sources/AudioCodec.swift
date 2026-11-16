import AVFoundation
import Foundation

enum AudioCodec {
    static let targetSampleRate: Double = 16_000

    /// Decode mono f32 little-endian PCM from base64.
    static func decodeF32leBase64(_ b64: String, sampleRate: UInt32) throws -> [Float] {
        guard sampleRate == UInt32(targetSampleRate) else {
            throw ProtocolError.invalidAudio("sample_rate must be 16000, got \(sampleRate)")
        }
        guard let data = Data(base64Encoded: b64, options: [.ignoreUnknownCharacters]) else {
            throw ProtocolError.invalidAudio("pcm_b64 is not valid base64")
        }
        guard data.count % MemoryLayout<Float>.size == 0 else {
            throw ProtocolError.invalidAudio("pcm byte length not multiple of 4")
        }
        let count = data.count / MemoryLayout<Float>.size
        var samples = [Float](repeating: 0, count: count)
        _ = samples.withUnsafeMutableBytes { dest in
            data.copyBytes(to: dest)
        }
        return samples
    }

    static func makePCMBuffer(samples: [Float], sampleRate: Double = targetSampleRate) throws
        -> AVAudioPCMBuffer
    {
        guard let format = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: sampleRate,
            channels: 1,
            interleaved: false
        ) else {
            throw ProtocolError.invalidAudio("failed to create AVAudioFormat")
        }
        guard let buffer = AVAudioPCMBuffer(
            pcmFormat: format,
            frameCapacity: AVAudioFrameCount(samples.count)
        ) else {
            throw ProtocolError.invalidAudio("failed to allocate AVAudioPCMBuffer")
        }
        buffer.frameLength = AVAudioFrameCount(samples.count)
        guard let channel = buffer.floatChannelData?[0] else {
            throw ProtocolError.invalidAudio("missing float channel data")
        }
        samples.withUnsafeBufferPointer { src in
            channel.update(from: src.baseAddress!, count: samples.count)
        }
        return buffer
    }

    /// Convert a mono float buffer into `targetFormat` if needed.
    static func convert(
        buffer: AVAudioPCMBuffer,
        to targetFormat: AVAudioFormat
    ) throws -> AVAudioPCMBuffer {
        if buffer.format == targetFormat {
            return buffer
        }
        guard let converter = AVAudioConverter(from: buffer.format, to: targetFormat) else {
            throw ProtocolError.invalidAudio("no converter \(buffer.format) → \(targetFormat)")
        }
        let ratio = targetFormat.sampleRate / buffer.format.sampleRate
        let capacity = AVAudioFrameCount(Double(buffer.frameLength) * ratio) + 32
        guard let out = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: capacity) else {
            throw ProtocolError.invalidAudio("failed to allocate converted buffer")
        }

        var error: NSError?
        var consumed = false
        let status = converter.convert(to: out, error: &error) { _, outStatus in
            if consumed {
                outStatus.pointee = .noDataNow
                return nil
            }
            consumed = true
            outStatus.pointee = .haveData
            return buffer
        }
        if status == .error {
            throw ProtocolError.invalidAudio(
                "convert failed: \(error?.localizedDescription ?? "unknown")"
            )
        }
        return out
    }
}

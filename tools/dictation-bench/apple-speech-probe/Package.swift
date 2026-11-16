// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "AppleSpeechProbe",
    platforms: [
        .macOS(.v26)
    ],
    targets: [
        .executableTarget(
            name: "AppleSpeechProbe",
            path: "Sources"
        )
    ]
)

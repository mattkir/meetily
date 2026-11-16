// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "DictationHelper",
    platforms: [
        .macOS(.v26)
    ],
    dependencies: [
        .package(
            url: "https://github.com/FluidInference/FluidAudio.git",
            from: "0.18.3"
        )
    ],
    targets: [
        .executableTarget(
            name: "dictation-helper",
            dependencies: [
                .product(name: "FluidAudio", package: "FluidAudio")
            ],
            path: "Sources"
        )
    ]
)

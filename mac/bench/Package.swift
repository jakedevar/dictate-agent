// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "MacSTTBench",
    platforms: [.macOS(.v14)],
    dependencies: [
        .package(url: "https://github.com/argmaxinc/WhisperKit.git", revision: "9f99b5cb6a907730543823ad271b82532782a899"),
        .package(url: "https://github.com/FluidInference/FluidAudio.git", revision: "c79c4106b954f81205113c3343e5a7e59a1089b5", traits: []),
    ],
    targets: [
        .target(name: "BenchSupport"),
        .executableTarget(name: "Prepare"),
        .executableTarget(name: "KitBench", dependencies: ["BenchSupport", .product(name: "WhisperKit", package: "WhisperKit")]),
        .executableTarget(name: "FluidBench", dependencies: ["BenchSupport", .product(name: "FluidAudio", package: "FluidAudio")]),
    ]
)

// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "DictateParakeet",
    platforms: [.macOS(.v14)],
    products: [.executable(name: "dictate-parakeet", targets: ["DictateParakeet"])],
    dependencies: [
        .package(url: "https://github.com/FluidInference/FluidAudio.git", revision: "c79c4106b954f81205113c3343e5a7e59a1089b5", traits: []),
    ],
    targets: [
        .target(name: "ParakeetProtocol"),
        .executableTarget(name: "UdsBench"),
        .executableTarget(name: "DictateParakeet", dependencies: ["ParakeetProtocol", .product(name: "FluidAudio", package: "FluidAudio")]),
        .testTarget(name: "ParakeetProtocolTests", dependencies: ["ParakeetProtocol"]),
    ]
)

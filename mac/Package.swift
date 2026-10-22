// swift-tools-version: 6.0
//
// DictateHub: a menu-bar dictation client for macOS. It records while a
// hotkey is held, uploads the clip to the hub's S33 network API
// (POST /v1/transcribe) and pastes the returned text at the cursor.
//
// DictateHubCore holds everything that can be tested headless (WAV encoding,
// request building, response parsing, the hotkey state machine, token and
// settings storage, pasteboard snapshots). DictateHub is the AppKit shell.
import PackageDescription

let package = Package(
    name: "DictateHub",
    platforms: [.macOS("15.0")],
    products: [
        .executable(name: "DictateHub", targets: ["DictateHub"]),
        .library(name: "DictateHubCore", targets: ["DictateHubCore"]),
    ],
    targets: [
        .target(name: "DictateHubCore"),
        .executableTarget(
            name: "DictateHub",
            dependencies: ["DictateHubCore"]
        ),
        .testTarget(
            name: "DictateHubCoreTests",
            dependencies: ["DictateHubCore"]
        ),
    ],
    swiftLanguageModes: [.v6]
)

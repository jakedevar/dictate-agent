import AppKit
import Foundation
import Testing
@testable import DictateHubCore

@Suite struct ClipAccumulatorTests {
    func ramp(_ range: Range<Int>) -> [Float] { range.map(Float.init) }

    @Test func coldClipHasNoPreRoll() {
        var acc = ClipAccumulator(preRollSamples: 0, maxSamples: 100)
        acc.append(ramp(0..<50))  // idle, no ring: dropped
        #expect(acc.preRollCount == 0)
        #expect(acc.beginClip() == 0)
        acc.append(ramp(50..<60))
        #expect(acc.endClip() == ramp(50..<60))
    }

    @Test func ringKeepsTheNewestSamplesInOrder() {
        var acc = ClipAccumulator(preRollSamples: 4, maxSamples: 100)
        acc.append(ramp(0..<3))
        #expect(acc.preRollCount == 3)
        acc.append(ramp(3..<10))  // wraps twice
        #expect(acc.preRollCount == 4)
        #expect(acc.beginClip() == 4)
        acc.append(ramp(10..<12))
        #expect(acc.endClip() == ramp(6..<12))  // pre-roll 6...9 then the clip
    }

    @Test func preRollIsSpokenWordAtKeyDown() {
        // 300 ms at 16 kHz: speech that started 0.2 s before the clip is kept.
        var acc = ClipAccumulator(preRollSamples: 4_800, maxSamples: 16_000 * 120)
        acc.append([Float](repeating: 0, count: 16_000))  // a second of silence
        acc.append([Float](repeating: 0.5, count: 3_200))  // 200 ms of "speech"
        #expect(acc.beginClip() == 4_800)
        let clip = acc.endClip()
        #expect(clip.count == 4_800)
        #expect(clip.suffix(3_200).allSatisfy { $0 == 0.5 })
        #expect(clip.prefix(1_600).allSatisfy { $0 == 0 })
    }

    @Test func endClipEmptiesTheRingSoNothingLeaksIntoTheNextClip() {
        var acc = ClipAccumulator(preRollSamples: 4, maxSamples: 100)
        acc.append(ramp(0..<4))
        acc.beginClip()
        acc.append(ramp(4..<6))
        _ = acc.endClip()
        #expect(acc.preRollCount == 0)
        #expect(acc.beginClip() == 0)
        #expect(acc.endClip() == [])
    }

    @Test func capCountsPreRollAndFiresOnce() {
        var acc = ClipAccumulator(preRollSamples: 3, maxSamples: 5)
        acc.append(ramp(0..<3))
        acc.beginClip()
        #expect(acc.append(ramp(3..<4)) == false)
        #expect(acc.append(ramp(4..<9)) == true)  // reaches 5
        #expect(acc.append(ramp(9..<12)) == false)  // already reported
        #expect(acc.endClip() == ramp(0..<5))
        // A new clip reports its own cap again.
        acc.beginClip()
        #expect(acc.append(ramp(0..<5)) == true)
    }

    @Test func preRollLongerThanCapIsTrimmedToTheNewest() {
        var acc = ClipAccumulator(preRollSamples: 6, maxSamples: 4)
        acc.append(ramp(0..<6))
        #expect(acc.beginClip() == 4)
        #expect(acc.endClip() == ramp(2..<6))
    }

    @Test func beginWhileRecordingIsANoOpAndEndWhileIdleIsEmpty() {
        var acc = ClipAccumulator(preRollSamples: 2, maxSamples: 10)
        #expect(acc.endClip() == [])
        acc.beginClip()
        acc.append(ramp(0..<3))
        #expect(acc.beginClip() == 0)
        #expect(acc.endClip() == ramp(0..<3))
    }
}

@Suite struct LaunchOptionsTests {
    @Test func noArgumentsIsNormal() throws {
        #expect(try LaunchOptions.parse(["DictateHub"]) == LaunchOptions())
    }

    @Test func overridesAreIgnoredInNormalMode() throws {
        let o = try LaunchOptions.parse(["x", "--endpoint", "http://evil:1", "--token-dir", "/tmp/t"])
        #expect(o.mode == .normal)
        #expect(o.endpointOverride == nil)
        #expect(o.tokenDirectory == nil)
    }

    @Test func smokeKeepsM1Shape() throws {
        let o = try LaunchOptions.parse(["x", "--smoke", "4", "--probe", "--endpoint", "http://127.0.0.1:9", "--token-dir", "/t"])
        #expect(o.mode == .smoke(seconds: 4, probe: true, onboarding: false))
        #expect(o.endpointOverride == "http://127.0.0.1:9")
        #expect(o.tokenDirectory == "/t")
        #expect(try LaunchOptions.parse(["x", "--smoke"]).mode == .smoke(seconds: 3, probe: false, onboarding: false))
        #expect(try LaunchOptions.parse(["x", "--smoke", "2", "--onboarding"]).mode
            == .smoke(seconds: 2, probe: false, onboarding: true))
    }

    @Test func fileMode() throws {
        let o = try LaunchOptions.parse(["x", "--transcribe-file", "/tmp/a b.wav", "--insert", "--app", "TextEdit",
                                         "--endpoint", "http://127.0.0.1:7313"])
        #expect(o.mode == .transcribeFile(path: "/tmp/a b.wav", insert: true, app: "TextEdit"))
        #expect(o.isFileMode)
        #expect(o.endpointOverride == "http://127.0.0.1:7313")
        let plain = try LaunchOptions.parse(["x", "--transcribe-file", "c.wav"])
        #expect(plain.mode == .transcribeFile(path: "c.wav", insert: false, app: nil))
    }

    @Test func missingValuesAndConflictsAreErrors() {
        #expect(throws: LaunchOptions.ParseError.missingValue("--transcribe-file")) {
            try LaunchOptions.parse(["x", "--transcribe-file"])
        }
        #expect(throws: LaunchOptions.ParseError.missingValue("--transcribe-file")) {
            try LaunchOptions.parse(["x", "--transcribe-file", "--insert"])
        }
        #expect(throws: LaunchOptions.ParseError.missingValue("--endpoint")) {
            try LaunchOptions.parse(["x", "--transcribe-file", "a.wav", "--endpoint"])
        }
        #expect(throws: LaunchOptions.ParseError.conflictingModes) {
            try LaunchOptions.parse(["x", "--smoke", "1", "--transcribe-file", "a.wav"])
        }
        #expect(throws: LaunchOptions.ParseError.conflictingModes) {
            try LaunchOptions.parse(["x", "--unregister-login-item", "--smoke", "1"])
        }
    }

    @Test func unregisterLoginItem() throws {
        #expect(try LaunchOptions.parse(["x", "--unregister-login-item"]).mode == .unregisterLoginItem)
    }

    @Test func resultLineIsOneSortedJSONLine() throws {
        let ok = FileModeResult(ok: true, text: "Hello, world.", audioSeconds: 1.5, hubMs: 420, inserted: false, app: "TextEdit")
        #expect(ok.line == #"DICTATEHUB_RESULT {"app":"TextEdit","audio_s":1.5,"hub_ms":420,"inserted":false,"ok":true,"text":"Hello, world."}"#)
        let bad = FileModeResult(ok: false, error: "Hub unreachable")
        #expect(bad.line == #"DICTATEHUB_RESULT {"error":"Hub unreachable","inserted":false,"ok":false}"#)
        #expect(!FileModeResult(ok: true, text: "a\nb").line.dropFirst().contains("\n"))
    }
}

@Suite struct AudioFileLoaderTests {
    func write(_ data: Data) throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("dictatehub-\(UUID().uuidString).wav")
        try data.write(to: url)
        return url
    }

    @Test func hubFormatRoundTripsThroughTheEncoder() throws {
        let samples: [Int16] = (0..<16_000).map { Int16(truncatingIfNeeded: ($0 * 37) % 20_000 - 10_000) }
        let url = try write(WAV.encode(samples: samples))
        defer { try? FileManager.default.removeItem(at: url) }
        let loaded = try AudioFileLoader.load(url)
        #expect(loaded.count == samples.count)
        // Back through the live encoder: within one LSB of the original.
        let again = WAV.int16Samples(from: loaded)
        let worst = zip(again, samples).map { abs(Int($0) - Int($1)) }.max() ?? 0
        #expect(worst <= 1)
    }

    @Test func otherRatesAreResampledTo16k() throws {
        let tone: [Int16] = (0..<8_000).map { Int16(8_000 * sin(Double($0) * 2 * .pi * 440 / 8_000)) }
        let url = try write(WAV.encode(samples: tone, sampleRate: 8_000))
        defer { try? FileManager.default.removeItem(at: url) }
        let loaded = try AudioFileLoader.load(url)
        #expect(abs(loaded.count - 16_000) <= 64)  // one second either way
        #expect(loaded.contains { abs($0) > 0.1 })
    }

    @Test func garbageAndEmptyFilesAreErrors() throws {
        let junk = try write(Data("not audio".utf8))
        defer { try? FileManager.default.removeItem(at: junk) }
        #expect(throws: AudioFileError.self) { try AudioFileLoader.load(junk) }
        let empty = try write(WAV.encode(samples: []))
        defer { try? FileManager.default.removeItem(at: empty) }
        #expect(throws: AudioFileError.self) { try AudioFileLoader.load(empty) }
        #expect(throws: AudioFileError.self) {
            try AudioFileLoader.load(URL(fileURLWithPath: "/nonexistent/dictatehub.wav"))
        }
    }
}

@Suite struct M2SettingsTests {
    func defaults() -> UserDefaults {
        let name = "dictatehub-tests-\(UUID().uuidString)"
        let d = UserDefaults(suiteName: name)!
        d.removePersistentDomain(forName: name)
        return d
    }

    @Test func newDefaults() {
        let s = HubSettings(defaults: defaults())
        #expect(s.playSounds == true)
        #expect(s.keepMicWarm == false)  // privacy: no open mic unless asked
        #expect(s.onboardingDone == false)
        #expect(s.loginItemConfigured == false)
    }

    @Test func persistence() {
        let d = defaults()
        let s = HubSettings(defaults: d)
        s.playSounds = false
        s.keepMicWarm = true
        s.onboardingDone = true
        s.loginItemConfigured = true
        let t = HubSettings(defaults: d)
        #expect(t.playSounds == false)
        #expect(t.keepMicWarm == true)
        #expect(t.onboardingDone == true)
        #expect(t.loginItemConfigured == true)
    }
}

@Suite struct OnboardingTests {
    @Test func opensOnFirstRunOrWhenSomethingIsMissing() {
        let all = OnboardingChecklist(microphone: .granted, accessibility: .granted, tokenPresent: true)
        #expect(all.isComplete)
        #expect(!all.shouldOpenAtLaunch(firstRun: false))
        #expect(all.shouldOpenAtLaunch(firstRun: true))
        for missing in [
            OnboardingChecklist(microphone: .notAsked, accessibility: .granted, tokenPresent: true),
            OnboardingChecklist(microphone: .denied, accessibility: .granted, tokenPresent: true),
            OnboardingChecklist(microphone: .granted, accessibility: .notAsked, tokenPresent: true),
            OnboardingChecklist(microphone: .granted, accessibility: .granted, tokenPresent: false),
        ] {
            #expect(!missing.isComplete)
            #expect(missing.shouldOpenAtLaunch(firstRun: false))
        }
    }

    @Test func paneLinks() {
        #expect(PrivacyPane.accessibility.url.absoluteString
            == "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        #expect(PrivacyPane.microphone.url.absoluteString.hasSuffix("?Privacy_Microphone"))
        #expect(PrivacyPane.inputMonitoring.url.absoluteString.hasSuffix("?Privacy_ListenEvent"))
        #expect(Set([PermissionState.granted, .notAsked, .denied].map(\.symbolName)).count == 3)
    }

    @Test func busyPhasesAnimateAndIdleIsStill() {
        #expect(AppPhase.recording.animationFrames.count == 2)
        #expect(AppPhase.transcribing.animationFrames.count == 2)
        #expect(AppPhase.idle.animationFrames == [AppPhase.idle.symbolName])
        #expect(AppPhase.error("x").animationFrames == [AppPhase.error("x").symbolName])
        #expect(AppPhase.recording.animationFrames.first == AppPhase.recording.symbolName)
        for name in AppPhase.recording.animationFrames + AppPhase.transcribing.animationFrames {
            #expect(NSImage(systemSymbolName: name, accessibilityDescription: nil) != nil, "\(name)")
        }
    }
}

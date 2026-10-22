import Foundation
import Testing
@testable import DictateHubCore

@Suite struct HotkeyChoiceTests {
    // Raw flags as AppKit reports them: device-independent bit | device bit | 0x100.
    let rightOption = ModifierBits.option | ModifierBits.deviceRightOption | 0x100
    let leftOption = ModifierBits.option | ModifierBits.deviceLeftOption | 0x100
    let rightCommand = ModifierBits.command | ModifierBits.deviceRightCommand | 0x100
    let leftCommand = ModifierBits.command | ModifierBits.deviceLeftCommand | 0x100
    let leftControl = ModifierBits.control | ModifierBits.deviceLeftControl | 0x100
    let fn = ModifierBits.function | 0x100

    @Test func rightOptionIgnoresLeftOption() {
        #expect(HotkeyChoice.rightOption.isHeld(flags: rightOption))
        #expect(!HotkeyChoice.rightOption.isHeld(flags: leftOption))
        #expect(HotkeyChoice.rightOption.isHeld(flags: leftOption | rightOption))
        #expect(!HotkeyChoice.rightOption.isHeld(flags: 0x100))
    }

    @Test func rightCommandIgnoresLeftCommand() {
        #expect(HotkeyChoice.rightCommand.isHeld(flags: rightCommand))
        #expect(!HotkeyChoice.rightCommand.isHeld(flags: leftCommand))
    }

    @Test func fnAndControlOption() {
        #expect(HotkeyChoice.fn.isHeld(flags: fn))
        #expect(!HotkeyChoice.fn.isHeld(flags: rightOption))
        #expect(HotkeyChoice.controlOption.isHeld(flags: leftControl | leftOption))
        #expect(HotkeyChoice.controlOption.isHeld(flags: leftControl | rightOption))
        #expect(!HotkeyChoice.controlOption.isHeld(flags: leftControl))
        #expect(!HotkeyChoice.controlOption.isHeld(flags: leftOption))
    }

    @Test func rawValuesAreStable() {
        #expect(HotkeyChoice.allCases.map(\.rawValue) == ["right_option", "fn", "right_command", "control_option", "option_n"])
        for raw in ["right_option", "fn", "right_command", "control_option"] {
            #expect(HotkeyChoice(rawValue: raw)?.rawValue == raw)  // every old raw value still decodes
            #expect(HotkeyChoice(rawValue: raw)?.mode == .hold)
        }
        #expect(HotkeyChoice(rawValue: "option_n") == .optionN)
        #expect(HotkeyChoice.default == .rightOption)
    }

    @Test func edgeDetectorReportsTransitionsOnly() {
        var d = HotkeyEdgeDetector(choice: .rightOption)
        #expect(d.update(flags: leftOption) == nil)  // left option: not ours
        #expect(d.update(flags: leftOption | rightOption) == .pressed)
        #expect(d.update(flags: rightOption | ModifierBits.shift) == nil)  // shift added mid-hold
        #expect(d.update(flags: 0x100) == .released)
        #expect(d.update(flags: 0x100) == nil)
        #expect(d.update(flags: rightOption) == .pressed)
        d.reset()
        #expect(!d.held)
    }
}

@Suite struct HoldToTalkTests {
    @Test func holdThenReleaseSends() {
        var h = HoldToTalk()
        #expect(h.press(at: 10) == .start)
        #expect(h.isCapturing)
        #expect(h.release(at: 11) == .finish)
        #expect(!h.isCapturing)
    }

    @Test func tapUnder250msIsDiscarded() {
        var h = HoldToTalk()
        #expect(h.press(at: 10) == .start)
        #expect(h.release(at: 10.2) == .discard)
        #expect(h.press(at: 20) == .start)
        #expect(h.release(at: 20.25) == .finish)  // exactly the minimum counts
    }

    @Test func escapeCancelsAndSwallowsTheRelease() {
        var h = HoldToTalk()
        _ = h.press(at: 1)
        #expect(h.escape() == .cancel)
        #expect(h.escape() == nil)
        #expect(!h.isCapturing)
        #expect(h.release(at: 5) == nil)
        #expect(h.press(at: 6) == .start)  // the next hold works normally
    }

    @Test func escapeWhenIdleDoesNothing() {
        var h = HoldToTalk()
        #expect(h.escape() == nil)
        #expect(h.release(at: 1) == nil)
    }

    @Test func repeatedPressIsIgnored() {
        var h = HoldToTalk()
        #expect(h.press(at: 1) == .start)
        #expect(h.press(at: 1.1) == nil)
        #expect(h.release(at: 2) == .finish)
    }

    @Test func lengthCapSendsOnceAndSwallowsTheRelease() {
        var h = HoldToTalk()
        _ = h.press(at: 0)
        #expect(h.limitReached() == .finish)
        #expect(h.limitReached() == nil)
        #expect(h.escape() == nil)
        #expect(h.release(at: 130) == nil)
    }
}

@Suite struct AppPhaseTests {
    @Test func everyPhaseHasAnIconAndALine() {
        let phases: [AppPhase] = [.idle, .recording, .transcribing, .error("Hub unreachable")]
        #expect(Set(phases.map(\.symbolName)).count == 4)
        #expect(AppPhase.error("Hub unreachable").statusLine == "Hub unreachable")
    }
}

@Suite struct OptionNChordTests {
    let n = KeyCode.ansiN
    let option = ModifierBits.option | ModifierBits.deviceLeftOption | 0x100
    let rightOptionOnly = ModifierBits.option | ModifierBits.deviceRightOption | 0x100
    let capsLock: UInt64 = 1 << 16

    @Test func optionNMatchesOnlyOptionPlusN() throws {
        let chord = try #require(HotkeyChoice.optionN.chord)
        #expect(chord.matches(keyCode: n, flags: option))
        #expect(chord.matches(keyCode: n, flags: rightOptionOnly))  // either Option key
        #expect(chord.matches(keyCode: n, flags: option | capsLock))  // Caps Lock is ignored
        #expect(!chord.matches(keyCode: n, flags: option | ModifierBits.shift))
        #expect(!chord.matches(keyCode: n, flags: option | ModifierBits.command))
        #expect(!chord.matches(keyCode: n, flags: option | ModifierBits.control))
        #expect(!chord.matches(keyCode: n, flags: 0x100))  // plain N
        #expect(!chord.matches(keyCode: n, flags: ModifierBits.shift))  // Shift+N
        #expect(!chord.matches(keyCode: KeyCode.ansiV, flags: option))  // Option+V
    }

    @Test func chordChoiceIsAToggleAndModifierChoicesStayHold() {
        #expect(HotkeyChoice.optionN.mode == .toggle)
        #expect(HotkeyChoice.optionN.displayName == "Option + N (⌥N)")
        #expect(HotkeyChoice.allCases.filter { $0 != .optionN }.allSatisfy { $0.mode == .hold && $0.chord == nil })
        #expect(HotkeyChoice.default == .rightOption)
        // The choices UI shows the mode next to each choice.
        for choice in HotkeyChoice.allCases {
            #expect(choice.pickerLabel.hasSuffix(choice.mode.label))
        }
        #expect(HotkeyChoice.optionN.hint.contains("Esc"))
        #expect(HotkeyChoice.rightOption.hint == "Hold Right Option (⌥) to dictate")
    }

    @Test func chordIsNeverHeldAsAModifier() {
        var d = HotkeyEdgeDetector(choice: .optionN)
        #expect(d.update(flags: option) == nil)
        #expect(d.update(flags: 0x100) == nil)
    }

    @Test func settingsPersistAndOldValuesDecode() throws {
        let suite = "dictatehub.test.optionn.\(UUID().uuidString)"
        let d = try #require(UserDefaults(suiteName: suite))
        defer { d.removePersistentDomain(forName: suite) }
        let s = HubSettings(defaults: d)
        #expect(s.hotkey == .rightOption)  // new installs keep the default
        s.hotkey = .optionN
        #expect(d.string(forKey: HubSettings.hotkeyKey) == "option_n")
        #expect(HubSettings(defaults: d).hotkey == .optionN)
        for choice in [HotkeyChoice.fn, .rightCommand, .controlOption, .rightOption] {
            d.set(choice.rawValue, forKey: HubSettings.hotkeyKey)
            #expect(HubSettings(defaults: d).hotkey == choice)
        }
    }
}

@Suite struct ToggleToTalkTests {
    @Test func firstPressStartsSecondPressSends() {
        var t = ToggleToTalk()
        #expect(!t.isCapturing)
        #expect(t.press() == .start)
        #expect(t.isCapturing)
        #expect(t.press() == .finish)
        #expect(!t.isCapturing)
        #expect(t.press() == .start)  // the next dictation starts normally
    }

    @Test func escapeCancelsOnlyWhileCapturing() {
        var t = ToggleToTalk()
        #expect(t.escape() == nil)
        _ = t.press()
        #expect(t.escape() == .cancel)
        #expect(!t.isCapturing)
        #expect(t.escape() == nil)
        #expect(t.press() == .start)
    }

    @Test func lengthCapSendsOnceAndResetForgets() {
        var t = ToggleToTalk()
        #expect(t.limitReached() == nil)
        _ = t.press()
        #expect(t.limitReached() == .finish)
        #expect(t.limitReached() == nil)
        _ = t.press()
        t.reset()
        #expect(!t.isCapturing)
        #expect(t.press() == .start)
    }
}

@Suite struct ChordKeyRouterTests {
    let n = KeyCode.ansiN
    let esc = KeyCode.escape
    let option = ModifierBits.option | ModifierBits.deviceLeftOption | 0x100
    func router() -> ChordKeyRouter { ChordKeyRouter(chord: HotkeyChoice.optionN.chord) }

    @Test func chordKeydownAndKeyupAreSwallowed() {
        var r = router()
        #expect(r.route(.keyDown, keyCode: n, flags: option, isRepeat: false, recording: false) == .swallow(.press))
        // Option may be released before N: the keyup is still consumed.
        #expect(r.route(.keyUp, keyCode: n, flags: 0x100, isRepeat: false, recording: false) == .swallow(nil))
        // And only once.
        #expect(r.route(.keyUp, keyCode: n, flags: 0x100, isRepeat: false, recording: false) == .passThrough)
    }

    @Test func autoRepeatIsSwallowedButIsNotANewPress() {
        var r = router()
        #expect(r.route(.keyDown, keyCode: n, flags: option, isRepeat: false, recording: false) == .swallow(.press))
        #expect(r.route(.keyDown, keyCode: n, flags: option, isRepeat: true, recording: true) == .swallow(nil))
        #expect(r.route(.keyDown, keyCode: n, flags: option, isRepeat: false, recording: true) == .swallow(nil))
        _ = r.route(.keyUp, keyCode: n, flags: option, isRepeat: false, recording: true)
        #expect(r.route(.keyDown, keyCode: n, flags: option, isRepeat: false, recording: true) == .swallow(.press))
    }

    @Test func otherChordsAndPlainNPassThrough() {
        var r = router()
        for flags in [option | ModifierBits.shift, option | ModifierBits.command, option | ModifierBits.control, 0x100, ModifierBits.shift] {
            #expect(r.route(.keyDown, keyCode: n, flags: flags, isRepeat: false, recording: true) == .passThrough)
            #expect(r.route(.keyUp, keyCode: n, flags: flags, isRepeat: false, recording: true) == .passThrough)
        }
        #expect(r.route(.keyDown, keyCode: KeyCode.ansiV, flags: option, isRepeat: false, recording: true) == .passThrough)
    }

    @Test func escapeIsSwallowedOnlyWhileRecording() {
        var r = router()
        #expect(r.route(.keyDown, keyCode: esc, flags: 0x100, isRepeat: false, recording: false) == .passThrough)
        #expect(r.route(.keyUp, keyCode: esc, flags: 0x100, isRepeat: false, recording: false) == .passThrough)
        #expect(r.route(.keyDown, keyCode: esc, flags: 0x100, isRepeat: false, recording: true) == .swallow(.escape))
        // The recording ends on the keydown; its keyup is still consumed.
        #expect(r.route(.keyUp, keyCode: esc, flags: 0x100, isRepeat: false, recording: false) == .swallow(nil))
        #expect(r.route(.keyUp, keyCode: esc, flags: 0x100, isRepeat: false, recording: false) == .passThrough)
    }

    @Test func modifierOnlyChoicesNeverSwallow() {
        var r = ChordKeyRouter(chord: HotkeyChoice.rightOption.chord)
        #expect(r.route(.keyDown, keyCode: n, flags: option, isRepeat: false, recording: true) == .passThrough)
        #expect(r.route(.keyDown, keyCode: esc, flags: 0x100, isRepeat: false, recording: true) == .passThrough)
    }

    @Test func toggleSessionWithRoutedKeysStartsStopsAndCancels() {
        var r = router()
        var t = ToggleToTalk()
        func press() -> HoldToTalk.Action? {
            guard case .swallow(.press) = r.route(.keyDown, keyCode: n, flags: option, isRepeat: false, recording: t.isCapturing) else { return nil }
            _ = r.route(.keyUp, keyCode: n, flags: option, isRepeat: false, recording: t.isCapturing)
            return t.press()
        }
        #expect(press() == .start)
        #expect(press() == .finish)
        #expect(press() == .start)
        guard case .swallow(.escape) = r.route(.keyDown, keyCode: esc, flags: 0x100, isRepeat: false, recording: t.isCapturing) else {
            Issue.record("Esc while recording must be swallowed")
            return
        }
        #expect(t.escape() == .cancel)
        #expect(!t.isCapturing)
    }
}

@Suite struct AppPhaseModeTests {
    @Test func recordingLineFollowsTheHotkeyMode() {
        #expect(AppPhase.recording.statusLine(mode: .hold).contains("release to send"))
        #expect(AppPhase.recording.statusLine(mode: .toggle).contains("press the hotkey to send"))
        #expect(AppPhase.recording.statusLine == AppPhase.recording.statusLine(mode: .hold))
        #expect(AppPhase.idle.statusLine(mode: .toggle) == "Ready")
    }
}

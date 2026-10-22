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
        #expect(HotkeyChoice.allCases.map(\.rawValue) == ["right_option", "fn", "right_command", "control_option"])
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

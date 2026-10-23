import Foundation
import Testing
@testable import DictateHubCore

@Suite struct PermissionDiagnosisTests {
    func snap(ax: Bool = true, im: Bool = true, mic: PermissionState = .granted, chord: Bool = false) -> PermissionSnapshot {
        PermissionSnapshot(accessibility: ax, inputMonitoring: im, microphone: mic, hotkeyNeedsInputMonitoring: chord)
    }

    @Test func everythingGrantedIsComplete() {
        let d = PermissionDiagnosis(snap(chord: true))
        #expect(d.isComplete)
        #expect(d.missing.isEmpty)
        #expect(d.headline == nil)
        #expect(d.detail == nil)
        #expect(d.lines().isEmpty)
    }

    @Test func accessibilityOnlyNamesAccessibility() {
        let d = PermissionDiagnosis(snap(ax: false))
        #expect(d.missing == [.accessibility])
        #expect(d.headline == "Accessibility not granted: hotkey and paste are off")
        let line = d.lines()[0]
        #expect(line.contains("System Settings → Privacy & Security → Accessibility"))
    }

    @Test func staleGrantHintOnAccessibilityAndInputMonitoring() {
        for permission in [MissingPermission.accessibility, .inputMonitoring] {
            let hint = permission.staleGrantHint ?? ""
            #expect(hint.hasPrefix("If DictateHub already looks enabled there, remove it with − and add "))
            #expect(hint.contains("/Applications/DictateHub.app again with +"))
            #expect(hint.contains("a grant made for an older build does not carry over"))
        }
        let d = PermissionDiagnosis(snap(ax: false, im: false, chord: true))
        #expect(d.lines().allSatisfy { $0.contains("remove it with −") })
    }

    @Test func microphoneHasNoStaleHintAndItsOwnPath() {
        let d = PermissionDiagnosis(snap(mic: .denied))
        #expect(d.missing == [.microphone])
        #expect(d.headline == "Microphone not granted: it cannot record")
        #expect(d.lines()[0].contains("Privacy & Security → Microphone"))
        #expect(!d.lines()[0].contains("remove it with −"))
        // not asked yet is also "not granted" for the Permissions window
        #expect(PermissionDiagnosis(snap(mic: .notAsked)).missing == [.microphone])
    }

    @Test func inputMonitoringOnlyCountsForChordHotkeys() {
        #expect(PermissionDiagnosis(snap(im: false, chord: false)).isComplete)
        let d = PermissionDiagnosis(snap(im: false, chord: true))
        #expect(d.missing == [.inputMonitoring])
        #expect(d.headline == "Input Monitoring not granted: the hotkey cannot see its key")
        #expect(d.lines()[0].contains("Privacy & Security → Input Monitoring"))
    }

    @Test func everythingMissingListsAllInOrder() {
        let d = PermissionDiagnosis(snap(ax: false, im: false, mic: .denied, chord: true))
        #expect(d.missing == [.accessibility, .inputMonitoring, .microphone])
        #expect(d.headline == "Accessibility + Input Monitoring + Microphone not granted: hotkey and paste are off, it cannot record")
        #expect(d.lines().count == 3)
    }

    @Test func diagnosisFollowsTrustChanges() {
        // The grant arrives while the app runs: the same model moves to complete.
        #expect(PermissionDiagnosis(snap(ax: false)).missing == [.accessibility])
        #expect(PermissionDiagnosis(snap(ax: true)).isComplete)
    }

    @Test func menuWrapKeepsWordsAndWidth() {
        let text = PermissionDiagnosis(snap(ax: false)).lines()[0]
        let wrapped = wrapForMenu(text, width: 64)
        #expect(wrapped.count > 1)
        #expect(wrapped.allSatisfy { $0.count <= 64 })
        #expect(wrapped.joined(separator: " ") == text)
        #expect(wrapForMenu("").isEmpty)
    }
}

@Suite struct AeroSpaceConflictTests {
    @Test func detectsAnActiveAltNBinding() {
        #expect(AeroSpaceConflict.bindsAltN(in: "[mode.main.binding]\nalt-n = 'workspace N'\n"))
        #expect(AeroSpaceConflict.bindsAltN(in: "  alt-n   =   ['workspace 1']\n"))
        #expect(AeroSpaceConflict.bindsAltN(in: "'alt-n' = 'workspace N'"))
    }

    @Test func ignoresCommentsAndOtherChords() {
        #expect(!AeroSpaceConflict.bindsAltN(in: "    # alt-n = 'workspace N'   # freed for DictateHub"))
        #expect(!AeroSpaceConflict.bindsAltN(in: "alt-shift-n = 'move-node-to-workspace N'"))
        #expect(!AeroSpaceConflict.bindsAltN(in: "alt-m = 'workspace M'\nalt-1 = 'workspace 1'"))
        #expect(!AeroSpaceConflict.bindsAltN(in: ""))
    }
}

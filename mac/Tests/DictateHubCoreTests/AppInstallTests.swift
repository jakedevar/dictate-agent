import Foundation
import Testing
@testable import DictateHubCore

@Suite struct InstallLocationTests {
    @Test func installedPathIsApplications() {
        #expect(InstallLocation.installedPath == "/Applications/DictateHub.app")
        #expect(InstallLocation.isInstalled(bundlePath: "/Applications/DictateHub.app"))
        #expect(InstallLocation.isInstalled(bundlePath: "/Applications/DictateHub.app/"))
        #expect(InstallLocation.isInstalled(bundlePath: "/Applications//DictateHub.app"))
    }

    @Test func otherLocationsAreNotInstalled() {
        let home = "/Users/someone"
        #expect(!InstallLocation.isInstalled(bundlePath: "\(home)/Applications/DictateHub.app"))
        #expect(!InstallLocation.isInstalled(bundlePath: "\(home)/dictate-mac-build/x.noindex/build/DictateHub.app"))
        #expect(!InstallLocation.isInstalled(bundlePath: "/Applications/Other/DictateHub.app"))
        #expect(!InstallLocation.isInstalled(bundlePath: "/Applications/DictateHub.app.bak"))
    }

    @Test func legacyPathIsPerUser() {
        let home = "/Users/someone"
        #expect(InstallLocation.legacyPath(home: home) == "/Users/someone/Applications/DictateHub.app")
        #expect(InstallLocation.isLegacy(bundlePath: "/Users/someone/Applications/DictateHub.app", home: home))
        #expect(!InstallLocation.isLegacy(bundlePath: "/Applications/DictateHub.app", home: home))
    }
}

@Suite struct ReopenWindowTests {
    private let complete = OnboardingChecklist(microphone: .granted, accessibility: .granted, tokenPresent: true)

    @Test func settingsWhenEverythingIsGrantedAndOnboardingDone() {
        #expect(ReopenWindow.decide(checklist: complete, onboardingDone: true, missing: []) == .settings)
    }

    @Test func onboardingBeforeTheFirstRunIsFinished() {
        #expect(ReopenWindow.decide(checklist: complete, onboardingDone: false, missing: []) == .onboarding)
    }

    @Test func onboardingWhenAnyChecklistItemIsMissing() {
        for checklist in [
            OnboardingChecklist(microphone: .notAsked, accessibility: .granted, tokenPresent: true),
            OnboardingChecklist(microphone: .denied, accessibility: .granted, tokenPresent: true),
            OnboardingChecklist(microphone: .granted, accessibility: .notAsked, tokenPresent: true),
            OnboardingChecklist(microphone: .granted, accessibility: .granted, tokenPresent: false),
        ] {
            #expect(ReopenWindow.decide(checklist: checklist, onboardingDone: true, missing: []) == .onboarding)
        }
    }

    @Test func onboardingWhenInputMonitoringIsMissingEvenIfTheChecklistIsComplete() {
        #expect(ReopenWindow.decide(checklist: complete, onboardingDone: true,
                                    missing: [.inputMonitoring]) == .onboarding)
    }
}

@Suite struct LoginItemPlanTests {
    private let installed = "/Applications/DictateHub.app"

    @Test func reregistersWhenTheRecordedPathIsMissingOrDifferent() {
        #expect(LoginItemPlan.decide(bundlePath: installed, registeredPath: nil,
                                     enabled: true, isNormalLaunch: true) == .reregister)
        #expect(LoginItemPlan.decide(bundlePath: installed,
                                     registeredPath: "/Users/someone/Applications/DictateHub.app",
                                     enabled: true, isNormalLaunch: true) == .reregister)
    }

    @Test func keepsWhenAlreadyRegisteredFromThisPath() {
        #expect(LoginItemPlan.decide(bundlePath: installed, registeredPath: installed + "/",
                                     enabled: true, isNormalLaunch: true) == .keep)
    }

    @Test func keepsWhenTheUserSwitchedItOff() {
        #expect(LoginItemPlan.decide(bundlePath: installed, registeredPath: nil,
                                     enabled: false, isNormalLaunch: true) == .keep)
    }

    @Test func neverTouchesTheLoginItemFromABuildDirSmokeOrFileRun() {
        let build = "/Users/someone/dictate-mac-build/x.noindex/build/DictateHub.app"
        #expect(LoginItemPlan.decide(bundlePath: build, registeredPath: nil,
                                     enabled: true, isNormalLaunch: true) == .keep)
        #expect(LoginItemPlan.decide(bundlePath: installed, registeredPath: nil,
                                     enabled: true, isNormalLaunch: false) == .keep)
    }

    @Test func settingsRoundTripsTheRegisteredPath() {
        let d = UserDefaults(suiteName: "dh-test-\(UUID().uuidString)")!
        let s = HubSettings(defaults: d)
        #expect(s.loginItemRegisteredPath == nil)
        s.loginItemRegisteredPath = installed
        #expect(HubSettings(defaults: d).loginItemRegisteredPath == installed)
    }
}

import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct DiagnosticsSwitchTests {
    /// A switch over defaults of its own, recording what it hands the client core.
    final class Shared {
        var calls: [Diagnostics?] = []
    }

    func fresh(keys: (sentryDsn: String?, posthogToken: String?) = ("https://k@o1.ingest.de.sentry.io/2", "phc_test"))
        -> (DiagnosticsSwitch, Shared, UserDefaults) {
        let name = "diagnostics-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defaults.removePersistentDomain(forName: name)
        let shared = Shared()
        let diagnostics = DiagnosticsSwitch(defaults: defaults, keys: keys) { shared.calls.append($0) }
        return (diagnostics, shared, defaults)
    }

    @Test func itIsOffAndUnaskedUntilThePersonAnswers() {
        let (diagnostics, shared, _) = fresh()
        #expect(!diagnostics.on)
        #expect(!diagnostics.asked)
        diagnostics.apply()
        #expect(shared.calls.count == 1)
        #expect(shared.calls[0] == nil)
        #expect(diagnostics.diagnostics == nil)
    }

    @Test func itAsksOnceAndNeverOverATestsProfileOrFakeFleet() {
        let (diagnostics, _, _) = fresh()
        #expect(diagnostics.shouldAsk(environment: [:]))
        #expect(!diagnostics.shouldAsk(environment: ["HERDER_PROFILE": "/tmp/profile"]))
        #expect(!diagnostics.shouldAsk(environment: ["HERDER_FAKE_FLEET": "herder://fleet#x"]))
        diagnostics.set(false)
        #expect(!diagnostics.shouldAsk(environment: [:]))
    }

    @Test func turningItOnSharesWithOneInstallIdAndOffSharesNothing() throws {
        let (diagnostics, shared, defaults) = fresh()
        diagnostics.set(true)
        let on = try #require(shared.calls.last ?? nil)
        #expect(UUID(uuidString: on.installId) != nil)
        #expect(on.sentryDsn == "https://k@o1.ingest.de.sentry.io/2")
        #expect(on.posthogToken == "phc_test")
        #expect(diagnostics.asked)

        diagnostics.set(false)
        #expect(shared.calls.last! == nil)

        // The answer and the id outlive the launch.
        let again = DiagnosticsSwitch(defaults: defaults, keys: (nil, nil)) { shared.calls.append($0) }
        #expect(!again.on)
        #expect(again.asked)
        again.set(true)
        #expect((shared.calls.last ?? nil)?.installId == on.installId)
    }

    @Test func aBuildWithoutKeysSharesWithNone() throws {
        let (diagnostics, shared, _) = fresh(keys: (nil, nil))
        diagnostics.set(true)
        let on = try #require(shared.calls.last ?? nil)
        #expect(on.sentryDsn == nil)
        #expect(on.posthogToken == nil)
    }
}

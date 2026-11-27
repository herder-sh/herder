import Foundation
import Herder
import Observation
import SwiftUI

/// Whether this app shares diagnostics with herder's Sentry and PostHog: one switch, off until
/// the person turns it on. The app asks once, calmly, on first launch (`DiagnosticsAsk`); the
/// switch then stays on the You page. Turning it off stops everything at once: the client core
/// drops what it has not sent (`setDiagnostics(nil)`).
@MainActor @Observable
final class DiagnosticsSwitch {
    static let shared = DiagnosticsSwitch()

    static let onKey = "diagnosticsOn"
    static let askedKey = "diagnosticsAsked"
    static let installIdKey = "diagnosticsInstallId"

    /// Whether the app shares diagnostics.
    private(set) var on: Bool
    /// Whether the person has answered, on first launch or with the switch.
    private(set) var asked: Bool

    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private let keys: (sentryDsn: String?, posthogToken: String?)
    @ObservationIgnored private let share: (Diagnostics?) -> Void

    /// `keys` are the write-only client keys the build was made with; `share` hands the
    /// diagnostics in effect to the client core.
    init(defaults: UserDefaults = .standard,
         keys: (sentryDsn: String?, posthogToken: String?) = DiagnosticsSwitch.buildKeys,
         share: @escaping (Diagnostics?) -> Void = { setDiagnostics(diagnostics: $0) }) {
        self.defaults = defaults
        self.keys = keys
        self.share = share
        on = defaults.bool(forKey: Self.onKey)
        asked = defaults.bool(forKey: Self.askedKey)
    }

    /// Hands the switch, as saved, to the client core; at launch.
    func apply() {
        share(diagnostics)
    }

    /// Turns sharing on or off, and counts as the person's answer.
    func set(_ on: Bool) {
        self.on = on
        asked = true
        defaults.set(on, forKey: Self.onKey)
        defaults.set(true, forKey: Self.askedKey)
        apply()
    }

    /// Whether to ask on this launch: not yet answered, and not a test's or a developer's
    /// profile (`HERDER_PROFILE`), whose screens must not change.
    var shouldAsk: Bool {
        !asked && ProcessInfo.processInfo.environment["HERDER_PROFILE"] == nil
    }

    /// What the client core shares with while the switch is on.
    var diagnostics: Diagnostics? {
        guard on else { return nil }
        return Diagnostics(installId: installId, sentryDsn: keys.sentryDsn, posthogToken: keys.posthogToken)
    }

    /// A random id for this install, made the first time it shares; it names nothing else.
    private var installId: String {
        if let id = defaults.string(forKey: Self.installIdKey), UUID(uuidString: id) != nil { return id }
        let id = UUID().uuidString.lowercased()
        defaults.set(id, forKey: Self.installIdKey)
        return id
    }

    /// The keys from the app's Info.plist, which the release build fills from its secrets;
    /// none in a build made without them, which then sends nothing anywhere.
    nonisolated static var buildKeys: (sentryDsn: String?, posthogToken: String?) {
        func key(_ name: String) -> String? {
            (Bundle.main.object(forInfoDictionaryKey: name) as? String).flatMap { $0.isEmpty ? nil : $0 }
        }
        return (key("HerderSentryDSN"), key("HerderPostHogToken"))
    }
}

/// The question on first launch: a card at the bottom of the window, which goes away once
/// answered. Nothing is sent before the person says yes.
struct DiagnosticsAsk: View {
    let answer: (Bool) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Help make herder faster?")
                .font(Theme.title)
                .foregroundStyle(Theme.text)
            Text(DiagnosticsRow.explanation)
                .font(.footnote)
                .foregroundStyle(Theme.secondary)
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 10) {
                Spacer(minLength: 0)
                SettingsButton(title: "Not Now", style: .quiet) { answer(false) }
                    .fixedSize()
                    .accessibilityIdentifier("diagnostics-not-now")
                SettingsButton(title: "Share Diagnostics", style: .primary) { answer(true) }
                    .fixedSize()
                    .accessibilityIdentifier("diagnostics-share")
            }
        }
        .padding(16)
        .frame(maxWidth: 480)
        .background(Theme.raised, in: RoundedRectangle(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
        .padding(16)
    }
}

/// The switch on the You page.
struct DiagnosticsRow: View {
    @Bindable var diagnostics: DiagnosticsSwitch

    static let explanation = "Share how long herder takes to connect, send and load, and its errors and crashes, "
        + "with herder’s developers. Never your prompts, transcripts, files, names or addresses. "
        + "Off until you turn it on; see herder.sh/privacy."

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: "Diagnostics")
            SettingsGroup {
                HStack {
                    Text("Share diagnostics").font(.subheadline).foregroundStyle(Theme.text)
                    Spacer(minLength: 8)
                    Toggle("Share diagnostics", isOn: Binding(get: { diagnostics.on }, set: { diagnostics.set($0) }))
                        .labelsHidden()
                        .toggleStyle(.settings)
                        .accessibilityIdentifier("diagnostics-switch")
                }
                .padding(.horizontal, Theme.cellPadding)
                .frame(minHeight: Theme.rowHeight)
            }
            Text(Self.explanation)
                .font(.footnote).foregroundStyle(Theme.tertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}

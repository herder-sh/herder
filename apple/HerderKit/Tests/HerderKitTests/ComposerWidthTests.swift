#if os(macOS)
import AppKit
@testable import HerderKit
import SwiftUI
import Testing

/// The composer stays within an iPhone's width: a wider one widens the whole session view,
/// which then runs off both sides of the screen.
@MainActor struct ComposerWidthTests {
    @Test func longMachineAndAccountNamesTruncateInsteadOfWideningTheComposer() {
        let machine = SettingsSection(kind: .machine, options: [], choose: { _ in })
        let account = SettingsSection(kind: .account, options: [], choose: { _ in })
        let composer = ComposerBox(
            text: .constant(""), images: .constant([]), files: .constant([]), placeholder: "Ask", models: [],
            current: .init(provider: "claude", model: "claude-opus-4-5"), mode: .fullAccess, running: false,
            choose: { _ in }, setMode: { _ in }, send: {}, stop: {}
        ) {
            FooterMenu(section: machine, text: "Studio MacBook Pro in the office")
            FooterMenu(section: account, text: "a-long-account-label@example.com")
            Spacer()
            Label("herder/a-long-branch-name", systemImage: "arrow.triangle.branch").lineLimit(1)
        }
        let width = NSHostingController(rootView: composer.environment(\.minimumHitSize, 44))
            .sizeThatFits(in: CGSize(width: 370, height: 2000)).width
        #expect(width <= 370)
    }
}
#endif

#if os(macOS)
import AppKit
@testable import HerderKit
import SwiftUI
import Testing

/// The session's composer on an iPhone 18 Pro: 402 points, less the controls' 12 point margins.
@MainActor struct PhoneWidthTests {
    private let width: CGFloat = 378

    /// A machine and an account named as long as real ones get still fit the phone: the footer's
    /// menus truncate rather than push the composer, and the session with it, past the screen.
    @Test func theComposerFitsAPhoneWithLongMachineAndAccountNames() {
        let composer = ComposerBox(
            text: .constant(""), images: .constant([]), placeholder: "Queue a follow-up…",
            models: [], current: .init(provider: "claude", model: "claude-opus-5-5"), mode: .ask, running: true,
            choose: { _ in }, setMode: { _ in }, send: {}, stop: {}
        ) {
            FooterMenu(section: SettingsSection(kind: .machine, options: [], choose: { _ in }), text: "Studio MacBook Pro")
            FooterMenu(section: SettingsSection(kind: .account, options: [], choose: { _ in }), text: "Claude Max · team workspace")
            Spacer()
            Label("herder/manual-connection-ordering", systemImage: "arrow.triangle.branch").lineLimit(1)
        }
        .environment(\.minimumHitSize, 44)
        let size = NSHostingController(rootView: composer).sizeThatFits(in: CGSize(width: width, height: 1000))
        #expect(size.width <= width, "the composer is \(size.width) points wide on a \(width) point phone")
    }
}
#endif

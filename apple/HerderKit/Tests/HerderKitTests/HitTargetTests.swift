#if os(macOS)
import AppKit
@testable import HerderKit
import SwiftUI
import Testing

/// The size a control lays out at, with the iOS hit area or the Mac's.
@MainActor private func size(_ control: some View, ios: Bool) -> CGSize {
    NSHostingView(rootView: control.environment(\.minimumHitSize, ios ? 44 : 0)).fittingSize
}

@MainActor struct HitTargetTests {
    private let controls: [(String, AnyView)] = [
        ("HeaderLabel", AnyView(HeaderLabel(symbol: "info.circle", title: nil))),
        ("labelled HeaderLabel", AnyView(HeaderLabel(symbol: "terminal", title: "Terminal"))),
        ("IconButton", AnyView(IconButton(symbol: "gearshape", help: "Settings") {})),
        ("PaneButton", AnyView(PaneButton(title: "New Session", symbol: "plus") {})),
        ("DictationButton", AnyView(DictationButton(listening: false) {})),
        ("FooterMenu", AnyView(FooterMenu(section: SettingsSection(kind: .machine, options: [], choose: { _ in }), text: "studio"))),
    ]

    @Test func everySharedControlIsAtLeast44PointsOnIOS() {
        for (name, control) in controls {
            let size = size(control, ios: true)
            #expect(size.width >= 44 && size.height >= 44, "\(name) is \(size) on iOS")
        }
    }

    @Test func theMacKeepsItsDenserControls() {
        #expect(size(IconButton(symbol: "gearshape", help: "Settings") {}, ios: false) == CGSize(width: 30, height: 30))
        #expect(size(HeaderLabel(symbol: "info.circle", title: nil), ios: false).height == 32)
        #expect(size(DictationButton(listening: false) {}, ios: false) == CGSize(width: 30, height: 30))
        #expect(size(FooterMenu(section: SettingsSection(kind: .machine, options: [], choose: { _ in }), text: "studio"), ios: false).height < 44)
    }
}
#endif

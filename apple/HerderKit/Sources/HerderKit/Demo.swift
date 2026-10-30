import Foundation
import Herder
import SwiftUI

extension Fleet {
    /// The demo the empty state offers, for App Review and anyone without a daemon: the demo
    /// fleet as recorded into `Resources/demo.json` (scripts/record-demo.sh), shown with no
    /// profile and no connection. Anything that would reach a daemon says it is a demo.
    static func demo() throws -> Fleet {
        guard let url = Bundle.module.url(forResource: "demo", withExtension: "json") else {
            throw HerderError.Local(detail: "The demo is missing from the app.")
        }
        return Fleet(client: try Client.openDemo(demo: try String(contentsOf: url, encoding: .utf8)))
    }
}

extension EnvironmentValues {
    /// Opens the demo; `nil` where there is no demo to open, as inside it.
    @Entry var exploreDemo: (() -> Void)?
}

/// "Explore a demo", for an empty state; nothing where there is no demo to open.
struct ExploreDemoButton: View {
    @Environment(\.exploreDemo) private var explore

    var body: some View {
        if let explore {
            ActionButton(title: "Explore a demo", style: .secondary) { explore() }
                .accessibilityIdentifier("explore-demo")
        }
    }
}

/// The band across the top of the demo: it is one, and the way out.
struct DemoBanner: View {
    let leave: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            Text("Demo")
                .font(Theme.title)
                .foregroundStyle(Theme.text)
            Text("·").foregroundStyle(Theme.tertiary)
            Button("Leave demo", action: leave)
                .buttonStyle(.plain)
                .font(Theme.title)
                .foregroundStyle(Theme.action)
                .accessibilityIdentifier("leave-demo")
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 8)
        .background(Theme.raised)
        .overlay(alignment: .bottom) { Theme.stroke.frame(height: 1) }
    }
}

import SwiftUI

/// A page's header: its title, a live status sentence built from the page's state ("9 open ·
/// 3 working"), then the page's actions, the main one `prominent`, centred on the title's line.
/// The header says what is true now; what a page is for goes in its empty state or an ⓘ, never
/// under the title.
struct PageHeader<Actions: View>: View {
    let title: String
    var status = ""
    /// The status for a narrow pane, where the whole one would not fit on its line ("1 needs
    /// you · 238 tasks"): it stands for the state too.
    var shortStatus: String?
    /// A project's rolled-up state, before the status.
    var state: SessionState?
    /// A project's tile before the title.
    var icon: ProjectIcon?
    /// Makes the title a menu of the app's sections and projects.
    var switcher: Switcher?
    @ViewBuilder var actions: Actions
    @State private var narrow = false
    @Environment(\.pageMargin) private var margin

    /// Below this width the actions are icons, their titles in tooltips.
    static var narrowWidth: CGFloat { 480 }

    var body: some View {
        HStack(alignment: .titleLine, spacing: 8) {
            if let icon { icon.padding(.trailing, 4) }
            // The status follows the title on its line, else takes the line under it, shortened
            // where the whole one would be cut.
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .firstTextBaseline, spacing: 10) { titleView; statusView(status).fixedSize() }
                VStack(alignment: .leading, spacing: 2) { titleView; statusView(status).fixedSize() }
                if let shortStatus {
                    VStack(alignment: .leading, spacing: 2) { titleView; statusView(shortStatus, short: true).fixedSize() }
                }
                VStack(alignment: .leading, spacing: 2) { titleView; statusView(shortStatus ?? status).lineLimit(1) }
            }
            Spacer(minLength: 8)
            // The actions take their room before the title, so a label never wraps.
            HStack(spacing: HeaderControl.spacing) { actions }
                .environment(\.compactActions, narrow)
                .layoutPriority(1)
        }
        .padding(.horizontal, margin)
        .padding(.vertical, 8)
        .frame(minHeight: Theme.headerHeight)
        .onGeometryChange(for: Bool.self) { $0.size.width < Self.narrowWidth } action: { narrow = $0 }
    }

    /// The state's glyph, then its label unless the status is `short`, then `status`.
    @ViewBuilder private func statusView(_ status: String, short: Bool = false) -> some View {
        if state != nil || !status.isEmpty {
            HStack(spacing: 4) {
                if let state {
                    StatusGlyph(state: state, size: 7, pulses: false)
                    if !short { Text(status.isEmpty ? state.label : "\(state.label) ·") }
                }
                if !status.isEmpty { Text(status) }
            }
            .font(Theme.body).foregroundStyle(Theme.secondary)
        }
    }

    @ViewBuilder private var titleView: some View {
        if let switcher {
            Menu {
                switcher.items
            } label: {
                HStack(spacing: 6) {
                    Text(title).font(Theme.page).foregroundStyle(Theme.text).lineLimit(1).titleLine()
                    Image(systemName: "chevron.down").font(.footnote.weight(.bold)).foregroundStyle(Theme.secondary)
                }
                .contentShape(.rect)
            }
            .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden).fixedSize()
            .help("Switch section or project")
        } else {
            Text(title).font(Theme.page).foregroundStyle(Theme.text).lineLimit(1).titleLine()
        }
    }
}

extension EnvironmentValues {
    /// Whether a page header's actions show as icons alone, in a narrow pane (`PaneButton`,
    /// `BoardSortMenu`, `ActivityChip`).
    @Entry var compactActions = false
}

#Preview("PageHeader") {
    VStack(spacing: 0) {
        PageHeader(title: "Board", status: "9 open · 3 working") {
            PaneButton(title: "New Session", symbol: "plus", prominent: true) {}
        }
        PageHeader(title: "Providers", status: "5 accounts · 1 needs sign-in · 3 updates") {
            PaneButton(title: "Update All (3)", symbol: "arrow.down.circle") {}
            PaneButton(title: "Add Account", symbol: "plus", prominent: true) {}
        }
    }
    .frame(width: 760)
    .background(.ground)
}

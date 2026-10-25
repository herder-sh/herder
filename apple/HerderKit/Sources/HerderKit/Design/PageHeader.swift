import SwiftUI

/// A page's header: its title, a live status sentence built from the page's state ("9 open ·
/// 3 working"), then the page's actions, the main one `prominent`. The header says what is
/// true now; what a page is for goes in its empty state or an ⓘ, never under the title.
struct PageHeader<Actions: View>: View {
    let title: String
    var status = ""
    /// A project's rolled-up state, before the status.
    var state: SessionState?
    /// A project's tile before the title.
    var icon: ProjectIcon?
    /// Makes the title a menu of the app's sections and projects.
    var switcher: Switcher?
    @ViewBuilder var actions: Actions

    var body: some View {
        HStack(alignment: .center, spacing: 8) {
            if let icon { icon.padding(.trailing, 4) }
            // The status follows the title on its line, or goes under it in a narrow pane.
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .firstTextBaseline, spacing: 10) { titleView; statusView.fixedSize() }
                VStack(alignment: .leading, spacing: 2) { titleView; statusView }
            }
            Spacer(minLength: 8)
            // The actions take their room before the title, so a label never wraps.
            HStack(spacing: 8) { actions }
                .layoutPriority(1)
        }
        .padding(.horizontal, 20)
        .padding(.top, 22)
        .padding(.bottom, 10)
    }

    @ViewBuilder private var statusView: some View {
        if state != nil || !status.isEmpty {
            HStack(spacing: 4) {
                if let state {
                    StatusGlyph(state: state, size: 7, pulses: false)
                    Text(status.isEmpty ? state.label : "\(state.label) ·")
                }
                if !status.isEmpty { Text(status) }
            }
            .font(Theme.body).foregroundStyle(Theme.secondary).lineLimit(1)
        }
    }

    @ViewBuilder private var titleView: some View {
        if let switcher {
            Menu {
                switcher.items
            } label: {
                HStack(spacing: 6) {
                    Text(title).font(Theme.page).foregroundStyle(Theme.text).lineLimit(1)
                    Image(systemName: "chevron.down").font(.footnote.weight(.bold)).foregroundStyle(Theme.secondary)
                }
                .contentShape(.rect)
            }
            .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden).fixedSize()
            .help("Switch section or project")
        } else {
            Text(title).font(Theme.page).foregroundStyle(Theme.text).lineLimit(1)
        }
    }
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
    .background(Theme.background)
}

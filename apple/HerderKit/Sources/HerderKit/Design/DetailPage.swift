import SwiftUI

/// An object's page: a machine, an account, a project's settings, a vault. Every one has the
/// same anatomy, top to bottom: the header (name, status chip, a live status sentence, the
/// actions), `NeedsAttention` when something is wrong, the settings in `SettingsList` groups,
/// `AboutSection` with the identifiers, collapsed, then `DangerZone`. Each part marked with
/// `detailSection(_:)` is listed in the index on the left of a long page, on the Mac and iPad,
/// which scrolls to it; iPhone pushes the same page with no index.
struct DetailPage<Leading: View, Actions: View, Content: View>: View {
    let title: String
    var chip: StatusChip?
    /// The page's live status sentence: "owner · 2 running · Linux".
    var status = ""
    /// A tile or mark before the title.
    @ViewBuilder var leading: Leading
    @ViewBuilder var actions: Actions
    @ViewBuilder var content: Content
    @State private var sections: [String] = []
    /// The sections scrolled up to the reading line, of which the last is the one being read,
    /// raised in the index; the index's own jump picks it at once.
    @State private var reached: Set<String> = []
    @State private var picked: String?
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    #endif

    /// A page with at least this many sections is long enough for an index.
    static var indexedFrom: Int { 4 }

    var body: some View {
        ScrollViewReader { proxy in
            HStack(alignment: .top, spacing: 0) {
                if showsIndex {
                    SectionIndex(sections: sections, current: picked ?? DetailReading.current(reached, in: sections)) { section in
                        picked = section
                        withAnimation(.easeInOut(duration: 0.2)) { proxy.scrollTo(section, anchor: .top) }
                    }
                    .frame(width: 188)
                    .padding(.top, 26)
                }
                ScrollView {
                    VStack(alignment: .leading, spacing: 26) {
                        header
                        content
                    }
                    .frame(maxWidth: Theme.settingsWidth, alignment: .leading)
                    .padding(.horizontal, Theme.margin)
                    .padding(.bottom, 32)
                    .frame(maxWidth: .infinity)
                }
                .coordinateSpace(.named(DetailReading.space))
            }
        }
        .onPreferenceChange(DetailSections.self) { sections = $0 }
        .environment(\.detailSectionReached) { section, isReached in
            picked = nil
            if isReached { reached.insert(section) } else { reached.remove(section) }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(.ground)
        #if os(iOS)
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
        #endif
    }

    private var showsIndex: Bool {
        #if os(iOS)
        sizeClass == .regular && sections.count >= Self.indexedFrom
        #else
        sections.count >= Self.indexedFrom
        #endif
    }

    private var header: some View {
        HStack(alignment: .center, spacing: 12) {
            leading
            VStack(alignment: .leading, spacing: 4) {
                // The title keeps its width: the chip goes under it where both do not fit.
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 10) {
                        Text(title).font(Theme.page).foregroundStyle(Theme.text).lineLimit(1)
                        if let chip { chip }
                    }
                    VStack(alignment: .leading, spacing: 4) {
                        Text(title).font(Theme.page).foregroundStyle(Theme.text).lineLimit(1)
                        if let chip { chip }
                    }
                }
                if !status.isEmpty {
                    Text(status).font(Theme.body).foregroundStyle(Theme.secondary).lineLimit(2)
                }
            }
            Spacer(minLength: 8)
            HStack(spacing: 8) { actions }
                .layoutPriority(1)
        }
        .padding(.top, 22)
    }
}

extension DetailPage where Leading == EmptyView {
    init(title: String, chip: StatusChip? = nil, status: String = "",
         @ViewBuilder actions: () -> Actions, @ViewBuilder content: () -> Content) {
        self.init(title: title, chip: chip, status: status, leading: { EmptyView() }, actions: actions,
                  content: content)
    }
}

extension DetailPage where Leading == EmptyView, Actions == EmptyView {
    init(title: String, chip: StatusChip? = nil, status: String = "", @ViewBuilder content: () -> Content) {
        self.init(title: title, chip: chip, status: status, leading: { EmptyView() }, actions: { EmptyView() },
                  content: content)
    }
}

/// The sections a `DetailPage` lists in its index, in the order they appear.
struct DetailSections: PreferenceKey {
    static let defaultValue: [String] = []

    static func reduce(value: inout [String], nextValue: () -> [String]) {
        value.append(contentsOf: nextValue())
    }
}

/// Which section of a `DetailPage` is being read. Each section says only when its top crosses
/// the reading line: a position sent up on every frame of a scroll would update the page each
/// frame, and the page lays out every section again.
enum DetailReading {
    static let space = "detail-page"
    /// How far under the scrolled area's top a section still counts as the one being read.
    static let reading: CGFloat = 80

    /// The last of `sections` whose top has scrolled up to the reading line, else the first.
    static func current(_ reached: Set<String>, in sections: [String]) -> String? {
        sections.last { reached.contains($0) } ?? sections.first
    }
}

extension EnvironmentValues {
    /// Tells the `DetailPage` that a section's top has crossed its reading line, up or down.
    @Entry var detailSectionReached: (String, Bool) -> Void = { _, _ in }
}

extension View {
    /// Lists this part of a `DetailPage` in its index, as `title`, and lets the index scroll to it.
    func detailSection(_ title: String) -> some View {
        id(title)
            .preference(key: DetailSections.self, value: [title])
            .modifier(DetailSectionTop(title: title))
    }
}

private struct DetailSectionTop: ViewModifier {
    let title: String
    @Environment(\.detailSectionReached) private var reached

    func body(content: Content) -> some View {
        content.onGeometryChange(for: Bool.self) { proxy in
            proxy.frame(in: .named(DetailReading.space)).minY <= DetailReading.reading
        } action: { isReached in
            reached(title, isReached)
        }
    }
}

/// A long page's sections, each a link that scrolls to it, quiet but for the one being read on
/// the raised surface. A destructive section is no louder than the rest here.
private struct SectionIndex: View {
    let sections: [String]
    let current: String?
    let go: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(sections, id: \.self) { section in
                let selected = section == current
                Button { go(section) } label: {
                    Text(section)
                        .font(Theme.body.weight(selected ? .medium : .regular))
                        .foregroundStyle(selected ? Theme.text : Theme.secondary)
                        .lineLimit(1)
                        .padding(.horizontal, 10)
                        .frame(maxWidth: .infinity, minHeight: 28, alignment: .leading)
                        .background(selected ? Theme.raised : .clear, in: .rect(cornerRadius: 6))
                        .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("section-index-\(section)")
                .accessibilityAddTraits(selected ? .isSelected : [])
            }
        }
        .padding(.leading, 12)
        .padding(.trailing, 4)
    }
}

/// A coloured dot and a word or two after a page's title: "Connected · 38 ms".
struct StatusChip: View {
    let text: String
    var urgency = Urgency.quiet

    var body: some View {
        HStack(spacing: 6) {
            Circle().fill(urgency == .done ? Theme.switchOn : urgency.color).frame(width: 7, height: 7)
            Text(text).lineLimit(1)
        }
        .font(Theme.meta.weight(.medium))
        .foregroundStyle(Theme.secondary)
        .padding(.horizontal, 9)
        .padding(.vertical, 3)
        .background(Theme.raised, in: .capsule)
        .fixedSize()
    }
}

/// What is wrong on a page, first after its header; each `AttentionRow` carries its fix. A page
/// shows it only when there is something in it.
struct NeedsAttention<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: "Needs attention", tint: Theme.accent)
            VStack(spacing: 0) { content }
                .background(.card, in: .rect(cornerRadius: Theme.corner))
                .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.accent.opacity(0.45)))
        }
        .detailSection("Needs attention")
    }
}

/// A problem and its fix, in `NeedsAttention`.
struct AttentionRow<Fix: View>: View {
    let title: String
    var detail = ""
    var urgency = Urgency.needsYou
    @ViewBuilder var fix: Fix

    var body: some View {
        HStack(spacing: 12) {
            Circle().fill(urgency.color).frame(width: 8, height: 8)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text)
                if !detail.isEmpty {
                    Text(detail).font(.caption).foregroundStyle(Theme.tertiary).lineLimit(2)
                }
            }
            Spacer(minLength: 12)
            fix
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 8)
        .frame(minHeight: Theme.tallRowHeight)
    }
}

/// The identifiers of a page's object, folded away until asked for: they are for comparing
/// and copying, not for reading every time.
struct AboutSection<Content: View>: View {
    @ViewBuilder var content: Content
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button { withAnimation(.easeInOut(duration: 0.15)) { expanded.toggle() } } label: {
                HStack(spacing: 6) {
                    SectionHeading(title: "About")
                    Image(systemName: "chevron.right")
                        .font(.caption2.weight(.bold))
                        .foregroundStyle(Theme.secondary)
                        .rotationEffect(.degrees(expanded ? 90 : 0))
                }
                .frame(minHeight: 24)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(expanded ? "Hide About" : "Show About")
            if expanded {
                VStack(alignment: .leading, spacing: 10) { content }
                    .padding(Theme.cellPadding)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(.card, in: .rect(cornerRadius: Theme.corner))
                    .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
            }
        }
        .detailSection("About")
    }
}

/// What cannot be undone from a page, last on it, each a `SettingRow` with a `DangerButton`.
struct DangerZone<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: "Danger zone", tint: Theme.danger)
            VStack(spacing: 0) { content }
                .background(.card, in: .rect(cornerRadius: Theme.corner))
                .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.danger.opacity(0.4)))
        }
        .detailSection("Danger zone")
    }
}

/// The button of a `DangerZone` row: it asks before it acts, so its title ends in "…".
struct DangerButton: View {
    let title: String
    let action: () -> Void
    @Environment(\.isEnabled) private var enabled

    var body: some View {
        Button(title, role: .destructive, action: action)
            .buttonStyle(.plain)
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(Theme.danger)
            .padding(.horizontal, 12)
            .frame(height: 32)
            .background(Theme.raised, in: .rect(cornerRadius: 7))
            .opacity(enabled ? 1 : 0.4)
    }
}

/// Where a setting's last change stands, for the quiet word beside its row.
enum SaveState: Equatable {
    case saving, saved
    case failed(String)
}

/// The saves of a page whose changes apply as they are made, row by row: "Saved" beside a row
/// for a moment after its change lands, or its error until the next change.
@MainActor @Observable final class Autosave {
    private(set) var rows: [String: SaveState] = [:]
    /// How long "Saved" stays.
    var lingers = Duration.seconds(2)
    @ObservationIgnored private var generations: [String: Int] = [:]

    subscript(row: String) -> SaveState? { rows[row] }

    /// Runs `work`, the change to `row`, and shows how it went.
    func save(_ row: String, _ work: () async throws -> Void) async {
        let generation = (generations[row] ?? 0) + 1
        generations[row] = generation
        rows[row] = .saving
        do {
            try await work()
        } catch {
            if generations[row] == generation { rows[row] = .failed(describe(error)) }
            return
        }
        guard generations[row] == generation else { return }
        rows[row] = .saved
        try? await Task.sleep(for: lingers)
        if generations[row] == generation { rows[row] = nil }
    }
}

/// A row's save: a spinner while it goes, a quiet "Saved", or nothing.
struct SaveMark: View {
    let state: SaveState?

    var body: some View {
        switch state {
        case .saving:
            ProgressView().controlSize(.mini)
        case .saved:
            Label("Saved", systemImage: "checkmark")
                .font(Theme.meta)
                .foregroundStyle(Theme.tertiary)
                .transition(.opacity)
        case .failed, nil:
            EmptyView()
        }
    }
}

#Preview("DetailPage") {
    DetailPage(title: "trash-can-01", chip: StatusChip(text: "Connected · 38 ms", urgency: .done),
               status: "owner · 2 running · Linux") {
        PaneButton(title: "Open Terminal", symbol: "terminal", prominent: true) {}
    } content: {
        NeedsAttention {
            AttentionRow(title: "Claude · ops is signed out", detail: "2 sessions waiting") {
                PillButton(title: "Sign in again", prominent: true) {}
            }
            RowDivider()
            AttentionRow(title: "2 CLI updates", detail: "Claude 2.1.303 → 2.1.310") {
                PillButton(title: "Update both") {}
            }
        }
        SettingsList(title: "Capacity") {
            SettingRow(label: "Turns at once", hint: "16 GB, 8 cores · about 6 fit", saved: .saved) {
                Text("4").foregroundStyle(Theme.text)
            }
        }
        .detailSection("Capacity")
        SettingsList(title: "Connection") {
            SettingRow(label: "Name on this device", saved: .failed("The machine is offline")) {
                Text("trash-can-01").foregroundStyle(Theme.secondary)
            }
        }
        .detailSection("Connection")
        AboutSection {
            DetailRow(label: "Host id", value: "h-3f9a", mono: true)
        }
        DangerZone {
            SettingRow(label: "Forget this machine", hint: "Pair again to get it back") {
                DangerButton(title: "Forget…") {}
            }
        }
    }
    .frame(width: 960, height: 760)
}

import Herder
import SwiftUI

/// The composer's model button: the provider's logo and the model's name, opening the model
/// menu in a popover.
struct ModelPicker: View {
    let groups: [ModelCatalog.Group]
    let current: ModelCatalog.Choice
    let choose: (ModelCatalog.Choice) -> Void
    @State private var open = false
    @State private var hovering = false

    var body: some View {
        Button { open.toggle() } label: {
            HStack(spacing: 7) {
                ProviderMark(provider: current.provider, size: 14)
                Text(ModelCatalog.name(current.model, provider: current.provider)).lineLimit(1)
                Image(systemName: "chevron.down").font(.caption2.weight(.semibold))
            }
            .font(.subheadline.weight(.medium))
            .foregroundStyle(open || hovering ? Theme.text : Theme.secondary)
            .padding(.horizontal, 8)
            .frame(height: 32)
            .background(open ? Theme.raised : .clear, in: .rect(cornerRadius: 8))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .fixedSize()
        .onHover { hovering = $0 }
        .help("Model")
        .accessibilityIdentifier("model-picker")
        .popover(isPresented: $open, arrowEdge: .top) {
            ModelMenu(groups: groups, current: current) { choice in
                open = false
                choose(choice)
            } dismiss: {
                open = false
            }
            .presentationCompactAdaptation(.popover)
        }
    }
}

/// The model menu: models grouped by provider under its logo, the current one checked, a
/// filter when the list is long, and "Other…" for a model by name. Up and down move, return
/// picks, escape closes.
struct ModelMenu: View {
    let groups: [ModelCatalog.Group]
    let current: ModelCatalog.Choice
    let choose: (ModelCatalog.Choice) -> Void
    let dismiss: () -> Void
    @State private var query = ""
    /// Typing a model by name rather than filtering.
    @State private var custom = false
    @State private var highlighted: Entry?
    @FocusState private var focus: Focus?

    private enum Focus { case field, list }

    enum Entry: Hashable {
        case model(ModelCatalog.Choice)
        case other
    }

    /// Lists this long get a filter.
    static let filterFrom = 7

    private var filtered: [ModelCatalog.Group] { custom ? [] : ModelCatalog.filter(groups, query) }
    private var entries: [Entry] {
        filtered.flatMap { group in group.models.map { Entry.model(.init(provider: group.provider, model: $0.id)) } }
            + (custom ? [] : [.other])
    }
    private var showsField: Bool { custom || groups.map(\.models.count).reduce(0, +) >= Self.filterFrom }
    private var typed: String { query.trimmingCharacters(in: .whitespaces) }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if showsField {
                field
                Rectangle().fill(Theme.stroke).frame(height: 1)
            }
            if custom {
                Text("Return uses it with \(ModelCatalog.providerName(current.provider)). Escape goes back.")
                    .font(.caption)
                    .foregroundStyle(Theme.tertiary)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
            } else {
                list
            }
        }
        .frame(width: 320)
        .background(Theme.surface)
        .focusable(!showsField)
        .focusEffectDisabled()
        .focused($focus, equals: .list)
        .onKeyPress(keys: [.upArrow, .downArrow, .return, .escape], action: key)
        .onAppear {
            highlighted = .model(current)
            focus = showsField ? .field : .list
        }
        .onChange(of: query) {
            highlighted = query.isEmpty ? .model(current) : entries.first
        }
    }

    private var field: some View {
        HStack(spacing: 8) {
            Image(systemName: custom ? "character.cursor.ibeam" : "magnifyingglass")
                .foregroundStyle(Theme.tertiary)
            TextField(custom ? "Model name, as the CLI writes it" : "Search models", text: $query)
                .textFieldStyle(.plain)
                .foregroundStyle(Theme.text)
                .focused($focus, equals: .field)
                .onKeyPress(keys: [.upArrow, .downArrow, .return, .escape], action: key)
                .accessibilityIdentifier("model-search")
        }
        .font(.subheadline)
        .padding(.horizontal, 14)
        .frame(height: 40)
    }

    private var list: some View {
        ScrollViewReader { proxy in
            ScrollView {
                ModelMenuRows(groups: filtered, current: current, query: typed,
                              highlighted: $highlighted, pick: activate)
            }
            .frame(maxHeight: 400)
            .fixedSize(horizontal: false, vertical: true)
            .onChange(of: highlighted) {
                if let highlighted { proxy.scrollTo(highlighted) }
            }
        }
    }

    private func key(_ press: KeyPress) -> KeyPress.Result {
        switch press.key {
        case .upArrow: move(-1)
        case .downArrow: move(1)
        case .return: activate(highlighted)
        default:
            if custom {
                custom = false
                query = ""
                focus = showsField ? .field : .list
            } else {
                dismiss()
            }
        }
        return .handled
    }

    private func move(_ step: Int) {
        let entries = entries
        guard !entries.isEmpty else { return }
        let index = highlighted.flatMap { entries.firstIndex(of: $0) } ?? (step > 0 ? -1 : entries.count)
        highlighted = entries[(index + step + entries.count) % entries.count]
    }

    private func activate(_ entry: Entry?) {
        if custom || entry == .other {
            if !typed.isEmpty {
                choose(.init(provider: current.provider, model: typed))
            } else if !custom {
                custom = true
                focus = .field
            }
        } else if case .model(let choice) = entry {
            choose(choice)
        }
    }
}

/// The menu's rows: a heading with the logo per provider, its models, then "Other…".
struct ModelMenuRows: View {
    let groups: [ModelCatalog.Group]
    let current: ModelCatalog.Choice
    let query: String
    @Binding var highlighted: ModelMenu.Entry?
    let pick: (ModelMenu.Entry) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            ForEach(groups) { group in
                HStack(spacing: 7) {
                    ProviderMark(provider: group.provider, size: 13)
                        .foregroundStyle(Theme.secondary)
                    Text(ModelCatalog.providerName(group.provider).uppercased())
                        .font(.caption2.weight(.semibold))
                        .tracking(0.6)
                        .foregroundStyle(Theme.tertiary)
                }
                .padding(.horizontal, 10)
                .padding(.top, group.id == groups.first?.id ? 4 : 10)
                .padding(.bottom, 4)
                ForEach(group.models, id: \.id) { model in
                    let choice = ModelCatalog.Choice(provider: group.provider, model: model.id)
                    row(.model(choice)) {
                        Text(model.name).foregroundStyle(Theme.text).lineLimit(1)
                        Spacer(minLength: 8)
                        if let detail = model.detail {
                            Text(detail).font(.caption).foregroundStyle(Theme.tertiary).lineLimit(1)
                        }
                        Image(systemName: "checkmark")
                            .font(.caption.weight(.bold))
                            .foregroundStyle(Theme.text)
                            .opacity(choice == current ? 1 : 0)
                            .frame(width: 14)
                    }
                }
            }
            if !groups.isEmpty { Rectangle().fill(Theme.stroke).frame(height: 1).padding(.vertical, 5) }
            row(.other) {
                Image(systemName: "pencil")
                    .foregroundStyle(Theme.secondary)
                Text(query.isEmpty ? "Other…" : "Use \u{201C}\(query)\u{201D}")
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                Spacer()
            }
        }
        .padding(6)
    }

    private func row(_ entry: ModelMenu.Entry, @ViewBuilder content: () -> some View) -> some View {
        Button { pick(entry) } label: {
            HStack(spacing: 8) { content() }
                .font(.subheadline)
                .padding(.horizontal, 10)
                .frame(height: 30)
                .background(highlighted == entry ? Theme.raised : .clear, in: .rect(cornerRadius: 7))
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .onHover { if $0 { highlighted = entry } }
        .id(entry)
    }
}

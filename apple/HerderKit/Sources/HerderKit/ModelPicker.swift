import Herder
import SwiftUI

/// The composer's settings button: the provider's logo and the model's name, opening the
/// session's settings (model, account, machine) in a popover.
struct ModelPicker: View {
    let groups: [ModelCatalog.Group]
    let current: ModelCatalog.Choice
    var sections: [SettingsSection] = []
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
            .hitTarget()
        }
        .buttonStyle(.plain)
        .fixedSize()
        .onHover { hovering = $0 }
        .help(sections.isEmpty ? "Model" : "Model, account and machine")
        .accessibilityIdentifier("model-picker")
        .popover(isPresented: $open, arrowEdge: .top) {
            ModelMenu(groups: groups, current: current, sections: sections) { choice in
                open = false
                choose(choice)
            } dismiss: {
                open = false
            }
            .presentationCompactAdaptation(.popover)
        }
    }
}

/// One row of a settings section: an account or a machine.
struct SettingsOption: Hashable, Identifiable {
    let id: String
    let title: String
    var detail: String?
    /// The provider an account signs in to, shown as its mark.
    var provider: Provider?
    /// The busiest usage window's share, 0 to 100, for an account that reports usage.
    var usage: Double?
    var current = false
    /// Why it cannot be picked, if it cannot.
    var unavailable: String?

    /// Accounts on a machine, each with its provider and busiest usage window.
    static func accounts(_ accounts: [Account], current: AccountId?) -> [SettingsOption] {
        accounts.map { account in
            let busiest = account.usage.max { $0.usedPercent < $1.usedPercent }
            return SettingsOption(
                id: account.accountId, title: account.label,
                detail: busiest.map { "\(Lists.usageLabel($0.window)) \(Int($0.usedPercent.rounded()))%" },
                provider: account.provider, usage: busiest?.usedPercent, current: account.accountId == current)
        }
    }

    /// The machines, the current one first; the others say why they cannot be picked.
    static func machines(_ machines: [Machine], current: HostId, unavailable: (Machine) -> String?) -> [SettingsOption] {
        machines.sorted { $0.hostId == current && $1.hostId != current }.map { machine in
            let reason = machine.hostId == current ? nil : unavailable(machine)
            return SettingsOption(id: machine.hostId, title: machine.name, detail: reason,
                                  current: machine.hostId == current, unavailable: reason)
        }
    }
}

/// A settings section after the models: the accounts or the machines, and what picking one
/// does.
struct SettingsSection: Identifiable {
    enum Kind: String {
        case account = "Account", machine = "Machine"

        var symbol: String { self == .account ? "person.crop.circle" : "desktopcomputer" }
    }

    let kind: Kind
    let options: [SettingsOption]
    /// What picking another option does, when that is more than a switch.
    var hint: String?
    /// A last row that is not an option, as "Fork Session…" under the machines.
    var action: Action?
    let choose: (SettingsOption.ID) -> Void
    var id: Kind { kind }

    /// Picks an entry of the section: another option, or its action; the current option
    /// changes nothing.
    func perform(_ entry: ModelMenu.Entry) {
        switch entry {
        case .option(_, let id) where options.first(where: { $0.id == id })?.current == false: choose(id)
        case .action: action?.run()
        default: break
        }
    }

    struct Action {
        let title: String
        let symbol: String
        var enabled = true
        let run: () -> Void
    }
}

/// The settings menu: models grouped by provider under its logo, the current one checked, a
/// filter when the list is long, and "Other…" for a model by name; then the account and
/// machine sections. Up and down move, return picks, escape closes.
struct ModelMenu: View {
    let groups: [ModelCatalog.Group]
    let current: ModelCatalog.Choice
    var sections: [SettingsSection] = []
    let choose: (ModelCatalog.Choice) -> Void
    let dismiss: () -> Void
    @State private var query = ""
    /// Typing a model by name rather than filtering.
    @State private var custom = false
    @State private var highlighted: Entry?
    @FocusState private var focus: Focus?

    private enum Focus { case field, list }

    enum Entry: Hashable {
        /// The settings section it is in, if any.
        var section: SettingsSection.Kind? {
            switch self {
            case .option(let kind, _), .action(let kind): kind
            default: nil
            }
        }

        case model(ModelCatalog.Choice)
        case other
        case option(SettingsSection.Kind, SettingsOption.ID)
        case action(SettingsSection.Kind)
    }

    /// Lists this long get a filter.
    static let filterFrom = 7

    private var filtered: [ModelCatalog.Group] { custom ? [] : ModelCatalog.filter(groups, query) }
    /// The sections show while the menu is not filtering.
    private var shownSections: [SettingsSection] { custom || !typed.isEmpty ? [] : sections }
    private var entries: [Entry] {
        filtered.flatMap { group in group.models.map { Entry.model(.init(provider: group.provider, model: $0.id)) } }
            + (custom ? [] : [.other])
            + shownSections.flatMap { section in
                section.options.filter { $0.unavailable == nil }.map { Entry.option(section.kind, $0.id) }
                    + (section.action?.enabled == true ? [.action(section.kind)] : [])
            }
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
        .frame(width: 340)
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
                ModelMenuRows(groups: filtered, current: current, sections: shownSections, query: typed,
                              highlighted: $highlighted, pick: activate)
            }
            .frame(maxHeight: 520)
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
        } else if let entry, let section = sections.first(where: { $0.kind == entry.section }) {
            dismiss()
            section.perform(entry)
        }
    }
}

/// The menu's rows: under "Model" a heading with the logo per provider, its models, then
/// "Other…"; then each settings section.
struct ModelMenuRows: View {
    let groups: [ModelCatalog.Group]
    let current: ModelCatalog.Choice
    var sections: [SettingsSection] = []
    let query: String
    @Binding var highlighted: ModelMenu.Entry?
    let pick: (ModelMenu.Entry) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            if !sections.isEmpty { MenuHeading(title: "Model") }
            ForEach(groups) { group in
                HStack(spacing: 7) {
                    ProviderMark(provider: group.provider, size: 13)
                    Text(ModelCatalog.providerName(group.provider))
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Theme.secondary)
                }
                .padding(.horizontal, 10)
                .padding(.top, group.id == groups.first?.id ? 4 : 10)
                .padding(.bottom, 4)
                ForEach(group.models, id: \.id) { model in
                    let choice = ModelCatalog.Choice(provider: group.provider, model: model.id)
                    MenuRow(entry: .model(choice), highlighted: $highlighted, pick: pick) {
                        Text(model.name).foregroundStyle(Theme.text).lineLimit(1)
                        Spacer(minLength: 8)
                        if let detail = model.detail {
                            Text(detail).font(.caption).foregroundStyle(Theme.tertiary).lineLimit(1)
                        }
                        Checkmark(shown: choice == current)
                    }
                }
            }
            if !groups.isEmpty { MenuDivider() }
            MenuRow(entry: .other, highlighted: $highlighted, pick: pick) {
                Image(systemName: "pencil")
                    .foregroundStyle(Theme.secondary)
                    .frame(width: 16)
                Text(query.isEmpty ? "Other…" : "Use \u{201C}\(query)\u{201D}")
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                Spacer()
            }
            ForEach(sections) { section in
                MenuDivider()
                SettingsSectionRows(section: section, highlighted: $highlighted, pick: pick)
            }
        }
        .padding(6)
    }
}

/// A settings section's rows under its heading: the option's icon and name, its detail (an
/// account's usage with a meter), the current one checked.
struct SettingsSectionRows: View {
    let section: SettingsSection
    @Binding var highlighted: ModelMenu.Entry?
    let pick: (ModelMenu.Entry) -> Void

    var body: some View {
        MenuHeading(title: section.kind.rawValue, hint: section.hint)
        ForEach(section.options) { option in
            MenuRow(entry: .option(section.kind, option.id), highlighted: $highlighted, pick: pick) {
                if let provider = option.provider {
                    ProviderMark(provider: provider, size: 14).frame(width: 16)
                } else {
                    Image(systemName: section.kind.symbol)
                        .foregroundStyle(option.current ? Theme.text : Theme.secondary)
                        .frame(width: 16)
                }
                Text(option.title).foregroundStyle(Theme.text).lineLimit(1)
                Spacer(minLength: 8)
                if let usage = option.usage {
                    UsageMeter(percent: usage)
                }
                if let detail = option.detail {
                    Text(detail).font(.caption).foregroundStyle(Theme.tertiary).lineLimit(1)
                }
                Checkmark(shown: option.current)
            }
            .disabled(option.unavailable != nil)
            .opacity(option.unavailable == nil ? 1 : 0.5)
        }
        if let action = section.action {
            MenuRow(entry: .action(section.kind), highlighted: $highlighted, pick: pick) {
                Image(systemName: action.symbol).foregroundStyle(Theme.secondary).frame(width: 16)
                Text(action.title).foregroundStyle(Theme.text).lineLimit(1)
                Spacer()
            }
            .disabled(!action.enabled)
            .opacity(action.enabled ? 1 : 0.5)
        }
    }
}

/// A section's heading in a menu, with a hint on the right.
struct MenuHeading: View {
    let title: String
    var hint: String?

    var body: some View {
        HStack {
            Text(title.uppercased()).tracking(0.6)
            Spacer()
            if let hint { Text(hint).fontWeight(.regular) }
        }
        .font(.caption2.weight(.semibold))
        .foregroundStyle(Theme.tertiary)
        .padding(.horizontal, 10)
        .padding(.top, 6)
        .padding(.bottom, 2)
    }
}

/// A row of a menu: highlighted under the pointer or the arrow keys.
struct MenuRow<Content: View>: View {
    let entry: ModelMenu.Entry
    @Binding var highlighted: ModelMenu.Entry?
    let pick: (ModelMenu.Entry) -> Void
    @ViewBuilder var content: Content
    @Environment(\.isEnabled) private var enabled

    var body: some View {
        Button { pick(entry) } label: {
            HStack(spacing: 8) { content }
                .font(.subheadline)
                .padding(.horizontal, 10)
                .frame(height: 30)
                .background(enabled && highlighted == entry ? Theme.raised : .clear, in: .rect(cornerRadius: 7))
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .onHover { if $0 && enabled { highlighted = entry } }
        .id(entry)
    }
}

private struct MenuDivider: View {
    var body: some View {
        Rectangle().fill(Theme.stroke).frame(height: 1).padding(.vertical, 5)
    }
}

private struct Checkmark: View {
    let shown: Bool

    var body: some View {
        Image(systemName: "checkmark")
            .font(.caption.weight(.bold))
            .foregroundStyle(Theme.text)
            .opacity(shown ? 1 : 0)
            .frame(width: 14)
    }
}

/// How much of an account's busiest window is used, as a short bar that warms as it fills.
struct UsageMeter: View {
    let percent: Double

    var body: some View {
        Capsule().fill(Theme.stroke)
            .frame(width: 28, height: 4)
            .overlay(alignment: .leading) {
                Capsule().fill(percent >= 90 ? Theme.failure : percent >= 70 ? Theme.accent : Theme.secondary)
                    .frame(width: 28 * min(max(percent, 0), 100) / 100)
            }
    }
}

/// A footer item under the composer: an icon, the current option and a chevron, opening the
/// section's rows in a popover.
struct FooterMenu: View {
    let section: SettingsSection
    let text: String
    var help: String?
    @State private var open = false
    @State private var highlighted: ModelMenu.Entry?

    var body: some View {
        Button { open.toggle() } label: {
            HStack(spacing: 5) {
                Image(systemName: section.kind.symbol)
                Text(text).lineLimit(1)
                Image(systemName: "chevron.down").font(.caption2)
            }
            .foregroundStyle(open ? Theme.secondary : Theme.tertiary)
            .hitTarget()
        }
        .buttonStyle(.plain)
        .fixedSize()
        .help(help ?? section.kind.rawValue)
        .popover(isPresented: $open, arrowEdge: .bottom) {
            VStack(alignment: .leading, spacing: 1) {
                SettingsSectionRows(section: section, highlighted: $highlighted) { entry in
                    open = false
                    section.perform(entry)
                }
            }
            .padding(6)
            .frame(width: 300)
            .background(Theme.surface)
            .presentationCompactAdaptation(.popover)
        }
    }
}

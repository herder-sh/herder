import Herder
import SwiftUI

/// The prompt box: the text on top, and inside its bottom edge the model and permission menus
/// with the send (or stop) button; a footer bar under it says where the session runs.
struct ComposerBox<Footer: View>: View {
    @Binding var text: String
    let placeholder: String
    /// The provider whose models the menu offers.
    let provider: Provider?
    /// The model in use, by id; `""` for the provider's default.
    let model: String
    /// Models used on this machine with this provider, offered after the catalog's.
    let usedModels: [String]
    /// Offers "Default model" first, for a session not created yet.
    var offersDefault = false
    let providers: [Provider]
    let mode: PermissionMode?
    let running: Bool
    let setModel: (String) -> Void
    let setProvider: (Provider) -> Void
    let setMode: (PermissionMode) -> Void
    let send: () -> Void
    let stop: () -> Void
    @ViewBuilder var footer: Footer
    @FocusState private var focused: Bool
    @State private var otherModel = false
    @State private var typedModel = ""

    var body: some View {
        VStack(spacing: 0) {
            VStack(spacing: 0) {
                TextField(placeholder, text: $text, axis: .vertical)
                    .textFieldStyle(.plain)
                    .font(.body)
                    .foregroundStyle(Theme.text)
                    .lineLimit(2...12)
                    .focused($focused)
                    .onSubmit(send)
                    .padding(.horizontal, 18)
                    .padding(.top, 16)
                    .padding(.bottom, 8)
                    .frame(maxWidth: .infinity, alignment: .topLeading)
                    .accessibilityIdentifier("composer")
                HStack(spacing: 4) {
                    Menu {
                        if providers.count > 1 {
                            Section("Provider") {
                                ForEach(providers, id: \.self) { provider in Button(provider) { setProvider(provider) } }
                            }
                        }
                        Section("Model") {
                            ForEach(menuModels, id: \.self) { id in
                                Button { setModel(id) } label: {
                                    let name = ModelCatalog.name(id, provider: provider)
                                    if id == model { Label(name, systemImage: "checkmark") } else { Text(name) }
                                }
                            }
                            Button("Other…") { otherModel = true }
                        }
                    } label: {
                        MenuLabel(symbol: "sparkle", text: ModelCatalog.name(model, provider: provider))
                    }
                    .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden).fixedSize()
                    Divider().frame(height: 16).overlay(Theme.stroke)
                    Menu {
                        ForEach([PermissionMode.readOnly, .ask, .autoEdit, .fullAccess], id: \.self) { option in
                            Button { setMode(option) } label: {
                                if option == mode { Label(option.label, systemImage: "checkmark") } else { Text(option.label) }
                            }
                        }
                    } label: {
                        MenuLabel(symbol: mode == .fullAccess ? "lock.open" : "lock", text: mode?.label ?? "Permissions")
                    }
                    .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden).fixedSize()
                    Spacer()
                    if running && trimmed.isEmpty {
                        CircleButton(symbol: "stop.fill", help: "Interrupt", action: stop)
                    } else {
                        CircleButton(symbol: "arrow.up", help: "Send", action: send)
                            .disabled(trimmed.isEmpty)
                            .opacity(trimmed.isEmpty ? 0.35 : 1)
                            .keyboardShortcut(.return, modifiers: .command)
                    }
                }
                .padding(.horizontal, 10)
                .padding(.bottom, 10)
            }
            .background(Theme.surface, in: .rect(cornerRadius: 22))
            .overlay(RoundedRectangle(cornerRadius: 22).strokeBorder(focused ? Theme.secondary.opacity(0.5) : Theme.stroke))
            .contentShape(.rect)
            .onTapGesture { focused = true }
            HStack(spacing: 14) { footer }
                .font(.footnote)
                .foregroundStyle(Theme.tertiary)
                .padding(.horizontal, 16)
                .padding(.vertical, 8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Theme.surface.opacity(0.6),
                            in: UnevenRoundedRectangle(bottomLeadingRadius: 14, bottomTrailingRadius: 14))
                .padding(.horizontal, 18)
        }
        .onAppear { focused = true }
        .alert("Model", isPresented: $otherModel) {
            TextField("Model name", text: $typedModel)
            Button("Use") { if !typedModel.trimmingCharacters(in: .whitespaces).isEmpty { setModel(typedModel) } }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("A model in the provider's naming.")
        }
    }

    private var trimmed: String { text.trimmingCharacters(in: .whitespacesAndNewlines) }

    /// The catalog's models, then the ones used here, then the provider's default for a draft.
    private var menuModels: [String] {
        var ids = ModelCatalog.models(provider ?? "").map(\.id)
        for id in usedModels + [model] where !id.isEmpty && !ids.contains(id) { ids.append(id) }
        if offersDefault { ids.append("") }
        return ids
    }
}

/// A menu's label in the composer: icon, text and a chevron.
struct MenuLabel: View {
    let symbol: String
    let text: String

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: symbol).imageScale(.small)
            Text(text).lineLimit(1)
            Image(systemName: "chevron.down").font(.caption2.weight(.semibold))
        }
        .font(.subheadline.weight(.medium))
        .foregroundStyle(Theme.secondary)
        .padding(.horizontal, 8)
        .frame(height: 32)
        .contentShape(.rect)
    }
}

/// A footer item: an icon and text, as a menu when it can change.
struct FooterItem<Items: View>: View {
    let symbol: String
    let text: String
    @ViewBuilder var items: Items

    var body: some View {
        Menu {
            items
        } label: {
            HStack(spacing: 5) {
                Image(systemName: symbol)
                Text(text).lineLimit(1)
                Image(systemName: "chevron.down").font(.caption2)
            }
            .contentShape(.rect)
        }
        .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden).fixedSize()
    }
}

/// A round send/stop button in the primary colour.
struct CircleButton: View {
    let symbol: String
    let help: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.callout.weight(.bold))
                .foregroundStyle(Theme.onPrimary)
                .frame(width: 34, height: 34)
                .background(Theme.primary, in: .circle)
                .contentShape(.circle)
        }
        .buttonStyle(.plain)
        .help(help)
        .accessibilityLabel(help)
    }
}

extension Fleet {
    /// Models used on a machine with a provider, newest sessions first, for the model menu.
    func models(on hostId: HostId, provider: Provider?) -> [String] {
        var seen: [String] = []
        for session in sessions.values.sorted(by: { ($0.updatedAt ?? .distantPast) > ($1.updatedAt ?? .distantPast) })
        where session.key.hostId == hostId && session.provider == provider {
            if let model = session.model, !seen.contains(model) { seen.append(model) }
        }
        return seen
    }
}

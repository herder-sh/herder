import Herder
import SwiftUI
import UniformTypeIdentifiers
#if os(macOS)
import AppKit
#endif

/// The prompt box: the text on top, and inside its bottom edge the model and permission menus
/// with the send (or stop) button; a footer bar under it says where the session runs.
struct ComposerBox<Footer: View>: View {
    @Binding var text: String
    /// Images going with the prompt: pasted, dropped or attached.
    @Binding var images: [Herder.Image]
    let placeholder: String
    /// The model menu's groups, from `ModelCatalog.groups`.
    let models: [ModelCatalog.Group]
    /// The provider and model in use; `""` is the provider's default.
    let current: ModelCatalog.Choice
    let mode: PermissionMode?
    let running: Bool
    let choose: (ModelCatalog.Choice) -> Void
    let setMode: (PermissionMode) -> Void
    let send: () -> Void
    let stop: () -> Void
    @ViewBuilder var footer: Footer
    @FocusState private var focused: Bool
    @State private var imageError: String?
    @State private var dictation = Dictation()
    /// The text before dictation started; what is heard follows it.
    @State private var dictatedAfter = ""
    #if os(macOS)
    @State private var pasteMonitor: Any?
    #endif

    var body: some View {
        VStack(spacing: 0) {
            VStack(spacing: 0) {
                if !images.isEmpty { AttachmentStrip(images: images, remove: remove) }
                TextField(placeholder, text: $text, axis: .vertical)
                    .textFieldStyle(.plain)
                    .font(.body)
                    .foregroundStyle(Theme.text)
                    .lineLimit(2...12)
                    .focused($focused)
                    .onSubmit(submit)
                    .onKeyPress(.return, phases: .down) { press in
                        guard press.modifiers.contains(.shift) else { return .ignored }
                        text = ListContinuation.newline(after: text)
                        return .handled
                    }
                    .padding(.horizontal, 18)
                    .padding(.top, 16)
                    .padding(.bottom, 8)
                    .frame(maxWidth: .infinity, alignment: .topLeading)
                    .accessibilityIdentifier("composer")
                HStack(spacing: 4) {
                    ModelPicker(groups: models, current: current, choose: choose)
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
                    if let problem = imageError ?? dictation.error {
                        Text(problem).font(.caption).foregroundStyle(Theme.failure).lineLimit(2)
                    }
                    Button(action: toggleDictation) {
                        SwiftUI.Image(systemName: dictation.listening ? "mic.fill" : "mic")
                            .font(.callout.weight(.semibold))
                            .foregroundStyle(dictation.listening ? Theme.accent : Theme.secondary)
                            .frame(width: 30, height: 30)
                            .background(dictation.listening ? Theme.accent.opacity(0.18) : .clear, in: .circle)
                            .contentShape(.rect)
                            .symbolEffect(.pulse, isActive: dictation.listening)
                    }
                    .buttonStyle(.plain)
                    .keyboardShortcut("d", modifiers: [.command, .shift])
                    .help(dictation.listening ? "Stop dictating (⇧⌘D)" : "Dictate, on this device (⇧⌘D)")
                    #if os(macOS)
                    Button(action: attach) {
                        SwiftUI.Image(systemName: "paperclip").font(.callout.weight(.semibold))
                            .foregroundStyle(Theme.secondary).frame(width: 30, height: 30).contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .help("Attach images (or paste or drop them)")
                    #endif
                    if running && trimmed.isEmpty && images.isEmpty {
                        CircleButton(symbol: "stop.fill", help: "Interrupt", action: stop)
                    } else {
                        CircleButton(symbol: "arrow.up", help: "Send", action: submit)
                            .disabled(trimmed.isEmpty && images.isEmpty)
                            .opacity(trimmed.isEmpty && images.isEmpty ? 0.35 : 1)
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
            .onDrop(of: [.image], isTargeted: nil) { providers in
                Task { add(await ImageAttachment.load(providers)) }
                return true
            }
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
        #if os(macOS)
        .onChange(of: focused) { watchPaste(focused) }
        .onDisappear { watchPaste(false) }
        #endif
    }

    private var trimmed: String { text.trimmingCharacters(in: .whitespacesAndNewlines) }

    private func submit() {
        dictation.stop()
        send()
    }

    private func toggleDictation() {
        if dictation.listening {
            dictation.stop()
            return
        }
        dictatedAfter = text.isEmpty || text.hasSuffix(" ") || text.hasSuffix("\n") ? text : text + " "
        focused = true
        Task { await dictation.start { heard in text = dictatedAfter + heard } }
    }

    /// Removes an image and its marker, renumbering the markers after it.
    private func remove(_ index: Int) {
        images.remove(at: index)
        text = text.replacingOccurrences(of: "[Image #\(index + 1)] ", with: "")
            .replacingOccurrences(of: "[Image #\(index + 1)]", with: "")
        for number in (index + 2)...(images.count + 1) where number > index + 1 {
            text = text.replacingOccurrences(of: "[Image #\(number)]", with: "[Image #\(number - 1)]")
        }
    }

    /// Adds images and a `[Image #N]` marker for each to the text, numbered in the order they
    /// go to the agent, so the prompt can refer to them.
    private func add(_ added: [Herder.Image]) {
        for image in added {
            images.append(image)
            let marker = "[Image #\(images.count)]"
            text += text.isEmpty || text.hasSuffix(" ") || text.hasSuffix("\n") ? marker + " " : " " + marker + " "
        }
    }

    #if os(macOS)
    /// While the box has focus, ⌘V with an image on the clipboard attaches it; text pastes as usual.
    private func watchPaste(_ on: Bool) {
        if let pasteMonitor { NSEvent.removeMonitor(pasteMonitor) }
        pasteMonitor = nil
        guard on else { return }
        pasteMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
            guard event.modifierFlags.contains(.command), event.charactersIgnoringModifiers == "v" else { return event }
            let pasted = ImageAttachment.fromPasteboard()
            guard !pasted.isEmpty else { return event }
            add(pasted)
            return nil
        }
    }

    private func attach() {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.image]
        panel.allowsMultipleSelection = true
        guard panel.runModal() == .OK else { return }
        do {
            add(try panel.urls.map { url in
                try ImageAttachment.make(try Data(contentsOf: url), type: UTType(filenameExtension: url.pathExtension))
            })
            imageError = nil
        } catch {
            imageError = error.localizedDescription
        }
    }
    #endif
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

extension Fleet {
    /// The providers a machine has accounts for.
    func providers(on hostId: HostId) -> [Provider] {
        Array(Set(machines.first { $0.hostId == hostId }?.accounts.map(\.provider) ?? []))
    }

    /// The model menu's groups on a machine: the providers' models with the ones used there.
    func modelGroups(
        on hostId: HostId, providers: [Provider], current: ModelCatalog.Choice, offersDefault: Bool
    ) -> [ModelCatalog.Group] {
        ModelCatalog.groups(
            providers: providers, current: current,
            used: Dictionary(uniqueKeysWithValues: Set(providers).map { ($0, models(on: hostId, provider: $0)) }),
            offersDefault: offersDefault)
    }

    /// What a new session on a machine starts on: the default provider there, on its default
    /// model.
    func draftChoice(on hostId: HostId, projectId: String?) -> ModelCatalog.Choice {
        let provider = defaultProvider(on: hostId, projectId: projectId) ?? ""
        return ModelCatalog.Choice(provider: provider, model: ModelCatalog.defaultModel(provider))
    }

    /// A draft's choice once it moves to another machine: kept while that machine has an
    /// account for its provider, else that machine's default.
    func draftChoice(_ choice: ModelCatalog.Choice, movedTo hostId: HostId, projectId: String?) -> ModelCatalog.Choice {
        providers(on: hostId).contains(choice.provider) ? choice : draftChoice(on: hostId, projectId: projectId)
    }
}

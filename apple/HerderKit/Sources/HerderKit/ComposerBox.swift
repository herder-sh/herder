import Herder
import SwiftUI
import UniformTypeIdentifiers
#if os(macOS)
import AppKit
#else
import GameController
import PhotosUI
#endif

/// The prompt box: the text on top, and inside its bottom edge the settings and permission menus
/// with the send (or stop) button; a footer bar under it says where the session runs.
struct ComposerBox<Footer: View>: View {
    @Binding var text: String
    /// Images going with the prompt: pasted, dropped or attached.
    @Binding var images: [Herder.Image]
    /// Files of any other type going with the prompt: dropped or attached. The agent gets them
    /// as paths on the session's machine, so they have no marker in the text.
    @Binding var files: [PromptFile]
    let placeholder: String
    /// Edges the box in a colour of its own, as in a child session.
    var tint: Color?
    /// The model menu's groups, from `ModelCatalog.groups`.
    let models: [ModelCatalog.Group]
    /// The provider and model in use; `""` is the provider's default.
    let current: ModelCatalog.Choice
    let mode: PermissionMode?
    let running: Bool
    let choose: (ModelCatalog.Choice) -> Void
    /// The settings the model menu offers after the models: account and machine.
    var settings: [SettingsSection] = []
    /// The skills the prompt can mention with `$`.
    var skills: [SessionSkill] = []
    let setMode: (PermissionMode) -> Void
    let send: () -> Void
    let stop: () -> Void
    @ViewBuilder var footer: Footer
    @State private var editing = false
    /// Long pastes, shown as chips and sent in place of their markers.
    @State private var pastes: [String] = []
    @State private var imageError: String?
    @State private var dictation = Dictation()
    /// The text before dictation started; what is heard follows it.
    @State private var dictatedAfter = ""
    #if os(iOS)
    /// Photos picked to attach, until they load.
    @State private var picked: [PhotosPickerItem] = []
    @State private var importing = false
    @Environment(\.horizontalSizeClass) private var sizeClass
    #endif

    var body: some View {
        VStack(spacing: 0) {
            VStack(spacing: 0) {
                if !skillMatches.isEmpty {
                    SkillPicker(skills: Array(skillMatches.prefix(6)), choose: mention)
                    Rectangle().fill(Theme.stroke).frame(height: 1)
                } else if SkillMention.query(in: text) == "" && skills.isEmpty {
                    // A bare `$` with nothing to pick says why, rather than nothing at all.
                    Label("No skills here yet. Add some under Skills.", systemImage: "book.closed")
                        .font(.footnote).foregroundStyle(Theme.tertiary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 16).padding(.vertical, 10)
                        .accessibilityIdentifier("skill-picker-empty")
                    Rectangle().fill(Theme.stroke).frame(height: 1)
                }
                if !files.isEmpty { FileStrip(files: files) { files.remove(at: $0) } }
                #if os(macOS)
                PromptEditor(
                    text: $text, focused: $editing, images: images, pastes: pastes, skills: skills,
                    addImages: add, addFiles: attach, addPaste: addPaste, submit: submit)
                    .overlay(alignment: .topLeading) {
                        if text.isEmpty { Text(placeholder).foregroundStyle(Theme.tertiary).allowsHitTesting(false) }
                    }
                    .padding(.horizontal, 18)
                    .padding(.top, 16)
                    .padding(.bottom, 8)
                #else
                if !images.isEmpty { AttachmentStrip(images: images, remove: remove) }
                let mentioned = SkillMention.mentioned(in: text, skills: skills)
                if !mentioned.isEmpty {
                    SkillStrip(skills: mentioned) { text = SkillMention.remove($0.name, from: text) }
                }
                PromptEditor(text: $text, focused: $editing, addImages: add, addPaste: addPaste, submit: submit)
                    .overlay(alignment: .topLeading) {
                        if text.isEmpty { Text(placeholder).foregroundStyle(Theme.tertiary).allowsHitTesting(false) }
                    }
                    .padding(.horizontal, compact ? 14 : 18)
                    .padding(.top, compact ? 12 : 16)
                    .padding(.bottom, 8)
                    .frame(maxWidth: .infinity, alignment: .topLeading)
                #endif
                // The permission menu drops its label before the row runs wider than the box.
                ViewThatFits(in: .horizontal) {
                    toolbar(labels: true)
                    toolbar(labels: false)
                }
                .padding(.horizontal, 10)
                .padding(.bottom, 10)
            }
            .background(Theme.surface, in: .rect(cornerRadius: 22))
            .overlay(RoundedRectangle(cornerRadius: 22).strokeBorder(
                tint.map { $0.opacity(isFocused ? 0.7 : 0.4) } ?? (isFocused ? Theme.secondary.opacity(0.5) : Theme.stroke)))
            .contentShape(.rect)
            .onTapGesture { focus() }
            .onDrop(of: [.fileURL, .image], isTargeted: nil) { providers in
                Task {
                    // A file on this device goes by its URL, as picked; an image dragged out of
                    // an app, by its bytes.
                    let urls = await FileAttachment.urls(providers)
                    if urls.isEmpty {
                        text += add(await ImageAttachment.load(providers))
                    } else {
                        attach(urls)
                    }
                }
                return true
            }
            HStack(spacing: compact ? 10 : 14) { footer }
                .font(.footnote)
                .foregroundStyle(Theme.tertiary)
                // One line on a phone too: what does not fit truncates rather than wrapping.
                .lineLimit(compact ? 1 : nil)
                .padding(.horizontal, compact ? 12 : 16)
                .padding(.vertical, 8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Theme.surface.opacity(0.6),
                            in: UnevenRoundedRectangle(bottomLeadingRadius: 14, bottomTrailingRadius: 14))
                .padding(.horizontal, compact ? 12 : 18)
        }
        .onAppear { if focusesOnAppear { focus() } }
        .onChange(of: text) {
            // A chip deleted from the text takes its image or paste with it.
            PromptText.prune(.image, items: &images, text: &text)
            PromptText.prune(.paste, items: &pastes, text: &text)
        }
    }

    /// The model and permission menus, dictation, attach and send, inside the box's bottom edge.
    private func toolbar(labels: Bool) -> some View {
        HStack(spacing: 4) {
            ModelPicker(groups: models, current: current, sections: settings, choose: choose)
            Divider().frame(height: 16).overlay(Theme.stroke)
            Menu {
                ForEach([PermissionMode.readOnly, .ask, .autoEdit, .fullAccess], id: \.self) { option in
                    Button { setMode(option) } label: {
                        if option == mode { Label(option.label, systemImage: "checkmark") } else { Text(option.label) }
                    }
                }
            } label: {
                MenuLabel(symbol: mode == .fullAccess ? "lock.open" : "lock", text: mode?.label ?? "Permissions", showsText: labels)
            }
            .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden).fixedSize()
            Spacer()
            if let problem = imageError ?? dictation.error {
                Text(problem).font(.caption).foregroundStyle(Theme.failure).lineLimit(2)
            } else if dictation.downloading {
                Text("Downloading the speech model…").font(.caption).foregroundStyle(Theme.secondary)
            }
            DictationButton(listening: dictation.listening, action: toggleDictation)
            #if os(macOS)
            Button(action: attach) { attachLabel }
                .buttonStyle(.plain)
                .help("Attach files and images (or paste or drop them)")
            #else
            Menu {
                PhotosPicker(selection: $picked, matching: .images, preferredItemEncoding: .compatible) {
                    Label("Photo Library", systemImage: "photo.on.rectangle")
                }
                .accessibilityIdentifier("attach-images")
                Button { importing = true } label: { Label("Choose Files", systemImage: "folder") }
                    .accessibilityIdentifier("attach-files")
            } label: {
                attachLabel
            }
            .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden)
            .accessibilityLabel("Attach")
            .accessibilityIdentifier("attach")
            .onChange(of: picked) {
                guard !picked.isEmpty else { return }
                let items = picked
                picked = []
                Task { await attach(items) }
            }
            .fileImporter(isPresented: $importing, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
                switch result {
                case .success(let urls): attach(urls)
                case .failure(let error): imageError = error.localizedDescription
                }
            }
            #endif
            if running && nothingToSend {
                CircleButton(symbol: "stop.fill", help: "Interrupt", action: stop)
            } else {
                CircleButton(symbol: "arrow.up", help: "Send", action: submit)
                    .disabled(nothingToSend)
                    .opacity(nothingToSend ? 0.35 : 1)
                    .keyboardShortcut(.return, modifiers: .command)
            }
        }
    }

    private var isFocused: Bool { editing }

    private var attachLabel: some View {
        SwiftUI.Image(systemName: "paperclip").font(.callout.weight(.semibold))
            .foregroundStyle(Theme.secondary).frame(width: 30, height: 30).contentShape(.rect)
            .hitTarget()
    }

    /// A phone's width: the box and its footer sit closer to the edges.
    private var compact: Bool {
        #if os(macOS)
        false
        #else
        sizeClass == .compact
        #endif
    }

    /// The prompt takes focus as it appears on the Mac and on an iPad with a hardware keyboard.
    /// On iPhone the session opens on its transcript: the keyboard waits for a tap on the prompt.
    private var focusesOnAppear: Bool {
        #if os(macOS)
        true
        #else
        UIDevice.current.userInterfaceIdiom == .pad && GCKeyboard.coalesced != nil
        #endif
    }

    private func focus() { editing = true }

    private var trimmed: String { text.trimmingCharacters(in: .whitespacesAndNewlines) }

    private var nothingToSend: Bool { trimmed.isEmpty && images.isEmpty && files.isEmpty }

    /// The skills the `$` mention being typed picks.
    private var skillMatches: [SessionSkill] {
        SkillMention.query(in: text).map { SkillMention.matches($0, in: skills) } ?? []
    }

    private func mention(_ skill: SessionSkill) {
        text = SkillMention.complete(text, with: skill.name)
        focus()
    }

    /// Sends the prompt; while the skill picker is open, Return chooses its first skill instead.
    private func submit() {
        if let first = skillMatches.first {
            mention(first)
            return
        }
        dictation.cancel()
        text = PromptText.expand(text, pastes: pastes)
        pastes = []
        send()
    }

    private func toggleDictation() {
        if dictation.listening {
            dictation.stop()
            return
        }
        dictatedAfter = text.isEmpty || text.hasSuffix(" ") || text.hasSuffix("\n") ? text : text + " "
        focus()
        Task { await dictation.start { heard in text = dictatedAfter + heard } }
    }

    private func remove(_ index: Int) {
        PromptText.remove(.image, at: index, items: &images, text: &text)
    }

    /// Adds images; returns a `[Image #N]` marker for each, numbered in the order they go to
    /// the agent, so the prompt can refer to them.
    private func add(_ added: [Herder.Image]) -> String {
        added.map { image in
            images.append(image)
            return PromptText.marker(.image, images.count)
        }
        .map { marker in text.isEmpty || text.hasSuffix(" ") || text.hasSuffix("\n") ? marker + " " : " " + marker + " " }
        .joined()
    }

    /// Attaches files picked, imported or dropped: pictures as images, with their markers at
    /// the end of the text, anything else as files. Says why one could not go.
    private func attach(_ urls: [URL]) {
        // Files imported on iOS are outside the app's sandbox until it asks.
        let scoped = urls.filter { $0.startAccessingSecurityScopedResource() }
        defer { scoped.forEach { $0.stopAccessingSecurityScopedResource() } }
        let sorted = FileAttachment.sort(urls)
        let carried = images.reduce(0) { $0 + $1.data.count } + files.reduce(0) { $0 + $1.data.count }
            + sorted.images.reduce(0) { $0 + $1.data.count }
        let fitting = FileAttachment.fitting(sorted.files, carried: carried)
        text += add(sorted.images)
        files += fitting.files
        imageError = fitting.refused ?? sorted.refused
    }

    /// Keeps a long paste; returns its `[Pasted text #N]` marker.
    private func addPaste(_ pasted: String) -> String {
        pastes.append(pasted)
        return PromptText.marker(.paste, pastes.count) + " "
    }

    #if os(macOS)
    /// Picks files of any type to attach, several at once.
    private func attach() {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = true
        panel.canChooseDirectories = false
        guard panel.runModal() == .OK else { return }
        attach(panel.urls)
    }
    #else
    /// Attaches photos picked from the library, in the order they were picked.
    private func attach(_ items: [PhotosPickerItem]) async {
        var added: [Herder.Image] = []
        imageError = nil
        for item in items {
            do {
                guard let data = try await item.loadTransferable(type: Data.self) else { continue }
                added.append(try ImageAttachment.make(data, type: item.supportedContentTypes.first { $0.conforms(to: .image) }))
            } catch {
                imageError = error.localizedDescription
            }
        }
        text += add(added)
    }
    #endif
}

/// A menu's label in the composer: icon, text and a chevron.
struct MenuLabel: View {
    let symbol: String
    let text: String
    /// Off where the row has no room for the text; it stays the accessibility label.
    var showsText = true

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: symbol).imageScale(.small)
            if showsText { Text(text).lineLimit(1) }
            Image(systemName: "chevron.down").font(.caption2.weight(.semibold))
        }
        .font(.subheadline.weight(.medium))
        .foregroundStyle(Theme.secondary)
        .padding(.horizontal, 8)
        .frame(height: 32)
        .hitTarget()
        .accessibilityLabel(text)
    }
}

/// The composer's microphone: dictates on this device, pulsing while it listens.
struct DictationButton: View {
    let listening: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            SwiftUI.Image(systemName: listening ? "mic.fill" : "mic")
                .font(.callout.weight(.semibold))
                .foregroundStyle(listening ? Theme.accent : Theme.secondary)
                .frame(width: 30, height: 30)
                .background(listening ? Theme.accent.opacity(0.18) : .clear, in: .circle)
                .symbolEffect(.pulse, isActive: listening)
                .hitTarget()
        }
        .buttonStyle(.plain)
        .keyboardShortcut("d", modifiers: [.command, .shift])
        .help(listening ? "Stop dictating (⇧⌘D)" : "Dictate, on this device (⇧⌘D)")
        .accessibilityLabel(listening ? "Stop dictating" : "Dictate")
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
            .hitTarget()
        }
        .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden)
        // Truncates rather than widening the footer past a phone's width.
        .fixedSize(horizontal: false, vertical: true)
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
                .hitTarget()
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

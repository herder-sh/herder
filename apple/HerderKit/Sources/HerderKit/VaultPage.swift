import Herder
import SwiftUI

/// A vault's page: how current its copy of each host is, with their sessions and whether they
/// take handoffs, and for its owners a Secrets tab: each set with the hosts it is granted to and
/// the secrets in it, and the latest fetches. Owners create and delete sets, add, replace and
/// delete secrets, and grant a set to the vault's hosts or revoke it. A value is typed into a
/// secure field and never shown again.
struct VaultPage: View {
    let fleet: Fleet
    let vault: HostId
    @Environment(\.pages) private var pages
    @State private var tab = Tab.overview
    @State private var secrets: VaultSecrets
    @State private var newSet = ""
    @State private var editor: SecretEditor?
    @State private var deleting: Deleting?
    @State private var error: String?

    enum Tab: Hashable {
        case overview, secrets
    }

    /// A set or a secret, asked about before it is deleted.
    private enum Deleting: Hashable {
        case set(String)
        case secret(SecretInfo)

        var title: String {
            switch self {
            case .set(let set): "Delete the set \(set)?"
            case .secret(let secret): "Delete \(secret.set)/\(secret.name)?"
            }
        }

        var message: String {
            switch self {
            case .set: "Hosts it is granted to no longer get its secrets."
            case .secret: "Sessions that use it no longer get its value."
            }
        }
    }

    init(fleet: Fleet, vault: HostId, tab: Tab = .overview) {
        self.fleet = fleet
        self.vault = vault
        _tab = State(initialValue: tab)
        _secrets = State(initialValue: VaultSecrets(fleet: fleet, vault: vault))
    }

    var body: some View {
        let machine = fleet.machines.first { $0.hostId == vault }
        let stats = machine.flatMap { VaultStats(machine: $0, sessions: fleet.sessions) }
        let owned = stats?.owned == true
        // The status sentence leads with the connection, so no chip repeats it.
        DetailPage(title: machine?.name ?? "Vault", status: stats?.status ?? "") {
            if tab == .secrets {
                IconButton(symbol: "arrow.clockwise", help: "Reload") { Task { await secrets.reload() } }
            }
            IconButton(symbol: "gearshape", help: "Machine Settings") { pages.open(.machine(vault)) }
        } content: {
            if owned {
                Picker("View", selection: $tab) {
                    Text("Overview").tag(Tab.overview)
                    Text("Secrets").tag(Tab.secrets)
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .fixedSize()
                .accessibilityIdentifier("vault-tabs")
            }
            if owned && tab == .secrets {
                VStack(alignment: .leading, spacing: 18) { sets(machine) }
                if let error {
                    Text(error).font(.footnote).foregroundStyle(Theme.failure)
                        .accessibilityIdentifier("secrets-error")
                }
                audit(machine)
            } else if let stats {
                VaultOverview(fleet: fleet, vault: stats)
            }
        }
        .task(id: tab) { if tab == .secrets { await secrets.reload() } }
        .sheet(item: Binding(get: { editor.map(EditorItem.init) }, set: { if $0 == nil { editor = nil } })) { item in
            SecretEditorSheet(editor: item.editor)
        }
        .confirmationDialog(deleting?.title ?? "", isPresented: Binding(
            get: { deleting != nil }, set: { if !$0 { deleting = nil } }), titleVisibility: .visible) {
            Button("Delete", role: .destructive) {
                guard let deleting else { return }
                Task {
                    await run {
                        switch deleting {
                        case .set(let set): try await secrets.deleteSet(set)
                        case .secret(let secret): try await secrets.deleteSecret(secret)
                        }
                    }
                }
            }
        } message: {
            Text(deleting?.message ?? "")
        }
    }

    /// The sets, each with its secrets, and a field that creates one.
    @ViewBuilder private func sets(_ machine: Machine?) -> some View {
        HStack(spacing: 4) {
            SectionHeading(title: "Secrets")
            InfoButton(text: "Values are never shown again; replace one to change it.")
        }
        switch secrets.list {
        case .asking:
            ProgressView().frame(maxWidth: .infinity)
        case .failed(let why):
            failure(why)
        case .loaded:
            if secrets.sets.isEmpty {
                Text("No sets yet. A set groups secrets you grant to hosts together; create one below.")
                    .font(.subheadline).foregroundStyle(Theme.tertiary)
            }
            ForEach(secrets.sets, id: \.name) { set in
                SetCard(set: set, secrets: secrets, hosts: VaultSecrets.hosts(of: set, vaultHosts: machine?.hosts ?? []),
                        add: { editor = SecretEditor(secrets: secrets, set: set.name) },
                        replace: { editor = SecretEditor(secrets: secrets, replacing: $0) },
                        delete: { deleting = .secret($0) },
                        deleteSet: { deleting = .set(set.name) },
                        failed: { error = $0 })
            }
            Field(label: "New set", hint: "Lowercase letters, digits and hyphens, such as `prod` or `coingate-prod`.") {
                HStack(spacing: 10) {
                    InputBox(placeholder: "prod", text: $newSet, mono: true)
                        .accessibilityIdentifier("secret-set-name")
                    ActionButton(title: "Create", style: .secondary) {
                        await run {
                            try await secrets.createSet(newSet)
                            newSet = ""
                        }
                    }
                    .frame(width: 120)
                    .disabled(newSet.trimmingCharacters(in: .whitespaces).isEmpty)
                    .accessibilityIdentifier("secret-set-create")
                }
            }
        }
    }

    private func failure(_ why: String) -> some View {
        Text(why).font(.subheadline).foregroundStyle(Theme.failure)
            .fixedSize(horizontal: false, vertical: true)
            .accessibilityIdentifier("secrets-error")
    }

    /// The latest fetches: which host's session read which secret, when.
    @ViewBuilder private func audit(_ machine: Machine?) -> some View {
        Field(label: "Recent access") {
            VStack(alignment: .leading, spacing: 8) {
                switch secrets.audit {
                case .asking:
                    EmptyView()
                case .failed(let why):
                    Text(why).foregroundStyle(Theme.failure)
                case .loaded(let entries) where entries.isEmpty:
                    Text("No session has fetched a secret yet.").foregroundStyle(Theme.tertiary)
                case .loaded(let entries):
                    ForEach(entries, id: \.id) { entry in
                        let host = machine?.hosts.first { $0.hostId == entry.hostId }?.hostName ?? entry.hostId
                        HStack(alignment: .firstTextBaseline, spacing: 10) {
                            Text("\(entry.set)/\(entry.name)").font(Theme.mono).foregroundStyle(Theme.text)
                            Text("\(host) · \(entry.sessionId ?? entry.purpose ?? "")").foregroundStyle(Theme.secondary).lineLimit(1)
                            Spacer()
                            Text(Self.ago(entry.at)).foregroundStyle(Theme.tertiary).monospacedDigit()
                        }
                    }
                }
            }
            .font(.footnote)
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
        }
    }

    static func ago(_ at: String) -> String {
        let age = Timestamp.age(Timestamp.date(at), now: .now)
        return age == "now" || age.isEmpty ? age : "\(age) ago"
    }

    private func run(_ action: () async throws -> Void) async {
        error = nil
        do { try await action() } catch { self.error = describe(error) }
    }
}

/// The editor as a sheet item: one per editor opened.
private struct EditorItem: Identifiable {
    let editor: SecretEditor
    var id: ObjectIdentifier { ObjectIdentifier(editor) }
}

/// A set: the hosts it is granted to, each a switch, and its secrets.
private struct SetCard: View {
    let set: SecretSet
    let secrets: VaultSecrets
    let hosts: [(id: HostId, name: String?)]
    let add: () -> Void
    let replace: (SecretInfo) -> Void
    let delete: (SecretInfo) -> Void
    let deleteSet: () -> Void
    let failed: (String) -> Void
    /// Grants being changed, until the vault lists the set again.
    @State private var asked: [HostId: Bool] = [:]

    var body: some View {
        let inSet = secrets.secrets(in: set.name)
        Card {
            VStack(alignment: .leading, spacing: 12) {
                HStack(spacing: 8) {
                    Image(systemName: "key").foregroundStyle(Theme.secondary)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(set.name).font(.headline.monospaced()).foregroundStyle(Theme.text)
                            .lineLimit(1).truncationMode(.middle)
                        Text(inSet.count == 1 ? "1 secret" : "\(inSet.count) secrets")
                            .font(.caption).foregroundStyle(Theme.tertiary)
                    }
                    Spacer()
                    IconButton(symbol: "plus", help: "Add Secret", action: add)
                        .accessibilityIdentifier("secret-add-\(set.name)")
                    IconButton(symbol: "trash", help: "Delete Set", action: deleteSet)
                }
                SectionHeading(title: "Granted to")
                if hosts.isEmpty {
                    Text("No host backs up to this vault yet.").font(.footnote).foregroundStyle(Theme.tertiary)
                }
                ForEach(hosts, id: \.id) { host in
                    let granted = set.hosts.contains(host.id)
                    Toggle(isOn: Binding(get: { asked[host.id] ?? granted }, set: { grant($0, host.id) })) {
                        Text(host.name ?? host.id).font(.subheadline).foregroundStyle(Theme.text)
                    }
                    .toggleStyle(.switch)
                    .disabled(asked[host.id] != nil)
                    .accessibilityIdentifier("secret-grant-\(set.name)-\(host.id)")
                }
                SectionHeading(title: "Secrets")
                if inSet.isEmpty {
                    Text("No secrets in this set.").font(.footnote).foregroundStyle(Theme.tertiary)
                }
                ForEach(inSet, id: \.name) { secret in
                    SecretRow(secret: secret, replace: { replace(secret) }, delete: { delete(secret) })
                }
            }
        }
        .onChange(of: set.hosts) { asked = [:] }
    }

    private func grant(_ on: Bool, _ host: HostId) {
        asked[host] = on
        Task {
            do {
                try await secrets.setGranted(on, set: set.name, host: host)
            } catch {
                failed(describe(error))
            }
            asked[host] = nil
        }
    }
}

/// A secret: its name, what it is for, and when it was last saved; never its value.
private struct SecretRow: View {
    let secret: SecretInfo
    let replace: () -> Void
    let delete: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            VStack(alignment: .leading, spacing: 3) {
                Text(secret.name).font(Theme.mono).foregroundStyle(Theme.text)
                Text(secret.description).font(.caption).foregroundStyle(Theme.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Text("Updated \(VaultPage.ago(secret.updatedAt))").font(.caption2).foregroundStyle(Theme.tertiary)
            }
            Spacer()
            IconButton(symbol: "arrow.triangle.2.circlepath", help: "Replace Value", action: replace)
                .accessibilityIdentifier("secret-replace-\(secret.set)-\(secret.name)")
            IconButton(symbol: "trash", help: "Delete Secret", action: delete)
                .accessibilityIdentifier("secret-delete-\(secret.set)-\(secret.name)")
        }
        .padding(10)
        .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("secret-\(secret.set)-\(secret.name)")
    }
}

/// Adds a secret to a set or replaces one: its name, description and value, typed into a
/// secure field. The value is gone once saved, or refused.
private struct SecretEditorSheet: View {
    @Bindable var editor: SecretEditor
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        SheetScaffold(title: editor.replacing ? "Replace \(editor.name)" : "Add Secret",
                      subtitle: "In the set \(editor.set)", height: 420,
                      hint: editor.replacing ? "The value it holds now is never shown." : "The value is never shown again once saved.") {
            Field(label: "Name") {
                InputBox(placeholder: "API_KEY", text: $editor.name, mono: true)
                    .disabled(editor.replacing)
                    .accessibilityIdentifier("secret-name")
            }
            Field(label: "Description") {
                InputBox(placeholder: "What it is for, such as deploys to staging", text: $editor.description)
                    .accessibilityIdentifier("secret-description")
            }
            Field(label: "Value") {
                SecureField("Value", text: $editor.value.text)
                    .textFieldStyle(.plain)
                    .font(Theme.mono)
                    .foregroundStyle(Theme.text)
                    .autocorrectionDisabled()
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    #endif
                    .padding(12)
                    .frame(minHeight: 44)
                    .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
                    .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
                    .accessibilityIdentifier("secret-value")
            }
            if let error = editor.error {
                Text(error).font(.footnote).foregroundStyle(Theme.failure)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("secret-error")
            }
        } footer: {
            Spacer()
            ActionButton(title: editor.replacing ? "Replace" : "Save", style: .primary) {
                if await editor.save() { dismiss() }
            }
            .frame(maxWidth: 200)
            .disabled(editor.sending)
            .accessibilityIdentifier("secret-save")
        }
    }
}

/// A vault's overview: its sessions and size as quiet chips, then a row per host with how
/// current the vault's copy of it is.
struct VaultOverview: View {
    let fleet: Fleet
    let vault: VaultStats
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    #endif

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            FlowLayout(spacing: 6) {
                Chip(text: vault.sessions == 1 ? "1 session" : "\(vault.sessions) sessions")
                    .help(Self.breakdown(vault.byState))
                if vault.count(.idle) > 0 { Chip(text: "\(vault.count(.idle)) idle") }
                if vault.count(.archived) > 0 { Chip(text: "\(vault.count(.archived)) archived") }
                if let size = vault.size { Chip(text: size) }
            }
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 4) {
                    SectionHeading(title: "Hosts", count: vault.hosts.count)
                    InfoButton(text: "A host that takes handoffs may read other hosts' sessions from the vault, so you can hand one off to it.")
                }
                if vault.hosts.isEmpty {
                    Text("No host backs up to this vault yet. Link one from its machine's Backup settings.")
                        .font(.subheadline).foregroundStyle(Theme.tertiary)
                } else {
                    SortableTable("vault.hosts", rows: vault.hosts, sortedBy: TableSort(column: "Host"), columns: columns)
                }
            }
        }
    }

    private var compact: Bool {
        #if os(iOS)
        sizeClass == .compact
        #else
        false
        #endif
    }

    private var columns: [SortableColumn<VaultStats.Host>] {
        let now = vault.now
        var columns: [SortableColumn<VaultStats.Host>] = [
            SortableColumn("Host", value: \.name) { host in
                HStack(spacing: 6) {
                    Circle().fill(host.online ? Theme.success : Theme.failure).frame(width: 6, height: 6)
                    Text(host.name).fontWeight(.semibold).lineLimit(1)
                }
            },
            SortableColumn("Freshness", width: compact ? nil : 220, value: { $0.freshness.behind(now: now) }) { host in
                FreshnessCell(host: host, now: now)
            },
        ]
        if !compact {
            columns += [
                SortableColumn("Sessions", width: 70, alignment: .trailing, value: \.sessions) { host in
                    Text("\(host.sessions)").monospacedDigit()
                },
                SortableColumn("Top projects") { host in
                    Text(host.topProjects.map { "\($0.name) \($0.sessions)" }.joined(separator: " · "))
                        .font(Theme.meta).foregroundStyle(Theme.secondary).lineLimit(1)
                },
            ]
        }
        columns.append(SortableColumn("Takes handoffs", width: 110) { host in
            if vault.owned, let takesHandoffs = host.takesHandoffs {
                HandoffToggle(fleet: fleet, vault: vault.hostId, host: host.id, takesHandoffs: takesHandoffs)
            } else {
                Text(host.takesHandoffs.map { $0 ? "On" : "Off" } ?? "–").foregroundStyle(Theme.secondary)
            }
        })
        return columns
    }

    /// "3 running · 1 needs you · 49 idle": the sessions' states, on hover over their chip.
    nonisolated static func breakdown(_ byState: [SessionState: Int]) -> String {
        let order: [SessionState] = [.running, .needsYou, .waiting, .idle, .done, .error, .moved, .archived]
        return order.compactMap { state in byState[state].map { "\($0) \(state.label.lowercased())" } }
            .joined(separator: " · ")
    }
}

/// How current the vault's copy of a host is: "✓ caught up" with its last batch, or "6 h
/// behind" with a "Why?" that explains it.
private struct FreshnessCell: View {
    let host: VaultStats.Host
    let now: Date
    @State private var explaining = false

    var body: some View {
        HStack(spacing: 6) {
            if let why = host.freshness.why(host: host.name, now: now) {
                Chip(text: host.freshness.label(now: now), tint: Theme.accent)
                Button("Why?") { explaining.toggle() }
                    .buttonStyle(.plain)
                    .font(Theme.meta)
                    .foregroundStyle(Theme.link)
                    .accessibilityIdentifier("freshness-why-\(host.id)")
                    .popover(isPresented: $explaining) {
                        Text(why)
                            .font(Theme.body)
                            .foregroundStyle(Theme.text)
                            .fixedSize(horizontal: false, vertical: true)
                            .frame(width: 300, alignment: .leading)
                            .padding(14)
                            .presentationCompactAdaptation(.popover)
                    }
            } else {
                Chip(text: host.freshness.label(now: now), tint: Theme.success)
                if let last = host.lastBatch ?? { if case .caughtUp(let last) = host.freshness { last } else { nil } }() {
                    RelativeTime(date: last).font(Theme.meta).foregroundStyle(Theme.tertiary).lineLimit(1)
                }
            }
        }
    }
}

/// Whether a host may read the vault, so a session can be handed off to it, for the vault's
/// owners. Shows the change at once and keeps it until the vault's status agrees.
private struct HandoffToggle: View {
    let fleet: Fleet
    let vault: HostId
    let host: HostId
    let takesHandoffs: Bool
    @State private var asked: Bool?
    @State private var error: String?

    var body: some View {
        HStack(spacing: 6) {
            Toggle("Takes handoffs", isOn: Binding(get: { asked ?? takesHandoffs }, set: { change($0) }))
                .toggleStyle(.switch)
                .labelsHidden()
                .controlSize(.small)
                .disabled(asked != nil)
                .accessibilityIdentifier("takes-handoffs-\(host)")
            if let error {
                Image(systemName: "exclamationmark.triangle").foregroundStyle(Theme.failure).help(error)
            }
        }
        .onChange(of: takesHandoffs) { asked = nil }
    }

    private func change(_ on: Bool) {
        asked = on
        error = nil
        Task {
            do {
                try await fleet.setTakesHandoffs(on, host: host, vault: vault)
                if on == takesHandoffs { asked = nil }
            } catch {
                self.error = describe(error)
                asked = nil
            }
        }
    }
}

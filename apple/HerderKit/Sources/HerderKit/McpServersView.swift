import Herder
import SwiftUI

/// A sheet over the MCP servers: adding one, editing one on a machine, or importing JSON.
enum McpSheet: Identifiable {
    case add(targets: Set<HostId>)
    case edit(HostId, McpServerSettings)
    case importJSON(targets: Set<HostId>)

    var id: String {
        switch self {
        case .add: "add"
        case .edit(let host, let server): "edit-\(host)-\(server.name)"
        case .importJSON: "import"
        }
    }
}

extension Fleet {
    /// The machines that run sessions and so take MCP servers: a vault runs none.
    var mcpMachines: [Machine] { machines.filter { $0.hosts.isEmpty && $0.role != nil } }

    /// The machines whose MCP servers this device can change now: those it owns, connected.
    var mcpEditable: [Machine] { mcpMachines.filter { $0.role == .owner && $0.connection == .connected } }
}

/// The MCP servers on the Providers page, as a table of every machine's, a row per name: the
/// machines that have it, how it connects, and a switch that turns it on or off on all of them.
/// Its header adds one to any or all of the machines, or imports a JSON snippet.
struct McpServersSection: View {
    let fleet: Fleet
    @State private var lists: McpServerLists?
    @State private var sheet: McpSheet?
    @State private var removing: McpServerTable.Row?
    @State private var error: String?

    var body: some View {
        let machines = fleet.mcpMachines
        let editable = Set(fleet.mcpEditable.map(\.hostId))
        ProvidersSection(title: "MCP servers", note: "every session on the machine gets them") {
            PillButton(title: "Paste JSON", symbol: "doc.on.clipboard") { sheet = .importJSON(targets: editable) }
                .disabled(editable.isEmpty)
            PillButton(title: "Add", symbol: "plus") { sheet = .add(targets: editable) }
                .disabled(editable.isEmpty)
                .accessibilityLabel("Add MCP Server")
        } content: {
            if let lists {
                let table = McpServerTable(machines.map { (hostId: $0.hostId, name: $0.name) }, servers: lists.servers(on:))
                if table.rows.isEmpty {
                    if machines.contains(where: { lists.servers(on: $0.hostId) != nil }) {
                        note("No MCP servers yet. Add one, or paste a client’s JSON.")
                    }
                } else {
                    ViewThatFits(in: .horizontal) {
                        servers(table, editable, compact: false).frame(minWidth: ProvidersColumns.wideTable)
                        servers(table, editable, compact: true)
                    }
                }
                ForEach(machines, id: \.hostId) { machine in state(machine, lists) }
            }
            if let error { note(error, failed: true) }
            Text("Changes reach each session when its agent next starts; no restart needed.")
                .font(.footnote).foregroundStyle(Theme.tertiary)
        }
        .task(id: editable) {
            let lists = lists ?? McpServerLists(fleet: fleet)
            self.lists = lists
            await withTaskGroup(of: Void.self) { group in
                for hostId in editable { group.addTask { await lists.reload(hostId) } }
            }
        }
        .sheet(item: $sheet) { sheet in
            if let lists { McpSheetView(fleet: fleet, lists: lists, sheet: sheet) }
        }
        .confirmationDialog("Remove \(removing?.name ?? "")?",
                            isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
                            titleVisibility: .visible, presenting: removing) { row in
            Button("Remove", role: .destructive) {
                change(row) { servers, _ in servers.filter { $0.name != row.name } }
            }
        } message: { row in
            Text("Sessions on \(row.places.map(\.machine).joined(separator: ", ")) lose it from their agent’s next start.")
        }
    }

    private func servers(_ table: McpServerTable, _ editable: Set<HostId>, compact: Bool) -> some View {
        SortableTable("providers.mcp", rows: table.rows, sortedBy: TableSort(column: "Server"),
                      columns: columns(editable, compact: compact))
    }

    /// Server, machines, transport and status, lined up with the accounts table's columns; a
    /// narrow pane keeps the server and its switch.
    private func columns(_ editable: Set<HostId>, compact: Bool) -> [SortableColumn<McpServerTable.Row>] {
        var all = [
            SortableColumn<McpServerTable.Row>("Server", value: \.name) { row in server(row, compact: compact) },
        ]
        if !compact {
            all += [
                SortableColumn("Machines", width: ProvidersColumns.machines, value: \.machines) { row in
                    Text(row.machines).foregroundStyle(Theme.secondary).lineLimit(1)
                        .help(row.places.map(\.machine).joined(separator: ", "))
                },
                SortableColumn("Transport", width: ProvidersColumns.wide, value: { McpKind($0.transport).title }) { row in
                    HStack(spacing: 6) {
                        Badge(text: McpKind(row.transport).title)
                        Text(McpServers.target(row.transport)).font(Theme.monoSmall).foregroundStyle(Theme.tertiary)
                            .lineLimit(1).truncationMode(.middle)
                    }
                    .help(McpServers.target(row.transport))
                },
            ]
        }
        all += [
            SortableColumn("Status", width: compact ? 56 : ProvidersColumns.narrow, value: \.status) { row in
                status(row, editable, compact: compact)
            },
            SortableColumn("", width: compact ? 28 : ProvidersColumns.last, alignment: .trailing) { row in menu(row, editable) },
        ]
        return all
    }

    /// The server's name, which opens it to edit on its first machine; its project when it is
    /// one project's, and on a narrow pane how it connects.
    private func server(_ row: McpServerTable.Row, compact: Bool) -> some View {
        let place = row.places.first
        return Button { if let place { sheet = .edit(place.hostId, place.server) } } label: {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(row.name).font(Theme.body.weight(.semibold).monospaced())
                        .foregroundStyle(row.enabled ? Theme.text : Theme.tertiary).lineLimit(1)
                    if let project = place?.server.project {
                        Label(projectName(project, on: place?.hostId), systemImage: "folder")
                            .font(.caption2.weight(.semibold)).foregroundStyle(Theme.secondary).lineLimit(1)
                    }
                }
                if compact {
                    Text("\(McpKind(row.transport).title) · \(row.machines)").font(.caption).foregroundStyle(Theme.tertiary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("mcp-server-\(row.name)")
    }

    /// A switch that turns the server on or off on every machine that has it, and where it is
    /// off when only some have it off.
    private func status(_ row: McpServerTable.Row, _ editable: Set<HostId>, compact: Bool) -> some View {
        HStack(spacing: 8) {
            Toggle("Enabled", isOn: Binding(get: { row.enabled }, set: { on in
                change(row) { servers, _ in
                    servers.map { $0.name == row.name ? McpServerSettings(
                        name: $0.name, enabled: on, project: $0.project, transport: $0.transport) : $0 }
                }
            }))
            .labelsHidden()
            .toggleStyle(.switch)
            .controlSize(.small)
            .disabled(!row.places.contains { editable.contains($0.hostId) })
            .accessibilityLabel("\(row.name) enabled")
            if !compact && row.partlyOff {
                Text(row.status).font(.caption).foregroundStyle(Theme.tertiary).lineLimit(1)
                    .help(row.status)
            }
        }
    }

    private func menu(_ row: McpServerTable.Row, _ editable: Set<HostId>) -> some View {
        let others = fleet.mcpEditable.filter { machine in !row.places.contains { $0.hostId == machine.hostId } }
        return Menu {
            if row.places.count == 1, let place = row.places.first {
                Button("Edit…", systemImage: "pencil") { sheet = .edit(place.hostId, place.server) }
            } else {
                Menu("Edit on", systemImage: "pencil") {
                    ForEach(row.places, id: \.hostId) { place in
                        Button(place.machine) { sheet = .edit(place.hostId, place.server) }
                    }
                }
            }
            if !others.isEmpty, let server = row.places.first?.server {
                Menu("Copy to Machine", systemImage: "square.on.square") {
                    ForEach(others, id: \.hostId) { other in
                        Button(other.name) { copy(server, to: [other]) }
                    }
                    if others.count > 1 {
                        Divider()
                        Button("All Other Machines") { copy(server, to: others) }
                    }
                }
            }
            Divider()
            Button("Remove…", systemImage: "trash", role: .destructive) { removing = row }
        } label: {
            Image(systemName: "ellipsis").font(.callout.weight(.semibold)).foregroundStyle(Theme.secondary)
                .frame(width: 28, height: 28).contentShape(.rect)
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .disabled(!row.places.contains { editable.contains($0.hostId) })
        .accessibilityLabel("\(row.name) actions")
    }

    /// Why a machine's servers are not in the table: it is someone else's, not connected, still
    /// answering, or failed to.
    @ViewBuilder private func state(_ machine: Machine, _ lists: McpServerLists) -> some View {
        if machine.role != .owner {
            note("Only \(machine.name)’s owner can see and change its MCP servers.")
        } else if machine.connection != .connected, lists.servers(on: machine.hostId) == nil {
            note("\(machine.name) is not connected.")
        } else {
            switch lists.lists[machine.hostId] {
            case nil, .asking?:
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Asking \(machine.name)…").font(.caption).foregroundStyle(Theme.tertiary)
                }
            case .failed(let message)?:
                note("\(machine.name): \(message)", failed: true)
            case .loaded?:
                EmptyView()
            }
        }
    }

    private func note(_ text: String, failed: Bool = false) -> some View {
        Text(text).font(.caption).foregroundStyle(failed ? Theme.failure : Theme.tertiary)
            .fixedSize(horizontal: false, vertical: true)
    }

    private func projectName(_ project: ProjectId, on hostId: HostId?) -> String {
        fleet.machines.first { $0.hostId == hostId }?.projects.first { $0.projectId == project }?.name ?? project
    }

    /// Changes the servers on each machine that has `row`'s server and this device can change.
    private func change(_ row: McpServerTable.Row,
                        _ body: @escaping ([McpServerSettings], HostId) -> [McpServerSettings]) {
        guard let lists else { return }
        let editable = Set(fleet.mcpEditable.map(\.hostId))
        let hosts = row.places.map(\.hostId).filter(editable.contains)
        Task {
            error = nil
            for hostId in hosts {
                do {
                    try await lists.change(on: hostId) { body($0, hostId) }
                } catch {
                    let name = fleet.machines.first { $0.hostId == hostId }?.name ?? hostId
                    self.error = "\(name): \(describe(error))"
                    await lists.reload(hostId)
                }
            }
        }
    }

    private func copy(_ server: McpServerSettings, to machines: [Machine]) {
        guard let lists else { return }
        Task {
            let failures = await lists.add([server], to: machines.map(\.hostId))
            let names = machines.map(\.name).joined(separator: ", ")
            if let failure = failures.first {
                let name = machines.first { $0.hostId == failure.key }?.name ?? failure.key
                fleet.toast = Toast(text: "Could not copy \(server.name) to \(name)", detail: failure.value, failed: true)
            } else {
                fleet.toast = Toast(text: "Copied \(server.name) to \(names)")
            }
        }
    }
}

/// The MCP servers in a machine's settings.
struct McpMachineSettings: View {
    let fleet: Fleet
    let machine: Machine
    @State private var lists: McpServerLists?
    @State private var sheet: McpSheet?

    var body: some View {
        let editable = machine.role == .owner && machine.connection == .connected
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                SectionHeading(title: "MCP servers")
                Spacer()
                if editable {
                    PillButton(title: "Paste JSON", symbol: "doc.on.clipboard") {
                        sheet = .importJSON(targets: [machine.hostId])
                    }
                    PillButton(title: "Add", symbol: "plus") { sheet = .add(targets: [machine.hostId]) }
                        .accessibilityLabel("Add MCP Server")
                }
            }
            if let lists {
                McpMachineServers(fleet: fleet, lists: lists, machine: machine, sheet: $sheet)
                    .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
                    .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
            }
        }
        .onAppear { if lists == nil { lists = McpServerLists(fleet: fleet) } }
        .sheet(item: $sheet) { sheet in
            if let lists { McpSheetView(fleet: fleet, lists: lists, sheet: sheet) }
        }
    }
}

/// One machine's MCP servers: a row for each, to turn on and off, edit, copy or remove.
struct McpMachineServers: View {
    let fleet: Fleet
    let lists: McpServerLists
    let machine: Machine
    @Binding var sheet: McpSheet?
    @State private var removing: McpServerSettings?
    @State private var error: String?

    private var owner: Bool { machine.role == .owner }
    private var connected: Bool { machine.connection == .connected }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            content
            if let error {
                Text(error).font(.footnote).foregroundStyle(Theme.failure)
                    .padding(.horizontal, 14).padding(.bottom, 10)
            }
        }
        .task(id: owner && connected) {
            if owner && connected { await lists.reload(machine.hostId) }
        }
        .confirmationDialog("Remove \(removing?.name ?? "") from \(machine.name)?",
                            isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
                            titleVisibility: .visible, presenting: removing) { server in
            Button("Remove", role: .destructive) {
                change { $0.filter { $0.name != server.name } }
            }
        } message: { _ in
            Text("Sessions there lose it from their agent’s next start.")
        }
    }

    @ViewBuilder private var content: some View {
        if !owner {
            note("Only the machine’s owner can see and change its MCP servers.")
        } else if !connected, lists.servers(on: machine.hostId) == nil {
            note("\(machine.name) is not connected.")
        } else {
            switch lists.lists[machine.hostId] {
            case nil, .asking?:
                HStack { ProgressView().controlSize(.small); Spacer() }.padding(.horizontal, 14).frame(minHeight: 44)
            case .failed(let message)?:
                note(message, failed: true)
            case .loaded(let servers)?:
                if servers.isEmpty {
                    note("No MCP servers yet.")
                }
                ForEach(Array(servers.enumerated()), id: \.element.name) { index, server in
                    if index > 0 { RowDivider() }
                    row(server)
                }
            }
        }
    }

    private func note(_ text: String, failed: Bool = false) -> some View {
        Text(text).font(.subheadline).foregroundStyle(failed ? Theme.failure : Theme.tertiary)
            .padding(.horizontal, 14).frame(minHeight: 44, alignment: .leading)
    }

    private func row(_ server: McpServerSettings) -> some View {
        HStack(alignment: .center, spacing: 12) {
            Button { if connected { sheet = .edit(machine.hostId, server) } } label: {
                VStack(alignment: .leading, spacing: 3) {
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text(server.name).font(.subheadline.weight(.semibold).monospaced())
                            .foregroundStyle(server.enabled ? Theme.text : Theme.tertiary).lineLimit(1)
                        Badge(text: McpKind(server.transport).title)
                        if let project = server.project {
                            Label(projectName(project), systemImage: "folder")
                                .font(.caption2.weight(.semibold)).foregroundStyle(Theme.secondary)
                                .lineLimit(1)
                        }
                        if !server.enabled { Text("Off").font(.caption).foregroundStyle(Theme.tertiary) }
                    }
                    Text(McpServers.target(server.transport)).font(Theme.monoSmall).foregroundStyle(Theme.tertiary)
                        .lineLimit(1).truncationMode(.middle)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("mcp-server-\(server.name)")
            menu(server)
            Toggle("Enabled", isOn: Binding(get: { server.enabled }, set: { on in
                change { $0.map { $0.name == server.name ? McpServerSettings(
                    name: $0.name, enabled: on, project: $0.project, transport: $0.transport) : $0 } }
            }))
            .labelsHidden()
            .toggleStyle(.switch)
            .controlSize(.small)
            .disabled(!connected)
            .accessibilityLabel("\(server.name) enabled")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }

    private func menu(_ server: McpServerSettings) -> some View {
        let others = fleet.mcpEditable.filter { $0.hostId != machine.hostId }
        return Menu {
            Button("Edit…", systemImage: "pencil") { sheet = .edit(machine.hostId, server) }
            if !others.isEmpty {
                Menu("Copy to Machine", systemImage: "square.on.square") {
                    ForEach(others, id: \.hostId) { other in
                        Button(other.name) { copy(server, to: [other]) }
                    }
                    if others.count > 1 {
                        Divider()
                        Button("All Other Machines") { copy(server, to: others) }
                    }
                }
            }
            Divider()
            Button("Remove…", systemImage: "trash", role: .destructive) { removing = server }
        } label: {
            Image(systemName: "ellipsis").font(.callout.weight(.semibold)).foregroundStyle(Theme.secondary)
                .frame(width: 28, height: 28).contentShape(.rect)
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .disabled(!connected)
        .accessibilityLabel("\(server.name) actions")
    }

    private func projectName(_ project: ProjectId) -> String {
        machine.projects.first { $0.projectId == project }?.name ?? project
    }

    private func change(_ body: @escaping ([McpServerSettings]) -> [McpServerSettings]) {
        Task {
            do {
                error = nil
                try await lists.change(on: machine.hostId, body)
            } catch {
                self.error = describe(error)
                await lists.reload(machine.hostId)
            }
        }
    }

    private func copy(_ server: McpServerSettings, to machines: [Machine]) {
        Task {
            let failures = await lists.add([server], to: machines.map(\.hostId))
            let names = machines.map(\.name).joined(separator: ", ")
            if let failure = failures.first {
                let name = machines.first { $0.hostId == failure.key }?.name ?? failure.key
                fleet.toast = Toast(text: "Could not copy \(server.name) to \(name)", detail: failure.value, failed: true)
            } else {
                fleet.toast = Toast(text: "Copied \(server.name) to \(names)")
            }
        }
    }
}

/// The sheet `McpSheet` names.
struct McpSheetView: View {
    let fleet: Fleet
    let lists: McpServerLists
    let sheet: McpSheet

    var body: some View {
        switch sheet {
        case .add(let targets): McpServerSheet(fleet: fleet, lists: lists, editing: nil, targets: targets)
        case .edit(let host, let server): McpServerSheet(fleet: fleet, lists: lists, editing: (host, server), targets: [host])
        case .importJSON(let targets): McpImportSheet(fleet: fleet, lists: lists, targets: targets)
        }
    }
}

/// Adds an MCP server to some of the machines, or edits one on its machine.
struct McpServerSheet: View {
    let fleet: Fleet
    let lists: McpServerLists
    /// The machine and the server being edited; adding when nil.
    let editing: (host: HostId, server: McpServerSettings)?
    @State var targets: Set<HostId>
    @State private var draft = McpServerDraft()
    @State private var started = false
    @State private var error: String?
    @Environment(\.dismiss) private var dismiss

    private var machines: [Machine] { fleet.mcpEditable }

    /// The names the other servers on the edited machine use; adding replaces one of its name.
    private var taken: [String] {
        guard let editing else { return [] }
        return (lists.servers(on: editing.host) ?? []).map(\.name).filter { $0 != editing.server.name }
    }

    private var problem: String? {
        if editing == nil, targets.isEmpty { return "Pick at least one machine." }
        return draft.problem(taken: taken)
    }

    /// The machines adding replaces a server of the same name on.
    private var replacing: [String] {
        guard editing == nil else { return [] }
        let name = draft.server.name
        return machines.filter { targets.contains($0.hostId) }
            .filter { lists.servers(on: $0.hostId)?.contains { $0.name == name } == true }.map(\.name)
    }

    var body: some View {
        let machineName = editing.flatMap { editing in fleet.machines.first { $0.hostId == editing.host }?.name }
        SheetScaffold(title: editing == nil ? "Add MCP Server" : "Edit \(editing?.server.name ?? "")",
                      subtitle: machineName ?? "Every session on the machines gets it", height: 640,
                      hint: "Values go to the machine’s daemon.toml as given; keep tokens in Vault secrets.") {
            McpServerForm(draft: $draft, projects: projects)
            if editing == nil {
                Field(label: "Machines") { McpMachinePicker(machines: machines, selection: $targets) }
            }
            if !draft.name.isEmpty || editing != nil, let problem {
                Label(problem, systemImage: "exclamationmark.circle").font(.footnote).foregroundStyle(Theme.failure)
            } else if !replacing.isEmpty {
                Label("Replaces the server named \(draft.server.name) on \(replacing.joined(separator: ", ")).",
                      systemImage: "arrow.triangle.2.circlepath")
                    .font(.footnote).foregroundStyle(Theme.accent)
            }
            if let error {
                Label(error, systemImage: "xmark.octagon").font(.footnote).foregroundStyle(Theme.failure)
                    .accessibilityIdentifier("mcp-error")
            }
        } footer: {
            Spacer()
            ActionButton(title: editing == nil ? (targets.count > 1 ? "Add to \(targets.count) Machines" : "Add Server")
                         : "Save", style: .primary) { await save() }
                .frame(maxWidth: 240)
                .disabled(problem != nil)
        }
        .onAppear {
            guard !started else { return }
            started = true
            if let editing { draft = McpServerDraft(editing.server) }
        }
        .task { for host in targets where lists.servers(on: host) == nil { await lists.reload(host) } }
    }

    /// The projects of the machines it goes to, by id.
    private var projects: [Project] {
        var seen = Set<ProjectId>()
        return machines.filter { targets.contains($0.hostId) }.flatMap(\.projects)
            .filter { seen.insert($0.projectId).inserted }
            .sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    private func save() async {
        guard problem == nil else { return }
        error = nil
        let server = draft.server
        if let editing {
            do {
                try await lists.change(on: editing.host) { current in
                    guard let index = current.firstIndex(where: { $0.name == editing.server.name }) else {
                        return current + [server]
                    }
                    var next = current
                    next[index] = server
                    return next
                }
                dismiss()
            } catch {
                self.error = describe(error)
            }
            return
        }
        let order = machines.filter { targets.contains($0.hostId) }
        let failures = await lists.add([server], to: order.map(\.hostId))
        if failures.isEmpty {
            dismiss()
        } else {
            // The machines that took it are done: only the others stay picked, to try again.
            targets = Set(failures.keys)
            error = order.compactMap { machine in failures[machine.hostId].map { "\(machine.name): \($0)" } }
                .joined(separator: "\n")
        }
    }
}

/// An MCP server's fields: its name, transport and the transport's own, its project, and
/// whether it is on.
struct McpServerForm: View {
    @Binding var draft: McpServerDraft
    let projects: [Project]

    var body: some View {
        Field(label: "Name") {
            InputBox(placeholder: "github", text: $draft.name, mono: true)
                .accessibilityIdentifier("mcp-name")
        }
        Field(label: "Transport") {
            ChoiceChips(options: [(McpKind.stdio, "stdio", "Runs a program"),
                                  (McpKind.http, "HTTP", "Streamable HTTP"),
                                  (McpKind.sse, "SSE", "Older HTTP + events")],
                        selection: $draft.kind)
        }
        switch draft.kind {
        case .stdio:
            Field(label: "Command") {
                InputBox(placeholder: "npx, or a path on the machine", text: $draft.command, mono: true)
                    .accessibilityIdentifier("mcp-command")
            }
            Field(label: "Arguments, one per line") {
                InputBox(placeholder: "-y\n@modelcontextprotocol/server-github", text: $draft.args, mono: true, lines: 2...6)
            }
            Field(label: "Environment") {
                McpPairsEditor(pairs: $draft.env, keyPlaceholder: "NAME", valuePlaceholder: "value", masked: false)
            }
            Field(label: "Vault secrets") {
                McpSecretsEditor(secrets: $draft.secrets)
            }
        case .http, .sse:
            Field(label: "URL") {
                InputBox(placeholder: draft.kind == .sse ? "https://mcp.example.com/sse" : "https://mcp.example.com/mcp",
                         text: $draft.url, mono: true)
                    .accessibilityIdentifier("mcp-url")
            }
            Field(label: "Headers") {
                McpPairsEditor(pairs: $draft.headers, keyPlaceholder: "Authorization", valuePlaceholder: "Bearer …",
                               masked: true)
            }
        }
        Field(label: "Sessions") {
            Picker("Sessions", selection: $draft.project) {
                Text("Every session").tag(ProjectId?.none)
                ForEach(projects, id: \.projectId) { project in
                    Text(project.name).tag(ProjectId?.some(project.projectId))
                }
                if let project = draft.project, !projects.contains(where: { $0.projectId == project }) {
                    Text(project).tag(ProjectId?.some(project))
                }
            }
            .labelsHidden()
            .pickerStyle(.menu)
            .fixedSize()
        }
        Toggle("Enabled", isOn: $draft.enabled)
            .toggleStyle(.switch)
            .font(.subheadline)
            .foregroundStyle(Theme.text)
    }
}

/// Lines of a key and a value, to add and remove; `masked` hides the values until shown. On a
/// phone the value goes under its key, so neither is cut short.
struct McpPairsEditor: View {
    @Binding var pairs: [McpPair]
    let keyPlaceholder: String
    let valuePlaceholder: String
    let masked: Bool
    @State private var shown: Set<UUID> = []

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach($pairs) { $pair in
                #if os(iOS)
                VStack(spacing: 6) {
                    HStack(spacing: 8) {
                        key($pair)
                        remove(pair)
                    }
                    HStack(spacing: 8) {
                        value($pair)
                        reveal(pair)
                    }
                }
                #else
                HStack(spacing: 8) {
                    key($pair).frame(width: 170)
                    value($pair)
                    reveal(pair)
                    remove(pair)
                }
                #endif
            }
            PillButton(title: pairs.isEmpty ? "Add" : "Add Another", symbol: "plus") { pairs.append(McpPair()) }
        }
    }

    private func key(_ pair: Binding<McpPair>) -> some View {
        InputBox(placeholder: keyPlaceholder, text: pair.key, mono: true)
    }

    /// The value; masked, its length in dots until shown, and a tap shows it to edit.
    @ViewBuilder private func value(_ pair: Binding<McpPair>) -> some View {
        let id = pair.wrappedValue.id
        if masked && !shown.contains(id) && !pair.wrappedValue.value.isEmpty {
            Button { shown.insert(id) } label: {
                Text(String(repeating: "•", count: min(pair.wrappedValue.value.count, 16)))
                    .font(Theme.mono)
                    .foregroundStyle(Theme.secondary)
                    .padding(12)
                    .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                    .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
                    .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
                    .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("\(pair.wrappedValue.key) value, hidden")
        } else {
            InputBox(placeholder: valuePlaceholder, text: pair.value, mono: true)
        }
    }

    @ViewBuilder private func reveal(_ pair: McpPair) -> some View {
        if masked {
            let visible = shown.contains(pair.id)
            IconButton(symbol: visible ? "eye.slash" : "eye", help: visible ? "Hide" : "Show") {
                if visible { shown.remove(pair.id) } else { shown.insert(pair.id) }
            }
        }
    }

    private func remove(_ pair: McpPair) -> some View {
        IconButton(symbol: "minus", help: "Remove") { pairs.removeAll { $0.id == pair.id } }
    }
}

/// The vault secrets a stdio server gets, as `NAME` or `set/NAME`.
struct McpSecretsEditor: View {
    @Binding var secrets: [String]
    @State private var new = ""

    private var valid: Bool {
        let name = new.trimmingCharacters(in: .whitespaces)
        return McpServers.isValidSecretRef(name) && !secrets.contains(name)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if !secrets.isEmpty {
                FlowLayout(spacing: 6) {
                    ForEach(secrets, id: \.self) { secret in
                        HStack(spacing: 4) {
                            Image(systemName: "key.fill").imageScale(.small)
                            Text(secret).font(Theme.monoSmall)
                            Button { secrets.removeAll { $0 == secret } } label: {
                                Image(systemName: "xmark").imageScale(.small)
                            }
                            .buttonStyle(.plain)
                            .accessibilityLabel("Remove \(secret)")
                        }
                        .foregroundStyle(Theme.text)
                        .padding(.horizontal, 8).padding(.vertical, 5)
                        .background(Theme.raised, in: .capsule)
                    }
                }
            }
            HStack(spacing: 8) {
                InputBox(placeholder: "GITHUB_TOKEN or set/NAME", text: $new, mono: true)
                    .onSubmit(add)
                PillButton(title: "Add", symbol: "plus", action: add).disabled(!valid)
            }
            if !new.isEmpty, !valid {
                Text(secrets.contains(new.trimmingCharacters(in: .whitespaces)) ? "Listed already."
                     : "NAME in capitals, digits and underscores, or set/NAME.")
                    .font(.footnote).foregroundStyle(Theme.failure)
            }
        }
    }

    private func add() {
        guard valid else { return }
        secrets.append(new.trimmingCharacters(in: .whitespaces))
        new = ""
    }
}

/// Machines to pick, any number of them, and all at once.
struct McpMachinePicker: View {
    let machines: [Machine]
    @Binding var selection: Set<HostId>

    var body: some View {
        FlowLayout(spacing: 8) {
            if machines.count > 1 {
                let all = selection == Set(machines.map(\.hostId))
                chip("All machines", selected: all) { selection = all ? [] : Set(machines.map(\.hostId)) }
            }
            ForEach(machines, id: \.hostId) { machine in
                let selected = selection.contains(machine.hostId)
                chip(machine.name, selected: selected) {
                    if selected { selection.remove(machine.hostId) } else { selection.insert(machine.hostId) }
                }
            }
        }
    }

    private func chip(_ title: String, selected: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 6) {
                Image(systemName: selected ? "checkmark.circle.fill" : "circle")
                Text(title).font(.subheadline.weight(.semibold))
            }
            .foregroundStyle(Theme.text)
            .padding(.horizontal, 12)
            .frame(minHeight: 40)
            .primaryFill(in: .rect(cornerRadius: Theme.corner), on: selected)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// Imports the `mcpServers` JSON a server's README or Claude Desktop's config gives, onto some
/// of the machines.
struct McpImportSheet: View {
    let fleet: Fleet
    let lists: McpServerLists
    @State var targets: Set<HostId>
    @State private var text = ""
    @State private var error: String?
    @Environment(\.dismiss) private var dismiss

    private var machines: [Machine] { fleet.mcpEditable }

    private var parsed: Result<[McpServerSettings], McpImport.Failure> {
        Result { try McpImport.parse(text) }.mapError { $0 as? McpImport.Failure ?? McpImport.Failure(message: describe($0)) }
    }

    var body: some View {
        let parsed = parsed
        SheetScaffold(title: "Import MCP Servers", subtitle: "From a README or Claude Desktop’s config", height: 640,
                      hint: "Entries with a `url` are HTTP unless their `type` says `sse`.") {
            Field(label: "JSON") {
                VStack(alignment: .leading, spacing: 8) {
                    InputBox(placeholder: "{\n  \"mcpServers\": { … }\n}", text: $text, mono: true, lines: 6...14)
                        .accessibilityIdentifier("mcp-json")
                    PillButton(title: "Paste from Clipboard", symbol: "doc.on.clipboard") {
                        if let pasted = Clipboard.string { text = pasted }
                    }
                }
            }
            if !text.isEmpty {
                switch parsed {
                case .failure(let failure):
                    Label(failure.message, systemImage: "exclamationmark.circle")
                        .font(.footnote).foregroundStyle(Theme.failure)
                case .success(let servers):
                    preview(servers)
                }
            }
            Field(label: "Machines") { McpMachinePicker(machines: machines, selection: $targets) }
            if let error {
                Label(error, systemImage: "xmark.octagon").font(.footnote).foregroundStyle(Theme.failure)
                    .accessibilityIdentifier("mcp-error")
            }
        } footer: {
            Spacer()
            let count = (try? parsed.get())?.count ?? 0
            ActionButton(title: count == 0 ? "Import" : count == 1 ? "Import 1 Server" : "Import \(count) Servers",
                         style: .primary) { await save() }
                .frame(maxWidth: 240)
                .disabled(count == 0 || targets.isEmpty)
        }
        .task { for host in targets where lists.servers(on: host) == nil { await lists.reload(host) } }
    }

    private func preview(_ servers: [McpServerSettings]) -> some View {
        SettingsList(title: servers.count == 1 ? "1 server" : "\(servers.count) servers") {
            ForEach(Array(servers.enumerated()), id: \.element.name) { index, server in
                if index > 0 { RowDivider() }
                HStack(spacing: 8) {
                    Text(server.name).font(.subheadline.weight(.semibold).monospaced()).foregroundStyle(Theme.text)
                    Badge(text: McpKind(server.transport).title)
                    Text(McpServers.target(server.transport)).font(Theme.monoSmall).foregroundStyle(Theme.tertiary)
                        .lineLimit(1).truncationMode(.middle)
                    Spacer(minLength: 4)
                    let on = machines.filter { targets.contains($0.hostId) }
                        .filter { lists.servers(on: $0.hostId)?.contains { $0.name == server.name } == true }
                    if !on.isEmpty {
                        Text("Replaces on \(on.map(\.name).joined(separator: ", "))")
                            .font(.caption).foregroundStyle(Theme.accent).lineLimit(1)
                    }
                }
                .padding(.horizontal, 14)
                .frame(minHeight: 40)
            }
        }
    }

    private func save() async {
        guard case .success(let servers) = parsed else { return }
        error = nil
        let order = machines.filter { targets.contains($0.hostId) }
        let failures = await lists.add(servers, to: order.map(\.hostId))
        if failures.isEmpty {
            dismiss()
        } else {
            targets = Set(failures.keys)
            error = order.compactMap { machine in failures[machine.hostId].map { "\(machine.name): \($0)" } }
                .joined(separator: "\n")
        }
    }
}

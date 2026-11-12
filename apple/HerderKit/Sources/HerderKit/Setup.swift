import Herder
import SwiftUI

/// Something on your machines that needs you, with the one button that fixes it. A problem
/// needs you (amber); a suggestion, such as an update, does not stop anything (blue).
struct SetupProblem: Identifiable, Equatable {
    enum Fix: Equatable {
        /// Connect to every machine again.
        case reconnect
        /// Add a Device's code, which another device of yours scans.
        case addDevice
        /// Update herder on these machines when they are idle.
        case updateHerder([HostId])
        /// Update every CLI herder can update.
        case updateCLIs
        /// A signed-out account's page, where it signs in again.
        case signIn(loginId: String)
        /// Approve with your passkey on the machines that know you by an earlier setup.
        case claim
    }

    let id: String
    var suggestion = false
    let symbol: String
    let title: String
    let detail: String
    let action: String
    let fix: Fix

    var tint: Color { suggestion ? Theme.action : Urgency.needsYou.color }
}

extension SetupProblem {
    /// Everything that needs you across `machines`, problems first, then suggestions: a machine
    /// not connected, machines that know you by an earlier setup, a device of yours missing from
    /// a machine you own, a signed-out account, then herder and CLI updates. `directory` is nil
    /// until the machines list their people.
    static func found(machines: [Machine], overview: ProvidersOverview, unclaimed: Bool,
                      directory: Directory?) -> [SetupProblem] {
        var found: [SetupProblem] = []
        for machine in machines {
            guard case .disconnected(let error) = machine.connection else { continue }
            let name = UnpairedVault.name(of: machine)
            found.append(SetupProblem(id: "offline.\(machine.hostId)", symbol: "bolt.horizontal.circle",
                                      title: "\(name) isn’t connected", detail: ConnectionState.explain(error),
                                      action: "Reconnect", fix: .reconnect))
        }
        if unclaimed {
            found.append(SetupProblem(id: "claim", symbol: "person.badge.key",
                                      title: "Some machines know you by an earlier setup",
                                      detail: "Approve once with your passkey, and they know you by it.",
                                      action: "Approve…", fix: .claim))
        }
        if let directory { found += missingDevices(directory) }
        for signedOut in overview.signedOut where signedOut.place.owner {
            let login = signedOut.login
            let account = login.email ?? login.account.label
            found.append(SetupProblem(
                id: "signed-out.\(signedOut.id)", symbol: "person.crop.circle.badge.exclamationmark",
                title: "\(ModelCatalog.providerName(login.provider)) · \(account) is signed out on \(signedOut.place.name)",
                detail: "Sessions there can’t use it until it signs in again.",
                action: "Sign In…", fix: .signIn(loginId: login.id)))
        }
        let behind = HerderVersionsSection.behind(machines).filter { $0.machine.herderVersion != nil }
        if let newest = behind.first?.newest {
            let versions = Set(behind.compactMap { $0.machine.herderVersion })
            let names = Directory.list(behind.map { $0.machine.name })
            let run = behind.count == 1 ? "runs" : "run"
            found.append(SetupProblem(
                id: "herder-update", suggestion: true, symbol: "arrow.up",
                title: "herder \(newest) is out",
                detail: versions.count == 1 ? "\(names) \(run) \(versions.first ?? "")." : "\(names) \(run) an older herder.",
                action: behind.count == 1 ? "Update" : "Update All \(behind.count)",
                fix: .updateHerder(behind.map { $0.machine.hostId })))
        }
        let clis = overview.updatable
        if !clis.isEmpty {
            let providers = Directory.list(Array(Set(clis.map { ModelCatalog.providerName($0.provider) })).sorted())
            let hosts = Directory.list(Array(Set(clis.map(\.machine))).sorted())
            found.append(SetupProblem(
                id: "cli-updates", suggestion: true, symbol: "arrow.down.circle",
                title: clis.count == 1 ? "1 CLI update" : "\(clis.count) CLI updates",
                detail: "\(providers) on \(hosts).", action: "Update All", fix: .updateCLIs))
        }
        return found
    }

    @MainActor static func found(fleet: Fleet, directory: Directory?) -> [SetupProblem] {
        found(machines: fleet.machines, overview: ProvidersOverview(fleet: fleet), unclaimed: fleet.hasUnclaimed,
              directory: directory)
    }

    /// Your other Macs and iPhones that are not signed in to every machine you own. A
    /// terminal signs in to one machine with `herder login`, so it misses none.
    static func missingDevices(_ directory: Directory) -> [SetupProblem] {
        let owned = directory.columns.filter { !$0.vault && directory.me.roles[$0.id] == .owner }
        return directory.devices.compactMap { device in
            guard !device.current, device.kind != .terminal else { return nil }
            let missing = owned.filter { device.paired[$0.id] == nil }
            guard !missing.isEmpty else { return nil }
            return SetupProblem(id: "device.\(device.id)", symbol: device.kind == .phone ? "iphone" : "laptopcomputer",
                                title: "Your \(device.title) isn’t on \(Directory.list(missing.map(\.name))) yet",
                                detail: "Show the code here, and scan it on your \(device.title) to sign in to the rest.",
                                action: "Add a Device", fix: .addDevice)
        }
    }

    /// The Setup header's line: "2 need you · 5 machines · 3 devices", or "Everything is set
    /// up · …" with nothing to fix.
    static func status(_ problems: [SetupProblem], machines: [Machine], directory: Directory?) -> String {
        let hosts = machines.filter { $0.hosts.isEmpty && $0.vault == nil }.count
        var parts = [problems.isEmpty ? "Everything is set up"
            : problems.count == 1 ? "1 needs you" : "\(problems.count) need you"]
        parts.append(hosts == 1 ? "1 machine" : "\(hosts) machines")
        if let devices = directory?.devices.count, devices > 0 {
            parts.append(devices == 1 ? "1 device" : "\(devices) devices")
        }
        return parts.joined(separator: " · ")
    }
}

/// How many things need you, for the Setup row and tab's badge: on a view of its own, so the
/// rest of the sidebar does not draw again as the machines change.
struct SetupCount<Content: View>: View {
    let fleet: Fleet
    let people: FleetPeople
    @ViewBuilder let content: (Int) -> Content

    var body: some View {
        content(SetupProblem.found(fleet: fleet, directory: people.directory(fleet.machines)).count)
    }
}

/// Setup's pane on the Mac and iPad: its header, with what needs you counted in its line and
/// Add a Device and Add a Machine, over the page. A view of its own, so only it draws again as
/// the machines change.
struct SetupPane: View {
    let fleet: Fleet
    let people: FleetPeople
    @Binding var sheet: AppSheet?
    let switcher: Switcher

    var body: some View {
        let machines = fleet.machines
        let directory = people.directory(machines)
        let problems = SetupProblem.found(fleet: fleet, directory: directory)
        Pane(fleet: fleet, title: "Setup", status: SetupProblem.status(problems, machines: machines, directory: directory),
             switcher: switcher) {
            SetupView(fleet: fleet, people: people, sheet: $sheet)
        } actions: {
            if !AwsSignIn.machines(machines).isEmpty {
                PaneButton(title: "AWS Sign-in", symbol: "key") { sheet = .awsSignIn }
            }
            PaneButton(title: "Add a Device", symbol: "qrcode") { sheet = .addDevice }
                .disabled(fleet.shareLink == nil)
                .accessibilityIdentifier("add-device")
            PaneButton(title: "Add a Machine", symbol: "plus", prominent: true) { sheet = .pair(.machine) }
                .accessibilityIdentifier("add-to-fleet")
        }
    }
}

/// Setup: one page for your machines, your devices and the people on them. In order: what
/// needs you, each with its fix (hidden when nothing does); you; the machines, a vault among
/// them; and the people. On iPhone, also the way to Skills, Providers and Integrations.
struct SetupView: View {
    let fleet: Fleet
    let people: FleetPeople
    @Binding var sheet: AppSheet?
    /// Leads to Skills, Providers and Integrations, where they have no section of their own,
    /// and puts the header's line at the top.
    var showsSections = false
    @Environment(\.pages) private var pages

    var body: some View {
        let machines = fleet.machines
        let directory = people.directory(machines)
        let problems = SetupProblem.found(fleet: fleet, directory: directory)
        ScrollView {
            VStack(alignment: .leading, spacing: 28) {
                if showsSections {
                    Text(SetupProblem.status(problems, machines: machines, directory: directory))
                        .font(.subheadline).foregroundStyle(Theme.secondary)
                }
                // A device with no machines shows Welcome instead.
                if !problems.isEmpty {
                    NeedsYouSection(problems: problems, fix: fix)
                }
                youSection(directory)
                VStack(alignment: .leading, spacing: 8) {
                    SectionHeading(title: "Machines", count: machines.count)
                    MachinesView(fleet: fleet, sheet: $sheet)
                }
                if let directory {
                    PeopleSection(fleet: fleet, people: people, directory: directory, sheet: $sheet)
                } else if !people.failures.isEmpty {
                    Text(people.failures.values.sorted().joined(separator: "\n"))
                        .font(Theme.body).foregroundStyle(Theme.secondary)
                }
                if showsSections {
                    VStack(alignment: .leading, spacing: 8) {
                        SectionHeading(title: "Workspace")
                        WorkspaceLinks(fleet: fleet, sheet: $sheet)
                    }
                }
            }
            .pageColumn(width: .infinity)
        }
        .background(.ground)
        .refreshable {
            fleet.wake()
            await people.reload(fleet.machines)
        }
        .navigationTitle("Setup")
        .toolbar {
            #if os(iOS)
            ActivityToolbarButton(fleet: fleet) { sheet = .activity }
            if !AwsSignIn.machines(machines).isEmpty {
                Button("AWS Sign-in", systemImage: "key") { sheet = .awsSignIn }
            }
            Button("Add a Device", systemImage: "qrcode") { sheet = .addDevice }
                .disabled(fleet.shareLink == nil)
                .accessibilityIdentifier("add-device")
            Button("Add a Machine", systemImage: "plus") { sheet = .pair(.machine) }
                .accessibilityIdentifier("add-to-fleet")
            #endif
        }
    }

    /// You, in one row: your role and passkey, and how many devices you have; it opens You.
    private func youSection(_ directory: Directory?) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: "You")
            SettingsGroup {
                let me = directory?.me
                let devices = directory?.devices.count ?? 0
                SummaryRow(title: me?.name ?? "You",
                           summary: directory.map(Self.you) ?? "",
                           value: devices == 0 ? "" : devices == 1 ? "1 device" : "\(devices) devices",
                           open: { pages.open(.you) },
                           leading: { Avatar(name: me?.name ?? "You", id: me?.id ?? "", size: 30) },
                           accessory: { EmptyView() })
                    .accessibilityIdentifier("setup-you")
            }
        }
    }

    /// "Owner of every machine · passkey in iCloud Keychain".
    nonisolated static func you(_ directory: Directory) -> String {
        let passkey: String? = switch directory.passkey {
        case .none: "no passkey yet"
        case .stored(let keychain?): "passkey in \(keychain)"
        case .stored(nil): nil
        }
        return ([directory.scope(directory.me)] + [passkey].compactMap(\.self)).joined(separator: " · ")
    }

    private func fix(_ fix: SetupProblem.Fix) {
        switch fix {
        case .reconnect: fleet.wake()
        case .addDevice: sheet = .addDevice
        case .updateHerder(let hosts): for host in hosts { fleet.updateHerder(on: host, when: .idle) }
        case .updateCLIs: fleet.update(ProvidersOverview(fleet: fleet).updatable)
        case .signIn(let loginId): pages.open(.account(loginId: loginId))
        case .claim: fleet.offerClaim(in: $sheet)
        }
    }
}

/// The top of Setup: each thing that needs you as a card, with the button that fixes it.
struct NeedsYouSection: View {
    let problems: [SetupProblem]
    let fix: (SetupProblem.Fix) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: "Needs you", count: problems.count)
            VStack(spacing: 10) {
                ForEach(problems) { problem in
                    NeedsYouCard(problem: problem) { fix(problem.fix) }
                }
            }
        }
        .accessibilityIdentifier("setup-needs-you")
    }
}

/// One thing that needs you: its mark in amber, or blue for a suggestion, what happened and
/// what to do, and its fix, beside it where there is room and under it where there is not.
struct NeedsYouCard: View {
    let problem: SetupProblem
    let action: () -> Void

    var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(alignment: .center, spacing: 14) {
                mark
                text
                Spacer(minLength: 12)
                button
            }
            HStack(alignment: .top, spacing: 14) {
                mark
                VStack(alignment: .leading, spacing: 12) {
                    text
                    button
                }
                Spacer(minLength: 0)
            }
        }
        .padding(14)
        .background(.card, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("needs-you-\(problem.id)")
    }

    private var mark: some View {
        Image(systemName: problem.symbol)
            .font(.callout.weight(.semibold))
            .foregroundStyle(problem.tint)
            .frame(width: 32, height: 32)
            .background(problem.tint.opacity(0.16), in: .circle)
    }

    private var text: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(problem.title).font(Theme.body.weight(.semibold)).foregroundStyle(Theme.text)
                .fixedSize(horizontal: false, vertical: true)
            Text(problem.detail).font(.footnote).foregroundStyle(Theme.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var button: some View {
        SettingsButton(title: problem.action, style: .primary, action: action).fixedSize()
    }
}

/// Skills, Providers and Integrations on iPhone, where they have no tab of their own.
private struct WorkspaceLinks: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    @Environment(\.pages) private var pages

    var body: some View {
        SettingsGroup {
            NavigationLink { SkillsView(fleet: fleet, sheet: $sheet) } label: {
                link("Skills", symbol: "book.closed", summary: Self.skills(SkillLibrary(fleet.machines).skills.count))
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("skills-link")
            RowDivider()
            NavigationLink { ProvidersView(fleet: fleet, sheet: $sheet) } label: {
                let gaps = SetUpGaps(ProviderAccounts(machines: fleet.machines))
                link("Providers", symbol: "person.2", summary: gaps.isEmpty ? "Accounts, plan limits, usage and CLIs" : gaps.title)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("providers-link")
            RowDivider()
            // A page on the stack, so the bot pages it opens push over it rather than replace it.
            Button { pages.open(.integrations) } label: {
                link("Integrations", symbol: Switcher.integrationsSymbol, summary: "Slack bots on your machines")
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("integrations-link")
        }
    }

    private static func skills(_ count: Int) -> String {
        count == 1 ? "1 skill in the library" : "\(count) skills in the library"
    }

    private func link(_ title: String, symbol: String, summary: String) -> some View {
        HStack(spacing: 12) {
            Image(systemName: symbol).foregroundStyle(Theme.secondary).frame(width: 24)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text)
                Text(summary).font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
            }
            Spacer(minLength: 8)
            Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
        }
        .padding(.horizontal, Theme.cellPadding)
        .frame(minHeight: Theme.tallRowHeight)
        .contentShape(.rect)
    }
}

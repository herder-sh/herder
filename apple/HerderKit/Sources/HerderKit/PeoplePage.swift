import Foundation
import Herder
import Observation
import SwiftUI

/// The fleet's people, from each machine's users, until the machines know one person across
/// them (P17.1): a user is the same person wherever they have the same name. Changes go to each
/// machine the change is for, then every machine is asked again.
@MainActor @Observable
final class FleetPeople {
    typealias Send = @MainActor (HostId, CommandBody) async throws -> CommandResult

    private let send: Send
    private(set) var users: [HostId: [UserInfo]] = [:]
    /// Why a machine did not list its users.
    private(set) var failures: [HostId: String] = [:]

    init(send: @escaping Send) {
        self.send = send
    }

    convenience init(fleet: Fleet) {
        let client = fleet.client
        self.init { try await client.send(hostId: $0, command: $1) }
    }

    /// The client this app pairs as, without its version: "herder-macos".
    nonisolated static var thisClient: String {
        #if os(macOS)
        "herder-macos"
        #else
        "herder-ios"
        #endif
    }

    func reload(_ machines: [Machine]) async {
        for machine in machines where machine.connection == .connected {
            do {
                guard case .users(let users) = try await send(machine.hostId, .listUsers) else {
                    throw HerderError.Local(detail: "the machine did not list its users")
                }
                self.users[machine.hostId] = users
                failures[machine.hostId] = nil
            } catch {
                failures[machine.hostId] = describe(error)
            }
        }
    }

    /// The directory of `machines`, or nil before any of them listed its users.
    func directory(_ machines: [Machine], localNames: Set<String> = MachineRow.localNames) -> Directory? {
        guard !users.isEmpty else { return nil }
        let local = MachineRow.thisMac(machines.filter { $0.hosts.isEmpty && $0.vault == nil }, names: localNames)
        let columns = machines.filter { users[$0.hostId] != nil }.map { machine in
            Directory.Column(id: machine.hostId, name: machine.hostId == local ? "this Mac" : machine.name,
                             vault: !machine.hosts.isEmpty || machine.vault != nil)
        }
        let me = Self.me(users, machines: machines) ?? "you"
        return .merging(users, columns: columns, me: me, thisClient: Self.thisClient + "/")
    }

    /// Your name: on each machine, the user with this app's device online and the role the
    /// machine gave this device; the name most machines agree on.
    nonisolated static func me(_ users: [HostId: [UserInfo]], machines: [Machine]) -> String? {
        var votes: [String: Int] = [:]
        for machine in machines {
            let mine = (users[machine.hostId] ?? []).filter { user in
                user.role == machine.role
                    && user.devices.contains { $0.online && $0.client.hasPrefix(thisClient + "/") }
            }
            for user in mine { votes[user.name.lowercased(), default: 0] += 1 }
        }
        return votes.max { ($0.value, $1.key) < ($1.value, $0.key) }?.key
    }

    /// Sets `person`'s role on `hosts`; taking access away waits for one identity per person.
    func setRole(_ role: Role?, of person: Directory.Person, on hosts: [Directory.Column], machines: [Machine]) async throws {
        guard let role else {
            throw HerderError.Local(detail: "Taking \(person.name)'s access away arrives with one identity per person.")
        }
        for host in hosts {
            guard let user = users[host.id]?.first(where: { $0.name.lowercased() == person.id }) else {
                throw HerderError.Local(detail: "Invite \(person.name) to \(host.name) first.")
            }
            if user.role != role { _ = try await send(host.id, .setUserRole(userId: user.userId, role: role)) }
        }
        await reload(machines)
    }

    /// Signs `device` out of every machine it paired with.
    func signOut(_ device: Directory.Device, machines: [Machine]) async throws {
        for (hostId, devices) in device.paired {
            for deviceId in devices { _ = try await send(hostId, .revokeDevice(deviceId: deviceId)) }
        }
        await reload(machines)
    }
}

/// The People page: the whole fleet's, or one machine's when opened from its page.
struct PeoplePage: View {
    let fleet: Fleet
    /// The machine whose people show; every machine's when nil.
    var hostId: HostId?
    @Binding var sheet: AppSheet?
    @State private var people: FleetPeople
    @State private var error: String?
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    #endif

    init(fleet: Fleet, hostId: HostId? = nil, sheet: Binding<AppSheet?>) {
        self.fleet = fleet
        self.hostId = hostId
        _sheet = sheet
        _people = State(initialValue: FleetPeople(fleet: fleet))
    }

    var body: some View {
        let machines = fleet.machines
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                if let error {
                    Text(error).font(.footnote).foregroundStyle(Theme.failure).accessibilityIdentifier("people-error")
                }
                if let directory = directory(machines) {
                    PeopleView(directory: directory, grid: grid,
                               setRole: { person, role, host in
                                   let hosts = host.map { id in directory.columns.filter { $0.id == id } }
                                       ?? directory.columns.filter { !$0.vault || role == .owner }
                                   run { try await people.setRole(role, of: person, on: hosts, machines: machines) }
                               },
                               revoke: { device in run { try await people.signOut(device, machines: machines) } },
                               invite: invite)
                } else if people.failures.isEmpty {
                    ProgressView().frame(maxWidth: .infinity, minHeight: 120)
                } else {
                    Text(people.failures.values.sorted().joined(separator: "\n"))
                        .font(Theme.body).foregroundStyle(Theme.secondary)
                }
            }
            .pageColumn()
        }
        .background(.ground)
        .navigationTitle("People")
        #if os(iOS)
        .toolbar {
            Button("Invite", systemImage: "plus", action: invite)
        }
        #endif
        .refreshable { await people.reload(machines) }
        .task(id: machines.filter { $0.connection == .connected }.map(\.hostId)) { await people.reload(machines) }
        .onChange(of: sheet) { if sheet == nil { Task { await people.reload(machines) } } }
    }

    private func directory(_ machines: [Machine]) -> Directory? {
        guard let directory = people.directory(machines) else { return nil }
        return hostId.map(directory.only) ?? directory
    }

    private var grid: Bool {
        #if os(iOS)
        sizeClass != .compact
        #else
        true
        #endif
    }

    /// Invites someone to the machine shown, else the first machine you own.
    private func invite() {
        if let host = PeoplePage.inviteHost(fleet.machines, shown: hostId) { sheet = .invite(host) }
    }

    nonisolated static func inviteHost(_ machines: [Machine], shown: HostId?) -> HostId? {
        shown ?? machines.first { $0.role == .owner && $0.connection == .connected && $0.hosts.isEmpty && $0.vault == nil }?.hostId
    }

    private func run(_ action: @escaping () async throws -> Void) {
        error = nil
        Task {
            do { try await action() } catch { self.error = describe(error) }
        }
    }
}

/// A machine's way to its people: the People page, narrowed to it.
struct PeopleLink: View {
    let hostId: HostId
    @Environment(\.pages) private var pages

    var body: some View {
        SettingsList(title: "People") {
            SummaryRow(title: "People on this machine", summary: "Their roles here, and who can use it") {
                pages.open(.people(hostId))
            }
        }
        .accessibilityIdentifier("machine-people")
    }
}

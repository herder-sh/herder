import Foundation
import Herder

/// Everyone across the fleet, one person per person: you with your passkey and devices, then
/// your teammates, each with a role on each machine. The People page draws it.
struct Directory: Equatable {
    /// A machine or vault, a column of the grid.
    struct Column: Equatable, Identifiable {
        let id: HostId
        let name: String
        /// Vaults keep backups and secrets; a member has nothing to do on one.
        var vault = false
    }

    /// A person and their role on each machine they can use; a machine they cannot is missing.
    struct Person: Equatable, Identifiable {
        let id: String
        let name: String
        var roles: [HostId: Role]
    }

    /// One of your devices: a Mac, an iPhone, or a terminal signed in with `herder login`.
    struct Device: Equatable, Identifiable {
        enum Kind: Equatable { case mac, phone, terminal }
        let id: String
        let kind: Kind
        /// Where a terminal runs.
        var host: String?
        var lastSeen: Date?
        /// The device showing this.
        var current = false
        /// Its pairing on each machine, to sign it out of every one.
        var paired: [HostId: [String]] = [:]
    }

    /// Whether you have a passkey, and where it is kept when that is known.
    enum Passkey: Equatable {
        case none
        case stored(in: String?)
    }

    var columns: [Column]
    var me: Person
    var passkey = Passkey.none
    var devices: [Device] = []
    var teammates: [Person] = []

    var everyone: [Person] { [me] + teammates }

    /// The directory of one machine: its column, and the people who can use it.
    func only(_ hostId: HostId) -> Directory {
        var only = self
        only.columns = columns.filter { $0.id == hostId }
        only.teammates = teammates.filter { $0.roles[hostId] != nil }
        return only
    }

    /// "3 people · 5 machines".
    var status: String {
        let people = everyone.count == 1 ? "1 person" : "\(everyone.count) people"
        let machines = columns.count == 1 ? "1 machine" : "\(columns.count) machines"
        return "\(people) · \(machines)"
    }

    /// The machines that run sessions, where a member has something to do.
    private var hosts: [Column] { columns.filter { !$0.vault } }

    /// A person's role across the fleet, as their row's menu says it.
    enum FleetRole: Equatable {
        case owner, member, custom

        var title: String {
            switch self {
            case .owner: "Owner"
            case .member: "Member"
            case .custom: "Custom"
            }
        }
    }

    /// Owner when they own every machine, member when they are a member of every one that runs
    /// sessions, and custom for anything narrower or mixed.
    func fleetRole(_ person: Person) -> FleetRole {
        if !columns.isEmpty, columns.allSatisfy({ person.roles[$0.id] == .owner }) { return .owner }
        if !hosts.isEmpty, hosts.allSatisfy({ person.roles[$0.id] == .member }),
           !columns.contains(where: { person.roles[$0.id] == .owner }) { return .member }
        return .custom
    }

    /// The line under a person's name: "Owner of every machine", "All machines", "Owner on
    /// draugelis · member elsewhere".
    func scope(_ person: Person) -> String {
        switch fleetRole(person) {
        case .owner: return "Owner of every machine"
        case .member: return "All machines"
        case .custom: break
        }
        let owned = columns.filter { person.roles[$0.id] == .owner }
        let member = hosts.filter { person.roles[$0.id] == .member }
        // A vault named after its machine says so already ("draugelis vault").
        let named = owned.filter { vault in !vault.vault || !owned.contains { !$0.vault && vault.name.hasPrefix($0.name) } }
        if owned.isEmpty && member.isEmpty { return "No machines yet" }
        if owned.isEmpty { return "Member on \(Self.list(member.map(\.name)))" }
        let owner = "Owner on \(Self.list(named.map(\.name)))"
        if member.isEmpty { return owner }
        let elsewhere = hosts.filter { person.roles[$0.id] != .owner }
        return member == elsewhere ? "\(owner) · member elsewhere" : "\(owner) · member on \(Self.list(member.map(\.name)))"
    }

    /// "a", "a and b", "a, b and c".
    nonisolated static func list(_ names: [String]) -> String {
        switch names.count {
        case 0: ""
        case 1: names[0]
        default: names.dropLast().joined(separator: ", ") + " and " + (names.last ?? "")
        }
    }
}

extension Directory.Device {
    /// "This Mac", "iPhone", "Terminal on trash-can-01".
    var title: String {
        switch kind {
        case .mac: current ? "This Mac" : "Mac"
        case .phone: current ? "This iPhone" : "iPhone"
        case .terminal: host.map { "Terminal on \($0)" } ?? "Terminal"
        }
    }

    /// "today", "2 hours ago", "Oct 27".
    func seen(now: Date, calendar: Calendar = .current) -> String {
        guard let lastSeen else { return "not yet" }
        if current || now.timeIntervalSince(lastSeen) < 60 { return "today" }
        let hours = Int(now.timeIntervalSince(lastSeen) / 3600)
        if calendar.isDate(lastSeen, inSameDayAs: now) || hours < 12 {
            return hours < 1 ? "just now" : hours == 1 ? "1 hour ago" : "\(hours) hours ago"
        }
        return lastSeen.formatted(.dateTime.month(.abbreviated).day())
    }
}

extension Directory {
    /// The fleet's people from each machine's, one person per person id. Your devices are yours
    /// across the machines, by kind.
    static func merging(_ people: [HostId: [PersonInfo]], columns: [Column], me: PersonId?,
                        thisClient: String) -> Directory {
        var found: [PersonId: Person] = [:]
        var order: [PersonId] = []
        var keyed = false
        for column in columns {
            for info in people[column.id] ?? [] {
                if found[info.personId] == nil {
                    found[info.personId] = Person(id: info.personId, name: info.name, roles: [:])
                    order.append(info.personId)
                }
                found[info.personId]?.roles[column.id] = info.role
                if info.personId == me, !info.passkeys.isEmpty { keyed = true }
            }
        }
        let mine = me.flatMap { found[$0] } ?? Person(id: me ?? "", name: "You", roles: [:])
        let devices = columns.flatMap { column in
            (people[column.id] ?? []).filter { $0.personId == mine.id }.flatMap(\.devices).map { (column.id, $0) }
        }
        return Directory(columns: columns, me: mine, passkey: keyed ? .stored(in: nil) : .none,
                         devices: Self.devices(devices, thisClient: thisClient),
                         teammates: order.filter { $0 != mine.id }.compactMap { found[$0] })
    }

    /// Your devices, one per kind of client: each machine pairs a device of its own.
    private static func devices(_ devices: [(HostId, UserDevice)], thisClient: String) -> [Device] {
        let kinds: [(String, Device.Kind)] = [("herder-macos", .mac), ("herder-ios", .phone), ("herder-tui", .terminal),
                                              ("herder-cli", .terminal)]
        let this = thisClient.split(separator: "/").first.map(String.init)
        return kinds.compactMap { client, kind in
            let paired = devices.filter { $0.1.client.hasPrefix(client + "/") && !$0.1.hostOnly }
            guard let last = paired.map(\.1).max(by: { ($0.lastSeen ?? "") < ($1.lastSeen ?? "") }) else { return nil }
            return Device(id: client, kind: kind, lastSeen: last.lastSeen.flatMap(Timestamp.date), current: client == this,
                          paired: Dictionary(grouping: paired, by: \.0).mapValues { $0.map(\.1.deviceId) })
        }
    }
}

/// The fleet the mockup draws (docs/mockups/identity.html): you, Sam and Kim on three machines
/// and two vaults. The screenshot tests and previews draw it; its ids pick the mockup's avatar
/// fills.
extension Directory {
    static func demo(now: Date) -> Directory {
        let mac = Column(id: "mac", name: "this Mac")
        let trash = Column(id: "trash", name: "trash-can-01")
        let draugelis = Column(id: "draugelis", name: "draugelis")
        let vault = Column(id: "vault-0", name: "herder-vault-0", vault: true)
        let draugelisVault = Column(id: "draugelis-vault", name: "draugelis vault", vault: true)
        let columns = [mac, trash, draugelis, vault, draugelisVault]
        return Directory(
            columns: columns,
            me: Person(id: "tomas-1", name: "Tomas", roles: Dictionary(uniqueKeysWithValues: columns.map { ($0.id, .owner) })),
            passkey: .stored(in: "iCloud Keychain"),
            devices: [
                Device(id: "mac", kind: .mac, lastSeen: now, current: true),
                Device(id: "phone", kind: .phone, lastSeen: now.addingTimeInterval(-2 * 3600)),
                Device(id: "terminal", kind: .terminal, host: "trash-can-01", lastSeen: Timestamp.date("2026-10-27T10:00:00Z")),
            ],
            teammates: [
                Person(id: "sam-13", name: "Sam", roles: ["mac": .member, "trash": .member, "draugelis": .member]),
                Person(id: "kim-6", name: "Kim", roles: ["mac": .member, "trash": .member, "draugelis": .owner,
                                                       "draugelis-vault": .owner]),
            ])
    }
}

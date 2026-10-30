import Herder
import SwiftUI

/// The People page: one for the whole fleet. You, with your passkey and your devices; your
/// teammates, each with a fleet-wide role; then who can do what on each machine, as a grid of
/// menus on the Mac and a list of machines under each person on the iPhone. Machines never show
/// as people.
struct PeopleView: View {
    let directory: Directory
    /// The grid, or on narrow screens a list of machines under each person.
    var grid = true
    var now = Date.now
    /// Sets a person's role on one machine, or on every machine for nil; a nil role takes
    /// their access away.
    let setRole: (Directory.Person, Role?, HostId?) -> Void
    let revoke: (Directory.Device) -> Void
    let invite: () -> Void
    /// Opens Claim your machines, while a machine you own is not claimed yet.
    var claim: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 22) {
            section("You") {
                personRow(directory.me, mine: true)
                ForEach(directory.devices) { device in
                    RowDivider()
                    deviceRow(device)
                }
                if let claim {
                    RowDivider()
                    AddRow(title: "Claim your machines…", action: claim)
                        .accessibilityIdentifier("people-claim")
                }
            }
            section("Teammates") {
                ForEach(directory.teammates) { person in
                    personRow(person, mine: false)
                    RowDivider()
                }
                AddRow(title: "Invite a teammate…", action: invite)
                    .accessibilityIdentifier("people-invite")
            }
            if grid {
                VStack(alignment: .leading, spacing: 8) {
                    SectionHeading(title: "Who can do what", tint: Theme.tertiary)
                    AccessGrid(directory: directory, setRole: setRole)
                    Text(Self.legend).font(Theme.body).foregroundStyle(Theme.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.top, 2)
                }
            } else {
                ForEach(directory.everyone) { person in
                    section(person.name) { AccessList(directory: directory, person: person, setRole: setRole) }
                }
                Text(Self.legend.replacingOccurrences(of: "Each cell", with: "Each machine's role")
                    .replacingOccurrences(of: "an owner cell", with: "an owner"))
                    .font(.footnote).foregroundStyle(Theme.tertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    static let legend = "Each cell is a menu. Owner gets everything, including terminals and secrets. Member drives "
        + "sessions but never gets a terminal. — means no access. Changing an owner cell asks for Face ID."

    private func section<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: title, tint: Theme.tertiary)
            SettingsGroup { content() }
        }
    }

    private func personRow(_ person: Directory.Person, mine: Bool) -> some View {
        HStack(spacing: 12) {
            Avatar(name: person.name, id: person.id, size: 30)
            VStack(alignment: .leading, spacing: 2) {
                // The passkey's pill follows the name, or takes the line under it on a narrow screen.
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 8) { name(person, mine: mine) }
                    VStack(alignment: .leading, spacing: 4) { name(person, mine: mine) }
                }
                Text(directory.scope(person)).font(.caption).foregroundStyle(Theme.secondary).lineLimit(2)
            }
            Spacer(minLength: 8)
            let role = directory.fleetRole(person)
            if mine {
                RoleBox(title: role.title)
            } else {
                Menu {
                    Button("Owner of every machine") { setRole(person, .owner, nil) }
                    Button("Member of every machine") { setRole(person, .member, nil) }
                    Divider()
                    Button("Remove from the fleet", role: .destructive) { setRole(person, nil, nil) }
                } label: {
                    RoleBox(title: role.title, menu: true)
                }
                .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden).fixedSize()
                .accessibilityLabel("Role of \(person.name)")
            }
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 11)
        .accessibilityIdentifier("person-\(person.id)")
    }

    @ViewBuilder private func name(_ person: Directory.Person, mine: Bool) -> some View {
        Text(person.name).font(Theme.body.weight(.medium)).foregroundStyle(Theme.text)
        if mine, case .stored(let keychain) = directory.passkey {
            Pill(text: (["● passkey"] + [keychain].compactMap(\.self)).joined(separator: " · "), tint: Theme.switchOn)
                .fixedSize()
        }
    }

    private func deviceRow(_ device: Directory.Device) -> some View {
        HStack(spacing: 12) {
            Image(systemName: device.symbol).font(.body).foregroundStyle(Theme.secondary).frame(width: 30)
            VStack(alignment: .leading, spacing: 2) {
                Text(device.title).font(Theme.body.weight(.medium)).foregroundStyle(Theme.text)
                Group {
                    if device.kind == .terminal {
                        Text("Signed in with \(Text("herder login").font(Theme.monoSmall)) · \(device.seen(now: now))")
                    } else {
                        Text("Signed in · \(device.seen(now: now))")
                    }
                }
                .font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
            }
            Spacer(minLength: 8)
            if device.current {
                Pill(text: "this device", tint: Theme.secondary)
            } else {
                Menu {
                    Button("Sign out this device", role: .destructive) { revoke(device) }
                } label: {
                    Image(systemName: "ellipsis").font(.subheadline).foregroundStyle(Theme.tertiary)
                        .frame(width: 28, height: 24).contentShape(.rect)
                }
                .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden).fixedSize()
                .accessibilityLabel("\(device.title) options")
            }
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 11)
    }
}

private extension Directory.Device {
    var symbol: String {
        switch kind {
        case .mac: "laptopcomputer"
        case .phone: "iphone"
        case .terminal: "terminal"
        }
    }
}

/// A role in its box: the person row's, and on a teammate a menu ("Member ⌄").
struct RoleBox: View {
    let title: String
    var menu = false

    var body: some View {
        HStack(spacing: 4) {
            Text(title)
            if menu { Image(systemName: "chevron.down").font(.caption2.weight(.semibold)) }
        }
        .font(.caption)
        .foregroundStyle(Theme.text)
        .padding(.horizontal, 10)
        .padding(.vertical, 4)
        .background(Theme.cardInSheet, in: .rect(cornerRadius: 8))
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(Theme.stroke))
        .contentShape(.rect)
    }
}

/// A capsule of quiet text: "this device", "● passkey · iCloud Keychain", "✓ ready".
struct Pill: View {
    let text: String
    var tint = Theme.secondary

    var body: some View {
        Text(text).font(.caption2.weight(.medium)).foregroundStyle(tint).lineLimit(1)
            .padding(.horizontal, 8).padding(.vertical, 2)
            .background(Theme.cardInSheet, in: .capsule)
    }
}

/// A role as a cell of the grid says it: Owner green, Member blue, no access a dash.
struct AccessLabel: View {
    let role: Role?

    var body: some View {
        Text(role == .owner ? "Owner" : role == .member ? "Member" : "—")
            .font(Theme.body)
            .foregroundStyle(role == .owner ? Theme.switchOn : role == .member ? Theme.action : Theme.tertiary)
    }
}

/// The menu a cell opens: owner, member or no access on one machine.
private struct AccessMenu<Label: View>: View {
    let person: Directory.Person
    let machine: Directory.Column
    let setRole: (Directory.Person, Role?, HostId?) -> Void
    @ViewBuilder var label: Label

    var body: some View {
        Menu {
            Button("Owner") { setRole(person, .owner, machine.id) }
            Button("Member") { setRole(person, .member, machine.id) }
            Button("No access") { setRole(person, nil, machine.id) }
        } label: {
            label
        }
        .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden)
        .accessibilityLabel("\(person.name) on \(machine.name)")
    }
}

/// People × machines: each cell a menu of the person's role there.
struct AccessGrid: View {
    let directory: Directory
    let setRole: (Directory.Person, Role?, HostId?) -> Void

    var body: some View {
        Grid(horizontalSpacing: 0, verticalSpacing: 0) {
            GridRow {
                Text("Person").padding(.leading, Theme.cellPadding).frame(maxWidth: .infinity, alignment: .leading)
                ForEach(directory.columns) { Text($0.name).lineLimit(1).frame(maxWidth: .infinity) }
            }
            .font(.caption)
            .foregroundStyle(Theme.tertiary)
            .frame(minHeight: 34)
            ForEach(directory.everyone) { person in
                Rectangle().fill(Theme.stroke).frame(height: 1).gridCellUnsizedAxes(.horizontal)
                GridRow {
                    Text(person.name).font(Theme.body).foregroundStyle(Theme.text)
                        .padding(.leading, Theme.cellPadding)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    ForEach(directory.columns) { machine in
                        AccessMenu(person: person, machine: machine, setRole: setRole) {
                            AccessLabel(role: person.roles[machine.id])
                                .frame(maxWidth: .infinity, minHeight: 36)
                                .contentShape(.rect)
                        }
                    }
                }
                .frame(minHeight: 36)
            }
        }
        .background(.card, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
    }
}

/// One person's machines on the iPhone, the grid's row turned on its side.
struct AccessList: View {
    let directory: Directory
    let person: Directory.Person
    let setRole: (Directory.Person, Role?, HostId?) -> Void

    var body: some View {
        ForEach(Array(directory.columns.enumerated()), id: \.element.id) { index, machine in
            if index > 0 { RowDivider() }
            AccessMenu(person: person, machine: machine, setRole: setRole) {
                HStack {
                    Text(machine.name).font(Theme.body).foregroundStyle(Theme.text)
                    Spacer()
                    AccessLabel(role: person.roles[machine.id])
                    Image(systemName: "chevron.up.chevron.down").font(.caption2).foregroundStyle(Theme.tertiary)
                }
                .padding(.horizontal, Theme.cellPadding)
                .frame(minHeight: Theme.rowHeight)
                .contentShape(.rect)
            }
        }
    }
}

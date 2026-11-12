import Herder
import SwiftUI

/// The People section of Setup: one row per person, you first, each with the line that says
/// where they can do what and a menu of their role across your machines. A row opens the
/// person's sheet (`PersonSheet`), or You for you. Machines never show as people.
struct PeopleView: View {
    let directory: Directory
    /// Sets a person's role on one machine, or on every machine for nil; a nil role takes
    /// their access away.
    let setRole: (Directory.Person, Role?, HostId?) -> Void
    /// Shows a person: their sheet, or You for you.
    let show: (Directory.Person) -> Void
    let invite: () -> Void

    static let legend = "Owners can do everything, terminals included. Members drive sessions and never get a terminal."

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: "People", count: directory.everyone.count)
            SettingsGroup {
                ForEach(directory.everyone) { person in
                    row(person, mine: person.id == directory.me.id)
                    RowDivider()
                }
                AddRow(title: "Invite a teammate…", action: invite)
                    .accessibilityIdentifier("people-invite")
            }
            Text(Self.legend).font(.footnote).foregroundStyle(Theme.tertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func row(_ person: Directory.Person, mine: Bool) -> some View {
        HStack(spacing: 12) {
            Button { show(person) } label: {
                HStack(spacing: 12) {
                    Avatar(name: person.name, id: person.id, size: 30)
                    VStack(alignment: .leading, spacing: 2) {
                        HStack(spacing: 8) {
                            Text(person.name).font(Theme.body.weight(.medium)).foregroundStyle(Theme.text).lineLimit(1)
                            if mine { Pill(text: "You").fixedSize() }
                        }
                        Text(directory.scope(person)).font(.caption).foregroundStyle(Theme.secondary).lineLimit(2)
                    }
                    Spacer(minLength: 8)
                }
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("\(person.name), \(directory.scope(person))")
            let role = directory.fleetRole(person)
            if mine {
                RoleBox(title: role.title)
            } else {
                Menu {
                    Button("Owner on all my machines") { setRole(person, .owner, nil) }
                    Button("Member on all my machines") { setRole(person, .member, nil) }
                    Button("Different per machine…") { show(person) }
                } label: {
                    RoleBox(title: role.title, menu: true)
                }
                .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden).fixedSize()
                .accessibilityLabel("Role of \(person.name)")
            }
            Button { show(person) } label: {
                Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
                    .frame(width: 16, height: 28).contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityHidden(true)
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 11)
        .accessibilityIdentifier("person-\(person.id)")
    }
}

/// A teammate's sheet: their role on all your machines or on each one, their devices, each
/// signed out on its own, and Remove, which takes them off every machine you own.
struct PersonSheet: View {
    let directory: Directory
    let person: Directory.Person
    /// Why the last change failed.
    var error: String?
    /// Sets their role on one machine, or on every machine for nil; nil takes their access away.
    let setRole: (Role?, HostId?) -> Void
    let signOut: (Directory.Device) -> Void
    /// Folds them into you, when they are really you.
    let itsMe: () -> Void
    @State private var perMachine = false
    @State private var removing = false
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let devices = directory.devices(of: person)
        DetailSheet(title: person.name, destructive: "Remove \(person.name)…", onDestructive: { removing = true }) {
            Text(Self.summary(directory, person))
                .font(.subheadline).foregroundStyle(Theme.secondary)
            if let error {
                Text(error).font(.footnote).foregroundStyle(Theme.failure).accessibilityIdentifier("people-error")
            }
            Picker("Role", selection: role) {
                Text("Owner on all").tag(Directory.FleetRole.owner)
                Text("Member on all").tag(Directory.FleetRole.member)
                Text("Per machine").tag(Directory.FleetRole.custom)
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            if role.wrappedValue == .custom {
                SettingsList(title: "Per machine") {
                    ForEach(Array(directory.columns.enumerated()), id: \.element.id) { index, machine in
                        if index > 0 { RowDivider() }
                        machineRow(machine)
                    }
                }
            }
            if !devices.isEmpty {
                SettingsList(title: "Devices") {
                    ForEach(Array(devices.enumerated()), id: \.element.id) { index, device in
                        if index > 0 { RowDivider() }
                        deviceRow(device)
                    }
                }
            }
            SettingsList(title: "Same person?",
                         caption: "If \(person.name) is you from an earlier setup, fold them into you. "
                             + "Approve with the passkey \(person.name) was made with.") {
                Button("This is me…", action: itsMe)
                    .buttonStyle(.plain)
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Theme.action)
                    .padding(.horizontal, Theme.cellPadding)
                    .frame(maxWidth: .infinity, minHeight: Theme.rowHeight, alignment: .leading)
                    .contentShape(.rect)
            }
        }
        .confirmationDialog("Remove \(person.name) from all your machines?", isPresented: $removing,
                            titleVisibility: .visible) {
            Button("Remove \(person.name)", role: .destructive) {
                setRole(nil, nil)
                dismiss()
            }
        } message: {
            Text("They can no longer use any machine you own, and their devices are signed out of them.")
        }
    }

    /// "Different per machine · 2 devices".
    nonisolated static func summary(_ directory: Directory, _ person: Directory.Person) -> String {
        let role = switch directory.fleetRole(person) {
        case .owner: "Owner on all your machines"
        case .member: "Member on all your machines"
        case .custom: "Different per machine"
        }
        let devices = directory.devices(of: person).count
        return devices == 0 ? role : "\(role) · \(devices == 1 ? "1 device" : "\(devices) devices")"
    }

    /// Owner or Member on every machine sets it at once; Per machine shows each machine's.
    private var role: Binding<Directory.FleetRole> {
        Binding(get: { perMachine ? .custom : directory.fleetRole(person) }, set: { role in
            perMachine = role == .custom
            switch role {
            case .owner: setRole(.owner, nil)
            case .member: setRole(.member, nil)
            case .custom: break
            }
        })
    }

    private func machineRow(_ machine: Directory.Column) -> some View {
        HStack {
            Text(machine.name).font(Theme.body).foregroundStyle(Theme.text).lineLimit(1)
            Spacer()
            Menu {
                Button("Owner") { setRole(.owner, machine.id) }
                Button("Member") { setRole(.member, machine.id) }
                Button("No access") { setRole(nil, machine.id) }
            } label: {
                HStack(spacing: 4) {
                    AccessLabel(role: person.roles[machine.id])
                    Image(systemName: "chevron.down").font(.caption2.weight(.semibold)).foregroundStyle(Theme.tertiary)
                }
                .contentShape(.rect)
            }
            .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden).fixedSize()
            .accessibilityLabel("\(person.name) on \(machine.name)")
        }
        .padding(.horizontal, Theme.cellPadding)
        .frame(minHeight: Theme.rowHeight)
    }

    private func deviceRow(_ device: Directory.Device) -> some View {
        HStack(spacing: 12) {
            Image(systemName: device.symbol).font(.body).foregroundStyle(Theme.secondary).frame(width: 30)
            VStack(alignment: .leading, spacing: 2) {
                Text(device.title).font(Theme.body.weight(.medium)).foregroundStyle(Theme.text)
                Group {
                    if device.kind == .terminal {
                        Text("Signed in with \(Text("herder login").font(Theme.monoSmall))")
                    } else {
                        Text(device.reach)
                    }
                }
                .font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
            }
            Spacer(minLength: 8)
            SettingsButton(title: "Sign Out…", style: .quiet) { signOut(device) }
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 9)
    }
}

/// You: your role and passkey, your devices, and the ways to add one and to bring machines
/// that know you by an earlier setup over to your passkey.
struct YouView: View {
    let directory: Directory
    var now = Date.now
    let revoke: (Directory.Device) -> Void
    /// Opens Add a Device, while this device has machines to share.
    var addDevice: (() -> Void)?
    /// Opens Claim your machines, while a machine you own is not claimed yet.
    var claim: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: "You", tint: Theme.tertiary)
            SettingsGroup {
                personRow
                ForEach(directory.devices) { device in
                    RowDivider()
                    deviceRow(device)
                }
                if let addDevice {
                    RowDivider()
                    AddRow(title: "Add a device…", action: addDevice)
                        .accessibilityIdentifier("people-add-device")
                }
                if let claim {
                    RowDivider()
                    AddRow(title: "Use your passkey on every machine…", action: claim)
                        .accessibilityIdentifier("people-claim")
                }
            }
        }
    }

    private var personRow: some View {
        let me = directory.me
        return HStack(spacing: 12) {
            Avatar(name: me.name, id: me.id, size: 30)
            VStack(alignment: .leading, spacing: 2) {
                // The passkey's pill follows the name, or takes the line under it on a narrow screen.
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 8) { name }
                    VStack(alignment: .leading, spacing: 4) { name }
                }
                Text(directory.scope(me)).font(.caption).foregroundStyle(Theme.secondary).lineLimit(2)
            }
            Spacer(minLength: 8)
            RoleBox(title: directory.fleetRole(me).title)
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 11)
        .accessibilityIdentifier("person-\(me.id)")
    }

    @ViewBuilder private var name: some View {
        Text(directory.me.name).font(Theme.body.weight(.medium)).foregroundStyle(Theme.text)
        if case .stored(let keychain) = directory.passkey {
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
                        Text("\(device.reach) · \(device.seen(now: now))")
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

extension Directory.Device {
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

/// A role on one machine: Owner green, Member blue, no access a dash.
struct AccessLabel: View {
    let role: Role?

    var body: some View {
        Text(role == .owner ? "Owner" : role == .member ? "Member" : "—")
            .font(Theme.body)
            .foregroundStyle(role == .owner ? Theme.switchOn : role == .member ? Theme.action : Theme.tertiary)
    }
}

import Herder
import SwiftUI
import UniformTypeIdentifiers

/// What the Remote access page does, as closures, so it is drawn in tests without a fleet.
struct RemoteActions {
    /// Pins a machine's address, disables it or puts it back in the race.
    var setRoute: @MainActor (HostId, String, RouteChoice) throws -> Void = { _, _, _ in }
    /// Sets a machine's addresses in the order they are tried.
    var setAddresses: @MainActor (HostId, [String]) throws -> Void = { _, _ in }
    /// Imports a WireGuard config's text under a name.
    var importConfig: @MainActor (String, String) throws -> Void = { _, _ in }
    var removeTunnel: @MainActor (String) throws -> Void = { _ in }
    /// Uses herder's WireGuard on the hub on this device, or stops.
    var useHubWireGuard: @MainActor (Bool) async throws -> Void = { _ in }
    /// Sets what the hub sets up (owners).
    var setSettings: @MainActor (RemoteAccessSettings) async throws -> Void = { _ in }
    /// Keeps the Cloudflare API token on the hub (owners).
    var putToken: @MainActor (String) async throws -> Void = { _ in }
}

/// Your machines: each one's route in use and round trip in a line; a tap opens its routes in
/// the order they are tried, each to pin, disable or move.
struct MachineRoutesCard: View {
    let machines: [MachineRoutes]
    let actions: RemoteActions
    @State private var open: HostId?
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: "Your machines")
            SettingsGroup {
                ForEach(Array(machines.enumerated()), id: \.element.id) { index, machine in
                    if index > 0 { RowDivider() }
                    Button {
                        withAnimation(.easeInOut(duration: 0.2)) { open = open == machine.hostId ? nil : machine.hostId }
                    } label: {
                        HStack(spacing: 10) {
                            RouteMark(state: machine.route == nil ? .notSetUp : .working)
                            Text(machine.name).font(Theme.body).foregroundStyle(Theme.text).lineLimit(1)
                            Spacer(minLength: 8)
                            Text(machine.line).font(.subheadline.monospacedDigit()).foregroundStyle(Theme.secondary)
                                .lineLimit(1)
                            Image(systemName: open == machine.hostId ? "chevron.up" : "chevron.down")
                                .font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
                        }
                        .padding(.horizontal, Theme.cellPadding)
                        .frame(maxWidth: .infinity, minHeight: Theme.rowHeight, alignment: .leading)
                        .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("remote-access-machine")
                    if open == machine.hostId {
                        routes(machine)
                    }
                }
                if let error {
                    RowDivider()
                    Text(error).font(.footnote).foregroundStyle(Theme.failure)
                        .padding(Theme.cellPadding)
                }
            }
        }
        .accessibilityIdentifier("remote-access-machines")
    }

    /// The machine's routes, first tried first, each with its choice.
    private func routes(_ machine: MachineRoutes) -> some View {
        let addresses = machine.entries.map(\.address)
        return VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(machine.entries.enumerated()), id: \.element.id) { index, entry in
                HStack(spacing: 8) {
                    Text("\(index + 1)").font(.caption.monospacedDigit()).foregroundStyle(Theme.tertiary)
                        .frame(width: 16, alignment: .trailing)
                    VStack(alignment: .leading, spacing: 2) {
                        HStack(spacing: 6) {
                            Text(entry.name).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text)
                            if entry.inUse {
                                Text("In use").font(.caption.weight(.medium)).foregroundStyle(Theme.success)
                            }
                        }
                        Text(entry.address).font(.caption.monospaced()).foregroundStyle(Theme.tertiary)
                            .lineLimit(1).truncationMode(.middle)
                    }
                    .opacity(entry.choice == .disabled ? 0.5 : 1)
                    Spacer(minLength: 8)
                    Menu {
                        Picker("Use", selection: Binding(get: { entry.choice }, set: { choice in
                            perform { try actions.setRoute(machine.hostId, entry.address, choice) }
                        })) {
                            Text("In the race").tag(RouteChoice.auto)
                            Text("Only this route").tag(RouteChoice.pinned)
                            Text("Never").tag(RouteChoice.disabled)
                        }
                        .pickerStyle(.inline)
                        Divider()
                        Button("Try Earlier", systemImage: "arrow.up") {
                            perform { try actions.setAddresses(machine.hostId, Self.moving(addresses, from: index, by: -1)) }
                        }
                        .disabled(index == 0)
                        Button("Try Later", systemImage: "arrow.down") {
                            perform { try actions.setAddresses(machine.hostId, Self.moving(addresses, from: index, by: 1)) }
                        }
                        .disabled(index == addresses.count - 1)
                    } label: {
                        HStack(spacing: 4) {
                            Text(MachineRoutes.label(entry.choice) ?? "Auto")
                            Image(systemName: "chevron.up.chevron.down").font(.caption2)
                        }
                        .font(.caption.weight(.medium)).foregroundStyle(Theme.secondary)
                        .contentShape(.rect)
                    }
                    .menuStyle(.button)
                    .buttonStyle(.plain)
                    .menuIndicator(.hidden)
                    .fixedSize()
                    .accessibilityIdentifier("remote-access-route-choice")
                }
            }
            Text("Routes are tried in this order when a connection starts, after a drop or a network change; never moved mid-use.")
                .font(.caption).foregroundStyle(Theme.tertiary).fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.bottom, Theme.cellPadding)
    }

    /// `addresses` with the one at `index` moved `by` places, kept in bounds.
    static func moving(_ addresses: [String], from index: Int, by offset: Int) -> [String] {
        var addresses = addresses
        let to = min(max(index + offset, 0), addresses.count - 1)
        addresses.swapAt(index, to)
        return addresses
    }

    private func perform(_ action: () throws -> Void) {
        do {
            try action()
            error = nil
        } catch {
            self.error = describe(error)
        }
    }
}

/// Your VPN, through herder's own WireGuard client: each imported config, and the form that
/// imports one from its text or file.
struct OwnVpnFacts: View {
    let options: RemoteOptions
    let actions: RemoteActions
    @State private var importing = false
    @State private var choosingFile = false
    @State private var scanning = false
    @State private var name = ""
    @State private var config = ""
    @State private var error: String?

    var body: some View {
        ForEach(options.imported) { tunnel in
            RowDivider()
            HStack(spacing: 10) {
                RouteMark(state: tunnel.up ? .working : .unseen)
                VStack(alignment: .leading, spacing: 2) {
                    Text(tunnel.name).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text)
                    Text(([tunnel.endpoint] + tunnel.allowedIps).joined(separator: " · "))
                        .font(.caption.monospaced()).foregroundStyle(Theme.tertiary).lineLimit(1).truncationMode(.middle)
                }
                Spacer(minLength: 8)
                SettingsButton(title: "Remove", style: .quiet) { perform { try actions.removeTunnel(tunnel.id) } }
                    .fixedSize()
                    .accessibilityIdentifier("remote-access-remove-vpn")
            }
            .padding(.horizontal, Theme.cellPadding)
            .padding(.vertical, 8)
        }
        RowDivider()
        if importing {
            VStack(alignment: .leading, spacing: 10) {
                InputBox(placeholder: "Name, e.g. Home router", text: $name)
                InputBox(placeholder: "[Interface]\nPrivateKey = …\n\n[Peer]\nEndpoint = …", text: $config, mono: true,
                         lines: 4...10)
                Text("The config’s private key stays on this device. herder carries only its own connections through it, so another VPN here stays on.")
                    .font(.caption).foregroundStyle(Theme.tertiary).fixedSize(horizontal: false, vertical: true)
                if let error {
                    Text(error).font(.footnote).foregroundStyle(Theme.failure).fixedSize(horizontal: false, vertical: true)
                }
                HStack(spacing: 10) {
                    SettingsButton(title: "Choose File…") { choosingFile = true }.fixedSize()
                    #if os(iOS)
                    SettingsButton(title: "Scan QR Code…") { scanning = true }
                        .fixedSize()
                        .accessibilityIdentifier("remote-access-scan-vpn")
                    #endif
                    Spacer(minLength: 0)
                    SettingsButton(title: "Cancel", style: .quiet) { close() }.fixedSize()
                    SettingsButton(title: "Import", style: .primary) {
                        perform {
                            try actions.importConfig(name.isEmpty ? "WireGuard" : name, config)
                            close()
                        }
                    }
                    .fixedSize()
                    .disabled(config.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .accessibilityIdentifier("remote-access-import-vpn-save")
                }
            }
            .padding(Theme.cellPadding)
            .fileImporter(isPresented: $choosingFile, allowedContentTypes: [.item]) { result in
                if case .success(let url) = result { read(url) }
            }
            #if os(iOS)
            .fullScreenCover(isPresented: $scanning) {
                QRScanner(
                    hint: "Point the camera at the QR code your router shows for this device’s WireGuard config.",
                    rejected: "That QR code holds no WireGuard config.",
                    otherwise: "Choose the .conf file instead.",
                    close: { scanning = false }
                ) { payload in
                    guard isWireGuardConfig(payload) else { return false }
                    config = payload
                    error = nil
                    return true
                }
            }
            #endif
        } else {
            HStack {
                SettingsButton(title: options.imported.isEmpty ? "Import a WireGuard Config…" : "Import Another…",
                               style: options.imported.isEmpty ? .primary : .plain) { importing = true }
                    .fixedSize()
                    .accessibilityIdentifier("remote-access-import-vpn")
                Spacer(minLength: 0)
            }
            .padding(.horizontal, Theme.cellPadding)
            .padding(.vertical, 8)
        }
    }

    /// A `.conf` file's text into the form, named after the file.
    private func read(_ url: URL) {
        do {
            config = try SecretInput(contentsOf: url).text
            if name.isEmpty { name = url.deletingPathExtension().lastPathComponent }
            error = nil
        } catch {
            self.error = describe(error)
        }
    }

    private func close() {
        importing = false
        name = ""
        config = ""
        error = nil
    }

    private func perform(_ action: () throws -> Void) {
        do {
            try action()
            error = nil
        } catch {
            self.error = describe(error)
        }
    }
}

/// herder's WireGuard on the hub: to an owner, its endpoint and port and one button that turns
/// it on; to every device, whether it uses it.
struct HubWireGuardFacts: View {
    let options: RemoteOptions
    let actions: RemoteActions
    @State private var endpoint = ""
    @State private var port = ""
    @State private var editing = false
    @State private var error: String?

    var body: some View {
        if options.hubWireGuard != nil {
            RowDivider()
            HStack {
                Text("Use on this device").font(.subheadline).foregroundStyle(Theme.text)
                Spacer(minLength: 8)
                Toggle("Use on this device", isOn: Binding(get: { options.enrolled }, set: { on in
                    run { try await actions.useHubWireGuard(on) }
                }))
                .labelsHidden()
                .toggleStyle(.settings)
                .accessibilityIdentifier("remote-access-hub-wireguard-use")
            }
            .padding(.horizontal, Theme.cellPadding)
            .frame(minHeight: Theme.rowHeight)
        }
        if options.owner, options.answered {
            RowDivider()
            if editing || options.hubWireGuard == nil {
                VStack(alignment: .leading, spacing: 10) {
                    InputBox(placeholder: "Your router’s public name and port, e.g. home.example.net:\(RemoteOptions.defaultPort)",
                             text: $endpoint, mono: true)
                    InputBox(placeholder: "UDP port on the hub, \(RemoteOptions.defaultPort)", text: $port, mono: true)
                    Text("Forward that UDP port on your router to the hub. Only devices you paired get an answer; to anyone else the port looks closed.")
                        .font(.caption).foregroundStyle(Theme.tertiary).fixedSize(horizontal: false, vertical: true)
                    buttons
                }
                .padding(Theme.cellPadding)
                .onAppear {
                    endpoint = options.wireGuardEndpoint
                    port = options.wireGuardPort.map(String.init) ?? ""
                }
            } else {
                buttons.padding(.horizontal, Theme.cellPadding).padding(.vertical, 8)
            }
        }
        if let error {
            Text(error).font(.footnote).foregroundStyle(Theme.failure)
                .padding(.horizontal, Theme.cellPadding).padding(.bottom, 8)
        }
    }

    @ViewBuilder private var buttons: some View {
        HStack(spacing: 10) {
            Spacer(minLength: 0)
            if options.hubWireGuard != nil {
                SettingsButton(title: "Turn Off", style: .destructive) {
                    run { try await actions.setSettings(options.settings(wireGuard: nil)) }
                }
                .fixedSize()
                if !editing {
                    SettingsButton(title: "Change…") { editing = true }.fixedSize()
                }
            }
            if editing || options.hubWireGuard == nil {
                SettingsButton(title: options.hubWireGuard == nil ? "Turn On" : "Save", style: .primary) {
                    let settings = WireGuardSettings(port: UInt16(port) ?? RemoteOptions.defaultPort,
                                                     endpoint: endpoint.trimmingCharacters(in: .whitespaces))
                    run {
                        try await actions.setSettings(options.settings(wireGuard: settings))
                        editing = false
                    }
                }
                .fixedSize()
                .disabled(!endpoint.contains(":"))
                .accessibilityIdentifier("remote-access-hub-wireguard-on")
            }
        }
    }

    private func run(_ action: @escaping () async throws -> Void) {
        Task {
            do {
                try await action()
                error = nil
            } catch {
                self.error = describe(error)
            }
        }
    }
}

/// Cloudflare, for owners: the account and API token once, then the managed tunnel's hostname
/// and the relay, each on or off; to everyone, each one's state.
struct CloudflareFacts: View {
    let options: RemoteOptions
    let actions: RemoteActions
    @State private var account = ""
    @State private var token = ""
    @State private var hostname = ""
    @State private var relay = false
    @State private var editing = false
    @State private var error: String?

    var body: some View {
        if let tunnel = options.tunnel {
            RowDivider()
            status("Tunnel", tunnel)
        }
        if let relay = options.relay {
            RowDivider()
            status("Relay", relay)
        }
        if options.owner, options.answered {
            RowDivider()
            if editing {
                form
            } else {
                HStack(spacing: 10) {
                    Spacer(minLength: 0)
                    if options.cloudflare != nil {
                        SettingsButton(title: "Remove", style: .destructive) {
                            run { try await actions.setSettings(options.settings(cloudflare: nil)) }
                        }
                        .fixedSize()
                    }
                    SettingsButton(title: options.cloudflare == nil ? "Set Up…" : "Change…",
                                   style: options.cloudflare == nil ? .primary : .plain) {
                        account = options.cloudflareAccount
                        hostname = options.tunnelHostname
                        relay = options.relayOn
                        token = ""
                        editing = true
                    }
                    .fixedSize()
                    .accessibilityIdentifier("remote-access-cloudflare-set-up")
                }
                .padding(.horizontal, Theme.cellPadding)
                .padding(.vertical, 8)
            }
        }
        if let error {
            Text(error).font(.footnote).foregroundStyle(Theme.failure)
                .padding(.horizontal, Theme.cellPadding).padding(.bottom, 8)
        }
    }

    private var form: some View {
        VStack(alignment: .leading, spacing: 10) {
            InputBox(placeholder: "Cloudflare account ID", text: $account, mono: true)
            InputBox(placeholder: options.cloudflareToken == nil ? "API token" : "API token (kept; type to replace)",
                     text: $token, mono: true, secure: true)
            Text("The token stays on the hub, as a secret no device or machine reads back. It needs Workers Scripts, Cloudflare Tunnel and DNS edit.")
                .font(.caption).foregroundStyle(Theme.tertiary).fixedSize(horizontal: false, vertical: true)
            InputBox(placeholder: "Tunnel hostname on your zone, e.g. hub.example.com (optional)", text: $hostname,
                     mono: true)
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Relay").font(.subheadline).foregroundStyle(Theme.text)
                    Text("For no public IP and no domain: on workers.dev").font(.caption).foregroundStyle(Theme.tertiary)
                }
                Spacer(minLength: 8)
                Toggle("Relay", isOn: $relay).labelsHidden().toggleStyle(.settings)
            }
            HStack(spacing: 10) {
                Spacer(minLength: 0)
                SettingsButton(title: "Cancel", style: .quiet) { editing = false }.fixedSize()
                SettingsButton(title: "Set Up", style: .primary) { save() }
                    .fixedSize()
                    .disabled(!complete)
                    .accessibilityIdentifier("remote-access-cloudflare-save")
            }
        }
        .padding(Theme.cellPadding)
    }

    /// An account, a token (typed, or kept), and a tunnel or the relay.
    private var complete: Bool {
        !account.trimmingCharacters(in: .whitespaces).isEmpty
            && (!token.isEmpty || options.cloudflareToken != nil)
            && (relay || !hostname.trimmingCharacters(in: .whitespaces).isEmpty)
    }

    private func save() {
        let typed = token
        token = ""
        let host = hostname.trimmingCharacters(in: .whitespaces)
        let settings = CloudflareSettings(accountId: account.trimmingCharacters(in: .whitespaces),
                                          apiToken: options.cloudflareToken ?? RemoteOptions.tokenRef, relay: relay,
                                          tunnelHostname: host.isEmpty ? nil : host)
        run {
            if !typed.isEmpty { try await actions.putToken(typed) }
            try await actions.setSettings(options.settings(cloudflare: settings))
            editing = false
        }
    }

    private func status(_ label: String, _ status: RemoteOptions.Status) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Text(label).font(.subheadline).foregroundStyle(Theme.secondary)
            Spacer(minLength: 8)
            RouteMark(state: status.state)
            Text(status.state == .wontWork ? status.error ?? "Failed" : URL(string: status.url)?.host ?? status.url)
                .font(.subheadline).foregroundStyle(Theme.text).multilineTextAlignment(.trailing)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 10)
    }

    private func run(_ action: @escaping () async throws -> Void) {
        Task {
            do {
                try await action()
                error = nil
            } catch {
                self.error = describe(error)
            }
        }
    }
}

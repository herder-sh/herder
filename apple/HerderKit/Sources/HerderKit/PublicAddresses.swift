import Herder
import SwiftUI

/// Where other devices reach a machine from outside its networks: the public addresses its
/// owner sets here once. They are the machine's setting, so it names them in every hello, invite
/// and claim link, and every device of every person on it dials them with nothing to type.
struct PublicAddressesSection: View {
    let fleet: Fleet
    let machine: Machine
    /// The addresses as the machine said last; `nil` until it did.
    @State var addresses: [String]?
    @State private var newAddress = ""
    @State private var error: String?

    static let caption = "A port forward or TCP proxy to port 7447, or a VPN address, as host:port; or a tunnel that unwraps HTTPS, such as a Cloudflare Tunnel, as its https:// address. Every device picks these up when it next connects."

    var body: some View {
        SettingsList(title: "Public addresses", caption: Self.caption) {
            if let addresses {
                if addresses.isEmpty {
                    SettingRow(label: "None yet", hint: "Reached only on its own networks, or through a hub") { EmptyView() }
                }
                ForEach(Array(addresses.enumerated()), id: \.element) { index, address in
                    if index > 0 { RowDivider() }
                    SettingRow(label: address) {
                        Button("Remove", systemImage: "minus.circle") {
                            Task { await save(addresses.filter { $0 != address }) }
                        }
                        .labelStyle(.iconOnly)
                        .buttonStyle(.borderless)
                        .accessibilityIdentifier("public-address-remove")
                    }
                }
                RowDivider()
                HStack(spacing: 10) {
                    InputBox(placeholder: "e.g. home.example.net:7447 or https://hub.example.com",
                             text: $newAddress, mono: true)
                        .accessibilityIdentifier("public-address-new")
                    ActionButton(title: "Add", style: .secondary) {
                        let typed = newAddress.trimmingCharacters(in: .whitespaces)
                        guard !typed.isEmpty else { return }
                        if await save(addresses + [typed]) { newAddress = "" }
                    }
                    .frame(width: 120)
                    .disabled(newAddress.trimmingCharacters(in: .whitespaces).isEmpty)
                    .accessibilityIdentifier("public-address-add")
                }
                .padding(Theme.cellPadding)
            } else if error == nil {
                SettingRow(label: "Public addresses") { ProgressView().controlSize(.small) }
            }
            if let error {
                RowDivider()
                Text(error).font(.footnote).foregroundStyle(Theme.failure)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, 14).frame(minHeight: 36)
            }
        }
        .task(id: machine.hostId) {
            guard addresses == nil else { return }
            do {
                addresses = try await fleet.publicAddresses(on: machine.hostId)
            } catch {
                self.error = describe(error)
            }
        }
    }

    /// Saves `wanted` on the machine, which normalizes each and refuses what no other device
    /// could reach; whether it took them.
    @discardableResult
    private func save(_ wanted: [String]) async -> Bool {
        do {
            addresses = try await fleet.setPublicAddresses(wanted, on: machine.hostId)
            error = nil
            return true
        } catch {
            self.error = describe(error)
            return false
        }
    }
}

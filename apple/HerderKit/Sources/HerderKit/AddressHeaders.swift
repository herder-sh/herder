import Herder
import SwiftUI

/// The headers a proxy in front of a machine's tunnel wants on the upgrade, such as a
/// Cloudflare Access service token, typed one per line as `Name: value`. They stay on this
/// device: the machine and other devices never get them.
struct AddressHeaders {
    /// `text` as headers: each nonblank line `Name: value`; else what is wrong with a line.
    static func parse(_ text: String) -> Result<[String: String], AddressHeadersError> {
        var headers: [String: String] = [:]
        for line in text.split(whereSeparator: \.isNewline) {
            let line = line.trimmingCharacters(in: .whitespaces)
            if line.isEmpty { continue }
            guard let colon = line.firstIndex(of: ":") else {
                return .failure(AddressHeadersError(message: "“\(line)” is not Name: value"))
            }
            let name = line[..<colon].trimmingCharacters(in: .whitespaces)
            let value = line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
            guard !name.isEmpty else {
                return .failure(AddressHeadersError(message: "“\(line)” names no header"))
            }
            headers[name] = value
        }
        return .success(headers)
    }

    /// What a tunnel address's row says of its headers: "Sends cf-access-client-id,
    /// cf-access-client-secret", or nothing.
    static func summary(_ names: [String]) -> String? {
        names.isEmpty ? nil : "Sends \(names.sorted().joined(separator: ", "))"
    }

    /// Whether `address` is a tunnel's URL, whose upgrade can carry headers.
    static func takesHeaders(_ address: String) -> Bool {
        address.hasPrefix("wss://") || address.hasPrefix("ws://")
    }
}

struct AddressHeadersError: Error, Equatable {
    let message: String
}

/// Where a sheet sets the headers of one of a machine's tunnel addresses.
struct HeaderTarget: Identifiable {
    let hostId: HostId
    let address: String
    /// The names it sends now; their values are not read back.
    let names: [String]

    var id: String { address }
}

/// Sets the headers one tunnel address sends on its upgrade. The values already set are not
/// shown: typing replaces them all, and an empty box removes them.
struct AddressHeadersSheet: View {
    let fleet: Fleet
    let target: HeaderTarget
    @State private var text = ""
    @State private var error: String?
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        SheetScaffold(title: "Proxy headers", subtitle: target.address, height: 420,
                      hint: "Kept on this device only, never sent to the machine.") {
            Field(label: "Headers", hint: "One per line, as `Name: value`; for Cloudflare Access, `CF-Access-Client-Id` and `CF-Access-Client-Secret` of a service token.") {
                InputBox(placeholder: "CF-Access-Client-Id: …", text: $text, mono: true, lines: 3...6)
                    .accessibilityIdentifier("address-headers")
            }
            if let names = AddressHeaders.summary(target.names) {
                Text("\(names) now. Saving replaces them.").font(.footnote).foregroundStyle(Theme.secondary)
            }
            if let error {
                Text(error).font(.footnote).foregroundStyle(Theme.failure)
            }
        } footer: {
            if !target.names.isEmpty {
                ActionButton(title: "Remove", style: .secondary) { save([:]) }
            }
            ActionButton(title: "Save", style: .primary) {
                switch AddressHeaders.parse(text) {
                case .success(let headers): save(headers)
                case .failure(let failure): error = failure.message
                }
            }
            .accessibilityIdentifier("address-headers-save")
        }
    }

    private func save(_ headers: [String: String]) {
        do {
            try fleet.setAddressHeaders(target.hostId, address: target.address, to: headers)
            dismiss()
        } catch {
            self.error = describe(error)
        }
    }
}

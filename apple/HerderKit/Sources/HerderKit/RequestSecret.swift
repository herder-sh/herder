import Herder
import SwiftUI

/// What an agent's `request_secret` asks: "This session asks you to add the secret
/// GITHUB_SIEM_PAT in coingate on herder-vault-0", with its hint, a warning when it replaces
/// a value, the vault's sets to pick another from, and a secure field for the value.
struct SecretRequestAsk: View {
    let fleet: Fleet
    let hostId: HostId
    let secret: SecretRequest
    @Binding var picked: String?
    @Binding var value: SecretInput
    @State private var vault: Machine?
    @State private var sets: [String] = []
    /// The vault's secrets, to tell whether the picked set has the name already.
    @State private var held: [SecretInfo]?
    @State private var problem: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(Self.question(secret, set: chosen, vault: vault?.name))
                .font(.subheadline)
                .foregroundStyle(Theme.text)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier("request-secret-question")
            if !secret.description.isEmpty {
                Text(secret.description).font(.footnote).foregroundStyle(Theme.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("request-secret-hint")
            }
            if Self.replaces(secret, set: chosen, held: held) {
                Label("\(chosen) already holds \(secret.name): storing replaces its value.", systemImage: "exclamationmark.triangle.fill")
                    .font(.footnote)
                    .foregroundStyle(Theme.accent)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("request-secret-replaces")
            }
            HStack(spacing: 8) {
                Text("Set").font(.footnote).foregroundStyle(Theme.secondary)
                Picker("Set", selection: Binding(get: { chosen }, set: { picked = $0 })) {
                    ForEach(choices, id: \.self) { Text($0).tag($0) }
                }
                .labelsHidden()
                .accessibilityIdentifier("request-secret-set")
                Spacer(minLength: 0)
            }
            valueField
            Text("Goes straight to the vault. The agent only learns that it was stored.")
                .font(.caption).foregroundStyle(Theme.tertiary)
            if let problem {
                Text(problem).font(.footnote).foregroundStyle(Theme.failure)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .task(id: hostId) { await load() }
    }

    private var chosen: String { picked ?? secret.set }

    /// The vault's sets, with the agent's first when the vault lacks it: Store creates it.
    private var choices: [String] {
        sets.contains(secret.set) ? sets : [secret.set] + sets
    }

    /// A secure field for one line, or Paste for a value of several, such as a PEM key, whose
    /// newlines a secure field drops; a pasted value shows only its size.
    private var valueField: some View {
        HStack(spacing: 8) {
            Group {
                if Self.isMultiline(value) {
                    HStack(spacing: 8) {
                        Image(systemName: "doc.on.clipboard").foregroundStyle(Theme.secondary)
                        Text(Self.pasted(value)).font(.subheadline).foregroundStyle(Theme.text)
                        Spacer(minLength: 0)
                        IconButton(symbol: "xmark", help: "Clear the Value") { value = SecretInput() }
                    }
                    .padding(.horizontal, 12)
                    .accessibilityIdentifier("request-secret-pasted")
                } else {
                    SecureField("Value", text: $value.text)
                        .textFieldStyle(.plain)
                        .font(Theme.mono)
                        .foregroundStyle(Theme.text)
                        .autocorrectionDisabled()
                        #if os(iOS)
                        .textInputAutocapitalization(.never)
                        #endif
                        .padding(12)
                        .accessibilityIdentifier("request-secret-value")
                }
            }
            .frame(maxWidth: .infinity, minHeight: 44)
            .background(.card, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
            PasteButton(payloadType: String.self) { strings in
                guard let text = strings.first else { return }
                Task { @MainActor in value = SecretInput(text: text) }
            }
            .labelStyle(.iconOnly)
            .accessibilityIdentifier("request-secret-paste")
        }
    }

    private func load() async {
        do {
            let vault = try await fleet.ownedVault(of: hostId)
            self.vault = vault
            guard case .secrets(let sets, let secrets) = try await fleet.client.send(hostId: vault.hostId, command: .listSecrets)
            else { return }
            self.sets = sets.map(\.name)
            held = secrets
            problem = nil
        } catch {
            problem = describe(error)
        }
    }

    /// "This session asks you to add the secret GITHUB_SIEM_PAT in coingate on vault-0."
    nonisolated static func question(_ secret: SecretRequest, set: String, vault: String?) -> String {
        "This session asks you to add the secret \(secret.name) in \(set) on \(vault ?? "its vault")."
    }

    /// Whether storing in `set` replaces a value: as the vault lists its secrets once loaded,
    /// else as the host saw its granted sets when the agent asked.
    nonisolated static func replaces(_ secret: SecretRequest, set: String, held: [SecretInfo]?) -> Bool {
        guard let held else { return set == secret.set && secret.replaces }
        return held.contains { $0.set == set && $0.name == secret.name }
    }

    /// The description to store: the agent's, else that of the secret it replaces in `held`.
    nonisolated static func description(_ secret: SecretRequest, set: String, held: [SecretInfo]) -> String {
        guard secret.description.isEmpty else { return secret.description }
        return held.first { $0.set == set && $0.name == secret.name }?.description ?? ""
    }

    /// Why `value` cannot be stored, as the vault checks it; `nil` when it can.
    nonisolated static func problem(_ value: SecretInput) -> String? {
        if value.isEmpty { return "Enter or paste the value." }
        if value.text.utf8.count > 64 * 1024 || value.text.contains("\0") {
            return "The value is longer than 64 KiB, or holds a NUL."
        }
        return nil
    }

    /// Whether `value` has lines a secure field would drop.
    nonisolated static func isMultiline(_ value: SecretInput) -> Bool {
        value.text.contains(where: \.isNewline)
    }

    /// "Pasted 1.7 KB, 28 lines": what a pasted value shows of itself.
    nonisolated static func pasted(_ value: SecretInput) -> String {
        let lines = value.text.split(whereSeparator: \.isNewline).count
        return "Pasted \(SecretStoreAsk.size(UInt64(value.text.utf8.count))), \(lines) line\(lines == 1 ? "" : "s")"
    }
}

import Herder
import SwiftUI

/// An agent's `store_secret`, allowed from its approval card. The host read the file and holds
/// the value; this device takes it once, puts it on the host's hub as that hub's owner,
/// then tells the host, which deletes the file when the agent asked and lets the agent go on.
/// The value passes through memory only, and neither the host's link to its hub nor the
/// agent ever gets to write a secret.
extension Fleet {
    /// The hub `hostId` fetches its secrets from, paired with this device as its owner.
    func ownedHub(of hostId: HostId) async throws -> Machine {
        guard case .linked(let linked) = try await backupLink(of: hostId), linked.secrets else {
            throw HerderError.Local(detail: "This machine is not linked to a hub with Secrets on. Link it to one on its page, under Secrets.")
        }
        guard let hub = pairedHub(linked) else {
            throw HerderError.Local(detail: "This device is not paired with the machine's hub. Pair it with the hub as an owner to store secrets there.")
        }
        guard hub.role == .owner else {
            throw HerderError.Local(detail: "Only the owners of \(hub.name) store secrets there. Ask one of them to allow this.")
        }
        return hub
    }

    /// `ownedHub(of:)`, once it has the set `set`: created when it lacks it. With the
    /// hub's secrets, as it listed them.
    func ownedHub(of hostId: HostId, creating set: String) async throws -> (hub: Machine, secrets: [SecretInfo]) {
        let hub = try await ownedHub(of: hostId)
        guard case .secrets(let sets, let secrets) = try await client.send(hostId: hub.hostId, command: .listSecrets) else {
            throw HerderError.Local(detail: "the hub did not list its secrets")
        }
        if !sets.contains(where: { $0.name == set }) {
            _ = try await client.send(hostId: hub.hostId, command: .createSecretSet(set: set, assertion: nil))
        }
        return (hub, secrets)
    }
}

/// What an agent's `store_secret` asks: "Store ~/Downloads/AuthKey_X.p8 (257 B) as
/// ASC_PRIVATE_KEY in personal on herder-hub-0?", with the hub's sets to pick another from.
struct SecretStoreAsk: View {
    let fleet: Fleet
    let hostId: HostId
    let secret: SecretStoreRequest
    @Binding var picked: String?
    @State private var hub: Machine?
    @State private var sets: [String] = []
    @State private var problem: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(Self.question(secret, set: chosen, hub: hub?.name))
                .font(.subheadline)
                .foregroundStyle(Theme.text)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier("store-secret-question")
            if !secret.description.isEmpty {
                Text(secret.description).font(.footnote).foregroundStyle(Theme.secondary)
            }
            HStack(spacing: 8) {
                Text("Set").font(.footnote).foregroundStyle(Theme.secondary)
                Picker("Set", selection: Binding(get: { chosen }, set: { picked = $0 })) {
                    ForEach(choices, id: \.self) { Text($0).tag($0) }
                }
                .labelsHidden()
                .accessibilityIdentifier("store-secret-set")
                Spacer(minLength: 0)
            }
            Text(Self.detail(secret)).font(.caption).foregroundStyle(Theme.tertiary)
            if let problem {
                Text(problem).font(.footnote).foregroundStyle(Theme.failure)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .task(id: hostId) { await load() }
    }

    private var chosen: String { picked ?? secret.set }

    /// The hub's sets, with the agent's first when the hub lacks it: Allow creates it.
    private var choices: [String] {
        sets.contains(secret.set) ? sets : [secret.set] + sets
    }

    private func load() async {
        do {
            let hub = try await fleet.ownedHub(of: hostId)
            self.hub = hub
            guard case .secrets(let sets, _) = try await fleet.client.send(hostId: hub.hostId, command: .listSecrets)
            else { return }
            self.sets = sets.map(\.name)
            problem = nil
        } catch {
            problem = describe(error)
        }
    }

    /// "Store ~/Downloads/AuthKey_X.p8 (257 B) as ASC_PRIVATE_KEY in personal on hub-0?"
    nonisolated static func question(_ secret: SecretStoreRequest, set: String, hub: String?) -> String {
        "Store \(secret.path) (\(size(secret.bytes))) as \(secret.name) in \(set) on \(hub ?? "its hub")?"
    }

    /// The value's fingerprint, and what happens to the file.
    nonisolated static func detail(_ secret: SecretStoreRequest) -> String {
        let file = secret.deleteFileAfter ? "the file is deleted once the hub stored it" : "the file stays"
        return "Fingerprint \(secret.fingerprint) · \(file)"
    }

    /// "257 B", "12 KB".
    nonisolated static func size(_ bytes: UInt64) -> String {
        bytes < 1024 ? "\(bytes) B" : ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file)
    }
}

import Foundation
import Herder

/// Where running a vault on a machine from here stands.
enum VaultSetupStep: Equatable {
    /// The machine installs the vault, or with `install` off pairs with the one installed there
    /// by hand, and links itself to it.
    case setting(install: Bool)
    /// This device pairs with the vault, with the code the machine minted for it.
    case pairing
    /// Done: the vault is paired here, named so.
    case ready(String)
    /// The machine's herder does not know the command: install the vault there by hand.
    case needsNewerHerder
    case failed(String)
}

/// Runs a vault on a machine: the machine installs it (`install_vault`), or pairs with the one
/// installed there by hand (`link_local_vault`), and links itself to it with Secrets on and
/// Backup off. It answers with a code that pairs this device with the vault as an owner, which
/// is redeemed here, so no code is typed, and the vault is named after the machine.
@MainActor
struct VaultSetup {
    /// Sends a command to the machine.
    let send: (CommandBody) async throws -> CommandResult
    /// Pairs this device with the machines a `herder://pair` link names.
    let pair: (String) async throws -> [PairResult]
    /// Renames a paired machine on this device.
    let rename: (HostId, String) throws -> Void

    /// Sets up the vault on the machine named `machine`: installs one unless `installed`.
    /// `report` hears each step, the last one the outcome.
    func run(machine: String, installed: Bool, report: (VaultSetupStep) -> Void) async {
        report(.setting(install: !installed))
        do {
            let answer = try await send(installed ? .linkLocalVault : .installVault)
            guard case .devicePairing(let code, let fingerprint, let addresses, _) = answer else {
                throw HerderError.Local(detail: "The machine did not answer with a pairing for its vault.")
            }
            report(.pairing)
            let link = pairingUriToString(uri: PairingUri(hosts: addresses, fingerprint: fingerprint, code: code))
            let name = Self.name(machine: machine)
            for result in try await pair(link) {
                switch result {
                case .paired(let vault): try rename(vault.hostId, name)
                case .failed(_, let error): throw HerderError.Pairing(detail: error)
                }
            }
            report(.ready(name))
        } catch let error as HerderError where error.needsNewerHerder {
            report(.needsNewerHerder)
        } catch {
            report(.failed(describe(error)))
        }
    }

    /// The vault's name on this device: after its machine, as both report the same host name.
    nonisolated static func name(machine: String) -> String { "\(machine) vault" }
}

extension Fleet {
    /// Runs a vault on `hostId` as an Activity job, or pairs and links the one installed there
    /// by hand, as its Backup link says; the one running when it runs already.
    @discardableResult
    func setUpVault(on hostId: HostId) -> JobKey {
        let key = JobKey(hostId: hostId, kind: .vault)
        guard activity.enqueue(key, nil) else { return key }
        activity.start(key, before: nil)
        let installed = backupLinks[hostId] == .nowhere(localVault: true)
        let machine = machines.first { $0.hostId == hostId }?.name ?? hostId
        let client = client
        let setup = VaultSetup(
            send: { try await client.send(hostId: hostId, command: $0) },
            pair: { [weak self] link in try await self?.pair(link: link) ?? [] },
            rename: { [weak self] hostId, name in try self?.rename(hostId, to: name) })
        Task {
            await setup.run(machine: machine, installed: installed) { activity.setVault(key, $0) }
            activity.end(key)
            try? await backupLink(of: hostId)
        }
        return key
    }
}

extension JobProgress {
    /// A vault setup's, from its step.
    init(vault step: VaultSetupStep?) {
        switch step {
        case nil: self = .starting
        case .setting(let install): self = .settingUpVault(install: install)
        case .pairing: self = .pairingVault
        case .ready(let name): self = .vaultReady(name)
        case .needsNewerHerder: self = .needsNewerHerder
        case .failed(let message): self = .failed(message)
        }
    }
}

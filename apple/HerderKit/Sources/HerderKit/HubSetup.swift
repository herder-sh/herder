import Foundation
import Herder

/// Where running a hub on a machine from here stands.
enum HubSetupStep: Equatable {
    /// The machine installs the hub and links itself to it.
    case setting
    /// You claim the new hub as its owner, approving with your passkey.
    case claiming
    /// Done: the hub is yours, named so.
    case ready(String)
    /// The machine's herder does not know the command: install the hub there by hand.
    case needsNewerHerder
    case failed(String)
}

/// Runs a hub on a machine: the machine installs it (`install_hub`) and links itself to it
/// with Secrets on and Backup off. It answers with the new hub's ownership claim, which you
/// approve with your passkey, so no code is typed, and the hub is named after the machine, as
/// both report the same host name.
@MainActor
struct HubSetup {
    /// Sends a command to the machine.
    let send: (CommandBody) async throws -> CommandResult
    /// Claims the hub a request names with your passkey: its host id once approved.
    let claim: (ApprovalRequest) async throws -> HostId
    /// Renames a machine on this device.
    let rename: (HostId, String) throws -> Void

    /// Sets up the hub on the machine named `machine`. `report` hears each step, the last
    /// one the outcome.
    func run(machine: String, report: (HubSetupStep) -> Void) async {
        report(.setting)
        let name = UnpairedHub.name(machine: machine)
        do {
            guard case .ownershipClaim(let code, let fingerprint, let addresses, _) = try await send(.installHub) else {
                throw HerderError.Local(detail: "The machine did not answer with its hub’s claim.")
            }
            report(.claiming)
            let request = ApprovalRequest(kind: .machine, name: name, code: code, fingerprint: fingerprint,
                                          hosts: addresses)
            try rename(try await claim(request), name)
            report(.ready(name))
        } catch let error as HerderError where error.needsNewerHerder {
            report(.needsNewerHerder)
        } catch is CancellationError {
            report(.failed("\(name) runs, but nobody owns it yet: run \(MachineSource.claimCommand) on \(machine) to claim it."))
        } catch {
            report(.failed(describe(error)))
        }
    }
}

extension Fleet {
    /// Runs a hub on `hostId` as an Activity job; the one running when it runs already.
    @discardableResult
    func setUpHub(on hostId: HostId) -> JobKey {
        let key = JobKey(hostId: hostId, kind: .hub)
        guard activity.enqueue(key, nil) else { return key }
        activity.start(key, before: nil)
        let machine = machines.first { $0.hostId == hostId }?.name ?? hostId
        let client = client
        let setup = HubSetup(
            send: { try await client.send(hostId: hostId, command: $0) },
            claim: { [weak self] request in
                guard let self else { throw CancellationError() }
                return try await self.claim(request)
            },
            rename: { [weak self] hostId, name in try self?.rename(hostId, to: name) })
        Task {
            await setup.run(machine: machine) { activity.setHub(key, $0) }
            activity.end(key)
            try? await backupLink(of: hostId)
        }
        return key
    }

    /// Asks for `request`'s approval with the approval sheet, which the window shows, and waits
    /// for it: the claimed machine's host id. Throws `CancellationError` once the sheet closes
    /// without approving.
    func claim(_ request: ApprovalRequest) async throws -> HostId {
        try await withCheckedThrowingContinuation { continuation in
            claims[request]?.resume(throwing: CancellationError())
            claims[request] = continuation
            claiming = request
        }
    }

    /// Hands a claim's outcome to the setup waiting on it, if one is.
    func settleClaim(_ request: ApprovalRequest, _ outcome: Result<HostId, Error>) {
        claims.removeValue(forKey: request)?.resume(with: outcome)
    }
}

extension JobProgress {
    /// A hub setup's, from its step.
    init(hub step: HubSetupStep?) {
        switch step {
        case nil: self = .starting
        case .setting: self = .settingUpHub
        case .claiming: self = .claimingHub
        case .ready(let name): self = .hubReady(name)
        case .needsNewerHerder: self = .needsNewerHerder
        case .failed(let message): self = .failed(message)
        }
    }
}

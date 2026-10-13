import Foundation
import Herder

/// Where a fork made on this device came from.
struct ForkOrigin: Equatable {
    let sessionId: SessionId
    let hostId: HostId
}

/// Handing a session off to a machine, from the composer's machine menu: picking a machine
/// forks the session there and opens the fork, with no sheet in between, and archives the
/// original, so the session is listed once, where it now runs. The client core finds the
/// history: on the machine itself, relayed from the session's machine while it is connected,
/// else in the destination's vault.
extension Fleet {
    /// Why `machine` cannot take a session of `provider`, if it cannot.
    static func handoffBlocker(_ machine: Machine, provider: Provider?) -> String? {
        if machine.connection != .connected { return "Offline" }
        if machine.vault != nil || !machine.hosts.isEmpty { return "Vault" }
        if machine.role != .owner { return "Owner only" }
        guard let provider else { return "Loading" }
        if !machine.accounts.contains(where: { $0.provider == provider }) {
            return "No \(ModelCatalog.providerName(provider)) account"
        }
        return nil
    }

    /// The machine section of a session's menus: the current machine first, each other one
    /// handing the session off when picked, or with its accounts of the provider to pick from
    /// when it has several logins; "Fork Session" forks onto the current machine. `handOff` gets the
    /// machine and the account, `nil` for the machine's default.
    func machineSection(for key: SessionKey, provider: Provider?, forkable: Bool,
                        handOff: @escaping (HostId, AccountId?) -> Void) -> SettingsSection {
        let busy = handoffs[key] != nil
        let options = SettingsOption.machines(machines, current: key.hostId) { machine in
            forkable ? Self.handoffBlocker(machine, provider: provider) : "Cannot fork"
        }.map { option in
            guard !option.current, option.unavailable == nil,
                  let machine = machines.first(where: { $0.hostId == option.id }) else { return option }
            let accounts = SettingsOption.accounts(machine.accounts.filter { $0.provider == provider }, current: nil)
            guard accounts.count > 1 else { return option }
            var option = option
            option.children = accounts
            return option
        }
        return SettingsSection(
            kind: .machine, options: options, hint: "Another takes it over",
            action: .init(title: "Fork Session", symbol: "arrow.triangle.branch", enabled: forkable && !busy) {
                handOff(key.hostId, nil)
            },
            chooseChild: { host, account in if !busy { handOff(host, account) } }
        ) { host in
            if !busy { handOff(host, nil) }
        }
    }

    /// Forks `key` onto `hostId`, on `accountId` or the machine's default for the session's
    /// provider, showing it in `handoffs` meanwhile; returns the fork to open. A failure shows
    /// in a toast. Handed off to another machine, the original is archived, as `retire` does;
    /// a fork onto its own machine leaves it be.
    func handOff(_ key: SessionKey, to hostId: HostId, account accountId: AccountId?) async -> SessionKey? {
        guard handoffs[key] == nil else { return nil }
        let midTurn = hostId != key.hostId && [.running, .needsYou].contains(sessions[key]?.status)
        let name = machines.first { $0.hostId == hostId }?.name ?? hostId
        handoffs[key] = hostId
        defer { handoffs[key] = nil }
        do {
            let result = try await client.forkSession(
                source: key.hostId, sessionId: key.sessionId, destination: hostId, accountId: accountId)
            guard case .sessionForked(let sessionId, _, let original, let fromHost) = result else {
                throw HerderError.Local(detail: "\(name) did not return the fork")
            }
            let fork = SessionKey(hostId: hostId, sessionId: sessionId)
            forkOrigins[fork] = ForkOrigin(sessionId: original, hostId: fromHost)
            if hostId == key.hostId {
                toast = Toast(text: "Forked the session")
            } else if machines.first(where: { $0.hostId == key.hostId })?.connection == .connected {
                Task { await retire(key, midTurn: midTurn, handedOffTo: name) }
            } else {
                // Its machine cannot archive it now: it stays, read-only, until that machine is back.
                toast = Toast(text: "Handed off to \(name)")
            }
            return fork
        } catch {
            let what = hostId == key.hostId ? "fork the session" : "hand off to \(name)"
            toast = Toast(text: "Could not \(what): \(describe(error))", failed: true)
            return nil
        }
    }

    /// Archives `key` once it is handed off to the machine named `name`, so the fork is the one
    /// session left to go on with. Handed off mid-turn, the session's turn runs again on the
    /// other machine, so its own is interrupted first, as far as its machine can be reached:
    /// the original must not go on with the same work unseen. A machine refuses to archive while
    /// a turn runs, so the archive waits a while for it to end; a refusal, such as a child still
    /// at work, goes to `archiveRefusal` as any archive's does.
    func retire(_ key: SessionKey, midTurn: Bool, handedOffTo name: String) async {
        if midTurn {
            _ = try? await client.send(hostId: key.hostId, command: .interrupt(sessionId: key.sessionId))
            let deadline = ContinuousClock.now + .seconds(10)
            while [.running, .needsYou].contains(sessions[key]?.status), ContinuousClock.now < deadline {
                try? await Task.sleep(for: .milliseconds(100))
            }
        }
        await archive(key, toast: "Handed off to \(name)")
    }
}

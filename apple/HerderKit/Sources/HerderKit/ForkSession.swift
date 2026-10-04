import Foundation
import Herder
import Observation
import SwiftUI

struct ForkOrigin: Equatable {
    let sessionId: SessionId
    let hostId: HostId
}

/// Selection and submission state kept together so reconnects cannot send to a stale destination.
@MainActor
@Observable
final class ForkSessionModel {
    var hostId: HostId = ""
    var accountId: AccountId = ""
    private(set) var working = false
    private(set) var error: String?
    private(set) var opened: SessionKey?
    private(set) var origin: ForkOrigin?

    static func ineligible(_ machine: Machine, source: SessionKey, provider: Provider?) -> String? {
        if machine.connection != .connected { return "Offline" }
        if machine.vault != nil || !machine.hosts.isEmpty { return "Vault · stores sessions only" }
        if machine.role != .owner { return "Owner access required" }
        if provider == nil { return "Waiting for session details" }
        if !machine.accounts.contains(where: { $0.provider == provider }) { return "No matching provider account" }
        return nil
    }

    func select(_ machine: Machine, provider: Provider?) {
        hostId = machine.hostId
        accountId = machine.accounts.first { $0.provider == provider }?.accountId ?? ""
        error = nil
    }

    func destination(in machines: [Machine], source: SessionKey, provider: Provider?) -> Machine? {
        machines.first {
            $0.hostId == hostId && Self.ineligible($0, source: source, provider: provider) == nil
                && $0.accounts.contains { $0.accountId == accountId && $0.provider == provider }
        }
    }

    /// The sheet's submission path: real core request, provenance, then navigation.
    func forkAndOpen(source: SessionKey, fleet: Fleet, open: (SessionKey) -> Void) async {
        guard !working else { return }
        await fork(source: source, provider: fleet.sessions[source]?.provider, machines: fleet.machines) { host, command in
            try await fleet.client.send(hostId: host, command: command)
        }
        guard error == nil, let opened else { return }
        fleet.forkOrigins[opened] = origin
        open(opened)
    }

    func fork(source: SessionKey, provider: Provider?, machines: [Machine],
              send: (HostId, CommandBody) async throws -> CommandResult) async {
        guard !working else { return }
        guard let destination = destination(in: machines, source: source, provider: provider) else {
            error = "Choose a connected machine and a matching account."
            return
        }
        working = true
        error = nil
        opened = nil
        origin = nil
        defer { working = false }
        do {
            let result = try await send(destination.hostId, .forkSession(sessionId: source.sessionId, accountId: accountId))
            guard case .sessionForked(let sessionId, _, let original, let fromHost) = result else {
                throw HerderError.Local(detail: "the machine did not return a forked session")
            }
            origin = ForkOrigin(sessionId: original, hostId: fromHost)
            opened = SessionKey(hostId: destination.hostId, sessionId: sessionId)
        } catch {
            self.error = describe(error)
        }
    }
}

struct ForkSessionSheet: View {
    let fleet: Fleet
    let key: SessionKey
    let open: (SessionKey) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var model = ForkSessionModel()

    var body: some View {
        let provider = fleet.sessions[key]?.provider
        let chosen = fleet.machines.first { $0.hostId == model.hostId }
        SheetScaffold(title: "Fork Session", subtitle: "Create a separate copy of this session", height: 560) {
            Text("The conversation history and latest checkpoint are copied. The original session stays on its current machine.")
                .font(.subheadline).foregroundStyle(Theme.secondary)
            Field(label: "Destination machine", hint: "The destination needs a clone of the repository; another machine also needs access to this session through its vault. The machine checks these when you fork.") {
                ForEach(fleet.machines, id: \.hostId) { machine in
                    let reason = ForkSessionModel.ineligible(machine, source: key, provider: provider)
                    Button { model.select(machine, provider: provider) } label: {
                        HStack {
                            Image(systemName: "desktopcomputer")
                            Text(machine.name)
                            if machine.hostId == key.hostId { Text("This machine").font(.caption) }
                            Spacer()
                            if let reason { Text(reason).font(.caption).foregroundStyle(Theme.tertiary) }
                            else if model.hostId == machine.hostId { Image(systemName: "checkmark").foregroundStyle(Theme.accent) }
                        }
                        .frame(minHeight: 44).padding(12).background(Theme.raised, in: .rect(cornerRadius: Theme.corner))
                    }
                    .buttonStyle(.plain).disabled(reason != nil || model.working)
                }
            }
            if let chosen {
                Field(label: "Account") {
                    Picker("Account", selection: $model.accountId) {
                        ForEach(chosen.accounts.filter { $0.provider == provider }, id: \.accountId) { account in
                            Text(account.label).tag(account.accountId)
                        }
                    }.labelsHidden().disabled(model.working)
                }
            }
            if fleet.machines.allSatisfy({ ForkSessionModel.ineligible($0, source: key, provider: provider) != nil }) {
                Text("Connect a machine with owner access and an account for this provider to continue.")
                    .font(.footnote).foregroundStyle(Theme.secondary)
            }
            if let error = model.error { Text(error).font(.footnote).foregroundStyle(Theme.failure).textSelection(.enabled) }
        } footer: {
            if model.working { ProgressView().controlSize(.small); Text("Copying session…").font(.footnote) }
            Spacer()
            Button("Fork and Open") {
                Task {
                    await model.forkAndOpen(source: key, fleet: fleet) { opened in
                        open(opened)
                        dismiss()
                    }
                }
            }
            .buttonStyle(.borderedProminent)
            .disabled(model.working || model.destination(in: fleet.machines, source: key, provider: provider) == nil)
        }
        .disabled(model.working)
        .interactiveDismissDisabled(model.working)
        .onAppear {
            let eligible = fleet.machines.filter {
                ForkSessionModel.ineligible($0, source: key, provider: provider) == nil
            }
            if let first = eligible.first(where: { $0.hostId == key.hostId }) ?? eligible.first {
                model.select(first, provider: provider)
            }
        }
    }
}

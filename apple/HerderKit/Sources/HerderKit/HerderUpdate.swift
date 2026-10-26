import Herder
import SwiftUI

/// A machine's herder update as Activity follows it: how it was asked to restart, whether the
/// machine took it, and the last stage it reported, kept past the restart that clears it.
struct HerderUpdateJob: Equatable {
    enum Answer: Equatable {
        case pending, accepted
        case refused(String)
    }

    let when: UpdateWhen
    var answer = Answer.pending
    var stage: UpdateStage?
}

extension JobProgress {
    /// A herder update's, from `job` and the machine as it is listed now: done once the machine
    /// is back on the version it restarted on.
    init(herder job: HerderUpdateJob, update: UpdateStage?, connected: Bool, version: String?) {
        switch job.answer {
        case .pending: self = .starting; return
        case .refused(let message): self = .failed(message); return
        case .accepted: break
        }
        switch update ?? job.stage {
        case nil: self = .starting
        case .waitingForIdle(let turns)?: self = .waitingForTurns(Int(turns))
        case .downloading(_, let to)?: self = .downloading(to)
        case .restarting(let from, let to)?:
            self = connected && version == to ? .updated(from: from, to: to) : .restarting(to)
        case .upToDate(let version)?: self = .upToDate(version)
        case .failed(let reason)?: self = .failed(reason)
        }
    }
}

extension Fleet {
    /// Updates `hostId`'s herder to the latest release, restarting it `when` asked; a job in
    /// Activity follows it. Asking `.now` while it waits for turns starts it at once.
    @discardableResult
    func updateHerder(on hostId: HostId, when: UpdateWhen) -> JobKey {
        let key = JobKey(hostId: hostId, kind: .updateHerder)
        if activity[key] != nil, !progress(key).finished {
            if when == .now { Task { _ = try? await client.send(hostId: hostId, command: .updateDaemon(when: .now)) } }
            return key
        }
        activity.enqueue(key, update: HerderUpdateJob(when: when))
        Task {
            do {
                _ = try await client.send(hostId: hostId, command: .updateDaemon(when: when))
                activity.answer(key, .accepted)
            } catch {
                activity.answer(key, .refused(describe(error)))
            }
            settleUpdates()
        }
        return key
    }

    /// Keeps each herder update's latest stage, which the machine forgets as it restarts, and
    /// ends the updates that finished.
    func settleUpdates() {
        for key in activity.keys where key.kind == .updateHerder {
            if let stage = machines.first(where: { $0.hostId == key.hostId })?.update { activity.record(key, stage) }
            if activity.phase(key) != .ended, progress(key).finished { activity.end(key) }
        }
    }
}

/// Updates a machine's herder through Activity. With turns running it asks first: when idle,
/// the default, or now, which ends them. A machine too old to update from here gets the
/// command to run there once.
struct HerderUpdateButton: View {
    let fleet: Fleet
    let hostId: HostId
    @State private var asking = false

    var body: some View {
        let machine = fleet.machines.first { $0.hostId == hostId }
        let name = machine?.name ?? hostId
        if machine?.herderVersion == nil {
            VStack(alignment: .trailing, spacing: 2) {
                CopyPill(title: "herder update", text: "herder update")
                    .help("Run it on \(name) to update herder there")
                Text("Update once by hand; after that it updates from here.")
                    .font(.caption2).foregroundStyle(Theme.tertiary)
            }
        } else {
            let key = JobKey(hostId: hostId, kind: .updateHerder)
            let running = Int(machine?.resources?.runningTurns ?? 0)
            let underway = fleet.activity[key] != nil && !fleet.progress(key).finished
            PillButton(title: underway ? "Updating…" : "Update", symbol: "arrow.up.circle") {
                if running > 0 { asking = true } else { fleet.updateHerder(on: hostId, when: .idle) }
            }
            .disabled(underway || machine?.role != .owner || machine?.connection != .connected)
            .help("Update herder on \(name) to the latest release and restart it")
            .accessibilityIdentifier("update-herder-\(name)")
            .confirmationDialog(running == 1 ? "A turn is running on \(name)" : "\(running) turns are running on \(name)",
                                isPresented: $asking, titleVisibility: .visible) {
                Button("Update when idle") { fleet.updateHerder(on: hostId, when: .idle) }
                    .keyboardShortcut(.defaultAction)
                Button("Update now", role: .destructive) { fleet.updateHerder(on: hostId, when: .now) }
            } message: {
                Text("The restart ends running turns. When idle, it waits for them to end and starts no new one meanwhile.")
            }
        }
    }
}

/// The owned machines running an older herder than the newest one among them, each with its
/// Update; for the Providers page, next to the CLIs.
struct HerderVersionsSection: View {
    let fleet: Fleet

    var body: some View {
        let behind = Self.behind(fleet.machines)
        if !behind.isEmpty {
            ProvidersSection(title: "herder", note: "the machines’ own") {
                VStack(spacing: 0) {
                    ForEach(Array(behind.enumerated()), id: \.element.machine.hostId) { index, row in
                        if index > 0 { RowDivider() }
                        HStack(spacing: 10) {
                            Text("\(row.machine.name) runs an older herder").font(.subheadline).foregroundStyle(Theme.text)
                            Text(row.machine.herderVersion.map { "\($0) → \(row.newest)" } ?? "older than \(row.newest)")
                                .font(Theme.monoSmall).foregroundStyle(Theme.secondary)
                            Spacer(minLength: 8)
                            HerderUpdateButton(fleet: fleet, hostId: row.machine.hostId)
                        }
                        .padding(.horizontal, Theme.cellPadding)
                        .frame(minHeight: 48)
                    }
                }
                .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
                .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
            }
        }
    }

    /// The connected machines the user owns that run sessions on a herder older than the
    /// newest of theirs, which a machine too old to say its version is.
    static func behind(_ machines: [Machine]) -> [(machine: Machine, newest: String)] {
        let owned = machines.filter { $0.role == .owner && $0.hosts.isEmpty && $0.connection == .connected }
        guard let newest = owned.compactMap(\.herderVersion).max(by: { ProviderVersions.version($1, isNewerThan: $0) })
        else { return [] }
        return owned.compactMap { machine in
            let older = machine.herderVersion.map { ProviderVersions.version(newest, isNewerThan: $0) } ?? true
            return older ? (machine, newest) : nil
        }
    }
}

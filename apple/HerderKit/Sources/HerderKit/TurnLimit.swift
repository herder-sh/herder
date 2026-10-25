import Herder
import SwiftUI

/// How many agent turns a machine runs at once: the limit its owner sets, and how full it is.
struct TurnLoad: Hashable {
    let running: Int
    let max: Int
    let waiting: Int
    /// What keeps the machine from starting another turn now, if anything.
    var constraint: Constraint?

    init(running: Int, max: Int, waiting: Int, constraint: Constraint? = nil) {
        self.running = running
        self.max = max
        self.waiting = waiting
        self.constraint = constraint
    }

    init(_ resources: HostResources) {
        self.init(running: Int(resources.runningTurns), max: Int(resources.maxTurns),
                  waiting: Int(resources.waitingTurns), constraint: resources.constraint)
    }

    /// The highest limit the daemon accepts.
    static let ceiling = Int(maxTurnsLimit())

    /// "2/4 turns", beside the running count on a machine's card.
    var fraction: String { "\(running)/\(max) turns" }

    /// "2 running · 1 waiting", behind the ⓘ of "Turns at once".
    var usage: String { "\(running) running · \(waiting) waiting" }

    /// Why a session waits for capacity: "Waiting for a free slot · 2 of 2 turns in use", or
    /// what about the machine itself holds turns back.
    var waitingHint: String {
        switch constraint {
        case .memory: "Waiting for free memory on this machine"
        case .load: "Waiting for this machine's CPU load to drop"
        case .pressure: "Waiting while this machine stalls on memory"
        case .maxTurns, nil: "Waiting for a free slot · \(running) of \(max) \(max == 1 ? "turn" : "turns") in use"
        }
    }

    /// The hint when the machine has not reported its turns.
    static let unknownHint = "Waiting for a free slot on this machine"
}

/// Applies a new turn limit as soon as it is picked, one command at a time: picks made while
/// one is in flight collapse into the latest, which is sent next.
@MainActor @Observable
final class TurnLimitSetter {
    /// The limit picked and not yet reported back by the machine; shown instead of its own.
    private(set) var pending: Int?
    private(set) var error: String?
    private var sending = false
    private let send: (Int) async throws -> Void

    init(send: @escaping (Int) async throws -> Void) {
        self.send = send
    }

    convenience init(fleet: Fleet, hostId: HostId) {
        self.init { value in
            _ = try await fleet.client.send(hostId: hostId, command: .setResourceLimits(maxTurns: UInt32(value)))
        }
    }

    /// The limit to show: the one picked while it is on its way, else the machine's.
    func shown(_ reported: Int) -> Int { pending ?? reported }

    func pick(_ value: Int) {
        pending = value
        error = nil
        guard !sending else { return }
        Task { await flush() }
    }

    /// The machine reported `limit`: a pick it matches has arrived.
    func reported(_ limit: Int) {
        if pending == limit { pending = nil }
    }

    /// Sends picks until the latest is applied, or one is refused.
    func flush() async {
        sending = true
        defer { sending = false }
        var sent: Int?
        while let value = pending, value != sent {
            do {
                try await send(value)
                sent = value
            } catch {
                self.error = describe(error)
                pending = nil
                return
            }
        }
    }
}

/// The "Turns at once" row of a machine's Capacity: its limit with a stepper for owners, what
/// fits on the machine and how many turns run and wait now.
struct TurnLimitRow: View {
    let resources: HostResources
    let canChange: Bool
    @State private var setter: TurnLimitSetter

    init(fleet: Fleet, hostId: HostId, resources: HostResources, canChange: Bool) {
        self.resources = resources
        self.canChange = canChange
        _setter = State(initialValue: TurnLimitSetter(fleet: fleet, hostId: hostId))
    }

    var body: some View {
        let load = TurnLoad(resources)
        let value = setter.shown(load.max)
        SettingRow(label: "Turns at once", hint: TurnLoad.fitHint(resources),
                   help: "Agent turns this machine runs at the same time; more turns use more CPU and memory. The rest wait for a free slot. \(load.usage).",
                   saved: setter.error.map { .failed($0) } ?? (value != load.max ? .saving : nil)) {
            if load.waiting > 0 {
                Text("\(load.waiting) waiting").font(.caption.monospacedDigit()).foregroundStyle(Theme.accent)
            }
            Text("\(value)")
                .font(.title3.weight(.semibold).monospacedDigit())
                .foregroundStyle(Theme.text)
                .contentTransition(.numericText())
                .frame(minWidth: 28, alignment: .trailing)
            if canChange {
                Stepper("Turns at once", value: Binding(get: { value }, set: { setter.pick($0) }),
                        in: 1...TurnLoad.ceiling)
                    .labelsHidden()
            }
        }
        .onChange(of: load.max, initial: true) { setter.reported(load.max) }
    }
}

extension TurnLoad {
    /// About how many turns fit on a machine at once: a core and 2 GB of memory each, leaving a
    /// quarter of its cores to everything else.
    static func fit(cores: UInt32, memoryBytes: UInt64) -> Int {
        Swift.max(1, Swift.min(Int(cores) * 3 / 4, Int(memoryBytes >> 31)))
    }

    /// "16 GB, 8 cores · about 6 fit", under "Turns at once".
    static func fitHint(_ resources: HostResources) -> String {
        let memory = (resources.memoryTotalBytes + (1 << 29)) >> 30
        let fit = fit(cores: resources.cpuCores, memoryBytes: resources.memoryTotalBytes)
        return "\(memory) GB, \(resources.cpuCores) cores · about \(fit) fit"
    }
}

/// Why a session's prompt has not started: the machine's turns are all in use, or it is short
/// of memory or CPU; with a way to the machine's settings to raise the limit.
struct WaitingForCapacityNote: View {
    let load: TurnLoad?
    let settings: () -> Void

    var body: some View {
        VStack(spacing: 10) {
            Label(load?.waitingHint ?? TurnLoad.unknownHint, systemImage: "hourglass")
                .font(.subheadline)
                .foregroundStyle(Theme.secondary)
                .multilineTextAlignment(.center)
            Text("Your prompt is queued and starts on its own once there is room.")
                .font(.footnote)
                .foregroundStyle(Theme.tertiary)
                .multilineTextAlignment(.center)
            Button("Machine Settings…", action: settings)
                .buttonStyle(.bordered)
                .controlSize(.small)
        }
        .frame(maxWidth: .infinity)
        .padding(24)
    }
}

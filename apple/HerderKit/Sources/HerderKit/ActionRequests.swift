import Foundation
import Herder
import Observation
import SwiftUI

/// What one of your devices without a passkey, a TUI signed in with `herder login`, asks a
/// machine to do that needs your Face ID: open or attach to a terminal, change a secret. The
/// machine holds it until you approve or refuse it here, or it expires.
struct ActionRequest: Identifiable, Equatable {
    let hostId: HostId
    /// The machine it asks, by name.
    let machine: String
    let request: Herder.ApprovalRequest

    var id: String { "\(hostId)/\(request.requestId)" }

    /// "Open a terminal on trash-can-01?"
    var title: String { "\(request.action) on \(machine)?" }

    /// "From the TUI · herder 0.17.0": the device asking, by its client.
    var from: String {
        let parts = request.client.split(separator: "/", maxSplits: 1).map(String.init)
        let device = switch parts.first {
        case "herder-tui": "the TUI"
        case "herder-cli": "the herder CLI"
        default: parts.first ?? request.client
        }
        return (["From \(device)"] + parts.dropFirst().map { "herder \($0)" }).joined(separator: " · ")
    }

    var expires: Date? { Timestamp.date(request.expiresAt) }
}

extension Fleet {
    /// What your devices without a passkey ask of every machine, oldest first.
    var actionRequests: [ActionRequest] { Self.actionRequests(machines) }

    nonisolated static func actionRequests(_ machines: [Machine]) -> [ActionRequest] {
        machines
            .flatMap { machine in machine.approvals.map { ActionRequest(hostId: machine.hostId, machine: machine.name, request: $0) } }
            .sorted { $0.request.expiresAt < $1.request.expiresAt }
    }
}

/// Settling a request: Face ID signs the approval, and the machine lets the device that asked do
/// that one thing; Refuse tells it no. The sheet closes once the machine closes the request.
@MainActor @Observable
final class ActionApproval {
    enum State: Equatable {
        case asking
        case signing
        case settled
        case failed(String)
    }

    /// Approves the request (true), signing with Face ID, or refuses it (false).
    typealias Settle = @MainActor (Bool) async throws -> Void

    let request: ActionRequest
    private(set) var state = State.asking
    private let settle: Settle

    init(_ request: ActionRequest, settle: @escaping Settle) {
        self.request = request
        self.settle = settle
    }

    func approve() async { await run(true) }

    func refuse() async { await run(false) }

    private func run(_ approve: Bool) async {
        state = .signing
        do {
            try await settle(approve)
            state = .settled
        } catch {
            state = .failed(describe(error))
        }
    }
}

/// A request of a device without a passkey: what it asks to do on which machine, who asks, and
/// how long it waits; then Approve with Face ID or Refuse.
struct ActionRequestSheet: View {
    let approval: ActionApproval
    var biometry = Biometry.name
    /// When the expiry is counted from; the live sheet counts from now.
    var now: Date?

    private var request: ActionRequest { approval.request }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("TERMINAL REQUEST").font(.caption).foregroundStyle(Theme.tertiary)
            Text(request.title).font(.title3.weight(.bold)).foregroundStyle(Theme.text)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, 2)
                .accessibilityIdentifier("action-request-title")
            Text(request.from).font(Theme.body).foregroundStyle(Theme.secondary).padding(.top, 4)
            VStack(alignment: .leading, spacing: 2) {
                Text("Approving lets it do this once; it gets nothing else.")
                    .fixedSize(horizontal: false, vertical: true)
                expiry.foregroundStyle(Theme.secondary)
            }
            .font(.footnote)
            .foregroundStyle(Theme.text)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 10)
            .padding(.horizontal, 12)
            .background(.card, in: .rect(cornerRadius: Theme.corner))
            .padding(.vertical, 12)
            if case .failed(let why) = approval.state {
                Text(why).font(.footnote).foregroundStyle(Theme.danger).padding(.bottom, 8)
            }
            WideButton(title: "Approve with \(biometry)", primary: true) { Task { await approval.approve() } }
                .disabled(approval.state == .signing || approval.state == .settled)
                .keyboardShortcut(.defaultAction)
                .accessibilityIdentifier("approve-action")
            WideButton(title: "Refuse", tint: Theme.danger) { Task { await approval.refuse() } }
                .disabled(approval.state == .signing || approval.state == .settled)
                .padding(.top, 8)
                .accessibilityIdentifier("refuse-action")
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 20)
        .frame(maxWidth: .infinity, alignment: .leading)
        .layer(.sheet)
        #if os(macOS)
        .frame(idealWidth: 400)
        #else
        .presentationDetents([.medium])
        .presentationBackground(Theme.sheet)
        #endif
        .preferredColorScheme(.dark)
    }

    /// "It expires in 1:52 unless you approve it."
    @ViewBuilder private var expiry: some View {
        if let expires = request.expires {
            if let now {
                Text("It expires in \(Duration.seconds(max(0, expires.timeIntervalSince(now))).formatted(.time(pattern: .minuteSecond))) unless you approve it.")
            } else {
                Text("It expires in \(Text(timerInterval: Date.now...max(expires, .now), countsDown: true)) unless you approve it.")
            }
        }
    }
}

/// The request a machine holds for one of your devices without a passkey, settled through the
/// client core: approving asks Face ID for the assertion.
struct ActionRequestPresenter: View {
    let fleet: Fleet
    @State private var approval: ActionApproval

    init(fleet: Fleet, request: ActionRequest) {
        self.fleet = fleet
        _approval = State(initialValue: ActionApproval(request) { approve in
            if approve {
                try await fleet.client.approveAction(hostId: request.hostId, requestId: request.request.requestId)
            } else {
                try await fleet.client.refuseAction(hostId: request.hostId, requestId: request.request.requestId)
            }
        })
    }

    var body: some View {
        ActionRequestSheet(approval: approval)
    }
}

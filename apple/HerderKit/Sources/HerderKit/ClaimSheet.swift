import SwiftUI

/// Claim your machines: what becomes yours, then Create passkey and claim, then the users that
/// need a decision. Later puts it off until the next launch.
struct ClaimSheet: View {
    @State private var claim: Claim
    /// Where the machines' versions come from as they update.
    private let fleet: Fleet?
    @Environment(\.dismiss) private var dismiss

    init(claim: Claim, fleet: Fleet? = nil) {
        _claim = State(initialValue: claim)
        self.fleet = fleet
    }

    /// Claims `plan` on `fleet`'s machines with your passkey; a machine too old updates first.
    init(fleet: Fleet, plan: ClaimPlan) {
        let client = fleet.client
        self.init(claim: Claim(plan, authenticator: passkeyAuthenticator(),
                               send: { try await client.send(hostId: $0, command: $1) },
                               update: { fleet.updateHerder(on: $0, when: .idle) }),
                  fleet: fleet)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Claim your machines").font(.title3.weight(.bold)).foregroundStyle(Theme.text)
                .padding(.horizontal, 22)
                .padding(.top, 20)
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    banner
                    heading("Becomes yours")
                    SettingsGroup {
                        ForEach(Array(claim.machines.enumerated()), id: \.element.id) { index, machine in
                            if index > 0 { RowDivider() }
                            machineRow(machine)
                        }
                    }
                    if !claim.questions.isEmpty {
                        heading("Needs a decision")
                        SettingsGroup {
                            ForEach(Array(claim.questions.enumerated()), id: \.element.id) { index, question in
                                if index > 0 { RowDivider() }
                                questionRow(question)
                            }
                        }
                    }
                }
                .padding(.horizontal, 22)
                .padding(.top, 14)
            }
            .scrollBounceBehavior(.basedOnSize)
            HStack(spacing: 8) {
                Spacer()
                SettingsButton(title: claim.done ? "Done" : "Later", style: .quiet) { dismiss() }
                if claim.claimable {
                    SettingsButton(title: claim.plan.known == nil ? "Create passkey and claim" : "Claim with passkey",
                                   style: .primary) {
                        Task { await claim.claim() }
                    }
                    .disabled(claim.working)
                    .keyboardShortcut(.defaultAction)
                    .accessibilityIdentifier("claim")
                } else if !claim.outdated.isEmpty {
                    SettingsButton(title: "Update all", style: .primary) { claim.updateAll() }
                        .keyboardShortcut(.defaultAction)
                        .accessibilityIdentifier("update-all")
                } else if claim.claimed && !claim.done {
                    SettingsButton(title: "Apply decisions", style: .primary) {
                        Task { await claim.decide() }
                    }
                    .disabled(claim.working)
                    .keyboardShortcut(.defaultAction)
                    .accessibilityIdentifier("decide")
                }
            }
            .padding(.horizontal, 22)
            .padding(.vertical, 16)
        }
        .layer(.sheet)
        #if os(macOS)
        .frame(idealWidth: 640, minHeight: 420, idealHeight: 620)
        #else
        .presentationDetents([.large])
        .presentationBackground(Theme.sheet)
        #endif
        .preferredColorScheme(.dark)
        .onChange(of: fleet?.machines.map(\.herderVersion), initial: true) {
            if let fleet { claim.refresh(fleet.machines) }
        }
    }

    private var banner: some View {
        HStack(spacing: 12) {
            Image(systemName: "key.fill").font(.title3).foregroundStyle(Theme.action)
            VStack(alignment: .leading, spacing: 2) {
                Text(claim.plan.headline).font(Theme.body.weight(.medium)).foregroundStyle(Theme.text)
                Text(claim.plan.detail).font(.caption).foregroundStyle(Theme.secondary)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .background(Theme.action.opacity(0.12), in: .rect(cornerRadius: Theme.corner))
        .background(Theme.card, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.action.opacity(0.4)))
        .padding(.bottom, 6)
    }

    private func heading(_ title: String) -> some View {
        SectionHeading(title: title, tint: Theme.tertiary).padding(.top, 20).padding(.bottom, 8)
    }

    private func machineRow(_ machine: ClaimPlan.Machine) -> some View {
        let row = claim.rows[machine.id] ?? .ready
        return HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(machine.name).font(Theme.body.weight(.medium)).foregroundStyle(Theme.text)
                Text(Self.line(machine, row)).font(.caption)
                    .foregroundStyle(row.failed ? Theme.danger : row == .outdated(updating: false) ? Theme.accent : Theme.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            switch row {
            case .outdated(let updating):
                if machine.version == nil {
                    CopyPill(title: "herder update", text: "herder update")
                        .help("Run it on \(machine.name) to update herder there")
                } else {
                    PillButton(title: updating ? "Updating…" : "Update", symbol: "arrow.up.circle") {
                        claim.update(machine.id)
                    }
                    .disabled(updating)
                    .accessibilityIdentifier("update-\(machine.name)")
                }
            case .ready: Pill(text: "✓ ready", tint: Theme.switchOn)
            case .claiming: ProgressView().controlSize(.small)
            case .claimed: Pill(text: "✓ yours", tint: Theme.switchOn)
            case .failed: Pill(text: "✕ failed", tint: Theme.danger)
            }
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 11)
    }

    /// The line under a machine's name: what changes, what it needs, or why it refused.
    static func line(_ machine: ClaimPlan.Machine, _ row: Claim.Row) -> String {
        switch row {
        case .outdated(updating: false): "Needs herder \(ClaimPlan.short(ClaimPlan.needs))\(machine.version.map { " · runs \($0)" } ?? "")"
        case .outdated(updating: true): "Updating; claimable once it restarts"
        case .failed(let reason): reason
        case .ready, .claiming, .claimed: machine.change
        }
    }

    private func questionRow(_ question: ClaimPlan.Question) -> some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text("“\(question.user)” on \(question.machine)").font(Theme.body.weight(.medium)).foregroundStyle(Theme.text)
                if let refusal = claim.refusals[question.id] {
                    Text(refusal).font(.caption).foregroundStyle(Theme.danger).lineLimit(1)
                } else {
                    Text(question.reason).font(.caption).foregroundStyle(Theme.secondary)
                }
            }
            Spacer(minLength: 8)
            Menu {
                Picker("Who is this?", selection: Binding(get: { claim.answers[question.id] ?? .me },
                                                         set: { claim.answers[question.id] = $0 })) {
                    ForEach(ClaimPlan.Answer.allCases, id: \.self) { Text($0.title).tag($0) }
                }
                .pickerStyle(.inline)
            } label: {
                RoleBox(title: (claim.answers[question.id] ?? .me).title, menu: true)
            }
            .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden).fixedSize()
            .accessibilityLabel("Who is \(question.user) on \(question.machine)")
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 11)
    }
}

/// Fleet's way back to Claim your machines once put off, while a machine you own is not claimed.
struct ClaimBanner: View {
    let open: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "key.fill").font(.title3).foregroundStyle(Theme.action)
            VStack(alignment: .leading, spacing: 2) {
                Text("Claim your machines").font(Theme.body.weight(.medium)).foregroundStyle(Theme.text)
                Text("Some machines you own still know you by an old user.").font(.caption).foregroundStyle(Theme.secondary)
            }
            Spacer(minLength: 8)
            PillButton(title: "Claim…", prominent: true, action: open).accessibilityIdentifier("fleet-claim")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .background(Theme.action.opacity(0.12), in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.action.opacity(0.4)))
    }
}

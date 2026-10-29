import SwiftUI

/// Claim your machines: what becomes yours, what needs a decision, then Create passkey and
/// claim, with Face ID once for each machine.
struct ClaimSheet: View {
    let claim: Claim
    @Environment(\.dismiss) private var dismiss

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
                        ForEach(Array(claim.plan.machines.enumerated()), id: \.element.id) { index, machine in
                            if index > 0 { RowDivider() }
                            machineRow(machine)
                        }
                    }
                    if !claim.plan.questions.isEmpty {
                        heading("Needs a decision")
                        SettingsGroup {
                            ForEach(Array(claim.plan.questions.enumerated()), id: \.element.id) { index, question in
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
                if !claim.done {
                    SettingsButton(title: "Create passkey and claim", style: .primary) {
                        Task { await claim.claim() }
                    }
                    .disabled(claim.working)
                    .keyboardShortcut(.defaultAction)
                    .accessibilityIdentifier("claim")
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
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(machine.name).font(Theme.body.weight(.medium)).foregroundStyle(Theme.text)
                Text(machine.change).font(.caption).foregroundStyle(Theme.secondary)
                if case .failed(let why) = claim.rows[machine.id] {
                    Text(why).font(.caption).foregroundStyle(Theme.danger).fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 8)
            switch claim.rows[machine.id] ?? .ready {
            case .ready: Pill(text: "✓ ready", tint: Theme.switchOn)
            case .claiming: ProgressView().controlSize(.small)
            case .claimed: Pill(text: "✓ yours", tint: Theme.switchOn)
            case .failed: Pill(text: "✕ failed", tint: Theme.danger)
            }
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 11)
    }

    private func questionRow(_ question: ClaimPlan.Question) -> some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text("“\(question.user)” on \(question.machine)").font(Theme.body.weight(.medium)).foregroundStyle(Theme.text)
                Text(question.reason).font(.caption).foregroundStyle(Theme.secondary)
            }
            Spacer(minLength: 8)
            Menu {
                Picker("Who is this?", selection: Binding(get: { claim.answers[question.id] ?? question.suggested },
                                                         set: { claim.answers[question.id] = $0 })) {
                    ForEach(ClaimPlan.Answer.allCases, id: \.self) { Text($0.title).tag($0) }
                }
                .pickerStyle(.inline)
            } label: {
                RoleBox(title: (claim.answers[question.id] ?? question.suggested).title, menu: true)
            }
            .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden).fixedSize()
            .accessibilityLabel("Who is \(question.user) on \(question.machine)")
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 11)
    }
}

/// Claim your machines on the fleet, as `person` when a machine already knows your passkey.
struct ClaimFleetSheet: View {
    @State private var claim: Claim

    init(fleet: Fleet, plan: ClaimPlan, person: Claim.Person?, authenticator: Authenticator) {
        let name = person?.name ?? plan.machines.first?.users.first ?? NSFullUserName()
        _claim = State(initialValue: Claim(plan, name: name, person: person, authenticator: authenticator) {
            try await fleet.client.send(hostId: $0, command: $1)
        })
    }

    var body: some View {
        ClaimSheet(claim: claim)
    }
}

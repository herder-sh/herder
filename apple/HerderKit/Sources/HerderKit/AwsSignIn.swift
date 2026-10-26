import Herder
import SwiftUI

/// Why a machine gave no AWS profiles to pick from.
enum AwsListProblem: Error, Equatable {
    /// The machine runs a herder older than AWS sign-in, which cannot read the request.
    case outdated
    case offline
    /// The machine answered, with no profile in its `~/.aws/config`.
    case noProfiles
    case failed(String)

    /// Why listing a machine's profiles threw `error`.
    init(error: any Error) {
        if (error as? HerderError)?.needsNewerHerder == true {
            self = .outdated
        } else {
            self = .failed(describe(error))
        }
    }

    var label: String {
        switch self {
        case .outdated: "needs herder 0.8 or newer — run herder update"
        case .offline: "offline"
        case .noProfiles: "no ~/.aws/config profiles"
        case .failed(let message): message
        }
    }

    /// Why there is no profile to pick, given why each owned machine listed none.
    static func nothingToPick(_ problems: [AwsListProblem]) -> String {
        let only = { (problem: AwsListProblem) in !problems.isEmpty && problems.allSatisfy { $0 == problem } }
        return if only(.outdated) {
            "Your machines run a herder older than 0.8, which cannot sign in to AWS. Run herder update on them."
        } else if only(.offline) {
            "None of the machines you own is connected."
        } else if only(.noProfiles) {
            "None of the machines you own has a profile in ~/.aws/config."
        } else {
            "No connected machine you own listed an AWS profile."
        }
    }
}

/// Where one machine's AWS sign-in is: why it cannot start there, or how its job stands.
enum AwsSignInStatus: Equatable {
    case starting
    /// The CLI printed its device code; the owner approves it in a browser.
    case waiting(AwsDeviceCode)
    case signedIn
    /// The CLI exited without signing in, with this code.
    case failed(Int32?)
    /// The machine would not start the sign-in, for this reason.
    case refused(String)
    case noSuchProfile
    /// The machine listed no profiles, for this reason.
    case unlisted(AwsListProblem)
    case notInstalled
    case offline

    /// The status of a running sign-in: its terminal's state, the machine's refusal to open it,
    /// and the device code the machine's terminal list carries once the CLI prints it.
    init(state: TerminalConnection.State, refusal: ErrorCode?, terminalId: TerminalId?, terminals: [Terminal]) {
        switch state {
        case .connecting, .attached:
            let device = terminals.first { $0.terminalId == terminalId }.flatMap { terminal -> AwsDeviceCode? in
                if case .awsLogin(_, let device) = terminal.purpose { return device }
                return nil
            }
            self = device.map(AwsSignInStatus.waiting) ?? .starting
        case .exited(let code):
            self = code == 0 ? .signedIn : .failed(code)
        case .failed(let reason):
            switch refusal {
            case .unsupported: self = .notInstalled
            case .badRequest: self = .noSuchProfile
            default: self = .refused(reason)
            }
        }
    }

    var label: String {
        switch self {
        case .starting: "Starting"
        case .waiting: "Waiting for approval"
        case .signedIn: "Signed in"
        case .failed(let code): code.map { "Failed (exit \($0))" } ?? "Failed"
        case .refused(let reason): "Failed: \(reason)"
        case .noSuchProfile: "No such profile"
        case .unlisted(let problem): problem.label.prefix(1).uppercased() + problem.label.dropFirst()
        case .notInstalled: "aws not installed"
        case .offline: "Offline"
        }
    }
}

/// Signing the machines the user owns in to AWS: which machines, and which profiles they offer.
/// Each sign-in runs as a job in Activity.
enum AwsSignIn {
    /// The machines a sign-in runs on: those that run sessions and that this device owns.
    static func machines(_ machines: [Machine]) -> [Machine] {
        machines.filter { $0.hosts.isEmpty && $0.role == .owner }
    }

    /// The profiles to pick from: every profile any machine has, Identity Center ones first, as
    /// those are the ones `aws sso login` signs in.
    static func choices(_ profiles: [HostId: [AwsProfile]]) -> [AwsProfile] {
        var sso: [String: Bool] = [:]
        for profile in profiles.values.joined() { sso[profile.name] = (sso[profile.name] ?? false) || profile.sso }
        return sso.map { AwsProfile(name: $0.key, sso: $0.value) }
            .sorted { ($0.sso ? 0 : 1, $0.name) < ($1.sso ? 0 : 1, $1.name) }
    }

    /// Why `profile` cannot be signed in on `machine`, given the profiles each connected machine
    /// listed and why the others listed none; `nil` when it can.
    static func blocker(_ machine: Machine, profile: String, profiles: [HostId: [AwsProfile]],
                        problems: [HostId: AwsListProblem]) -> AwsSignInStatus? {
        if machine.connection != .connected { return .offline }
        if profiles[machine.hostId]?.contains(where: { $0.name == profile }) == true { return nil }
        return problems[machine.hostId].map(AwsSignInStatus.unlisted) ?? .noSuchProfile
    }

    /// The owned machines a sign-in to `profile` starts on: every one it can, less those the
    /// owner left out.
    static func selected(_ machines: [Machine], profile: String, profiles: [HostId: [AwsProfile]],
                         problems: [HostId: AwsListProblem], leftOut: Set<HostId>) -> Set<HostId> {
        Set(Self.machines(machines).filter {
            !leftOut.contains($0.hostId) && blocker($0, profile: profile, profiles: profiles, problems: problems) == nil
        }.map(\.hostId))
    }
}

/// Signs the machines the user owns in to AWS IAM Identity Center at once: the owner picks a
/// profile and the machines to sign in, and herder runs the AWS CLI's own device-code sign-in on
/// each as a job in Activity, where the owner approves each code in the browser.
struct AwsSignInSheet: View {
    let fleet: Fleet
    @AppStorage("awsProfile") private var picked = ""
    /// The machines the owner left out of each profile's sign-in, as JSON: profile to host ids.
    @AppStorage("awsLeftOut") private var leftOutJSON = ""
    /// The profiles each connected owned machine listed; `nil` until they answered.
    @State private var profiles: [HostId: [AwsProfile]]?
    /// Why each owned machine listed no profiles, when it did not.
    @State private var problems: [HostId: AwsListProblem] = [:]
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        SheetScaffold(title: "AWS Sign-in", subtitle: "IAM Identity Center on the machines you own", height: 600) {
            picker
            Text("Each sign-in runs in Activity, where you approve its code in the browser. herder never sees the tokens the AWS CLI caches.")
                .font(.footnote).foregroundStyle(Theme.tertiary)
        } footer: {
            Spacer()
            let count = selected.count
            ActionButton(title: "Sign in on \(count) machine\(count == 1 ? "" : "s")", style: .primary) { start() }
                .frame(maxWidth: 220).disabled(!choices.contains { $0.name == picked } || count == 0)
        }
        .task { await load() }
    }

    private var choices: [AwsProfile] { AwsSignIn.choices(profiles ?? [:]) }

    @ViewBuilder private var picker: some View {
        Field(label: "Profile", hint: "From ~/.aws/config on your machines. Identity Center profiles come first.") {
            if profiles == nil {
                ProgressView().frame(maxWidth: .infinity, alignment: .leading)
            } else if choices.isEmpty {
                Text(AwsSignIn.machines(fleet.machines).isEmpty
                     ? "You own no machine here; signing in to AWS is the owner’s."
                     : AwsListProblem.nothingToPick(Array(problems.values)))
                    .foregroundStyle(Theme.secondary)
                problemList
            } else {
                Picker("Profile", selection: $picked) {
                    ForEach(choices, id: \.name) { profile in
                        Text(profile.sso ? profile.name : "\(profile.name) · no Identity Center").tag(profile.name)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        if !choices.isEmpty { machineList }
    }

    private var leftOut: [String: Set<HostId>] {
        (try? JSONDecoder().decode([String: Set<HostId>].self, from: Data(leftOutJSON.utf8))) ?? [:]
    }

    private var selected: Set<HostId> {
        AwsSignIn.selected(fleet.machines, profile: picked, profiles: profiles ?? [:], problems: problems,
                           leftOut: leftOut[picked] ?? [])
    }

    /// Leaves `hostId` out of the picked profile's sign-ins, or takes it back in.
    private func leave(_ hostId: HostId, out: Bool) {
        var all = leftOut
        if out { all[picked, default: []].insert(hostId) } else { all[picked]?.remove(hostId) }
        all = all.filter { !$0.value.isEmpty }
        leftOutJSON = (try? JSONEncoder().encode(all)).map { String(decoding: $0, as: UTF8.self) } ?? ""
    }

    /// Every owned machine, on when the picked profile signs in there, and why not when it cannot.
    private var machineList: some View {
        Field(label: "Machines",
              hint: "A sign-in lands in the machine’s ~/.aws/sso/cache, where every session on it can use it.") {
            VStack(spacing: 0) {
                ForEach(Array(AwsSignIn.machines(fleet.machines).enumerated()), id: \.element.hostId) { index, machine in
                    if index > 0 { RowDivider() }
                    let blocker = AwsSignIn.blocker(machine, profile: picked, profiles: profiles ?? [:], problems: problems)
                    Toggle(isOn: Binding(get: { selected.contains(machine.hostId) },
                                         set: { leave(machine.hostId, out: !$0) })) {
                        HStack(spacing: 10) {
                            Image(systemName: "desktopcomputer").foregroundStyle(Theme.secondary)
                            Text(machine.name).foregroundStyle(Theme.text)
                            Spacer(minLength: 8)
                            if let blocker {
                                Text(blocker.label).font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
                            }
                        }
                    }
                    .toggleStyle(.switch)
                    .disabled(blocker != nil)
                    .font(.subheadline)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 8)
                    .frame(minHeight: 44)
                }
            }
            .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
        }
    }

    /// Each owned machine that listed no profiles, and why.
    @ViewBuilder private var problemList: some View {
        let unlisted = AwsSignIn.machines(fleet.machines).compactMap { machine in
            problems[machine.hostId].map { (hostId: machine.hostId, name: machine.name, problem: $0) }
        }
        if !unlisted.isEmpty {
            VStack(alignment: .leading, spacing: 2) {
                ForEach(unlisted, id: \.hostId) { item in
                    HStack(spacing: 8) {
                        Text("\(item.name): \(item.problem.label)")
                        if item.problem == .outdated {
                            Spacer(minLength: 8)
                            HerderUpdateButton(fleet: fleet, hostId: item.hostId)
                        }
                    }
                }
            }
            .font(.footnote)
            .foregroundStyle(Theme.tertiary)
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    /// Asks every connected machine the user owns for its profiles, keeping why each machine
    /// that listed none did not.
    private func load() async {
        let client = fleet.client
        let machines = AwsSignIn.machines(fleet.machines)
        let hosts = machines.filter { $0.connection == .connected }.map(\.hostId)
        let answers = await withTaskGroup(of: (HostId, Result<[AwsProfile], AwsListProblem>).self) { group in
            for hostId in hosts {
                group.addTask {
                    do {
                        let profiles = try await client.listAwsProfiles(hostId: hostId)
                        return (hostId, profiles.isEmpty ? .failure(.noProfiles) : .success(profiles))
                    } catch {
                        return (hostId, .failure(AwsListProblem(error: error)))
                    }
                }
            }
            var answers: [HostId: Result<[AwsProfile], AwsListProblem>] = [:]
            for await (hostId, answer) in group { answers[hostId] = answer }
            return answers
        }
        var listed: [HostId: [AwsProfile]] = [:]
        var problems: [HostId: AwsListProblem] = [:]
        for machine in machines {
            switch answers[machine.hostId] {
            case .success(let profiles): listed[machine.hostId] = profiles
            case .failure(let problem): problems[machine.hostId] = problem
            case nil: problems[machine.hostId] = .offline
            }
        }
        self.problems = problems
        profiles = listed
        if !choices.contains(where: { $0.name == picked }), let first = choices.first { picked = first.name }
    }

    private func start() {
        guard choices.contains(where: { $0.name == picked }) else { return }
        for hostId in selected { fleet.signInToAws(picked, on: hostId) }
        dismiss()
    }
}

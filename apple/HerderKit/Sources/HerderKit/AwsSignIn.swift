import Herder
import SwiftUI

/// Where one machine's AWS sign-in is, as the sign-in sheet shows it.
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

    /// Whether the sign-in there is over, one way or the other.
    var isFinal: Bool {
        switch self {
        case .starting, .waiting: false
        default: true
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
        case .notInstalled: "aws not installed"
        case .offline: "Offline"
        }
    }
}

/// A sign-in to one AWS profile on every machine the user owns. The fleet keeps it, so closing
/// the sheet leaves the sign-ins running and reopening it shows them.
@MainActor
@Observable
final class AwsSignIn {
    struct Row: Identifiable {
        let hostId: HostId
        let name: String
        /// Why the sign-in did not start there, when it did not.
        var skipped: AwsSignInStatus?
        var connection: TerminalConnection?

        var id: HostId { hostId }
    }

    let profile: String
    private(set) var rows: [Row] = []

    init(profile: String) {
        self.profile = profile
    }

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

    /// Starts the sign-in on `machines`, or on every owned machine, given the profiles each
    /// connected machine listed.
    func start(fleet: Fleet, profiles: [HostId: [AwsProfile]], only hosts: Set<HostId>? = nil) {
        for machine in Self.machines(fleet.machines) where hosts?.contains(machine.hostId) ?? true {
            var row = Row(hostId: machine.hostId, name: machine.name)
            if machine.connection != .connected {
                row.skipped = .offline
            } else if profiles[machine.hostId]?.contains(where: { $0.name == profile }) != true {
                row.skipped = .noSuchProfile
            } else {
                let connection = TerminalConnection(hostId: machine.hostId, terminalId: nil, awsProfile: profile)
                connection.connect(client: fleet.client, sessionId: nil, cols: 100, rows: 30)
                row.connection = connection
            }
            if let index = rows.firstIndex(where: { $0.hostId == machine.hostId }) {
                rows[index] = row
            } else {
                rows.append(row)
            }
        }
    }

    func status(of row: Row, in fleet: Fleet) -> AwsSignInStatus {
        if let skipped = row.skipped { return skipped }
        guard let connection = row.connection else { return .starting }
        let terminals = fleet.machines.first { $0.hostId == row.hostId }?.terminals ?? []
        return AwsSignInStatus(state: connection.state, refusal: connection.refusal,
                               terminalId: connection.terminalId, terminals: terminals)
    }
}

/// Signs every machine the user owns in to AWS IAM Identity Center at once: the owner picks a
/// profile, herder runs the AWS CLI's own device-code sign-in on each machine that has it, and
/// the owner approves each code in the browser.
struct AwsSignInSheet: View {
    let fleet: Fleet
    @AppStorage("awsProfile") private var picked = ""
    /// The profiles each connected owned machine listed; `nil` until they answered.
    @State private var profiles: [HostId: [AwsProfile]]?
    @State private var expanded: Set<HostId> = []
    @Environment(\.openURL) private var openURL
    @Environment(\.dismiss) private var dismiss

    private var run: AwsSignIn? { fleet.awsSignIn }

    private var statuses: [(row: AwsSignIn.Row, status: AwsSignInStatus)] {
        guard let run else { return [] }
        return run.rows.map { ($0, run.status(of: $0, in: fleet)) }
    }

    private var finished: Bool { statuses.allSatisfy(\.status.isFinal) }

    private var pending: [AwsDeviceCode] {
        statuses.compactMap {
            if case .waiting(let device) = $0.status { return device }
            return nil
        }
    }

    private var failed: Set<HostId> {
        Set(statuses.filter { $0.status.isFinal && $0.status != .signedIn }.map(\.row.hostId))
    }

    var body: some View {
        SheetScaffold(title: "AWS Sign-in", subtitle: "IAM Identity Center on every machine you own", height: 700) {
            if run == nil || finished { picker }
            if let run {
                Field(label: run.profile) {
                    VStack(spacing: 0) {
                        ForEach(Array(statuses.enumerated()), id: \.element.row.id) { index, item in
                            if index > 0 { RowDivider() }
                            row(item.row, item.status)
                        }
                    }
                    .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
                    .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
                }
                if finished {
                    let signedIn = statuses.filter { $0.status == .signedIn }.count
                    Label("\(signedIn) of \(statuses.count) machines signed in to \(run.profile).",
                          systemImage: signedIn == statuses.count ? "checkmark.circle.fill" : "exclamationmark.circle")
                        .font(.subheadline)
                        .foregroundStyle(signedIn == statuses.count ? Theme.success : Theme.text)
                } else {
                    Text("Approve each code in the browser. herder never sees the tokens the AWS CLI caches.")
                        .font(.footnote).foregroundStyle(Theme.tertiary)
                }
            }
        } footer: {
            if run != nil && !finished {
                ActionButton(title: "Approve all", style: .secondary) {
                    for device in pending { open(device) }
                }.frame(maxWidth: 180).disabled(pending.isEmpty)
            }
            if run != nil && finished && !failed.isEmpty {
                ActionButton(title: "Retry failed", style: .secondary) { await retry() }.frame(maxWidth: 180)
            }
            Spacer()
            if run == nil || finished {
                ActionButton(title: "Sign in on all machines", style: .primary) { start() }
                    .frame(maxWidth: 220).disabled(!choices.contains { $0.name == picked })
            } else {
                ActionButton(title: "Done", style: .primary) { dismiss() }.frame(maxWidth: 180)
            }
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
                     : "No connected machine you own has an AWS profile.")
                    .foregroundStyle(Theme.secondary)
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
    }

    private func row(_ row: AwsSignIn.Row, _ status: AwsSignInStatus) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 10) {
                Image(systemName: "desktopcomputer").foregroundStyle(Theme.secondary)
                Text(row.name).foregroundStyle(Theme.text)
                Spacer(minLength: 8)
                if case .waiting(let device) = status {
                    Text(device.userCode).font(Theme.mono).foregroundStyle(Theme.text).textSelection(.enabled)
                    Button("Approve") { open(device) }.buttonStyle(.borderedProminent).controlSize(.small)
                }
                statusLabel(status)
                if row.connection != nil {
                    Button {
                        if expanded.contains(row.hostId) { expanded.remove(row.hostId) } else { expanded.insert(row.hostId) }
                    } label: {
                        Image(systemName: expanded.contains(row.hostId) ? "chevron.down" : "chevron.right")
                            .font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
                            .frame(width: 24, height: 24).contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .help("Show the machine’s terminal")
                }
            }
            if let connection = row.connection, expanded.contains(row.hostId) {
                TerminalSurface(connection: connection, client: fleet.client, sessionId: nil)
                    .id(ObjectIdentifier(connection))
                    .frame(height: 240).background(.black)
            }
        }
        .font(.subheadline)
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .frame(minHeight: 44)
    }

    private func statusLabel(_ status: AwsSignInStatus) -> some View {
        HStack(spacing: 6) {
            switch status {
            case .starting, .waiting: ProgressView().controlSize(.mini)
            case .signedIn: Image(systemName: "checkmark.circle.fill").foregroundStyle(Theme.success)
            case .offline, .noSuchProfile: Image(systemName: "minus.circle").foregroundStyle(Theme.tertiary)
            default: Image(systemName: "xmark.circle.fill").foregroundStyle(Theme.failure)
            }
            Text(status.label).foregroundStyle(status == .signedIn ? Theme.text : Theme.secondary).lineLimit(1)
        }
        .font(.caption)
    }

    private func open(_ device: AwsDeviceCode) {
        if let url = URL(string: device.verificationUrl) { openURL(url) }
    }

    /// Asks every connected machine the user owns for its profiles.
    private func load() async {
        let client = fleet.client
        let hosts = AwsSignIn.machines(fleet.machines).filter { $0.connection == .connected }.map(\.hostId)
        let listed = await withTaskGroup(of: (HostId, [AwsProfile]).self) { group in
            for hostId in hosts {
                group.addTask { (hostId, (try? await client.listAwsProfiles(hostId: hostId)) ?? []) }
            }
            var listed: [HostId: [AwsProfile]] = [:]
            for await (hostId, profiles) in group { listed[hostId] = profiles }
            return listed
        }
        profiles = listed
        if !choices.contains(where: { $0.name == picked }), let first = choices.first { picked = first.name }
    }

    private func start() {
        guard let profiles, choices.contains(where: { $0.name == picked }) else { return }
        let run = AwsSignIn(profile: picked)
        run.start(fleet: fleet, profiles: profiles)
        fleet.awsSignIn = run
        expanded = []
    }

    /// Runs the sign-in again where it did not succeed, with the machines' profiles listed afresh.
    private func retry() async {
        guard let run else { return }
        let hosts = failed
        await load()
        run.start(fleet: fleet, profiles: profiles ?? [:], only: hosts)
    }
}

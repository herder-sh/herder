import Herder
import SwiftUI

/// What the Activity indicator sums up: how many jobs are listed, how many have not ended, and
/// whether one waits on the user.
struct ActivitySummary: Equatable {
    let total: Int
    let open: Int
    let needsYou: Bool
    let failed: Bool

    init(_ progress: [JobProgress]) {
        total = progress.count
        open = progress.filter { !$0.finished }.count
        needsYou = progress.contains { $0.group == .needsYou }
        failed = progress.contains(where: \.failed)
    }

    var title: String { open > 0 ? "Activity · \(open)" : "Activity" }
}

/// A job as the Activity list shows it.
struct ActivityItem: Identifiable {
    let key: JobKey
    let progress: JobProgress
    let text: JobText
    /// When it ended, once it has.
    var ended: Date?
    /// Whether Fix with AI is offered for it.
    var fixable = false

    var id: JobKey { key }
}

extension Fleet {
    var activitySummary: ActivitySummary { ActivitySummary(activity.keys.map(progress)) }

    /// Every job this device started, oldest first.
    var activityItems: [ActivityItem] {
        activity.keys.map { key in
            let progress = progress(key)
            return ActivityItem(key: key, progress: progress, text: JobText(fleet: self, key: key, progress: progress),
                                ended: activity[key]?.ended, fixable: canFix(key))
        }
    }
}

/// The page header's Activity control, on the Mac and iPad: a ring of the jobs done while some
/// run, its label amber while one needs the user, and the Activity list in a popover. Hidden
/// with no jobs.
struct ActivityChip: View {
    let fleet: Fleet
    @State private var open = false
    @Environment(\.compactActions) private var compact

    var body: some View {
        let summary = fleet.activitySummary
        if summary.total > 0 {
            Button { open.toggle() } label: {
                HStack(spacing: 6) {
                    ActivityRing(summary: summary)
                    if !compact { Text(summary.title).lineLimit(1) }
                }
                .headerControl(tint: summary.needsYou ? Theme.accent : Theme.secondary, selected: open,
                               labelled: !compact)
            }
            .buttonStyle(.plain)
            .help(compact ? summary.title : summary.needsYou ? "A job needs you" : "Installs, logins, sign-ins and updates")
            .accessibilityIdentifier("activity")
            .popover(isPresented: $open, arrowEdge: .bottom) {
                ActivityPanel(fleet: fleet).frame(width: 560)
            }
        }
    }
}

/// The Activity button in an iPhone screen's toolbar, which opens the list as a sheet.
struct ActivityToolbarButton: View {
    let fleet: Fleet
    let open: () -> Void

    var body: some View {
        let summary = fleet.activitySummary
        if summary.total > 0 {
            Button(action: open) {
                HStack(spacing: 4) {
                    ActivityRing(summary: summary)
                    if summary.open > 0 { Text("\(summary.open)").font(.subheadline.weight(.semibold)).monospacedDigit() }
                }
            }
            .accessibilityLabel(summary.title)
            .accessibilityIdentifier("activity")
        }
    }
}

/// The jobs done out of all of them, as a ring; amber while one needs the user, a mark once
/// every one has ended.
struct ActivityRing: View {
    let summary: ActivitySummary

    var body: some View {
        if summary.open == 0 {
            Image(systemName: summary.failed ? "exclamationmark.circle.fill" : "checkmark.circle.fill")
                .foregroundStyle(summary.failed ? Theme.failure : Theme.success)
                .font(.subheadline)
        } else {
            let done = Double(summary.total - summary.open) / Double(max(summary.total, 1))
            ZStack {
                Circle().stroke(Theme.stroke, lineWidth: 2.5)
                Circle().trim(from: 0, to: max(done, 0.08))
                    .stroke(summary.needsYou ? Theme.accent : Theme.running, style: StrokeStyle(lineWidth: 2.5, lineCap: .round))
                    .rotationEffect(.degrees(-90))
            }
            .frame(width: 13, height: 13)
        }
    }
}

/// The Activity list and, in its place, the detail of the job picked: the Mac's popover and the
/// iPhone's sheet.
struct ActivityPanel: View {
    let fleet: Fleet
    /// Closes the sheet, where there is one to close.
    var close: (() -> Void)?
    @State private var picked: JobKey?

    var body: some View {
        if let picked, fleet.activity[picked] != nil {
            ActivityDetail(fleet: fleet, key: picked) { self.picked = nil }
        } else {
            ActivityList(items: fleet.activityItems, clear: fleet.clearFinished, close: close,
                         open: { picked = $0 }, retry: fleet.retry, fix: { key in Task { await fleet.fix(key) } })
        }
    }
}

/// The jobs grouped as Needs you, Running, Queued and Done, with Clear finished.
struct ActivityList: View {
    let items: [ActivityItem]
    let clear: () -> Void
    var close: (() -> Void)?
    let open: (JobKey) -> Void
    let retry: (JobKey) -> Void
    var fix: (JobKey) -> Void = { _ in }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 10) {
                Text("Activity").font(.headline).foregroundStyle(Theme.text)
                Spacer()
                if items.contains(where: \.progress.finished) {
                    Button("Clear finished", action: clear)
                        .buttonStyle(.plain).font(.subheadline).foregroundStyle(Theme.secondary)
                }
                if let close {
                    Button("Done", action: close).buttonStyle(.plain).font(.subheadline.weight(.semibold))
                        .foregroundStyle(Theme.text)
                }
            }
            .padding(.horizontal, 14)
            .frame(minHeight: 44)
            Divider()
            if items.isEmpty {
                Text("Nothing running. Installs, updates, logins and AWS sign-ins you start show here.")
                    .font(.subheadline).foregroundStyle(Theme.secondary)
                    .padding(14)
            }
            // Scrolls only once the jobs outgrow the popover.
            ViewThatFits(in: .vertical) {
                groups
                ScrollView { groups }
            }
            .frame(maxHeight: 560)
        }
        .fixedSize(horizontal: false, vertical: true)
        .layer(.sheet)
    }

    private var groups: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Self.grouped(items), id: \.group) { group, members in
                HStack(spacing: 6) {
                    Text(group.title.uppercased())
                    Text("\(members.count)").foregroundStyle(Theme.tertiary)
                }
                .font(.caption.weight(.semibold))
                .foregroundStyle(group == .needsYou ? Theme.accent : Theme.secondary)
                .padding(.horizontal, 14)
                .padding(.top, 12)
                .padding(.bottom, 2)
                ForEach(members) { item in
                    ActivityRow(item: item, open: { open(item.key) }, retry: { retry(item.key) }, fix: { fix(item.key) })
                }
            }
        }
        .padding(.bottom, 8)
    }

    /// The groups that have jobs, in order: Done newest first, the others oldest first.
    nonisolated static func grouped(_ items: [ActivityItem]) -> [(group: JobProgress.Group, members: [ActivityItem])] {
        JobProgress.Group.allCases.compactMap { group in
            var members = items.filter { $0.progress.group == group }
            if group == .done { members.sort { ($0.ended ?? .distantPast) > ($1.ended ?? .distantPast) } }
            return members.isEmpty ? nil : (group, members)
        }
    }
}

/// A job's row: its glyph, title and detail line, and what it asks for: open a login, approve a
/// code, retry a failure.
struct ActivityRow: View {
    let item: ActivityItem
    let open: () -> Void
    let retry: () -> Void
    var fix: () -> Void = {}
    @Environment(\.openURL) private var openURL

    var body: some View {
        let progress = item.progress
        let text = item.text
        HStack(spacing: 10) {
            Button(action: open) {
                HStack(spacing: 10) {
                    JobGlyph(progress: progress)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(text.title).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text).lineLimit(1)
                        HStack(spacing: 0) {
                            Text(text.detail)
                            if progress.finished, !progress.failed, let ended = item.ended {
                                Text(" · ")
                                RelativeTime(date: ended)
                            }
                        }
                        .font(progress.group == .done && !progress.failed ? Theme.monoSmall : .caption)
                        .foregroundStyle(progress.failed ? Theme.failure : Theme.secondary)
                        .lineLimit(1)
                    }
                    Spacer(minLength: 8)
                    if !text.warnings.isEmpty {
                        Text(text.warnings.count == 1 ? "1 warning" : "\(text.warnings.count) warnings")
                            .font(.caption.weight(.semibold)).foregroundStyle(Theme.accent)
                            .padding(.horizontal, 8).padding(.vertical, 3)
                            .overlay(Capsule().strokeBorder(Theme.accent.opacity(0.6)))
                    }
                }
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityElement(children: .combine)
            .accessibilityLabel("\(text.title), \(progress.label)")
            .accessibilityIdentifier(item.key.accessibilityId(machine: text.machine))
            switch progress {
            case .running: PillButton(title: "Open login", prominent: true, action: open)
            case .approve(let device):
                PillButton(title: "Approve", prominent: true) {
                    if let url = URL(string: device.verificationUrl) { openURL(url) }
                }
            case .failed:
                if item.fixable { PillButton(title: "Fix with AI", symbol: "sparkles", action: fix) }
                PillButton(title: "Retry", action: retry)
            default: EmptyView()
            }
            Button(action: open) {
                Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
                    .frame(width: 16, height: 36).contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityHidden(true)
        }
        .padding(.horizontal, 14)
        .frame(minHeight: 48)
    }
}

/// One job: its outcome, the warnings read out of the vendor's output with the fix herder can
/// offer, then its terminal and its log to copy.
struct ActivityDetail: View {
    let fleet: Fleet
    let key: JobKey
    let back: () -> Void
    @Environment(\.openURL) private var openURL

    var body: some View {
        let progress = fleet.progress(key)
        let text = JobText(fleet: fleet, key: key, progress: progress)
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 10) {
                Button(action: back) {
                    Label("Activity", systemImage: "chevron.left").font(.subheadline.weight(.medium))
                        .foregroundStyle(Theme.secondary).contentShape(.rect)
                }
                .buttonStyle(.plain)
                Spacer()
                if let connection = fleet.activity[key]?.connection, progress.group != .queued {
                    PillButton(title: "Copy log", symbol: "doc.on.doc") { Clipboard.string = connection.log }
                }
            }
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Text(text.title).font(.headline).foregroundStyle(Theme.text)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 8)
                JobBadge(progress: progress)
            }
            TranscriptText(text.detail, style: .subheadline, color: progress.failed ? Theme.failure : Theme.secondary)
            ForEach(text.warnings) { WarningCard(warning: $0) }
            actions(progress)
            if let connection = fleet.activity[key]?.connection, progress.group != .queued {
                TerminalSurface(connection: connection, client: fleet.client, sessionId: nil)
                    .frame(minHeight: 260, maxHeight: 360)
                    .background(.black, in: .rect(cornerRadius: Theme.corner))
                    .clipShape(.rect(cornerRadius: Theme.corner))
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier("job-terminal")
            }
        }
        .padding(14)
        .layer(.sheet)
    }

    @ViewBuilder private func actions(_ progress: JobProgress) -> some View {
        switch (key.kind, progress) {
        case (_, .approve(let device)):
            HStack(spacing: 10) {
                TranscriptText(device.userCode, style: .footnote, monospaced: true)
                PillButton(title: "Approve in the browser", prominent: true) {
                    if let url = URL(string: device.verificationUrl) { openURL(url) }
                }
            }
        case (.add, .running), (.relogin, .running):
            HStack(spacing: 10) {
                Text("Finish the provider’s login in the terminal below.").font(.footnote).foregroundStyle(Theme.tertiary)
                Spacer()
                CancelLoginButton(fleet: fleet, key: key)
            }
        case (_, .needsNewerHerder):
            VaultInstallCommand()
        case (.updateHerder, .waitingForTurns):
            HStack(spacing: 10) {
                Text("Updating now ends the running turns.").font(.footnote).foregroundStyle(Theme.tertiary)
                Spacer()
                PillButton(title: "Update now") { fleet.updateHerder(on: key.hostId, when: .now) }
            }
        case (_, .failed):
            HStack(spacing: 8) {
                PillButton(title: "Retry", symbol: "arrow.clockwise", prominent: true) { fleet.retry(key) }
                if fleet.canFix(key) {
                    PillButton(title: "Fix with AI", symbol: "sparkles") { Task { await fleet.fix(key) } }
                }
            }
        default:
            EmptyView()
        }
    }
}

/// A warning the vendor printed, in amber, with its fix as a command to copy and run there.
struct WarningCard: View {
    let warning: VendorWarning

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Theme.accent)
            VStack(alignment: .leading, spacing: 4) {
                Text(warning.title).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                Text(warning.detail).font(.caption).foregroundStyle(Theme.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if let fix = warning.fix {
                    HStack(spacing: 6) {
                        TranscriptText(fix, style: .caption, monospaced: true)
                        CopyButton(text: fix)
                    }
                    Text("Run it on the machine to fix this.").font(.caption2).foregroundStyle(Theme.tertiary)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(12)
        .background(Theme.accent.opacity(0.08), in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.accent.opacity(0.5)))
    }
}

/// A job's title, detail line and warnings.
struct JobText {
    let machine: String
    let title: String
    let detail: String
    var warnings: [VendorWarning] = []
}

extension JobText {
    /// From what the fleet knows of the job.
    @MainActor init(fleet: Fleet, key: JobKey, progress: JobProgress) {
        let job = fleet.activity[key]
        let found = fleet.machines.first { $0.hostId == key.hostId }
        let machine = found?.name ?? key.hostId
        let provider: Provider = switch key.kind {
        case .install(let provider): provider
        case .add: job?.connection?.account?.provider ?? ""
        case .relogin(let id): found?.accounts.first { $0.accountId == id }?.provider ?? ""
        case .awsSignIn, .vault, .updateHerder: ""
        }
        // Read once the installer has ended: its output is whole then, and it is not read again
        // on every redraw while it streams.
        let warnings = if key.isInstall, progress.finished, let log = job?.connection?.log { VendorWarning.read(log, provider: provider) }
            else { [VendorWarning]() }
        self.init(machine: machine,
                  title: key.title(machine: machine, provider: provider, progress: progress, updating: job?.before != nil),
                  detail: progress.detail(machine: machine, before: job?.before), warnings: warnings)
    }
}

extension JobKey {
    /// The row's id for UI tests: `job-install-claude-devbox`.
    func accessibilityId(machine: String) -> String {
        switch kind {
        case .install(let provider): "job-install-\(provider)-\(machine)"
        case .add(let id): "job-add-\(id)-\(machine)"
        case .relogin(let id): "job-relogin-\(id)-\(machine)"
        case .awsSignIn(let profile): "job-aws-\(profile)-\(machine)"
        case .vault: "job-vault-\(machine)"
        case .updateHerder: "job-herder-\(machine)"
        }
    }
}

/// A job's state as a glyph: a spinner while it runs, amber when it needs the user.
struct JobGlyph: View {
    let progress: JobProgress

    var body: some View {
        Group {
            switch progress.group {
            case .needsYou: Image(systemName: "exclamationmark.circle.fill").foregroundStyle(Theme.accent)
            case .running:
                Image(systemName: "circle.lefthalf.filled").foregroundStyle(Theme.running).symbolEffect(.pulse)
            case .queued: Image(systemName: "clock").foregroundStyle(Theme.tertiary)
            case .done:
                progress.failed ? Image(systemName: "xmark.circle.fill").foregroundStyle(Theme.failure)
                    : progress == .ended ? Image(systemName: "minus.circle").foregroundStyle(Theme.tertiary)
                    : Image(systemName: "checkmark.circle.fill").foregroundStyle(Theme.success)
            }
        }
        .frame(width: 20)
    }
}

/// A job's progress as a small coloured label.
struct JobBadge: View {
    let progress: JobProgress

    var body: some View {
        HStack(spacing: 5) {
            if progress.group == .running {
                ProgressView().controlSize(.mini)
            } else {
                Circle().fill(progress.tint).frame(width: 6, height: 6)
            }
            Text(progress.label)
        }
        .font(.caption.weight(.medium))
        .foregroundStyle(progress.tint)
        .lineLimit(1)
        .fixedSize()
    }
}

extension JobProgress {
    var tint: Color {
        switch self {
        case .queued, .ended: Theme.tertiary
        case .starting, .installing, .settingUpVault, .claimingVault, .waitingForTurns, .downloading, .restarting: Theme.running
        case .running, .approve: Theme.accent
        case .added, .signedIn, .installed, .updated, .upToDate, .vaultReady: Theme.success
        case .failed, .needsNewerHerder: Theme.failure
        }
    }
}

/// Cancels a running login, once asked to confirm: closes its terminal on the machine, which
/// hangs up the provider's login, so it adds no account.
struct CancelLoginButton: View {
    let fleet: Fleet
    let key: JobKey
    @State private var confirming = false

    private var subject: String {
        switch key.kind {
        case .add(let id), .relogin(let id): id
        case .install(let provider): ModelCatalog.providerName(provider)
        case .awsSignIn(let profile): profile
        case .vault: "the vault"
        case .updateHerder: "herder"
        }
    }

    var body: some View {
        PillButton(title: "Cancel Login") { confirming = true }
            // The terminal has an id once the machine opened it.
            .disabled(fleet.activity[key]?.connection?.terminalId == nil)
            .accessibilityLabel("Cancel Login \(subject)")
            .confirmationDialog("Cancel the login of \(subject)?", isPresented: $confirming, titleVisibility: .visible) {
                Button("Cancel Login", role: .destructive) {
                    Task { await cancel() }
                }
                Button("Keep Running", role: .cancel) {}
            } message: {
                Text(key.kind == .relogin(subject) ? "The account keeps its current login."
                     : "The machine closes the login’s terminal and adds no account.")
            }
    }

    private func cancel() async {
        do {
            try await fleet.cancelLogin(key)
        } catch {
            fleet.toast = .failure("Could not cancel the login of \(subject)", error)
        }
    }
}

/// The Activity list as a sheet, opened from an iPhone screen's toolbar.
struct ActivitySheet: View {
    let fleet: Fleet
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        ActivityPanel(fleet: fleet) { dismiss() }
            .frame(maxHeight: .infinity, alignment: .top)
            .layer(.sheet)
    }
}

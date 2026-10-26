import Foundation
import Herder

/// A failed job handed to an agent on its machine: what ran there, how it ended and what it
/// printed, as the first prompt of a session that finds the cause, fixes it and runs it again.
struct FixRequest: Equatable {
    /// What failed, as a sentence: "Installing Codex failed".
    let job: String
    let machine: String
    /// The machine's home directory, where the session runs.
    let home: String
    /// The command the job ran, as its terminal's first line printed it; none for a job that
    /// has no terminal.
    let command: String?
    let exitCode: Int32?
    /// What herder said went wrong.
    let failure: String
    /// The job's last `tailLines` lines of output, plain text, with anything like a secret
    /// redacted.
    let output: String

    /// How many lines of output the prompt and the card's details keep.
    static let tailLines = 200

    init(job: String, machine: String, home: String, log: String?, exitCode: Int32?, failure: String) {
        self.job = job
        self.machine = machine
        self.home = home
        self.exitCode = exitCode
        self.failure = failure
        let lines = (log ?? "").split(separator: "\n", omittingEmptySubsequences: false)
        command = lines.first { $0.hasPrefix("$ ") }.map { String($0.dropFirst(2)) }
        output = Self.redacted(Self.tail(log ?? ""))
    }

    /// The first prompt: the failure, then what to do about it. It names no account and holds
    /// no login token; the agent is told to leave the providers' logins alone.
    var prompt: String {
        var facts = ["- Machine: \(machine), home directory \(home). herder does not know its OS: check with `uname -a` first."]
        if let command { facts.append("- Command: `\(command)`") }
        if let exitCode { facts.append("- Exit code: \(exitCode)") }
        facts.append("- herder reported: \(failure)")
        var text = "\(job) on this machine. Find out why and fix it.\n\n" + facts.joined(separator: "\n")
        if !output.isEmpty {
            text += "\n\nIts output, the last \(Self.tailLines) lines at most:\n\n```\n\(output)\n```"
        }
        text += """


            Find the cause, then fix it with the smallest change that works. \
            \(command == nil ? "Then run the job again" : "Then run the command again") and check that it succeeds. \
            Report what was wrong, what you changed and how the second run went.

            Never read, print or copy the provider CLIs' login tokens or credential files \
            (such as ~/.claude, ~/.codex or ~/.config); they are not needed for this.
            """
        return text
    }

    /// The last `tailLines` lines of `log`, without its trailing blank lines.
    static func tail(_ log: String) -> String {
        var lines = log.split(separator: "\n", omittingEmptySubsequences: false)
        while let last = lines.last, last.trimmingCharacters(in: .whitespaces).isEmpty { lines.removeLast() }
        return lines.suffix(tailLines).joined(separator: "\n")
    }

    /// `text` with what looks like a token, key or password replaced by `[redacted]`: an
    /// installer may echo one from its environment or a config it read.
    static func redacted(_ text: String) -> String {
        text.replacing(#/(?i)\b(authorization:\s*bearer|bearer|[a-z_]*(?:token|secret|password|api[_-]?key))(\s*[:=]\s*|\s+)("?)[^\s"]+/#) {
            "\($0.output.1)\($0.output.2)\($0.output.3)[redacted]"
        }
        .replacing(#/\b(?:sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9]{20,}|xox[abp]-[A-Za-z0-9-]{10,})/#) { _ in "[redacted]" }
    }

    /// Whether Fix with AI is offered for a job: a failed install or update, herder update or
    /// vault setup, on a machine this device owns. Logins and sign-ins wait on the user, so
    /// a shell cannot fix them.
    static func offered(_ kind: JobKind, progress: JobProgress, role: Role?) -> Bool {
        guard role == .owner, case .failed = progress else { return false }
        switch kind {
        case .install, .updateHerder, .vault: return true
        case .add, .relogin, .awsSignIn: return false
        }
    }
}

extension Fleet {
    /// Whether Fix with AI is offered for the job `key`.
    func canFix(_ key: JobKey) -> Bool {
        let machine = machines.first { $0.hostId == key.hostId }
        return machine?.connection == .connected && FixRequest.offered(key.kind, progress: progress(key), role: machine?.role)
    }

    /// The account a fix runs on, on `hostId`: one whose CLI is installed there and that is
    /// signed in, of another provider than `avoiding`, picked as the machine picks one.
    func fixAccount(on hostId: HostId, avoiding provider: Provider?) -> Account? {
        guard let machine = machines.first(where: { $0.hostId == hostId }) else { return nil }
        let installed = Set(machine.providers.filter(\.installed).map(\.provider))
        return pick(machine.accounts.filter {
            $0.provider != provider && installed.contains($0.provider) && $0.needsLogin == nil
        })
    }

    /// The output the failed job `key` printed, its last lines, for the card's details.
    func failureOutput(_ key: JobKey) -> String {
        FixRequest.redacted(FixRequest.tail(activity[key]?.connection?.log ?? ""))
    }

    /// Starts a session on the failed job `key`'s machine, in its home directory, on an account
    /// that works there, prompted with the failure; then shows it. A failure to start is toasted.
    func fix(_ key: JobKey) async {
        let machine = machines.first { $0.hostId == key.hostId }
        let name = machine?.name ?? key.hostId
        let avoiding: Provider? = if case .install(let provider) = key.kind { provider } else { nil }
        guard let account = fixAccount(on: key.hostId, avoiding: avoiding) else {
            toast = Toast(text: "\(name) has no signed-in account to fix this with", failed: true)
            return
        }
        guard case .failed(let failure) = progress(key) else { return }
        do {
            let home = try await listDirectory("~/", on: key.hostId).path
            let job = activity[key]
            let exitCode: Int32? = if case .exited(let code) = job?.connection?.state { code } else { nil }
            let request = FixRequest(job: fixTitle(key), machine: name, home: home, log: job?.connection?.log,
                                     exitCode: exitCode, failure: failure)
            let session = try await createSession(on: key.hostId, repo: home, projectId: nil, accountId: account.accountId,
                                                  model: "", mode: .fullAccess, prompt: request.prompt)
            revealed = session
        } catch {
            toast = .failure("Could not start a session on \(name) to fix this", error)
        }
    }

    /// What the job `key` did, as the prompt's first sentence says it failed.
    private func fixTitle(_ key: JobKey) -> String {
        switch key.kind {
        case .install(let provider):
            let name = ModelCatalog.providerName(provider)
            return activity[key]?.before == nil ? "Installing the \(name) CLI failed" : "Updating the \(name) CLI failed"
        case .updateHerder: return "Updating herder failed"
        case .vault: return "Setting up a herder vault failed"
        case .add, .relogin, .awsSignIn: return "A login failed"
        }
    }
}

import Herder
import SwiftUI

/// A failed turn, or the same failure repeated with no turn completed between: while it stands
/// the transcript shows it as a card with its fix, and once it is over, as a quiet line.
struct Failure: Hashable {
    enum Resolution: Hashable {
        /// A later turn completed.
        case resumed
        /// The account was logged in again, and what it held runs.
        case signedIn
        /// The session moved to another account or provider.
        case switched
        /// A different failure came after it.
        case superseded
    }

    /// The first failure's event, which the card keeps as it repeats.
    let id: UInt64
    /// The last turn that failed this way.
    var turnId: TurnId
    let error: TurnError
    /// How many turns failed this way in a row.
    var count = 1
    /// What the last of those turns was asked, to send again.
    var prompt: String?
    /// The provider and account the turn failed on.
    let provider: Provider?
    let accountId: AccountId?
    var resolution: Resolution?

    /// Whether `error` is this failure again: of the same class, saying the same in a line.
    func repeats(_ error: TurnError) -> Bool {
        error.class == self.error.class && TurnFailure.summary(error) == TurnFailure.summary(self.error)
    }

    /// The quiet line the failure reads as once it is over, "×N" when it repeated.
    var line: String {
        let text = switch (error.class, resolution) {
        case (.auth, .signedIn), (.auth, .resumed): "Signed in again · resumed"
        case (_, .switched): "Turn failed: \(TurnFailure.summary(error)) · continued on another account"
        default: "Turn failed: \(TurnFailure.summary(error))"
        }
        return count > 1 ? "\(text) ×\(count)" : text
    }
}

/// What a standing failure's card says and offers, from the failure and where the session is.
struct FailureCard: Equatable {
    enum Kind: Equatable { case auth, limit, problem }

    enum Action: Equatable {
        /// Logs the account in again, as an Activity job.
        case signIn
        /// Moves the session to the provider's account with the most room.
        case continueOn(AccountId, label: String)
        /// The machine runs the turn again once the limit resets.
        case wait(until: Date)
        /// Sends the failed turn's prompt again.
        case retry
        /// Shows the error as the provider said it.
        case details
    }

    /// Where the session stands, as far as the card goes.
    struct Context {
        var status: SessionStatus
        /// When the machine runs the turn again, while it waits for a limit to reset.
        var retryAt: Date?
        /// How many messages wait for the account to work again.
        var held = 0
        /// The accounts on the session's machine.
        var accounts: [Account] = []
        var machine: String
        /// Whether the user may log an account in, which takes a terminal: owners only.
        var canSignIn: Bool
    }

    let kind: Kind
    let title: String
    let message: String
    let actions: [Action]

    init(_ failure: Failure, _ context: Context) {
        let provider = failure.provider.map(ModelCatalog.providerName) ?? "The provider"
        let account = context.accounts.first { $0.accountId == failure.accountId }
        let name = account.map { $0.label.isEmpty ? $0.accountId : $0.label } ?? failure.accountId ?? "its"
        let other = Self.alternative(to: failure.accountId, provider: failure.provider, in: context.accounts)
            .map { Action.continueOn($0.accountId, label: $0.label.isEmpty ? $0.accountId : $0.label) }
        switch failure.error.class {
        case .auth:
            kind = .auth
            title = "\(provider) signed out the \(name) account on \(context.machine)"
            let held = switch context.held {
            case 0: "Messages you send now wait until the account works again."
            case 1: "Your message is held. It sends as soon as the account works again."
            default: "Your \(context.held) messages are held. They send as soon as the account works again."
            }
            message = context.canSignIn ? held : held + " Ask the machine’s owner to sign it in again."
            actions = (context.canSignIn ? [.signIn] : []) + [other, .details].compactMap { $0 }
        case .limitReached:
            kind = .limit
            title = "\(provider) usage limit reached on the \(name) account"
            if let at = context.retryAt, context.status == .waitingForCapacity {
                message = "It resets at \(Self.time(at)). Your message sends then, or continue on another account now."
                actions = [.wait(until: at), other, .details].compactMap { $0 }
            } else {
                let reset = TurnFailure.summary(failure.error).components(separatedBy: " · ").dropFirst().first
                message = reset.map { "It \($0)." } ?? "It resets later; the provider did not say when."
                actions = [failure.prompt == nil ? nil : .retry, other, .details].compactMap { $0 }
            }
        case .transient, .fatal:
            kind = .problem
            title = failure.error.class == .transient ? "The turn failed, for now" : "The turn failed"
            message = Self.firstSentence(TurnFailure.summary(failure.error))
            actions = (failure.prompt == nil ? [] : [.retry]) + [.details]
        }
    }

    /// The provider's other account with the most room: logged in, not used up, a fallback one
    /// only when no other has room.
    static func alternative(to accountId: AccountId?, provider: Provider?, in accounts: [Account]) -> Account? {
        let candidates = accounts.filter { account in
            account.provider == provider && account.accountId != accountId && account.needsLogin == nil
                && busiest(account) < 100
        }
        let rotating = candidates.filter { !$0.fallback }
        return (rotating.isEmpty ? candidates : rotating).min { busiest($0) < busiest($1) }
    }

    private static func busiest(_ account: Account) -> Double {
        account.usage.map(\.usedPercent).max() ?? 0
    }

    /// The text up to its first full stop, with it.
    static func firstSentence(_ text: String) -> String {
        guard let end = text.range(of: ". ") else { return text }
        return String(text[..<end.lowerBound]) + "."
    }

    static func time(_ date: Date) -> String {
        date.formatted(date: Calendar.current.isDateInToday(date) ? .omitted : .abbreviated, time: .shortened)
    }
}

/// A failure in the transcript: a card with its fix while it stands, then a quiet line.
struct FailureBlock: View {
    let failure: Failure
    let fleet: Fleet
    let key: SessionKey

    var body: some View {
        if failure.resolution == nil, let model = fleet.session(key), model.state != .archived {
            FailureCardView(failure: failure, card: FailureCard(failure, context(model)), fleet: fleet, key: key)
        } else {
            NoticeLine(notice: Notice(id: failure.id, text: failure.line,
                                      tone: failure.resolution == nil || failure.error.class != .auth ? .error : .info,
                                      detail: failure.error.message))
        }
    }

    private func context(_ model: SessionModel) -> FailureCard.Context {
        let machine = fleet.machines.first { $0.hostId == key.hostId }
        return FailureCard.Context(
            status: model.status, retryAt: model.retryAt,
            held: Transcript.held(model, queue: fleet.queue(of: key)).count,
            accounts: machine?.accounts ?? [], machine: fleet.machineName(key.hostId, of: key),
            canSignIn: machine?.role == .owner && machine?.connection == .connected)
    }
}

/// A standing failure's card: what happened, then its fixes as buttons.
struct FailureCardView: View {
    let failure: Failure
    let card: FailureCard
    let fleet: Fleet
    let key: SessionKey
    @State private var details = false
    /// Waiting for the reset was chosen: the card gives way to a line.
    @State private var waiting = false

    var body: some View {
        if waiting, case .wait(let at)? = card.actions.first(where: { if case .wait = $0 { true } else { false } }) {
            NoticeLine(notice: Notice(id: failure.id, text: "Usage limit reached · sends when it resets at \(FailureCard.time(at))",
                                      tone: .info, detail: failure.error.message))
        } else {
            content
        }
    }

    private var tint: Color { card.kind == .problem ? Theme.failure : Theme.accent }

    private var content: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: card.kind == .limit ? "hourglass" : "exclamationmark.circle.fill")
                    .font(.subheadline.weight(.bold))
                    .foregroundStyle(tint)
                VStack(alignment: .leading, spacing: 3) {
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text(card.title).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                            .fixedSize(horizontal: false, vertical: true)
                        if failure.count > 1 {
                            Text("×\(failure.count)").font(.caption.weight(.semibold)).monospacedDigit()
                                .foregroundStyle(Theme.secondary)
                                .accessibilityLabel("\(failure.count) times")
                        }
                    }
                    Text(card.message).font(.footnote).foregroundStyle(Theme.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            FlowLayout(spacing: 8) {
                ForEach(Array(card.actions.enumerated()), id: \.offset) { _, action in button(action) }
            }
            if details {
                Text(failure.error.message)
                    .font(Theme.monoSmall).foregroundStyle(Theme.secondary)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
                    .background(Theme.background, in: .rect(cornerRadius: 8))
            }
            if let refusal = fleet.refusals[key] {
                Text(refusal).font(.footnote).foregroundStyle(Theme.failure)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(tint.opacity(0.7), lineWidth: 1.5))
        .accessibilityIdentifier("failure-card")
    }

    @ViewBuilder private func button(_ action: FailureCard.Action) -> some View {
        switch action {
        case .signIn:
            let job = failure.accountId.map { JobKey(hostId: key.hostId, kind: .relogin($0)) }
            let signingIn = job.map { fleet.activity[$0] != nil && fleet.activity.phase($0) != .ended } ?? false
            PillButton(title: signingIn ? "Signing in… see Activity" : "Sign in again", prominent: true) {
                guard let accountId = failure.accountId else { return }
                fleet.relogin(accountId, on: key.hostId)
            }
            .disabled(signingIn)
            .help("Logs the account in again in Activity; the held messages send once it works")
        case .continueOn(let accountId, let label):
            PillButton(title: "Continue on \(label) account") {
                guard let account = fleet.machines.first(where: { $0.hostId == key.hostId })?
                    .accounts.first(where: { $0.accountId == accountId }) else { return }
                let resend = card.kind == .limit ? failure.prompt : nil
                Task {
                    await fleet.switchSession(key, to: account, model: "")
                    // The switch drops the turn waiting for the reset; it runs on the new account.
                    if let resend, fleet.refusals[key] == nil { await fleet.submit(resend, to: key) }
                }
            }
        case .wait:
            PillButton(title: "Wait", prominent: true) { waiting = true }
                .help("The machine sends the message again once the limit resets")
        case .retry:
            PillButton(title: "Retry", prominent: true) {
                guard let prompt = failure.prompt else { return }
                Task { await fleet.submit(prompt, to: key) }
            }
        case .details:
            PillButton(title: details ? "Hide Details" : "Details") { details.toggle() }
        }
    }
}

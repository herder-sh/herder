import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct VaultSetupTests {
    /// A machine's side of the setup, and this device's claim and renaming, recorded.
    private final class Recorder {
        var sent: [CommandBody] = []
        var claims: [HerderKit.ApprovalRequest] = []
        var renamed: [(HostId, String)] = []
        var steps: [VaultSetupStep] = []
    }

    private func setup(_ recorder: Recorder, answer: @escaping () throws -> CommandResult,
                       claim: @escaping () throws -> HostId = { "vault-1" }) -> VaultSetup {
        VaultSetup(
            send: { command in
                recorder.sent.append(command)
                return try answer()
            },
            claim: { request in
                recorder.claims.append(request)
                return try claim()
            },
            rename: { hostId, name in recorder.renamed.append((hostId, name)) })
    }

    private let fingerprint = String(repeating: "ab", count: 32)
    private var answer: CommandResult {
        .ownershipClaim(code: "K7Q-4PD", fingerprint: fingerprint, addresses: ["10.0.0.5:7448"],
                       expiresAt: "2026-10-26T12:00:00Z")
    }

    @Test func installingClaimsTheVaultWithYourPasskeyAndNamesItAfterTheMachine() async throws {
        let recorder = Recorder()
        let answer = answer
        await setup(recorder, answer: { answer }).run(machine: "devbox") { recorder.steps.append($0) }
        #expect(recorder.sent == [.installVault])
        #expect(recorder.steps == [.setting, .claiming, .ready("devbox’s vault")])
        // The vault's claim is approved here, with no code typed.
        #expect(recorder.claims == [HerderKit.ApprovalRequest(kind: .machine, name: "devbox’s vault", code: "K7Q-4PD",
                                                    fingerprint: fingerprint, hosts: ["10.0.0.5:7448"])])
        #expect(recorder.renamed.map(\.0) == ["vault-1"] && recorder.renamed.map(\.1) == ["devbox’s vault"])
    }

    @Test func aRefusalOrAFailedClaimEndsTheJobWithWhy() async {
        let refused = Recorder()
        await setup(refused, answer: {
            throw HerderError.Rejected(info: ErrorInfo(code: .conflict, message: "a vault is installed on this machine already"))
        }).run(machine: "devbox") { refused.steps.append($0) }
        #expect(refused.steps == [.setting, .failed("a vault is installed on this machine already")])
        #expect(refused.claims.isEmpty)

        let failed = Recorder()
        let answer = answer
        await setup(failed, answer: { answer }, claim: { throw HerderError.Local(detail: "no address answered") })
            .run(machine: "devbox") { failed.steps.append($0) }
        #expect(failed.steps.last == .failed("no address answered"))
        #expect(failed.renamed.isEmpty)
    }

    @Test func closingTheApprovalLeavesTheVaultToClaimByHand() async {
        let recorder = Recorder()
        let answer = answer
        await setup(recorder, answer: { answer }, claim: { throw CancellationError() })
            .run(machine: "devbox") { recorder.steps.append($0) }
        guard case .failed(let why) = recorder.steps.last else { Issue.record("not failed"); return }
        #expect(why.contains(PairKind.command))
        #expect(recorder.renamed.isEmpty)
    }

    @Test func anOlderHerderLeavesTheCommandToRunByHand() async {
        let recorder = Recorder()
        await setup(recorder, answer: {
            throw HerderError.Rejected(info: ErrorInfo(
                code: .badRequest, message: "invalid message: unknown variant `install_vault`, expected one of `subscribe`"))
        }).run(machine: "devbox") { recorder.steps.append($0) }
        #expect(recorder.steps.last == .needsNewerHerder)
        let progress = JobProgress(vault: .needsNewerHerder)
        #expect(progress.failed && progress.finished)
        #expect(progress.detail(machine: "devbox", before: nil).contains(VaultChoices.installCommand))
    }

    @Test func theJobRunsThenEndsReadyOrFailed() {
        #expect(JobProgress(vault: nil) == .starting)
        #expect(JobProgress(vault: .setting).group == .running)
        #expect(JobProgress(vault: .claiming).group == .running)
        let ready = JobProgress(vault: .ready("devbox’s vault"))
        #expect(ready.finished && !ready.failed)
        let key = JobKey(hostId: "devbox", kind: .vault)
        #expect(key.title(machine: "devbox", provider: "", progress: ready, updating: false) == "Ran devbox’s vault")
        #expect(key.title(machine: "devbox", provider: "", progress: .settingUpVault, updating: false)
            == "Run a vault on devbox")
        #expect(JobProgress(vault: .failed("nope")) == .failed("nope"))
    }

    @Test func aClaimWaitsForItsApproval() async throws {
        let fleet = try #require(Self.fleet())
        let request = HerderKit.ApprovalRequest(kind: .machine, name: "devbox’s vault", code: "K7Q-4PD", fingerprint: fingerprint,
                                      hosts: ["10.0.0.5:7448"])
        let approved = Task { try await fleet.claim(request) }
        #expect(await eventually { fleet.claiming == request })
        fleet.settleClaim(request, .success("vault-1"))
        #expect(try await approved.value == "vault-1")

        // The sheet closing unapproved ends it; settling again does nothing.
        let closed = Task { try await fleet.claim(request) }
        #expect(await eventually { fleet.claims[request] != nil })
        fleet.settleClaim(request, .failure(CancellationError()))
        fleet.settleClaim(request, .success("vault-1"))
        await #expect(throws: CancellationError.self) { try await closed.value }
    }

    @Test func activityKeepsAVaultSetupWithoutATerminal() {
        var activity = Activity()
        let key = JobKey(hostId: "devbox", kind: .vault)
        let queued = activity.enqueue(key, nil)
        #expect(queued)
        activity.start(key, before: nil)
        // It runs already, so no terminal is opened for it.
        #expect(activity.startable.isEmpty && activity.phase(key) == .running)
        let again = activity.enqueue(key, nil)
        #expect(!again)
        activity.setVault(key, .claiming)
        #expect(activity[key]?.vault == .claiming)
        activity.end(key)
        #expect(activity.phase(key) == .ended)
    }

    private static func fleet() -> Fleet? {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else { return nil }
        return fleet
    }
}

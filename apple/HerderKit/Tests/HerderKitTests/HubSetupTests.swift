import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct HubSetupTests {
    /// A machine's side of the setup, and this device's claim and renaming, recorded.
    private final class Recorder {
        var sent: [CommandBody] = []
        var claims: [HerderKit.ApprovalRequest] = []
        var renamed: [(HostId, String)] = []
        var steps: [HubSetupStep] = []
    }

    private func setup(_ recorder: Recorder, answer: @escaping () throws -> CommandResult,
                       claim: @escaping () throws -> HostId = { "hub-1" }) -> HubSetup {
        HubSetup(
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

    @Test func installingClaimsTheHubWithYourPasskeyAndNamesItAfterTheMachine() async throws {
        let recorder = Recorder()
        let answer = answer
        await setup(recorder, answer: { answer }).run(machine: "devbox") { recorder.steps.append($0) }
        #expect(recorder.sent == [.installHub])
        #expect(recorder.steps == [.setting, .claiming, .ready("devbox’s hub")])
        // The hub's claim is approved here, with no code typed.
        #expect(recorder.claims == [HerderKit.ApprovalRequest(kind: .machine, name: "devbox’s hub", code: "K7Q-4PD",
                                                    fingerprint: fingerprint, hosts: ["10.0.0.5:7448"])])
        #expect(recorder.renamed.map(\.0) == ["hub-1"] && recorder.renamed.map(\.1) == ["devbox’s hub"])
    }

    @Test func aRefusalOrAFailedClaimEndsTheJobWithWhy() async {
        let refused = Recorder()
        await setup(refused, answer: {
            throw HerderError.Rejected(info: ErrorInfo(code: .conflict, message: "a hub is installed on this machine already"))
        }).run(machine: "devbox") { refused.steps.append($0) }
        #expect(refused.steps == [.setting, .failed("a hub is installed on this machine already")])
        #expect(refused.claims.isEmpty)

        let failed = Recorder()
        let answer = answer
        await setup(failed, answer: { answer }, claim: { throw HerderError.Local(detail: "no address answered") })
            .run(machine: "devbox") { failed.steps.append($0) }
        #expect(failed.steps.last == .failed("no address answered"))
        #expect(failed.renamed.isEmpty)
    }

    @Test func closingTheApprovalLeavesTheHubToClaimByHand() async {
        let recorder = Recorder()
        let answer = answer
        await setup(recorder, answer: { answer }, claim: { throw CancellationError() })
            .run(machine: "devbox") { recorder.steps.append($0) }
        guard case .failed(let why) = recorder.steps.last else { Issue.record("not failed"); return }
        #expect(why.contains(MachineSource.claimCommand))
        #expect(recorder.renamed.isEmpty)
    }

    @Test func anOlderHerderLeavesTheCommandToRunByHand() async {
        let recorder = Recorder()
        await setup(recorder, answer: {
            throw HerderError.Rejected(info: ErrorInfo(
                code: .badRequest, message: "invalid message: unknown variant `install_hub`, expected one of `subscribe`"))
        }).run(machine: "devbox") { recorder.steps.append($0) }
        #expect(recorder.steps.last == .needsNewerHerder)
        let progress = JobProgress(hub: .needsNewerHerder)
        #expect(progress.failed && progress.finished)
        #expect(progress.detail(machine: "devbox", before: nil).contains(HubChoices.installCommand))
    }

    @Test func theJobRunsThenEndsReadyOrFailed() {
        #expect(JobProgress(hub: nil) == .starting)
        #expect(JobProgress(hub: .setting).group == .running)
        #expect(JobProgress(hub: .claiming).group == .running)
        let ready = JobProgress(hub: .ready("devbox’s hub"))
        #expect(ready.finished && !ready.failed)
        let key = JobKey(hostId: "devbox", kind: .hub)
        #expect(key.title(machine: "devbox", provider: "", progress: ready, updating: false) == "Ran devbox’s hub")
        #expect(key.title(machine: "devbox", provider: "", progress: .settingUpHub, updating: false)
            == "Run a hub on devbox")
        #expect(JobProgress(hub: .failed("nope")) == .failed("nope"))
    }

    @Test func aClaimWaitsForItsApproval() async throws {
        let fleet = try #require(Self.fleet())
        let request = HerderKit.ApprovalRequest(kind: .machine, name: "devbox’s hub", code: "K7Q-4PD", fingerprint: fingerprint,
                                      hosts: ["10.0.0.5:7448"])
        let approved = Task { try await fleet.claim(request) }
        #expect(await eventually { fleet.claiming == request })
        fleet.settleClaim(request, .success("hub-1"))
        #expect(try await approved.value == "hub-1")

        // The sheet closing unapproved ends it; settling again does nothing.
        let closed = Task { try await fleet.claim(request) }
        #expect(await eventually { fleet.claims[request] != nil })
        fleet.settleClaim(request, .failure(CancellationError()))
        fleet.settleClaim(request, .success("hub-1"))
        await #expect(throws: CancellationError.self) { try await closed.value }
    }

    @Test func activityKeepsAHubSetupWithoutATerminal() {
        var activity = Activity()
        let key = JobKey(hostId: "devbox", kind: .hub)
        let queued = activity.enqueue(key, nil)
        #expect(queued)
        activity.start(key, before: nil)
        // It runs already, so no terminal is opened for it.
        #expect(activity.startable.isEmpty && activity.phase(key) == .running)
        let again = activity.enqueue(key, nil)
        #expect(!again)
        activity.setHub(key, .claiming)
        #expect(activity[key]?.hub == .claiming)
        activity.end(key)
        #expect(activity.phase(key) == .ended)
    }

    private static func fleet() -> Fleet? {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else { return nil }
        return fleet
    }
}

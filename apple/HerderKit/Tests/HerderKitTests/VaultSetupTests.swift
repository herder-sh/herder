import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct VaultSetupTests {
    /// A machine's side of the setup, and this device's pairing and renaming, recorded.
    private final class Recorder {
        var sent: [CommandBody] = []
        var links: [String] = []
        var renamed: [(HostId, String)] = []
        var steps: [VaultSetupStep] = []
    }

    private func setup(_ recorder: Recorder, answer: @escaping () throws -> CommandResult,
                       pairing: [PairResult]? = nil) -> VaultSetup {
        VaultSetup(
            send: { command in
                recorder.sent.append(command)
                return try answer()
            },
            pair: { link in
                recorder.links.append(link)
                return pairing ?? [.paired(machine: machine("vault-1", name: "devbox", sessions: []))]
            },
            rename: { hostId, name in recorder.renamed.append((hostId, name)) })
    }

    private let pairing = CommandResult.devicePairing(
        code: "ABCDE-FGHJK", fingerprint: String(repeating: "ab", count: 32),
        addresses: ["10.0.0.5:7448"], expiresAt: "2026-10-26T12:00:00Z")

    @Test func installingPairsThisDeviceWithTheVaultAndNamesItAfterTheMachine() async throws {
        let recorder = Recorder()
        let pairing = pairing
        await setup(recorder, answer: { pairing }).run(machine: "devbox", installed: false) { recorder.steps.append($0) }
        #expect(recorder.sent == [.installVault])
        #expect(recorder.steps == [.setting(install: true), .pairing, .ready("devbox vault")])
        // The code the machine minted is redeemed here, with no code typed.
        let link = try #require(recorder.links.first)
        let uri = try parsePairingUri(link: link)
        #expect(uri.code == "ABCDE-FGHJK" && uri.hosts == ["10.0.0.5:7448"])
        #expect(uri.fingerprint == String(repeating: "ab", count: 32))
        #expect(recorder.renamed.map(\.0) == ["vault-1"] && recorder.renamed.map(\.1) == ["devbox vault"])
    }

    @Test func aVaultInstalledByHandIsPairedAndLinked() async {
        let recorder = Recorder()
        let pairing = pairing
        await setup(recorder, answer: { pairing }).run(machine: "devbox", installed: true) { recorder.steps.append($0) }
        #expect(recorder.sent == [.linkLocalVault])
        #expect(recorder.steps == [.setting(install: false), .pairing, .ready("devbox vault")])
    }

    @Test func aRefusalOrAFailedPairingEndsTheJobWithWhy() async {
        let refused = Recorder()
        await setup(refused, answer: {
            throw HerderError.Rejected(info: ErrorInfo(code: .conflict, message: "a vault is installed on this machine already"))
        }).run(machine: "devbox", installed: false) { refused.steps.append($0) }
        #expect(refused.steps == [.setting(install: true), .failed("a vault is installed on this machine already")])
        #expect(refused.links.isEmpty)

        let unpaired = Recorder()
        let pairing = pairing
        await setup(unpaired, answer: { pairing }, pairing: [.failed(addresses: ["10.0.0.5:7448"], error: "no address answered")])
            .run(machine: "devbox", installed: false) { unpaired.steps.append($0) }
        #expect(unpaired.steps.last == .failed("Pairing failed: no address answered"))
        #expect(unpaired.renamed.isEmpty)
    }

    @Test func anOlderHerderLeavesTheCommandToRunByHand() async {
        let recorder = Recorder()
        await setup(recorder, answer: {
            throw HerderError.Rejected(info: ErrorInfo(
                code: .badRequest, message: "invalid message: unknown variant `install_vault`, expected one of `subscribe`"))
        }).run(machine: "devbox", installed: false) { recorder.steps.append($0) }
        #expect(recorder.steps.last == .needsNewerHerder)
        let progress = JobProgress(vault: .needsNewerHerder)
        #expect(progress.failed && progress.finished)
        #expect(progress.detail(machine: "devbox", before: nil).contains(VaultChoices.installCommand))
    }

    @Test func theJobRunsThenEndsReadyOrFailed() {
        #expect(JobProgress(vault: nil) == .starting)
        #expect(JobProgress(vault: .setting(install: true)).group == .running)
        #expect(JobProgress(vault: .pairing).group == .running)
        let ready = JobProgress(vault: .ready("devbox vault"))
        #expect(ready.finished && !ready.failed)
        let key = JobKey(hostId: "devbox", kind: .vault)
        #expect(key.title(machine: "devbox", provider: "", progress: ready, updating: false) == "Ran devbox vault")
        #expect(key.title(machine: "devbox", provider: "", progress: .settingUpVault(install: true), updating: false)
            == "Run a vault on devbox")
        #expect(key.title(machine: "devbox", provider: "", progress: .settingUpVault(install: false), updating: false)
            == "Pair and link the vault on devbox")
        #expect(JobProgress(vault: .failed("nope")) == .failed("nope"))
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
        activity.setVault(key, .pairing)
        #expect(activity[key]?.vault == .pairing)
        activity.end(key)
        #expect(activity.phase(key) == .ended)
    }
}

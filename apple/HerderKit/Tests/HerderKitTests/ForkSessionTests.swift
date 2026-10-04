import Herder
@testable import HerderKit
import Testing

@MainActor
struct ForkSessionTests {
    let source = SessionKey(hostId: "source", sessionId: "original")

    func destination() -> Machine {
        var host = machine("destination", name: "Other Mac", sessions: [])
        host.accounts = [Account(accountId: "other-account", provider: "claude", label: "Personal", configDir: nil, usage: []),
                         Account(accountId: "codex", provider: "codex", label: "Codex", configDir: nil, usage: [])]
        return host
    }

    @Test func selectionRequiresConnectedOwnerAndMatchingProvider() {
        let flow = ForkSessionModel()
        var host = destination()
        flow.select(host, provider: "claude")
        #expect(flow.accountId == "other-account")
        #expect(flow.destination(in: [host], source: source, provider: "claude")?.hostId == host.hostId)
        host.connection = .disconnected(error: "offline")
        #expect(flow.destination(in: [host], source: source, provider: "claude") == nil)
        host.connection = .connected
        host.role = .member
        #expect(flow.destination(in: [host], source: source, provider: "claude") == nil)
        host.role = .owner
        flow.accountId = "codex"
        #expect(flow.destination(in: [host], source: source, provider: "claude") == nil)
        #expect(ForkSessionModel.ineligible(host, source: source, provider: "gemini") != nil)
        host.vault = VaultStatus(sessions: 0, events: 0, storageBytes: 0, hosts: [])
        #expect(ForkSessionModel.ineligible(host, source: source, provider: "claude") != nil)
        host.vault = nil
        host.hosts = [FleetHost(hostId: "replicated", hostName: "replicated", online: false, lastSeen: "")]
        #expect(ForkSessionModel.ineligible(host, source: source, provider: "claude") != nil)
        host.hosts = []
        host.hostId = source.hostId
        #expect(ForkSessionModel.ineligible(host, source: source, provider: "claude") == nil)
    }

    @Test func forkSendsToDestinationAndOpensReturnedSession() async {
        let flow = ForkSessionModel()
        let host = destination()
        flow.select(host, provider: "claude")
        await flow.fork(source: source, provider: "claude", machines: [host]) { hostId, command in
            #expect(hostId == "destination")
            #expect(command == .forkSession(sessionId: "original", accountId: "other-account"))
            return .sessionForked(sessionId: "copy", accountId: "other-account", forkedFrom: "original", fromHostId: "source")
        }
        #expect(flow.opened == SessionKey(hostId: "destination", sessionId: "copy"))
        #expect(flow.opened != source)
        #expect(flow.origin == ForkOrigin(sessionId: "original", hostId: "source"))
        #expect(!flow.working)
        #expect(flow.error == nil)
    }

    @Test func disconnectPreventsSubmissionAndRefusalKeepsOriginalOpen() async {
        let flow = ForkSessionModel()
        var host = destination()
        flow.select(host, provider: "claude")
        host.connection = .disconnected(error: "offline")
        await flow.fork(source: source, provider: "claude", machines: [host]) { _, _ in
            Issue.record("sent to a disconnected machine")
            return .applied
        }
        #expect(flow.opened == nil)
        #expect(flow.error != nil)
        host.connection = .connected
        await flow.fork(source: source, provider: "claude", machines: [host]) { _, _ in
            throw HerderError.Local(detail: "No checkpoint in vault")
        }
        #expect(flow.opened == nil)
        #expect(flow.error?.contains("No checkpoint") == true)
        #expect(!flow.working)
        await flow.fork(source: source, provider: "claude", machines: [host]) { _, _ in .applied }
        #expect(flow.opened == nil)
        #expect(flow.error?.contains("did not return") == true)
    }
}

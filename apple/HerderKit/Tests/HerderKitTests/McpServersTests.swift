import Herder
@testable import HerderKit
import Testing

struct McpServersTests {
    @MainActor
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func anOwnerAddsTogglesAndRemovesMcpServers() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let machine = try await fleet.pair(daemon)
        try await fleet.client.synced(hostId: machine.hostId)
        #expect(try await fleet.mcpServers(on: machine.hostId).isEmpty)

        let docs = McpServerSettings(
            name: "docs", enabled: true, project: nil,
            transport: .stdio(command: "docs-mcp", args: ["--stdio"], env: ["DOCS_HOST": "docs.example.com"],
                              secrets: ["DOCS_TOKEN"]))
        let linear = McpServerSettings(
            name: "linear", enabled: true, project: nil,
            transport: .http(url: "https://mcp.linear.app/mcp", headers: ["Authorization": "Bearer t"]))
        try await fleet.setMcpServers([docs, linear], on: machine.hostId)
        #expect(try await fleet.mcpServers(on: machine.hostId) == [docs, linear])

        var off = linear
        off.enabled = false
        try await fleet.setMcpServers([off], on: machine.hostId)
        #expect(try await fleet.mcpServers(on: machine.hostId) == [off])

        do {
            var herder = docs
            herder.name = "herder"
            try await fleet.setMcpServers([herder], on: machine.hostId)
            Issue.record("a server named herder must be refused")
        } catch let error as HerderError {
            guard case .Rejected(let info) = error else { throw error }
            #expect(info.code == .badRequest)
        }
        try await fleet.setMcpServers([], on: machine.hostId)
        #expect(try await fleet.mcpServers(on: machine.hostId).isEmpty)
    }
}

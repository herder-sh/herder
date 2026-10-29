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
        let machine = try await fleet.signIn(to: daemon)
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

struct McpImportTests {
    @Test func claudeDesktopsConfigImportsEachServer() throws {
        let servers = try McpImport.parse("""
        {
          "mcpServers": {
            "filesystem": {
              "command": "npx",
              "args": ["-y", "@modelcontextprotocol/server-filesystem", "/Users/sam/Desktop"]
            },
            "github": {
              "command": "docker",
              "args": ["run", "-i", "--rm", "ghcr.io/github/github-mcp-server"],
              "env": {"GITHUB_HOST": "github.com", "PORT": 8080}
            }
          }
        }
        """)
        #expect(servers == [
            McpServerSettings(name: "filesystem", enabled: true, project: nil, transport: .stdio(
                command: "npx", args: ["-y", "@modelcontextprotocol/server-filesystem", "/Users/sam/Desktop"],
                env: [:], secrets: [])),
            McpServerSettings(name: "github", enabled: true, project: nil, transport: .stdio(
                command: "docker", args: ["run", "-i", "--rm", "ghcr.io/github/github-mcp-server"],
                env: ["GITHUB_HOST": "github.com", "PORT": "8080"], secrets: [])),
        ])
    }

    @Test func typeAndUrlPickTheTransport() throws {
        let servers = try McpImport.parse("""
        {"mcpServers": {
          "linear": {"type": "http", "url": "https://mcp.linear.app/mcp", "headers": {"Authorization": "Bearer t"}},
          "legacy": {"type": "sse", "url": "http://127.0.0.1:9000/sse"},
          "plain": {"url": "https://mcp.example.com/mcp"},
          "streamed": {"type": "streamable-http", "url": "https://mcp.example.com/stream"},
          "local": {"type": "stdio", "command": "uvx", "args": ["mcp-server-time"], "disabled": true}
        }}
        """)
        #expect(servers.map(\.name) == ["legacy", "linear", "local", "plain", "streamed"])
        #expect(servers[0].transport == .sse(url: "http://127.0.0.1:9000/sse", headers: [:]))
        #expect(servers[1].transport == .http(url: "https://mcp.linear.app/mcp", headers: ["Authorization": "Bearer t"]))
        #expect(servers[2].transport == .stdio(command: "uvx", args: ["mcp-server-time"], env: [:], secrets: []))
        #expect(!servers[2].enabled)
        // A url alone is streamable HTTP, as the daemon reads it.
        #expect(servers[3].transport == .http(url: "https://mcp.example.com/mcp", headers: [:]))
        #expect(servers[4].transport == .http(url: "https://mcp.example.com/stream", headers: [:]))
    }

    @Test func aReadmesFragmentAndVsCodesServersImportToo() throws {
        let fragment = try McpImport.parse("""
        "sentry": {"url": "https://mcp.sentry.dev/sse", "type": "sse"}
        """)
        #expect(fragment.map(\.name) == ["sentry"])
        let vscode = try McpImport.parse(#"{"servers": {"time": {"command": "uvx", "args": ["mcp-server-time"]}}}"#)
        #expect(vscode.map(\.name) == ["time"])
        let bare = try McpImport.parse(#"{"time": {"command": "uvx"}}"#)
        #expect(bare.map(\.name) == ["time"])
    }

    @Test func namesAreCleanedIntoOnesTheMachinesTake() throws {
        let servers = try McpImport.parse(#"""
        {"mcpServers": {"@acme/docs server": {"command": "docs"}, "herder": {"command": "other"},
                        "acme-docs-server": {"command": "third"}}}
        """#)
        #expect(servers.map(\.name) == ["acme-docs-server", "acme-docs-server-2", "herder-server"])
        #expect(servers.allSatisfy { McpServers.nameProblem($0.name, taken: []) == nil })
    }

    @Test func whatCannotBeImportedSaysWhy() {
        #expect(throws: McpImport.Failure.self) { try McpImport.parse("") }
        #expect(throws: McpImport.Failure.self) { try McpImport.parse("not json") }
        #expect(throws: McpImport.Failure(message: "ws has type websocket; herder takes stdio, http and sse.")) {
            try McpImport.parse(#"{"mcpServers": {"ws": {"type": "websocket", "url": "wss://x"}}}"#)
        }
        #expect(throws: McpImport.Failure(message: "empty has no command or url.")) {
            try McpImport.parse(#"{"mcpServers": {"empty": {}}}"#)
        }
        #expect(throws: McpImport.Failure.self) {
            try McpImport.parse(#"{"command": "npx", "args": []}"#)
        }
    }
}

struct McpValidationTests {
    private func draft(_ body: (inout McpServerDraft) -> Void) -> McpServerDraft {
        var draft = McpServerDraft()
        draft.name = "github"
        draft.command = "npx"
        body(&draft)
        return draft
    }

    @Test func namesFollowTheMachinesRules() {
        #expect(McpServers.nameProblem("github_2-x", taken: []) == nil)
        #expect(McpServers.nameProblem("", taken: []) != nil)
        #expect(McpServers.nameProblem("my server", taken: []) != nil)
        #expect(McpServers.nameProblem("ünicode", taken: []) != nil)
        #expect(McpServers.nameProblem(String(repeating: "a", count: 65), taken: []) != nil)
        #expect(McpServers.nameProblem("herder", taken: [])?.contains("herder’s own") == true)
        #expect(McpServers.nameProblem("linear", taken: ["linear"])?.contains("exists") == true)
    }

    @Test func eachTransportChecksItsOwnFields() {
        #expect(draft { _ in }.problem(taken: []) == nil)
        #expect(draft { $0.command = "" }.problem(taken: []) != nil)
        #expect(draft { $0.command = "bin/server" }.problem(taken: []) != nil)
        #expect(draft { $0.command = "~/bin/server" }.problem(taken: []) == nil)
        #expect(draft { $0.env = [McpPair(key: "A=B", value: "1")] }.problem(taken: []) != nil)
        #expect(draft { $0.env = [McpPair(key: "A", value: "1"), McpPair(key: "A", value: "2")] }.problem(taken: []) != nil)
        #expect(draft { $0.secrets = ["GITHUB_TOKEN", "work/JIRA_TOKEN"] }.problem(taken: []) == nil)
        #expect(draft { $0.secrets = ["github_token"] }.problem(taken: []) != nil)
        #expect(draft { $0.secrets = ["Work/TOKEN"] }.problem(taken: []) != nil)

        let http = { (body: (inout McpServerDraft) -> Void) in draft { $0.kind = .http; $0.url = "https://mcp.example.com"; body(&$0) } }
        #expect(http { _ in }.problem(taken: []) == nil)
        #expect(http { $0.url = "ftp://mcp.example.com" }.problem(taken: []) != nil)
        #expect(http { $0.url = "mcp.example.com" }.problem(taken: []) != nil)
        #expect(http { $0.headers = [McpPair(key: "Authorization", value: "Bearer t")] }.problem(taken: []) == nil)
        #expect(http { $0.headers = [McpPair(key: "Bad:Name", value: "x")] }.problem(taken: []) != nil)
        // The stdio fields typed before switching are not sent, so they are not checked.
        #expect(http { $0.command = "" }.problem(taken: []) == nil)
    }

    @Test func aDraftSendsOnlyItsTransportsFields() {
        let server = McpServerSettings(name: "docs", enabled: false, project: "github.com/acme/api", transport: .stdio(
            command: "docs-mcp", args: ["--root", "/srv/my docs"], env: ["B": "2", "A": "1"], secrets: ["DOCS_TOKEN"]))
        var draft = McpServerDraft(server)
        #expect(draft.args == "--root\n/srv/my docs")
        #expect(draft.server == server)
        draft.kind = .sse
        draft.url = " https://docs.example.com/sse "
        #expect(draft.server.transport == .sse(url: "https://docs.example.com/sse", headers: [:]))
    }
}

struct McpCopyTests {
    private let linear = McpServerSettings(name: "linear", enabled: true, project: nil,
                                           transport: .http(url: "https://mcp.linear.app/mcp", headers: [:]))
    private let github = McpServerSettings(name: "github", enabled: true, project: nil,
                                           transport: .stdio(command: "npx", args: [], env: [:], secrets: []))

    @Test func mergeReplacesASameNamedServerInPlaceAndAppendsTheRest() {
        var newer = linear
        newer.enabled = false
        let sentry = McpServerSettings(name: "sentry", enabled: true, project: nil,
                                       transport: .sse(url: "https://mcp.sentry.dev/sse", headers: [:]))
        #expect(McpServers.merge([newer, sentry], into: [linear, github]) == [newer, github, sentry])
        #expect(McpServers.replaced([newer, sentry], in: [linear, github]) == ["linear"])
    }

    @MainActor
    @Test func addingToSeveralMachinesKeepsEachOnesOwnServers() async {
        var machines: [HostId: [McpServerSettings]] = ["devbox": [github, linear], "studio": [], "laptop": [linear]]
        let lists = McpServerLists(
            read: { host in
                guard let servers = machines[host] else { throw HerderError.Local(detail: "\(host) is gone") }
                return servers
            },
            write: { servers, host in
                if host == "laptop" { throw HerderError.Local(detail: "laptop refused") }
                machines[host] = servers
            })
        var docs = github
        docs.name = "docs"
        let failures = await lists.add([docs, linear], to: ["devbox", "studio", "laptop", "gone"])
        #expect(machines["devbox"] == [github, linear, docs])
        #expect(machines["studio"] == [docs, linear])
        // A machine that refuses keeps what it had, and says so.
        #expect(machines["laptop"] == [linear])
        #expect(Set(failures.keys) == ["laptop", "gone"])
        #expect(failures["laptop"]?.contains("laptop refused") == true)
        #expect(lists.servers(on: "studio") == [docs, linear])
    }
}

struct McpServerTableTests {
    private func server(_ name: String, enabled: Bool = true) -> McpServerSettings {
        McpServerSettings(name: name, enabled: enabled, project: nil, transport: .http(url: "https://\(name).example.com/mcp", headers: [:]))
    }

    @Test func aServerOnSeveralMachinesIsOneRowThatSaysWhereItIsOff() throws {
        let servers: [HostId: [McpServerSettings]] = [
            "a": [server("linear"), server("github")],
            "b": [server("linear", enabled: false)],
            "c": [server("sentry", enabled: false)],
        ]
        let table = McpServerTable([(hostId: "a", name: "devbox"), (hostId: "b", name: "studio"),
                                    (hostId: "c", name: "laptop"), (hostId: "d", name: "unknown")],
                                   servers: { servers[$0] })
        #expect(table.rows.map(\.name) == ["github", "linear", "sentry"])
        let linear = try #require(table.rows.first { $0.name == "linear" })
        #expect(linear.places.map(\.machine) == ["devbox", "studio"])
        #expect(linear.machines == "2 machines")
        #expect(!linear.enabled && linear.partlyOff)
        #expect(linear.status == "Off on studio")
        #expect(table.rows[0].machines == "devbox" && table.rows[0].status == "On")
        #expect(table.rows[2].status == "Off" && !table.rows[2].partlyOff)
    }
}

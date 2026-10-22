import Foundation
import Herder
import Observation

/// How a CLI reaches an MCP server, as the form picks it.
enum McpKind: String, CaseIterable, Hashable {
    case stdio, http, sse

    var title: String {
        switch self {
        case .stdio: "stdio"
        case .http: "HTTP"
        case .sse: "SSE"
        }
    }

    init(_ transport: McpTransport) {
        switch transport {
        case .stdio: self = .stdio
        case .http: self = .http
        case .sse: self = .sse
        }
    }
}

/// One line of a key and value editor: an environment variable or a header.
struct McpPair: Equatable, Identifiable {
    let id = UUID()
    var key = ""
    var value = ""

    static func == (left: Self, right: Self) -> Bool { left.key == right.key && left.value == right.value }

    /// The pairs of `map`, by key.
    static func list(_ map: [String: String]) -> [Self] {
        map.sorted { $0.key < $1.key }.map { McpPair(key: $0.key, value: $0.value) }
    }

    /// The pairs with a key, trimmed; a line left empty is no pair.
    static func map(_ pairs: [Self]) -> [String: String] {
        var map: [String: String] = [:]
        for pair in pairs {
            let key = pair.key.trimmingCharacters(in: .whitespaces)
            if !key.isEmpty { map[key] = pair.value }
        }
        return map
    }
}

/// An MCP server as the form has it: every transport's fields, so switching transport keeps
/// what was typed; `server` is what is sent, with only the chosen transport's.
struct McpServerDraft: Equatable {
    var name = ""
    var enabled = true
    var project: ProjectId?
    var kind = McpKind.stdio
    var command = ""
    /// The arguments, one per line, so one may hold spaces.
    var args = ""
    var env: [McpPair] = []
    var secrets: [String] = []
    var url = ""
    var headers: [McpPair] = []

    init() {}

    init(_ server: McpServerSettings) {
        name = server.name
        enabled = server.enabled
        project = server.project
        kind = McpKind(server.transport)
        switch server.transport {
        case .stdio(let command, let args, let env, let secrets):
            self.command = command
            self.args = args.joined(separator: "\n")
            self.env = McpPair.list(env)
            self.secrets = secrets
        case .http(let url, let headers), .sse(let url, let headers):
            self.url = url
            self.headers = McpPair.list(headers)
        }
    }

    var server: McpServerSettings {
        let transport: McpTransport = switch kind {
        case .stdio:
            .stdio(command: command.trimmingCharacters(in: .whitespaces),
                   args: args.split(separator: "\n", omittingEmptySubsequences: true).map(String.init),
                   env: McpPair.map(env), secrets: secrets)
        case .http: .http(url: url.trimmingCharacters(in: .whitespaces), headers: McpPair.map(headers))
        case .sse: .sse(url: url.trimmingCharacters(in: .whitespaces), headers: McpPair.map(headers))
        }
        return McpServerSettings(name: name.trimmingCharacters(in: .whitespaces), enabled: enabled,
                                 project: project, transport: transport)
    }

    /// What is wrong with the server, as the machine would refuse it; `taken` are the names
    /// the other servers there use.
    func problem(taken: [String]) -> String? {
        let server = server
        if let problem = McpServers.nameProblem(server.name, taken: taken) { return problem }
        switch server.transport {
        case .stdio(let command, _, _, let secrets):
            if command.isEmpty { return "Enter the command that runs the server." }
            if command.contains("/"), !AccountDraft.validPath(command) {
                return "Give the command as a name on PATH, an absolute path or a path starting with ~/."
            }
            if let key = McpServers.duplicate(env) { return "\(key) is set twice." }
            if let bad = env.map({ $0.key.trimmingCharacters(in: .whitespaces) })
                .first(where: { $0.contains("=") || $0.contains("\0") }) {
                return "\(bad) is not an environment variable name."
            }
            if let bad = secrets.first(where: { !McpServers.isValidSecretRef($0) }) {
                return "\(bad) is not a secret name: NAME or set/NAME, NAME in capitals, digits and underscores."
            }
            if Set(secrets).count < secrets.count { return "A secret is listed twice." }
        case .http(let url, _), .sse(let url, _):
            if url.isEmpty { return "Enter the server’s URL." }
            guard let parsed = URL(string: url), ["http", "https"].contains(parsed.scheme?.lowercased() ?? ""),
                  parsed.host?.isEmpty == false
            else { return "The URL must start with http:// or https://." }
            if let key = McpServers.duplicate(headers) { return "The \(key) header is set twice." }
            if let bad = headers.map({ $0.key.trimmingCharacters(in: .whitespaces) })
                .first(where: { !McpServers.isValidHeaderName($0) && !$0.isEmpty }) {
                return "\(bad) is not a header name."
            }
        }
        return nil
    }
}

/// The rules the machines check MCP servers by, and the conversions the screens use.
enum McpServers {
    /// What `herder` registers its own server as; no server of the user's may take it.
    static let reserved = "herder"

    /// Why `name` cannot name a server among `taken`: 1 to 64 ASCII letters, digits, `-` and
    /// `_`, unique, and not `herder`.
    static func nameProblem(_ name: String, taken: [String]) -> String? {
        if name.isEmpty { return "Enter a name." }
        guard (1...64).contains(name.utf8.count), name.utf8.allSatisfy({ byte in
            (65...90).contains(byte) || (97...122).contains(byte) || (48...57).contains(byte) || byte == 45 || byte == 95
        }) else { return "Names use letters, digits, - and _, at most 64." }
        if name == reserved { return "herder is the name of herder’s own server." }
        if taken.contains(name) { return "A server named \(name) exists on this machine." }
        return nil
    }

    /// `name` made into a valid server name: each other character becomes `-`, a scope's `@`
    /// goes, and `herder` gets a suffix.
    static func cleanName(_ name: String) -> String {
        let trimmed = name.trimmingCharacters(in: .whitespaces).trimmingCharacters(in: CharacterSet(charactersIn: "@"))
        var clean = String(trimmed.map { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-" || $0 == "_") ? $0 : "-" })
        clean = String(clean.prefix(64))
        if clean.isEmpty { return "server" }
        return clean == reserved ? "herder-server" : clean
    }

    /// Whether `ref` names a vault secret, as `NAME` or `set/NAME`.
    static func isValidSecretRef(_ ref: String) -> Bool {
        let parts = ref.split(separator: "/", maxSplits: 1, omittingEmptySubsequences: false).map(String.init)
        if parts.count == 2 {
            return VaultSecrets.setNameProblem(parts[0]) == nil && VaultSecrets.isValidSecretName(parts[1])
        }
        return VaultSecrets.isValidSecretName(ref)
    }

    /// Whether `name` is an HTTP header name: printable ASCII without `:`.
    static func isValidHeaderName(_ name: String) -> Bool {
        !name.isEmpty && name.utf8.allSatisfy { (33...126).contains($0) && $0 != 58 }
    }

    /// A key two lines both set.
    static func duplicate(_ pairs: [McpPair]) -> String? {
        var seen = Set<String>()
        for pair in pairs {
            let key = pair.key.trimmingCharacters(in: .whitespaces)
            if !key.isEmpty, !seen.insert(key).inserted { return key }
        }
        return nil
    }

    /// The program and its arguments, or the URL: what the row shows.
    static func target(_ transport: McpTransport) -> String {
        switch transport {
        case .stdio(let command, let args, _, _): ([command] + args).joined(separator: " ")
        case .http(let url, _), .sse(let url, _): url
        }
    }

    /// `servers` added to `existing`: one of the same name takes its place, the others go last.
    static func merge(_ servers: [McpServerSettings], into existing: [McpServerSettings]) -> [McpServerSettings] {
        var merged = existing
        for server in servers {
            if let index = merged.firstIndex(where: { $0.name == server.name }) {
                merged[index] = server
            } else {
                merged.append(server)
            }
        }
        return merged
    }

    /// The names of `servers` that `existing` has already, which `merge` replaces.
    static func replaced(_ servers: [McpServerSettings], in existing: [McpServerSettings]) -> [String] {
        servers.map(\.name).filter { name in existing.contains { $0.name == name } }
    }
}

/// Reads the `mcpServers` JSON that MCP servers' READMEs and Claude Desktop's config give.
enum McpImport {
    struct Failure: Error, Equatable, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    /// The servers in `text`: an object with `mcpServers` (or VS Code's `servers`) mapping each
    /// name to its entry, that map alone, or one entry that `"name": {…}` wraps. An entry has a
    /// `command` with `args` and `env`, or a `url` with `headers`; `type` picks `stdio`,
    /// `http` (also `streamable-http`) or `sse`, and a `url` alone is HTTP. Names are cleaned
    /// into ones the machines take; `disabled: true` adds the server turned off.
    static func parse(_ text: String) throws -> [McpServerSettings] {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { throw Failure(message: "Paste the JSON of one or more servers.") }
        let root: Any
        do {
            root = try JSONSerialization.jsonObject(with: Data(trimmed.utf8))
        } catch {
            // A README often shows the entry without its braces: `"name": { … }`.
            guard let wrapped = try? JSONSerialization.jsonObject(with: Data("{\(trimmed)}".utf8)) else {
                throw Failure(message: "This isn’t JSON. Paste the snippet with its braces.")
            }
            root = wrapped
        }
        guard let object = root as? [String: Any] else {
            throw Failure(message: "Expected a JSON object with mcpServers.")
        }
        let map = (object["mcpServers"] ?? object["servers"]).flatMap { $0 as? [String: Any] } ?? object
        if map.isEmpty { throw Failure(message: "The JSON lists no servers.") }
        if isEntry(map) { throw Failure(message: "Give the server a name: {\"mcpServers\": {\"name\": {…}}}.") }
        var servers: [McpServerSettings] = []
        for (name, value) in map.sorted(by: { $0.key < $1.key }) {
            guard let entry = value as? [String: Any] else {
                throw Failure(message: "\(name) is not a server entry.")
            }
            var clean = McpServers.cleanName(name)
            var n = 2
            while servers.contains(where: { $0.name == clean }) {
                clean = "\(McpServers.cleanName(name))-\(n)"
                n += 1
            }
            servers.append(try server(clean, entry))
        }
        return servers
    }

    private static func isEntry(_ map: [String: Any]) -> Bool {
        map["command"] is String || map["url"] is String
    }

    private static func server(_ name: String, _ entry: [String: Any]) throws -> McpServerSettings {
        let type = (entry["type"] as? String ?? entry["transport"] as? String)?.lowercased()
        let command = entry["command"] as? String
        let url = (entry["url"] ?? entry["serverUrl"]) as? String
        let kind: McpKind
        switch type {
        case "stdio": kind = .stdio
        case "http", "streamable-http", "streamablehttp", "streamable_http": kind = .http
        case "sse": kind = .sse
        case nil:
            if command != nil {
                kind = .stdio
            } else if url != nil {
                kind = .http
            } else {
                throw Failure(message: "\(name) has no command or url.")
            }
        case let other?: throw Failure(message: "\(name) has type \(other); herder takes stdio, http and sse.")
        }
        let transport: McpTransport
        switch kind {
        case .stdio:
            guard let command, !command.isEmpty else { throw Failure(message: "\(name) has no command.") }
            transport = .stdio(command: command, args: try strings(entry["args"], of: name, "args"),
                               env: try table(entry["env"], of: name, "env"), secrets: [])
        case .http, .sse:
            guard let url, !url.isEmpty else { throw Failure(message: "\(name) has no url.") }
            let headers = try table(entry["headers"], of: name, "headers")
            transport = kind == .http ? .http(url: url, headers: headers) : .sse(url: url, headers: headers)
        }
        return McpServerSettings(name: name, enabled: entry["disabled"] as? Bool != true, project: nil,
                                 transport: transport)
    }

    private static func strings(_ value: Any?, of name: String, _ field: String) throws -> [String] {
        guard let value else { return [] }
        guard let list = value as? [Any] else { throw Failure(message: "\(name)’s \(field) is not a list.") }
        return try list.map { item in
            if let text = item as? String { return text }
            if let number = item as? NSNumber { return number.stringValue }
            throw Failure(message: "\(name)’s \(field) holds something other than text.")
        }
    }

    private static func table(_ value: Any?, of name: String, _ field: String) throws -> [String: String] {
        guard let value else { return [:] }
        guard let map = value as? [String: Any] else { throw Failure(message: "\(name)’s \(field) is not an object.") }
        return try map.mapValues { item in
            if let text = item as? String { return text }
            if let number = item as? NSNumber { return number.stringValue }
            throw Failure(message: "\(name)’s \(field) holds something other than text.")
        }
    }
}

/// The MCP servers of the machines this device owns, as they last said, and each change sent
/// to them. Each change starts from what the machine has then, so one made from another device
/// meanwhile is kept.
@MainActor @Observable
final class McpServerLists {
    typealias Read = @MainActor (HostId) async throws -> [McpServerSettings]
    typealias Write = @MainActor ([McpServerSettings], HostId) async throws -> Void

    enum Load: Equatable {
        case asking
        case loaded([McpServerSettings])
        case failed(String)
    }

    private let read: Read
    private let write: Write
    private(set) var lists: [HostId: Load] = [:]

    init(read: @escaping Read, write: @escaping Write) {
        self.read = read
        self.write = write
    }

    convenience init(fleet: Fleet) {
        self.init(read: { try await fleet.mcpServers(on: $0) },
                  write: { try await fleet.setMcpServers($0, on: $1) })
    }

    func servers(on hostId: HostId) -> [McpServerSettings]? {
        if case .loaded(let servers) = lists[hostId] { servers } else { nil }
    }

    /// Asks `hostId` for its servers.
    func reload(_ hostId: HostId) async {
        if lists[hostId] == nil { lists[hostId] = .asking }
        do {
            lists[hostId] = .loaded(try await read(hostId))
        } catch {
            lists[hostId] = .failed(describe(error))
        }
    }

    /// Changes the servers on `hostId` from what it has now.
    func change(on hostId: HostId, _ body: ([McpServerSettings]) throws -> [McpServerSettings]) async throws {
        let servers = try body(try await read(hostId))
        try await write(servers, hostId)
        lists[hostId] = .loaded(servers)
    }

    /// Adds `servers` on each of `hosts`, each replacing one of its name there. Returns the
    /// machines that refused, with why; the others have them.
    func add(_ servers: [McpServerSettings], to hosts: [HostId]) async -> [HostId: String] {
        var failures: [HostId: String] = [:]
        for host in hosts {
            do {
                try await change(on: host) { McpServers.merge(servers, into: $0) }
            } catch {
                failures[host] = describe(error)
            }
        }
        return failures
    }
}

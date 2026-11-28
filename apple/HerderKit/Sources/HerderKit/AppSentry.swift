import Foundation
import SentrySwift

/// Sentry in the apps, while the person shares diagnostics (`DiagnosticsSwitch`): crashes, app
/// hangs, slow starts and screen loads, sampled profiles on the iPhone, and the app's span
/// around each command, which the client core's and the daemon's join.
///
/// Off until the switch turns it on with a DSN the build was made with, and closed the moment
/// it turns off. Everything goes through `AppScrub` first, which keeps only what herder's
/// privacy page lists. This file imports Sentry alone: its `Event`, `Span` and `User` are not
/// the bindings'.
@MainActor
enum AppSentry {
    /// What starts and closes the Sentry SDK; tests put a fake in its place.
    static var engine: any SentryEngine = LiveSentry()

    /// The DSN Sentry runs with, while it runs.
    private(set) static var running: String?

    /// Starts Sentry with `dsn` for the install `installId`, or closes it, with `nil`.
    static func apply(dsn: String?, installId: String?) {
        let dsn = dsn.flatMap { $0.isEmpty ? nil : $0 }
        guard dsn != running else { return }
        if running != nil {
            engine.close()
            running = nil
        }
        guard let dsn, let installId else { return }
        engine.start(dsn: dsn, installId: installId)
        running = dsn
    }

    /// Runs `body` in a span of its own named after `command`, handing it the span's W3C
    /// `traceparent` for the client core to carry on; with none while Sentry is off.
    static func traced<T>(_ command: String, _ body: (String?) async throws -> T) async rethrows -> T {
        guard running != nil, SentrySDK.isEnabled else { return try await body(nil) }
        let span = SentrySDK.startTransaction(name: "command", operation: "app.command")
        span.setData(value: command, key: "command")
        let header = span.toTraceHeader()
        let trace = "00-\(header.traceId.sentryIdString)-\(header.spanId.sentrySpanIdString)-01"
        do {
            let result = try await body(trace)
            span.finish()
            return result
        } catch {
            span.finish(status: .internalError)
            throw error
        }
    }

    /// The options Sentry runs with: what it watches, and the scrub everything passes.
    nonisolated static func configure(_ options: Options, dsn: String, installId: String) {
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        options.dsn = dsn
        options.releaseName = version.map { "herder@\($0)" }
        options.environment = "app"
        options.sendDefaultPii = false
        // Every operation is recorded; `AppScrub` sends the picked and the slow ones.
        options.tracesSampleRate = 1
        options.enableAutoPerformanceTracing = true
        options.enableAppHangTracking = true
        options.appHangTimeoutInterval = 2
        options.enableWatchdogTerminationTracking = true
        options.enableAutoSessionTracking = false
        // Nothing that carries text or addresses: breadcrumbs, network requests and their URLs,
        // file paths, taps on labelled controls, screenshots and view hierarchies.
        options.maxBreadcrumbs = 0
        options.enableAutoBreadcrumbTracking = false
        options.enableNetworkBreadcrumbs = false
        options.enableNetworkTracking = false
        options.enableCaptureFailedRequests = false
        options.enableFileIOTracing = false
        options.enableCoreDataTracing = false
        options.tracePropagationTargets = []
        #if os(iOS)
        // Screen loads, by their view controllers' type names.
        options.enableUIViewControllerTracing = true
        options.enableUserInteractionTracing = false
        options.attachScreenshot = false
        options.attachViewHierarchy = false
        // On the iPhone only: a Mac app's profile names the folders its libraries load from,
        // which may be in the person's home.
        options.configureProfiling = { profiling in
            profiling.sessionSampleRate = Float(AppScrub.baseRate)
            profiling.lifecycle = .trace
        }
        #endif
        options.beforeBreadcrumb = { _ in nil }
        options.beforeSend = { AppScrub.event($0, installId: installId) }
        options.beforeSendSpan = { AppScrub.span($0) }
    }
}

/// What starts and closes the Sentry SDK.
@MainActor
protocol SentryEngine {
    func start(dsn: String, installId: String)
    func close()
}

/// The Sentry SDK itself.
struct LiveSentry: SentryEngine {
    func start(dsn: String, installId: String) {
        SentrySDK.start { AppSentry.configure($0, dsn: dsn, installId: installId) }
    }

    func close() {
        SentrySDK.close()
    }
}

/// What leaves the app, kept by the same rules as the client core's and the daemon's scrub
/// (`herder-telemetry`): names, numbers, the words herder records itself, error types and
/// stack frames, the OS and device model, the install id. Messages, error values, file names
/// and paths, hosts, addresses, emails, keys, tokens and other free text never go; nor do
/// breadcrumbs, request data, extras, the device's name or its locale.
enum AppScrub {
    /// The share of traces sent whatever their length, picked alike in every process by the
    /// trace id.
    static let baseRate = 0.05
    /// An operation that takes this long or longer sends its trace.
    static let slow: TimeInterval = 1
    /// Span data whose words are kept.
    static let wordKeys: Set<String> = ["route", "command", "outcome", "error", "kind"]
    /// The contexts that go, and of each the keys.
    static let contextKeys: [String: Set<String>] = [
        "os": ["name", "version", "build"],
        "device": ["family", "model", "arch", "simulator"],
        "app": ["app_version", "app_build"],
        "trace": ["trace_id", "span_id", "parent_span_id", "op", "status", "sampled"],
        "profile": ["profile_id"],
        "app_hang": ["duration"],
    ]

    /// `event` as it may leave; `nil` for a fast trace that was not picked.
    static func event(_ event: Event, installId: String) -> Event? {
        if event.type == "transaction", !keeps(event) { return nil }
        event.message = nil
        event.error = nil
        event.serverName = nil
        event.request = nil
        event.breadcrumbs = nil
        event.extra = nil
        event.modules = nil
        event.logger = nil
        event.user = User(userId: installId)
        event.tags = ["component": "app"]
        if let transaction = event.transaction, !isSymbol(transaction) { event.transaction = "screen" }
        event.exceptions = event.exceptions?.map(exception)
        event.threads?.forEach { thread in
            thread.name = thread.name.flatMap { isSymbol($0) ? $0 : nil }
            thread.stacktrace = thread.stacktrace.map(stacktrace)
        }
        event.stacktrace = event.stacktrace.map(stacktrace)
        // Kept for symbolication, by the file's name alone: its folder may be in the home.
        event.debugMeta?.forEach { image in image.codeFile = image.codeFile.map(lastComponent) }
        event.context = event.context.map(contexts)
        return event
    }

    /// `span` as it may leave: its name only when herder or the system named it, and of its
    /// data only numbers, booleans and herder's words.
    static func span(_ span: Span) -> Span? {
        if let description = span.spanDescription, !isSymbol(description) { span.spanDescription = nil }
        for (key, value) in span.data where !keeps(value, under: key) {
            span.removeData(key: key)
        }
        for key in span.tags.keys where key != "component" {
            span.removeTag(key: key)
        }
        return span
    }

    /// Whether a trace's transaction goes: picked by its trace id, or slow.
    static func keeps(_ event: Event) -> Bool {
        if let start = event.startTimestamp, let end = event.timestamp, end.timeIntervalSince(start) >= slow {
            return true
        }
        guard let trace = event.context?["trace"]?["trace_id"] as? String else { return false }
        return picked(trace, rate: baseRate)
    }

    /// Whether the trace `traceId`, 32 hex digits, is among the `rate` picked: the same pick as
    /// the client core's and the daemon's.
    static func picked(_ traceId: String, rate: Double) -> Bool {
        guard let head = UInt32(traceId.prefix(8), radix: 16) else { return false }
        return Double(head) < rate * Double(UInt32.max)
    }

    static func keeps(_ value: Any, under key: String) -> Bool {
        guard isName(key) else { return false }
        switch value {
        case is Bool, is Int, is Double, is NSNumber: return true
        case let word as String: return wordKeys.contains(key) && isWord(word)
        default: return false
        }
    }

    private static func exception(_ exception: Exception) -> Exception {
        let type = exception.type.flatMap { isSymbol($0) ? $0 : nil } ?? "error"
        let kept = Exception(value: nil, type: type)
        kept.threadId = exception.threadId
        kept.stacktrace = exception.stacktrace.map(stacktrace)
        if let mechanism = exception.mechanism {
            let safe = Mechanism(type: isSymbol(mechanism.type) ? mechanism.type : "generic")
            safe.handled = mechanism.handled
            safe.synthetic = mechanism.synthetic
            safe.meta = mechanism.meta
            kept.mechanism = safe
        }
        return kept
    }

    private static func stacktrace(_ stacktrace: SentryStacktrace) -> SentryStacktrace {
        for frame in stacktrace.frames {
            frame.fileName = nil
            frame.contextLine = nil
            frame.preContext = nil
            frame.postContext = nil
            frame.vars = nil
            frame.package = frame.package.map(lastComponent)
            if let function = frame.function, !isSymbol(function) { frame.function = nil }
        }
        return stacktrace
    }

    private static func contexts(_ contexts: [String: [String: Any]]) -> [String: [String: Any]] {
        var kept: [String: [String: Any]] = [:]
        for (name, context) in contexts {
            guard let keys = contextKeys[name] else { continue }
            var values: [String: Any] = [:]
            for (key, value) in context where keys.contains(key) {
                if let text = value as? String {
                    if isSymbol(text) || isHex(text) { values[key] = text }
                } else if value is Bool || value is NSNumber {
                    values[key] = value
                }
            }
            if name == "trace", let data = context["data"] as? [String: Any] {
                values["data"] = data.filter { keeps($0.value, under: $0.key) }
            }
            kept[name] = values
        }
        return kept
    }

    /// The file name of a path, without its folders.
    static func lastComponent(_ path: String) -> String {
        path.split(separator: "/").last.map(String.init) ?? path
    }

    /// A word herder records itself, such as a route kind or a command: lowercase letters,
    /// digits and underscores, starting with a letter, short parts, few digits. A host, an
    /// address, a path, an email, a branch, a key or a sentence is none.
    static func isWord(_ text: String) -> Bool {
        guard let first = text.unicodeScalars.first, ("a"..."z").contains(first), text.count <= 40 else {
            return false
        }
        let allowed = text.unicodeScalars.allSatisfy { ("a"..."z").contains($0) || ("0"..."9").contains($0) || $0 == "_" }
        let digits = text.unicodeScalars.filter { ("0"..."9").contains($0) }.count
        return allowed && digits <= 4 && text.split(separator: "_").allSatisfy { $0.count <= 16 }
    }

    /// Words joined by dots, as span data keys are.
    static func isName(_ text: String) -> Bool {
        !text.isEmpty && text.count <= 64 && text.split(separator: ".", omittingEmptySubsequences: false).allSatisfy { isWord(String($0)) }
    }

    /// A name code or the system gives: a type, a function, a view controller, an operation, a
    /// version or a device model. Never a path, an email, a URL, a host, an address or a sentence.
    static func isSymbol(_ text: String) -> Bool {
        guard !text.isEmpty, text.count <= 200, !text.contains("/"), !text.contains("@"), !text.contains("://") else {
            return false
        }
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "_.:<>()[],-$ "))
        guard text.unicodeScalars.allSatisfy({ allowed.contains($0) && $0.isASCII }) else { return false }
        // An address with its port, as `10.0.4.17:7447`, or an IPv6 one, as `fd7a:115c:a1e0::1`:
        // a symbol's colons come with its labels, as in `foo(bar:)`.
        if text.contains(":"), text.allSatisfy({ $0.isHexDigit || $0 == "." || $0 == ":" }) { return false }
        // A sentence: four or more words of letters.
        let words = text.split(separator: " ").filter { $0.allSatisfy(\.isLetter) }
        guard words.count < 4 else { return false }
        // A host, as `box.tail1234.ts.net` or `studio.local`: lowercase dotted parts, three or
        // more, or ending as a host name does.
        let dotted = text.split(separator: ".").filter { part in part.contains { $0.isLetter } }
        guard dotted.count >= 2, text == text.lowercased() else { return true }
        return dotted.count < 3 && !hostEndings.contains(String(dotted.last ?? ""))
    }

    /// The last parts of host names that are not also the last words of operations.
    static let hostEndings: Set<String> = [
        "local", "lan", "home", "internal", "localdomain", "net", "com", "org", "io", "dev", "sh", "app", "cloud",
    ]

    static func isHex(_ text: String) -> Bool {
        (16...32).contains(text.count) && text.unicodeScalars.allSatisfy { CharacterSet(charactersIn: "0123456789abcdef").contains($0) }
    }
}

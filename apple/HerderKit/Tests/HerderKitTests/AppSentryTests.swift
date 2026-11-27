import Foundation
@testable import HerderKit
import Sentry
import Testing

/// What would name a person, a machine or their work, as it shows up in real diagnostics.
private let secrets = [
    "/Users/tomas/Projects/herder/apple/HerderKit/Sources/HerderKit/Fleet.swift",
    "/private/var/mobile/Containers/Data/Application/0B7F1C2E/Library/herder",
    "10.0.4.17:7447",
    "100.64.0.7",
    "fd7a:115c:a1e0::1",
    "box.tail1234.ts.net",
    "studio.local",
    "wss://box.example.com",
    "tomas@example.com",
    "sk-ant-api03-AbCdEf0123456789xyz",
    "ghp_16C7e42F292c6912E7710c838347Ae178B4a",
    "p29-3-sentry-apple",
    "herder-sh/herder",
    "Fix the login bug in the settings page please",
    "Tomas’s iPhone",
    "en_LT",
    "Europe/Vilnius",
]

/// Starts and closes nothing: counts what Sentry would have done.
@MainActor
private final class FakeEngine: SentryEngine {
    var started: [String] = []
    var closed = 0

    func start(dsn: String, installId: String) { started.append(dsn) }
    func close() { closed += 1 }
}

@MainActor
struct AppSentryTests {
    /// A DSN no Sentry answers at.
    let dsn = "https://public@sentry.invalid/1"
    let installId = "0b7f1c2e-4d4b-4e0e-9a51-3f6f1b2a9c10"

    @Test func sentryRunsOnlyWhileTheSwitchIsOnAndTheBuildHasADsn() {
        let engine = FakeEngine()
        let previous = AppSentry.engine
        AppSentry.engine = engine
        defer {
            AppSentry.apply(dsn: nil, installId: nil)
            AppSentry.engine = previous
        }
        AppSentry.apply(dsn: nil, installId: installId)
        AppSentry.apply(dsn: "", installId: installId)
        #expect(engine.started.isEmpty)
        AppSentry.apply(dsn: dsn, installId: installId)
        AppSentry.apply(dsn: dsn, installId: installId)
        #expect(engine.started == [dsn])
        AppSentry.apply(dsn: nil, installId: nil)
        #expect(engine.closed == 1)
        #expect(AppSentry.running == nil)
    }

    @Test func theSwitchStartsAndStopsSentryAndNothingRunsWhileItIsOff() throws {
        let engine = FakeEngine()
        let previous = AppSentry.engine
        AppSentry.engine = engine
        defer {
            AppSentry.apply(dsn: nil, installId: nil)
            AppSentry.engine = previous
        }
        let name = "diagnostics-\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: name))
        defer { defaults.removePersistentDomain(forName: name) }
        let diagnostics = DiagnosticsSwitch(
            defaults: defaults, keys: (dsn, nil), share: DiagnosticsSwitch.shareEverywhere)
        diagnostics.apply()
        #expect(engine.started.isEmpty)
        diagnostics.set(true)
        #expect(engine.started == [dsn])
        diagnostics.set(false)
        #expect(engine.closed == 1)
        #expect(AppSentry.running == nil)
    }

    @Test func aCrashKeepsItsTypeAndFramesAndNothingThatNamesAnything() throws {
        let event = Event(level: .error)
        event.message = SentryMessage(formatted: "cannot read \(secrets[0]) for \(secrets[8])")
        event.serverName = secrets[5]
        event.transaction = secrets[13]
        event.tags = ["host": secrets[2], "route": "system_vpn"]
        event.extra = ["token": secrets[9]]
        event.user = {
            let user = User(userId: "someone")
            user.email = secrets[8]
            user.ipAddress = secrets[3]
            return user
        }()
        event.breadcrumbs = [Breadcrumb(level: .info, category: secrets[7])]
        let frame = Frame()
        frame.function = "Fleet.send(_:about:)"
        frame.fileName = secrets[0]
        frame.package = "/Users/tomas/Applications/herder.app/Contents/MacOS/herder"
        frame.lineNumber = 636
        frame.vars = ["text": secrets[13]]
        let exception = Exception(value: "No such file \(secrets[1])", type: "NSInvalidArgumentException")
        exception.stacktrace = SentryStacktrace(frames: [frame], registers: [:])
        exception.mechanism = {
            let mechanism = Mechanism(type: "nsexception")
            mechanism.desc = secrets[6]
            mechanism.data = ["path": secrets[1]]
            return mechanism
        }()
        event.exceptions = [exception]
        let image = DebugMeta()
        image.codeFile = "/Users/tomas/Applications/herder.app/Contents/MacOS/herder"
        image.debugID = "8f6d3b2a-1c4e-4f6a-9b7d-2e5c8a1f3d40"
        event.debugMeta = [image]
        event.context = [
            "device": ["name": secrets[14], "model": "iPhone17,1", "family": "iOS", "arch": "arm64e"],
            "os": ["name": "iOS", "version": "26.1", "build": "23B85", "kernel_version": secrets[5]],
            "culture": ["locale": secrets[15], "timezone": secrets[16]],
            "app": ["app_version": "0.25.0", "app_build": "812.1", "device_app_hash": "abc"],
        ]

        let scrubbed = try #require(AppScrub.event(event, installId: installId))
        let json = String(decoding: try JSONSerialization.data(withJSONObject: scrubbed.serialize()), as: UTF8.self)
        for secret in secrets {
            let escaped = secret.replacingOccurrences(of: "/", with: "\\/")
            #expect(!json.contains(secret) && !json.contains(escaped), "\(secret) left in \(json)")
        }
        #expect(scrubbed.exceptions?.first?.type == "NSInvalidArgumentException")
        #expect(scrubbed.exceptions?.first?.value == nil)
        let kept = try #require(scrubbed.exceptions?.first?.stacktrace?.frames.first)
        #expect(kept.function == "Fleet.send(_:about:)")
        #expect(kept.lineNumber == 636)
        #expect(kept.fileName == nil)
        #expect(kept.package == "herder")
        #expect(scrubbed.debugMeta?.first?.codeFile == "herder")
        #expect(scrubbed.user?.userId == installId)
        #expect(scrubbed.tags == ["component": "app"])
        #expect(json.contains("iPhone17,1"))
        #expect(json.contains("26.1"))
        #expect(scrubbed.context?["culture"] == nil)
    }

    @Test func aTraceIsPickedAsTheDaemonPicksItAndASlowOneAlwaysGoes() {
        #expect(AppScrub.picked("0000000a77b34da6a3ce929d0e0e4736", rate: AppScrub.baseRate))
        #expect(!AppScrub.picked("fff92f3577b34da6a3ce929d0e0e4736", rate: AppScrub.baseRate))
        let fast = Event()
        fast.type = "transaction"
        fast.startTimestamp = Date(timeIntervalSince1970: 0)
        fast.timestamp = Date(timeIntervalSince1970: 0.005)
        fast.context = ["trace": ["trace_id": "fff92f3577b34da6a3ce929d0e0e4736"]]
        #expect(AppScrub.event(fast, installId: installId) == nil)
        let slow = Event()
        slow.type = "transaction"
        slow.startTimestamp = Date(timeIntervalSince1970: 0)
        slow.timestamp = Date(timeIntervalSince1970: AppScrub.slow)
        slow.context = ["trace": ["trace_id": "fff92f3577b34da6a3ce929d0e0e4736"]]
        #expect(AppScrub.event(slow, installId: installId) != nil)
    }

    @Test func onlyWordsHerderRecordsGoAsSpanData() {
        #expect(AppScrub.keeps("send_prompt", under: "command"))
        #expect(AppScrub.keeps(42, under: "events"))
        #expect(AppScrub.keeps(true, under: "relayed"))
        #expect(!AppScrub.keeps("send_prompt", under: "text"))
        for secret in secrets {
            #expect(!AppScrub.keeps(secret, under: "route"), "\(secret)")
            #expect(!AppScrub.isSymbol(secret) || !secret.contains(where: { "/@:".contains($0) }), "\(secret)")
        }
        #expect(!AppScrub.isSymbol(secrets[5]))
        #expect(!AppScrub.isSymbol(secrets[6]))
        #expect(!AppScrub.isSymbol(secrets[13]))
        #expect(AppScrub.isSymbol("UIHostingController<RootView>"))
        #expect(AppScrub.isSymbol("app.command"))
        // Colons in a function's labels keep it; an address's colons do not.
        #expect(AppScrub.isSymbol("Fleet.start(_:accountId:)"))
        #expect(!AppScrub.isSymbol("100.64.0.7:7447"))
    }
}

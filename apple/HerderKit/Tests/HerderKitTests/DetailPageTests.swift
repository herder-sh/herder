import Foundation
@testable import HerderKit
import SwiftUI
import Testing

struct PageStackTests {
    @Test func opensPagesOverEachOther() {
        var stack = PageStack()
        #expect(stack.top == nil)
        stack.open(.machine("h1"))
        stack.open(.account(loginId: "claude/work"))
        #expect(stack.pages == [.machine("h1"), .account(loginId: "claude/work")])
        #expect(stack.top == .account(loginId: "claude/work"))
    }

    @Test func reopeningAPageBelowGoesBackToIt() {
        var stack = PageStack()
        stack.open(.machine("h1"))
        stack.open(.account(loginId: "claude/work"))
        stack.open(.vault("v1"))
        stack.open(.machine("h1"))
        #expect(stack.pages == [.machine("h1")])
    }

    @Test func closesAndGoesBackToADepth() {
        var stack = PageStack()
        stack.open(.machine("h1"))
        stack.open(.account(loginId: "claude/work"))
        stack.open(.vault("v1"))
        stack.close()
        #expect(stack.pages == [.machine("h1"), .account(loginId: "claude/work")])
        stack.back(to: 1)
        #expect(stack.pages == [.machine("h1")])
        stack.back(to: 0)
        #expect(stack.pages.isEmpty)
        stack.close()
        stack.back(to: 3)
        #expect(stack.pages.isEmpty)
    }
}

@MainActor struct AutosaveTests {
    private struct Refused: LocalizedError {
        var errorDescription: String? { "The machine is offline" }
    }

    @Test func showsSavedForAMomentAfterAChangeLands() async {
        let saves = Autosave()
        saves.lingers = .milliseconds(50)
        let saving = Task { await saves.save("Name") {} }
        for _ in 0..<50 where saves["Name"] != .saved { await Task.yield() }
        #expect(saves["Name"] == .saved)
        await saving.value
        #expect(saves["Name"] == nil)
        #expect(saves["Account"] == nil)
    }

    @Test func keepsTheErrorUntilTheNextChange() async {
        let saves = Autosave()
        saves.lingers = .milliseconds(50)
        await saves.save("Name") { throw Refused() }
        guard case .failed(let message) = saves["Name"] else {
            Issue.record("no error shown: \(String(describing: saves["Name"]))")
            return
        }
        #expect(message.contains("offline"))
        await saves.save("Name") {}
        #expect(saves["Name"] == nil)
    }

    @Test func aLaterChangeKeepsItsOwnSaved() async {
        // Each moment ends when its task is cancelled, not after a time a busy runner can miss.
        let saves = Autosave()
        saves.lingers = .seconds(60)
        let first = Task { await saves.save("Name") {} }
        for _ in 0..<100 where saves["Name"] != .saved { await Task.yield() }
        var started = false
        let second = Task { await saves.save("Name") { started = true } }
        for _ in 0..<100 where !started { await Task.yield() }
        first.cancel()
        await first.value
        // The first change's moment is over, but the second one's is not.
        #expect(saves["Name"] == .saved)
        second.cancel()
        await second.value
        #expect(saves["Name"] == nil)
    }
}

/// Sheets never open sheets: a view that draws a sheet (`SheetScaffold`) presents nothing over
/// itself. What it leads to replaces it, or opens as a page.
struct NoNestedSheetsTests {
    @Test func noSheetPresentsAnother() throws {
        let sources = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("Sources/HerderKit")
        let files = try #require(FileManager.default.enumerator(at: sources, includingPropertiesForKeys: nil))
            .compactMap { $0 as? URL }.filter { $0.pathExtension == "swift" }
        #expect(files.count > 20)
        var nested: [String] = []
        for file in files {
            let text = try String(contentsOf: file, encoding: .utf8)
            for (name, body) in Self.types(in: text) where body.contains("SheetScaffold(") {
                for presenter in [".sheet(", ".fullScreenCover("] where body.contains(presenter) {
                    nested.append("\(name) in \(file.lastPathComponent) uses \(presenter)")
                }
            }
        }
        #expect(nested.isEmpty, "\(nested.joined(separator: "\n"))")
    }

    /// Each top-level `struct` in `text`, by name, with its source up to the next one.
    static func types(in text: String) -> [(name: String, body: String)] {
        let lines = text.components(separatedBy: "\n")
        var types: [(name: String, body: String)] = []
        for line in lines {
            let declaration = ["struct ", "private struct ", "fileprivate struct "].first { line.hasPrefix($0) }
            if let declaration {
                let name = line.dropFirst(declaration.count).prefix { $0.isLetter || $0.isNumber }
                types.append((String(name), ""))
            } else if !types.isEmpty {
                types[types.count - 1].body += line + "\n"
            }
        }
        return types
    }

    @Test func findsAPresenterInASheet() {
        let source = """
            struct Picker: View {
                var body: some View {
                    SheetScaffold(title: "Pick") {} footer: {}
                        .sheet(isPresented: $more) { Other() }
                }
            }
            struct Page: View {
                var body: some View { Text("").sheet(isPresented: $open) { Picker() } }
            }
            """
        let types = NoNestedSheetsTests.types(in: source)
        #expect(types.map(\.name) == ["Picker", "Page"])
        #expect(types[0].body.contains("SheetScaffold(") && types[0].body.contains(".sheet("))
        #expect(!types[1].body.contains("SheetScaffold("))
    }
}

#if os(macOS)
import AppKit

/// Draws a machine's page as `DetailPage` lays it out, with its section index, Needs attention,
/// a row just saved and one whose save failed, the folded About and the Danger zone, and
/// attaches it; with `HERDER_SCREENSHOT_DIR` set, also writes it there as `detail-page.png`.
@MainActor struct DetailPageScreenshotTests {
    @Test func drawsAMachinePage() async throws {
        let host = NSHostingView(rootView: SamplePage().environment(\.colorScheme, .dark))
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(x: 0, y: 0, width: 1040, height: 900)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        // The index shows once the page has read its sections.
        for _ in 0..<5 {
            host.layoutSubtreeIfNeeded()
            try await Task.sleep(for: .milliseconds(100))
        }
        let rep = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        #expect(rep.pixelsWide >= 1040)
        let png = try #require(rep.representation(using: .png, properties: [:]))
        Attachment.record(png, named: "detail-page.png")
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("detail-page.png"))
        }
    }
}

private struct SamplePage: View {
    var body: some View {
        VStack(spacing: 0) {
            PageTrail(crumbs: ["Fleet", "trash-can-01"]) { _ in }
            DetailPage(title: "trash-can-01", chip: StatusChip(text: "Connected · 38 ms", urgency: .done),
                       status: "owner · 2 running · Linux") {
                PaneButton(title: "Open Terminal", symbol: "terminal", prominent: true) {}
            } content: {
                NeedsAttention {
                    AttentionRow(title: "Claude · ops is signed out", detail: "2 sessions waiting · since 14 min ago") {
                        PillButton(title: "Sign in again", prominent: true) {}
                    }
                    RowDivider()
                    AttentionRow(title: "2 CLI updates", detail: "Claude 2.1.303 → 2.1.310 · Codex 0.48.0 → 0.51.2") {
                        PillButton(title: "Update both") {}
                    }
                }
                SettingsList(title: "Accounts") {
                    SettingRow(label: "Claude · work", hint: "primary · 18% of the session left") {
                        Image(systemName: "chevron.right").foregroundStyle(Theme.tertiary)
                    }
                    RowDivider()
                    SettingRow(label: "Codex · Pro", hint: "91% of the session left") {
                        Image(systemName: "chevron.right").foregroundStyle(Theme.tertiary)
                    }
                }
                .detailSection("Accounts")
                SettingsList(title: "Capacity") {
                    SettingRow(label: "Turns at once", hint: "16 GB, 8 cores · about 6 fit", saved: .saved) {
                        Text("4").font(.subheadline.monospacedDigit()).foregroundStyle(Theme.text)
                    }
                }
                .detailSection("Capacity")
                SettingsList(title: "Connection") {
                    SettingRow(label: "Name on this device", saved: .failed("The machine is offline")) {
                        Text("trash-can-01").foregroundStyle(Theme.secondary)
                    }
                }
                .detailSection("Connection")
                AboutSection {
                    DetailRow(label: "Host id", value: "h-3f9a", mono: true)
                }
                DangerZone {
                    SettingRow(label: "Forget this machine", hint: "Pair again to get it back") {
                        DangerButton(title: "Forget…") {}
                    }
                }
            }
        }
        .background(Theme.canvas)
    }
}
#endif

struct SettingsPatternTests {
    @Test func aPersonsAvatarColourComesFromTheirIdAlone() {
        #expect(Avatar.colorIndex("U04SAM0001") == Avatar.colorIndex("U04SAM0001"))
        // Pinned, so a change to the hash that would recolour everyone fails here.
        #expect(Avatar.colorIndex("") == 4)
        #expect(Avatar.colorIndex("a") == 5)
        // The demo's people, whose ids differ in a few letters, get fills of their own.
        #expect(["U04SAM0001", "U04KIM0003", "U04ALEX002"].map(Avatar.colorIndex) == [0, 3, 4])
        let ids = (0..<60).map { "U\($0)" }
        #expect(Set(ids.map(Avatar.colorIndex)).count == Theme.avatars.count)
        #expect(Avatar.initials("Sam") == "S")
        #expect(Avatar.initials("Alex Kim") == "AK")
        #expect(Avatar.initials(" ") == "?")
    }

    @Test func theIndexRaisesTheSectionBeingRead() {
        let sections = ["People", "New threads", "Channels", "About"]
        #expect(DetailReading.current([], in: sections) == "People")
        #expect(DetailReading.current(["People"], in: sections) == "People")
        #expect(DetailReading.current(["People", "New threads"], in: sections) == "New threads")
        #expect(DetailReading.current(["People", "New threads", "Channels", "About"], in: sections) == "About")
    }
}

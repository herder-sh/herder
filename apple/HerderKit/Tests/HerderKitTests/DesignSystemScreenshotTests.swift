#if os(macOS)
import AppKit
import Foundation
@testable import HerderKit
import SwiftUI
import Testing

/// Draws the design system's components on one sheet, as the app's dark appearance shows them,
/// and attaches it; with `HERDER_SCREENSHOT_DIR` set, also writes it there as
/// `design-system.png`.
@MainActor struct DesignSystemScreenshotTests {
    @Test func drawsEveryComponent() throws {
        let renderer = ImageRenderer(content: Gallery().environment(\.colorScheme, .dark))
        renderer.scale = 2
        let image = try #require(renderer.nsImage)
        #expect(image.size.width >= 700 && image.size.height >= 700)
        let tiff = try #require(image.tiffRepresentation)
        let png = try #require(NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]))
        Attachment.record(png, named: "design-system.png")
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("design-system.png"))
        }
    }
}

private struct Account: Identifiable {
    let id: String
    let machines: Int
    let left: Double
    let used: Date
}

/// Every component of `docs/apple-design.md`'s design system, as a page uses it.
private struct Gallery: View {
    @State private var filter: Signal.ID? = "running"
    private let now = Date.now

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            PageHeader(title: "Providers", status: "5 accounts · 1 needs sign-in · 3 updates") {
                PaneButton(title: "Update All (3)", symbol: "arrow.down.circle") {}
                PaneButton(title: "Add Account", symbol: "plus", prominent: true) {}
            }
            // A narrow list pane: the status goes under the title.
            PageHeader(title: "Board", status: "All 3 machines connected") {
                PaneButton(title: "New Session", symbol: "plus", prominent: true) {}
            }
            .frame(width: 360)
            VStack(alignment: .leading, spacing: 18) {
                SignalStrip(signals: [
                    Signal(id: "needs", count: 1, label: "needs sign-in", urgency: .needsYou),
                    Signal(id: "failed", count: 1, label: "failed", urgency: .failing),
                    Signal(id: "running", count: 2, label: "updating", urgency: .running),
                    Signal(id: "zero", count: 0, label: "offline", urgency: .quiet),
                    Signal(id: "done", count: 4, label: "up to date", urgency: .done),
                ], filter: $filter)

                SectionHeading(title: "Accounts", count: 3)
                SortableTable("gallery.accounts", rows: [
                    Account(id: "work", machines: 3, left: 0.62, used: now.addingTimeInterval(-190)),
                    Account(id: "personal", machines: 1, left: 0.18, used: now.addingTimeInterval(-4000)),
                    Account(id: "team-2", machines: 2, left: 0.91, used: now.addingTimeInterval(-90000)),
                ], sortedBy: TableSort(column: "Session left", ascending: false), columns: [
                    SortableColumn("Account", value: \.id) { Text($0.id) },
                    SortableColumn("Machines", width: 80, alignment: .trailing, value: \.machines) {
                        Text("\($0.machines)").monospacedDigit()
                    },
                    SortableColumn("Session left", width: 120, value: \.left) {
                        UsageBar(percent: (1 - $0.left) * 100).frame(width: 90)
                    },
                    SortableColumn("Last used", width: 110, value: \.used) {
                        RelativeTime(date: $0.used).font(Theme.meta).foregroundStyle(Theme.secondary)
                    },
                ])

                SettingsList(title: "Failover") {
                    SettingRow(label: "Account", hint: "Rotates to another when it runs out",
                               help: "herder moves to the next account only after a limit.") {
                        Text("Any").foregroundStyle(Theme.secondary)
                    }
                    RowDivider()
                    SettingRow(label: "Setup command", hint: "Runs once in each new worktree") {
                        Text("pnpm install").font(Theme.mono).foregroundStyle(Theme.secondary)
                    }
                }

                SectionHeading(title: "Buttons")
                HStack(spacing: 10) {
                    PaneButton(title: "New Session", symbol: "plus", prominent: true) {}
                    PaneButton(title: "Secrets", symbol: "key") {}
                    PillButton(title: "Set Up…", prominent: true) {}
                    PillButton(title: "Edit…") {}
                    CircleButton(symbol: "arrow.up", help: "Send") {}
                }
                HStack(spacing: 10) {
                    ActionButton(title: "Deny", style: .secondary) {}
                    ActionButton(title: "Allow", style: .primary) {}
                }

                SectionHeading(title: "Type")
                VStack(alignment: .leading, spacing: 4) {
                    Text("Page 20").font(Theme.page)
                    Text("Section and title 14").font(Theme.title)
                    Text("Body 13").font(Theme.body)
                    Text("Meta 11.5 · 3 min. ago").font(Theme.meta).foregroundStyle(Theme.tertiary)
                }
                .foregroundStyle(Theme.text)
            }
            .padding(.horizontal, 20)
            .padding(.bottom, 24)
        }
        .frame(width: 760)
        .background(Theme.background)
    }
}
#endif

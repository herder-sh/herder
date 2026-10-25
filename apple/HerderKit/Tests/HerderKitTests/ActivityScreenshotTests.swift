#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws the Activity popover with a job in every group, as the app's dark appearance shows
/// it, and attaches it; with `HERDER_SCREENSHOT_DIR` set, also writes it there as
/// `activity.png`. The detail view, with its live terminal, is shot by the app's
/// ScreenshotTests.
@MainActor struct ActivityScreenshotTests {
    @Test func drawsThePopoverWithEveryGroup() throws {
        let renderer = ImageRenderer(content: ActivityGroupsPreview().environment(\.colorScheme, .dark))
        renderer.scale = 2
        let image = try #require(renderer.nsImage)
        #expect(image.size.width == 560 && image.size.height >= 400)
        let tiff = try #require(image.tiffRepresentation)
        let png = try #require(NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]))
        Attachment.record(png, named: "activity.png")
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("activity.png"))
        }
    }
}

/// The popover's list, as mockup §2 has it: a login waiting on the user, an update running and
/// the next one on its machine queued, then what finished, a warning and a failure among them.
private struct ActivityGroupsPreview: View {
    var body: some View {
        let now = Date.now
        let items = [
            item(.add("claude-ops"), on: "trash-can-01", .running, "claude"),
            item(.install("codex"), on: "work-mbp", .installing, "codex", before: "0.48.0"),
            item(.install("codex"), on: "trash-can-01", .queued, "codex", before: "0.48.0"),
            item(.install("claude"), on: "work-mbp", .updated(from: "2.1.303", to: "2.1.310"), "claude",
                 ended: now.addingTimeInterval(-120), warnings: [
                    VendorWarning(title: "2 Claude installs on this machine", detail: "",
                                  fix: "npm uninstall -g @anthropic-ai/claude-code"),
                 ]),
            item(.install("grok"), on: "work-mbp", .updated(from: "0.9.1", to: "0.9.4"), "grok",
                 ended: now.addingTimeInterval(-130)),
            item(.install("opencode"), on: "trash-can-01", .failed("npm: EACCES on /usr/local/lib · exit 243"),
                 "opencode", ended: now.addingTimeInterval(-60)),
        ]
        ActivityList(items: items, clear: {}, open: { _ in }, retry: { _ in })
            .frame(width: 560)
    }

    private func item(_ kind: JobKind, on machine: String, _ progress: JobProgress, _ provider: Provider,
                      before: String? = nil, ended: Date? = nil, warnings: [VendorWarning] = []) -> ActivityItem {
        let key = JobKey(hostId: machine, kind: kind)
        return ActivityItem(key: key, progress: progress, text: JobText(
            machine: machine, title: key.title(machine: machine, provider: provider, progress: progress, updating: before != nil),
            detail: progress.detail(machine: machine, before: before), warnings: warnings), ended: ended)
    }
}
#endif

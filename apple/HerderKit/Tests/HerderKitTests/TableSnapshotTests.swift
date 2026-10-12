#if os(macOS)
import AppKit
import SwiftUI
import Testing
@testable import HerderKit

// TEMPORARY: renders before/after PNGs for the PR; dropped before merge.
@MainActor
struct TableSnapshotTests {
    @Test func renderTableSnapshots() throws {
        guard let dir = ProcessInfo.processInfo.environment["HERDER_TABLE_SNAPSHOTS"] else { return }
        try FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        let text = """
        "OTA" is just the business label for any travel site. The legal question is whose behalf we act on and in whose name we contract. Here are the roles, from least to most responsibility:

        | Role | Who we act for | Whose name the contract is in | Who the guest's contract for the stay is with | Who carries the risk |
        |---|---|---|---|---|
        | **Platform / facilitator** | Nobody; we connect the parties | No one's; we're not a party | Hotel | Hotel |
        | **Agent of the hotel** | Hotel | Hotel's | Hotel | Hotel |
        | **Intermediary / agent of the customer** | Guest | Guest's | Hotel | Guest |
        | **Commissionaire ("komisas")** | Guest | **Ours** (on the guest's account) | Us, while the guest's stay is delivered by the hotel | Us |
        | **Reseller / merchant** | Ourselves | Ours | Us | Us |

        Two terms that get mixed in: merchant of record is a payments term.
        """
        let view = MarkdownText(text: text)
            .padding(24)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .background(Theme.background)
        let host = NSHostingView(rootView: view)
        host.wantsLayer = true
        let window = NSWindow(contentRect: CGRect(x: 0, y: 0, width: 1100, height: 560), styleMask: .borderless,
                              backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: .darkAqua)
        window.contentView = host
        for (index, width) in [1100, 760, 1100].enumerated() {
            window.setContentSize(CGSize(width: width, height: 560))
            for _ in 0..<5 {
                host.layoutSubtreeIfNeeded()
                RunLoop.main.run(until: Date().addingTimeInterval(0.1))
            }
            host.layoutSubtreeIfNeeded()
            let rep = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
            host.cacheDisplay(in: host.bounds, to: rep)
            let png = try #require(rep.representation(using: .png, properties: [:]))
            try png.write(to: URL(fileURLWithPath: "\(dir)/\(index + 1)-\(width).png"))
        }
    }
}
#endif

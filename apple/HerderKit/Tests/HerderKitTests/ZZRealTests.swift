import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

// Temporary, local only, not committed: 120 rail jumps on the owner's session (REAL_DEMO) or its synthetic copy (SYNTH).
@MainActor final class ZZRealTests {
    @Test func real() async throws {
        guard let path = ProcessInfo.processInfo.environment["REAL_DEMO"] else { return }
        let fleet: Fleet
        var key: SessionKey?
        if ProcessInfo.processInfo.environment["SYNTH"] != nil {
            guard case .opened(let opened) = Profile.open(at: temporaryProfile(), client: "test") else { return }
            fleet = opened
            var script = Script("01A", host: "h")
            fleet.applyForTesting(SessionUpdate(events: leadSession(&script), streaming: []), to: script.key)
            key = script.key
        } else {
            fleet = Fleet(client: try Client.openDemo(demo: try String(contentsOfFile: path, encoding: .utf8)))
            Task { await fleet.follow() }
            for _ in 0..<500 where key == nil {
                try await Task.sleep(for: .milliseconds(20))
                key = fleet.machines.first.flatMap { m in m.sessions.first.map { SessionKey(hostId: m.hostId, sessionId: $0.sessionId) } }
            }
        }
        guard let key else { Issue.record("no session"); return }
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1100, height: 760), styleMask: [.titled], backing: .buffered, defer: false)
        window.contentView = NSHostingView(rootView: NavigationStack { SessionView(fleet: fleet, key: key) })
        window.orderFront(nil)
        for _ in 0..<500 where fleet.transcript(key).count < 10 { try await Task.sleep(for: .milliseconds(20)) }
        spin(1)
        let blocks = fleet.transcript(key)
        func find(_ v: NSView?) -> NSScrollView? {
            guard let v else { return nil }
            if let s = v as? NSScrollView, (s.documentView?.frame.height ?? 0) > 2 * s.frame.height { return s }
            return v.subviews.lazy.compactMap(find).first
        }
        guard let scroll = find(window.contentView) else { Issue.record("no scroll"); return }
        let root = window.contentView!
        func ink() -> Int {
            let frame = scroll.convert(scroll.bounds, to: root)
            let w = Int(frame.width), h = Int(frame.height)
            let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
            ctx.translateBy(x: -frame.minX, y: -(root.isFlipped ? root.bounds.height - frame.maxY : frame.minY))
            root.layer!.render(in: ctx)
            let px = ctx.data!.bindMemory(to: UInt8.self, capacity: w * h * 4)
            func lum(_ x: Int, _ y: Int) -> Int { let i = (y * w + x) * 4; return Int(px[i]) + Int(px[i+1]) + Int(px[i+2]) }
            let ground = lum(w - 2, h / 2)
            var n = 0
            for y in stride(from: 0, to: h, by: 4) { for x in stride(from: 60, to: w, by: 4) where abs(lum(x, y) - ground) > 90 { n += 1 } }
            return n
        }
        let items = Checkpoints(blocks).items
        var blanks = 0
        for i in 0..<120 {
            let f = [0.0, 0.9, 0.1, 0.5, 1.0, 0.3, 0.05, 0.99][i % 8]
            RecordHook.jump(items[min(items.count - 1, Int(f * Double(items.count - 1)))].id)
            spin(0.7)
            if ink() < 100 { blanks += 1 }
        }
        print("REAL blocks=\(blocks.count) jumps=120 blanks=\(blanks)")
    }
    func spin(_ s: Double) { RunLoop.main.run(until: Date().addingTimeInterval(s)) }
}

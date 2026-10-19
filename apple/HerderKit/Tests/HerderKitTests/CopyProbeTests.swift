#if os(macOS)
import AppKit
import SwiftUI
import Testing
@testable import HerderKit

@MainActor
struct CopyProbeTests {
    @Test(arguments: [-1.0, 0.0, 0.05, 0.3])
    func probe(delay: Double) throws {
        let saved = NSPasteboard.general.string(forType: .string)
        defer { Clipboard.string = saved }
        let text = "Done. The change is on this session's branch, with the tests passing and more words here."
        let view = VStack {
            TranscriptText(string: AttributedString(text), color: .white).frame(width: 380, alignment: .leading)
            Spacer()
        }.frame(width: 400, height: 300).background(.black)
        let window = NSWindow(contentRect: CGRect(x: -6000, y: -6000, width: 400, height: 300), styleMask: .titled,
                              backing: .buffered, defer: false)
        let host = NSHostingView(rootView: view)
        window.contentView = host
        window.orderFrontRegardless()
        window.makeKey()
        host.layoutSubtreeIfNeeded()
        RunLoop.main.run(until: Date().addingTimeInterval(0.3))
        Clipboard.string = "<before>"
        func mouse(_ type: NSEvent.EventType, _ x: CGFloat) {
            // Window coordinates: origin bottom left; the text is at the top.
            let event = NSEvent.mouseEvent(with: type, location: CGPoint(x: x, y: 300 - 8), modifierFlags: [],
                                           timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber,
                                           context: nil, eventNumber: 0, clickCount: 1, pressure: type == .leftMouseUp ? 0 : 1)!
            window.sendEvent(event)
        }
        mouse(.leftMouseDown, 3)
        RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        for x in stride(from: 10.0, through: 200.0, by: 10.0) {
            mouse(.leftMouseDragged, x)
        }
        RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        mouse(.leftMouseUp, 200)
        if delay >= 0 { RunLoop.main.run(until: Date().addingTimeInterval(delay)) }
        let responder = window.firstResponder
        let sent = NSApp.sendAction(#selector(NSText.copy(_:)), to: nil, from: nil)
        print("probe delay \(delay) responder \(String(describing: responder.map { type(of: $0) })) sent \(sent) copied \(String(describing: Clipboard.string))")
        window.orderOut(nil)
    }
}
#endif

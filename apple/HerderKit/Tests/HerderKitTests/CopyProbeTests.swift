#if os(macOS)
import AppKit
import SwiftUI
import Testing
@testable import HerderKit

@MainActor
final class ProbeModel: ObservableObject {
    @Published var text = ""
    @Published var focused = true
    @Published var tick = 0
}

private struct ProbeView: View {
    @ObservedObject var model: ProbeModel
    var body: some View {
        VStack {
            Text("tick \(model.tick)")
            PromptEditor(text: $model.text, focused: $model.focused, images: [], pastes: [],
                         addImages: { _ in "" }, addFiles: { _ in }, addPaste: { $0 }, submit: {}, complete: { false })
        }.frame(width: 400, height: 200)
    }
}

@MainActor
struct CopyProbeTests {
    @Test func composerTakesFocusBackOnAnyRedraw() throws {
        let model = ProbeModel()
        let window = NSWindow(contentRect: CGRect(x: -6000, y: -6000, width: 400, height: 300), styleMask: .titled,
                              backing: .buffered, defer: false)
        let host = NSHostingView(rootView: ProbeView(model: model))
        let other = NSTextView(frame: CGRect(x: 0, y: 0, width: 100, height: 20))
        other.isEditable = false
        other.string = "transcript text"
        let root = NSView(frame: CGRect(x: 0, y: 0, width: 400, height: 300))
        host.frame = CGRect(x: 0, y: 50, width: 400, height: 250)
        root.addSubview(host); root.addSubview(other)
        window.contentView = root
        host.layoutSubtreeIfNeeded()
        RunLoop.main.run(until: Date().addingTimeInterval(0.3))
        print("probe start responder \(type(of: window.firstResponder!)) focused \(model.focused)")
        window.makeFirstResponder(other)
        other.setSelectedRange(NSRange(location: 0, length: 10))
        RunLoop.main.run(until: Date().addingTimeInterval(0.1))
        print("probe after click responder \(type(of: window.firstResponder!)) focused \(model.focused)")
        model.tick += 1
        host.layoutSubtreeIfNeeded()
        RunLoop.main.run(until: Date().addingTimeInterval(0.2))
        print("probe after redraw responder \(type(of: window.firstResponder!)) focused \(model.focused)")
    }
}
#endif

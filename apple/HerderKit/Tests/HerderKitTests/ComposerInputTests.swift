import Foundation
import Herder
import SwiftUI
import Testing
#if os(macOS)
import AppKit
#endif
@testable import HerderKit

#if os(macOS)
/// The Mac composer's text view, driven as the keyboard would, without a window.
@MainActor
struct PromptEditorTests {
    final class Box {
        var text = ""
        var submitted = 0
    }

    func editor(_ initial: String) -> (ChipTextView, PromptEditor.Coordinator, Box) {
        let box = Box()
        box.text = initial
        let editor = PromptEditor(
            text: Binding(get: { box.text }, set: { box.text = $0 }), focused: .constant(true),
            images: [], pastes: [], addImages: { _ in "" }, addPaste: { _ in "" }, submit: { box.submitted += 1 })
        let coordinator = editor.makeCoordinator()
        let view = ChipTextView(usingTextLayoutManager: true)
        view.allowsUndo = true
        coordinator.attach(view)
        coordinator.show(initial, in: view)
        return (view, coordinator, box)
    }

    @Test func shiftEnterAtTheEndContinuesTheListWithTheCaretAfterTheMarker() {
        let (view, coordinator, box) = editor("1. asd")
        coordinator.newline(in: view)
        #expect(box.text == "1. asd\n2. ")
        #expect(view.string == "1. asd\n2. ")
        #expect(view.selectedRange() == NSRange(location: 10, length: 0))
    }

    @Test func shiftEnterOnAnEmptyItemEndsTheList() {
        let (view, coordinator, box) = editor("1. asd\n2. ")
        coordinator.newline(in: view)
        #expect(box.text == "1. asd\n")
        #expect(view.selectedRange() == NSRange(location: 7, length: 0))
    }

    @Test func shiftEnterMidTextBreaksTheLineAtTheCaret() {
        let (view, coordinator, box) = editor("hello world")
        view.setSelectedRange(NSRange(location: 5, length: 0))
        coordinator.newline(in: view)
        #expect(box.text == "hello\n world")
        #expect(view.selectedRange() == NSRange(location: 6, length: 0))
    }

    @Test func undoTakesTheContinuationBack() throws {
        let (view, coordinator, box) = editor("1. asd")
        // A text view's undo manager is its window's.
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 300, height: 100), styleMask: [], backing: .buffered, defer: true)
        window.contentView = view
        coordinator.newline(in: view)
        let undo = try #require(view.undoManager)
        #expect(undo.canUndo)
        undo.undo()
        #expect(box.text == "1. asd")
        #expect(view.selectedRange() == NSRange(location: 6, length: 0))
    }

    @Test func plainEnterSubmits() {
        let (view, coordinator, box) = editor("send me")
        #expect(coordinator.textView(view, doCommandBy: #selector(NSResponder.insertNewline(_:))))
        #expect(box.submitted == 1)
        #expect(box.text == "send me")
    }

    /// A pasted image's chip goes in before the binding holds the image; once it does, the chip
    /// draws again, with its thumbnail and size.
    @Test func aChipDrawsAgainOnceItsImageArrives() throws {
        let (view, coordinator, box) = editor("")
        coordinator.insert("[Image #1] ", in: view)
        let before = try #require(view.attributedString().attribute(.attachment, at: 0, effectiveRange: nil) as? ChipAttachment)
        // A 1×1 PNG.
        let png = try #require(Data(base64Encoded: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg=="))
        let image = Herder.Image(mediaType: "image/png", data: png)
        coordinator.parent = PromptEditor(
            text: Binding(get: { box.text }, set: { box.text = $0 }), focused: .constant(true),
            images: [image], pastes: [], addImages: { _ in "" }, addPaste: { _ in "" }, submit: {})
        coordinator.refresh(view)
        let after = try #require(view.attributedString().attribute(.attachment, at: 0, effectiveRange: nil) as? ChipAttachment)
        #expect(after !== before)
        #expect(coordinator.chip(after.token).detail != "Zero KB")
    }

    @Test func textAddedAtTheEndKeepsTheCaretAtTheEnd() {
        let (view, coordinator, _) = editor("I said")
        coordinator.show("I said more", in: view)
        #expect(view.selectedRange() == NSRange(location: 11, length: 0))
    }
}
#endif

@MainActor
struct DictationTests {
    @Test func deniedMicrophoneSaysWhereToAllowIt() async {
        let dictation = Dictation { .microphoneDenied }
        await dictation.start { _ in }
        #expect(!dictation.listening)
        #expect(dictation.error?.contains("Microphone") == true)
    }

    @Test func stoppingWhileGettingReadyCancelsTheStart() async {
        final class Box: @unchecked Sendable { var dictation: Dictation? }
        let box = Box()
        let dictation = Dictation {
            await MainActor.run { box.dictation?.stop() }
            return .granted
        }
        box.dictation = dictation
        await dictation.start { _ in }
        #expect(!dictation.listening)
        #expect(dictation.error == nil)
    }

    /// Without these the system refuses the microphone without asking.
    @Test func theAppDeclaresWhatDictationNeeds() throws {
        let apple = URL(filePath: #filePath).deletingLastPathComponent().appending(path: "../../..").standardized
        let project = try String(contentsOf: apple.appending(path: "project.yml"), encoding: .utf8)
        #expect(project.contains("INFOPLIST_KEY_NSMicrophoneUsageDescription:"))
        let entitlements = try PropertyListSerialization.propertyList(
            from: Data(contentsOf: apple.appending(path: "App/herder.entitlements")), format: nil) as? [String: Any]
        #expect(entitlements?["com.apple.security.device.audio-input"] as? Bool == true)
    }
}

#if os(macOS)
@MainActor
struct PromptEditorMeasureTests {
    @Test func measuringLeavesTheTextViewAlone() {
        let text = NSAttributedString(string: String(repeating: "word ", count: 200), attributes: PromptEditor.attributes)
        let narrow = PromptEditor.height(of: text, width: 200)
        let wide = PromptEditor.height(of: text, width: 800)
        #expect(narrow > wide)
        #expect(wide > 0)
    }
}
#endif

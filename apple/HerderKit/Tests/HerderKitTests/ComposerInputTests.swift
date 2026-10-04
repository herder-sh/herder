import Foundation
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

    @Test func textAddedAtTheEndKeepsTheCaretAtTheEnd() {
        let (view, coordinator, _) = editor("I said")
        coordinator.show("I said more", in: view)
        #expect(view.selectedRange() == NSRange(location: 11, length: 0))
    }
}
#endif

@MainActor
struct DictationTests {
    @Test func deniedSpeechRecognitionSaysWhereToAllowIt() async {
        let dictation = Dictation { .speechDenied }
        await dictation.start { _ in }
        #expect(!dictation.listening)
        #expect(dictation.error?.contains("Speech Recognition") == true)
    }

    @Test func deniedMicrophoneSaysWhereToAllowIt() async {
        let dictation = Dictation { .microphoneDenied }
        await dictation.start { _ in }
        #expect(!dictation.listening)
        #expect(dictation.error?.contains("Microphone") == true)
    }

    @Test func dictationTurnedOffInSettingsIsExplained() {
        let off = NSError(domain: "kLSRErrorDomain", code: 201, userInfo: [NSLocalizedDescriptionKey: "Siri and Dictation are disabled"])
        #expect(Dictation.message(for: off) == "Turn on Dictation in System Settings › Keyboard to dictate.")
        #expect(Dictation.message(for: NSError(domain: "kAFAssistantErrorDomain", code: 216)) == nil)
        #expect(Dictation.message(for: NSError(domain: "x", code: 1, userInfo: [NSLocalizedDescriptionKey: "boom"])) == "Dictation stopped: boom")
    }

    /// Without these the system refuses the microphone and the recognizer without asking.
    @Test func theAppDeclaresWhatDictationNeeds() throws {
        let apple = URL(filePath: #filePath).deletingLastPathComponent().appending(path: "../../..").standardized
        let project = try String(contentsOf: apple.appending(path: "project.yml"), encoding: .utf8)
        #expect(project.contains("INFOPLIST_KEY_NSMicrophoneUsageDescription:"))
        #expect(project.contains("INFOPLIST_KEY_NSSpeechRecognitionUsageDescription:"))
        let entitlements = try PropertyListSerialization.propertyList(
            from: Data(contentsOf: apple.appending(path: "App/herder.entitlements")), format: nil) as? [String: Any]
        #expect(entitlements?["com.apple.security.device.audio-input"] as? Bool == true)
    }
}

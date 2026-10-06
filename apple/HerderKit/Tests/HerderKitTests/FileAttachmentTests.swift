import Foundation
import Herder
@testable import HerderKit
import Testing

struct FileAttachmentTests {
    @Test func aNameLosesFoldersAndControlCharactersAndKeepsItsExtensionWhenCut() {
        #expect(FileAttachment.name("report.xlsx") == "report.xlsx")
        #expect(FileAttachment.name("a/b\\c\nd.txt") == "a_b_c_d.txt")
        #expect(FileAttachment.name("") == "file")
        #expect(FileAttachment.name("..") == "file")
        let long = FileAttachment.name(String(repeating: "é", count: 200) + ".pdf")
        #expect(long.utf8.count <= Int(maxFileNameBytes()))
        #expect(long.hasSuffix("é.pdf"))
    }

    @Test func picturesGoAsImagesAndEverythingElseAsFiles() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("files-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        // A 1×1 PNG.
        let png = try #require(Data(base64Encoded: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg=="))
        let picture = dir.appendingPathComponent("shot.png")
        let sheet = dir.appendingPathComponent("report.xlsx")
        let notes = dir.appendingPathComponent("notes.txt")
        try png.write(to: picture)
        try Data("PK".utf8).write(to: sheet)
        try Data("hi".utf8).write(to: notes)
        let sorted = FileAttachment.sort([sheet, picture, notes, dir.appendingPathComponent("gone.csv")])
        #expect(sorted.images == [Herder.Image(mediaType: "image/png", data: png)])
        #expect(sorted.files == [PromptFile(name: "report.xlsx", data: Data("PK".utf8)),
                                 PromptFile(name: "notes.txt", data: Data("hi".utf8))])
        #expect(sorted.refused == "gone.csv cannot be read.")
    }

    @Test func aFileOverTheLimitIsRefused() {
        let big = Data(count: Int(maxFileBytes()) + 1)
        #expect(throws: ImageAttachment.Refused.self) { try FileAttachment.make(name: "big.bin", data: big) }
    }

    @Test func filesFitWithinWhatThePromptCarriesAlready() {
        let limit = Int(maxPromptAttachmentBytes())
        let half = PromptFile(name: "a.bin", data: Data(count: limit / 2))
        let small = PromptFile(name: "b.txt", data: Data(count: 10))
        let fitting = FileAttachment.fitting([half, half, small], carried: 20)
        #expect(fitting.files == [half, small])
        #expect(fitting.refused?.hasPrefix("a.bin would take the prompt over") == true)
        let all = FileAttachment.fitting([small], carried: 0)
        #expect(all.files == [small])
        #expect(all.refused == nil)
    }

    @Test func aFileShowsBySymbolOfItsType() {
        #expect(FileAttachment.symbol("spec.pdf") == "doc.richtext")
        #expect(FileAttachment.symbol("data.csv") == "tablecells")
        #expect(FileAttachment.symbol("bundle.zip") == "doc.zipper")
        #expect(FileAttachment.symbol("noext") == "doc")
    }
}

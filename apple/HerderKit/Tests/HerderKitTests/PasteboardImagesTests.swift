import Foundation
import Herder
import Testing
@testable import HerderKit

/// The images an iPhone's clipboard holds, read from its items as `UIPasteboard` gives them.
struct PasteboardImagesTests {
    /// A 1×1 PNG.
    let png = Data(base64Encoded: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg==")!

    @Test func anImageItemGivesItsBytesAsTheyAre() {
        let images = ImageAttachment.images(in: [["public.png": png, "public.utf8-plain-text": Data("x".utf8)]])
        #expect(images.count == 1)
        #expect(images.first?.mediaType == "image/png")
        #expect(images.first?.data == png)
    }

    @Test func textAloneGivesNoImages() {
        #expect(ImageAttachment.images(in: [["public.utf8-plain-text": Data("hello".utf8)]]).isEmpty)
        #expect(ImageAttachment.images(in: []).isEmpty)
    }

    @Test func eachItemGivesOneImageInOrder() {
        let images = ImageAttachment.images(in: [["public.png": png], ["public.text": Data()], ["public.png": png]])
        #expect(images.count == 2)
    }

    /// A photo copied on an iPhone comes as HEIC and often PNG too; the PNG goes as it is
    /// rather than the HEIC being converted to JPEG.
    @Test func aTypeTheDaemonTakesWinsOverOneToConvert() {
        let images = ImageAttachment.images(in: [["public.heic": png, "public.png": png]])
        #expect(images.first?.mediaType == "image/png")
        #expect(images.first?.data == png)
    }
}

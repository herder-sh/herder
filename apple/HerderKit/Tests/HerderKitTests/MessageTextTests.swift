@testable import HerderKit
import Testing

struct MessageTextTests {
    let picture = MessagePicture(number: 1, mediaType: "image/png", size: 11_000, data: nil, attachmentId: "a1")

    @Test func anImageMarkerBecomesItsChipBetweenTheWords() {
        let lines = MessageText.lines("[Image #1] also this\nlike [Image #2]", pictures: [picture])
        #expect(lines == [
            [.picture(picture), .word(" "), .word("also "), .word("this")],
            [.word("like "), .word("[Image "), .word("#2]")],
        ])
    }

    @Test func onlyAMarkerWithAnImageBehindItDrawsAsChips() {
        #expect(MessageText.refers("see [Image #1]", to: [picture]))
        #expect(!MessageText.refers("see [Image #2]", to: [picture]))
        #expect(!MessageText.refers("see [Image #1]", to: []))
    }

    @Test func aPictureIsNamedByItsNumberAndType() {
        #expect(picture.name == "image.png")
        #expect(MessagePicture(number: 2, mediaType: "image/jpeg", size: 0, data: nil, attachmentId: nil).name == "image-2.jpg")
    }
}

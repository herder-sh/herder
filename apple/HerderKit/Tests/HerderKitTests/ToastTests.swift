import Herder
@testable import HerderKit
import Testing

struct ToastTests {
    @Test func aFailureKeepsItsHeadlineApartFromTheError() {
        let toast = Toast.failure("Could not hand off to trash-can-01",
                                  HerderError.Local(detail: "cannot read the session: no such file"))
        #expect(toast.text == "Could not hand off to trash-can-01")
        #expect(toast.detail == "cannot read the session: no such file")
        #expect(toast.failed)
        #expect(!Toast(text: "Handed off to trash-can-01").failed)
    }

    @Test func anErrorChainGoesOneLinkALine() {
        #expect(chainLines("cannot read the session: open /tmp/x.jsonl: No such file (os error 2)")
            == "cannot read the session\nopen /tmp/x.jsonl\nNo such file (os error 2)")
        #expect(chainLines("no chain at http://host:8080") == "no chain at http://host:8080")
        #expect(chainLines("trailing: ") == "trailing")
    }
}

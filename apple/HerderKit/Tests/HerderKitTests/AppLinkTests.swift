import Foundation
import Herder
@testable import HerderKit
import Testing

struct AppLinkTests {
    let pair = "herder://pair?host=127.0.0.1%3A7420&fp=" + String(repeating: "ab", count: 32) + "&code=ABCD2345"

    private func link(_ text: String) -> AppLink? { URL(string: text).flatMap(AppLink.init(url:)) }

    @Test func readsASessionLink() {
        #expect(link("herder://session?host=fake-host-1&session=01JABC")
            == .session(SessionKey(hostId: "fake-host-1", sessionId: "01JABC")))
        #expect(link("herder://session?session=01JABC&host=fake-host-1")
            == .session(SessionKey(hostId: "fake-host-1", sessionId: "01JABC")))
    }

    @Test func readsAPairLinkOfOneMachineOrSeveral() {
        #expect(link(pair) == .pair(pair))
        let shared = pair + "&host=10.0.0.9%3A7447&fp=" + String(repeating: "cd", count: 32) + "&code=MNPQR-STVWX"
        #expect(link(shared) == .pair(shared))
    }

    @Test func rejectsAMalformedLink() {
        #expect(link("herder://session?host=fake-host-1") == nil)
        #expect(link("herder://session?session=01JABC") == nil)
        #expect(link("herder://session?host=&session=01JABC") == nil)
        #expect(link("herder://pair?host=127.0.0.1%3A7420") == nil)
    }

    @Test func rejectsAnUnknownLink() {
        #expect(link("herder://settings?host=fake-host-1") == nil)
        #expect(link("https://session?host=fake-host-1&session=01JABC") == nil)
        #expect(link("herder:") == nil)
    }

    @Test func aSessionLinkOpensOnlyWhatItsMachineLists() {
        let key = SessionKey(hostId: "host-a", sessionId: "01A")
        let listed = machine("host-a", name: "a", sessions: ["01A"])
        #expect(SessionLinkTarget(key, machines: [listed]) == .open)
        #expect(SessionLinkTarget(key, machines: [machine("host-b", name: "b", sessions: ["01A"])]) == .unpaired)
        #expect(SessionLinkTarget(key, machines: [machine("host-a", name: "a", sessions: [])]) == .notFound)
        #expect(SessionLinkTarget(key, machines: [machine("host-a", name: "a", sessions: [], connection: .connecting)])
            == .connecting)
        var archived = listed
        archived.sessions[0].status = .archived
        #expect(SessionLinkTarget(key, machines: [archived]) == .archived)
    }
}

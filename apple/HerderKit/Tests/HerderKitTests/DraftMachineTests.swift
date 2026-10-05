@testable import HerderKit
import Testing

struct DraftMachineTests {
    @Test func aNewSessionStartsOnTheMachineLastPicked() {
        #expect(Draft.machine(among: ["mac", "box"], last: "box", newest: "mac") == "box")
    }

    @Test func aMachineWithoutTheProjectFallsBackToTheNewestSessionsThenTheFirst() {
        #expect(Draft.machine(among: ["mac", "box"], last: "gone", newest: "box") == "box")
        #expect(Draft.machine(among: ["mac", "box"], last: nil, newest: nil) == "mac")
        #expect(Draft.machine(among: [], last: "box", newest: "box") == nil)
    }
}

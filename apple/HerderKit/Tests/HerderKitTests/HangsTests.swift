@testable import HerderKit
import Testing

struct HangsTests {
    @Test func aTurnOfATenthOfASecondOrLongerIsLoggedWithItsTimeOnTheCPU() {
        let woke = Hangs.Moment(time: 1_000_000_000, cpu: 50_000_000)
        // 250 ms, 180 of them running: the rest it waited.
        #expect(woke.hang(until: Hangs.Moment(time: 1_250_000_000, cpu: 230_000_000)) == "herder-hang ms=250 cpu=180")
        #expect(woke.hang(until: Hangs.Moment(time: 1_100_000_000, cpu: 50_000_000)) == "herder-hang ms=100 cpu=0")
    }

    @Test func aShorterTurnIsNot() {
        let woke = Hangs.Moment(time: 1_000_000_000, cpu: 50_000_000)
        #expect(woke.hang(until: Hangs.Moment(time: 1_099_999_999, cpu: 149_999_999)) == nil)
        #expect(woke.hang(until: woke) == nil)
    }
}

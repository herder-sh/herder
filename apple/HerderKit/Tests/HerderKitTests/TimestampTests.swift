import Foundation
@testable import HerderKit
import Testing

struct TimestampTests {
    @Test(arguments: [
        ("2026-10-04T15:27:59Z", 0.0),
        ("2026-10-04T15:27:59.7Z", 0.7),
        ("2026-10-04T15:27:59.708Z", 0.708),
        ("2026-10-04T15:27:59.280485Z", 0.280485),
        ("2026-10-04T15:27:59.240496823Z", 0.240496823),
        ("2026-10-04T18:27:59.708+03:00", 0.708),
    ])
    func parsesRFC3339WithAnyFraction(text: String, fraction: Double) throws {
        let date = try #require(Timestamp.date(text))
        let whole = Date(timeIntervalSince1970: 1_791_127_679)
        #expect(abs(date.timeIntervalSince(whole) - fraction) < 0.001)
    }

    @Test func rejectsWhatIsNoTimestamp() {
        #expect(Timestamp.date("yesterday") == nil)
        #expect(Timestamp.date("") == nil)
    }
}

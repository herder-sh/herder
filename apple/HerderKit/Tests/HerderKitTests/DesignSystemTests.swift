import Foundation
@testable import HerderKit
import SwiftUI
import Testing

@MainActor struct DesignSystemTests {
    private struct Account: Identifiable, Equatable {
        let id: String
        let left: Int
    }

    private let columns: [SortableColumn<Account>] = [
        SortableColumn("Account", value: \.id) { _ in EmptyView() },
        SortableColumn("Left", value: \.left) { _ in EmptyView() },
        SortableColumn("Actions") { _ in EmptyView() },
    ]
    private let accounts = [
        Account(id: "work-10", left: 40), Account(id: "Personal", left: 90),
        Account(id: "work-9", left: 40), Account(id: "aws", left: 10),
    ]

    @Test func aTableSortsByTheClickedColumnEitherWay() {
        let ascending = SortableTable.sorted(accounts, columns, by: TableSort(column: "Left"))
        #expect(ascending.map(\.id) == ["aws", "work-10", "work-9", "Personal"])
        let descending = SortableTable.sorted(accounts, columns, by: TableSort(column: "Left", ascending: false))
        #expect(descending.map(\.id) == ["Personal", "work-10", "work-9", "aws"])
    }

    @Test func textSortsAsFinderDoes() {
        let sorted = SortableTable.sorted(accounts, columns, by: TableSort(column: "Account"))
        #expect(sorted.map(\.id) == ["aws", "Personal", "work-9", "work-10"])
    }

    @Test func aColumnThatDoesNotSortKeepsTheOrderGiven() {
        #expect(SortableTable.sorted(accounts, columns, by: TableSort(column: "Actions")) == accounts)
        #expect(SortableTable.sorted(accounts, columns, by: TableSort(column: "Gone")) == accounts)
    }

    @Test func clickingTheSortedColumnFlipsItAndAnotherSortsAscending() {
        let sort = TableSort(column: "Left")
        #expect(sort.toggled("Left") == TableSort(column: "Left", ascending: false))
        #expect(sort.toggled("Left").toggled("Left") == sort)
        #expect(TableSort(column: "Left", ascending: false).toggled("Account") == TableSort(column: "Account"))
    }

    @Test func aDeviceKeepsTheSortAsText() {
        for sort in [TableSort(column: "Session left", ascending: false), TableSort(column: "a|b")] {
            #expect(TableSort(stored: sort.stored) == sort)
        }
        #expect(TableSort(stored: "") == nil)
        #expect(TableSort(stored: "Left|sideways") == nil)
    }

    @Test func theSignalStripHidesZeros() {
        let signals = [
            Signal(id: "needs", count: 0, label: "need you", urgency: .needsYou),
            Signal(id: "running", count: 3, label: "running", urgency: .running),
            Signal(id: "failed", count: 1, label: "failed", urgency: .failing),
        ]
        #expect(SignalStrip.shown(signals).map(\.id) == ["running", "failed"])
        #expect(SignalStrip.shown([signals[0]]).isEmpty)
    }

    @Test func aSignalFiltersAndASecondTapClearsIt() {
        #expect(SignalStrip.toggled(nil, "running") == "running")
        #expect(SignalStrip.toggled("running", "running") == nil)
        #expect(SignalStrip.toggled("failed", "running") == "running")
    }

    @Test func timesAreRelativeWithTheExactOneAside() {
        let now = Date(timeIntervalSince1970: 1_791_127_679)
        #expect(RelativeTime.text(now.addingTimeInterval(-20), now: now) == "just now")
        // ICU puts a no-break space between the number and its unit.
        let minutes = RelativeTime.text(now.addingTimeInterval(-190), now: now).replacing("\u{A0}", with: " ")
        #expect(minutes.hasPrefix("3 min") && minutes.hasSuffix("ago"))
        let exact = RelativeTime.exact(now)
        #expect(exact.contains("2026") && !exact.contains("2026-10"))
    }
}

import SwiftUI

/// A table's sort: a column by its title, and which way.
struct TableSort: Equatable {
    var column: String
    var ascending = true

    /// The sort after a click on `column`'s header: the other way on the sorted column, else
    /// that column ascending.
    func toggled(_ column: String) -> TableSort {
        column == self.column ? TableSort(column: column, ascending: !ascending) : TableSort(column: column)
    }

    /// How a device keeps it: "Session left|desc".
    var stored: String { "\(column)|\(ascending ? "asc" : "desc")" }

    init(column: String, ascending: Bool = true) {
        self.column = column
        self.ascending = ascending
    }

    init?(stored: String) {
        guard let bar = stored.lastIndex(of: "|"), ["asc", "desc"].contains(stored[stored.index(after: bar)...])
        else { return nil }
        self.init(column: String(stored[..<bar]), ascending: stored.hasSuffix("|asc"))
    }
}

/// A column of a `SortableTable`: its title, its cell, and the value it sorts by, if it sorts.
struct SortableColumn<Row> {
    let title: String
    /// Its fixed width; nil shares out the room the fixed ones leave.
    var width: CGFloat?
    var alignment: Alignment = .leading
    let less: ((Row, Row) -> Bool)?
    let cell: (Row) -> AnyView

    init<Value: Comparable, Cell: View>(
        _ title: String, width: CGFloat? = nil, alignment: Alignment = .leading,
        value: @escaping (Row) -> Value, @ViewBuilder cell: @escaping (Row) -> Cell
    ) {
        self.init(title, width: width, alignment: alignment, less: { value($0) < value($1) }, cell: cell)
    }

    /// Sorts text as Finder does: case-insensitive, numbers by value.
    init<Cell: View>(
        _ title: String, width: CGFloat? = nil, alignment: Alignment = .leading,
        value: @escaping (Row) -> String, @ViewBuilder cell: @escaping (Row) -> Cell
    ) {
        self.init(title, width: width, alignment: alignment,
                  less: { value($0).localizedStandardCompare(value($1)) == .orderedAscending }, cell: cell)
    }

    /// A column that does not sort.
    init<Cell: View>(
        _ title: String, width: CGFloat? = nil, alignment: Alignment = .leading,
        @ViewBuilder cell: @escaping (Row) -> Cell
    ) {
        self.init(title, width: width, alignment: alignment, less: nil, cell: cell)
    }

    private init<Cell: View>(
        _ title: String, width: CGFloat?, alignment: Alignment, less: ((Row, Row) -> Bool)?,
        cell: @escaping (Row) -> Cell
    ) {
        self.title = title
        self.width = width
        self.alignment = alignment
        self.less = less
        self.cell = { AnyView(cell($0)) }
    }
}

/// A collection as a table: a header whose columns sort on a click, with an arrow on the sorted
/// one, then rows split by hairlines, its first column on the page's content edge. Each device keeps each table's sort, under its `id`.
/// `open` makes a row a button that opens its object.
struct SortableTable<Row: Identifiable>: View {
    let rows: [Row]
    let columns: [SortableColumn<Row>]
    var open: ((Row) -> Void)?
    private let initial: TableSort
    @AppStorage private var stored: String
    @State private var hovered: Row.ID?

    init(_ id: String, rows: [Row], sortedBy initial: TableSort, columns: [SortableColumn<Row>],
         open: ((Row) -> Void)? = nil) {
        self.rows = rows
        self.columns = columns
        self.open = open
        self.initial = initial
        _stored = AppStorage(wrappedValue: initial.stored, "table.\(id).sort")
    }

    private var sort: TableSort {
        TableSort(stored: stored).flatMap { sort in
            columns.contains { $0.title == sort.column && $0.less != nil } ? sort : nil
        } ?? initial
    }

    var body: some View {
        let sort = sort
        VStack(spacing: 0) {
            header(sort)
            LazyVStack(spacing: 0) {
                ForEach(Self.sorted(rows, columns, by: sort)) { row in
                    line(row)
                }
            }
        }
    }

    /// `rows` in `sort`'s order; as given when its column does not sort. Equal rows keep their
    /// order.
    static func sorted(_ rows: [Row], _ columns: [SortableColumn<Row>], by sort: TableSort) -> [Row] {
        guard let less = columns.first(where: { $0.title == sort.column })?.less else { return rows }
        return rows.sorted { sort.ascending ? less($0, $1) : less($1, $0) }
    }

    private func header(_ sort: TableSort) -> some View {
        HStack(spacing: Theme.cellPadding) {
            ForEach(columns.indices, id: \.self) { index in
                let column = columns[index]
                let sorted = column.title == sort.column
                Group {
                    if column.less != nil {
                        Button { stored = sort.toggled(column.title).stored } label: {
                            HStack(spacing: 3) {
                                Text(column.title)
                                if sorted {
                                    Image(systemName: sort.ascending ? "arrow.up" : "arrow.down")
                                        .font(.caption2.weight(.bold)).foregroundStyle(Theme.secondary)
                                }
                            }
                            .contentShape(.rect)
                        }
                        .buttonStyle(.plain)
                        .help("Sort by \(column.title)")
                    } else {
                        Text(column.title)
                    }
                }
                .font(Theme.meta.weight(.medium))
                .foregroundStyle(sorted ? Theme.secondary : Theme.tertiary)
                .lineLimit(1)
                .cell(column)
            }
        }
        .padding(.vertical, 6)
        .overlay(alignment: .bottom) { Hairline() }
    }

    @ViewBuilder private func line(_ row: Row) -> some View {
        let cells = HStack(spacing: Theme.cellPadding) {
            ForEach(columns.indices, id: \.self) { index in
                columns[index].cell(row).cell(columns[index])
            }
        }
        .font(Theme.body)
        .foregroundStyle(Theme.text)
        .padding(.vertical, 8)
        .frame(minHeight: Theme.rowHeight)
        // The hover fill reaches past the content edge, which stays on the page's.
        .background((hovered == row.id ? Theme.raised : .clear).padding(.horizontal, -Theme.cellPadding / 2))
        .overlay(alignment: .bottom) { Hairline() }
        .contentShape(.rect)
        .onHover { inside in
            if inside { hovered = row.id } else if hovered == row.id { hovered = nil }
        }
        if let open {
            Button { open(row) } label: { cells }.buttonStyle(.plain)
        } else {
            cells
        }
    }
}

/// The hairline between rows.
private struct Hairline: View {
    var body: some View { Rectangle().fill(Theme.stroke).frame(height: 1) }
}

private extension View {
    func cell<Row>(_ column: SortableColumn<Row>) -> some View {
        frame(minWidth: column.width, maxWidth: column.width ?? .infinity, alignment: column.alignment)
    }
}

#Preview("SortableTable") {
    struct Account: Identifiable {
        let id: String
        let machines: Int
        let left: Double
        let cost: Double
    }
    return SortableTable("preview.accounts", rows: [
        Account(id: "work", machines: 3, left: 0.62, cost: 41.2),
        Account(id: "personal", machines: 1, left: 0.18, cost: 7.9),
        Account(id: "team-2", machines: 2, left: 0.91, cost: 18.4),
    ], sortedBy: TableSort(column: "Session left", ascending: false), columns: [
        SortableColumn("Account", value: \.id) { Text($0.id) },
        SortableColumn("Machines", width: 80, alignment: .trailing, value: \.machines) { Text("\($0.machines)") },
        SortableColumn("Session left", width: 110, value: \.left) { UsageBar(percent: (1 - $0.left) * 100) },
        SortableColumn("7 d cost", width: 80, alignment: .trailing, value: \.cost) {
            Text($0.cost, format: .currency(code: "USD")).monospacedDigit()
        },
    ])
    .frame(width: 620)
    .padding()
    .background(Theme.background)
}

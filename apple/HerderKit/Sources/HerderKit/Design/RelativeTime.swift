import SwiftUI

/// A moment as the UI shows it: "3 min. ago", kept current, with the exact time on hover on
/// the Mac and on a long press on iOS. The UI never shows a raw timestamp.
struct RelativeTime: View {
    let date: Date

    var body: some View {
        TimelineView(.periodic(from: .now, by: 30)) { context in
            Text(Self.text(date, now: context.date))
        }
        .help(Self.exact(date))
        #if os(iOS)
        .contextMenu { Text(Self.exact(date)) }
        #endif
    }

    /// "just now" under a minute, then "3 min. ago", "yesterday", "last wk.".
    @MainActor static func text(_ date: Date, now: Date = .now) -> String {
        if abs(now.timeIntervalSince(date)) < 60 { return "just now" }
        return formatter.localizedString(for: date, relativeTo: now)
    }

    /// The exact time, for hover and long press: "25 Oct 2026 at 21:04:49".
    static func exact(_ date: Date) -> String { date.formatted(date: .abbreviated, time: .standard) }

    @MainActor private static let formatter = {
        let formatter = RelativeDateTimeFormatter()
        formatter.dateTimeStyle = .named
        formatter.unitsStyle = .short
        return formatter
    }()
}

#Preview("RelativeTime") {
    VStack(alignment: .leading, spacing: 6) {
        ForEach([20.0, 190, 4000, 90000, 800000], id: \.self) { seconds in
            RelativeTime(date: .now.addingTimeInterval(-seconds))
        }
    }
    .font(Theme.meta).foregroundStyle(Theme.tertiary)
    .padding()
    .background(Theme.canvas)
}

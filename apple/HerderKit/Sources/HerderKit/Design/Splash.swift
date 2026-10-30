import Herder
import SwiftUI

/// What shows while the app opens with nothing to show yet: the launch screen's background,
/// then herder.sh's pixel wordmark etched in row by row in its colour bands, until the Board has
/// sessions, from the offline cache or a machine, or the machines have answered. A launch with
/// a cached Board never waits on a machine, nor on the wordmark.
struct Splash: View {
    let fleet: Fleet
    @State private var rows = 0
    @State private var shown = true
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        if shown {
            ZStack {
                Theme.canvas.ignoresSafeArea()
                VStack(spacing: 22) {
                    Wordmark(rows: rows)
                        .frame(maxWidth: 260)
                        .aspectRatio(Wordmark.size.width / Wordmark.size.height, contentMode: .fit)
                    Text("Every coding agent. Every account. Every machine.")
                        .font(Theme.monoSmall)
                        .foregroundStyle(Theme.tertiary)
                        .opacity(rows == Wordmark.rows ? 1 : 0)
                        .animation(.easeOut(duration: 0.4), value: rows)
                }
                .padding(32)
            }
            // The fleet's views are there underneath and take touches as it fades.
            .allowsHitTesting(false)
            .accessibilityHidden(true)
            .transition(.opacity)
            .task { await etch() }
            .task {
                // A moment at most: one asleep machine must not hold an empty Board.
                try? await Task.sleep(for: .seconds(1.5))
                hide()
            }
            .onChange(of: fleet.ready, initial: true) { _, ready in
                if ready { hide() }
            }
        }
    }

    private func etch() async {
        // Not before a moment has passed: a launch with a cached Board is over by then, and the
        // splash goes before any of the wordmark shows.
        try? await Task.sleep(for: .milliseconds(300))
        if reduceMotion {
            rows = Wordmark.rows
            return
        }
        // As the site's wordmark: 14 steps over 0.9 s.
        for row in 1...Wordmark.rows {
            try? await Task.sleep(for: .milliseconds(65))
            guard !Task.isCancelled else { return }
            rows = row
        }
    }

    private func hide() {
        guard shown else { return }
        withAnimation(.easeOut(duration: 0.2)) { shown = false }
        Launch.mark(.shown)
    }
}

extension Fleet {
    /// Whether the machines have answered: one has connected, or none is still connecting.
    var opened: Bool {
        machines.contains { $0.connection == .connected }
            || !machines.contains { if case .connecting = $0.connection { true } else { false } }
    }

    /// Whether there is something to show: the Board lists a task, or the machines have answered.
    var ready: Bool { opened || !lists.board().isEmpty }
}

/// herder.sh's wordmark: 10-point pixel rows, coloured in the site's five Tokyo Night bands.
/// Only its top `rows` rows are drawn.
struct Wordmark: View {
    var rows = Wordmark.rows

    static let size = CGSize(width: 600, height: 140)
    static let rows = 14

    var body: some View {
        Canvas { context, canvasSize in
            let scale = canvasSize.width / Self.size.width
            for (x, y, width) in Self.runs where y < CGFloat(rows) * 10 {
                let rect = CGRect(x: x * scale, y: y * scale, width: width * scale, height: 10 * scale)
                context.fill(Path(rect), with: .color(Self.band(y)))
            }
        }
    }

    /// The band a row is in, as the site's gradient stops fall.
    private static func band(_ y: CGFloat) -> Color {
        let rgb: UInt32 = switch y {
        case ..<40: 0xBB9AF7
        case ..<60: 0x7AA2F7
        case ..<90: 0x7DCFFF
        case ..<110: 0x9ECE6A
        default: 0xE0AF68
        }
        return Color(light: rgb, dark: rgb)
    }

    /// Each pixel run: its left edge, row and width, from the site's wordmark path.
    private static let runs: [(CGFloat, CGFloat, CGFloat)] = path.matches(of: /M(\d+) (\d+)h(\d+)/)
        .compactMap { match -> (CGFloat, CGFloat, CGFloat)? in
        guard let x = Double(match.output.1), let y = Double(match.output.2), let width = Double(match.output.3) else {
            return nil
        }
        return (CGFloat(x), CGFloat(y), CGFloat(width))
    }

    // site/index.html's `.wordmark` path.
    private static let path = """
        M10 0h50v10h-50zM390 0h20v10h-20zM0 10h60v10h-60zM390 10h20v10h-20zM0 20h20v10h-20zM40 20h20v10h-20z\
        M390 20h20v10h-20zM0 30h20v10h-20zM40 30h20v10h-20zM390 30h20v10h-20zM40 40h70v10h-70zM150 40h60v10h-60z\
        M240 40h20v10h-20zM270 40h40v10h-40zM340 40h70v10h-70zM440 40h60v10h-60zM530 40h20v10h-20z\
        M560 40h40v10h-40zM40 50h80v10h-80zM140 50h80v10h-80zM240 50h70v10h-70zM330 50h80v10h-80z\
        M430 50h80v10h-80zM530 50h70v10h-70zM40 60h20v10h-20zM100 60h20v10h-20zM140 60h20v10h-20z\
        M200 60h20v10h-20zM240 60h30v10h-30zM290 60h20v10h-20zM330 60h20v10h-20zM390 60h20v10h-20z\
        M430 60h20v10h-20zM490 60h20v10h-20zM530 60h30v10h-30zM580 60h20v10h-20zM40 70h20v10h-20z\
        M100 70h20v10h-20zM140 70h20v10h-20zM200 70h20v10h-20zM240 70h20v10h-20zM330 70h20v10h-20z\
        M390 70h20v10h-20zM430 70h20v10h-20zM490 70h20v10h-20zM530 70h20v10h-20zM40 80h20v10h-20z\
        M100 80h20v10h-20zM140 80h80v10h-80zM240 80h20v10h-20zM330 80h20v10h-20zM390 80h20v10h-20z\
        M430 80h80v10h-80zM530 80h20v10h-20zM40 90h20v10h-20zM100 90h20v10h-20zM140 90h80v10h-80z\
        M240 90h20v10h-20zM330 90h20v10h-20zM390 90h20v10h-20zM430 90h80v10h-80zM530 90h20v10h-20z\
        M40 100h20v10h-20zM100 100h20v10h-20zM140 100h20v10h-20zM240 100h20v10h-20zM330 100h20v10h-20z\
        M390 100h20v10h-20zM430 100h20v10h-20zM530 100h20v10h-20zM40 110h20v10h-20zM100 110h20v10h-20z\
        M140 110h20v10h-20zM240 110h20v10h-20zM330 110h20v10h-20zM390 110h20v10h-20zM430 110h20v10h-20z\
        M530 110h20v10h-20zM40 120h20v10h-20zM100 120h20v10h-20zM140 120h80v10h-80zM240 120h20v10h-20z\
        M330 120h80v10h-80zM430 120h80v10h-80zM530 120h20v10h-20zM40 130h20v10h-20zM100 130h20v10h-20z\
        M150 130h70v10h-70zM240 130h20v10h-20zM340 130h70v10h-70zM440 130h70v10h-70zM530 130h20v10h-20z
        """
}

import OSLog
import SwiftUI

/// The transcript's scroll geometry in whole points: what the transcript log tells, and what
/// it compares to tell only changes.
struct TranscriptScrollState: Equatable {
    var contentHeight: CGFloat
    var offsetY: CGFloat
    var visible: CGRect
    var container: CGSize
    var insetTop: CGFloat
    var insetBottom: CGFloat

    /// Where the view's rect lies against the content.
    enum Placement: String {
        /// Within the scroll range.
        case inside
        /// Scrolled past the content's top.
        case aboveTop
        /// Scrolled past the content's end.
        case pastEnd
        /// Showing none of the content: what a blank transcript would look like.
        case outside
    }

    init(contentHeight: CGFloat, offsetY: CGFloat, visible: CGRect, container: CGSize,
         insetTop: CGFloat = 0, insetBottom: CGFloat = 0) {
        self.contentHeight = contentHeight.rounded()
        self.offsetY = offsetY.rounded()
        self.visible = CGRect(x: visible.minX.rounded(), y: visible.minY.rounded(),
                              width: visible.width.rounded(), height: visible.height.rounded())
        self.container = CGSize(width: container.width.rounded(), height: container.height.rounded())
        self.insetTop = insetTop.rounded()
        self.insetBottom = insetBottom.rounded()
    }

    init(_ geometry: ScrollGeometry) {
        self.init(contentHeight: geometry.contentSize.height, offsetY: geometry.contentOffset.y,
                  visible: geometry.visibleRect, container: geometry.containerSize,
                  insetTop: geometry.contentInsets.top, insetBottom: geometry.contentInsets.bottom)
    }

    var placement: Placement {
        if contentHeight > 0 && (visible.minY >= contentHeight || visible.maxY <= 0) { return .outside }
        let top = -insetTop
        let end = max(contentHeight + insetBottom - container.height, top)
        if offsetY < top - 1 { return .aboveTop }
        if offsetY > end + 1 { return .pastEnd }
        return .inside
    }

    /// No block in view though the transcript has some: the blank transcript.
    static func blank(visible: Int, blocks: Int) -> Bool { visible == 0 && blocks > 0 }
}


/// Logs the transcript's scroll state, to find out why a long transcript goes blank. Numbers
/// and block ids only: a transcript's text is its owner's. A plain box, not observed: logging
/// redraws nothing. Lines that only tell the view moved come at most twice a second; a blank,
/// the view leaving its content, the end reached or left and blocks added come at once.
@MainActor final class TranscriptLog {
    private static let logger = Logger(subsystem: "sh.herder.Herder", category: "transcript")
    /// The least time between two lines that only tell the view moved.
    private static let interval: Duration = .milliseconds(500)

    private var state: TranscriptScrollState?
    private var placement = TranscriptScrollState.Placement.inside
    private var toldGeometry: ContinuousClock.Instant?
    private var toldVisible: ContinuousClock.Instant?
    private var first: String?
    private var last: String?
    /// How many blocks were in view; `nil` until the view first says.
    private var count: Int?
    private var blocks = 0
    private var atEnd = true
    /// When the transcript last went blank, while it is.
    private var blankSince: ContinuousClock.Instant?

    func appeared(_ key: SessionKey, blocks: Int) {
        self.blocks = blocks
        Self.logger.notice("appear \(Self.name(key), privacy: .public) \(self.summary, privacy: .public)")
    }

    func geometry(_ state: TranscriptScrollState, blocks: Int, key: SessionKey) {
        self.state = state
        self.blocks = blocks
        let placement = state.placement
        let moved = placement != self.placement
        self.placement = placement
        let now = ContinuousClock.now
        if moved && placement != .inside {
            Self.logger.error("outside-content \(Self.name(key), privacy: .public) \(self.summary, privacy: .public)")
        } else if moved || Self.due(toldGeometry, now) {
            Self.logger.notice("geometry \(Self.name(key), privacy: .public) \(self.summary, privacy: .public)")
        } else {
            return
        }
        toldGeometry = now
    }

    /// The ids of the blocks in view, top first.
    func visible(_ ids: [String], blocks: Int, key: SessionKey) {
        self.blocks = blocks
        let wasBlank = count.map { TranscriptScrollState.blank(visible: $0, blocks: blocks) } ?? false
        (first, last, count) = (ids.first, ids.last, ids.count)
        let blank = TranscriptScrollState.blank(visible: ids.count, blocks: blocks)
        let now = ContinuousClock.now
        if blank && !wasBlank {
            blankSince = now
            Self.logger.error("blank \(Self.name(key), privacy: .public) \(self.summary, privacy: .public)")
        } else if wasBlank && !blank {
            let ms = blankSince.map { Self.milliseconds(now - $0) } ?? "-"
            blankSince = nil
            Self.logger.notice("drawn after=\(ms, privacy: .public)ms \(Self.name(key), privacy: .public) \(self.summary, privacy: .public)")
        } else if Self.due(toldVisible, now) {
            Self.logger.notice("visible \(Self.name(key), privacy: .public) \(self.summary, privacy: .public)")
        } else {
            return
        }
        toldVisible = now
    }

    func blocks(from old: Int, to new: Int, key: SessionKey) {
        blocks = new
        Self.logger.notice("blocks \(old, privacy: .public)->\(new, privacy: .public) \(Self.name(key), privacy: .public) \(self.summary, privacy: .public)")
    }

    func atEnd(_ atEnd: Bool, blocks: Int, key: SessionKey) {
        self.atEnd = atEnd
        self.blocks = blocks
        Self.logger.notice("at-end=\(atEnd, privacy: .public) \(Self.name(key), privacy: .public) \(self.summary, privacy: .public)")
    }

    private static func due(_ told: ContinuousClock.Instant?, _ now: ContinuousClock.Instant) -> Bool {
        told.map { now - $0 >= interval } ?? true
    }

    private static func name(_ key: SessionKey) -> String { "session=\(key.hostId)/\(key.sessionId)" }

    private static func milliseconds(_ duration: Duration) -> String {
        String(format: "%.0f", Double(duration.components.seconds) * 1000 + Double(duration.components.attoseconds) / 1e15)
    }

    /// The last geometry and the blocks in view, on one line.
    private var summary: String {
        var line = "at-end=\(atEnd) blocks=\(blocks) visible=\(count.map(String.init) ?? "-")"
        line += " first=\(first ?? "-") last=\(last ?? "-")"
        if let state {
            let v = state.visible
            line += " content=\(Self.points(state.contentHeight)) offset=\(Self.points(state.offsetY))"
            line += " rect=\(Self.points(v.minX)),\(Self.points(v.minY)),\(Self.points(v.width))x\(Self.points(v.height))"
            line += " container=\(Self.points(state.container.width))x\(Self.points(state.container.height))"
            line += " insets=\(Self.points(state.insetTop)),\(Self.points(state.insetBottom))"
            line += " placement=\(state.placement.rawValue)"
        }
        return line
    }

    private static func points(_ value: CGFloat) -> String { String(format: "%.0f", Double(value)) }
}

/// How much of each session's transcript is drawn: its last blocks, enough to fill the view, and
/// a page more each time its top comes into view. The transcript is a plain stack, which lays
/// out every row it holds: holding a long session's every row took a second to switch to it.
struct TranscriptWindow {
    /// The blocks drawn at first, and added at a time: a folded turn is about three.
    static let page = 12
    /// How far the top of what is drawn may be from the view before a page more is drawn.
    static let margin = 5

    private var counts: [SessionKey: Int] = [:]

    /// The blocks of `key`'s transcript drawn now.
    func shown(_ blocks: [TranscriptBlock], of key: SessionKey) -> ArraySlice<TranscriptBlock> {
        blocks.suffix(counts[key] ?? Self.page)
    }

    /// Whether earlier blocks of `key`'s transcript wait undrawn.
    func hides(_ blocks: [TranscriptBlock], of key: SessionKey) -> Bool {
        blocks.count > counts[key] ?? Self.page
    }

    /// Draws a page more of `key`'s transcript.
    mutating func extend(_ key: SessionKey) {
        counts[key] = (counts[key] ?? Self.page) + Self.page
    }

    /// Draws a page more when `top`, the block at the top of the view, is near the top of what
    /// is drawn; returns whether it did.
    @discardableResult
    mutating func scrolled(to top: String?, in blocks: [TranscriptBlock], of key: SessionKey) -> Bool {
        guard hides(blocks, of: key), let top,
              let index = shown(blocks, of: key).firstIndex(where: { $0.id == top }) else { return false }
        guard index - (blocks.count - (counts[key] ?? Self.page)) < Self.margin else { return false }
        extend(key)
        return true
    }

    /// Draws `key`'s transcript from block `id` on, at least, so it can be scrolled to.
    mutating func reveal(_ id: String, in blocks: [TranscriptBlock], of key: SessionKey) {
        guard let index = blocks.firstIndex(where: { $0.id == id }) else { return }
        counts[key] = max(counts[key] ?? Self.page, blocks.count - index)
    }

    /// Goes back to drawing only the last page of `key`'s transcript, as it is left.
    mutating func reset(_ key: SessionKey) {
        counts[key] = nil
    }
}

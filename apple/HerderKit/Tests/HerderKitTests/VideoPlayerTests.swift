#if os(macOS)
import AppKit
import AVKit
import SwiftUI
import Testing

@MainActor struct VideoPlayerTests {
    /// The app aborted building VideoPlayer's view while AVKit itself was not linked.
    @Test func aVideoPlayerLaysOut() {
        let view = NSHostingView(rootView: VideoPlayer(player: nil).frame(width: 320, height: 180))
        view.frame = CGRect(x: 0, y: 0, width: 320, height: 180)
        view.layoutSubtreeIfNeeded()
        #expect(view.fittingSize.width > 0)
    }
}
#endif

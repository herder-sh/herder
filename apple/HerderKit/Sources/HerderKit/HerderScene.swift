import Herder
import SwiftUI

/// The app's scene: shows the app's profile and ties its client to the app's lifecycle.
public struct HerderScene: Scene {
    /// The app's profile, opened once for the process.
    ///
    /// Not with each scene: SwiftUI makes the scene again whenever it evaluates the app's body,
    /// and a profile opened as a scene's initial state stays alive for as long as that scene
    /// value does, though SwiftUI shows the first one's. Each profile opened is a client that
    /// connects to every machine, so the app held two connections to each: the one it showed,
    /// and one nobody saw, suspended or woke, which had every list sent to it too and
    /// reconnected on its own.
    @MainActor static let opened = Profile.openDefault()

    let profile = HerderScene.opened
    /// The demo, while it is open in place of the profile's fleet.
    @State private var demo: Fleet?
    @Environment(\.scenePhase) private var scenePhase

    public init() {
        Hangs.watch()
    }

    public var body: some Scene {
        WindowGroup {
            switch profile {
            case .opened(let fleet):
                Group {
                    if let demo {
                        FleetView(fleet: demo)
                            .safeAreaInset(edge: .top, spacing: 0) {
                                DemoBanner { self.demo = nil }
                            }
                    } else {
                        FleetView(fleet: fleet)
                            .overlay { Splash(fleet: fleet) }
                            .environment(\.exploreDemo) {
                                do {
                                    demo = try Fleet.demo()
                                } catch {
                                    fleet.toast = Toast(text: describe(error))
                                }
                            }
                    }
                }
                // Dark only for now; light mode comes later.
                .preferredColorScheme(.dark)
            case .failed(let message):
                ContentUnavailableView(
                    "Cannot open the profile", systemImage: "exclamationmark.triangle",
                    description: Text(message))
            }
        }
        #if os(macOS)
        // herder draws its own window: the sidebar runs up under the traffic lights.
        .windowStyle(.hiddenTitleBar)
        // One window; ⌘N starts a session (the sidebar's button) instead of opening another.
        .commands { CommandGroup(replacing: .newItem) {} }
        #endif
        .onChange(of: scenePhase) { _, phase in
            guard case .opened(let fleet) = profile else { return }
            switch phase {
            case .active: fleet.wake()
            #if os(iOS)
            // iOS freezes a backgrounded app and may kill its sockets; a Mac app keeps running
            // hidden or minimised, so it keeps reconnecting and keeps every list current.
            case .background: fleet.suspend()
            #endif
            default: break
            }
        }
        #if os(macOS)
        // An artifact opens in a large window of its own, which remembers its frame.
        WindowGroup("Artifact", for: ArtifactViewer.Ref.self) { $ref in
            if case .opened(let fleet) = profile, let ref {
                ArtifactViewer(fleet: fleet, key: ref.key, start: ref.id)
            }
        }
        .defaultWindowPlacement { _, context in
            let screen = context.defaultDisplay.visibleRect
            return WindowPlacement(size: CGSize(width: screen.width * 0.9, height: screen.height * 0.9))
        }
        .windowToolbarStyle(.unified)
        #endif
    }
}

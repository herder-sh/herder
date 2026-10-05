import Herder
import SwiftUI

/// The app's scene: opens the profile once and ties the client to the app's lifecycle.
public struct HerderScene: Scene {
    @State private var profile = Profile.openDefault()
    @Environment(\.scenePhase) private var scenePhase

    public init() {}

    public var body: some Scene {
        WindowGroup {
            switch profile {
            case .opened(let fleet):
                FleetView(fleet: fleet)
                    .overlay { Splash(fleet: fleet) }
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
    }
}

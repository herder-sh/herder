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
                    // Dark only for now; light mode comes later.
                    .preferredColorScheme(.dark)
            case .failed(let message):
                ContentUnavailableView(
                    "Cannot open the profile", systemImage: "exclamationmark.triangle",
                    description: Text(message))
            }
        }
        .onChange(of: scenePhase) { _, phase in
            guard case .opened(let fleet) = profile else { return }
            switch phase {
            case .active: fleet.wake()
            case .background: fleet.suspend()
            default: break
            }
        }
    }
}

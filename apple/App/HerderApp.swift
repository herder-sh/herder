import HerderKit
import SwiftUI

@main
struct HerderApp: App {
    init() {
        // Siri learns the projects its phrases can name (Intents.swift).
        HerderShortcuts.updateAppShortcutParameters()
    }

    var body: some Scene {
        HerderScene()
    }
}

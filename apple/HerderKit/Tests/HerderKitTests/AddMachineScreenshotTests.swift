#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws Add a machine's three steps as the Mac's 560 pt sheet: Install, with
/// the command and the clipboard watched for the link, and a vault your machines use to sign in
/// to; Approve, the machine by name with the code its terminal shows and the passkey that
/// approves; Done, your devices and the backup to your vault. With `HERDER_SCREENSHOT_DIR` set,
/// also writes them there. Drawn in a window, as its text fields are AppKit's, which
/// `ImageRenderer` leaves out.
@MainActor struct AddMachineScreenshotTests {
    static let fingerprint = "3f9a1c0be47d2a865f01c3d9e8b7a6541203fedcba9876543210abcdef012345"
    static let vault = UnpairedVault(address: "127.0.0.1:7448", fingerprint: fingerprint,
                                     hosts: ["draugelis"], machine: "draugelis")
    static let request = ApprovalRequest(kind: .machine, name: "gpu-box", code: "MOS1186", fingerprint: fingerprint,
                                         hosts: ["192.168.1.80:7447"])
    static let devices = [
        Directory.Device(id: "herder-macos", kind: .mac, current: true),
        Directory.Device(id: "herder-ios", kind: .phone),
        Directory.Device(id: "herder-tui", kind: .terminal, host: "trash-can-01"),
    ]

    @Test
    func drawsInstall() throws {
        try shoot(AddMachinePreview(step: .install, vaults: [Self.vault], height: 760), height: 760,
                  named: "mac-add-machine-install.png")
    }

    @Test
    func drawsKubernetes() throws {
        try shoot(AddMachinePreview(source: .kubernetes, step: .install, height: 660), height: 660,
                  named: "mac-add-machine-kubernetes.png")
    }

    @Test
    func drawsApprove() throws {
        try shoot(AddMachinePreview(step: .approve(.machine(Self.request)), height: 560), height: 560,
                  named: "mac-add-machine-approve.png")
    }

    @Test
    func drawsDone() throws {
        try shoot(AddMachinePreview(step: .done(.machine(Self.request), "gpu-box"), devices: Self.devices,
                                    backup: BackupOffer(vault: "herder-vault-0"), height: 620),
                  height: 620, named: "mac-add-machine-done.png")
    }

    @Test
    func drawsAVaultToSignInTo() throws {
        try shoot(AddMachinePreview(step: .approve(.vault(Self.vault)), height: 520), height: 520,
                  named: "mac-add-vault.png")
    }

    private func shoot(_ preview: AddMachinePreview, height: Double, named name: String) throws {
        let width = 560.0
        let view = NSHostingView(rootView: preview.environment(\.colorScheme, .dark).frame(width: width, height: height))
        view.frame = CGRect(x: 0, y: 0, width: width, height: height)
        let window = NSWindow(contentRect: view.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: .darkAqua)
        window.contentView = view
        view.layoutSubtreeIfNeeded()
        let rep = try #require(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: rep)
        let png = try #require(rep.representation(using: .png, properties: [:]))
        Attachment.record(png, named: name)
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent(name))
        }
    }
}

private struct AddMachinePreview: View {
    @State var source = MachineSource.install
    @State var text = ""
    @State var newName: String?
    let step: AddMachineStep
    var vaults: [UnpairedVault] = []
    var devices: [Directory.Device] = []
    var backup: BackupOffer?
    let height: CGFloat

    var body: some View {
        var actions = AddMachineView.Actions()
        actions.addDevice = {}
        actions.startSession = {}
        return AddMachineView(source: $source, text: $text, newName: $newName, step: step,
                              approver: Approver(name: "Tomas", detail: "Your passkey in Apple Passwords"),
                              vaults: vaults, devices: devices, backup: backup, actions: actions, height: height)
    }
}
#endif

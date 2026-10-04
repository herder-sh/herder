import CoreImage.CIFilterBuiltins
import Herder
import SwiftUI

/// Pairs another device with every machine this one has: a one-time link from the client
/// core's `share()`, as a QR code to scan and as text to copy, with the machines it leaves out.
struct ShareSheet: View {
    let fleet: Fleet
    @Environment(\.dismiss) private var dismiss
    @State private var shared: SharedLink?
    @State private var error: String?

    var body: some View {
        SheetScaffold(title: "Pair Another Device",
                      subtitle: "Scan the code with herder on the other device, or paste the link into its Add Machine.",
                      height: 720) {
            if let shared {
                SharedLinkView(fleet: fleet, shared: shared)
            } else if let error {
                Text(error).font(.footnote).foregroundStyle(Theme.failure)
            } else {
                HStack(spacing: 10) {
                    ProgressView().controlSize(.small)
                    Text("Asking each machine for a code…").foregroundStyle(Theme.secondary)
                }
                .font(.subheadline)
                .frame(maxWidth: .infinity, minHeight: 200)
            }
        } footer: {
            Spacer()
            ActionButton(title: "New Link", style: .secondary) { await share() }
                .frame(maxWidth: 160)
            ActionButton(title: "Done", style: .primary) { dismiss() }
                .frame(maxWidth: 160)
                .keyboardShortcut(.defaultAction)
        }
        .task { await share() }
    }

    private func share() async {
        do {
            shared = try await fleet.share()
            error = nil
        } catch HerderError.Pairing(let detail) {
            shared = nil
            error = "Cannot share: \(detail)."
        } catch {
            shared = nil
            self.error = describe(error)
        }
    }
}

private struct SharedLinkView: View {
    let fleet: Fleet
    let shared: SharedLink

    var body: some View {
        let link = pairingLinkToString(link: shared.link)
        let expires = Timestamp.date(shared.expiresAt)
        TimelineView(.periodic(from: .now, by: 1)) { context in
            let expired = expires.map { $0 <= context.date } ?? false
            VStack(spacing: 12) {
                if let code = QRCode.image(link) {
                    Image(decorative: code, scale: 1)
                        .interpolation(.none)
                        .resizable()
                        .frame(width: 220, height: 220)
                        .padding(14)
                        .background(.white, in: .rect(cornerRadius: Theme.corner))
                        .opacity(expired ? 0.15 : 1)
                        .accessibilityLabel("QR code of the pairing link")
                }
                Text(expiry(expires, now: context.date))
                    .font(.subheadline.weight(.medium).monospacedDigit())
                    .foregroundStyle(expired ? Theme.failure : Theme.secondary)
            }
            .frame(maxWidth: .infinity)
        }
        Field(label: "Link", hint: "It works once on each machine. Anyone with it pairs as you, so share it only with your own devices.") {
            HStack(alignment: .top) {
                Text(link)
                    .font(Theme.monoSmall)
                    .foregroundStyle(Theme.text)
                    .lineLimit(3)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                Spacer()
                CopyButton(text: link)
            }
            .padding(12)
            .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
        }
        Field(label: "Pairs with") {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(shared.shared, id: \.self) { host in
                    Label(name(host), systemImage: "checkmark.circle.fill")
                        .foregroundStyle(Theme.text)
                }
            }
            .font(.subheadline)
        }
        if !shared.skipped.isEmpty {
            Field(label: "Left out", hint: "Pair the other device with these later, when they can give a code.") {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(shared.skipped, id: \.hostId) { skipped in
                        HStack(alignment: .firstTextBaseline, spacing: 6) {
                            Image(systemName: "minus.circle.fill").foregroundStyle(Theme.failure)
                            Text(name(skipped.hostId)).foregroundStyle(Theme.text)
                            Text(skipped.error).foregroundStyle(Theme.tertiary)
                        }
                    }
                }
                .font(.subheadline)
            }
        }
    }

    private func name(_ host: HostId) -> String {
        fleet.machines.first { $0.hostId == host }?.name ?? host
    }
}

/// How long a shared link still works: "Expires in 4:59", or that it expired.
func expiry(_ date: Date?, now: Date) -> String {
    guard let date else { return "" }
    let seconds = Int(date.timeIntervalSince(now).rounded(.up))
    guard seconds > 0 else { return "Expired: make a new link" }
    return String(format: "Expires in %d:%02d", seconds / 60, seconds % 60)
}

/// QR codes for pairing links.
enum QRCode {
    /// `text` as a QR code, one pixel per module.
    static func image(_ text: String) -> CGImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(text.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage else { return nil }
        return CIContext().createCGImage(output, from: output.extent)
    }
}

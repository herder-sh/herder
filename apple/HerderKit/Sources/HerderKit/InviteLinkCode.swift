import CoreImage.CIFilterBuiltins
import Herder
import SwiftUI

/// An invite link as a QR code to scan, with how long it still works, and as text to copy.
struct InviteLinkCode: View {
    let link: String
    let expiresAt: Date?
    let hint: String
    /// The clock the expiry counts down on; the live one when absent.
    var now: Date?

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { context in
            let at = now ?? context.date
            let expired = expiresAt.map { $0 <= at } ?? false
            VStack(spacing: 12) {
                if let code = QRCode.image(link) {
                    Image(decorative: code, scale: 1)
                        .interpolation(.none)
                        .resizable()
                        .frame(width: 220, height: 220)
                        .padding(14)
                        .background(.white, in: .rect(cornerRadius: Theme.corner))
                        .opacity(expired ? 0.15 : 1)
                        .accessibilityLabel("QR code of the link")
                }
                Text(expiry(expiresAt, now: at))
                    .font(.subheadline.weight(.medium).monospacedDigit())
                    .foregroundStyle(expired ? Theme.failure : Theme.secondary)
            }
            .frame(maxWidth: .infinity)
        }
        Field(label: "Link", hint: hint) {
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
            .background(.card, in: .rect(cornerRadius: Theme.corner))
        }
    }
}

/// How long a shared link still works: "Expires in 4:59", or that it expired.
func expiry(_ date: Date?, now: Date) -> String {
    guard let date else { return "" }
    let seconds = Int(date.timeIntervalSince(now).rounded(.up))
    guard seconds > 0 else { return "Expired: make a new link" }
    return String(format: "Expires in %d:%02d", seconds / 60, seconds % 60)
}

/// QR codes for invite links.
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

import CoreImage.CIFilterBuiltins
import Herder
import SwiftUI

/// A link as a QR code to scan, with how long it still works where it expires, and as text to
/// copy: an invite's, or Add a Device's.
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
                if expiresAt != nil {
                    Text(expiry(expiresAt, now: at))
                        .font(.subheadline.weight(.medium).monospacedDigit())
                        .foregroundStyle(expired ? Theme.failure : Theme.secondary)
                }
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

/// What a `herder://invite` link names: who invited you, as what, and each machine's own
/// one-time invite with where it is and how to recognise it.
///
/// `herder://invite?by=Tomas&for=Lina&role=member&host=…&fp=…&invite=…&name=devbox&host=…`:
/// each machine is its `host`s, then its `fp`, `invite` and optional `name`. `by`, `for` and
/// `role` only say what the screen shows; each machine decides with its invite.
struct InviteLink: Hashable {
    struct Machine: Hashable, Identifiable {
        /// Where it listens, as `host:port`.
        let hosts: [String]
        /// Its certificate's SHA-256, hex.
        let fingerprint: String
        /// Its one-time invite token.
        let invite: String
        /// What the inviter calls it.
        var name: String?
        var id: String { fingerprint }

        /// "devbox": its name, else its first address.
        var title: String {
            name.flatMap { $0.isEmpty ? nil : $0 } ?? ApprovalRequest.address(hosts.first ?? "")
        }
    }

    var machines: [Machine]
    var role: Role
    /// Who invited you, when the link says.
    var by: String?
    /// Who it is for, when the inviter named them: the name it suggests.
    var label: String?

    /// Nil for a link of another kind, one naming no machine, or a machine missing what it needs.
    init?(url: URL) {
        guard url.scheme?.lowercased() == "herder", url.host()?.lowercased() == "invite",
              let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems
        else { return nil }
        var machines: [Machine] = []
        var hosts: [String] = []
        var fingerprint: String?
        var invite: String?
        var name: String?
        var role = Role.member
        var by: String?
        var label: String?
        func close() -> Bool {
            guard let fingerprint else { return hosts.isEmpty }
            guard !hosts.isEmpty, let invite else { return false }
            machines.append(Machine(hosts: hosts, fingerprint: fingerprint, invite: invite, name: name))
            return true
        }
        for item in items {
            let value = item.value ?? ""
            switch item.name {
            case "host":
                if fingerprint != nil {
                    guard close() else { return nil }
                    (hosts, fingerprint, invite, name) = ([], nil, nil, nil)
                }
                if !value.isEmpty { hosts.append(value) }
            case "fp" where fingerprint == nil: fingerprint = value.isEmpty ? nil : value
            case "invite": invite = value.isEmpty ? nil : value
            case "name": name = value
            case "role": role = value == "owner" ? .owner : .member
            case "by": by = value.isEmpty ? nil : value
            case "for": label = value.isEmpty ? nil : value
            default: break
            }
        }
        guard close(), !machines.isEmpty else { return nil }
        self.init(machines: machines, role: role, by: by, label: label)
    }

    init(machines: [Machine], role: Role, by: String? = nil, label: String? = nil) {
        self.machines = machines
        self.role = role
        self.by = by
        self.label = label
    }

    /// The link, to share.
    var string: String {
        var link = URLComponents()
        link.scheme = "herder"
        link.host = "invite"
        var items: [URLQueryItem] = []
        if let by { items.append(URLQueryItem(name: "by", value: by)) }
        if let label { items.append(URLQueryItem(name: "for", value: label)) }
        items.append(URLQueryItem(name: "role", value: role == .owner ? "owner" : "member"))
        for machine in machines {
            items += machine.hosts.map { URLQueryItem(name: "host", value: $0) }
            items.append(URLQueryItem(name: "fp", value: machine.fingerprint))
            items.append(URLQueryItem(name: "invite", value: machine.invite))
            if let name = machine.name, !name.isEmpty { items.append(URLQueryItem(name: "name", value: name)) }
        }
        link.queryItems = items
        return link.string ?? ""
    }

    /// "a member on 5 machines".
    var grant: String { "\(role == .owner ? "an owner" : "a member") on \(scope)" }

    /// "Member on 5 machines".
    var summary: String { "\(role == .owner ? "Owner" : "Member") on \(scope)" }

    /// "5 machines", or the one machine's name.
    private var scope: String { machines.count == 1 ? machines[0].title : "\(machines.count) machines" }
}

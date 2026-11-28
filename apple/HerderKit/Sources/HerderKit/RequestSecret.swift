import Herder
import SwiftUI

/// An agent's `request_secret`, as its card shows it: the secret's name with Decline, where it
/// goes as a chip that changes the set, the agent's hint as a link and steps, and, fixed at the
/// bottom, the value field with Paste or Store. Only the hint scrolls, so short of room (the
/// keyboard up under a pinned card, a long hint) the field and Store stay in view.
struct SecretRequestAsk: View {
    let request: PendingRequest
    let fleet: Fleet
    let secret: SecretRequest
    @Binding var picked: String?
    @Binding var value: SecretInput
    /// What the card shows of the session and why it routed here, under the destination.
    let context: AnyView
    @State private var hub: Machine?
    @State private var sets: [String] = []
    /// The hub's secrets, to tell whether the picked set has the name already.
    @State private var held: [SecretInfo]?
    @State private var problem: String?
    @State private var explaining = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            destination
            context
            let hint = SecretHint(secret.description)
            if !hint.isEmpty {
                // A long hint scrolls under the header rather than pushing the field out of view.
                ViewThatFits(in: .vertical) {
                    hintView(hint)
                    ScrollView { hintView(hint) }.frame(maxHeight: 240)
                }
            }
            entry
            if let problem {
                Text(problem).font(.footnote).foregroundStyle(Theme.failure)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .task(id: request.session.key.hostId) { await load() }
    }

    /// The key and the secret's name, which is the question; Decline answers no.
    private var header: some View {
        HStack(spacing: 8) {
            Image(systemName: "key.fill")
                .font(.subheadline.weight(.bold))
                .foregroundStyle(Theme.accent)
            Text(secret.name)
                .font(.system(.subheadline, design: .monospaced).weight(.semibold))
                .foregroundStyle(Theme.text)
                .lineLimit(1)
                .truncationMode(.middle)
                .textSelection(.enabled)
                .accessibilityIdentifier("request-secret-name")
            Spacer(minLength: 4)
            Text(request.age).font(.caption).foregroundStyle(Theme.tertiary)
            Button { Task { await fleet.answer(request, allow: false) } } label: {
                Text("Decline")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Theme.secondary)
                    .padding(.horizontal, 10)
                    .frame(minHeight: Self.small)
                    .background(Theme.raised, in: .capsule)
                    .hitTarget()
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("request-secret-decline")
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Secret requested: \(secret.name)")
    }

    /// "→ coingate · hub-0", a menu of the hub's sets, and whether storing replaces a value.
    private var destination: some View {
        HStack(spacing: 8) {
            Menu {
                Picker("Set", selection: Binding(get: { chosen }, set: { picked = $0 })) {
                    ForEach(choices, id: \.self) { Text($0).tag($0) }
                }
                .pickerStyle(.inline)
            } label: {
                HStack(spacing: 5) {
                    Image(systemName: "arrow.right").font(.caption2.weight(.bold))
                    Text(Self.destination(set: chosen, hub: hub?.name)).lineLimit(1)
                    Image(systemName: "chevron.up.chevron.down").font(.caption2.weight(.semibold))
                        .foregroundStyle(Theme.tertiary)
                }
                .font(.caption.weight(.medium))
                .foregroundStyle(Theme.secondary)
                .padding(.horizontal, 9)
                .frame(minHeight: Self.small)
                .background(Theme.raised, in: .capsule)
                .hitTarget()
            }
            .menuStyle(.button)
            .menuIndicator(.hidden)
            .buttonStyle(.plain)
            .fixedSize()
            .accessibilityLabel("Stores in \(Self.destination(set: chosen, hub: hub?.name))")
            .accessibilityHint("Picks another set")
            .accessibilityIdentifier("request-secret-set")
            if Self.replaces(secret, set: chosen, held: held) {
                Label("Replaces its value", systemImage: "exclamationmark.triangle.fill")
                    .font(.caption.weight(.medium))
                    .foregroundStyle(Theme.accent)
                    .lineLimit(1)
                    .help("\(chosen) already holds \(secret.name): storing replaces its value.")
                    .accessibilityLabel("\(chosen) already holds \(secret.name): storing replaces its value.")
                    .accessibilityIdentifier("request-secret-replaces")
            }
            Spacer(minLength: 0)
        }
    }

    /// What the value is, then where to get it, a numbered step a line, links tappable.
    private func hintView(_ hint: SecretHint) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            if !hint.lead.isEmpty {
                Text(Self.linked(hint.lead))
                    .font(.footnote)
                    .foregroundStyle(Theme.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            ForEach(Array(hint.steps.enumerated()), id: \.offset) { index, step in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text("\(index + 1)")
                        .font(.caption2.weight(.bold).monospacedDigit())
                        .foregroundStyle(Theme.secondary)
                        .frame(width: 18, height: 18)
                        .background(Theme.raised, in: .circle)
                        .alignmentGuide(.firstTextBaseline) { $0[VerticalAlignment.center] + 4 }
                    Text(Self.linked(step))
                        .font(.footnote)
                        .foregroundStyle(Theme.text)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .tint(Theme.link)
        .textSelection(.enabled)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("request-secret-hint")
    }

    /// The value, and beside it Paste while it is empty, Store once it is not: never a grey
    /// button with nothing to say. Return in the field stores too.
    private var entry: some View {
        HStack(spacing: 8) {
            valueField
            if value.isEmpty {
                PasteButton(payloadType: String.self) { strings in
                    guard let text = strings.first else { return }
                    Task { @MainActor in value = SecretInput(text: text) }
                }
                .labelStyle(.titleAndIcon)
                .buttonBorderShape(.roundedRectangle(radius: Theme.corner))
                .controlSize(.large)
                .tint(Theme.accent)
                .accessibilityIdentifier("request-secret-paste")
            } else {
                ActionButton(title: "Store", style: .primary) { await store() }
                    .frame(width: 92)
                    .accessibilityIdentifier("request-secret-store")
            }
        }
    }

    /// A secure field for one line, or what was pasted for a value of several, such as a PEM
    /// key, whose newlines a secure field drops: its size, and a button that clears it.
    private var valueField: some View {
        HStack(spacing: 6) {
            if Self.isMultiline(value) {
                Image(systemName: "doc.on.clipboard").foregroundStyle(Theme.secondary)
                Text(Self.pasted(value)).font(.subheadline).foregroundStyle(Theme.text).lineLimit(1)
                    .accessibilityIdentifier("request-secret-pasted")
                Spacer(minLength: 0)
                Button { value = SecretInput() } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(Theme.tertiary).hitTarget()
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Clear the value")
            } else {
                SecureField("Value", text: $value.text)
                    .textFieldStyle(.plain)
                    .font(Theme.mono)
                    .foregroundStyle(Theme.text)
                    .autocorrectionDisabled()
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    #endif
                    .onSubmit { Task { await store() } }
                    .accessibilityIdentifier("request-secret-value")
            }
            Button { explaining = true } label: {
                Image(systemName: "info.circle").foregroundStyle(Theme.tertiary).hitTarget()
            }
            .buttonStyle(.plain)
            .help(Self.reassurance)
            .accessibilityLabel("Where the value goes")
            .accessibilityIdentifier("request-secret-info")
            .popover(isPresented: $explaining) {
                Text(Self.reassurance)
                    .font(.footnote)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(width: 240)
                    .padding(14)
                    .presentationCompactAdaptation(.popover)
            }
        }
        .padding(.horizontal, 12)
        .frame(maxWidth: .infinity, minHeight: 48)
        .background(Theme.canvas, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(value.isEmpty ? Theme.accent : Theme.stroke))
    }

    private func store() async {
        guard SecretRequestAsk.problem(value) == nil else { return }
        if await fleet.supplySecret(request, secret, value: value, in: chosen) {
            value = SecretInput()
        }
    }

    /// The height of the header's and the destination's small controls.
    #if os(iOS)
    private static var small: CGFloat { 32 }
    #else
    private static var small: CGFloat { 24 }
    #endif

    private var chosen: String { picked ?? secret.set }

    /// The hub's sets, with the agent's first when the hub lacks it: Store creates it.
    private var choices: [String] {
        sets.contains(secret.set) ? sets : [secret.set] + sets
    }

    private func load() async {
        do {
            let hub = try await fleet.ownedHub(of: request.session.key.hostId)
            self.hub = hub
            guard case .secrets(let sets, let secrets) = try await fleet.client.send(hostId: hub.hostId, command: .listSecrets)
            else { return }
            self.sets = sets.map(\.name)
            held = secrets
            problem = nil
        } catch {
            problem = describe(error)
        }
    }

    /// "coingate · hub-0": where Store puts the value; the set alone until the hub is known.
    nonisolated static func destination(set: String, hub: String?) -> String {
        hub.map { "\(set) · \($0)" } ?? set
    }

    nonisolated static let reassurance = "Goes straight to the hub. The agent only learns that it was stored."

    /// `text`'s inline Markdown with its bare URLs linked, as the transcript shows them.
    static func linked(_ text: String) -> AttributedString {
        MarkdownText.inline(text)
    }

    /// Whether storing in `set` replaces a value: as the hub lists its secrets once loaded,
    /// else as the host saw its granted sets when the agent asked.
    nonisolated static func replaces(_ secret: SecretRequest, set: String, held: [SecretInfo]?) -> Bool {
        guard let held else { return set == secret.set && secret.replaces }
        return held.contains { $0.set == set && $0.name == secret.name }
    }

    /// The description to store: the agent's, else that of the secret it replaces in `held`.
    nonisolated static func description(_ secret: SecretRequest, set: String, held: [SecretInfo]) -> String {
        guard secret.description.isEmpty else { return secret.description }
        return held.first { $0.set == set && $0.name == secret.name }?.description ?? ""
    }

    /// Why `value` cannot be stored, as the hub checks it; `nil` when it can.
    nonisolated static func problem(_ value: SecretInput) -> String? {
        if value.isEmpty { return "Enter or paste the value." }
        if value.text.utf8.count > 64 * 1024 || value.text.contains("\0") {
            return "The value is longer than 64 KiB, or holds a NUL."
        }
        return nil
    }

    /// Whether `value` has lines a secure field would drop.
    nonisolated static func isMultiline(_ value: SecretInput) -> Bool {
        value.text.contains(where: \.isNewline)
    }

    /// "Pasted 1.7 KB, 28 lines": what a pasted value shows of itself.
    nonisolated static func pasted(_ value: SecretInput) -> String {
        let lines = value.text.split(whereSeparator: \.isNewline).count
        return "Pasted \(SecretStoreAsk.size(UInt64(value.text.utf8.count))), \(lines) line\(lines == 1 ? "" : "s")"
    }
}

/// An agent's one-line hint, as the card shows it: what the value is, then where to get it as
/// numbered steps. An arrow-chained path, `https://dash.cloudflare.com/profile → API Tokens →
/// Create Token`, or numbered steps, `1. Open … 2. Click …`, become steps; the sentence before
/// them is the lead. A bare domain step, `dash.cloudflare.com`, links to it.
struct SecretHint: Equatable {
    var lead: String
    var steps: [String]

    var isEmpty: Bool { lead.isEmpty && steps.isEmpty }

    init(lead: String, steps: [String]) {
        self.lead = lead
        self.steps = steps
    }

    init(_ text: String) {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let numbered = text.split(separator: /(?:^|\s)\d{1,2}[.)]\s+/, omittingEmptySubsequences: false)
        let arrowed = text.split(separator: /\s+(?:→|->|›|»)\s+/)
        var lead: Substring
        var steps: [Substring]
        if numbered.count >= 3 {
            lead = numbered[0]
            steps = Array(numbered.dropFirst())
        } else if arrowed.count >= 2 {
            // The path starts after the first part's last sentence or colon.
            let first = arrowed[0]
            if let cut = first.ranges(of: /[.:]\s+/).last {
                lead = first[..<cut.lowerBound] + (first[cut].hasPrefix(".") ? "." : "")
                steps = [first[cut.upperBound...]] + arrowed.dropFirst()
            } else {
                lead = ""
                steps = Array(arrowed)
            }
        } else {
            lead = Substring(text)
            steps = []
        }
        self.lead = lead.trimmingCharacters(in: .whitespaces)
        self.steps = steps.map { Self.linkingDomain($0.trimmingCharacters(in: .whitespaces)) }.filter { !$0.isEmpty }
    }

    /// `dash.cloudflare.com/profile` as a link to it over https; any other step as it is.
    private static func linkingDomain(_ step: String) -> String {
        guard step.wholeMatch(of: /[a-z0-9-]+(\.[a-z0-9-]+)*\.[a-z]{2,}(\/\S*)?/.ignoresCase()) != nil else { return step }
        return "[\(step)](https://\(step))"
    }
}

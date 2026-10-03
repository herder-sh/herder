import Herder
import SwiftUI

/// Pairs with a machine from the `herder://pair` link `herder pair` prints.
struct PairSheet: View {
    let fleet: Fleet
    @Environment(\.dismiss) private var dismiss
    @State private var link = ""
    @State private var pairing = false
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Link", text: $link, prompt: Text("herder://pair?…"), axis: .vertical)
                        .labelsHidden()
                        .accessibilityIdentifier("pairing-link")
                        .lineLimit(3...6)
                        .autocorrectionDisabled()
                        #if os(iOS)
                        .textInputAutocapitalization(.never)
                        .keyboardType(.URL)
                        #endif
                } footer: {
                    Text("Run `herder pair` on the machine and paste the link it prints.")
                }
                if let uri {
                    Section {
                        LabeledContent("Address", value: uri.hosts.joined(separator: ", "))
                        LabeledContent("Fingerprint") {
                            Text(uri.fingerprint)
                                .font(.caption.monospaced())
                                .textSelection(.enabled)
                        }
                    } header: {
                        Text("Machine")
                    } footer: {
                        Text("Check that the fingerprint is the one `herder pair` printed.")
                    }
                }
                if let error {
                    Section {
                        Text(error).foregroundStyle(Theme.failure)
                    }
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
            .background(Theme.background)
            .navigationTitle("Add Machine")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    if pairing {
                        ProgressView()
                    } else {
                        Button("Pair") { Task { await pair() } }
                            .disabled(uri == nil)
                    }
                }
            }
        }
        #if os(macOS)
        .frame(minWidth: 440, minHeight: 320)
        #endif
    }

    private var trimmed: String {
        link.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var uri: PairingUri? {
        try? parsePairingUri(link: trimmed)
    }

    private func pair() async {
        pairing = true
        defer { pairing = false }
        do {
            try await fleet.pair(link: trimmed)
            dismiss()
        } catch {
            self.error = describe(error)
        }
    }
}

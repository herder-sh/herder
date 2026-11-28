import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct SecretsTests {
    /// A hub that records what it is sent, lists `sets` and `secrets`, and refuses changes
    /// with `refusal` when set.
    @MainActor final class Hub {
        var sent: [CommandBody] = []
        var sets: [SecretSet] = [SecretSet(name: "prod", hosts: ["devbox"], hostUse: [])]
        var secrets: [SecretInfo] = [
            SecretInfo(set: "prod", name: "API_KEY", description: "merchant api", updatedAt: "2026-10-13T08:00:00Z"),
        ]
        var refusal: String?
        /// Runs as each change arrives, before it is answered.
        var onChange: (CommandBody) -> Void = { _ in }

        func model() -> HubSecrets {
            HubSecrets(hub: "hub") { [self] command in
                sent.append(command)
                switch command {
                case .listSecrets: return .secrets(sets: sets, secrets: secrets)
                case .listSecretAudit: return .secretAudit(entries: [])
                default:
                    onChange(command)
                    if let refusal {
                        throw HerderError.Rejected(info: ErrorInfo(code: .badRequest, message: refusal))
                    }
                    return .applied
                }
            }
        }

        var changes: [CommandBody] {
            sent.filter {
                if case .listSecrets = $0 { return false }
                if case .listSecretAudit = $0 { return false }
                return true
            }
        }
    }

    @Test func eachActionSendsItsCommandToTheHubAndListsAgain() async throws {
        let hub = Hub()
        let secrets = hub.model()
        await secrets.reload()
        #expect(secrets.sets.map(\.name) == ["prod"])
        #expect(secrets.secrets(in: "prod").map(\.name) == ["API_KEY"])

        try await secrets.createSet(" ci ")
        try await secrets.setGranted(true, set: "prod", host: "laptop")
        try await secrets.setGranted(false, set: "prod", host: "devbox")
        try await secrets.deleteSecret(secrets.secrets(in: "prod")[0])
        try await secrets.deleteSet("ci")
        #expect(hub.changes == [
            .createSecretSet(set: "ci", assertion: nil),
            .grantSecretSet(set: "prod", hostId: "laptop", hostUse: false, assertion: nil),
            .revokeSecretSet(set: "prod", hostId: "devbox", hostUse: false, assertion: nil),
            .deleteSecret(set: "prod", name: "API_KEY", assertion: nil),
            .deleteSecretSet(set: "ci", assertion: nil),
        ])
        // Each change is followed by both lists.
        #expect(hub.sent.count == 2 + 5 * 3)
    }

    @Test func aBadSetNameIsRefusedBeforeAnythingIsSent() async {
        let hub = Hub()
        let secrets = hub.model()
        for bad in ["Prod", "-prod", "prod-", "a--b", "", "has space"] {
            await #expect(throws: HerderError.self) { try await secrets.createSet(bad) }
        }
        #expect(hub.changes.isEmpty)
    }

    @Test func savingSendsTheSecretAndClearsTheValueBeforeItGoes() async {
        let hub = Hub()
        let secrets = hub.model()
        let editor = SecretEditor(secrets: secrets, set: "prod")
        editor.name = "NPM_TOKEN"
        editor.description = " publishes packages to npm "
        editor.value.text = "npm_s3cr3t"
        var heldWhileSending: String?
        hub.onChange = { _ in heldWhileSending = editor.value.text }

        #expect(await editor.save())
        #expect(hub.changes == [
            .putSecret(set: "prod", name: "NPM_TOKEN", description: "publishes packages to npm", value: "npm_s3cr3t", assertion: nil),
        ])
        #expect(heldWhileSending == "")
        #expect(editor.value.isEmpty)
        #expect(editor.error == nil)
    }

    @Test func aRefusalShowsTheHubsMessageAndTheValueIsTypedAgain() async {
        let hub = Hub()
        let refusal = "this hub keeps no secrets: set HERDER_HUB_SECRETS_KEY"
        hub.refusal = refusal
        let editor = SecretEditor(secrets: hub.model(), replacing: hub.secrets[0])
        #expect(editor.name == "API_KEY")
        #expect(editor.description == "merchant api")
        editor.value.text = "new-value"

        #expect(await editor.save() == false)
        #expect(editor.error == refusal)
        #expect(editor.value.isEmpty)
        #expect(editor.name == "API_KEY")
    }

    @Test func aFormWithAProblemSendsNothing() async {
        let hub = Hub()
        let editor = SecretEditor(secrets: hub.model(), set: "prod")
        editor.name = "lower"
        editor.description = "x"
        editor.value.text = "v"
        #expect(await editor.save() == false)
        #expect(editor.error?.hasPrefix("A secret's name") == true)
        editor.name = "OK"
        editor.description = ""
        #expect(await editor.save() == false)
        #expect(editor.error?.hasPrefix("Say what the secret is for") == true)
        editor.description = "something"
        editor.value = SecretInput()
        #expect(await editor.save() == false)
        #expect(editor.error == "Enter the value.")
        #expect(hub.changes.isEmpty)
    }

    @Test func theValueNeverShowsInADescriptionOrADump() {
        let hub = Hub()
        let editor = SecretEditor(secrets: hub.model(), set: "prod")
        editor.value.text = "npm_s3cr3t"
        var dumped = ""
        dump(editor.value, to: &dumped)
        dump(editor, to: &dumped)
        for text in [String(describing: editor.value), String(reflecting: editor.value), dumped,
                     String(describing: editor), "\(editor.value)"] {
            #expect(!text.contains("npm_s3cr3t"))
        }
    }

    @Test func aSetCanBeGrantedToTheHubsHostsAndRevokedFromOnesItNoLongerLists() {
        let hosts = [
            FleetHost(hostId: "devbox", hostName: "devbox", online: true, lastSeen: "", usage: nil),
            FleetHost(hostId: "laptop", hostName: "Laptop", online: false, lastSeen: "", usage: nil),
        ]
        let listed = HubSecrets.hosts(of: SecretSet(name: "prod", hosts: ["laptop", "gone"], hostUse: []), hubHosts: hosts)
        #expect(listed.map(\.id) == ["devbox", "laptop", "gone"])
        #expect(listed.map(\.name) == ["devbox", "Laptop", nil])
    }

    @Test func addFromFileKeepsAMultiLineValueWhole() async throws {
        let pem = "-----BEGIN PRIVATE KEY-----\nMIGTAgEAMBMG\r\nByqGSM49AgEG\n-----END PRIVATE KEY-----\n"
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("AuthKey_\(UUID()).p8")
        try Data(pem.utf8).write(to: url)
        defer { try? FileManager.default.removeItem(at: url) }
        let hub = Hub()
        let editor = SecretEditor(secrets: hub.model(), set: "prod")
        editor.name = "ASC_PRIVATE_KEY"
        editor.description = "App Store Connect API key"

        editor.read(url)
        #expect(editor.file == url.lastPathComponent)
        #expect(editor.value.text == pem)
        #expect(await editor.save())
        #expect(hub.changes == [
            .putSecret(set: "prod", name: "ASC_PRIVATE_KEY", description: "App Store Connect API key", value: pem, assertion: nil),
        ])
        #expect(editor.file == nil)
        #expect(editor.value.isEmpty)
    }

    @Test func addFromFileRefusesWhatCannotBeASecret() throws {
        let dir = FileManager.default.temporaryDirectory
        let binary = dir.appendingPathComponent("binary-\(UUID())")
        try Data([0xff, 0xfe, 0xfd]).write(to: binary)
        let large = dir.appendingPathComponent("large-\(UUID())")
        try Data(repeating: 0x6b, count: 64 * 1024 + 1).write(to: large)
        defer {
            try? FileManager.default.removeItem(at: binary)
            try? FileManager.default.removeItem(at: large)
        }
        let editor = SecretEditor(secrets: Hub().model(), set: "prod")
        editor.read(binary)
        #expect(editor.file == nil)
        #expect(editor.error?.contains("not text") == true)
        editor.read(large)
        #expect(editor.file == nil)
        #expect(editor.error?.contains("64 KiB") == true)
    }

    @Test func aStoreSecretCardAsksWhatGoesWhere() {
        let secret = SecretStoreRequest(
            path: "~/Downloads/AuthKey_9WJ.p8", bytes: 257, fingerprint: "2bb80d53", set: "personal",
            name: "ASC_PRIVATE_KEY", description: "App Store Connect API key", deleteFileAfter: true)
        #expect(SecretStoreAsk.question(secret, set: "personal", hub: "herder-hub-0")
            == "Store ~/Downloads/AuthKey_9WJ.p8 (257 B) as ASC_PRIVATE_KEY in personal on herder-hub-0?")
        #expect(SecretStoreAsk.detail(secret) == "Fingerprint 2bb80d53 · the file is deleted once the hub stored it")
    }

    @Test func aRequestSecretCardAsksForTheValueAndWarnsOfAReplace() {
        let secret = SecretRequest(
            set: "coingate", name: "GITHUB_SIEM_PAT",
            description: "GitHub fine-grained PAT, read-only on the org audit log", replaces: true)
        #expect(SecretRequestAsk.destination(set: "coingate", hub: "herder-hub-0") == "coingate · herder-hub-0")
        #expect(SecretRequestAsk.destination(set: "coingate", hub: nil) == "coingate")
        // Until the hub lists its secrets, as the host saw them; then as the hub has them.
        #expect(SecretRequestAsk.replaces(secret, set: "coingate", held: nil))
        #expect(!SecretRequestAsk.replaces(secret, set: "personal", held: nil))
        let held = [SecretInfo(set: "personal", name: "GITHUB_SIEM_PAT", description: "", updatedAt: "2026-11-09T12:00:00Z")]
        #expect(!SecretRequestAsk.replaces(secret, set: "coingate", held: held))
        #expect(SecretRequestAsk.replaces(secret, set: "personal", held: held))
        // A replaced secret keeps its description when the agent gave none.
        let bare = SecretRequest(set: "personal", name: "GITHUB_SIEM_PAT", description: "", replaces: true)
        let described = [SecretInfo(set: "personal", name: "GITHUB_SIEM_PAT", description: "SIEM audit log", updatedAt: "2026-11-09T12:00:00Z")]
        #expect(SecretRequestAsk.description(bare, set: "personal", held: described) == "SIEM audit log")
        #expect(SecretRequestAsk.description(bare, set: "coingate", held: described) == "")
        #expect(SecretRequestAsk.description(secret, set: "personal", held: described) == secret.description)
    }

    /// An agent's one-line hint shows as what the value is, then where to get it, a step a line.
    @Test func aHintsPathBecomesNumberedSteps() {
        #expect(SecretHint("Cloudflare API token, Zone DNS edit. https://dash.cloudflare.com/profile/api-tokens → Create Token → Edit zone DNS")
            == SecretHint(lead: "Cloudflare API token, Zone DNS edit.",
                          steps: ["https://dash.cloudflare.com/profile/api-tokens", "Create Token", "Edit zone DNS"]))
        // A first step that is a bare domain links to it, a later one does not; a path without a
        // sentence before it has no lead.
        #expect(SecretHint("dash.cloudflare.com → My Profile → API Tokens → homelab.dev")
            == SecretHint(lead: "", steps: ["[dash.cloudflare.com](https://dash.cloudflare.com)", "My Profile", "API Tokens", "homelab.dev"]))
        #expect(SecretHint("Grafana token: grafana.example.com/org/apikeys -> Add API key")
            == SecretHint(lead: "Grafana token", steps: ["[grafana.example.com/org/apikeys](https://grafana.example.com/org/apikeys)", "Add API key"]))
        #expect(SecretHint("PagerDuty REST key. 1. Open Integrations 2. API Access Keys 3. Create New API Key")
            == SecretHint(lead: "PagerDuty REST key.", steps: ["Open Integrations", "API Access Keys", "Create New API Key"]))
        // A hint without a path is its lead alone.
        #expect(SecretHint("GitHub fine-grained PAT, read-only on the org audit log")
            == SecretHint(lead: "GitHub fine-grained PAT, read-only on the org audit log", steps: []))
        #expect(SecretHint("").isEmpty)
    }

    @Test func aRequestedValueIsCheckedAsTheHubChecksIt() {
        #expect(SecretRequestAsk.problem(SecretInput()) == "Enter or paste the value.")
        #expect(SecretRequestAsk.problem(SecretInput(text: "a\0b")) != nil)
        #expect(SecretRequestAsk.problem(SecretInput(text: String(repeating: "k", count: 64 * 1024 + 1))) != nil)
        let pem = "-----BEGIN PRIVATE KEY-----\nMIGT\n-----END PRIVATE KEY-----\n"
        let pasted = SecretInput(text: pem)
        #expect(SecretRequestAsk.problem(pasted) == nil)
        #expect(SecretRequestAsk.isMultiline(pasted))
        #expect(!SecretRequestAsk.isMultiline(SecretInput(text: "ghp_x")))
        #expect(SecretRequestAsk.pasted(pasted) == "Pasted 59 B, 3 lines")
        // Nothing that prints the value shows it.
        #expect(!"\(pasted)".contains("MIGT"))
        #expect(!String(reflecting: pasted).contains("MIGT"))
    }

    @Test func secretNamesAreEnvironmentVariables() {
        #expect(HubSecrets.isValidSecretName("API_KEY"))
        #expect(HubSecrets.isValidSecretName("_X1"))
        #expect(!HubSecrets.isValidSecretName("1X"))
        #expect(!HubSecrets.isValidSecretName("api_key"))
        #expect(!HubSecrets.isValidSecretName(""))
        #expect(!HubSecrets.isValidSecretName(String(repeating: "A", count: 65)))
    }
}

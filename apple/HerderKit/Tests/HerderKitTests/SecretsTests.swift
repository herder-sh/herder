import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct SecretsTests {
    /// A vault that records what it is sent, lists `sets` and `secrets`, and refuses changes
    /// with `refusal` when set.
    @MainActor final class Vault {
        var sent: [CommandBody] = []
        var sets: [SecretSet] = [SecretSet(name: "prod", hosts: ["devbox"], hostUse: [])]
        var secrets: [SecretInfo] = [
            SecretInfo(set: "prod", name: "API_KEY", description: "merchant api", updatedAt: "2026-10-13T08:00:00Z"),
        ]
        var refusal: String?
        /// Runs as each change arrives, before it is answered.
        var onChange: (CommandBody) -> Void = { _ in }

        func model() -> VaultSecrets {
            VaultSecrets(vault: "vault") { [self] command in
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

    @Test func eachActionSendsItsCommandToTheVaultAndListsAgain() async throws {
        let vault = Vault()
        let secrets = vault.model()
        await secrets.reload()
        #expect(secrets.sets.map(\.name) == ["prod"])
        #expect(secrets.secrets(in: "prod").map(\.name) == ["API_KEY"])

        try await secrets.createSet(" ci ")
        try await secrets.setGranted(true, set: "prod", host: "laptop")
        try await secrets.setGranted(false, set: "prod", host: "devbox")
        try await secrets.deleteSecret(secrets.secrets(in: "prod")[0])
        try await secrets.deleteSet("ci")
        #expect(vault.changes == [
            .createSecretSet(set: "ci"),
            .grantSecretSet(set: "prod", hostId: "laptop", hostUse: false),
            .revokeSecretSet(set: "prod", hostId: "devbox", hostUse: false),
            .deleteSecret(set: "prod", name: "API_KEY"),
            .deleteSecretSet(set: "ci"),
        ])
        // Each change is followed by both lists.
        #expect(vault.sent.count == 2 + 5 * 3)
    }

    @Test func aBadSetNameIsRefusedBeforeAnythingIsSent() async {
        let vault = Vault()
        let secrets = vault.model()
        for bad in ["Prod", "-prod", "prod-", "a--b", "", "has space"] {
            await #expect(throws: HerderError.self) { try await secrets.createSet(bad) }
        }
        #expect(vault.changes.isEmpty)
    }

    @Test func savingSendsTheSecretAndClearsTheValueBeforeItGoes() async {
        let vault = Vault()
        let secrets = vault.model()
        let editor = SecretEditor(secrets: secrets, set: "prod")
        editor.name = "NPM_TOKEN"
        editor.description = " publishes packages to npm "
        editor.value.text = "npm_s3cr3t"
        var heldWhileSending: String?
        vault.onChange = { _ in heldWhileSending = editor.value.text }

        #expect(await editor.save())
        #expect(vault.changes == [
            .putSecret(set: "prod", name: "NPM_TOKEN", description: "publishes packages to npm", value: "npm_s3cr3t"),
        ])
        #expect(heldWhileSending == "")
        #expect(editor.value.isEmpty)
        #expect(editor.error == nil)
    }

    @Test func aRefusalShowsTheVaultsMessageAndTheValueIsTypedAgain() async {
        let vault = Vault()
        let refusal = "this vault keeps no secrets: set HERDER_VAULT_SECRETS_KEY"
        vault.refusal = refusal
        let editor = SecretEditor(secrets: vault.model(), replacing: vault.secrets[0])
        #expect(editor.name == "API_KEY")
        #expect(editor.description == "merchant api")
        editor.value.text = "new-value"

        #expect(await editor.save() == false)
        #expect(editor.error == refusal)
        #expect(editor.value.isEmpty)
        #expect(editor.name == "API_KEY")
    }

    @Test func aFormWithAProblemSendsNothing() async {
        let vault = Vault()
        let editor = SecretEditor(secrets: vault.model(), set: "prod")
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
        #expect(vault.changes.isEmpty)
    }

    @Test func theValueNeverShowsInADescriptionOrADump() {
        let vault = Vault()
        let editor = SecretEditor(secrets: vault.model(), set: "prod")
        editor.value.text = "npm_s3cr3t"
        var dumped = ""
        dump(editor.value, to: &dumped)
        dump(editor, to: &dumped)
        for text in [String(describing: editor.value), String(reflecting: editor.value), dumped,
                     String(describing: editor), "\(editor.value)"] {
            #expect(!text.contains("npm_s3cr3t"))
        }
    }

    @Test func aSetCanBeGrantedToTheVaultsHostsAndRevokedFromOnesItNoLongerLists() {
        let hosts = [
            FleetHost(hostId: "devbox", hostName: "devbox", online: true, lastSeen: "", usage: nil),
            FleetHost(hostId: "laptop", hostName: "Laptop", online: false, lastSeen: "", usage: nil),
        ]
        let listed = VaultSecrets.hosts(of: SecretSet(name: "prod", hosts: ["laptop", "gone"], hostUse: []), vaultHosts: hosts)
        #expect(listed.map(\.id) == ["devbox", "laptop", "gone"])
        #expect(listed.map(\.name) == ["devbox", "Laptop", nil])
    }

    @Test func addFromFileKeepsAMultiLineValueWhole() async throws {
        let pem = "-----BEGIN PRIVATE KEY-----\nMIGTAgEAMBMG\r\nByqGSM49AgEG\n-----END PRIVATE KEY-----\n"
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("AuthKey_\(UUID()).p8")
        try Data(pem.utf8).write(to: url)
        defer { try? FileManager.default.removeItem(at: url) }
        let vault = Vault()
        let editor = SecretEditor(secrets: vault.model(), set: "prod")
        editor.name = "ASC_PRIVATE_KEY"
        editor.description = "App Store Connect API key"

        editor.read(url)
        #expect(editor.file == url.lastPathComponent)
        #expect(editor.value.text == pem)
        #expect(await editor.save())
        #expect(vault.changes == [
            .putSecret(set: "prod", name: "ASC_PRIVATE_KEY", description: "App Store Connect API key", value: pem),
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
        let editor = SecretEditor(secrets: Vault().model(), set: "prod")
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
        #expect(SecretStoreAsk.question(secret, set: "personal", vault: "herder-vault-0")
            == "Store ~/Downloads/AuthKey_9WJ.p8 (257 B) as ASC_PRIVATE_KEY in personal on herder-vault-0?")
        #expect(SecretStoreAsk.detail(secret) == "Fingerprint 2bb80d53 · the file is deleted once the vault stored it")
    }

    @Test func secretNamesAreEnvironmentVariables() {
        #expect(VaultSecrets.isValidSecretName("API_KEY"))
        #expect(VaultSecrets.isValidSecretName("_X1"))
        #expect(!VaultSecrets.isValidSecretName("1X"))
        #expect(!VaultSecrets.isValidSecretName("api_key"))
        #expect(!VaultSecrets.isValidSecretName(""))
        #expect(!VaultSecrets.isValidSecretName(String(repeating: "A", count: 65)))
    }
}

import Foundation
import Herder
@testable import HerderKit
import Testing

struct FixWithAITests {
    @Test func thePromptHoldsTheCommandExitCodeAndOutputTail() {
        let noise = (1...250).map { "line \($0)" }.joined(separator: "\n")
        let log = "$ npm install -g @openai/codex@latest\n\(noise)\nnpm error code EACCES\n\n"
        let request = FixRequest(job: "Installing the Codex CLI failed", machine: "trash-can-01", home: "/home/dev",
                                 log: log, exitCode: 243, failure: "The installer exited with 243.")
        let prompt = request.prompt
        #expect(prompt.hasPrefix("Installing the Codex CLI failed on this machine."))
        #expect(prompt.contains("- Command: `npm install -g @openai/codex@latest`"))
        #expect(prompt.contains("- Exit code: 243"))
        #expect(prompt.contains("home directory /home/dev"))
        #expect(prompt.contains("The installer exited with 243."))
        #expect(prompt.contains("npm error code EACCES"))
        // Only the last 200 lines: the command line and the first lines of noise are cut.
        #expect(request.output.split(separator: "\n").count == FixRequest.tailLines)
        #expect(request.output.hasPrefix("line 52\n") && !prompt.contains("line 51\n"))
        #expect(prompt.contains("Then run the command again"))
    }

    @Test func thePromptCarriesNoSecrets() {
        let log = """
            $ sh -c 'curl -fsSL https://x.ai/cli/install.sh | bash'
            export OPENAI_API_KEY=sk-proj-abcdefghijklmnopqrstuvwxyz
            Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload
            github_token: ghp_abcdefghijklmnopqrstuvwxyz0123
            curl: (22) The requested URL returned error: 403
            """
        let prompt = FixRequest(job: "Installing the Grok CLI failed", machine: "box", home: "/Users/dev", log: log,
                                exitCode: 22, failure: "The installer exited with 22.").prompt
        for secret in ["sk-proj-abcdefghijklmnopqrstuvwxyz", "eyJhbGciOiJIUzI1NiJ9.payload", "ghp_abcdefghijklmnopqrstuvwxyz0123"] {
            #expect(!prompt.contains(secret))
        }
        #expect(prompt.contains("OPENAI_API_KEY=[redacted]"))
        #expect(prompt.contains("returned error: 403"))
        #expect(prompt.contains("Never read, print or copy the provider CLIs' login tokens"))
    }

    @Test func aJobWithoutATerminalIsRunAgainAsAJob() {
        let prompt = FixRequest(job: "Updating herder failed", machine: "box", home: "/Users/dev", log: nil, exitCode: nil,
                                failure: "The download failed").prompt
        #expect(!prompt.contains("Command:") && !prompt.contains("Exit code:") && !prompt.contains("```"))
        #expect(prompt.contains("Then run the job again"))
    }

    @Test func fixWithAIIsOfferedToOwnersOnFailuresAShellCanFix() {
        let failed = JobProgress.failed("The installer exited with 1.")
        #expect(FixRequest.offered(.install("codex"), progress: failed, role: .owner))
        #expect(FixRequest.offered(.updateHerder, progress: failed, role: .owner))
        #expect(FixRequest.offered(.vault, progress: failed, role: .owner))
        // A member never gets a shell on the machine, so no agent fixing it either.
        #expect(!FixRequest.offered(.install("codex"), progress: failed, role: .member))
        #expect(!FixRequest.offered(.install("codex"), progress: failed, role: nil))
        #expect(!FixRequest.offered(.install("codex"), progress: .installing, role: .owner))
        #expect(!FixRequest.offered(.relogin("work"), progress: failed, role: .owner))
    }

    @MainActor
    @Test func theFixRunsOnASignedInAccountOfAnotherInstalledProvider() throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        func account(_ id: AccountId, _ provider: Provider, used: Double = 0, signedOut: Bool = false) -> Account {
            Account(accountId: id, provider: provider, label: id, configDir: nil, email: nil,
                    usage: [UsageWindow(window: "five_hour", usedPercent: used, resetsAt: nil)],
                    needsLogin: signedOut ? "2026-10-26T10:00:00Z" : nil)
        }
        func cli(_ provider: Provider, installed: Bool) -> ProviderStatus {
            ProviderStatus(provider: provider, installed: installed, version: installed ? "1.0.0" : nil, latestVersion: nil,
                           binary: nil, canInstall: true, canUpdate: installed)
        }
        fleet.setMachinesForTesting([machine("box", name: "trash-can-01", sessions: [], accounts: [
            account("codex", "codex"),
            account("signed-out", "claude", signedOut: true),
            account("busy", "claude", used: 90),
            account("free", "claude", used: 10),
            account("grok", "grok"),
        ], providers: [cli("codex", installed: false), cli("claude", installed: true), cli("grok", installed: false)])])
        // Not the Codex that failed to install, nor a signed-out or uninstalled one: the most room left.
        #expect(fleet.fixAccount(on: "box", avoiding: "codex")?.accountId == "free")
        #expect(fleet.fixAccount(on: "box", avoiding: "claude") == nil)
    }
}

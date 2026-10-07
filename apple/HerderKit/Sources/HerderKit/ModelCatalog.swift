import Herder

/// The models the model menu offers per provider, by their CLI names, with the default a new
/// session starts on. Models a machine has used are offered too.
enum ModelCatalog {
    struct Model: Hashable {
        let id: String
        let name: String
        /// A few words on when to pick it, where that helps.
        var detail: String?
    }

    /// The providers herder knows, in the order the menu lists them.
    static let knownProviders: [Provider] = ["claude", "codex", "gemini", "cursor", "grok", "opencode"]

    static func models(_ provider: Provider) -> [Model] {
        switch provider {
        case "claude":
            [
                Model(id: "claude-opus-5-5", name: "Claude Opus 5.5", detail: "Most capable"),
                Model(id: "claude-sonnet-5-5", name: "Claude Sonnet 5.5", detail: "Fast, for everyday work"),
                Model(id: "claude-fable-5-1", name: "Claude Fable 5.1"),
                Model(id: "claude-haiku-4-5-20251001", name: "Claude Haiku 4.5", detail: "Fastest"),
            ]
        case "codex":
            [
                Model(id: "gpt-6.1-sol", name: "GPT-6.1 Sol", detail: "Most capable"),
                Model(id: "gpt-6-luna", name: "GPT-6 Luna"),
            ]
        case "grok":
            [
                Model(id: "grok-4.6", name: "Grok 4.6", detail: "Most capable"),
                Model(id: "grok-4.5", name: "Grok 4.5"),
            ]
        case "cursor":
            [
                Model(id: "", name: "Cursor Auto", detail: "Lets Cursor pick"),
                Model(id: "composer-2.5", name: "Composer 2.5", detail: "Cursor's agent model"),
                Model(id: "composer-2.5-fast", name: "Composer 2.5 Fast", detail: "Faster Composer"),
            ]
        case "opencode":
            [Model(id: "", name: "OpenCode default")]
        default:
            []
        }
    }

    /// The model new sessions of a provider start on; `""` is the provider's own default.
    static func defaultModel(_ provider: Provider) -> String {
        provider == "claude" ? "claude-opus-5-5" : ""
    }

    /// A model's name for the menu: the catalog's, else its id.
    static func name(_ id: String, provider: Provider?) -> String {
        if id.isEmpty { return "\(providerName(provider ?? "")) default" }
        return models(provider ?? "").first { $0.id == id }?.name ?? id
    }

    /// A provider's name as people write it.
    static func providerName(_ provider: Provider) -> String {
        switch provider {
        case "claude": "Claude"
        case "codex": "Codex"
        case "gemini": "Gemini"
        case "cursor": "Cursor"
        case "grok": "Grok"
        case "opencode": "OpenCode"
        default: provider.prefix(1).uppercased() + provider.dropFirst()
        }
    }

    /// A provider's models in the menu.
    struct Group: Hashable, Identifiable {
        let provider: Provider
        let models: [Model]
        var id: Provider { provider }
    }

    /// One entry of the menu: a model of a provider.
    struct Choice: Hashable {
        let provider: Provider
        let model: String
    }

    /// The menu's groups: one per provider, known providers first, each with the catalog's
    /// models, then the ones used with it, then the one in use. The provider's default comes
    /// last where asked for, and always for a provider the catalog has no models for.
    static func groups(
        providers: [Provider], current: Choice?, used: [Provider: [String]], offersDefault: Bool
    ) -> [Group] {
        let ordered = Array(Set(providers)).sorted { a, b in
            let (i, j) = (knownProviders.firstIndex(of: a) ?? .max, knownProviders.firstIndex(of: b) ?? .max)
            return i == j ? a < b : i < j
        }
        return ordered.map { provider in
            var models = models(provider)
            var extra = used[provider] ?? []
            if let current, current.provider == provider { extra.append(current.model) }
            for id in extra where !id.isEmpty && !models.contains(where: { $0.id == id }) {
                models.append(Model(id: id, name: id))
            }
            if (offersDefault || models.isEmpty || current == Choice(provider: provider, model: ""))
                && !models.contains(where: { $0.id.isEmpty }) {
                models.append(Model(id: "", name: name("", provider: provider)))
            }
            return Group(provider: provider, models: models)
        }
    }

    /// The groups with only the models whose name, id, detail or provider has every word of
    /// the query; groups left empty drop out.
    static func filter(_ groups: [Group], _ query: String) -> [Group] {
        let words = query.lowercased().split(separator: " ")
        guard !words.isEmpty else { return groups }
        return groups.compactMap { group in
            let models = group.models.filter { model in
                let haystack = [model.name, model.id, model.detail ?? "", providerName(group.provider)]
                    .joined(separator: " ").lowercased()
                return words.allSatisfy { haystack.contains($0) }
            }
            return models.isEmpty ? nil : Group(provider: group.provider, models: models)
        }
    }

    /// Providers that have an account on another machine but not on `hostId`.
    static func usedElsewhere(_ machines: [Machine], hostId: HostId) -> [Provider] {
        let here = Set(machines.first { $0.hostId == hostId }?.accounts.map(\.provider) ?? [])
        return Array(Set(machines.filter { $0.hostId != hostId }.flatMap { $0.accounts.map(\.provider) })
            .subtracting(here)).sorted()
    }

    /// Machine names that already have an account for `provider`, other than `hostId`.
    static func usedOn(_ machines: [Machine], hostId: HostId, provider: Provider) -> [String] {
        machines.filter { $0.hostId != hostId && $0.accounts.contains { $0.provider == provider } }.map(\.name)
    }

    /// Whether `newer` looks like a later `--version` than `older`.
    static func versionNewer(_ newer: String, than older: String) -> Bool {
        versionKey(newer) > versionKey(older)
    }

    /// A later version of `provider` reported on another machine, if this host has an older one.
    static func newerElsewhere(_ machines: [Machine], hostId: HostId, provider: Provider) -> Machine? {
        guard let here = statusOn(machines, hostId: hostId, provider: provider)?.version else { return nil }
        return machines.first { machine in
            machine.hostId != hostId
                && statusOn(machines, hostId: machine.hostId, provider: provider)?.version
                    .map { versionNewer($0, than: here) } == true
        }
    }

    /// `provider`'s status on `hostId`, if that machine sent one.
    static func statusOn(_ machines: [Machine], hostId: HostId, provider: Provider) -> ProviderStatus? {
        machines.first { $0.hostId == hostId }?.providers.first { $0.provider == provider }
    }

    /// An unused account id for `provider` on this machine: `cursor`, then `cursor-2`.
    static func nextAccountId(_ accounts: [Account], provider: Provider) -> String {
        if !accounts.contains(where: { $0.accountId == provider }) { return provider }
        return (2...).lazy.map { "\(provider)-\($0)" }.first { id in
            !accounts.contains { $0.accountId == id }
        } ?? "\(provider)-new"
    }

    /// Quiet lines for a machine: providers used elsewhere, and a newer CLI on another host.
    static func machineHints(_ machines: [Machine], hostId: HostId) -> [String] {
        var lines: [String] = []
        let missing = usedElsewhere(machines, hostId: hostId)
        if !missing.isEmpty {
            lines.append("also used elsewhere: " + missing.joined(separator: ", "))
        }
        var seen = Set<Provider>()
        let machine = machines.first { $0.hostId == hostId }
        let providers = (machine?.accounts.map(\.provider) ?? []) + (machine?.providers.map(\.provider) ?? [])
        for provider in providers where seen.insert(provider).inserted {
            if let other = newerElsewhere(machines, hostId: hostId, provider: provider) {
                lines.append("\(provider): newer on \(other.name)")
            }
        }
        return lines
    }

    private static func versionKey(_ raw: String) -> [UInt64] {
        raw.split { !$0.isNumber }.compactMap { UInt64($0) }
    }
}

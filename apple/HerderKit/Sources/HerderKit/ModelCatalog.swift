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
                Model(id: "auto", name: "Auto", detail: "Picks for the task"),
                Model(id: "composer-2.5", name: "Composer 2.5", detail: "Cursor's agent"),
                Model(id: "composer-2.5-fast", name: "Composer 2.5 Fast"),
            ]
        case "opencode":
            [
                Model(id: "opencode/grok-code", name: "Grok Code"),
                Model(id: "opencode/claude", name: "Claude"),
                Model(id: "opencode/gpt", name: "GPT"),
            ]
        default:
            []
        }
    }

    /// The model new sessions of a provider start on; `""` is the provider's own default.
    static func defaultModel(_ provider: Provider) -> String {
        switch provider {
        case "claude": "claude-opus-5-5"
        case "cursor": "auto"
        default: ""
        }
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
            if offersDefault || models.isEmpty || current == Choice(provider: provider, model: "") {
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
}

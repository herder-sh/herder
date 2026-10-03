import Herder

/// The models the model menu offers per provider, by their CLI names, with the default a new
/// session starts on. Models a machine has used are offered too.
enum ModelCatalog {
    struct Model: Hashable {
        let id: String
        let name: String
    }

    static func models(_ provider: Provider) -> [Model] {
        switch provider {
        case "claude":
            [
                Model(id: "claude-opus-5-5", name: "Claude Opus 5.5"),
                Model(id: "claude-sonnet-5-5", name: "Claude Sonnet 5.5"),
                Model(id: "claude-fable-5-1", name: "Claude Fable 5.1"),
                Model(id: "claude-haiku-4-5-20251001", name: "Claude Haiku 4.5"),
            ]
        default:
            []
        }
    }

    /// The model new sessions of a provider start on; `""` is the provider's own default.
    static func defaultModel(_ provider: Provider) -> String {
        models(provider).first?.id ?? ""
    }

    /// A model's name for the menu: the catalog's, else its id.
    static func name(_ id: String, provider: Provider?) -> String {
        if id.isEmpty { return "\(provider ?? "") default" }
        return models(provider ?? "").first { $0.id == id }?.name ?? id
    }
}

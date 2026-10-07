import Herder

/// Quiet used-elsewhere and version lines for a machine's account list.
enum ProviderHints {
    /// One muted line per gap, from the other machines you own.
    static func lines(for machine: Machine, in all: [Machine]) -> [String] {
        hints(for: machine, in: all).map(\.text)
    }

    static func hints(for machine: Machine, in all: [Machine]) -> [Hint] {
        let here = Set(machine.accounts.map(\.provider))
        var out: [Hint] = []
        for provider in ModelCatalog.knownProviders where provider != "gemini" {
            let elsewhere = all.filter { $0.hostId != machine.hostId && $0.accounts.contains { $0.provider == provider } }
            if !elsewhere.isEmpty, !here.contains(provider) {
                out.append(Hint(provider: provider, text: "also on \(elsewhere.map(\.name).joined(separator: ", ")): \(provider)"))
                continue
            }
            guard let status = machine.providers.first(where: { $0.provider == provider }), status.installed else {
                continue
            }
            let newer = all.filter { $0.hostId != machine.hostId }
                .flatMap(\.providers)
                .filter { $0.provider == provider }
                .compactMap(\.version)
                .first { version($0, isNewerThan: status.version ?? "") }
            if let newer {
                out.append(Hint(provider: provider, text: "\(provider) \(status.version ?? "") · update"))
                _ = newer
            }
        }
        return out
    }

    static func version(_ left: String, isNewerThan right: String) -> Bool {
        parts(left) > parts(right)
    }

    private static func parts(_ text: String) -> [Int] {
        let start = text.firstIndex(where: \.isNumber) ?? text.endIndex
        let digits = text[start...].prefix { $0.isNumber || $0 == "." }
        return digits.split(separator: ".").compactMap { Int($0) }
    }

    struct Hint: Hashable {
        let provider: Provider
        let text: String
        var missing: Bool { text.hasPrefix("also on ") }
    }
}

import Foundation
import Herder

/// A terminal's output as plain text, for Copy log and for reading warnings out of it.
enum TerminalLog {
    /// `bytes` without their escape sequences (colours, cursor moves, titles), each line as it
    /// was left: a carriage return starts the line over, as a progress bar redraws it.
    static func plain(_ bytes: [UInt8]) -> String {
        var text: [UInt8] = []
        var index = 0
        while index < bytes.count {
            let byte = bytes[index]
            guard byte == 0x1B, index + 1 < bytes.count else {
                if byte >= 0x20 || byte == 0x0A || byte == 0x0D || byte == 0x09 { text.append(byte) }
                index += 1
                continue
            }
            switch bytes[index + 1] {
            case UInt8(ascii: "["):
                // CSI: parameters, then one final byte from @ to ~.
                index += 2
                while index < bytes.count, !(0x40...0x7E).contains(bytes[index]) { index += 1 }
                index += 1
            case UInt8(ascii: "]"):
                // OSC: up to BEL or ESC \.
                index += 2
                while index < bytes.count, bytes[index] != 0x07,
                      !(bytes[index] == 0x1B && index + 1 < bytes.count && bytes[index + 1] == UInt8(ascii: "\\")) {
                    index += 1
                }
                index += index < bytes.count && bytes[index] == 0x1B ? 2 : 1
            default:
                index += 2
            }
        }
        return String(decoding: text, as: UTF8.self)
            .replacingOccurrences(of: "\r\n", with: "\n")
            .split(separator: "\n", omittingEmptySubsequences: false)
            .map { line in line.split(separator: "\r", omittingEmptySubsequences: false).last(where: { !$0.isEmpty }) ?? "" }
            .joined(separator: "\n")
    }
}

/// A warning read out of a vendor's installer output, with the fix herder can offer for it.
struct VendorWarning: Equatable, Identifiable {
    let title: String
    let detail: String
    /// A command that fixes it on the machine, to copy and run there.
    var fix: String?

    var id: String { title }

    /// The warnings in `log`, the output of `provider`'s installer: the ones herder knows,
    /// explained with their fix, then any other line the vendor marked as a warning.
    static func read(_ log: String, provider: Provider) -> [VendorWarning] {
        let lines = log.split(separator: "\n").map { $0.trimmingCharacters(in: .whitespaces) }
        var warnings: [VendorWarning] = []
        if let multiple = multipleInstalls(lines, provider: provider) { warnings.append(multiple) }
        for line in lines {
            let lower = line.lowercased()
            guard let prefix = ["warning:", "warn:", "npm warn "].first(where: lower.hasPrefix) else { continue }
            // Known ones are explained above; npm's deprecation notes are about its own packages.
            if lower.contains("multiple installations") || lower.hasPrefix("npm warn deprecated") { continue }
            let text = line.dropFirst(prefix.count).trimmingCharacters(in: .whitespaces)
            guard !text.isEmpty, !warnings.contains(where: { $0.title == text }) else { continue }
            warnings.append(VendorWarning(title: text, detail: "Reported by the \(ModelCatalog.providerName(provider)) installer."))
        }
        return Array(warnings.prefix(5))
    }

    /// Claude's updater lists every copy it finds, `- npm-global at /opt/homebrew/bin/claude`,
    /// under "Multiple installations found"; an npm copy left behind may shadow the native one.
    private static func multipleInstalls(_ lines: [String], provider: Provider) -> VendorWarning? {
        guard provider == "claude",
              let start = lines.firstIndex(where: { $0.lowercased().contains("multiple installations found") }) else { return nil }
        let copies = lines[(start + 1)...].prefix { $0.hasPrefix("- ") }.map { $0.dropFirst(2) }
        guard copies.count > 1 else { return nil }
        let npm = copies.first { $0.hasPrefix("npm") }
        let running = copies.first { $0.contains("currently running") }
        let path = { (copy: Substring) in
            copy.components(separatedBy: " at ").last?.replacingOccurrences(of: " (currently running)", with: "") ?? ""
        }
        var detail = running.map { "The update went to the copy at \(path($0))." } ?? "The update went to one of them."
        if let npm {
            detail += " An npm copy at \(path(npm)) may come first on PATH for other tools."
        }
        return VendorWarning(title: "\(copies.count) Claude installs on this machine", detail: detail,
                             fix: npm.map { _ in "npm uninstall -g @anthropic-ai/claude-code" })
    }
}

import Herder
import SwiftUI

/// `$` skill mentions in the composer: typing `$` at the end of the prompt opens a picker of
/// the session's skills, and the one chosen goes in as `$name`, which the adapters rewrite for
/// each provider.
enum SkillMention {
    /// The skill name being typed at the end of `text`: what follows the `$` that starts its
    /// last word; `nil` when that word is no mention.
    static func query(in text: String) -> String? {
        let start = text.lastIndex(where: \.isWhitespace).map(text.index(after:)) ?? text.startIndex
        let word = text[start...]
        guard word.first == "$" else { return nil }
        let name = word.dropFirst()
        guard name.allSatisfy({ $0.isASCII && ($0.isLowercase || $0.isNumber || $0 == "-") }) else { return nil }
        return String(name)
    }

    /// The skills `query` picks: those whose name starts with it, then those that hold it,
    /// each by name.
    static func matches(_ query: String, in skills: [SessionSkill]) -> [SessionSkill] {
        let named = unique(skills).sorted { $0.name < $1.name }
        guard !query.isEmpty else { return named }
        return named.filter { $0.name.hasPrefix(query) }
            + named.filter { !$0.name.hasPrefix(query) && $0.name.contains(query) }
    }

    /// `text` with the mention being typed at its end completed to `name`, and a space after.
    static func complete(_ text: String, with name: String) -> String {
        guard let query = query(in: text) else { return text }
        return String(text.dropLast(query.count + 1)) + "$\(name) "
    }

    /// The skills `text` mentions, in the order it first does.
    static func mentioned(in text: String, skills: [SessionSkill]) -> [SessionSkill] {
        let byName = Dictionary(unique(skills).map { ($0.name, $0) }, uniquingKeysWith: { first, _ in first })
        var seen: [String] = []
        for (_, token) in PromptText.tokens(in: text, skills: Set(byName.keys)) where token.kind == .skill {
            if !seen.contains(token.name) { seen.append(token.name) }
        }
        return seen.compactMap { byName[$0] }
    }

    /// `text` without its mentions of `name`, each with the space after it.
    static func remove(_ name: String, from text: String) -> String {
        var out = ""
        var rest = text.startIndex
        for (range, token) in PromptText.tokens(in: text, skills: [name]) where token.kind == .skill {
            out += text[rest..<range.lowerBound]
            rest = range.upperBound
            if rest < text.endIndex && text[rest] == " " { rest = text.index(after: rest) }
        }
        return out + text[rest...]
    }

    /// One skill per name: a project skill of the same name as a library one is the one
    /// the agent gets, as its CLI prefers it.
    private static func unique(_ skills: [SessionSkill]) -> [SessionSkill] {
        var byName: [String: SessionSkill] = [:]
        for skill in skills where byName[skill.name]?.source != .project {
            byName[skill.name] = skill
        }
        return Array(byName.values)
    }
}

extension SessionSkill {
    /// Where it comes from, as the picker and chips say it.
    var sourceLabel: String { source == .library ? "Library" : "Project" }
}

extension Fleet {
    /// The skills a session's agent may use, as its machine lists them.
    func skills(of key: SessionKey) -> [SessionSkill] {
        machines.first { $0.hostId == key.hostId }?.sessionSkills[key.sessionId] ?? []
    }

    /// The library skills enabled on a machine, which a new session there gets.
    func librarySkills(on hostId: HostId) -> [SessionSkill] {
        let library = machines.first { $0.hostId == hostId }?.skills?.skills ?? []
        return library.filter(\.enabled).map {
            SessionSkill(name: $0.name, description: $0.description, source: .library, path: nil)
        }
    }
}

/// The skills a `$` mention being typed picks, above the prompt; the first is what Return
/// chooses.
struct SkillPicker: View {
    let skills: [SessionSkill]
    let choose: (SessionSkill) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(Array(skills.enumerated()), id: \.element) { index, skill in
                Button { choose(skill) } label: {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        SwiftUI.Image(systemName: "book.closed").imageScale(.small).foregroundStyle(Theme.secondary)
                        Text("$\(skill.name)").font(Theme.mono.weight(.semibold)).foregroundStyle(Theme.text)
                            .lineLimit(1).layoutPriority(1)
                        Text(skill.description).font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
                        Spacer(minLength: 8)
                        Text(skill.sourceLabel).font(.caption2.weight(.medium)).foregroundStyle(Theme.tertiary)
                    }
                    .padding(.horizontal, 10)
                    .frame(maxWidth: .infinity, minHeight: 34, alignment: .leading)
                    .background(index == 0 ? Theme.raised : .clear, in: .rect(cornerRadius: 8))
                    .contentShape(.rect)
                    .hitTarget()
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("skill-option-\(skill.name)")
            }
        }
        .padding(6)
        // A container of its own: without it the identifier replaces each option's.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("skill-picker")
    }
}

/// The skills the prompt mentions, as chips with their description; on iPhone and iPad, whose
/// text view shows the mentions as typed.
struct SkillStrip: View {
    let skills: [SessionSkill]
    let remove: (SessionSkill) -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(skills, id: \.self) { skill in
                    HStack(spacing: 6) {
                        SwiftUI.Image(systemName: "book.closed").imageScale(.small).foregroundStyle(Theme.secondary)
                        Text("$\(skill.name)").font(Theme.monoSmall.weight(.semibold)).foregroundStyle(Theme.text)
                        Text(skill.description).font(.caption).foregroundStyle(Theme.tertiary).lineLimit(1)
                            .frame(maxWidth: 220, alignment: .leading)
                        Button { remove(skill) } label: {
                            SwiftUI.Image(systemName: "xmark.circle.fill").foregroundStyle(Theme.tertiary)
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Remove \(skill.name)")
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(Theme.raised, in: .rect(cornerRadius: 6))
                    .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(Theme.stroke))
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier("skill-chip-\(skill.name)")
                }
            }
            .padding(.horizontal, 16)
            .padding(.top, 12)
        }
    }
}

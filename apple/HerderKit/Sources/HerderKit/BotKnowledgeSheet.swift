import Herder
import SwiftUI

/// A bot's instructions and memory, as one sheet: the instructions its owners write, then the
/// facts it keeps, each edited in place or deleted, with where it was learned. Members see both
/// and change neither.
struct BotKnowledgeSheet: View {
    let knowledge: BotKnowledge
    let name: String
    @State private var confirmingClear = false
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        SheetScaffold(title: "Instructions & memory", subtitle: name, height: 680,
                      hint: knowledge.editable ? "People can also tell the bot to remember or forget things in chat." : "Only the machine’s owners change these.") {
            BotKnowledgeForm(knowledge: knowledge)
        } footer: {
            Spacer()
            if knowledge.editable {
                DangerButton(title: "Clear memory…") { confirmingClear = true }
                    .disabled(knowledge.list.isEmpty)
                    .accessibilityIdentifier("bot-clear-memory")
            }
            ActionButton(title: "Done", style: .primary) { dismiss() }
                .frame(width: 120)
                .keyboardShortcut(.defaultAction)
        }
        .task { await knowledge.reload() }
        .confirmationDialog("Clear what \(name) remembers?", isPresented: $confirmingClear, titleVisibility: .visible) {
            Button("Clear memory", role: .destructive) { Task { await knowledge.clear() } }
        } message: {
            Text("Its instructions stay. New conversations start knowing nothing else.")
        }
    }
}

/// The sheet's sections, on a `BotKnowledge` alone so they can be drawn without a fleet.
struct BotKnowledgeForm: View {
    @Bindable var knowledge: BotKnowledge
    /// The fact being edited, with its text so far.
    @State private var editing: (id: FactId, text: String)?
    @State private var adding = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: "Instructions")
            if knowledge.editable {
                InputBox(placeholder: "What every conversation should know: tone, conventions, who to ask…",
                         text: $knowledge.instructions, lines: 5...14)
                    .accessibilityIdentifier("bot-instructions")
                HStack(spacing: 8) {
                    Text(knowledge.instructionsProblem ?? "Markdown. Every new session the bot starts is told these first.")
                        .font(.footnote)
                        .foregroundStyle(knowledge.instructionsProblem == nil ? Theme.tertiary : Theme.failure)
                    Spacer(minLength: 8)
                    if knowledge.changed {
                        PillButton(title: "Revert") { knowledge.instructions = knowledge.saved }
                        PillButton(title: "Save", prominent: true) { Task { await knowledge.saveInstructions() } }
                            .disabled(knowledge.instructionsProblem != nil)
                            .accessibilityIdentifier("bot-instructions-save")
                    }
                }
            } else {
                TranscriptText(knowledge.saved.isEmpty ? "No instructions." : knowledge.saved, style: .subheadline,
                               color: knowledge.saved.isEmpty ? Theme.tertiary : Theme.text)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(12)
                    .background(.card, in: .rect(cornerRadius: Theme.corner))
            }
        }
        memory
        if let error = knowledge.error {
            Text(error).font(.footnote).foregroundStyle(Theme.failure).accessibilityIdentifier("bot-memory-error")
        }
    }

    private var memory: some View {
        SettingsList(title: memoryTitle) {
            switch knowledge.facts {
            case .asking:
                ProgressView().frame(maxWidth: .infinity, minHeight: Theme.tallRowHeight)
            case .failed(let why):
                Text(why).font(.subheadline).foregroundStyle(Theme.failure)
                    .padding(.horizontal, Theme.cellPadding).frame(maxWidth: .infinity, minHeight: Theme.tallRowHeight, alignment: .leading)
            case .loaded(let facts):
                if facts.isEmpty {
                    Text("Nothing yet. The bot saves what future conversations need as it learns it.")
                        .font(.subheadline).foregroundStyle(Theme.tertiary)
                        .padding(.horizontal, Theme.cellPadding).frame(maxWidth: .infinity, minHeight: Theme.tallRowHeight, alignment: .leading)
                }
                ForEach(Array(facts.enumerated()), id: \.element.id) { index, fact in
                    if index > 0 { RowDivider() }
                    factRow(fact)
                }
                if knowledge.editable {
                    RowDivider()
                    HStack(spacing: 10) {
                        InputBox(placeholder: "Add a fact, such as who leads a team", text: $adding)
                            .accessibilityIdentifier("bot-fact-new")
                        PillButton(title: "Add", symbol: "plus") {
                            Task { if await knowledge.setFact(nil, adding) { adding = "" } }
                        }
                        .disabled(adding.trimmingCharacters(in: .whitespaces).isEmpty)
                    }
                    .padding(10)
                }
            }
        }
    }

    private var memoryTitle: String {
        let count = knowledge.list.count
        return count == 0 ? "Memory" : "Memory · \(count) of \(maxBotFacts())"
    }

    @ViewBuilder private func factRow(_ fact: BotFact) -> some View {
        if let editing, editing.id == fact.id {
            HStack(spacing: 10) {
                InputBox(placeholder: fact.text, text: Binding(get: { editing.text }, set: { self.editing?.text = $0 }))
                    .accessibilityIdentifier("bot-fact-edit")
                PillButton(title: "Cancel") { self.editing = nil }
                PillButton(title: "Save", prominent: true) {
                    Task { if await knowledge.setFact(fact.id, editing.text) { self.editing = nil } }
                }
            }
            .padding(10)
        } else {
            HStack(alignment: .top, spacing: 10) {
                VStack(alignment: .leading, spacing: 3) {
                    TranscriptText(fact.text, style: .subheadline)
                    HStack(spacing: 6) {
                        Text(BotKnowledge.learned(fact)).font(.caption).foregroundStyle(Theme.tertiary)
                        if let url = fact.thread?.url.flatMap(URL.init) {
                            Link("Open thread", destination: url).font(.caption.weight(.semibold)).foregroundStyle(Theme.secondary)
                        }
                    }
                }
                Spacer(minLength: 8)
                if knowledge.editable {
                    IconButton(symbol: "pencil", help: "Edit") { editing = (fact.id, fact.text) }
                    IconButton(symbol: "trash", help: "Delete") { Task { await knowledge.delete(fact.id) } }
                }
            }
            .padding(.horizontal, Theme.cellPadding)
            .padding(.vertical, 8)
            .frame(minHeight: Theme.tallRowHeight)
            .accessibilityIdentifier("bot-fact-\(fact.id)")
        }
    }
}

/// The bot page's row that opens its Instructions & memory sheet, with what it holds.
struct BotKnowledgeRow: View {
    let knowledge: BotKnowledge
    let open: () -> Void

    var body: some View {
        SettingsList(title: "Instructions & memory") {
            SummaryRow(title: "Instructions & memory", summary: knowledge.summary, open: open)
                .accessibilityIdentifier("bot-knowledge")
        }
    }
}

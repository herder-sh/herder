import Herder
import SwiftUI

/// Says when a session's agents act for a user its machine does not have, so the machine
/// refuses their herder commands: a user since unpaired. A prompt of the reader's makes them
/// act for the reader.
struct ActsForBanner: View {
    let machine: Machine
    /// The user the session's agents act for, its primary's for a child.
    let actsFor: UserId
    @State private var people: People

    init(fleet: Fleet, machine: Machine, actsFor: UserId) {
        self.machine = machine
        self.actsFor = actsFor
        _people = State(initialValue: People(fleet: fleet, hostId: machine.hostId))
    }

    var body: some View {
        Group {
            if case .loaded(let users) = people.users, !users.contains(where: { $0.userId == actsFor }) {
                SessionColumnLayout {
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Image(systemName: "person.crop.circle.badge.exclamationmark").foregroundStyle(Theme.accent)
                        Text("Agents here act as \(actsFor), who isn't paired with \(machine.name). Send a prompt and they act as you.")
                            .foregroundStyle(Theme.text)
                            .fixedSize(horizontal: false, vertical: true)
                        Spacer(minLength: 0)
                    }
                    .padding(.vertical, 10)
                }
                .font(.caption.weight(.semibold))
                .background(Theme.accent.opacity(0.09))
                .overlay(alignment: .bottom) { Rectangle().fill(Theme.accent.opacity(0.22)).frame(height: 1) }
                .accessibilityElement(children: .combine)
            }
        }
        .task(id: "\(machine.connection == .connected) \(actsFor)") {
            if machine.connection == .connected { await people.reload() }
        }
    }
}

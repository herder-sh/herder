import Foundation
import Herder
@testable import HerderKit
import Testing

struct BotSessionsTests {
    static let dm = SessionOrigin(botId: "trash-can", kind: .slack, by: "Ada", place: .direct)
    static let channel = SessionOrigin(botId: "ops", kind: .slack, by: "Bob", place: .channel(name: "deploys"))

    /// A machine with a project session the app started and a bot has linked a thread to, a chat
    /// of the app's, Ada's DM to `trash-can` with a child under it, and Bob's mention of `ops` in
    /// #deploys, which waits on an approval.
    static func lists() -> Lists {
        var host = machine("h", name: "h", sessions: ["01A", "01B", "01C", "01D", "01E"])
        host.sessions[0].projectId = "github.com/acme/app"
        host.sessions[0].threads = [ThreadLink(kind: .slack, botId: "trash-can", channelId: "D1", threadId: "1.1", url: nil)]
        host.sessions[1].chat = true
        host.sessions[1].origin = dm
        host.sessions[2].chat = true
        host.sessions[2].parent = "01B"
        host.sessions[3].projectId = "github.com/acme/app"
        host.sessions[3].origin = channel
        host.sessions[4].chat = true
        var scripts = ["01A", "01B", "01C", "01D", "01E"].map { Script($0, host: "h") }
        let approval = EventBody.approvalRequested(approvalId: "a", turnId: "t", toolCallId: "i", summary: "Deploy",
                                                   routedTo: .user, reason: nil, secret: nil)
        let events: [[EventBody]] = [[created()], [created()], [created(parent: "01B")], [created(), approval], [created()]]
        var sessions: [SessionKey: SessionModel] = [:]
        for index in scripts.indices { sessions[scripts[index].key] = scripts[index].model(events[index]) }
        return Lists(machines: [host], sessions: sessions)
    }

    @Test func botConversationsAreOnlyUnderBotsGroupedByBot() {
        let lists = Self.lists()
        #expect(lists.chats.map(\.key.sessionId) == ["01E"])
        #expect(lists.projects.flatMap(\.sessions).map(\.key.sessionId) == ["01A"])
        #expect(lists.bots.map(\.id) == ["h/ops", "h/trash-can"])
        #expect(lists.bots.map { $0.sessions.map(\.key.sessionId) } == [["01D"], ["01B", "01C"]])
        #expect(lists.bots(nil).count == 2)
        #expect(lists.bots("h/ops").map(\.botId) == ["ops"])
    }

    @Test func aWaitingBotTaskStillReachesTheBoardAndActivity() {
        let lists = Self.lists()
        #expect(Set(lists.board().map(\.lead.key.sessionId)) == ["01A", "01D"])
        #expect(lists.board().first { $0.lead.key.sessionId == "01D" }?.project == "Bob · #deploys")
        #expect(lists.requests.map(\.session.key.sessionId) == ["01D"])
    }

    @Test func botsCountOnlyWhatWaitsForYou() {
        let lists = Self.lists()
        #expect(lists.botsWaiting == 1)
        #expect(lists.bots.map(\.waiting) == [1, 0])
    }

    @Test func aRowSaysWhoWroteAndWhere() {
        #expect(Self.dm.line == "Ada · DM")
        #expect(Self.channel.line == "Bob · #deploys")
        let telegram = SessionOrigin(botId: "t", kind: .telegram, by: "Ada", place: .channel(name: "Team"))
        #expect(telegram.line == "Ada · Team")
    }
}

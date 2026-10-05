import Herder
@testable import HerderKit
import Testing

private var deploy: SessionSkill {
    SessionSkill(name: "deploy", description: "Deploy the app to staging.", source: .project, path: ".claude/skills/deploy")
}
private var review: SessionSkill {
    SessionSkill(name: "review-pr", description: "Review a pull request.", source: .library, path: nil)
}
private var release: SessionSkill {
    SessionSkill(name: "release-notes", description: "Write release notes.", source: .library, path: nil)
}

struct SkillMentionTests {
    @Test func aDollarStartingTheLastWordOpensTheQuery() {
        #expect(SkillMention.query(in: "$") == "")
        #expect(SkillMention.query(in: "Then $re") == "re")
        #expect(SkillMention.query(in: "line\n$dep") == "dep")
        #expect(SkillMention.query(in: "$deploy ") == nil)
        #expect(SkillMention.query(in: "costs 5$") == nil)
        #expect(SkillMention.query(in: "$HOME") == nil)
        #expect(SkillMention.query(in: "") == nil)
    }

    @Test func namesStartingWithTheQueryComeFirst() {
        let skills = [review, deploy, release]
        #expect(SkillMention.matches("", in: skills).map(\.name) == ["deploy", "release-notes", "review-pr"])
        #expect(SkillMention.matches("re", in: skills).map(\.name) == ["release-notes", "review-pr"])
        #expect(SkillMention.matches("e", in: skills).map(\.name) == ["deploy", "release-notes", "review-pr"])
        #expect(SkillMention.matches("pr", in: skills).map(\.name) == ["review-pr"])
        #expect(SkillMention.matches("zzz", in: skills).isEmpty)
    }

    @Test func aProjectSkillHidesTheLibrarySkillOfItsName() {
        let library = SessionSkill(name: "deploy", description: "Library deploy.", source: .library, path: nil)
        #expect(SkillMention.matches("", in: [library, deploy]) == [deploy])
    }

    @Test func choosingCompletesTheMention() {
        #expect(SkillMention.complete("$", with: "deploy") == "$deploy ")
        #expect(SkillMention.complete("Please $dep", with: "deploy") == "Please $deploy ")
        #expect(SkillMention.complete("no mention", with: "deploy") == "no mention")
    }

    @Test func theSessionsSkillsAreMarkersAndOthersAreText() {
        let text = "$deploy then [Image #1] $review-pr, not $deployed nor a$deploy nor $HOME"
        let tokens = PromptText.tokens(in: text, skills: ["deploy", "review-pr"])
        #expect(tokens.map(\.token.marker) == ["$deploy", "[Image #1]", "$review-pr"])
        #expect(tokens.map { String(text[$0.range]) } == ["$deploy", "[Image #1]", "$review-pr"])
        #expect(PromptText.tokens(in: text).map(\.token.marker) == ["[Image #1]"])
    }

    @Test func theChipsAreTheSkillsMentionedInOrder() {
        let text = "$review-pr first, then $deploy, then $review-pr again and $unknown"
        #expect(SkillMention.mentioned(in: text, skills: [deploy, review]) == [review, deploy])
    }

    @Test func removingAChipRemovesEachMention() {
        #expect(SkillMention.remove("deploy", from: "$deploy now, $deploy again") == "now, again")
        #expect(SkillMention.remove("deploy", from: "Ship $deploy") == "Ship ")
        #expect(SkillMention.remove("deploy", from: "$deployed") == "$deployed")
    }
}

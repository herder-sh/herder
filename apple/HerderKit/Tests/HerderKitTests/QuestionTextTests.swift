@testable import HerderKit
import Testing

struct QuestionTextTests {
    @Test func eachChoiceTakesItsDescriptionFromTheText() {
        let text = """
        How should icons be set?

        - **Upload any image (Recommended)**: Pick one on the Mac or phone.
        - **Pick a file**: Choose one in the clone.
        - **Elsewhere**: not a choice, so it stays.
        """
        let question = QuestionText(text, choices: ["Upload any image (Recommended)", "Pick a file", "Both"])
        #expect(question.body == "How should icons be set?\n\n- **Elsewhere**: not a choice, so it stays.")
        #expect(question.choices == [
            .init(label: "Upload any image", detail: "Pick one on the Mac or phone.", recommended: true),
            .init(label: "Pick a file", detail: "Choose one in the clone."),
            .init(label: "Both"),
        ])
    }

    @Test func aPlainQuestionIsLeftAsItIs() {
        let question = QuestionText("Proceed?", choices: ["Yes", "No"])
        #expect(question.body == "Proceed?")
        #expect(question.choices.map(\.label) == ["Yes", "No"])
    }
}

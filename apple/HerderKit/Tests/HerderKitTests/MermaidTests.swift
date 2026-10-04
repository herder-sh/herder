import Foundation
import Testing
@testable import HerderKit

@MainActor
struct MermaidTests {
    @Test func closedMermaidFencesBecomeDiagramsAndOpenOnesStayCode() {
        let flow = "flowchart LR\n  A --> B"
        #expect(MarkdownText.parse("Here:\n```mermaid\n\(flow)\n```\nDone") == [
            .line("Here:"), .diagram(flow), .line("Done")])
        #expect(MarkdownText.parse("```Mermaid\n\(flow)\n```") == [.diagram(flow)])
        #expect(MarkdownText.parse("```mermaid\n\(flow)", streaming: true) == [.code(flow, language: "mermaid")])
        #expect(MarkdownText.parse("```mermaid\n\n```") == [.code("", language: "mermaid")])
    }

    @Test func theLibraryAndItsLicenseShipInTheBundle() throws {
        let library = try #require(Bundle.module.url(forResource: "mermaid.min", withExtension: "js", subdirectory: "Mermaid"))
        #expect(try Data(contentsOf: library).count > 1_000_000)
        let license = try #require(Bundle.module.url(forResource: "LICENSE", withExtension: nil, subdirectory: "Mermaid"))
        #expect(try String(contentsOf: license, encoding: .utf8).contains("MIT License"))
    }

    @Test func rendersDiagramsAndReportsParseErrors() async throws {
        let source = "flowchart LR\n  A[Start] --> B{Ok?}\n  B -->|yes| C[Done]"
        guard case .diagram(let svg, let size) = await MermaidRenderer.shared.render(source) else {
            Issue.record("flowchart did not render")
            return
        }
        #expect(svg.hasPrefix("<svg"))
        #expect(size.width > size.height && size.height > 0)
        #expect(MermaidRenderer.shared.cached(source) == .diagram(svg: svg, size: size))

        guard case .failed(let message) = await MermaidRenderer.shared.render("flowchart LR\n  A --> -->") else {
            Issue.record("invalid source rendered")
            return
        }
        #expect(!message.isEmpty)
    }
}

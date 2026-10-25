import Herder
import SwiftUI

/// An object that opens as a page (`DetailPage`): in the main area on the Mac and iPad, under a
/// breadcrumb back to where it was opened from; pushed on the navigation stack on iPhone.
/// Sheets are for short create flows and confirmations only, and never open another sheet.
enum Page: Hashable {
    case machine(HostId)
    /// A login across the machines, by `ProviderAccounts.Login.id`.
    case account(loginId: String)
    /// A project's settings, by its group's id.
    case projectSettings(projectId: String)
    /// The skills screen, opened from a project's settings.
    case skills
    /// A vault, for its owners.
    case vault(HostId)
    /// A provider's own sub-agent, in its session.
    case agent(SessionKey, NativeAgent.ID)
}

extension Page {
    /// The page's name in a breadcrumb.
    @MainActor func title(in fleet: Fleet) -> String {
        switch self {
        case .machine(let hostId), .vault(let hostId):
            return fleet.machines.first { $0.hostId == hostId }?.name ?? "Machine"
        case .account(let loginId):
            let login = ProviderAccounts(machines: fleet.machines).groups.flatMap(\.logins).first { $0.id == loginId }
            return login.map { "\(ModelCatalog.providerName($0.provider)) · \($0.email ?? $0.account.label)" } ?? "Account"
        case .projectSettings:
            return "Settings"
        case .skills:
            return "Skills"
        case .agent(let key, let id):
            return fleet.session(key).flatMap { NativeAgent.find(id, in: $0) }?.title ?? "Sub-agent"
        }
    }

    @MainActor @ViewBuilder func view(fleet: Fleet) -> some View {
        switch self {
        case .machine(let hostId): MachinePage(fleet: fleet, hostId: hostId)
        case .account(let loginId): AccountPage(fleet: fleet, loginId: loginId)
        case .projectSettings(let projectId): ProjectSettingsPage(fleet: fleet, projectId: projectId)
        case .skills: SkillsView(fleet: fleet)
        case .vault(let hostId): VaultPage(fleet: fleet, vault: hostId)
        case .agent(let key, let id): NativeAgentPage(reference: id, fleet: fleet, key: key)
        }
    }
}

/// Opens a page over the one shown, and closes the one on top; `DesktopShell` and the iPhone's
/// tabs each set it for their own navigation.
struct PageNavigator {
    var open: @MainActor (Page) -> Void = { _ in }
    var close: @MainActor () -> Void = {}
}

extension EnvironmentValues {
    @Entry var pages = PageNavigator()
}

/// The pages open over a section on the Mac and iPad, on top of each other.
struct PageStack: Equatable {
    private(set) var pages: [Page] = []

    var top: Page? { pages.last }

    /// Opens `page`; one open already below becomes the top again, rather than opening twice.
    mutating func open(_ page: Page) {
        if let index = pages.firstIndex(of: page) {
            pages.removeSubrange((index + 1)...)
        } else {
            pages.append(page)
        }
    }

    /// Back one page.
    mutating func close() {
        _ = pages.popLast()
    }

    /// Back to the `depth`th page, 0 being the section itself.
    mutating func back(to depth: Int) {
        pages.removeSubrange(min(depth, pages.count)...)
    }
}

/// The breadcrumb over a page on the Mac and iPad: a back button (⌘[), the section, then each
/// page open on the way, each one a way back to it.
struct PageTrail: View {
    /// The section first, then the pages.
    let crumbs: [String]
    let back: (Int) -> Void

    var body: some View {
        HStack(spacing: 6) {
            Button { back(crumbs.count - 2) } label: {
                Image(systemName: "chevron.left")
                    .font(.callout.weight(.semibold))
                    .foregroundStyle(Theme.secondary)
                    .frame(width: 28, height: 28)
                    .hitTarget()
            }
            .buttonStyle(.plain)
            .keyboardShortcut("[", modifiers: .command)
            .help("Back to \(crumbs.dropLast().last ?? "")")
            .accessibilityLabel("Back")
            ForEach(Array(crumbs.enumerated()), id: \.offset) { index, crumb in
                if index > 0 {
                    Image(systemName: "chevron.right").font(.caption2.weight(.bold)).foregroundStyle(Theme.tertiary)
                }
                if index == crumbs.count - 1 {
                    Text(crumb).foregroundStyle(Theme.text).lineLimit(1)
                } else {
                    Button(crumb) { back(index) }
                        .buttonStyle(.plain)
                        .foregroundStyle(Theme.secondary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
        .font(Theme.body.weight(.medium))
        .padding(.horizontal, 14)
        .frame(height: 40)
    }
}

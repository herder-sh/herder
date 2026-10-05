import Herder
import SwiftTerm
import SwiftUI

/// One shell on a machine, attached through the client core's terminal stream: output feeds
/// the emulator, keystrokes and size changes go back. The shell keeps running on the machine.
@MainActor
@Observable
final class TerminalConnection {
    enum State: Equatable {
        case connecting
        case attached
        case exited(Int32?)
        case failed(String)
    }

    let hostId: HostId
    /// The account whose login this terminal runs to add it.
    let account: NewAccount?
    /// The existing account whose login this terminal runs again.
    let relogin: AccountId?
    private(set) var terminalId: TerminalId?
    private(set) var state: State = .connecting
    @ObservationIgnored private var stream: TerminalStream?
    @ObservationIgnored private var pump: Task<Void, Never>?
    /// The emulator, made once and kept with the connection, so leaving the pane and coming
    /// back shows the same screen.
    @ObservationIgnored private var emulator: SwiftTerm.TerminalView?
    @ObservationIgnored private var delegate: EmulatorDelegate?

    init(hostId: HostId, terminalId: TerminalId?, account: NewAccount? = nil, relogin: AccountId? = nil) {
        self.account = account
        self.relogin = relogin
        self.hostId = hostId
        self.terminalId = terminalId
    }

    /// Opens a new shell in the session's worktree, or a login, or attaches to `terminalId`.
    func connect(client: Client, sessionId: SessionId?, cols: Int, rows: Int) {
        guard pump == nil else { return }
        pump = Task {
            do {
                let stream = try await withTimeout(seconds: 10) { [hostId, terminalId, account, relogin] in
                    if let terminalId {
                        try await client.attachTerminal(hostId: hostId, terminalId: terminalId)
                    } else if let account {
                        try await client.addAccount(
                            hostId: hostId, account: account, cols: UInt16(clamping: cols), rows: UInt16(clamping: rows))
                    } else if let relogin {
                        try await client.logInAccount(
                            hostId: hostId, accountId: relogin, cols: UInt16(clamping: cols), rows: UInt16(clamping: rows))
                    } else if let sessionId {
                        try await client.openTerminal(
                            hostId: hostId, sessionId: sessionId, cols: UInt16(clamping: cols), rows: UInt16(clamping: rows))
                    } else {
                        throw HerderError.Local(detail: "No session or account selected.")
                    }
                }
                self.stream = stream
                terminalId = stream.terminalId()
                stream.resize(cols: UInt16(clamping: cols), rows: UInt16(clamping: rows))
                state = .attached
                while let event = await stream.next(), !Task.isCancelled {
                    switch event {
                    case .output(let data): feed(Array(data))
                    case .reattached: emulator?.getTerminal().resetToInitialState()
                    case .closed(let code):
                        state = .exited(code)
                        return
                    }
                }
                if state == .attached { state = .failed("The connection to the machine stopped.") }
            } catch let error as HerderError {
                if case .Rejected(let info) = error, info.code == .forbidden {
                    state = .failed("Terminals are owner-only.")
                } else {
                    state = .failed(error.message)
                }
            } catch {
                state = .failed(describe(error))
            }
        }
    }

    func input(_ bytes: ArraySlice<UInt8>) {
        stream?.input(data: Data(bytes))
    }

    func resize(cols: Int, rows: Int) {
        stream?.resize(cols: UInt16(clamping: cols), rows: UInt16(clamping: rows))
    }

    /// The emulator for this shell, wired to it on first use.
    func view(client: Client, sessionId: SessionId?) -> SwiftTerm.TerminalView {
        if let emulator { return emulator }
        let view = SwiftTerm.TerminalView(frame: .zero)
        let delegate = EmulatorDelegate(connection: self, client: client, sessionId: sessionId)
        view.terminalDelegate = delegate
        view.font = .monospacedSystemFont(ofSize: 13, weight: .regular)
        view.nativeBackgroundColor = .black
        #if os(macOS)
        view.nativeForegroundColor = NSColor(white: 0.92, alpha: 1)
        #else
        view.nativeForegroundColor = UIColor(white: 0.92, alpha: 1)
        #endif
        emulator = view
        self.delegate = delegate
        return view
    }

    private func feed(_ bytes: [UInt8]) {
        emulator?.feed(byteArray: bytes[...])
    }
}

/// Runs `body`, failing with a local error if it takes longer than `seconds`.
private func withTimeout<T: Sendable>(seconds: Double, _ body: @escaping @Sendable () async throws -> T) async throws -> T {
    try await withThrowingTaskGroup(of: T.self) { group in
        group.addTask { try await body() }
        group.addTask {
            try await Task.sleep(for: .seconds(seconds))
            throw HerderError.Local(detail: "The machine did not open the terminal in time.")
        }
        defer { group.cancelAll() }
        guard let first = try await group.next() else { throw HerderError.Closed }
        return first
    }
}

/// A session's shells and which one is shown. The fleet keeps it for as long as the app runs:
/// a shell's stream cannot be attached twice, so hiding the pane must not let it go.
@MainActor
@Observable
final class SessionTerminals {
    var connections: [TerminalConnection] = []
    var selected = 0
}

extension Fleet {
    /// The shells of a session, made the first time its terminal pane opens.
    func terminals(of key: SessionKey) -> SessionTerminals {
        if let terminals = sessionTerminals[key] { return terminals }
        let terminals = SessionTerminals()
        sessionTerminals[key] = terminals
        return terminals
    }
}

/// A session's shells: one tab per shell the machine has open for it, a new one, and the
/// emulator for the selected tab. Owners only, as the daemon enforces.
struct TerminalPane: View {
    let fleet: Fleet
    let key: SessionKey

    private var store: SessionTerminals { fleet.terminals(of: key) }
    private var connections: [TerminalConnection] { store.connections }

    var body: some View {
        let machine = fleet.machines.first { $0.hostId == key.hostId }
        Group {
            if machine?.role != .owner {
                VStack(spacing: 8) {
                    Image(systemName: "lock").font(.title2).foregroundStyle(Theme.tertiary)
                    Text(machine?.role == nil ? "The machine has not connected yet." : "Terminals are owner-only.")
                        .foregroundStyle(Theme.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                VStack(spacing: 0) {
                    tabs
                    if connections.indices.contains(store.selected) {
                        let connection = connections[store.selected]
                        TerminalSurface(connection: connection, client: fleet.client, sessionId: key.sessionId)
                            .id(ObjectIdentifier(connection))
                    }
                }
            }
        }
        .background(Color.black)
        .onAppear(perform: load)
    }

    private var tabs: some View {
        HStack(spacing: 4) {
            ForEach(Array(connections.enumerated()), id: \.offset) { index, connection in
                Button { store.selected = index } label: {
                    HStack(spacing: 6) {
                        Image(systemName: "terminal").imageScale(.small)
                        Text("Shell \(index + 1)")
                        if case .exited = connection.state { Text("exited").foregroundStyle(Theme.tertiary) }
                    }
                    .font(.caption.weight(.medium))
                    .foregroundStyle(index == store.selected ? Theme.text : Theme.secondary)
                    .padding(.horizontal, 10)
                    .frame(height: 28)
                    .background(index == store.selected ? Theme.raised : .clear, in: .rect(cornerRadius: 7))
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
            }
            Button {
                store.connections.append(TerminalConnection(hostId: key.hostId, terminalId: nil))
                store.selected = store.connections.count - 1
            } label: {
                Image(systemName: "plus").font(.caption.weight(.bold)).foregroundStyle(Theme.secondary)
                    .frame(width: 28, height: 28).contentShape(.rect)
            }
            .buttonStyle(.plain)
            .help("New shell in the session's worktree")
            Spacer()
            Text("Shells keep running on the machine.").font(.caption2).foregroundStyle(Theme.tertiary)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(Theme.surface)
    }

    /// The machine's open shells of this session, then a new one if there are none.
    private func load() {
        guard connections.isEmpty else { return }
        let shells = fleet.machines.first { $0.hostId == key.hostId }?.terminals.compactMap { terminal -> TerminalId? in
            if case .shell(let sessionId) = terminal.purpose, sessionId == key.sessionId { return terminal.terminalId }
            return nil
        } ?? []
        store.connections = shells.map { TerminalConnection(hostId: key.hostId, terminalId: $0) }
        if store.connections.isEmpty { store.connections = [TerminalConnection(hostId: key.hostId, terminalId: nil)] }
        store.selected = 0
    }
}

/// The emulator for one connection, with its state over it while it connects or after it ends.
struct TerminalSurface: View {
    let connection: TerminalConnection
    let client: Client
    let sessionId: SessionId?

    var body: some View {
        EmulatorView(connection: connection, client: client, sessionId: sessionId)
            .padding(6)
            .overlay {
                switch connection.state {
                case .connecting:
                    ProgressView().tint(Theme.secondary)
                case .attached:
                    EmptyView()
                case .exited(let code):
                    notice(accountExitMessage(code))
                case .failed(let reason):
                    notice(reason)
                }
            }
    }

    private func accountExitMessage(_ code: Int32?) -> String {
        if connection.account != nil {
            return code == 0 ? "Login finished. Check the account list for confirmation."
                : "Login did not complete. Check the terminal output."
        }
        return code.map { "The shell exited with \($0)." } ?? "The shell exited."
    }

    private func notice(_ text: String) -> some View {
        Text(text)
            .font(.footnote.weight(.medium))
            .foregroundStyle(Theme.text)
            .padding(.horizontal, 14)
            .padding(.vertical, 8)
            .background(Theme.raised, in: .capsule)
            .frame(maxHeight: .infinity, alignment: .bottom)
            .padding(.bottom, 16)
    }
}

/// SwiftTerm's emulator, wired to a connection: it connects once it knows its size.
@MainActor
final class EmulatorDelegate: NSObject, @preconcurrency TerminalViewDelegate {
    unowned let connection: TerminalConnection
    let client: Client
    let sessionId: SessionId?

    init(connection: TerminalConnection, client: Client, sessionId: SessionId?) {
        self.connection = connection
        self.client = client
        self.sessionId = sessionId
    }

    func sizeChanged(source: SwiftTerm.TerminalView, newCols: Int, newRows: Int) {
        if connection.state == .connecting {
            connection.connect(client: client, sessionId: sessionId, cols: newCols, rows: newRows)
        } else {
            connection.resize(cols: newCols, rows: newRows)
        }
    }

    func send(source: SwiftTerm.TerminalView, data: ArraySlice<UInt8>) {
        connection.input(data)
    }

    func setTerminalTitle(source: SwiftTerm.TerminalView, title: String) {}
    func hostCurrentDirectoryUpdate(source: SwiftTerm.TerminalView, directory: String?) {}
    func scrolled(source: SwiftTerm.TerminalView, position: Double) {}
    func bell(source: SwiftTerm.TerminalView) {}
    func rangeChanged(source: SwiftTerm.TerminalView, startY: Int, endY: Int) {}

    func requestOpenLink(source: SwiftTerm.TerminalView, link: String, params: [String: String]) {
        guard let url = URL(string: link) else { return }
        #if os(macOS)
        NSWorkspace.shared.open(url)
        #else
        UIApplication.shared.open(url)
        #endif
    }

    func clipboardCopy(source: SwiftTerm.TerminalView, content: Data) {
        if let text = String(data: content, encoding: .utf8) {
            Clipboard.string = text
        }
    }
}

#if os(macOS)
private struct EmulatorView: NSViewRepresentable {
    let connection: TerminalConnection
    let client: Client
    let sessionId: SessionId?

    func makeNSView(context: Context) -> NSView {
        // A plain container, so the kept emulator can move into each new one.
        let container = NSView()
        let view = connection.view(client: client, sessionId: sessionId)
        view.removeFromSuperview()
        view.translatesAutoresizingMaskIntoConstraints = true
        view.autoresizingMask = [.width, .height]
        view.frame = container.bounds
        container.addSubview(view)
        DispatchQueue.main.async { view.window?.makeFirstResponder(view) }
        return container
    }

    func updateNSView(_ container: NSView, context: Context) {}
}
#else
private struct EmulatorView: UIViewRepresentable {
    let connection: TerminalConnection
    let client: Client
    let sessionId: SessionId?

    func makeUIView(context: Context) -> UIView {
        let container = UIView()
        let view = connection.view(client: client, sessionId: sessionId)
        view.removeFromSuperview()
        view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        view.frame = container.bounds
        container.addSubview(view)
        DispatchQueue.main.async { _ = view.becomeFirstResponder() }
        return container
    }

    func updateUIView(_ container: UIView, context: Context) {}
}
#endif

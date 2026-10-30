import Darwin
import Foundation
import OSLog

/// The app's launch, timed from the process starting: signposts Instruments shows under
/// `sh.herder` › `launch`, and once the Board shows content, one line that says when each step
/// came. With `HERDER_LAUNCH_REPORT` set, that line also goes to standard output, for
/// apple/scripts/launch-ios.sh to read through `devicectl --console`.
@MainActor
enum Launch {
    enum Step: String, CaseIterable {
        /// The profile is open and the fleet holds what the client had.
        case profile
        /// The fleet's first frame is on screen, under the splash.
        case frame
        /// The Board lists sessions.
        case content
        /// The splash has gone: the Board is what you see.
        case shown
    }

    private static let signposter = OSSignposter(subsystem: "sh.herder", category: "launch")
    private static let logger = Logger(subsystem: "sh.herder", category: "launch")
    private static var reached: [Step: Duration] = [:]
    private static let report = ProcessInfo.processInfo.environment["HERDER_LAUNCH_REPORT"] != nil

    /// Marks `step` reached, the first time only.
    static func mark(_ step: Step) {
        guard reached[step] == nil, let since = sinceProcessStart() else { return }
        reached[step] = since
        signposter.emitEvent("step", "\(step.rawValue, privacy: .public)")
        guard Step.allCases.allSatisfy({ reached[$0] != nil }) else { return }
        let line = "herder-launch " + Step.allCases
            .map { "\($0.rawValue)=\(milliseconds(reached[$0] ?? .zero))" }
            .joined(separator: " ")
        logger.notice("\(line, privacy: .public)")
        if report {
            print(line)
            fflush(stdout)
        }
    }

    private static func milliseconds(_ duration: Duration) -> Int {
        Int(duration / .milliseconds(1))
    }

    /// How long ago the kernel started this process.
    private static func sinceProcessStart() -> Duration? {
        var info = kinfo_proc()
        var size = MemoryLayout<kinfo_proc>.stride
        var name: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid()]
        guard sysctl(&name, 4, &info, &size, nil, 0) == 0 else { return nil }
        let start = info.kp_proc.p_un.__p_starttime
        var now = timeval()
        gettimeofday(&now, nil)
        let micros = (Int64(now.tv_sec) - Int64(start.tv_sec)) * 1_000_000
            + Int64(now.tv_usec) - Int64(start.tv_usec)
        return .microseconds(micros)
    }
}

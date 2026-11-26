import Darwin
import Foundation
import OSLog

/// The main thread's hangs, in the log under `sh.herder` › `hang`: every turn of the main run
/// loop, from waking for an event to waiting for the next, that took `Moment.shortest` or
/// longer, as one line, `herder-hang ms=<how long it took> cpu=<how long of it the thread ran>`.
/// Nothing the app shows or takes moves during such a turn. One far longer than its CPU time
/// waited on something; so does one iOS froze in the background, which is no hang.
///
/// Read them from the app that hung (see apple/README.md); apple/scripts/perf.sh reads them to
/// say how the app launched, resumed and scrolled.
enum Hangs {
    /// A moment on the main thread, in nanoseconds: the time, and the CPU time the thread has
    /// used.
    struct Moment {
        /// The shortest turn the log tells of: a tenth of a second, a delay a person notices.
        /// The measurements (apple/PerfTests) set `HERDER_HANG_MS` to the milliseconds of a
        /// frame, to count every turn that cost one.
        static let shortest: UInt64 =
            (ProcessInfo.processInfo.environment["HERDER_HANG_MS"].flatMap { UInt64($0) } ?? 100) * 1_000_000

        var time: UInt64
        var cpu: UInt64

        static func now() -> Moment {
            Moment(
                time: clock_gettime_nsec_np(CLOCK_UPTIME_RAW),
                cpu: clock_gettime_nsec_np(CLOCK_THREAD_CPUTIME_ID))
        }

        /// The log's line for a turn that began at this moment and ended at `end`, when it was
        /// long enough to tell of.
        func hang(until end: Moment) -> String? {
            guard end.time >= time, end.time - time >= Moment.shortest else { return nil }
            let ran = end.cpu >= cpu ? end.cpu - cpu : 0
            return "herder-hang ms=\((end.time - time) / 1_000_000) cpu=\(ran / 1_000_000)"
        }
    }

    private static let logger = Logger(subsystem: "sh.herder", category: "hang")
    @MainActor private static var observer: CFRunLoopObserver?
    /// When the turn under way began; nil while the loop waits.
    @MainActor private static var woke: Moment?

    /// Starts logging the hangs, the first time only. The turn under way counts from now: at
    /// launch, the rest of the launch.
    @MainActor
    static func watch() {
        guard observer == nil else { return }
        woke = Moment.now()
        let activities: CFRunLoopActivity = [.afterWaiting, .beforeWaiting]
        let watching = CFRunLoopObserverCreateWithHandler(
            kCFAllocatorDefault, activities.rawValue, true, 0
        ) { _, activity in
            // The main run loop calls its observers on the main thread.
            MainActor.assumeIsolated { Hangs.turned(activity) }
        }
        // Every common mode: the loop runs in another while a list scrolls or a menu is open.
        CFRunLoopAddObserver(CFRunLoopGetMain(), watching, CFRunLoopMode.commonModes)
        observer = watching
    }

    /// The loop woke, or is about to wait: the end of a turn, logged when it was a hang.
    @MainActor
    private static func turned(_ activity: CFRunLoopActivity) {
        if activity == .afterWaiting {
            woke = Moment.now()
        } else if let began = woke {
            woke = nil
            if let line = began.hang(until: Moment.now()) {
                logger.notice("\(line, privacy: .public)")
            }
        }
    }
}

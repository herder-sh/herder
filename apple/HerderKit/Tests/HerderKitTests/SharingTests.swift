import CoreImage
import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct SharingTests {
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func aSharedLinkPairsAnotherProfileWithEveryMachine() async throws {
        let first = try FakeDaemon(name: "fake-host-1")
        let second = try FakeDaemon(name: "fake-host-2")
        var gone: FakeDaemon? = try FakeDaemon(name: "fake-host-3")
        guard case .opened(let mac) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        let macFollowing = Task { await mac.follow() }
        defer { macFollowing.cancel() }
        for daemon in [first, second, gone!] {
            try await mac.pair(daemon)
        }
        #expect(await eventually { mac.machines.allSatisfy { $0.connection == .connected } })
        // A machine that went away is left out, and says why.
        gone = nil
        #expect(await eventually { mac.machines.last?.connection != .connected })

        let shared = try await mac.share()
        #expect(shared.shared == ["fake-host-1", "fake-host-2"])
        #expect(shared.skipped.map(\.hostId) == ["fake-host-3"])
        #expect(shared.skipped.first?.error.isEmpty == false)
        #expect(shared.link.machines.count == 2)
        #expect(Timestamp.date(shared.expiresAt).map { $0 > .now } == true)
        let link = pairingLinkToString(link: shared.link)
        // What the Mac shows, the phone finds: in the QR code and in pasted text.
        #expect(pairingLink(in: link) == link)

        guard case .opened(let phone) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a second profile")
            return
        }
        let phoneFollowing = Task { await phone.follow() }
        defer { phoneFollowing.cancel() }
        let results = try await phone.pair(link: link)
        let paired = results.compactMap { result -> String? in
            guard case .paired(let machine) = result else { return nil }
            return machine.name
        }
        #expect(paired == ["fake-host-1", "fake-host-2"])
        #expect(phone.machines.map(\.hostId) == ["fake-host-1", "fake-host-2"])
        #expect(await eventually { phone.machines.allSatisfy { $0.connection == .connected } })
        #expect(await eventually { phone.machines.allSatisfy { $0.role == .owner } })

        // The codes work once: a third profile gets each machine's failure.
        guard case .opened(let again) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a third profile")
            return
        }
        let failed = try await again.pair(link: link)
        #expect(failed.count == 2)
        #expect(failed.allSatisfy { if case .failed = $0 { true } else { false } })
        #expect(again.machines.isEmpty)
    }

    @Test func theQRCodeHoldsTheLink() throws {
        let link = "herder://pair?host=127.0.0.1%3A7420&fp=" + String(repeating: "ab", count: 32) + "&code=ABCDE-FGHJK"
            + "&host=10.0.0.9%3A7447&fp=" + String(repeating: "cd", count: 32) + "&code=MNPQR-STVWX"
        let code = try #require(QRCode.image(link))
        // Scaled up, with a quiet zone, as a camera would see it.
        let image = CIImage(cgImage: code)
            .transformed(by: CGAffineTransform(scaleX: 8, y: 8))
            .composited(over: CIImage(color: .white).cropped(to: CGRect(x: -64, y: -64,
                width: CGFloat(code.width) * 8 + 128, height: CGFloat(code.height) * 8 + 128)))
        let detector = try #require(CIDetector(ofType: CIDetectorTypeQRCode, context: nil))
        let found = detector.features(in: image).compactMap { ($0 as? CIQRCodeFeature)?.messageString }
        #expect(found == [link])
    }

    @Test func theExpiryCountsDown() {
        let now = Date(timeIntervalSince1970: 1_000)
        #expect(expiry(now.addingTimeInterval(299.2), now: now) == "Expires in 5:00")
        #expect(expiry(now.addingTimeInterval(61), now: now) == "Expires in 1:01")
        #expect(expiry(now.addingTimeInterval(9), now: now) == "Expires in 0:09")
        #expect(expiry(now, now: now) == "Expired: make a new link")
        #expect(expiry(nil, now: now) == "")
    }
}

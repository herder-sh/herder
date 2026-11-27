import Herder
@testable import HerderKit
import Testing

struct AddressHeadersTests {
    @Test func headersAreTypedOnePerLineAsNameColonValue() {
        let typed = """
        CF-Access-Client-Id: abc.access

          CF-Access-Client-Secret:  s3cret:with:colons
        """
        #expect(AddressHeaders.parse(typed)
            == .success(["CF-Access-Client-Id": "abc.access", "CF-Access-Client-Secret": "s3cret:with:colons"]))
        #expect(AddressHeaders.parse("") == .success([:]))
        #expect(AddressHeaders.parse("no colon here") == .failure(AddressHeadersError(message: "“no colon here” is not Name: value")))
        #expect(AddressHeaders.parse(": value") == .failure(AddressHeadersError(message: "“: value” names no header")))
    }

    @Test func onlyATunnelTakesHeadersAndItsRowNamesThem() {
        #expect(AddressHeaders.takesHeaders("wss://hub.example.com"))
        #expect(AddressHeaders.takesHeaders("ws://10.0.0.4:7448"))
        #expect(!AddressHeaders.takesHeaders("hub.example.com:7447"))
        #expect(!AddressHeaders.takesHeaders("hub://h1"))
        #expect(AddressHeaders.summary([]) == nil)
        #expect(AddressHeaders.summary(["cf-access-client-secret", "cf-access-client-id"])
            == "Sends cf-access-client-id, cf-access-client-secret")
    }
}

struct FrontDoorTests {
    @Test func aLoginProxyAsksToSignInThenAgainOnceItsTokenIsRefused() {
        let address = "wss://hub.example.com"
        #expect(FrontDoorPrompt.action(FrontDoor(address: address, expiresAt: nil, signIn: true)) == "Sign In…")
        #expect(FrontDoorPrompt.action(FrontDoor(address: address, expiresAt: "2096-10-02T07:06:40Z", signIn: false)) == nil)
        #expect(FrontDoorPrompt.action(FrontDoor(address: address, expiresAt: "2096-10-02T07:06:40Z", signIn: true)) == "Sign In Again…")
    }
}

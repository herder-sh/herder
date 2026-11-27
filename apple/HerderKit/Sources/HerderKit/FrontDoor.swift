import Herder

/// What a tunnel address's row says of the login proxy in front of it, such as Cloudflare
/// Access with an identity provider: a front door the person signs in to with a browser
/// (`Machine.frontDoors`, P28.7).
enum FrontDoorPrompt {
    /// The front door of `address`, one of `machine`'s addresses, if it has one.
    static func door(of address: String, in machine: Machine) -> FrontDoor? {
        machine.frontDoors.first { $0.address == address }
    }

    /// The row's button: "Sign In…" before the first sign-in, "Sign In Again…" once the proxy
    /// refused the token or it expired; `nil` while signed in.
    static func action(_ door: FrontDoor) -> String? {
        guard door.signIn else { return nil }
        return door.expiresAt == nil ? "Sign In…" : "Sign In Again…"
    }
}

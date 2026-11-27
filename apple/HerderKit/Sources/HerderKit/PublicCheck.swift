import Foundation
import Herder

/// What is known of whether a hub's public address reaches it from outside its network. Its
/// shape says only that it could: a name may resolve to the LAN alone, or to nothing.
enum PublicCheck: Hashable {
    /// Not seen to work from outside yet: not looked up, or it resolves to a public address
    /// this device is not connected through.
    case unchecked
    /// This device is connected to the hub through it now.
    case connected
    /// Its name resolves only to these local-network addresses, so only that network reaches
    /// the hub there.
    case localOnly([String])
    /// Its name resolves to nothing.
    case unresolved

    /// What is known of `address`, one of `hub`'s, given what each name looked up resolved
    /// to (`HostLookup`); a name not looked up yet is unchecked.
    init(address: String, of hub: Machine, resolved: [String: [String]]) {
        if hub.connection == .connected && hub.address == address {
            self = .connected
            return
        }
        let host = AddressScope.host(of: address)
        guard HostLookup.needsLookup(host), let found = resolved[host] else {
            self = .unchecked
            return
        }
        if found.isEmpty {
            self = .unresolved
        } else if found.allSatisfy({ AddressScope(HostLookup.withPort($0)) == .network }) {
            self = .localOnly(found)
        } else {
            self = .unchecked
        }
    }
}

/// Looks host names up as this device's resolver does, off the main actor.
enum HostLookup {
    /// Whether `host` is a name, not an IP address.
    static func needsLookup(_ host: String) -> Bool {
        if host.contains(":") { return false }
        let parts = host.split(separator: ".", omittingEmptySubsequences: false)
        return !(parts.count == 4 && parts.allSatisfy { UInt8($0) != nil })
    }

    /// `ip` as `host:port`, as `AddressScope` reads an address.
    static func withPort(_ ip: String) -> String {
        ip.contains(":") ? "[\(ip)]:0" : "\(ip):0"
    }

    /// The addresses `host` resolves to, sorted; none when it does not resolve.
    static func addresses(of host: String) async -> [String] {
        await Task.detached {
            var hints = addrinfo()
            hints.ai_family = AF_UNSPEC
            hints.ai_socktype = SOCK_STREAM
            var list: UnsafeMutablePointer<addrinfo>?
            guard getaddrinfo(host, nil, &hints, &list) == 0, let first = list else { return [] }
            defer { freeaddrinfo(first) }
            var found = Set<String>()
            var next: UnsafeMutablePointer<addrinfo>? = first
            while let info = next {
                var name = [CChar](repeating: 0, count: Int(NI_MAXHOST))
                let named = getnameinfo(info.pointee.ai_addr, info.pointee.ai_addrlen, &name,
                                        socklen_t(name.count), nil, 0, NI_NUMERICHOST)
                if named == 0 {
                    found.insert(name.withUnsafeBufferPointer { String(cString: $0.baseAddress!) })
                }
                next = info.pointee.ai_next
            }
            return found.sorted()
        }.value
    }
}

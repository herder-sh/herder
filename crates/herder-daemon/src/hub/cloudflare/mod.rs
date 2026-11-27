//! What the hub sets up in its owner's Cloudflare account through Cloudflare's API
//! (`docs/remote-access.md`): the managed Cloudflare Tunnel to the hub ([`ManagedTunnel`]),
//! which runs a pinned `cloudflared` ([`cloudflared`]), and the Durable Object relay
//! ([`ManagedRelay`]).

mod api;
pub mod cloudflared;
#[cfg(test)]
pub(crate) mod fake;
pub(crate) mod relay;
pub(crate) mod tunnel;
mod workers;

pub use api::BASE as API;
pub use relay::{ManagedRelay, WantedRelay};
pub use tunnel::{ManagedTunnel, Wanted, hostname};

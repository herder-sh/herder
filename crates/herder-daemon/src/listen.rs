//! The TCP listeners the daemon and the vault accept connections on: one per configured
//! address, served by one accept loop.

use std::future::poll_fn;
use std::io;
use std::net::SocketAddr;
use std::task::Poll;

use anyhow::{Context, Result};
use tokio::net::{TcpListener, TcpStream};

/// Binds every one of `addresses`, failing on the first that cannot be bound.
pub(crate) async fn bind(addresses: &[SocketAddr]) -> Result<Vec<TcpListener>> {
    let mut listeners = Vec::with_capacity(addresses.len());
    for address in addresses {
        let listener = TcpListener::bind(address)
            .await
            .with_context(|| format!("cannot listen on {address}"))?;
        listeners.push(listener);
    }
    Ok(listeners)
}

/// The addresses `listeners` are bound to, in their order.
pub(crate) fn local_addrs(listeners: &[TcpListener]) -> io::Result<Vec<SocketAddr>> {
    listeners.iter().map(TcpListener::local_addr).collect()
}

/// Accepts listeners' connections in turn, so a busy one cannot starve the others.
#[derive(Default)]
pub(crate) struct Acceptor {
    next: usize,
}

impl Acceptor {
    /// The next connection on any of `listeners`; cancel-safe, as `TcpListener::accept` is.
    /// Never ready when there are none.
    pub(crate) async fn accept(
        &mut self,
        listeners: &[TcpListener],
    ) -> io::Result<(TcpStream, SocketAddr)> {
        poll_fn(|cx| {
            let count = listeners.len();
            for offset in 0..count {
                let index = (self.next + offset) % count;
                if let Poll::Ready(accepted) = listeners[index].poll_accept(cx) {
                    self.next = (index + 1) % count;
                    return Poll::Ready(accepted);
                }
            }
            Poll::Pending
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connections_on_every_listener_are_accepted() {
        let listeners = bind(&["127.0.0.1:0".parse().unwrap(); 2]).await.unwrap();
        let addrs = local_addrs(&listeners).unwrap();
        assert_ne!(addrs[0], addrs[1]);
        let mut acceptor = Acceptor::default();
        for addr in [addrs[1], addrs[0], addrs[1]] {
            let client = TcpStream::connect(addr).await.unwrap();
            let (_, peer) = acceptor.accept(&listeners).await.unwrap();
            assert_eq!(peer, client.local_addr().unwrap());
        }
    }

    #[tokio::test]
    async fn an_address_that_cannot_be_bound_is_named() {
        let taken = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let taken = taken.local_addr().unwrap();
        let err = bind(&["127.0.0.1:0".parse().unwrap(), taken])
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").contains(&format!("cannot listen on {taken}")),
            "{err:#}"
        );
    }
}

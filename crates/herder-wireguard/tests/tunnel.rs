//! A tunnel to an endpoint, both in this process, over loopback UDP.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use herder_wireguard::{Endpoint, IpCidr, Key, Peer, Tunnel, TunnelConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;

const HUB: Ipv4Addr = Ipv4Addr::new(10, 247, 0, 1);
const DEVICE: Ipv4Addr = Ipv4Addr::new(10, 247, 0, 2);

async fn endpoint() -> Endpoint {
    Endpoint::bind(
        "127.0.0.1:0".parse().unwrap(),
        Key::generate().unwrap(),
        IpCidr::new(HUB.into(), 16),
        SocketAddr::new(HUB.into(), 7447),
    )
    .await
    .unwrap()
}

async fn tunnel(endpoint: &Endpoint, private_key: Key) -> Tunnel {
    Tunnel::open(TunnelConfig {
        private_key,
        peer_public_key: endpoint.public_key(),
        preshared_key: None,
        endpoint: endpoint.local_addr().to_string(),
        addresses: vec![(IpAddr::V4(DEVICE), 32)],
    })
    .await
    .unwrap()
}

/// Echoes every connection the endpoint takes.
fn echo(endpoint: std::sync::Arc<Endpoint>) {
    tokio::spawn(async move {
        while let Some(accepted) = endpoint.accept().await {
            assert_eq!(accepted.from.ip(), IpAddr::V4(DEVICE));
            tokio::spawn(async move {
                let (mut read, mut write) = tokio::io::split(accepted.stream);
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_peer_connects_through_the_endpoint_and_bytes_go_both_ways() {
    let endpoint = std::sync::Arc::new(endpoint().await);
    let key = Key::generate().unwrap();
    endpoint.set_peers(vec![Peer {
        public_key: key.public(),
        address: DEVICE.into(),
    }]);
    echo(std::sync::Arc::clone(&endpoint));
    let tunnel = tunnel(&endpoint, key).await;
    let mut stream = tunnel
        .connect(SocketAddr::new(HUB.into(), 7447))
        .await
        .unwrap();
    assert_eq!(tunnel.connections(), 1);
    // More than a socket buffers, so flow control is exercised.
    let sent: Vec<u8> = (0..1_000_000u32).map(|n| (n % 251) as u8).collect();
    let writing = {
        let sent = sent.clone();
        let (mut read, mut write) = tokio::io::split(stream);
        let writer = tokio::spawn(async move {
            write.write_all(&sent).await.unwrap();
            write.shutdown().await.unwrap();
            write
        });
        let mut echoed = Vec::new();
        read.read_to_end(&mut echoed).await.unwrap();
        stream = read.unsplit(writer.await.unwrap());
        echoed
    };
    assert_eq!(writing, sent);
    drop(stream);
    tokio::time::timeout(Duration::from_secs(5), async {
        while tunnel.connections() != 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    // Nothing but the address the endpoint listens on answers inside the tunnel.
    let refused = tunnel.connect(SocketAddr::new(HUB.into(), 22)).await;
    assert!(refused.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_the_endpoint_does_not_have_gets_no_answer() {
    let endpoint = std::sync::Arc::new(endpoint().await);
    endpoint.set_peers(vec![Peer {
        public_key: Key::generate().unwrap().public(),
        address: DEVICE.into(),
    }]);
    echo(std::sync::Arc::clone(&endpoint));

    // A stranger's tunnel gets nowhere.
    let stranger = tunnel(&endpoint, Key::generate().unwrap()).await;
    let error = stranger
        .connect(SocketAddr::new(HUB.into(), 7447))
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);

    // Nor does anything that is not a handshake with the endpoint's key: not one byte back.
    let probe = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    probe.connect(endpoint.local_addr()).await.unwrap();
    for junk in [&b"hello"[..], &[1u8; 148][..], &[4u8; 64][..]] {
        probe.send(junk).await.unwrap();
    }
    let mut buf = [0u8; 256];
    let answer = tokio::time::timeout(Duration::from_secs(1), probe.recv(&mut buf)).await;
    assert!(answer.is_err(), "the endpoint answered a stranger");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_peer_taken_off_is_cut_off_at_once() {
    let endpoint = std::sync::Arc::new(endpoint().await);
    let key = Key::generate().unwrap();
    let peer = Peer {
        public_key: key.public(),
        address: DEVICE.into(),
    };
    endpoint.set_peers(vec![peer]);
    echo(std::sync::Arc::clone(&endpoint));
    let tunnel = tunnel(&endpoint, key).await;
    let mut stream = tunnel
        .connect(SocketAddr::new(HUB.into(), 7447))
        .await
        .unwrap();
    stream.write_all(b"ping").await.unwrap();
    let mut pong = [0u8; 4];
    stream.read_exact(&mut pong).await.unwrap();

    endpoint.set_peers(Vec::new());
    // Nothing more gets through its connection: the endpoint resets it, and a reset lost on
    // the way leaves it carrying nothing until the client gives up on it.
    let _ = stream.write_all(b"more").await;
    let answered = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut pong)).await;
    assert!(
        matches!(answered, Err(_) | Ok(Ok(0)) | Ok(Err(_))),
        "{answered:?}"
    );
    assert!(
        tunnel
            .connect(SocketAddr::new(HUB.into(), 7447))
            .await
            .is_err()
    );
}

#[test]
fn keys_read_and_write_as_wireguard_does() {
    let key: Key = "yAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk="
        .parse()
        .unwrap();
    assert_eq!(
        key.to_base64(),
        "yAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk="
    );
    // As `wg pubkey` derives it.
    assert_eq!(
        key.public().to_base64(),
        "HIgo9xNzJMWLKASShiTqIybxZ0U3wGLiUeJ1PKf8ykw="
    );
    assert!("short".parse::<Key>().is_err());
    assert_eq!(format!("{key:?}"), "Key(..)");
}

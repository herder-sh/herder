//! Compressed frames, which a peer may send once the other said `deflate` in its hello.
//!
//! A daemon's binary WebSocket frame carries one or more messages: their JSON texts joined by
//! `\n`, compressed as raw DEFLATE (RFC 1951). A client's carries one. Each frame is
//! compressed by itself, with nothing carried over from the frames before it. JSON text holds
//! no raw newline, so the texts split apart again at each `\n`. Text frames carry one message
//! uncompressed, as before: a small message gains nothing from compression.

use std::io::{self, Read, Write};

use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;

/// The binary frame that carries `texts`, each a message's JSON.
pub fn deflate<'a>(texts: impl IntoIterator<Item = &'a str>) -> Vec<u8> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    // Writing into a `Vec` cannot fail, so neither can the encoder that writes into one.
    for (index, text) in texts.into_iter().enumerate() {
        if index > 0 {
            encoder
                .write_all(b"\n")
                .expect("writing into a Vec cannot fail");
        }
        encoder
            .write_all(text.as_bytes())
            .expect("writing into a Vec cannot fail");
    }
    encoder.finish().expect("writing into a Vec cannot fail")
}

/// The JSON texts in a binary frame, joined by `\n` as they were sent; split them at each
/// `\n`. Fails on a frame that is not DEFLATE or UTF-8, or that inflates past `limit` bytes.
pub fn inflate(frame: &[u8], limit: usize) -> io::Result<String> {
    let mut text = String::new();
    let cap = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    DeflateDecoder::new(frame)
        .take(cap)
        .read_to_string(&mut text)?;
    if text.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a compressed frame holds over {limit} bytes"),
        ));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_come_back_as_they_went() {
        let texts = [
            r#"{"type":"synced","token":"a"}"#,
            r#"{"type":"x","s":"a\nb"}"#,
        ];
        let frame = deflate(texts);
        let text = inflate(&frame, 1024).unwrap();
        assert_eq!(text.split('\n').collect::<Vec<_>>(), texts);
    }

    #[test]
    fn a_frame_that_inflates_past_the_limit_is_refused() {
        let big = "x".repeat(10_000);
        let frame = deflate([big.as_str()]);
        assert!(frame.len() < 100);
        assert!(inflate(&frame, 9_999).is_err());
        assert_eq!(inflate(&frame, 10_000).unwrap(), big);
    }

    #[test]
    fn a_frame_that_is_not_deflate_is_refused() {
        assert!(inflate(b"{\"type\":\"synced\"}", 1024).is_err());
    }
}

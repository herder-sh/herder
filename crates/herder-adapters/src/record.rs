//! Recording a real CLI's stdio into a [`Fixture`](crate::fixture::Fixture), behind
//! `herder dev record`.
//!
//! The recorder sits between a terminal and the child: each line typed is sent to the child,
//! each line the child prints is shown, and both are written to the fixture as they happen.
//! It sees only the child's stdio, never its config dir or credential files, and redacts what
//! it writes to the fixture, never what it passes through.

use std::io;

use regex::Regex;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::Command;

use crate::fixture::{Header, Record};
use crate::transport::{Exit, Transport};

/// What redacted text is replaced with.
pub const REDACTED: &str = "[REDACTED]";

/// Secrets redacted from every recording: bearer tokens, API keys and tokens in the common
/// vendor formats, JWTs, and email addresses.
const DEFAULT_PATTERNS: &[&str] = &[
    r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]+",
    r"\bsk-[A-Za-z0-9_-]{16,}",
    r"\bgh[pousr]_[A-Za-z0-9]{20,}",
    r"\bgithub_pat_[A-Za-z0-9_]{20,}",
    r"\bxox[abprs]-[A-Za-z0-9-]{10,}",
    r"\bAKIA[0-9A-Z]{16}\b",
    r"\bAIza[0-9A-Za-z_-]{35}",
    r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+",
    r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}",
];

/// Replaces secrets in recorded lines with [`REDACTED`].
#[derive(Clone, Debug)]
pub struct Redactor {
    patterns: Vec<Regex>,
}

impl Redactor {
    /// The default patterns plus `extra` regexes.
    pub fn new(extra: &[String]) -> Result<Self, regex::Error> {
        let patterns = DEFAULT_PATTERNS
            .iter()
            .copied()
            .chain(extra.iter().map(String::as_str))
            .map(Regex::new)
            .collect::<Result<_, _>>()?;
        Ok(Self { patterns })
    }

    /// `line` with every match of every pattern replaced.
    pub fn redact(&self, line: &str) -> String {
        self.patterns.iter().fold(line.to_owned(), |line, pattern| {
            pattern.replace_all(&line, REDACTED).into_owned()
        })
    }
}

/// Runs `command`, proxying lines from `terminal_in` to it and its output to `terminal_out`,
/// while writing the fixture to `fixture`; must be called inside a tokio runtime.
///
/// Records stop at the child's end of output; then its exit is recorded and returned. Every
/// record is flushed as it is written, so a recording cut short keeps what came before.
pub async fn record(
    command: Command,
    header: &Header,
    redactor: &Redactor,
    terminal_in: impl AsyncBufRead + Unpin,
    mut terminal_out: impl AsyncWrite + Unpin,
    mut fixture: impl AsyncWrite + Unpin,
) -> io::Result<Option<i32>> {
    let Transport {
        stdin,
        mut stdout,
        exit,
    } = Transport::spawn(command)?;
    let mut stdin = Some(stdin);
    let mut terminal_in = terminal_in.lines();
    write_line(&mut fixture, header.to_line()).await?;

    loop {
        tokio::select! {
            line = stdout.recv() => {
                let Some(line) = line else { break };
                write_line(&mut fixture, Record::Out(redactor.redact(&line)).to_line()).await?;
                write_line(&mut terminal_out, line).await?;
            }
            line = terminal_in.next_line(), if stdin.is_some() => match line? {
                Some(line) => {
                    let recorded = Record::In(redactor.redact(&line)).to_line();
                    let sent = match &stdin {
                        Some(stdin) => stdin.send(line).await.is_ok(),
                        None => false,
                    };
                    if sent {
                        write_line(&mut fixture, recorded).await?;
                    } else {
                        // The child stopped reading; what it never got is not recorded.
                        stdin = None;
                    }
                }
                None => {
                    stdin = None;
                    write_line(&mut fixture, Record::InEof.to_line()).await?;
                }
            },
        }
    }

    drop(stdin);
    let code = match exit.await {
        Ok(Exit::Code(code)) => code,
        Ok(Exit::Failed(message)) => return Err(io::Error::other(message)),
        Err(_) => return Err(io::Error::other("the child's exit was never reported")),
    };
    write_line(&mut fixture, Record::Exit(code).to_line()).await?;
    Ok(code)
}

async fn write_line(writer: &mut (impl AsyncWrite + Unpin), line: String) -> io::Result<()> {
    let mut bytes = line.into_bytes();
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_patterns_redact_secrets() {
        let redactor = Redactor::new(&[]).unwrap();
        let cases = [
            (
                r#"{"headers":{"authorization":"Bearer abc.DEF-123_xyz"}}"#,
                r#"{"headers":{"authorization":"[REDACTED]"}}"#,
            ),
            (
                "key sk-ant-api03-AbCdEfGhIjKlMnOp1234 end",
                "key [REDACTED] end",
            ),
            ("ghp_abcdefghijklmnopqrstuvwxyz0123", "[REDACTED]"),
            ("AKIAIOSFODNN7EXAMPLE", "[REDACTED]"),
            ("jwt eyJhbGciOi.eyJzdWIiOi.c2lnbmF0dXJl", "jwt [REDACTED]"),
            (
                r#"{"email":"jane.doe+x@example.co.uk"}"#,
                r#"{"email":"[REDACTED]"}"#,
            ),
            (
                r#"{"input_tokens":12,"session_id":"0199a1b2"}"#,
                r#"{"input_tokens":12,"session_id":"0199a1b2"}"#,
            ),
        ];
        for (line, redacted) in cases {
            assert_eq!(redactor.redact(line), redacted, "{line}");
        }
    }

    #[test]
    fn extra_patterns_add_to_the_defaults() {
        let redactor = Redactor::new(&["acct-[0-9]+".into()]).unwrap();
        assert_eq!(
            redactor.redact("acct-42 owned by a@b.io"),
            "[REDACTED] owned by [REDACTED]"
        );
    }

    #[test]
    fn invalid_pattern_is_an_error() {
        assert!(Redactor::new(&["(".into()]).is_err());
    }
}

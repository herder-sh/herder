//! Recorded CLI stdio, for replaying a vendor CLI in tests.
//!
//! A fixture is a JSON Lines file, one record per line, in the order things happened:
//!
//! - `{"header": {"provider": "claude", "cli_version": "2.1.3", "recorded_at": "...",
//!   "ignore_keys": ["session_id"]}}`: optional, first line only. Every field but `provider`
//!   may be left out.
//! - `{"dir": "in", "line": "..."}`: herder wrote this line to the child's stdin.
//! - `{"dir": "out", "line": "..."}`: the child wrote this line to its stdout.
//! - `{"dir": "in", "eof": true}`: herder closed the child's stdin.
//! - `{"exit": <code>}`: the child exited, `null` when a signal killed it. Always the last
//!   record.
//!
//! Blank lines and lines starting with `#` are skipped. `herder dev record` writes fixtures;
//! [`Transport::replay`](crate::transport::Transport::replay) plays them.
//!
//! # Matching
//!
//! In replay an `in` record is an expectation. Without `ignore_keys` the line sent must equal
//! the recorded one byte for byte. With them, both lines are parsed as JSON and compared with
//! every object key named in `ignore_keys` removed at any depth, so per-run values such as ids
//! and timestamps do not break the match.

use std::fmt;
use std::path::Path;

use herder_protocol::Timestamp;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use tokio::sync::{mpsc, oneshot};

use crate::transport::Exit;

/// A parsed fixture.
#[derive(Clone, Debug, PartialEq)]
pub struct Fixture {
    /// Where it came from, for error messages.
    source: String,
    /// The header, when the fixture has one.
    pub header: Option<Header>,
    /// Records, each with its 1-based line number.
    records: Vec<(usize, Record)>,
}

/// What a fixture was recorded from, and how to match it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    /// Provider the CLI belongs to, as in the fixture's directory name.
    pub provider: String,
    /// Version of the CLI that was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_version: Option<String>,
    /// When it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<Timestamp>,
    /// Object keys left out when matching `in` lines as JSON.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_keys: Vec<String>,
}

/// One thing that happened on the child's stdio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    /// herder wrote this line to stdin.
    In(String),
    /// The child wrote this line to stdout.
    Out(String),
    /// herder closed stdin.
    InEof,
    /// The child exited with this code; `None` when a signal killed it.
    Exit(Option<i32>),
}

/// A fixture that cannot be read or parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixtureError(String);

impl fmt::Display for FixtureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FixtureError {}

/// A record line as written, before its fields are checked against each other.
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    header: Option<Header>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dir: Option<Dir>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    line: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    eof: Option<bool>,
    /// `Some(None)` is `"exit": null`; the outer `None` is no `exit` key at all.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    exit: Option<Option<i32>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Dir {
    In,
    Out,
}

/// Deserializes a key that is present, even as `null`, into `Some`.
fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Option<i32>>, D::Error> {
    Option::<i32>::deserialize(deserializer).map(Some)
}

/// Serializes a record line. Serializing strings, bools, integers and timestamps into JSON
/// cannot fail, so the error branch is unreachable.
fn to_line(raw: &RawRecord) -> String {
    serde_json::to_string(raw).unwrap_or_default()
}

impl Header {
    /// The header as its fixture line, without the newline.
    pub fn to_line(&self) -> String {
        to_line(&RawRecord {
            header: Some(self.clone()),
            ..RawRecord::default()
        })
    }
}

impl Record {
    /// The record as its fixture line, without the newline.
    pub fn to_line(&self) -> String {
        let raw = match self {
            Record::In(line) => RawRecord {
                dir: Some(Dir::In),
                line: Some(line.clone()),
                ..RawRecord::default()
            },
            Record::Out(line) => RawRecord {
                dir: Some(Dir::Out),
                line: Some(line.clone()),
                ..RawRecord::default()
            },
            Record::InEof => RawRecord {
                dir: Some(Dir::In),
                eof: Some(true),
                ..RawRecord::default()
            },
            Record::Exit(code) => RawRecord {
                exit: Some(*code),
                ..RawRecord::default()
            },
        };
        to_line(&raw)
    }
}

impl Fixture {
    /// Reads and parses the fixture at `path`.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, FixtureError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|err| FixtureError(format!("fixture {}: {err}", path.display())))?;
        Self::parse(&path.display().to_string(), &text)
    }

    /// Parses fixture `text`; `source` names it in errors.
    pub fn parse(source: &str, text: &str) -> Result<Self, FixtureError> {
        let error =
            |number: usize, message: &str| FixtureError(format!("{source}:{number}: {message}"));
        let mut header = None;
        let mut records = Vec::new();
        let mut exited = false;
        let lines = text
            .lines()
            .enumerate()
            .map(|(index, line)| (index + 1, line.trim()))
            .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'));
        for (number, line) in lines {
            let raw: RawRecord =
                serde_json::from_str(line).map_err(|err| error(number, &err.to_string()))?;
            if exited {
                return Err(error(number, "record after the exit record"));
            }
            let record = match raw {
                RawRecord {
                    header: Some(found),
                    dir: None,
                    line: None,
                    eof: None,
                    exit: None,
                } => {
                    if header.is_some() || !records.is_empty() {
                        return Err(error(number, "the header must be the first record"));
                    }
                    header = Some(found);
                    continue;
                }
                RawRecord {
                    header: None,
                    dir: Some(dir),
                    line: Some(line),
                    eof: None,
                    exit: None,
                } => match dir {
                    Dir::In => Record::In(line),
                    Dir::Out => Record::Out(line),
                },
                RawRecord {
                    header: None,
                    dir: Some(Dir::In),
                    line: None,
                    eof: Some(true),
                    exit: None,
                } => Record::InEof,
                RawRecord {
                    header: None,
                    dir: None,
                    line: None,
                    eof: None,
                    exit: Some(code),
                } => {
                    exited = true;
                    Record::Exit(code)
                }
                _ => {
                    return Err(error(
                        number,
                        "expected one of {\"header\": {..}}, {\"dir\": \"in\"|\"out\", \
                         \"line\": ..}, {\"dir\": \"in\", \"eof\": true} or {\"exit\": ..}",
                    ));
                }
            };
            records.push((number, record));
        }
        if !exited {
            return Err(FixtureError(format!("{source}: no exit record")));
        }
        Ok(Self {
            source: source.to_owned(),
            header,
            records,
        })
    }

    /// The records in order.
    pub fn records(&self) -> impl Iterator<Item = &Record> {
        self.records.iter().map(|(_, record)| record)
    }

    /// Plays the fixture against a transport's channels; see
    /// [`Transport::replay`](crate::transport::Transport::replay).
    pub(crate) async fn play(
        self,
        mut stdin: mpsc::Receiver<String>,
        stdout: mpsc::Sender<String>,
        exit: oneshot::Sender<Exit>,
    ) {
        let ignore_keys = self
            .header
            .map(|header| header.ignore_keys)
            .unwrap_or_default();
        for (number, record) in self.records {
            let at = format!("fixture {}:{number}", self.source);
            let failure = match record {
                Record::Out(line) => {
                    // A reader that is gone no longer cares, as with a real child.
                    let _ = stdout.send(line).await;
                    continue;
                }
                Record::In(expected) => match stdin.recv().await {
                    Some(received) => match compare(&expected, &received, &ignore_keys) {
                        Ok(()) => continue,
                        Err(difference) => format!(
                            "{at}: sent line does not match\n  expected: {expected}\n  \
                             received: {received}\n  {difference}"
                        ),
                    },
                    None => format!("{at}: stdin closed, expected the line\n  {expected}"),
                },
                Record::InEof => match stdin.recv().await {
                    None => continue,
                    Some(received) => {
                        format!("{at}: expected stdin to close, received the line\n  {received}")
                    }
                },
                Record::Exit(code) => {
                    drop(stdout);
                    let _ = exit.send(Exit::Code(code));
                    return;
                }
            };
            // End of output first, as a real child's would, then the reason.
            drop(stdout);
            let _ = exit.send(Exit::Failed(failure));
            return;
        }
    }
}

/// Whether `received` matches the recorded `expected` line; otherwise where they differ.
fn compare(expected: &str, received: &str, ignore_keys: &[String]) -> Result<(), String> {
    if expected == received {
        return Ok(());
    }
    let (Ok(mut expected), Ok(mut received)) = (
        serde_json::from_str::<Value>(expected),
        serde_json::from_str::<Value>(received),
    ) else {
        return Err("lines differ and are not both JSON".into());
    };
    if ignore_keys.is_empty() {
        return Err(
            first_difference("$", &expected, &received).unwrap_or_else(|| {
                "same JSON, different bytes (key order or spacing); matching is exact without \
             ignore_keys"
                    .into()
            }),
        );
    }
    strip(&mut expected, ignore_keys);
    strip(&mut received, ignore_keys);
    match first_difference("$", &expected, &received) {
        None => Ok(()),
        Some(difference) => Err(format!(
            "{difference} (ignoring keys {})",
            ignore_keys.join(", ")
        )),
    }
}

/// Removes every object key in `keys`, at any depth.
fn strip(value: &mut Value, keys: &[String]) {
    match value {
        Value::Object(map) => {
            map.retain(|key, _| !keys.contains(key));
            map.values_mut().for_each(|value| strip(value, keys));
        }
        Value::Array(items) => items.iter_mut().for_each(|value| strip(value, keys)),
        _ => {}
    }
}

/// The first place `expected` and `received` differ, as a path and both values.
fn first_difference(path: &str, expected: &Value, received: &Value) -> Option<String> {
    match (expected, received) {
        (Value::Object(expected), Value::Object(received)) => {
            object_difference(path, expected, received)
        }
        (Value::Array(expected), Value::Array(received)) => {
            for index in 0..expected.len().max(received.len()) {
                let path = format!("{path}[{index}]");
                let difference = match (expected.get(index), received.get(index)) {
                    (Some(expected), Some(received)) => first_difference(&path, expected, received),
                    (Some(expected), None) => {
                        Some(format!("at {path}: expected {expected}, missing"))
                    }
                    (None, Some(received)) => Some(format!("at {path}: unexpected {received}")),
                    (None, None) => None,
                };
                if difference.is_some() {
                    return difference;
                }
            }
            None
        }
        _ if expected == received => None,
        _ => Some(format!(
            "at {path}: expected {expected}, received {received}"
        )),
    }
}

fn object_difference(
    path: &str,
    expected: &Map<String, Value>,
    received: &Map<String, Value>,
) -> Option<String> {
    for (key, value) in expected {
        let path = format!("{path}.{key}");
        let difference = match received.get(key) {
            Some(received) => first_difference(&path, value, received),
            None => Some(format!("at {path}: expected {value}, missing")),
        };
        if difference.is_some() {
            return difference;
        }
    }
    received
        .iter()
        .find(|(key, _)| !expected.contains_key(*key))
        .map(|(key, value)| format!("at {path}.{key}: unexpected {value}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|key| (*key).to_owned()).collect()
    }

    #[test]
    fn exact_match_without_ignore_keys() {
        assert_eq!(compare("hello", "hello", &[]), Ok(()));
        assert_eq!(
            compare("hello", "world", &[]),
            Err("lines differ and are not both JSON".into())
        );
        assert_eq!(
            compare(r#"{"a":1,"b":2}"#, r#"{"b":2,"a":1}"#, &[]),
            Err(
                "same JSON, different bytes (key order or spacing); matching is exact \
                 without ignore_keys"
                    .into()
            )
        );
    }

    #[test]
    fn ignore_keys_match_structurally_at_any_depth() {
        let ignore = keys(&["id", "at"]);
        assert_eq!(
            compare(
                r#"{"id":1,"msg":{"at":"t1","items":[{"id":7,"text":"hi"}]}}"#,
                r#"{"msg":{"items":[{"text":"hi","id":8}],"at":"t2"},"id":2}"#,
                &ignore,
            ),
            Ok(())
        );
    }

    #[test]
    fn difference_names_the_path() {
        let ignore = keys(&["id"]);
        assert_eq!(
            compare(
                r#"{"id":1,"msg":{"items":[{"text":"hi"}]}}"#,
                r#"{"id":2,"msg":{"items":[{"text":"ho"}]}}"#,
                &ignore,
            ),
            Err(
                r#"at $.msg.items[0].text: expected "hi", received "ho" (ignoring keys id)"#.into()
            )
        );
        assert_eq!(
            first_difference(
                "$",
                &serde_json::json!({"a": 1}),
                &serde_json::json!({"a": 1, "b": 2})
            ),
            Some("at $.b: unexpected 2".into())
        );
        assert_eq!(
            first_difference("$", &serde_json::json!([1, 2]), &serde_json::json!([1])),
            Some("at $[1]: expected 2, missing".into())
        );
    }

    #[test]
    fn records_round_trip_through_their_lines() {
        let header = Header {
            provider: "claude".into(),
            cli_version: Some("2.1.3".into()),
            recorded_at: Some("2026-10-02T12:00:00Z".parse().unwrap()),
            ignore_keys: keys(&["session_id"]),
        };
        let records = [
            Record::In(r#"{"type":"user"}"#.into()),
            Record::Out("plain text".into()),
            Record::InEof,
            Record::Exit(None),
        ];
        let text: String = std::iter::once(header.to_line())
            .chain(records.iter().map(Record::to_line))
            .map(|line| line + "\n")
            .collect();
        assert_eq!(
            text,
            "{\"header\":{\"provider\":\"claude\",\"cli_version\":\"2.1.3\",\
             \"recorded_at\":\"2026-10-02T12:00:00Z\",\"ignore_keys\":[\"session_id\"]}}\n\
             {\"dir\":\"in\",\"line\":\"{\\\"type\\\":\\\"user\\\"}\"}\n\
             {\"dir\":\"out\",\"line\":\"plain text\"}\n\
             {\"dir\":\"in\",\"eof\":true}\n\
             {\"exit\":null}\n"
        );
        let fixture = Fixture::parse("t", &text).unwrap();
        assert_eq!(fixture.header, Some(header));
        assert!(fixture.records().eq(records.iter()));
    }

    #[test]
    fn malformed_fixtures_name_the_line() {
        let cases = [
            ("{\"dir\":\"out\",\"line\":\"x\"}\n", "t: no exit record"),
            (
                "# c\n{\"dir\":\"up\",\"line\":\"x\"}\n",
                "t:2: unknown variant `up`, expected `in` or `out` at line 1 column 11",
            ),
            (
                "{\"dir\":\"out\",\"eof\":true}\n",
                "t:1: expected one of {\"header\": {..}}, {\"dir\": \"in\"|\"out\", \"line\": ..}, \
                 {\"dir\": \"in\", \"eof\": true} or {\"exit\": ..}",
            ),
            (
                "{\"exit\":0}\n{\"header\":{\"provider\":\"p\"}}\n",
                "t:2: record after the exit record",
            ),
            (
                "{\"dir\":\"out\",\"line\":\"x\"}\n{\"header\":{\"provider\":\"p\"}}\n{\"exit\":0}\n",
                "t:2: the header must be the first record",
            ),
        ];
        for (text, message) in cases {
            assert_eq!(Fixture::parse("t", text).unwrap_err().to_string(), message);
        }
    }
}

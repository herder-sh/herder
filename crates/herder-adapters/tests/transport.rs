//! The stdio transport: a real child, a recording of one, and its replay.

use std::path::PathBuf;
use std::time::Duration;

use herder_adapters::fixture::{Fixture, Header, Record};
use herder_adapters::record::{self, Redactor};
use herder_adapters::transport::{Exit, Transport};
use tokio::process::Command;
use tokio::time::timeout;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Echoes every JSON line it reads inside an envelope, then says bye and exits 3.
const ECHO: &str = r#"echo '{"type":"ready"}'; while read -r line; do echo "{\"type\":\"echo\",\"got\":$line}"; done; echo bye; exit 3"#;

fn sh(script: &str) -> Command {
    let mut command = Command::new("sh");
    command.args(["-c", script]);
    command
}

fn fixture(path: &str) -> Fixture {
    Fixture::load(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
}

/// Sends `lines`, closes stdin, and returns everything the child printed and its exit.
async fn drive(transport: Transport, lines: &[&str]) -> (Vec<String>, Exit) {
    let Transport {
        stdin,
        mut stdout,
        exit,
    } = transport;
    for line in lines {
        stdin.send((*line).to_owned()).await.unwrap();
    }
    drop(stdin);
    let mut printed = Vec::new();
    timeout(TIMEOUT, async {
        while let Some(line) = stdout.recv().await {
            printed.push(line);
        }
    })
    .await
    .expect("stdout did not close");
    let exit = timeout(TIMEOUT, exit).await.unwrap().unwrap();
    (printed, exit)
}

fn echoed(lines: &[&str]) -> Vec<String> {
    std::iter::once(r#"{"type":"ready"}"#.to_owned())
        .chain(
            lines
                .iter()
                .map(|line| format!(r#"{{"type":"echo","got":{line}}}"#)),
        )
        .chain(std::iter::once("bye".to_owned()))
        .collect()
}

#[tokio::test]
async fn process_transport_carries_lines_and_the_exit_code() {
    let lines = [r#"{"n":1}"#, r#"{"n":2}"#];
    let (printed, exit) = drive(Transport::spawn(sh(ECHO)).unwrap(), &lines).await;
    assert_eq!(printed, echoed(&lines));
    assert_eq!(exit, Exit::Code(Some(3)));
}

#[tokio::test]
async fn dropping_the_exit_kills_the_child() {
    let mut transport = Transport::spawn(sh("echo $$; exec sleep 30")).unwrap();
    let pid = timeout(TIMEOUT, transport.stdout.recv())
        .await
        .unwrap()
        .unwrap();
    drop(transport);
    let stat = format!("/proc/{pid}/stat");
    let dead = timeout(TIMEOUT, async {
        // Gone, or a zombie waiting to be reaped: either way no longer running.
        while std::fs::read_to_string(&stat).is_ok_and(|stat| !stat.contains(") Z ")) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(dead.is_ok(), "child {pid} still running");
}

#[tokio::test]
async fn committed_fixture_replays_deterministically() {
    // Recorded with:
    // herder dev record echo greeting --ignore-key id -- sh -c '<ECHO>'
    // The ids differ from the recording; the header says to ignore them.
    let lines = [
        r#"{"id":"b1","text":"hello"}"#,
        r#"{"text":"world","id":"b2"}"#,
    ];
    for _ in 0..20 {
        let replay = Transport::replay(fixture("fixtures/echo/greeting.jsonl"));
        let (printed, exit) = drive(replay, &lines).await;
        assert_eq!(
            printed,
            echoed(&[
                r#"{"id":"a1","text":"hello"}"#,
                r#"{"id":"a2","text":"world"}"#
            ])
        );
        assert_eq!(exit, Exit::Code(Some(3)));
    }
}

#[tokio::test]
async fn mismatch_fails_naming_the_line_and_the_difference() {
    let replay = Transport::replay(fixture("fixtures/echo/greeting.jsonl"));
    let (printed, exit) = drive(replay, &[r#"{"id":"b1","text":"goodbye"}"#]).await;
    assert_eq!(printed, [r#"{"type":"ready"}"#]);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/echo/greeting.jsonl");
    assert_eq!(
        exit,
        Exit::Failed(format!(
            "fixture {}:3: sent line does not match\n  \
             expected: {{\"id\":\"a1\",\"text\":\"hello\"}}\n  \
             received: {{\"id\":\"b1\",\"text\":\"goodbye\"}}\n  \
             at $.text: expected \"hello\", received \"goodbye\" (ignoring keys id)",
            path.display()
        ))
    );
}

#[tokio::test]
async fn closing_stdin_early_fails_naming_the_expected_line() {
    let replay = Transport::replay(fixture("fixtures/echo/greeting.jsonl"));
    let (_, exit) = drive(replay, &[r#"{"id":"b1","text":"hello"}"#]).await;
    let Exit::Failed(message) = exit else {
        panic!("expected a failure, got {exit:?}");
    };
    assert!(
        message
            .ends_with(":5: stdin closed, expected the line\n  {\"id\":\"a2\",\"text\":\"world\"}")
    );
}

#[tokio::test]
async fn exact_matching_without_ignore_keys() {
    let text =
        "{\"dir\":\"in\",\"line\":\"ping\"}\n{\"dir\":\"out\",\"line\":\"pong\"}\n{\"exit\":0}\n";
    let ok = Transport::replay(Fixture::parse("inline", text).unwrap());
    assert_eq!(
        drive(ok, &["ping"]).await,
        (vec!["pong".to_owned()], Exit::Code(Some(0)))
    );

    let bad = Transport::replay(Fixture::parse("inline", text).unwrap());
    assert_eq!(
        drive(bad, &["ping "]).await,
        (
            Vec::new(),
            Exit::Failed(
                "fixture inline:1: sent line does not match\n  expected: ping\n  received: \
                 ping \n  lines differ and are not both JSON"
                    .into()
            )
        )
    );
}

#[tokio::test]
async fn recording_replays_and_redacts_secrets() {
    let secret = r#"{"id":1,"auth":"Bearer s3cr3t-token","user":"jane@example.com"}"#;
    let plain = r#"{"id":2,"text":"hi"}"#;
    let input = format!("{secret}\n{plain}\n");
    let header = Header {
        provider: "echo".into(),
        cli_version: None,
        recorded_at: None,
        ignore_keys: vec!["id".into()],
    };
    let redactor = Redactor::new(&["s3cr3t".into()]).unwrap();
    let mut terminal = Vec::new();
    let mut written = Vec::new();
    let code = timeout(
        TIMEOUT,
        record::record(
            sh(ECHO),
            &header,
            &redactor,
            input.as_bytes(),
            &mut terminal,
            &mut written,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(code, Some(3));

    // The terminal sees the real output; the fixture only the redacted one.
    let printed = echoed(&[secret, plain]);
    assert_eq!(
        String::from_utf8(terminal).unwrap(),
        printed.join("\n") + "\n"
    );
    let written = String::from_utf8(written).unwrap();
    assert!(
        !written.contains("s3cr3t") && !written.contains("jane@"),
        "{written}"
    );

    let recorded = Fixture::parse("recorded", &written).unwrap();
    assert_eq!(recorded.header, Some(header));
    let redacted = r#"{"id":1,"auth":"[REDACTED]","user":"[REDACTED]"}"#;
    let ins: Vec<&Record> = recorded
        .records()
        .filter(|record| matches!(record, Record::In(_) | Record::InEof))
        .collect();
    assert_eq!(
        ins,
        [
            &Record::In(redacted.into()),
            &Record::In(plain.into()),
            &Record::InEof
        ]
    );
    assert_eq!(recorded.records().last(), Some(&Record::Exit(Some(3))));

    let replay = Transport::replay(recorded);
    let (replayed, exit) = drive(replay, &[redacted, r#"{"id":9,"text":"hi"}"#]).await;
    assert_eq!(replayed, echoed(&[redacted, plain]));
    assert_eq!(exit, Exit::Code(Some(3)));
}

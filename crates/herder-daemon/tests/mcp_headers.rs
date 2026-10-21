//! A header's `{secret:...}` placeholder, filled in by the daemon as a CLI starts, reaches each
//! provider in its native format: Claude's private `--mcp-config` file, Codex's `thread/start`
//! config and ACP's `session/new`, replayed from the adapters' recorded fixtures.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use herder_adapters::acp::{AcpAdapter, AgentProfile};
use herder_adapters::fixture::Fixture;
use herder_adapters::transport::Transport;
use herder_adapters::{
    Adapter, AdapterCommand, AdapterSession, McpServer, StartRequest, UserMcpServer, claude, codex,
};
use herder_daemon::mcp::SecretsFuture;
use herder_daemon::mcp::servers::UserServers;
use herder_protocol::{
    FetchedSecret, McpServerSettings, McpTransport, PermissionMode, SecretError, SecretErrorCode,
    SecretRef, SecretValue, TurnError,
};
use tokio::time::timeout;

const TIMEOUT: Duration = Duration::from_secs(10);

/// The value the fixtures were recorded with, as the vault holds it.
const VALUE: &str = "token";

/// The servers the adapters' `mcp_servers` fixtures were recorded with, the HTTP one's
/// `Authorization` naming its secret, as `daemon.toml` would.
fn configured(authorization: &str) -> UserServers {
    let server = |name: &str, transport| McpServerSettings {
        name: name.into(),
        enabled: true,
        project: None,
        transport,
    };
    UserServers::new(vec![
        server(
            "github",
            McpTransport::Stdio {
                command: "github-mcp-server".into(),
                args: vec!["stdio".into()],
                env: HashMap::from([("GITHUB_HOST".to_owned(), "github.com".to_owned())]),
                secrets: Vec::new(),
            },
        ),
        server(
            "linear",
            McpTransport::Http {
                url: "https://mcp.linear.app/mcp".into(),
                headers: HashMap::from([("Authorization".to_owned(), authorization.to_owned())]),
            },
        ),
        server(
            "legacy",
            McpTransport::Sse {
                url: "http://127.0.0.1:9000/sse".into(),
                headers: HashMap::new(),
            },
        ),
    ])
}

/// A vault granting the host `linear`, which holds `API_KEY`.
fn vault(secrets: Vec<SecretRef>) -> SecretsFuture {
    Box::pin(async move {
        secrets
            .into_iter()
            .map(|secret| {
                if secret.to_string() != "linear/API_KEY" {
                    return Err(SecretError {
                        code: SecretErrorCode::Unknown,
                        message: format!("no set granted to this host has {secret}"),
                        secret: Some(secret),
                        candidates: Vec::new(),
                    });
                }
                Ok(FetchedSecret {
                    secret,
                    set: "linear".into(),
                    value: SecretValue::new(VALUE).unwrap(),
                })
            })
            .collect()
    })
}

/// The servers a CLI start gets, the header's placeholder filled in from [`vault`].
async fn launched(authorization: &str) -> Vec<UserMcpServer> {
    let launch = configured(authorization)
        .for_session(None, None, vault)
        .await;
    assert!(launch.left_out.is_empty(), "{:?}", launch.left_out);
    launch.servers
}

/// The servers with the header as `daemon.toml` has it, placeholder and all.
async fn as_written() -> Vec<UserMcpServer> {
    let mut servers = launched("Bearer {secret:linear/API_KEY}").await;
    let herder_adapters::McpTransport::Http { headers, .. } = &mut servers[1].transport else {
        unreachable!("linear is the HTTP server");
    };
    headers.insert(
        "Authorization".into(),
        "Bearer {secret:linear/API_KEY}".into(),
    );
    servers
}

fn request(cwd: &str, mcp_servers: Vec<UserMcpServer>) -> StartRequest {
    StartRequest {
        config_dir: Some(PathBuf::from("/nonexistent/account")),
        env: BTreeMap::new(),
        cwd: PathBuf::from(cwd),
        model: None,
        permission_mode: PermissionMode::Ask,
        seed: Vec::new(),
        resume: None,
        mcp: Some(McpServer {
            command: PathBuf::from("/usr/bin/herder"),
            args: ["mcp", "--data-dir", "/data", "--session", "s1"]
                .map(String::from)
                .to_vec(),
        }),
        mcp_servers,
        launcher: Vec::new(),
        skills: None,
    }
}

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../herder-adapters/fixtures")
        .join(path)
}

async fn shutdown(session: AdapterSession) {
    session.commands.send(AdapterCommand::Shutdown).unwrap();
    let mut events = session.events;
    timeout(TIMEOUT, async { while events.recv().await.is_some() {} })
        .await
        .expect("events did not close");
}

async fn start_codex(servers: Vec<UserMcpServer>) -> Result<AdapterSession, TurnError> {
    let fixture = Fixture::load(fixture("codex/mcp_servers.jsonl")).unwrap();
    let request = request("/tmp/herder-codex-fixture", servers);
    timeout(TIMEOUT, codex::start(Transport::replay(fixture), request))
        .await
        .expect("start timed out")
}

async fn start_opencode(servers: Vec<UserMcpServer>) -> Result<AdapterSession, TurnError> {
    let adapter = AcpAdapter::replaying(
        AgentProfile::opencode(),
        fixture("opencode/mcp_servers.jsonl"),
    );
    let request = request("/tmp/acpwork/repo", servers);
    timeout(TIMEOUT, adapter.start(request))
        .await
        .expect("start timed out")
}

#[tokio::test]
async fn claude_gets_the_value_in_its_private_config_file_only() {
    use std::os::unix::fs::PermissionsExt;

    let servers = launched("Bearer {secret:linear/API_KEY}").await;
    let file = claude::user_mcp_file(&servers).unwrap().unwrap();
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(
        config["mcpServers"]["linear"],
        serde_json::json!({
            "type": "http",
            "url": "https://mcp.linear.app/mcp",
            "headers": {"Authorization": "Bearer token"},
        })
    );
    let request = request("/worktrees/s1", servers);
    let command = claude::command(Path::new("claude"), &request, Some(&file));
    let command = command.as_std();
    let line: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy())
        .collect();
    assert!(!line.iter().any(|arg| arg.contains("Bearer")), "{line:?}");
    assert!(command.get_envs().all(|(_, value)| value
        .is_none_or(|value| !value.to_string_lossy().contains("Bearer"))));
}

#[tokio::test]
async fn codex_gets_the_value_in_thread_start() {
    // The replay checks that `thread/start` carries `Authorization: Bearer token`.
    let session = start_codex(launched("Bearer {secret:linear/API_KEY}").await)
        .await
        .unwrap();
    shutdown(session).await;
    // The replay tells them apart: the header as written is not what Codex was recorded with.
    assert!(start_codex(as_written().await).await.is_err());
}

#[tokio::test]
async fn an_acp_agent_gets_the_value_in_session_new() {
    // The replay checks that `session/new` carries `Authorization: Bearer token`.
    let session = start_opencode(launched("Bearer {secret:linear/API_KEY}").await)
        .await
        .unwrap();
    shutdown(session).await;
    assert!(start_opencode(as_written().await).await.is_err());
}

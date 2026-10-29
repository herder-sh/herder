//! The user's MCP servers (`[[mcp_servers]]`, see [`crate::config`]), as each session's CLI
//! gets them next to herder's own.
//!
//! The list is the config file's, replaced whenever an owner changes the settings, and read as
//! a CLI starts: a session gets the servers that are enabled and are for every project or its
//! own. A stdio server with `secrets` runs through `herder secret run`, which fetches their
//! values from the vault with the session's token and hands them to the server only, as
//! variables of their names; without herder's own MCP server there is no token, and such a
//! server is left out.
//!
//! A header of a server at a URL may hold placeholders, `{secret:NAME}` or
//! `{secret:set/NAME}`, which take the secret's value as the CLI starts: fetched from the
//! vault for the session as `herder secret run` fetches them, so the vault checks its grant
//! and audits the fetch. Values go to the CLI only, in its private config file or on its stdin,
//! never in the config file, a log, a command line or the journal. A server whose secrets
//! cannot be fetched is left out of that run, and the session told why, by name only.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::{PoisonError, RwLock};

use herder_adapters::{McpTransport as Transport, UserMcpServer};
use herder_protocol::{McpServerSettings, McpTransport, ProjectId, SecretRef, SessionId};
use tracing::warn;

use crate::config::mcp_command;
use crate::mcp::SecretsFuture;

/// What starts a placeholder in a header value; the secret's name runs to the next `}`.
const PLACEHOLDER: &str = "{secret:";

/// The user's MCP servers as the config file has them now.
#[derive(Default)]
pub struct UserServers {
    servers: RwLock<Vec<McpServerSettings>>,
}

/// How a session's CLI reaches herder's MCP server, which `herder secret run` needs.
pub struct Herder<'a> {
    /// The herder binary.
    pub binary: &'a Path,
    /// The daemon's data dir.
    pub data_dir: &'a Path,
    /// The session.
    pub session_id: &'a SessionId,
}

/// The servers one start of a session's CLI gets, and those it goes without.
#[derive(Debug, Default)]
pub struct Launch {
    /// The servers, with their headers' secrets filled in.
    pub servers: Vec<UserMcpServer>,
    /// The enabled servers for the session that it goes without this run.
    pub left_out: Vec<LeftOut>,
}

/// A server a session goes without, and why; neither holds a secret's value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeftOut {
    /// The server's name.
    pub server: String,
    /// Why, for the session's user.
    pub reason: String,
}

impl UserServers {
    /// The servers `servers`.
    pub fn new(servers: Vec<McpServerSettings>) -> Self {
        Self {
            servers: RwLock::new(servers),
        }
    }

    /// Replaces the servers from the next CLI start on.
    pub fn set(&self, servers: Vec<McpServerSettings>) {
        // Every update is one assignment, so a poisoned lock holds a consistent list.
        *self.servers.write().unwrap_or_else(PoisonError::into_inner) = servers;
    }

    /// The servers a CLI of a session in `project` gets, as it is to reach them; `herder` is
    /// absent when the daemon serves no MCP server. `fetch` fetches the values of the secrets
    /// a server's headers name, for the session, as `herder secret run` does.
    pub async fn for_session(
        &self,
        project: Option<&ProjectId>,
        herder: Option<&Herder<'_>>,
        fetch: impl Fn(Vec<SecretRef>) -> SecretsFuture,
    ) -> Launch {
        let mut launch = Launch::default();
        let servers: Vec<_> = {
            let servers = self.servers.read().unwrap_or_else(PoisonError::into_inner);
            servers
                .iter()
                .filter(|server| server.enabled)
                .filter(|server| {
                    server
                        .project
                        .as_ref()
                        .is_none_or(|only| Some(only) == project)
                })
                .map(|server| (server.name.clone(), launch_one(server, herder)))
                .collect()
        };
        for (name, server) in servers {
            let server = match server {
                Ok(server) => fill_in_secrets(server, &fetch).await,
                Err(err) => Err(format!("{err:#}")),
            };
            match server {
                Ok(server) => launch.servers.push(server),
                Err(reason) => {
                    warn!(server = name, "the MCP server is left out: {reason}");
                    launch.left_out.push(LeftOut {
                        server: name,
                        reason,
                    });
                }
            }
        }
        launch
    }
}

/// The secrets the placeholders in header `value` name, in order; an error says what is wrong
/// with one.
pub fn header_secrets(value: &str) -> Result<Vec<SecretRef>, String> {
    Ok(parts(value)?
        .into_iter()
        .filter_map(|part| match part {
            Part::Secret(secret) => Some(secret),
            Part::Text(_) => None,
        })
        .collect())
}

/// A piece of a header value: text as written, or a secret's placeholder.
enum Part<'a> {
    Text(&'a str),
    Secret(SecretRef),
}

/// Header `value` split at its placeholders.
fn parts(mut value: &str) -> Result<Vec<Part<'_>>, String> {
    let mut parts = Vec::new();
    while let Some(start) = value.find(PLACEHOLDER) {
        if start > 0 {
            parts.push(Part::Text(&value[..start]));
        }
        let rest = &value[start + PLACEHOLDER.len()..];
        let end = rest
            .find('}')
            .ok_or_else(|| format!("`{PLACEHOLDER}` is never closed with `}}`"))?;
        let secret = rest[..end]
            .parse()
            .map_err(|err| format!("in `{PLACEHOLDER}...}}`: {err}"))?;
        parts.push(Part::Secret(secret));
        value = &rest[end + 1..];
    }
    if !value.is_empty() {
        parts.push(Part::Text(value));
    }
    Ok(parts)
}

/// `server` with the secrets its headers name filled in, fetched with `fetch`; an error says,
/// by name only, why it cannot have them.
async fn fill_in_secrets(
    mut server: UserMcpServer,
    fetch: &impl Fn(Vec<SecretRef>) -> SecretsFuture,
) -> Result<UserMcpServer, String> {
    let (Transport::Http { headers, .. } | Transport::Sse { headers, .. }) = &mut server.transport
    else {
        return Ok(server);
    };
    let mut secrets = BTreeSet::new();
    for value in headers.values() {
        secrets.extend(header_secrets(value)?);
    }
    if secrets.is_empty() {
        return Ok(server);
    }
    let names = secrets
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let fetched = fetch(secrets.into_iter().collect())
        .await
        .map_err(|err| format!("cannot fetch {names} from the vault: {}", err.message))?;
    let values: HashMap<_, _> = fetched
        .iter()
        .map(|fetched| (&fetched.secret, fetched.value.expose()))
        .collect();
    for (header, value) in headers.iter_mut() {
        let mut filled = String::new();
        for part in parts(value)? {
            match part {
                Part::Text(text) => filled.push_str(text),
                Part::Secret(secret) => {
                    let value = values
                        .get(&secret)
                        .ok_or_else(|| format!("the vault did not send {secret}"))?;
                    if value.contains(['\r', '\n']) {
                        return Err(format!(
                            "{secret} holds a line break, which header {header} cannot"
                        ));
                    }
                    filled.push_str(value);
                }
            }
        }
        *value = filled;
    }
    Ok(server)
}

/// `server` as the CLI is to reach it, its headers' placeholders as written.
fn launch_one(
    server: &McpServerSettings,
    herder: Option<&Herder<'_>>,
) -> anyhow::Result<UserMcpServer> {
    let sorted = |map: &HashMap<String, String>| -> BTreeMap<String, String> {
        map.iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    };
    let transport = match &server.transport {
        McpTransport::Stdio {
            command,
            args,
            env,
            secrets,
        } => {
            let program = mcp_command(command, &|key| std::env::var_os(key))?;
            let program = program.to_string_lossy().into_owned();
            let (command, args) = if secrets.is_empty() {
                (program, args.clone())
            } else {
                let herder = herder.ok_or_else(|| {
                    anyhow::anyhow!("its secrets need herder's MCP server, which is not running")
                })?;
                let mut wrapped = vec![
                    "secret".to_owned(),
                    "run".to_owned(),
                    "--session".to_owned(),
                    herder.session_id.to_string(),
                    "--data-dir".to_owned(),
                    herder.data_dir.to_string_lossy().into_owned(),
                ];
                wrapped.extend(secrets.iter().map(ToString::to_string));
                wrapped.push("--".to_owned());
                wrapped.push(program);
                wrapped.extend(args.iter().cloned());
                (herder.binary.to_string_lossy().into_owned(), wrapped)
            };
            Transport::Stdio {
                command,
                args,
                env: sorted(env),
            }
        }
        McpTransport::Http { url, headers } => Transport::Http {
            url: url.clone(),
            headers: sorted(headers),
        },
        McpTransport::Sse { url, headers } => Transport::Sse {
            url: url.clone(),
            headers: sorted(headers),
        },
    };
    Ok(UserMcpServer {
        name: server.name.clone(),
        transport,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use herder_protocol::{FetchedSecret, SecretError, SecretErrorCode, SecretValue};

    use super::*;

    /// A vault granting the host `prod`, which holds `LINEAR_KEY` and `DB_URL`: what
    /// [`crate::vault::HostSecrets::fetch`] answers for it, refusing every secret unless all
    /// are there, as the vault does.
    fn vault(secrets: Vec<SecretRef>) -> SecretsFuture {
        Box::pin(async move {
            let held = [("LINEAR_KEY", "lin_api_s3cr3t"), ("DB_URL", "postgres://x")];
            secrets
                .into_iter()
                .map(|secret| {
                    let value = held
                        .iter()
                        .find(|(name, _)| {
                            *name == secret.name
                                && secret.set.as_deref().is_none_or(|s| s == "prod")
                        })
                        .map(|(_, value)| *value);
                    match value {
                        Some(value) => Ok(FetchedSecret {
                            secret,
                            set: "prod".into(),
                            value: SecretValue::new(value).unwrap(),
                        }),
                        None => Err(SecretError {
                            code: SecretErrorCode::Unknown,
                            message: format!("no set granted to this host has {secret}"),
                            secret: Some(secret),
                            candidates: Vec::new(),
                        }),
                    }
                })
                .collect()
        })
    }

    fn no_vault(_: Vec<SecretRef>) -> SecretsFuture {
        Box::pin(async {
            Err(SecretError {
                code: SecretErrorCode::NoVault,
                message: "this host is not linked to a vault with Secrets on, so it has no secrets; an owner \
                          links it to one in the herder app, on the machine's page under Secrets"
                    .into(),
                secret: None,
                candidates: Vec::new(),
            })
        })
    }

    fn http(name: &str, headers: &[(&str, &str)]) -> McpServerSettings {
        McpServerSettings {
            name: name.into(),
            enabled: true,
            project: None,
            transport: McpTransport::Http {
                url: format!("https://{name}.example.com/mcp"),
                headers: headers
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                    .collect(),
            },
        }
    }

    fn stdio(name: &str, secrets: &[&str]) -> McpServerSettings {
        McpServerSettings {
            name: name.into(),
            enabled: true,
            project: None,
            transport: McpTransport::Stdio {
                command: "/opt/bin/server".into(),
                args: vec!["--stdio".into()],
                env: HashMap::from([("MODE".to_owned(), "fast".to_owned())]),
                secrets: secrets.iter().map(|s| s.parse().unwrap()).collect(),
            },
        }
    }

    fn names(servers: &[UserMcpServer]) -> Vec<&str> {
        servers.iter().map(|server| server.name.as_str()).collect()
    }

    #[tokio::test]
    async fn a_session_gets_the_enabled_servers_for_every_project_or_its_own() {
        let herder_project = ProjectId::new("github.com/herder-sh/herder");
        let servers = UserServers::new(vec![
            stdio("everywhere", &[]),
            McpServerSettings {
                enabled: false,
                ..stdio("off", &[])
            },
            McpServerSettings {
                project: Some(herder_project.clone()),
                ..stdio("herder-only", &[])
            },
            McpServerSettings {
                name: "remote".into(),
                enabled: true,
                project: None,
                transport: McpTransport::Http {
                    url: "https://mcp.example.com/mcp".into(),
                    headers: HashMap::from([("Authorization".to_owned(), "Bearer t".to_owned())]),
                },
            },
        ]);
        assert_eq!(
            names(
                &servers
                    .for_session(Some(&herder_project), None, no_vault)
                    .await
                    .servers
            ),
            ["everywhere", "herder-only", "remote"]
        );
        let other = ProjectId::new("github.com/other/repo");
        assert_eq!(
            names(
                &servers
                    .for_session(Some(&other), None, no_vault)
                    .await
                    .servers
            ),
            ["everywhere", "remote"]
        );
        assert_eq!(
            names(&servers.for_session(None, None, no_vault).await.servers),
            ["everywhere", "remote"]
        );

        servers.set(vec![stdio("replaced", &[])]);
        let replaced = servers.for_session(None, None, no_vault).await.servers;
        assert_eq!(names(&replaced), ["replaced"]);
        assert_eq!(
            replaced[0].transport,
            Transport::Stdio {
                command: "/opt/bin/server".into(),
                args: vec!["--stdio".into()],
                env: BTreeMap::from([("MODE".to_owned(), "fast".to_owned())]),
            }
        );
    }

    #[tokio::test]
    async fn secrets_run_the_server_through_herder_secret_run() {
        let servers = UserServers::new(vec![stdio("vaulted", &["API_TOKEN", "prod/DB_URL"])]);
        // Without herder's MCP server there is no token to fetch them with.
        let launch = servers.for_session(None, None, no_vault).await;
        assert!(launch.servers.is_empty());
        assert_eq!(
            launch.left_out,
            [LeftOut {
                server: "vaulted".into(),
                reason: "its secrets need herder's MCP server, which is not running".into(),
            }]
        );
        let session_id = SessionId::new("s1");
        let herder = Herder {
            binary: Path::new("/usr/bin/herder"),
            data_dir: Path::new("/data"),
            session_id: &session_id,
        };
        let launched = servers
            .for_session(None, Some(&herder), no_vault)
            .await
            .servers;
        assert_eq!(
            launched[0].transport,
            Transport::Stdio {
                command: "/usr/bin/herder".into(),
                args: [
                    "secret",
                    "run",
                    "--session",
                    "s1",
                    "--data-dir",
                    "/data",
                    "API_TOKEN",
                    "prod/DB_URL",
                    "--",
                    "/opt/bin/server",
                    "--stdio",
                ]
                .map(String::from)
                .to_vec(),
                env: BTreeMap::from([("MODE".to_owned(), "fast".to_owned())]),
            }
        );
    }

    #[test]
    fn placeholders_name_secrets_in_header_values() {
        let named = |value| {
            header_secrets(value)
                .map(|secrets| secrets.iter().map(ToString::to_string).collect::<Vec<_>>())
        };
        assert_eq!(
            named("Bearer {secret:linear/API_KEY}").unwrap(),
            ["linear/API_KEY"]
        );
        assert_eq!(
            named("{secret:USER}:{secret:prod/PASS}").unwrap(),
            ["USER", "prod/PASS"]
        );
        assert!(
            named("Bearer plain {not a placeholder}")
                .unwrap()
                .is_empty()
        );
        assert!(
            named("Bearer {secret:API_KEY")
                .unwrap_err()
                .contains("never closed")
        );
        assert!(named("{secret:}").is_err());
        assert!(named("{secret:Prod/API_KEY}").is_err());
        assert!(named("{secret:a/b/C}").is_err());
    }

    #[tokio::test]
    async fn header_placeholders_take_the_secrets_values_as_the_cli_starts() {
        let servers = UserServers::new(vec![
            http(
                "linear",
                &[
                    ("Authorization", "Bearer {secret:LINEAR_KEY}"),
                    ("X-Db", "{secret:prod/DB_URL}?{secret:LINEAR_KEY}"),
                    ("X-Plain", "as written"),
                ],
            ),
            http("plain", &[("Authorization", "Bearer kept")]),
        ]);
        let launch = servers.for_session(None, None, vault).await;
        assert!(launch.left_out.is_empty());
        assert_eq!(
            launch.servers[0].transport,
            Transport::Http {
                url: "https://linear.example.com/mcp".into(),
                headers: BTreeMap::from([
                    ("Authorization".into(), "Bearer lin_api_s3cr3t".into()),
                    ("X-Db".into(), "postgres://x?lin_api_s3cr3t".into()),
                    ("X-Plain".into(), "as written".into()),
                ]),
            }
        );
        assert_eq!(
            launch.servers[1].transport,
            Transport::Http {
                url: "https://plain.example.com/mcp".into(),
                headers: BTreeMap::from([("Authorization".into(), "Bearer kept".into())]),
            }
        );
        // The servers the CLI is given never show a value in their Debug form.
        assert!(!format!("{:?}", launch.servers).contains("s3cr3t"));
    }

    #[tokio::test]
    async fn a_server_whose_secrets_cannot_be_fetched_is_left_out_by_name() {
        let servers = UserServers::new(vec![
            http("linear", &[("Authorization", "Bearer {secret:LINEAR_KEY}")]),
            http(
                "github",
                &[("Authorization", "Bearer {secret:GITHUB_TOKEN}")],
            ),
            http("docs", &[]),
        ]);
        let launch = servers.for_session(None, None, vault).await;
        assert_eq!(names(&launch.servers), ["linear", "docs"]);
        assert_eq!(
            launch.left_out,
            [LeftOut {
                server: "github".into(),
                reason: "cannot fetch GITHUB_TOKEN from the vault: no set granted to this host \
                         has GITHUB_TOKEN"
                    .into(),
            }]
        );

        // Without a vault, every server with a placeholder goes; the rest stay.
        let launch = servers.for_session(None, None, no_vault).await;
        assert_eq!(names(&launch.servers), ["docs"]);
        assert_eq!(launch.left_out.len(), 2);
    }

    #[tokio::test]
    async fn a_secret_with_a_line_break_never_reaches_a_header() {
        let fetch = |secrets: Vec<SecretRef>| -> SecretsFuture {
            Box::pin(async move {
                Ok(secrets
                    .into_iter()
                    .map(|secret| FetchedSecret {
                        secret,
                        set: "prod".into(),
                        value: SecretValue::new("token\r\nX-Injected: 1").unwrap(),
                    })
                    .collect())
            })
        };
        let servers = UserServers::new(vec![http("linear", &[("Authorization", "{secret:K}")])]);
        let launch = servers.for_session(None, None, fetch).await;
        assert!(launch.servers.is_empty());
        assert_eq!(
            launch.left_out[0].reason,
            "K holds a line break, which header Authorization cannot"
        );
    }
}

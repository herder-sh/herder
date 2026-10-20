//! The user's MCP servers (`[[mcp_servers]]`, see [`crate::config`]), as each session's CLI
//! gets them next to herder's own.
//!
//! The list is the config file's, replaced whenever an owner changes the settings, and read as
//! a CLI starts: a session gets the servers that are enabled and are for every project or its
//! own. A stdio server with `secrets` runs through `herder secret run`, which fetches their
//! values from the vault with the session's token and hands them to the server only, as
//! variables of their names; without herder's own MCP server there is no token, and such a
//! server is left out.

use std::path::Path;
use std::sync::{PoisonError, RwLock};

use herder_adapters::{McpTransport as Transport, UserMcpServer};
use herder_protocol::{McpServerSettings, McpTransport, ProjectId, SessionId};
use tracing::warn;

use crate::config::mcp_command;

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
    /// absent when the daemon serves no MCP server.
    pub fn for_session(
        &self,
        project: Option<&ProjectId>,
        herder: Option<&Herder<'_>>,
    ) -> Vec<UserMcpServer> {
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
            .filter_map(|server| match launch(server, herder) {
                Ok(server) => Some(server),
                Err(err) => {
                    warn!(server = server.name, "the MCP server is left out: {err:#}");
                    None
                }
            })
            .collect()
    }
}

/// `server` as the CLI is to reach it.
fn launch(
    server: &McpServerSettings,
    herder: Option<&Herder<'_>>,
) -> anyhow::Result<UserMcpServer> {
    let sorted = |map: &std::collections::HashMap<String, String>| {
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

    use super::*;

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

    #[test]
    fn a_session_gets_the_enabled_servers_for_every_project_or_its_own() {
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
            names(&servers.for_session(Some(&herder_project), None)),
            ["everywhere", "herder-only", "remote"]
        );
        let other = ProjectId::new("github.com/other/repo");
        assert_eq!(
            names(&servers.for_session(Some(&other), None)),
            ["everywhere", "remote"]
        );
        assert_eq!(
            names(&servers.for_session(None, None)),
            ["everywhere", "remote"]
        );

        servers.set(vec![stdio("replaced", &[])]);
        let replaced = servers.for_session(None, None);
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

    #[test]
    fn secrets_run_the_server_through_herder_secret_run() {
        let servers = UserServers::new(vec![stdio("vaulted", &["API_TOKEN", "prod/DB_URL"])]);
        // Without herder's MCP server there is no token to fetch them with.
        assert!(servers.for_session(None, None).is_empty());
        let session_id = SessionId::new("s1");
        let herder = Herder {
            binary: Path::new("/usr/bin/herder"),
            data_dir: Path::new("/data"),
            session_id: &session_id,
        };
        let launched = servers.for_session(None, Some(&herder));
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
}

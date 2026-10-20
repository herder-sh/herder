//! Signing a host in to AWS IAM Identity Center with the unmodified AWS CLI.
//!
//! The profiles come from the section headers of `$AWS_CONFIG_FILE`, else `~/.aws/config`;
//! nothing else of the AWS CLI's files is ever opened, so the credentials and the tokens it
//! caches stay unread. A sign-in runs `aws sso login --use-device-code --no-browser` in a
//! terminal ([`crate::terminal`]) and [`DeviceWatch`] picks the device code out of what it
//! prints, for the owner to approve in a browser.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use herder_protocol::{AwsDeviceCode, AwsProfile, ErrorCode, ErrorInfo};
use portable_pty::CommandBuilder;

/// Output kept while looking for the device code; the CLI prints it within its first lines.
const WATCHED: usize = 16 * 1024;

/// Where this host's AWS CLI and its config are.
#[derive(Clone, Debug)]
pub struct Aws {
    /// The directories `aws` is looked for in, as `PATH` lists them.
    path: Option<OsString>,
    /// The home directory, where a sign-in runs.
    home: Option<PathBuf>,
    /// The config file the profiles are read from.
    config: Option<PathBuf>,
}

impl Aws {
    /// The AWS CLI on `env`'s `PATH`, its config at `AWS_CONFIG_FILE` or `$HOME/.aws/config`.
    pub fn from_env(env: impl Fn(&str) -> Option<OsString>) -> Self {
        let home = env("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute());
        let config = env("AWS_CONFIG_FILE")
            .filter(|file| !file.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|home| home.join(".aws").join("config")));
        Self {
            path: env("PATH"),
            home,
            config,
        }
    }

    /// The profiles of the config file, in its order; none when it is absent.
    pub fn profiles(&self) -> Result<Vec<AwsProfile>, ErrorInfo> {
        let Some(config) = &self.config else {
            return Ok(Vec::new());
        };
        match std::fs::read_to_string(config) {
            Ok(text) => Ok(profiles(&text)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(err) => Err(ErrorInfo {
                code: ErrorCode::Internal,
                message: format!("cannot read {}: {err}", config.display()),
            }),
        }
    }

    /// `aws sso login` of `profile`, run in the home directory.
    pub fn sso_login(&self, profile: &str) -> Result<CommandBuilder, ErrorInfo> {
        let Some(program) = self.path.as_deref().and_then(|path| find("aws", path)) else {
            return Err(ErrorInfo {
                code: ErrorCode::Unsupported,
                message: "the AWS CLI is not installed on this host: `aws` is not on its PATH"
                    .to_owned(),
            });
        };
        if !self.profiles()?.iter().any(|known| known.name == profile) {
            return Err(ErrorInfo {
                code: ErrorCode::BadRequest,
                message: format!("this host's AWS config has no profile {profile:?}"),
            });
        }
        let mut command = CommandBuilder::new(program);
        command.args([
            "sso",
            "login",
            "--profile",
            profile,
            "--use-device-code",
            "--no-browser",
        ]);
        if let Some(home) = &self.home {
            command.cwd(home);
        }
        Ok(command)
    }
}

/// The profiles named by the section headers of an AWS config file: `[default]` and
/// `[profile <name>]`, each signing in through IAM Identity Center when it has an
/// `sso_session` or `sso_start_url`.
fn profiles(text: &str) -> Vec<AwsProfile> {
    let mut profiles: Vec<AwsProfile> = Vec::new();
    // The profile the keys being read belong to; none in another kind of section.
    let mut current = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let header = header.trim();
            let name = match header.strip_prefix("profile") {
                Some(rest) if rest.starts_with(char::is_whitespace) => Some(rest.trim()),
                _ => (header == "default").then_some(header),
            };
            current = name.filter(|name| !name.is_empty()).map(|name| {
                // A profile named twice is one profile, its keys merged.
                profiles
                    .iter()
                    .position(|known| known.name == name)
                    .unwrap_or_else(|| {
                        profiles.push(AwsProfile {
                            name: name.to_owned(),
                            sso: false,
                        });
                        profiles.len() - 1
                    })
            });
            continue;
        }
        let key = line.split_once('=').map(|(key, _)| key.trim());
        if matches!(key, Some("sso_session" | "sso_start_url"))
            && let Some(profile) = current.and_then(|at| profiles.get_mut(at))
        {
            profile.sso = true;
        }
    }
    profiles
}

/// `name` in one of the directories of `path`, if it is a file there.
pub(crate) fn find(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|program| is_executable(program))
}

fn is_executable(program: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(program)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Picks the device code out of an `aws sso login --use-device-code --no-browser`'s output:
///
/// ```text
/// Browser will not be automatically opened.
/// Please visit the following URL:
///
/// https://device.sso.eu-west-1.amazonaws.com/
///
/// Then enter the code:
///
/// ABCD-EFGH
///
/// Alternatively, you may visit the following URL which will autofill the code upon loading:
/// https://device.sso.eu-west-1.amazonaws.com/?user_code=ABCD-EFGH
/// ```
///
/// Older CLIs print no autofilling link; the code is then added to the verification page's.
#[derive(Default)]
pub struct DeviceWatch {
    seen: Vec<u8>,
    found: Option<AwsDeviceCode>,
}

impl DeviceWatch {
    /// Takes more output; returns the device code when this output made it known or better.
    pub fn feed(&mut self, data: &[u8]) -> Option<AwsDeviceCode> {
        let room = WATCHED.saturating_sub(self.seen.len());
        if room == 0 {
            return None;
        }
        self.seen.extend(data.iter().take(room));
        let text = String::from_utf8_lossy(&self.seen);
        // Only whole lines: a link still being printed is not the link.
        let whole = text.rfind('\n').map_or("", |end| &text[..end]);
        let device = device(whole)?;
        if self.found.as_ref() == Some(&device) {
            return None;
        }
        self.found = Some(device.clone());
        Some(device)
    }
}

/// The device code in `text`, whole lines of the CLI's output.
fn device(text: &str) -> Option<AwsDeviceCode> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let after = |prompt: &str| {
        let at = lines.iter().position(|line| line.contains(prompt))?;
        lines[at + 1..]
            .iter()
            .find(|line| !line.is_empty())
            .copied()
    };
    let user_code = after("enter the code")?;
    let complete = lines
        .iter()
        .find(|line| is_url(line) && line.contains("user_code="));
    let verification_url = match complete {
        Some(url) => (*url).to_owned(),
        None => {
            let page = lines.iter().find(|line| is_url(line))?;
            let join = if page.contains('?') { '&' } else { '?' };
            format!("{page}{join}user_code={user_code}")
        }
    };
    Some(AwsDeviceCode {
        verification_url,
        user_code: user_code.to_owned(),
    })
}

fn is_url(line: &str) -> bool {
    line.starts_with("https://") && !line.contains(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str, sso: bool) -> AwsProfile {
        AwsProfile {
            name: name.to_owned(),
            sso,
        }
    }

    #[test]
    fn profiles_are_the_default_and_profile_sections() {
        let config = "\
# a comment
[default]
region = eu-west-1

[profile prod-admin]
sso_session = corp
sso_account_id = 111122223333
sso_role_name = Admin

[sso-session corp]
sso_start_url = https://corp.awsapps.com/start
sso_region = eu-west-1

[services local]
sso_session = corp

[profile legacy]
sso_start_url = https://corp.awsapps.com/start
[ profile  spaced ]
output=json
[profileless]
";
        assert_eq!(
            profiles(config),
            vec![
                profile("default", false),
                profile("prod-admin", true),
                profile("legacy", true),
                profile("spaced", false),
            ]
        );
    }

    #[test]
    fn keys_of_other_sections_never_mark_a_profile_sso() {
        let config = "[profile plain]\nregion = us-east-1\n[sso-session corp]\nsso_start_url = x\n";
        assert_eq!(profiles(config), vec![profile("plain", false)]);
        assert_eq!(profiles(""), Vec::new());
    }

    #[test]
    fn a_missing_config_has_no_profiles() {
        let tmp = tempfile::tempdir().unwrap();
        let aws = Aws {
            path: None,
            home: Some(tmp.path().to_owned()),
            config: Some(tmp.path().join(".aws/config")),
        };
        assert_eq!(aws.profiles().unwrap(), Vec::new());
    }

    #[test]
    fn the_config_file_is_aws_config_file_else_in_home() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                vars.iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| OsString::from(value))
            }
        };
        let aws = Aws::from_env(env(&[("HOME", "/h")]));
        assert_eq!(aws.config, Some(PathBuf::from("/h/.aws/config")));
        let aws = Aws::from_env(env(&[("HOME", "/h"), ("AWS_CONFIG_FILE", "/etc/aws")]));
        assert_eq!(aws.config, Some(PathBuf::from("/etc/aws")));
        assert_eq!(Aws::from_env(env(&[])).config, None);
    }

    #[test]
    fn signing_in_needs_aws_on_path_and_a_known_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        std::fs::write(&config, "[profile prod]\nsso_session = corp\n").unwrap();
        let mut aws = Aws {
            path: Some(tmp.path().as_os_str().to_owned()),
            home: Some(tmp.path().to_owned()),
            config: Some(config),
        };
        let missing = aws.sso_login("prod").unwrap_err();
        assert_eq!(missing.code, ErrorCode::Unsupported);
        let program = tmp.path().join("aws");
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            aws.sso_login("dev").unwrap_err().code,
            ErrorCode::BadRequest
        );
        let command = aws.sso_login("prod").unwrap();
        let argv: Vec<_> = command
            .get_argv()
            .iter()
            .map(|arg| arg.to_owned())
            .collect();
        assert_eq!(
            argv,
            [
                program.as_os_str(),
                "sso".as_ref(),
                "login".as_ref(),
                "--profile".as_ref(),
                "prod".as_ref(),
                "--use-device-code".as_ref(),
                "--no-browser".as_ref(),
            ]
        );
        aws.path = None;
        assert_eq!(
            aws.sso_login("prod").unwrap_err().code,
            ErrorCode::Unsupported
        );
    }

    const PROMPT: &str = "Browser will not be automatically opened.\r\n\
Please visit the following URL:\r\n\r\n\
https://device.sso.eu-west-1.amazonaws.com/\r\n\r\n\
Then enter the code:\r\n\r\n\
ABCD-EFGH\r\n\r\n\
Alternatively, you may visit the following URL which will autofill the code upon loading:\r\n\
https://device.sso.eu-west-1.amazonaws.com/?user_code=ABCD-EFGH\r\n";

    #[test]
    fn the_device_code_is_read_with_its_autofilling_link() {
        let mut watch = DeviceWatch::default();
        let found = PROMPT
            .as_bytes()
            .chunks(7)
            .filter_map(|chunk| watch.feed(chunk))
            .last();
        assert_eq!(
            found,
            Some(AwsDeviceCode {
                verification_url: "https://device.sso.eu-west-1.amazonaws.com/?user_code=ABCD-EFGH"
                    .into(),
                user_code: "ABCD-EFGH".into(),
            })
        );
        assert_eq!(watch.feed(b"Successfully logged into Start URL\r\n"), None);
    }

    #[test]
    fn an_older_cli_gets_the_code_added_to_the_verification_page() {
        let older = "Browser will not be automatically opened.\n\
Please visit the following URL:\n\n\
https://device.sso.us-east-1.amazonaws.com/\n\n\
Then enter the code:\n\n\
WXYZ-1234\n";
        let mut watch = DeviceWatch::default();
        assert_eq!(watch.feed(&older.as_bytes()[..older.len() - 1]), None);
        assert_eq!(
            watch.feed(b"\n"),
            Some(AwsDeviceCode {
                verification_url: "https://device.sso.us-east-1.amazonaws.com/?user_code=WXYZ-1234"
                    .into(),
                user_code: "WXYZ-1234".into(),
            })
        );
    }
}

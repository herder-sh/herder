//! Skills: folders of instructions, each with a `SKILL.md` (the Agent Skills format), that the
//! provider CLIs load.
//!
//! The skill library is one folder per skill in each daemon's data dir, outside any worktree;
//! the daemon links the enabled skills where each provider CLI looks for skills. Until the
//! owner picks a git repository for it, the library is the machine's own: a write (`put_skill`,
//! `delete_skill`, `import_skill`) commits there and goes nowhere else. Once a repository is
//! set, the library is a checkout of it: a write commits and pushes through the one daemon it
//! is sent to, and the client then sends `pull_skills` to its other machines. Whether a skill
//! is enabled is kept per machine. Sessions also see the project skills checked in to their
//! worktree and the skills in their account's own config dir.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AccountId, Bytes, Provider, Timestamp};

/// Most characters a skill name may have.
pub const MAX_SKILL_NAME_CHARS: usize = 64;

/// Most bytes the files of one `put_skill` may have together.
pub const MAX_SKILL_BYTES: usize = 5 * 1024 * 1024;

/// Whether `name` may name a skill, as the Agent Skills format allows: 1 to
/// [`MAX_SKILL_NAME_CHARS`] lowercase ASCII letters, digits and hyphens, neither starting nor
/// ending with a hyphen nor holding two in a row. It is also the skill's folder name.
pub fn is_valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_SKILL_NAME_CHARS
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
}

/// One file of a skill's folder, for `put_skill`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkillFile {
    /// Path within the skill's folder, `/`-separated, without `.` or `..` components, e.g.
    /// `SKILL.md` or `scripts/run.sh`.
    pub path: String,
    /// The file's bytes.
    pub data: Bytes,
    /// Whether the file is executable, as a script the skill runs is; `false` when absent.
    #[serde(default)]
    pub executable: bool,
}

/// The skill library as one daemon has it, and how its skills reach the provider CLIs there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkillsStatus {
    /// The library's git URL, as `set_skills_repo` set it, without any credentials it held;
    /// absent while the library is the machine's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The commit the daemon's checkout is at; absent until it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// When the daemon last pulled the library, successfully or not; absent until it tried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pull: Option<Timestamp>,
    /// Why the last pull failed; absent when it succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_error: Option<String>,
    /// The library's skills at `head`, ordered by name.
    pub skills: Vec<LibrarySkill>,
    /// When each provider CLI on this machine picks up a change to the skills.
    pub reload: Vec<ProviderReload>,
    /// The skills each account's CLI loads from the account's own config dir, such as
    /// `~/.claude/skills`, for the accounts that have any, ordered by account.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<AccountSkills>,
}

/// The skills an account's CLI loads from the account's own config dir.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AccountSkills {
    /// The account.
    pub account_id: AccountId,
    /// Its skills, each with `source` `account`, ordered by name, then by path.
    pub skills: Vec<SessionSkill>,
}

/// A skill of the library.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LibrarySkill {
    /// The skill's name, its folder in the library.
    pub name: String,
    /// What the skill is for, from its `SKILL.md`.
    pub description: String,
    /// Whether the skill is enabled on this machine, as `set_skill_enabled` set it; a new skill
    /// is.
    pub enabled: bool,
    /// The providers whose CLIs on this machine load the skill; empty while it is disabled.
    pub providers: Vec<Provider>,
}

/// When one provider's CLI picks up a change to the skills.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderReload {
    /// The provider.
    pub provider: Provider,
    /// When its running sessions see a change.
    pub reload: SkillReload,
}

/// When a provider CLI picks up a change to the skills.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkillReload {
    /// At once, within a running turn.
    Live,
    /// At the session's next turn.
    NextTurn,
    /// Only in sessions started after the change.
    NextSession,
}

/// A skill a session's agent may use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionSkill {
    /// The skill's name.
    pub name: String,
    /// What the skill is for, from its `SKILL.md`.
    pub description: String,
    /// Where the skill comes from.
    pub source: SkillSource,
    /// A project skill's folder, relative to the worktree, e.g. `.claude/skills/deploy` or
    /// `web/.agents/skills/deploy`; an account skill's, relative to the account's config dir,
    /// e.g. `skills/pdf`; absent for a library skill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Where a session's skill comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    /// The skill library, enabled on the session's machine.
    Library,
    /// The project: checked in to the session's worktree, where its provider looks for skills.
    Project,
    /// The account: in the `skills` dir of the account's own config dir, where its CLI looks
    /// for the user's skills.
    Account,
}

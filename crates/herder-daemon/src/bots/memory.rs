//! What a bot knows: its owners' instructions, kept in its `[[bots]]` entry, and the facts it
//! keeps in its memory, kept in the daemon's store per bot ([`herder_store::Store::bot_facts`]).
//!
//! Owners change the facts from a client; the bot's own sessions change them with the
//! `remember` and `forget` tools, which act on the bot that started the session only. Every
//! change goes through [`Change::apply`], which keeps each fact one line and the memory within
//! its caps, and refuses text that looks like a secret. Every session a bot starts is told
//! both in its MCP instructions ([`brief`]).

use std::fmt::Write as _;
use std::sync::LazyLock;

use herder_protocol::{
    Bot, BotFact, BotPlace, FactId, MAX_BOT_FACT_CHARS, MAX_BOT_FACTS, MAX_BOT_INSTRUCTIONS_BYTES,
    MAX_BOT_MEMORY_BYTES, ThreadLink, Timestamp,
};
use regex::Regex;

/// A change to a bot's memory.
pub enum Change {
    /// Add a fact, or replace the one `replaces` names in its place.
    Set {
        replaces: Option<FactId>,
        text: String,
        /// The thread it was learned in.
        thread: Option<ThreadLink>,
        /// Where in the chat.
        place: Option<BotPlace>,
        /// Who it was learned from.
        by: Option<String>,
    },
    /// Drop a fact.
    Delete(FactId),
    /// Drop every fact.
    Clear,
}

/// Why a change is refused; it changed nothing.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// The text, or the memory it would make, breaks a rule the message says.
    Invalid(String),
    /// No fact has the id.
    NotFound(FactId),
}

impl Change {
    /// Applies the change to `facts`; resolves to the fact it set, if any.
    pub fn apply(self, facts: &mut Vec<BotFact>) -> Result<Option<FactId>, Refused> {
        match self {
            Change::Set {
                replaces,
                text,
                thread,
                place,
                by,
            } => {
                let text = fact_text(&text).map_err(Refused::Invalid)?;
                let fact = BotFact {
                    id: replaces
                        .clone()
                        .unwrap_or_else(|| FactId::new(ulid::Ulid::generate().to_string())),
                    text,
                    learned_at: Timestamp::now(),
                    thread,
                    place,
                    by,
                };
                let mut next = facts.clone();
                match replaces {
                    Some(id) => {
                        let old = next
                            .iter_mut()
                            .find(|fact| fact.id == id)
                            .ok_or(Refused::NotFound(id))?;
                        *old = fact.clone();
                    }
                    None => next.push(fact.clone()),
                }
                if next.len() > MAX_BOT_FACTS {
                    return Err(Refused::Invalid(format!(
                        "the memory is full: it holds {MAX_BOT_FACTS} facts at most. Merge this \
                         into a fact it has (`replaces`), or forget the least useful one first"
                    )));
                }
                let bytes: usize = next.iter().map(|fact| fact.text.len()).sum();
                if bytes > MAX_BOT_MEMORY_BYTES {
                    return Err(Refused::Invalid(format!(
                        "the memory is full: its facts hold {MAX_BOT_MEMORY_BYTES} bytes at most \
                         together. Shorten or merge facts (`replaces`), or forget the least \
                         useful one first"
                    )));
                }
                *facts = next;
                Ok(Some(fact.id))
            }
            Change::Delete(id) => {
                let before = facts.len();
                facts.retain(|fact| fact.id != id);
                if facts.len() == before {
                    return Err(Refused::NotFound(id));
                }
                Ok(None)
            }
            Change::Clear => {
                facts.clear();
                Ok(None)
            }
        }
    }
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Invalid(message) => f.write_str(message),
            Refused::NotFound(id) => write!(f, "the memory has no fact {id}"),
        }
    }
}

/// `text` as a fact: one line, without the whitespace around it.
fn fact_text(text: &str) -> Result<String, String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return Err("a fact cannot be empty".to_owned());
    }
    let chars = text.chars().count();
    if chars > MAX_BOT_FACT_CHARS {
        return Err(format!(
            "a fact is one short line of {MAX_BOT_FACT_CHARS} characters at most; this one has \
             {chars}. Keep only what a future conversation needs"
        ));
    }
    refuse_secrets(&text)?;
    Ok(text)
}

/// Checks a bot's instructions: at most [`MAX_BOT_INSTRUCTIONS_BYTES`], and no secret.
pub fn check_instructions(instructions: &str) -> Result<(), String> {
    if instructions.len() > MAX_BOT_INSTRUCTIONS_BYTES {
        return Err(format!(
            "instructions are {MAX_BOT_INSTRUCTIONS_BYTES} bytes at most; these are {}",
            instructions.len()
        ));
    }
    refuse_secrets(instructions)
}

fn refuse_secrets(text: &str) -> Result<(), String> {
    if looks_secret(text) {
        return Err(
            "this looks like it holds a secret, such as a token, key or password; \
                    secrets are never kept here: keep them in the vault"
                .to_owned(),
        );
    }
    Ok(())
}

/// Known token shapes, private keys, credentials in URLs and `password: …`.
static SECRET: LazyLock<Regex> = LazyLock::new(|| {
    // Only an invalid pattern fails, and this constant one compiles in the tests.
    Regex::new(
        r"(?x)
        \b(?:xox[abposr]-|xapp-|gh[pousr]_|github_pat_|glpat-|sk-|sk_(?:live|test)_
            |rk_(?:live|test)_|npm_|hf_|AIza)[A-Za-z0-9_-]{8,}
        | \b(?:AKIA|ASIA)[A-Z0-9]{16}\b
        | -----BEGIN[A-Z\ ]*PRIVATE\ KEY-----
        | \beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}
        | ://[^\s/:@]+:[^\s/@]+@
        | (?i:\b(?:password|passwd|secret|token|api[_-]?key|access[_-]?key)\s*[:=]\s*\S{6,})
        ",
    )
    .expect("a valid pattern")
});

/// Whether `text` looks like it holds a secret: a known token shape, or a long random-looking
/// word, mixed-case with digits, or hex.
pub fn looks_secret(text: &str) -> bool {
    if SECRET.is_match(text) {
        return true;
    }
    text.split(|c: char| !(c.is_ascii_alphanumeric() || "+/=_-".contains(c)))
        .any(|word| {
            let count = |test: fn(&char) -> bool| word.chars().filter(test).count();
            let digits = count(char::is_ascii_digit);
            let random = word.len() >= 24
                && digits >= 3
                && count(char::is_ascii_uppercase) > 0
                && count(char::is_ascii_lowercase) > 0;
            let hex = word.len() >= 32 && digits > 0 && word.chars().all(|c| c.is_ascii_hexdigit());
            random || hex
        })
}

/// What a session `bot` starts is told, after herder's own MCP instructions: the owners'
/// instructions, then the facts the bot remembers, and how to keep them.
pub fn brief(bot: &Bot, facts: &[BotFact]) -> String {
    let mut brief = format!(
        "This session talks with people in a chat, as the chat bot {}.",
        bot.name
    );
    if let Some(instructions) = bot.instructions.as_deref().map(str::trim)
        && !instructions.is_empty()
    {
        brief.push_str(
            "\n\nThe bot's owners give these instructions, which you follow and never change:\n\n",
        );
        brief.push_str(instructions);
    }
    brief.push_str(
        "\n\nThe bot remembers these facts from earlier conversations, each after its id:\n",
    );
    if facts.is_empty() {
        brief.push_str("(none yet)\n");
    }
    for fact in facts {
        let _ = writeln!(brief, "- {}: {}", fact.id, fact.text);
    }
    brief.push_str(
        "\nKeep this memory with `remember` and `forget`: save only durable facts a future \
         conversation needs, never secrets or one-off task details, and update a fact rather \
         than add one. When someone says \"remember …\" or \"forget …\", do it with them.",
    );
    brief
}

#[cfg(test)]
mod tests {
    use herder_protocol::BotId;

    use super::*;

    fn set(text: &str, replaces: Option<&FactId>) -> Change {
        Change::Set {
            replaces: replaces.cloned(),
            text: text.into(),
            thread: None,
            place: None,
            by: Some("Ada".into()),
        }
    }

    #[test]
    fn remember_replace_and_forget() {
        let mut facts = Vec::new();
        let deploy = set("  Staging deploys go\nthrough make deploy-staging ", None)
            .apply(&mut facts)
            .unwrap()
            .unwrap();
        let lead = set("Ada leads payments", None)
            .apply(&mut facts)
            .unwrap()
            .unwrap();
        assert_eq!(
            facts[0].text,
            "Staging deploys go through make deploy-staging"
        );
        // Replacing keeps the fact's id and place.
        set("Staging deploys go through make deploy-stg", Some(&deploy))
            .apply(&mut facts)
            .unwrap();
        assert_eq!(
            facts
                .iter()
                .map(|fact| (&fact.id, fact.text.as_str()))
                .collect::<Vec<_>>(),
            [
                (&deploy, "Staging deploys go through make deploy-stg"),
                (&lead, "Ada leads payments")
            ]
        );
        Change::Delete(deploy.clone()).apply(&mut facts).unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(
            Change::Delete(deploy.clone()).apply(&mut facts),
            Err(Refused::NotFound(deploy.clone()))
        );
        assert_eq!(
            set("x", Some(&deploy)).apply(&mut facts),
            Err(Refused::NotFound(deploy))
        );
        Change::Clear.apply(&mut facts).unwrap();
        assert!(facts.is_empty());
    }

    #[test]
    fn the_caps_hold_and_a_refused_change_changes_nothing() {
        let mut facts = Vec::new();
        for n in 0..MAX_BOT_FACTS {
            set(&format!("fact {n}"), None).apply(&mut facts).unwrap();
        }
        let before = facts.clone();
        let Err(Refused::Invalid(full)) = set("one more", None).apply(&mut facts) else {
            panic!("a full memory took a fact");
        };
        assert!(full.contains("forget"), "{full}");
        assert_eq!(facts, before);
        // Replacing a fact in a full memory still works.
        let first = facts[0].id.clone();
        set("fact zero", Some(&first)).apply(&mut facts).unwrap();

        let mut facts = Vec::new();
        let long = "a".repeat(MAX_BOT_FACT_CHARS);
        for _ in 0..MAX_BOT_MEMORY_BYTES / MAX_BOT_FACT_CHARS {
            set(&long, None).apply(&mut facts).unwrap();
        }
        let Err(Refused::Invalid(bytes)) = set(&long, None).apply(&mut facts) else {
            panic!("a memory took more than its bytes");
        };
        assert!(bytes.contains("bytes"), "{bytes}");

        let mut facts = Vec::new();
        let too_long = "a".repeat(MAX_BOT_FACT_CHARS + 1);
        assert!(matches!(
            set(&too_long, None).apply(&mut facts),
            Err(Refused::Invalid(_))
        ));
        assert!(matches!(
            set(" \n ", None).apply(&mut facts),
            Err(Refused::Invalid(_))
        ));
        assert!(facts.is_empty());
    }

    #[test]
    fn text_that_looks_like_a_secret_is_refused() {
        for secret in [
            "The bot token is xoxb-1234567890-abcdefghij",
            "use ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "OpenAI key sk-proj-abcdefghijklmnop",
            "AWS AKIAIOSFODNN7EXAMPLE for deploys",
            "-----BEGIN OPENSSH PRIVATE KEY-----",
            "db at postgres://admin:hunter22@db.internal/app",
            "the staging password: hunter22!",
            "API_KEY=abcdef123456",
            "session eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abc",
            "token 7Fq2xR9vLm3Kp8Wz1Nb4Tc6Yh0Jd",
            "hash 3f786850e387550fdab836ed7e6dc881de23001b",
        ] {
            assert!(looks_secret(secret), "{secret}");
            let mut facts = Vec::new();
            assert!(
                matches!(
                    set(secret, None).apply(&mut facts),
                    Err(Refused::Invalid(_))
                ),
                "{secret}"
            );
        }
        for fact in [
            "Staging deploys go through make deploy-staging",
            "Ada leads payments; ask her about refunds",
            "Docs live at https://docs.example.com/payments/refunds",
            "Tokens rotate every 90 days; ask Bob to rotate them",
            "The release-checklist-for-production-environment is in Notion",
            "Bob prefers answers in British English",
        ] {
            assert!(!looks_secret(fact), "{fact}");
        }
        assert!(check_instructions("Answer in British English.").is_ok());
        assert!(check_instructions("Deploy with token=xoxb-1234567890-abcdefghij").is_err());
        assert!(check_instructions(&"a".repeat(MAX_BOT_INSTRUCTIONS_BYTES + 1)).is_err());
    }

    #[test]
    fn the_brief_has_the_instructions_then_the_facts() {
        let mut bot = Bot {
            bot_id: BotId::new("trash-can"),
            kind: herder_protocol::BotKind::Slack,
            name: "Trash can".into(),
            token: "slack/BOT_TOKEN".parse().unwrap(),
            app_token: None,
            allow: Vec::new(),
            defaults: Default::default(),
            channel_bindings: Vec::new(),
            instructions: Some("Answer in British English.".into()),
        };
        let mut facts = Vec::new();
        set("Staging deploys go through make deploy-staging", None)
            .apply(&mut facts)
            .unwrap();
        let brief = brief(&bot, &facts);
        let instructions = brief.find("Answer in British English.").unwrap();
        let fact = brief
            .find(&format!(
                "- {}: Staging deploys go through make deploy-staging",
                facts[0].id
            ))
            .unwrap();
        assert!(instructions < fact, "{brief}");
        assert!(brief.contains("Trash can"), "{brief}");

        bot.instructions = None;
        let brief = super::brief(&bot, &[]);
        assert!(!brief.contains("instructions"), "{brief}");
        assert!(brief.contains("(none yet)"), "{brief}");
    }
}

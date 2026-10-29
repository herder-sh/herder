//! `fake_daemon --record-demo`: records the demo fleet ([`crate::demo`]) as the device that
//! set it up sees it, for the apps' demo (client-core's `Client::open_demo`): each machine as
//! listed, every session's events, and the answers to the commands the apps send to read. `apple/scripts/record-demo.sh` writes it where the apps bundle it.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use herder_client_core::{Client, Demo, DemoAnswer, DemoMachine, Machine};
use herder_protocol::{CommandBody, CommandResult, UsagePeriod};

/// How long recording any step may take.
const TIMEOUT: Duration = Duration::from_secs(60);

/// How long a command may take to answer; one that does not, or is refused, is left out.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// Records the demo fleet into `out`, as JSON.
pub async fn record(out: &Path) -> Result<()> {
    let fleet = crate::demo::Demo::start(false).await?;
    let client = &fleet.client;
    let mut machines = Vec::new();
    for machine in client.machines() {
        machines.push(tokio::time::timeout(TIMEOUT, record_machine(client, machine)).await??);
    }
    let demo = Demo { machines };
    std::fs::write(out, serde_json::to_vec(&demo)?)
        .with_context(|| format!("writing {}", out.display()))?;
    fleet.stop().await
}

async fn record_machine(client: &Client, machine: Machine) -> Result<DemoMachine> {
    let host = machine.host_id.clone();
    client.synced(host.clone()).await?;
    let subscriptions = machine
        .sessions
        .iter()
        .map(|head| client.subscribe_session(host.clone(), head.session_id.clone()))
        .collect::<Result<Vec<_>, _>>()?;
    // Waits for the replay of every session subscribed to.
    client.synced(host.clone()).await?;
    let mut logs = Vec::new();
    for subscription in &subscriptions {
        let update = subscription
            .next()
            .await
            .context("a session subscription ended")?;
        if !update.events.is_empty() {
            logs.push(update.events);
        }
    }
    let mut commands = vec![
        CommandBody::GetSettings,
        CommandBody::ListPeople,
        CommandBody::ListBots,
        CommandBody::GetBotStatus,
        CommandBody::ListSecrets,
        CommandBody::ListSecretAudit { before: None },
        CommandBody::GetVaultLink,
    ];
    for period in [
        UsagePeriod::Day,
        UsagePeriod::Week,
        UsagePeriod::ThirtyDays,
        UsagePeriod::Month,
    ] {
        commands.push(CommandBody::GetUsageSummary { period });
    }
    for project in &machine.projects {
        commands.push(CommandBody::GetProjectIcon {
            project_id: project.project_id.clone(),
        });
    }
    let mut answers = answer(client, &machine, commands).await;
    let bots = answers.iter().find_map(|answer| match &answer.result {
        CommandResult::Bots { bots } => Some(bots.clone()),
        _ => None,
    });
    let memories = bots
        .unwrap_or_default()
        .into_iter()
        .map(|bot| CommandBody::GetBotMemory { bot_id: bot.bot_id })
        .collect();
    answers.extend(answer(client, &machine, memories).await);
    // The latest machine, which the commands may have refreshed.
    let machine = client
        .machines()
        .into_iter()
        .find(|m| m.host_id == host)
        .context("the machine went away")?;
    Ok(DemoMachine {
        machine,
        logs,
        answers,
    })
}

/// The answers to `commands` that the machine gave in time.
async fn answer(client: &Client, machine: &Machine, commands: Vec<CommandBody>) -> Vec<DemoAnswer> {
    let mut answers = Vec::new();
    for command in commands {
        let sent = client.send(machine.host_id.clone(), command.clone());
        if let Ok(Ok(result)) = tokio::time::timeout(ANSWER_TIMEOUT, sent).await {
            answers.push(DemoAnswer { command, result });
        }
    }
    answers
}

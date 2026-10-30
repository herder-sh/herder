//! A recorded fleet that [`crate::Client::open_demo`] shows without connecting anywhere.

use herder_protocol::{CommandBody, CommandResult, Event};
use serde::{Deserialize, Serialize};

use crate::{Error, Machine};

/// A fleet as a client saw it, recorded for [`crate::Client::open_demo`]: each machine with
/// the events of its sessions and the daemon's answers to the commands that only read.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Demo {
    /// The machines, in pairing order.
    pub machines: Vec<DemoMachine>,
}

/// A machine of a [`Demo`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DemoMachine {
    /// The machine as [`crate::Client::machines`] listed it.
    pub machine: Machine,
    /// Each session's events, in seq order from its first.
    pub logs: Vec<Vec<Event>>,
    /// What the daemon answered to commands that only read, such as `list_people`.
    pub answers: Vec<DemoAnswer>,
}

/// A command of a [`DemoMachine`] and the daemon's answer to it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DemoAnswer {
    /// The command as sent.
    pub command: CommandBody,
    /// What the daemon answered.
    pub result: CommandResult,
}

/// The recorded answer to `command`, else [`Error::Demo`].
pub(crate) fn answer(
    answers: &[DemoAnswer],
    command: &CommandBody,
) -> Result<CommandResult, Error> {
    answers
        .iter()
        .find(|answer| answer.command == *command)
        .map(|answer| answer.result.clone())
        .ok_or(Error::Demo)
}

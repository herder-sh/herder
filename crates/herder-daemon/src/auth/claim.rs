//! Moving a daemon's users to people: the one-time claim an owner runs from their device on
//! every machine and vault they own.
//!
//! The claiming owner's user becomes the person: it takes the person's id, its devices and role
//! stay, and a record maps the old id to the person, with the ids the person had on the other
//! machines claimed with it. A vault holds the sessions its hosts replicated, attributed to the
//! hosts' users, so it needs those too. Every id moved resolves to the person
//! ([`Auth::resolve`]), so history attributed to it reads as theirs: events and queued prompts
//! as clients get them ([`Auth::attribute`]), bot people linked to it, and the agents of forked
//! sessions acting for it.
//!
//! Users that are machines, whose every device replicates to this vault, become hosts, listed
//! with the vault's hosts. Any other user is left for the owner to decide on
//! ([`Auth::leftovers`]): the person themselves ([`Auth::claim_leftover`]), or someone to
//! remove ([`Auth::remove_user`]), whose name still reads in history.

use herder_protocol::{
    DeviceId, ErrorCode, ErrorInfo, Role, ServerMessage, Timestamp, UserId, UserInfo,
};
use serde::{Deserialize, Serialize};
use tracing::info;

use super::{Auth, DeviceRole, State, Users, forbidden, user_info};

/// A user id a claim moved: history attributed to it reads as `person`'s.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Moved {
    /// The old id.
    pub user_id: UserId,
    /// The person it is now.
    pub person: UserId,
    /// When it moved.
    pub at: Timestamp,
}

/// What a claim did on this daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claimed {
    /// Users made hosts, by name.
    pub hosts: Vec<String>,
    /// Users left for the owner to decide on, oldest first.
    pub leftovers: Vec<UserInfo>,
}

impl Auth {
    /// Moves the owner `caller` to the person `person`, with `also`, the ids the person has on
    /// the other machines claimed with this one, and makes hosts of the users whose every device
    /// is a host's or in `replicated`, those this vault's hosts replicate from.
    ///
    /// Claiming again as the same person records more ids. Refused for a member, and for an
    /// owner already moved to another person.
    pub fn claim(
        &self,
        caller: &UserId,
        person: &UserId,
        also: &[UserId],
        replicated: &[DeviceId],
    ) -> Result<Claimed, ErrorInfo> {
        let mut state = self.lock();
        let mut users = state.users.clone();
        let Some(user) = users.find(caller) else {
            return Err(forbidden("only a paired user can claim this daemon"));
        };
        if user.role != Role::Owner {
            return Err(forbidden("only an owner can claim this daemon"));
        }
        let current = user.user_id.clone();
        if is_person(&users, &current) && &current != person {
            return Err(ErrorInfo {
                code: ErrorCode::Conflict,
                message: "this daemon's owner is already someone's; sign in with their passkey"
                    .to_owned(),
            });
        }
        let now = Timestamp::now();
        rename(&mut users, &current, person, now);
        for old in also {
            let foreign = !users.users.iter().any(|user| &user.user_id == old);
            let known = users.moved.iter().any(|moved| &moved.user_id == old);
            if foreign && !known && old != person {
                users.moved.push(Moved {
                    user_id: old.clone(),
                    person: person.clone(),
                    at: now,
                });
            }
        }
        let hosts = machines(&users, replicated);
        for user in &mut users.users {
            if hosts.contains(&user.user_id) {
                user.host = true;
            }
        }
        self.store(&mut state, users, "the claim")?;
        let hosts = state
            .users
            .users
            .iter()
            .filter(|user| hosts.contains(&user.user_id))
            .map(|user| user.name.clone())
            .collect();
        info!(%person, ?hosts, "daemon claimed");
        Ok(Claimed {
            hosts,
            leftovers: leftovers(&state),
        })
    }

    /// The users a claim left for an owner to decide on, oldest first: neither a person nor a
    /// host. None before the first claim.
    pub fn leftovers(&self) -> Vec<UserInfo> {
        leftovers(&self.lock())
    }

    /// Decides the leftover `user_id` is the person `person`, who claimed this daemon: their
    /// devices join the person's, and history attributed to them reads as the person's.
    pub fn claim_leftover(&self, user_id: &UserId, person: &UserId) -> Result<(), ErrorInfo> {
        let mut state = self.lock();
        let mut users = state.users.clone();
        leftover(&users, user_id)?;
        if !users.users.iter().any(|user| &user.user_id == person) {
            return Err(ErrorInfo {
                code: ErrorCode::NotFound,
                message: format!("{person} has not claimed this daemon"),
            });
        }
        let theirs = devices_of(&users, user_id);
        users.users.retain(|user| &user.user_id != user_id);
        retarget(&mut users, user_id, person, Timestamp::now());
        self.store(&mut state, users, "the user's person")?;
        // Their connections carry their old role; they connect again with the person's.
        disconnect(&mut state, &theirs);
        info!(%user_id, %person, "user moved to a person");
        Ok(())
    }

    /// Removes the leftover `user_id` with their devices, closing their connections, as an
    /// owner decided: someone else, invited back as a person, or no one.
    pub fn remove_user(&self, user_id: &UserId) -> Result<(), ErrorInfo> {
        let mut state = self.lock();
        let mut users = state.users.clone();
        leftover(&users, user_id)?;
        users.users.retain(|user| &user.user_id != user_id);
        let gone = devices_of(&users, user_id);
        users.devices.retain(|device| &device.user_id != user_id);
        self.store(&mut state, users, "the removed user")?;
        disconnect(&mut state, &gone);
        info!(%user_id, "user removed");
        Ok(())
    }

    /// Attributes what `message` names a moved id as to its person, as clients read it.
    pub fn attribute(&self, message: &mut ServerMessage) {
        let state = self.lock();
        if state.users.moved.is_empty() {
            return;
        }
        let resolve = |by: &mut Option<UserId>| {
            if let Some(user_id) = by {
                *user_id = state.users.resolve(user_id).clone();
            }
        };
        match message {
            ServerMessage::Event(event) => resolve(&mut event.by),
            ServerMessage::Sessions { sessions } => sessions
                .iter_mut()
                .flat_map(|session| &mut session.queue)
                .for_each(|prompt| resolve(&mut prompt.by)),
            _ => {}
        }
    }

    fn store(&self, state: &mut State, users: Users, what: &str) -> Result<(), ErrorInfo> {
        self.save(&users).map_err(|err| {
            tracing::error!("cannot save {what}: {err:#}");
            ErrorInfo {
                code: ErrorCode::Internal,
                message: format!("cannot save {what}"),
            }
        })?;
        state.users = users;
        Ok(())
    }
}

fn devices_of(users: &Users, user_id: &UserId) -> Vec<DeviceId> {
    users
        .devices
        .iter()
        .filter(|device| &device.user_id == user_id)
        .map(|device| device.device_id.clone())
        .collect()
}

/// Closes the connections of `devices`.
fn disconnect(state: &mut State, devices: &[DeviceId]) {
    for device_id in devices {
        for (_, token) in state.connections.remove(device_id).unwrap_or_default() {
            token.cancel();
        }
    }
}

/// Whether `user_id` is a person, which a claim moved an id to.
fn is_person(users: &Users, user_id: &UserId) -> bool {
    users.moved.iter().any(|moved| &moved.person == user_id)
}

/// Gives the user `old` the id `person`, with their devices, recording the move.
fn rename(users: &mut Users, old: &UserId, person: &UserId, at: Timestamp) {
    if old == person {
        return;
    }
    for user in &mut users.users {
        if &user.user_id == old {
            user.user_id = person.clone();
        }
    }
    retarget(users, old, person, at);
}

/// Moves `old`'s devices, and the ids that moved to `old`, to `person`, recording the move.
fn retarget(users: &mut Users, old: &UserId, person: &UserId, at: Timestamp) {
    for device in &mut users.devices {
        if &device.user_id == old {
            device.user_id = person.clone();
        }
    }
    for moved in &mut users.moved {
        if &moved.person == old {
            moved.person = person.clone();
        }
    }
    users.moved.push(Moved {
        user_id: old.clone(),
        person: person.clone(),
        at,
    });
}

/// The users other than people whose devices are all hosts': host-only, or replicating to this
/// vault. A user with no devices is no machine.
fn machines(users: &Users, replicated: &[DeviceId]) -> Vec<UserId> {
    users
        .users
        .iter()
        .filter(|user| !is_person(users, &user.user_id))
        .filter(|user| {
            let mut devices = users
                .devices
                .iter()
                .filter(|device| device.user_id == user.user_id)
                .peekable();
            devices.peek().is_some()
                && devices.all(|device| {
                    device.role == DeviceRole::Host || replicated.contains(&device.device_id)
                })
        })
        .map(|user| user.user_id.clone())
        .collect()
}

fn leftovers(state: &State) -> Vec<UserInfo> {
    let users = &state.users;
    if users.moved.is_empty() {
        return Vec::new();
    }
    let mut left: Vec<_> = users
        .users
        .iter()
        .filter(|user| !user.host && !is_person(users, &user.user_id))
        .collect();
    left.sort_by_key(|user| user.created_at);
    left.into_iter()
        .map(|user| user_info(state, user))
        .collect()
}

/// Refuses `user_id` unless it is a leftover.
fn leftover(users: &Users, user_id: &UserId) -> Result<(), ErrorInfo> {
    match users.users.iter().find(|user| &user.user_id == user_id) {
        None => Err(ErrorInfo {
            code: ErrorCode::NotFound,
            message: format!("no user {user_id} on this daemon"),
        }),
        Some(user) if user.host || is_person(users, user_id) => Err(ErrorInfo {
            code: ErrorCode::Conflict,
            message: format!("{} is not left to decide on", user.name),
        }),
        Some(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests;

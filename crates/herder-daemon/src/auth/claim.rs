//! Moving a daemon's users to people: the one-time claim an owner runs from their device on
//! every machine and vault they own.
//!
//! The claiming owner's user becomes the person: the person joins this daemon as an owner, the
//! user's devices are listed under them, and a record maps the old id to the person, with the
//! ids the person had on the other machines claimed with this one. A vault holds the sessions
//! its hosts replicated, attributed to the hosts' users, so it needs those too. Every id moved
//! resolves to the person ([`Auth::resolve`]), so history attributed to it reads as theirs:
//! events and queued prompts as clients get them ([`Auth::attribute`]), bot people linked to
//! it, and the agents of forked sessions acting for it.
//!
//! Users that are machines, whose every device replicates to this vault, become hosts, listed
//! with the vault's hosts. Any other user is left for the owner to decide on
//! ([`Auth::leftovers`]): the person themselves ([`Auth::claim_leftover`]), or someone to
//! remove ([`Auth::remove_user`]).

use super::passkeys::Key;
use herder_protocol::{
    DeviceId, ErrorInfo, PersonId, Role, ServerMessage, Timestamp, UserId, UserInfo,
};
use serde::{Deserialize, Serialize};
use tracing::info;

use super::{
    Auth, DeviceRole, Person, ProvenPerson, State, Users, conflict, devices_of, disconnect,
    forbidden, not_found, saving,
};

/// A user id a claim moved: history attributed to it reads as `person`'s.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Moved {
    /// The old id.
    pub user_id: UserId,
    /// The person it is now, by their user id.
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
    /// Moves the owner `caller` to the person `proven`, with `also`, the ids the person has on
    /// the other machines claimed with this one, and makes hosts of the users whose every
    /// device is a host's or in `replicated`, those this vault's hosts replicate from.
    ///
    /// Claiming again as the same person records more ids. Refused for a member, and for an
    /// owner already moved to another person.
    pub fn claim(
        &self,
        caller: &UserId,
        proven: ProvenPerson,
        also: &[UserId],
        replicated: &[DeviceId],
    ) -> Result<Claimed, ErrorInfo> {
        let ProvenPerson {
            person_id,
            name,
            passkey,
        } = proven;
        self.claim_as(caller, &person_id, name, vec![passkey], also, replicated)
    }

    /// [`Auth::claim`], for a person holding `passkeys`.
    pub(crate) fn claim_as(
        &self,
        caller: &UserId,
        person_id: &PersonId,
        name: String,
        passkeys: Vec<Key>,
        also: &[UserId],
        replicated: &[DeviceId],
    ) -> Result<Claimed, ErrorInfo> {
        let mut state = self.lock();
        let mut users = state.users.clone();
        let person = UserId::new(person_id.as_str());
        match users.member(caller) {
            Some((Role::Owner, None)) => {}
            Some((Role::Owner, Some(current))) if &current == person_id => {}
            Some((Role::Owner, Some(_))) => {
                return Err(conflict(
                    "this daemon's owner is someone's already; sign in with their passkey",
                ));
            }
            Some((Role::Member, _)) => {
                return Err(forbidden("only an owner can claim this daemon"));
            }
            None => return Err(forbidden("only a paired owner can claim this daemon")),
        }
        let now = Timestamp::now();
        if let Some(index) = users.users.iter().position(|user| &user.user_id == caller) {
            let user = users.users.remove(index);
            match users.people.iter_mut().find(|p| &p.person_id == person_id) {
                Some(existing) => existing.passkeys.extend(passkeys),
                None => users.people.push(Person {
                    person_id: person_id.clone(),
                    name,
                    role: Role::Owner,
                    passkeys,
                    created_at: user.created_at,
                }),
            }
            retarget(&mut users, caller, &person, now);
        }
        for old in also {
            let foreign = users.member(old).is_none();
            if foreign && old != &person {
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
        self.save(&users).map_err(saving)?;
        state.users = users;
        let hosts = state
            .users
            .users
            .iter()
            .filter(|user| hosts.contains(&user.user_id))
            .map(|user| user.name.clone())
            .collect();
        info!(%person_id, ?hosts, "daemon claimed");
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

    /// Decides the leftover `user_id` is the person `person_id`: their devices join the
    /// person's, and history attributed to them reads as the person's.
    pub fn claim_leftover(&self, user_id: &UserId, person_id: &PersonId) -> Result<(), ErrorInfo> {
        let mut state = self.lock();
        let mut users = state.users.clone();
        leftover(&users, user_id)?;
        if users.person(person_id).is_none() {
            return Err(not_found(&format!("{person_id} is not on this daemon")));
        }
        users.users.retain(|user| &user.user_id != user_id);
        // Their connections carry their old role; they connect again with the person's.
        disconnect(&mut state, user_id);
        retarget(
            &mut users,
            user_id,
            &UserId::new(person_id.as_str()),
            Timestamp::now(),
        );
        self.save(&users).map_err(saving)?;
        state.users = users;
        info!(%user_id, %person_id, "user moved to a person");
        Ok(())
    }

    /// Removes the leftover `user_id` with their devices, closing their connections.
    pub fn remove_user(&self, user_id: &UserId) -> Result<(), ErrorInfo> {
        let mut state = self.lock();
        let mut users = state.users.clone();
        leftover(&users, user_id)?;
        users.users.retain(|user| &user.user_id != user_id);
        users.devices.retain(|device| &device.user_id != user_id);
        disconnect(&mut state, user_id);
        self.save(&users).map_err(saving)?;
        state.users = users;
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

/// The users whose devices are all hosts': host-only, or replicating to this vault. A user with
/// no devices is no machine.
fn machines(users: &Users, replicated: &[DeviceId]) -> Vec<UserId> {
    users
        .users
        .iter()
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
    if state.users.moved.is_empty() {
        return Vec::new();
    }
    let mut left: Vec<_> = state.users.users.iter().filter(|user| !user.host).collect();
    left.sort_by_key(|user| user.created_at);
    left.into_iter()
        .map(|user| UserInfo {
            user_id: user.user_id.clone(),
            name: user.name.clone(),
            role: user.role,
            devices: devices_of(state, &user.user_id),
        })
        .collect()
}

/// Refuses `user_id` unless it is a leftover.
fn leftover(users: &Users, user_id: &UserId) -> Result<(), ErrorInfo> {
    match users.users.iter().find(|user| &user.user_id == user_id) {
        Some(user) if !user.host && !users.moved.is_empty() => Ok(()),
        _ => Err(not_found(&format!(
            "{user_id} is not left over on this daemon"
        ))),
    }
}

#[cfg(test)]
mod tests;

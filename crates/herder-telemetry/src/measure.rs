//! Anonymous measurements to PostHog (EU): how connections, commands, replays and turns go,
//! as numbers, flags and the few words herder records itself, from a random install id.
//!
//! Sent only while diagnostics are on with a PostHog token; off drops everything not yet sent
//! and sends nothing more. Every event carries, besides its own properties, `component`,
//! `herder_version` and `arch`, and PostHog's SDK adds `$os` and `$os_version`; GeoIP is off and
//! no person profile is made. The events and their properties are listed in
//! `docs/diagnostics.md`.

use std::sync::{Arc, PoisonError, RwLock};

use crate::{Component, Telemetry, scrub};

/// PostHog's EU ingestion host.
pub const POSTHOG_HOST: &str = "https://eu.i.posthog.com";

/// One property of a measurement.
#[derive(Clone, Debug, PartialEq)]
pub enum Measure {
    /// A count or a duration, in the unit its name says.
    Number(u64),
    /// Yes or no.
    Flag(bool),
    /// One of the words herder records itself, such as a route kind or a command; anything
    /// else is left out ([`scrub::is_word`]).
    Word(String),
}

impl From<u64> for Measure {
    fn from(number: u64) -> Self {
        Self::Number(number)
    }
}

impl From<u32> for Measure {
    fn from(number: u32) -> Self {
        Self::Number(number.into())
    }
}

impl From<usize> for Measure {
    fn from(number: usize) -> Self {
        Self::Number(u64::try_from(number).unwrap_or(u64::MAX))
    }
}

impl From<std::time::Duration> for Measure {
    /// In milliseconds.
    fn from(duration: std::time::Duration) -> Self {
        Self::Number(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
    }
}

impl From<bool> for Measure {
    fn from(flag: bool) -> Self {
        Self::Flag(flag)
    }
}

impl From<&str> for Measure {
    fn from(word: &str) -> Self {
        Self::Word(word.to_owned())
    }
}

impl From<String> for Measure {
    fn from(word: String) -> Self {
        Self::Word(word)
    }
}

/// The PostHog client while diagnostics are on with a token.
struct Measurer {
    client: Arc<posthog::Client>,
    install_id: String,
    component: Component,
}

static MEASURER: RwLock<Option<Measurer>> = RwLock::new(None);

/// Starts measuring as `telemetry` says, sending to `host`, or stops, with `None`.
pub(crate) fn restart(telemetry: Option<&Telemetry>, host: &str) {
    let mut measurer = MEASURER.write().unwrap_or_else(PoisonError::into_inner);
    if let Some(old) = measurer.take() {
        // Stopping joins its worker, which may wait on the network: not on the caller,
        // which may be an app's main thread.
        std::thread::spawn(move || old.client.shutdown());
    }
    let Some(telemetry) = telemetry else { return };
    let Some(token) = telemetry
        .posthog_token
        .as_deref()
        .filter(|token| !token.is_empty())
    else {
        return;
    };
    // PostHog's HTTP client takes the process's TLS provider; herder's is ring. One installed
    // already, ring or not, is left as it is.
    let _ = rustls::crypto::ring::default_provider().install_default();
    *measurer = Some(Measurer {
        client: Arc::new(posthog::client((token, host))),
        install_id: telemetry.install_id.clone(),
        component: telemetry.component,
    });
}

/// Records `event` with `properties`, while diagnostics are on with PostHog: `event` and
/// each property's name are fixed names of herder's own.
pub fn measure(event: &'static str, properties: &[(&'static str, Measure)]) {
    let measurer = MEASURER.read().unwrap_or_else(PoisonError::into_inner);
    let Some(measurer) = &*measurer else { return };
    if let Some(event) = build(event, properties, &measurer.install_id, measurer.component) {
        measurer.client.capture(event);
    }
}

/// Sends what was recorded so far, waiting for it: for tests.
pub fn flush_measurements() {
    let client = MEASURER
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .map(|measurer| Arc::clone(&measurer.client));
    if let Some(client) = client {
        client.flush();
    }
}

/// The PostHog event `event` with what of `properties` may go.
pub(crate) fn build(
    event: &'static str,
    properties: &[(&'static str, Measure)],
    install_id: &str,
    component: Component,
) -> Option<posthog::Event> {
    if !scrub::is_name(event) {
        return None;
    }
    let mut out = posthog::Event::new(event, install_id);
    let common = [
        ("component", Measure::Word(component.name().to_owned())),
        ("arch", Measure::Word(std::env::consts::ARCH.to_owned())),
    ];
    for (key, value) in properties.iter().chain(&common) {
        if !scrub::is_name(key) {
            continue;
        }
        let inserted = match value {
            Measure::Number(number) => out.insert_prop(*key, number),
            Measure::Flag(flag) => out.insert_prop(*key, flag),
            Measure::Word(word) if scrub::is_word(word) => out.insert_prop(*key, word),
            Measure::Word(_) => continue,
        };
        // A number, a flag or a word always serializes.
        drop(inserted);
    }
    // No place looked up from the sender's address, and no person profile: an install is
    // only counted.
    drop(out.insert_prop("$geoip_disable", true));
    drop(out.insert_prop("$process_person_profile", false));
    // A version is no word: its dots and digits are herder's own, so it goes as it is.
    drop(out.insert_prop("herder_version", env!("CARGO_PKG_VERSION")));
    Some(out)
}

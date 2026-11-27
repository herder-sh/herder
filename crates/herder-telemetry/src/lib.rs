//! Sharing diagnostics, for the processes whose person turned it on: herder's own `tracing`
//! spans as Sentry traces, and its errors and panics, scrubbed of anything that could name a
//! person, a machine or their work.
//!
//! Off until [`set`] turns it on, and off again at once with `set(None)`: an envelope not yet
//! handed to the network is dropped. Every envelope goes through [`scrub`] on its way out,
//! which keeps only what herder's privacy page lists: span and event names, numbers, route
//! kinds, command kinds, error types, versions, the OS and device model, and a random install
//! id. Nothing a scrub cannot vouch for leaves.
//!
//! Traces are sampled at the tail: every operation is recorded, and a trace is sent when it
//! is one of the [`BASE_RATE`] picked by its trace id, the same pick in every process it
//! crosses, or when an operation in it took [`SLOW`] or longer.
//!
//! [`layer`] turns the `tracing` spans of herder's crates into Sentry spans; a process that
//! shares diagnostics installs it in its subscriber. [`traceparent`] is the W3C trace context
//! of the span it runs in, for a command to carry; [`sentry_trace`] turns a received one into
//! what a span continues the trace from (`sentry.trace`).

mod measure;
pub mod scrub;

pub use measure::{Measure, POSTHOG_HOST, flush_measurements, measure};

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use sentry::{ClientOptions, Envelope, Hub, Transport, TransportFactory, TransportOptions};
use tracing::{Level, Metadata, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::registry::LookupSpan;

/// The share of traces sent whatever their length.
pub const BASE_RATE: f64 = 0.05;

/// An operation that takes this long or longer sends its trace.
pub const SLOW: Duration = Duration::from_secs(1);

/// What shares diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Component {
    /// A Mac or iPhone app, through the client core.
    App,
    /// A daemon or a hub.
    Daemon,
}

impl Component {
    /// Its name, as diagnostics carry it.
    pub fn name(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Daemon => "daemon",
        }
    }
}

/// What a process shares diagnostics with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Telemetry {
    /// A random id made once for this install; it names nothing else.
    pub install_id: String,
    /// The Sentry DSN traces and errors go to; none goes to Sentry without one.
    pub sentry_dsn: Option<String>,
    /// The PostHog project token measurements go to.
    pub posthog_token: Option<String>,
    /// What shares them.
    pub component: Component,
}

/// The Sentry client while diagnostics are on, and whether its transport still sends.
struct Running {
    client: Arc<sentry::Client>,
    open: Arc<AtomicBool>,
}

static RUNNING: RwLock<Option<Running>> = RwLock::new(None);

/// Goes up whenever diagnostics turn on or off, so each thread binds the new client once.
static GENERATION: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The [`GENERATION`] this thread's Sentry hub was last bound for.
    static BOUND: Cell<u64> = const { Cell::new(0) };
}

/// Where diagnostics go: Sentry through its HTTP transport and PostHog's EU host, keeping
/// [`BASE_RATE`] of the traces; tests point them at fakes, never at the real services.
pub struct Endpoints {
    /// What sends to Sentry; its HTTP transport when absent.
    pub sentry: Option<Arc<dyn TransportFactory>>,
    /// Where measurements go.
    pub posthog_host: String,
    /// The share of traces sent whatever their length.
    pub base_rate: f64,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            sentry: None,
            posthog_host: POSTHOG_HOST.to_owned(),
            base_rate: BASE_RATE,
        }
    }
}

/// Turns sharing diagnostics on, with `Some`, or off, with `None`, for the whole process.
pub fn set(telemetry: Option<Telemetry>) {
    set_with(telemetry, Endpoints::default());
}

/// [`set`], sending to `endpoints`.
pub fn set_with(telemetry: Option<Telemetry>, endpoints: Endpoints) {
    measure::restart(telemetry.as_ref(), &endpoints.posthog_host);
    start(telemetry, endpoints.sentry, endpoints.base_rate);
    // Counts the installs that share, as each turns it on or starts with it on.
    measure("diagnostics_on", &[]);
}

/// Whether the process shares diagnostics with Sentry now.
pub fn is_on() -> bool {
    read().is_some()
}

fn read() -> std::sync::RwLockReadGuard<'static, Option<Running>> {
    // A writer that panicked left a whole value behind.
    RUNNING.read().unwrap_or_else(PoisonError::into_inner)
}

fn start(telemetry: Option<Telemetry>, transport: Option<Arc<dyn TransportFactory>>, rate: f64) {
    let mut running = RUNNING.write().unwrap_or_else(PoisonError::into_inner);
    if let Some(old) = running.take() {
        old.open.store(false, Ordering::SeqCst);
        Hub::main().bind_client(None);
        // Nothing waits for what was queued: off means off now.
        old.client.close(Some(Duration::ZERO));
    }
    *running = telemetry.and_then(|telemetry| client(telemetry, transport, rate));
    if let Some(running) = &*running {
        Hub::main().bind_client(Some(Arc::clone(&running.client)));
    }
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// The Sentry client `telemetry` sends with, if it names a DSN that parses.
fn client(
    telemetry: Telemetry,
    transport: Option<Arc<dyn TransportFactory>>,
    rate: f64,
) -> Option<Running> {
    let dsn = telemetry.sentry_dsn.as_deref()?.parse().ok()?;
    let open = Arc::new(AtomicBool::new(true));
    let inner = transport.unwrap_or_else(|| Arc::new(sentry::transports::DefaultTransportFactory));
    let gate = Gate {
        inner,
        open: Arc::clone(&open),
        scrub: scrub::Scrub {
            install_id: telemetry.install_id,
            component: telemetry.component,
            base_rate: rate,
        },
    };
    // Every operation is recorded; the gate picks what is sent, at the tail.
    let mut options = ClientOptions::new().traces_sample_rate(1.0).transport(gate);
    options.dsn = Some(dsn);
    options.release = Some(format!("herder@{}", env!("CARGO_PKG_VERSION")).into());
    options.environment = Some(telemetry.component.name().into());
    options.send_default_pii = false;
    options.attach_stacktrace = false;
    options.auto_session_tracking = false;
    options.max_breadcrumbs = 0;
    let client = Arc::new(sentry::Client::from(sentry::apply_defaults(options)));
    Some(Running { client, open })
}

/// Sends only while diagnostics are on, and only what [`scrub`] keeps.
struct Gate {
    inner: Arc<dyn TransportFactory>,
    open: Arc<AtomicBool>,
    scrub: scrub::Scrub,
}

impl TransportFactory for Gate {
    fn create_transport_with_options(&self, options: TransportOptions) -> Arc<dyn Transport> {
        Arc::new(Gated {
            inner: self.inner.create_transport_with_options(options),
            open: Arc::clone(&self.open),
            scrub: self.scrub.clone(),
        })
    }
}

struct Gated {
    inner: Arc<dyn Transport>,
    open: Arc<AtomicBool>,
    scrub: scrub::Scrub,
}

impl Transport for Gated {
    fn send_envelope(&self, envelope: Envelope) {
        if !self.open.load(Ordering::SeqCst) {
            return;
        }
        if let Some(envelope) = self.scrub.envelope(envelope) {
            self.inner.send_envelope(envelope);
        }
    }

    fn flush(&self, timeout: Duration) -> bool {
        self.inner.flush(timeout)
    }

    fn shutdown(&self, timeout: Duration) -> bool {
        self.inner.shutdown(timeout)
    }
}

/// Binds this thread's Sentry hub to the client in effect, once per change: a thread's hub
/// copies the process's only when the thread first uses it, and diagnostics turn on and off
/// long after the runtime's threads started.
fn bind_thread() {
    let generation = GENERATION.load(Ordering::SeqCst);
    if BOUND.get() == generation {
        return;
    }
    BOUND.set(generation);
    let client = read().as_ref().map(|running| Arc::clone(&running.client));
    Hub::current().bind_client(client);
}

/// Whether `metadata` is herder's own, at info or above: what goes to Sentry.
fn herder_info(metadata: &Metadata<'_>) -> bool {
    metadata.target().starts_with("herder") && *metadata.level() <= Level::INFO
}

/// The layer that turns herder's spans, at info or above, into Sentry spans and its errors
/// into Sentry events, while diagnostics are on.
pub fn layer<S>() -> impl Layer<S> + Send + Sync + 'static
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    BindThread
        .with_filter(filter_fn(herder_info))
        .and_then(
            sentry_tracing::SpanLayer::new().with_filter(filter_fn(|metadata| {
                metadata.is_span() && herder_info(metadata)
            })),
        )
        .and_then(
            sentry_tracing::ErrorLayer::new().with_filter(filter_fn(|metadata| {
                metadata.is_event() && herder_info(metadata) && *metadata.level() == Level::ERROR
            })),
        )
}

/// Runs [`bind_thread`] ahead of the Sentry layers.
struct BindThread;

impl<S: Subscriber> Layer<S> for BindThread {
    fn on_new_span(
        &self,
        _: &tracing::span::Attributes<'_>,
        _: &tracing::span::Id,
        _: tracing_subscriber::layer::Context<'_, S>,
    ) {
        bind_thread();
    }

    fn on_event(&self, _: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        bind_thread();
    }

    fn on_enter(&self, _: &tracing::span::Id, _: tracing_subscriber::layer::Context<'_, S>) {
        bind_thread();
    }
}

/// The W3C `traceparent` of the Sentry span the caller runs in, while diagnostics are on.
pub fn traceparent() -> Option<String> {
    bind_thread();
    let hub = Hub::current();
    hub.client()?;
    let span = hub.configure_scope(|scope| scope.get_span())?;
    let context = span.get_trace_context();
    // Every operation is recorded, and sent or not at the tail.
    Some(format!("00-{}-{}-01", context.trace_id, context.span_id))
}

/// The `sentry-trace` header a span continues `traceparent`'s trace from, if it is a W3C
/// trace context. Always sampled: whether the trace is sent is decided at the tail, alike in
/// every process ([`BASE_RATE`], [`SLOW`]).
pub fn sentry_trace(traceparent: &str) -> Option<String> {
    let mut parts = traceparent.trim().split('-');
    let (version, trace_id, span_id, flags) =
        (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let hex =
        |part: &str, len: usize| part.len() == len && part.bytes().all(|b| b.is_ascii_hexdigit());
    let zero = |part: &str| part.bytes().all(|b| b == b'0');
    let valid = parts.next().is_none()
        && hex(version, 2)
        && version != "ff"
        && hex(trace_id, 32)
        && !zero(trace_id)
        && hex(span_id, 16)
        && !zero(span_id)
        && hex(flags, 2);
    if !valid {
        return None;
    }
    Some(format!(
        "{}-{}-1",
        trace_id.to_ascii_lowercase(),
        span_id.to_ascii_lowercase()
    ))
}

#[cfg(test)]
mod tests;

/// The name of `value`'s enum variant, in snake case, such as `send_prompt` for a
/// `CommandBody::SendPrompt { .. }`: what a span records of a command, never its contents.
pub fn variant(value: &impl std::fmt::Debug) -> String {
    use std::fmt::Write;

    /// Takes the leading name `Debug` writes, and stops it there.
    struct Name(String);

    impl Write for Name {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            for c in text.chars() {
                if !c.is_ascii_alphanumeric() {
                    return Err(std::fmt::Error);
                }
                if c.is_ascii_uppercase() && !self.0.is_empty() {
                    self.0.push('_');
                }
                self.0.push(c.to_ascii_lowercase());
            }
            Ok(())
        }
    }

    let mut name = Name(String::new());
    // Stopped on purpose at the first character after the name.
    let _ = write!(name, "{value:?}");
    name.0
}

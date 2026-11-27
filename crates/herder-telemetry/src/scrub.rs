//! What leaves the process: every envelope is rebuilt from what may go, never stripped of
//! what may not, so a field Sentry's SDK adds later stays home too.
//!
//! An event keeps its level, time, release, environment, the type of each error and the
//! functions and lines of its stack traces, the module that logged it, the OS, runtime and
//! device model, and the install id. A transaction keeps the names, operations, times and
//! statuses of its spans, and of their data only numbers, booleans, and the few words herder
//! records itself ([`WORD_KEYS`]). Anything else, messages, error values, file names, paths,
//! hosts, addresses, emails, keys, tokens and every other free text, is dropped, as are
//! breadcrumbs, request data, server names and session updates.

use sentry::Envelope;
use sentry::protocol::{
    Context, DeviceContext, EnvelopeItem, Event, Exception, Frame, Map, Mechanism, OsContext,
    RuntimeContext, Span, Stacktrace, Thread, TraceContext, TraceId, Transaction, User, Value,
    Values,
};

use crate::{Component, SLOW};

/// Span data keys whose words are kept: what herder's spans record about themselves.
pub const WORD_KEYS: &[&str] = &["route", "command", "outcome", "error", "kind"];

/// How one process scrubs what it sends.
#[derive(Clone, Debug)]
pub struct Scrub {
    /// The install id every item carries, as its user.
    pub install_id: String,
    /// What sends.
    pub component: Component,
    /// The share of traces sent whatever their length.
    pub base_rate: f64,
}

impl Scrub {
    /// What of `envelope` may leave; `None` when nothing may, as for a fast trace that was
    /// not picked.
    pub fn envelope(&self, envelope: Envelope) -> Option<Envelope> {
        let mut out = Envelope::new().with_headers(envelope.headers().clone());
        let mut any = false;
        for item in envelope.into_items() {
            let item = match item {
                EnvelopeItem::Event(event) => EnvelopeItem::Event(Box::new(self.event(&event))),
                EnvelopeItem::Transaction(transaction) if self.keeps(&transaction) => {
                    EnvelopeItem::Transaction(Box::new(self.transaction(&transaction)))
                }
                _ => continue,
            };
            out.add_item(item);
            any = true;
        }
        any.then_some(out)
    }

    /// Whether a trace's transaction is sent: picked by its trace id, or slow.
    pub fn keeps(&self, transaction: &Transaction<'_>) -> bool {
        let slow = |start: std::time::SystemTime, end: Option<std::time::SystemTime>| {
            end.and_then(|end| end.duration_since(start).ok())
                .is_some_and(|took| took >= SLOW)
        };
        let trace_id = transaction
            .contexts
            .values()
            .find_map(|context| match context {
                Context::Trace(trace) => Some(trace.trace_id),
                _ => None,
            });
        trace_id.is_some_and(|trace_id| picked(trace_id, self.base_rate))
            || slow(transaction.start_timestamp, transaction.timestamp)
            || transaction
                .spans
                .iter()
                .any(|span| slow(span.start_timestamp, span.timestamp))
    }

    fn user(&self) -> Option<User> {
        Some(User {
            id: Some(self.install_id.clone()),
            ..Default::default()
        })
    }

    fn tags(&self) -> Map<String, String> {
        Map::from([("component".to_owned(), self.component.name().to_owned())])
    }

    /// An event rebuilt from what may go.
    pub fn event(&self, event: &Event<'static>) -> Event<'static> {
        let logger = event.logger.clone().filter(|logger| is_module(logger));
        Event {
            event_id: event.event_id,
            level: event.level,
            timestamp: event.timestamp,
            platform: event.platform.clone(),
            release: event.release.clone(),
            environment: event.environment.clone(),
            sdk: event.sdk.clone(),
            // What logged it is the title, in place of the message.
            message: logger.clone(),
            logger,
            exception: Values {
                values: event.exception.values.iter().map(exception).collect(),
            },
            threads: Values {
                values: event.threads.values.iter().map(thread).collect(),
            },
            stacktrace: event.stacktrace.as_ref().map(stacktrace),
            user: self.user(),
            tags: self.tags(),
            contexts: contexts(&event.contexts),
            ..Default::default()
        }
    }

    /// A transaction rebuilt from what may go.
    pub fn transaction(&self, transaction: &Transaction<'static>) -> Transaction<'static> {
        Transaction {
            event_id: transaction.event_id,
            name: transaction.name.clone().filter(|name| is_name(name)),
            release: transaction.release.clone(),
            environment: transaction.environment.clone(),
            sdk: transaction.sdk.clone(),
            platform: transaction.platform.clone(),
            timestamp: transaction.timestamp,
            start_timestamp: transaction.start_timestamp,
            spans: transaction.spans.iter().map(span).collect(),
            user: self.user(),
            tags: self.tags(),
            contexts: contexts(&transaction.contexts),
            ..Default::default()
        }
    }
}

/// Whether `trace_id` is among the `rate` of traces picked: alike in every process.
pub fn picked(trace_id: TraceId, rate: f64) -> bool {
    let hex = trace_id.to_string();
    let Some(head) = hex
        .get(..8)
        .and_then(|head| u32::from_str_radix(head, 16).ok())
    else {
        return false;
    };
    f64::from(head) < rate * f64::from(u32::MAX)
}

fn span(span: &Span) -> Span {
    Span {
        span_id: span.span_id,
        trace_id: span.trace_id,
        parent_span_id: span.parent_span_id,
        same_process_as_parent: span.same_process_as_parent,
        op: span.op.clone().filter(|op| is_module(op)),
        description: span.description.clone().filter(|name| is_name(name)),
        timestamp: span.timestamp,
        start_timestamp: span.start_timestamp,
        status: span.status,
        data: data(&span.data),
        ..Default::default()
    }
}

/// The contexts that may go: the OS, the runtime, the device's model and the trace.
fn contexts(contexts: &Map<String, Context>) -> Map<String, Context> {
    let mut kept = Map::new();
    for (key, context) in contexts {
        let context = match context {
            Context::Os(os) => Context::Os(Box::new(OsContext {
                name: os.name.clone().filter(|name| is_version(name)),
                version: os.version.clone().filter(|version| is_version(version)),
                ..Default::default()
            })),
            Context::Runtime(runtime) => Context::Runtime(Box::new(RuntimeContext {
                name: runtime.name.clone().filter(|name| is_version(name)),
                version: runtime
                    .version
                    .clone()
                    .filter(|version| is_version(version)),
                ..Default::default()
            })),
            // Never its name: a device's is its owner's, or the machine's host name.
            Context::Device(device) => Context::Device(Box::new(DeviceContext {
                family: device.family.clone().filter(|family| is_version(family)),
                model: device.model.clone().filter(|model| is_version(model)),
                arch: device.arch.clone().filter(|arch| is_version(arch)),
                ..Default::default()
            })),
            Context::Trace(trace) => Context::Trace(Box::new(TraceContext {
                span_id: trace.span_id,
                trace_id: trace.trace_id,
                parent_span_id: trace.parent_span_id,
                op: trace.op.clone().filter(|op| is_module(op)),
                description: trace.description.clone().filter(|name| is_name(name)),
                status: trace.status,
                data: data(&trace.data),
                ..Default::default()
            })),
            _ => continue,
        };
        kept.insert(key.clone(), context);
    }
    kept
}

/// Span data that may go: numbers and booleans, and the words of [`WORD_KEYS`].
fn data(data: &Map<String, Value>) -> Map<String, Value> {
    data.iter()
        .filter(|(key, value)| {
            is_name(key)
                && match value {
                    Value::Number(_) | Value::Bool(_) => true,
                    Value::String(word) => WORD_KEYS.contains(&key.as_str()) && is_word(word),
                    _ => false,
                }
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn exception(exception: &Exception) -> Exception {
    Exception {
        // Its type, never its value: the value is free text, and may name anything.
        ty: if is_type(&exception.ty) {
            exception.ty.clone()
        } else {
            "error".to_owned()
        },
        stacktrace: exception.stacktrace.as_ref().map(stacktrace),
        thread_id: exception.thread_id.clone(),
        mechanism: exception.mechanism.as_ref().map(|mechanism| Mechanism {
            ty: if is_name(&mechanism.ty) {
                mechanism.ty.clone()
            } else {
                "generic".to_owned()
            },
            handled: mechanism.handled,
            synthetic: mechanism.synthetic,
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn thread(thread: &Thread) -> Thread {
    Thread {
        id: thread.id.clone(),
        stacktrace: thread.stacktrace.as_ref().map(stacktrace),
        crashed: thread.crashed,
        current: thread.current,
        ..Default::default()
    }
}

/// A stack trace's functions and lines, without file names, paths, source or variables.
fn stacktrace(stacktrace: &Stacktrace) -> Stacktrace {
    Stacktrace {
        frames: stacktrace
            .frames
            .iter()
            .map(|frame| Frame {
                function: frame.function.clone().filter(|name| is_symbol(name)),
                module: frame.module.clone().filter(|module| is_module(module)),
                lineno: frame.lineno,
                colno: frame.colno,
                in_app: frame.in_app,
                instruction_addr: frame.instruction_addr,
                ..Default::default()
            })
            .collect(),
        frames_omitted: stacktrace.frames_omitted,
        ..Default::default()
    }
}

/// A word herder records itself, such as a route kind or a command: lowercase letters,
/// digits and underscores, starting with a letter, short parts, few digits. A host, an
/// address, a path, an email, a branch, a key or a sentence is none.
pub fn is_word(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && text.len() <= 40
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && text.split('_').all(|part| part.len() <= 16)
        && text.bytes().filter(u8::is_ascii_digit).count() <= 4
}

/// A span's or transaction's name, as herder's spans have them: words joined by dots.
pub fn is_name(text: &str) -> bool {
    !text.is_empty() && text.len() <= 64 && text.split('.').all(is_word)
}

/// A Rust module path, or an operation named after one: words joined by `::`.
pub fn is_module(text: &str) -> bool {
    !text.is_empty() && text.len() <= 128 && text.split("::").all(is_name)
}

/// An error's type, such as `std::io::Error` or `panic`.
pub fn is_type(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 128
        && text.split("::").all(|part| {
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
}

/// A function in a stack trace, as Rust or Swift names it: its path, generics and closures.
fn is_symbol(text: &str) -> bool {
    text.len() <= 512
        && !text.contains('/')
        && !text.contains('@')
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_:<>{}#&*,' ()[]$.-".contains(&b))
}

/// A version, OS or device model, such as `26.1`, `Linux`, `iPhone17,1` or `x86_64`.
fn is_version(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 40
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".,_-+() ".contains(&b))
        // A dotted name with more than one part of letters is a host's.
        && text
            .split('.')
            .filter(|part| part.bytes().any(|b| b.is_ascii_alphabetic()))
            .count()
            <= 1
}

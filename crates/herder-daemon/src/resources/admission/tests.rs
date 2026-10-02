use std::sync::Arc;

use herder_protocol::{Role, ServerMessage};

use super::*;
use crate::hub::Outbox;
use crate::resources::ResourcesConfig;

const GIB: u64 = 1024 * 1024 * 1024;

/// A host whose readings the test sets.
#[derive(Clone)]
struct Fake(Arc<Mutex<Option<Reading>>>);

impl Fake {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Some(roomy()))))
    }

    fn set(&self, reading: Option<Reading>) {
        *self.0.lock().unwrap() = reading;
    }
}

impl ReadHost for Fake {
    fn read(&self) -> anyhow::Result<Reading> {
        self.0
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("unreadable"))
    }
}

fn roomy() -> Reading {
    Reading {
        memory_total: 16 * GIB,
        memory_available: 8 * GIB,
        load_1m: 1.0,
        cpu_percent: 25.0,
        pressure: Some(Pressure {
            cpu_some: 1.0,
            memory_some: 0.0,
            memory_full: 0.0,
            io_some: 0.0,
        }),
    }
}

fn budget(max_turns: u32) -> Budget {
    Budget {
        cores: 4,
        max_turns,
        min_memory_available: 2 * GIB,
        max_memory_pressure: 20.0,
    }
}

fn admission(max_turns: u32) -> (Admission, Fake) {
    let host = Fake::new();
    (
        Admission::new(budget(max_turns), Box::new(host.clone())),
        host,
    )
}

fn admitted(ticket: Ticket) -> Permit {
    match ticket {
        Ticket::Admitted(permit) => permit,
        Ticket::Waiting(_) => panic!("expected the turn to be admitted"),
    }
}

fn waiting(ticket: Ticket) -> oneshot::Receiver<Permit> {
    match ticket {
        Ticket::Waiting(waiting) => waiting,
        Ticket::Admitted(_) => panic!("expected the turn to wait"),
    }
}

#[test]
fn the_default_budget_is_a_quarter_of_the_cores_and_2_gib() {
    let config = ResourcesConfig::default();
    assert_eq!(config.budget(12).max_turns, 3);
    assert_eq!(config.budget(2).max_turns, 1);
    let budget = config.budget(12);
    assert_eq!(budget.min_memory_available, 2 * GIB);
    assert_eq!(budget.max_memory_pressure, 20.0);
    let config = ResourcesConfig {
        max_turns: Some(5),
        ..ResourcesConfig::default()
    };
    assert_eq!(config.budget(12).max_turns, 5);
}

#[test]
fn the_first_failing_check_is_the_constraint() {
    let budget = budget(2);
    let reading = roomy();
    assert_eq!(budget.constraint(1, Some(&reading)), None);
    assert_eq!(
        budget.constraint(2, Some(&reading)),
        Some(Constraint::MaxTurns)
    );
    let low = Reading {
        memory_available: 2 * GIB - 1,
        load_1m: 9.0,
        ..roomy()
    };
    assert_eq!(budget.constraint(0, Some(&low)), Some(Constraint::Memory));
    let loaded = Reading {
        load_1m: 4.0,
        ..roomy()
    };
    assert_eq!(budget.constraint(0, Some(&loaded)), Some(Constraint::Load));
    let mut stalling = roomy();
    stalling.pressure.as_mut().unwrap().memory_some = 20.0;
    assert_eq!(
        budget.constraint(0, Some(&stalling)),
        Some(Constraint::Pressure)
    );
    // Without PSI, pressure is not checked; without a reading, only the turn limit is.
    let no_psi = Reading {
        pressure: None,
        ..roomy()
    };
    assert_eq!(budget.constraint(0, Some(&no_psi)), None);
    assert_eq!(budget.constraint(1, None), None);
    assert_eq!(budget.constraint(2, None), Some(Constraint::MaxTurns));
}

#[test]
fn waiting_turns_are_admitted_in_order_as_running_ones_end() {
    let (admission, _) = admission(1);
    let first = admitted(admission.request());
    let mut second = waiting(admission.request());
    let mut third = waiting(admission.request());
    assert_eq!(admission.constraint(), Some(Constraint::MaxTurns));
    assert_eq!(admission.resources().waiting_turns, 2);

    drop(first);
    let second = second
        .try_recv()
        .expect("the oldest waiting turn goes first");
    assert!(third.try_recv().is_err());
    assert_eq!(admission.resources().running_turns, 1);
    drop(second);
    let third = third.try_recv().unwrap();
    drop(third);
    assert_eq!(admission.resources().running_turns, 0);
    assert_eq!(admission.constraint(), None);
}

#[test]
fn a_turn_never_overtakes_a_waiting_one() {
    let (admission, host) = admission(1);
    host.set(Some(Reading {
        memory_available: GIB,
        ..roomy()
    }));
    let mut first = waiting(admission.request());
    // The host has room again before a recheck admitted the earlier turn: it still goes first.
    host.set(Some(roomy()));
    let mut second = waiting(admission.request());
    let first = first.try_recv().unwrap();
    assert!(second.try_recv().is_err());
    drop(first);
    assert!(second.try_recv().is_ok());
}

#[test]
fn memory_short_turns_wait_until_a_recheck_finds_room() {
    let (admission, host) = admission(4);
    host.set(Some(Reading {
        memory_available: GIB,
        ..roomy()
    }));
    let mut turn = waiting(admission.request());
    admission.recheck();
    assert!(turn.try_recv().is_err());
    assert_eq!(admission.resources().constraint, Some(Constraint::Memory));

    host.set(Some(roomy()));
    admission.recheck();
    let _permit = turn.try_recv().unwrap();
    assert_eq!(admission.resources().running_turns, 1);
}

#[test]
fn a_turn_given_up_while_waiting_takes_no_slot() {
    let (admission, _) = admission(1);
    let first = admitted(admission.request());
    let gone = waiting(admission.request());
    let mut next = waiting(admission.request());
    drop(gone);
    assert_eq!(admission.resources().waiting_turns, 1);
    drop(first);
    let _next = next.try_recv().unwrap();
    assert_eq!(admission.resources().running_turns, 1);
}

#[test]
fn an_unreadable_host_admits_by_the_turn_limit_only() {
    let (admission, host) = admission(1);
    host.set(None);
    let _permit = admitted(admission.request());
    assert_eq!(admission.constraint(), Some(Constraint::MaxTurns));
}

#[test]
fn host_resources_are_published_when_they_change() {
    let (admission, host) = admission(1);
    let hub = Hub::default();
    let client = Arc::new(Outbox::default());
    hub.connect(&client, Role::Member);
    let published = || std::iter::from_fn(|| client.pop()).collect::<Vec<_>>();

    admission.publish(&hub);
    let [ServerMessage::HostResources(first)] = <[_; 1]>::try_from(published()).unwrap() else {
        panic!("expected host resources");
    };
    assert_eq!(
        first,
        HostResources {
            cpu_cores: 4,
            cpu_percent: 25.0,
            load_1m: 1.0,
            memory_total_bytes: 16 * GIB,
            memory_available_bytes: 8 * GIB,
            pressure: roomy().pressure,
            running_turns: 0,
            max_turns: 1,
            waiting_turns: 0,
            constraint: None,
        }
    );
    admission.publish(&hub);
    assert!(published().is_empty(), "nothing changed");

    let _permit = admitted(admission.request());
    host.set(Some(Reading {
        memory_available: GIB,
        ..roomy()
    }));
    let _waiting = waiting(admission.request());
    admission.publish(&hub);
    let [ServerMessage::HostResources(busy)] = <[_; 1]>::try_from(published()).unwrap() else {
        panic!("expected host resources");
    };
    assert_eq!(busy.running_turns, 1);
    assert_eq!(busy.waiting_turns, 1);
    assert_eq!(busy.constraint, Some(Constraint::MaxTurns));
}

#[test]
fn proc_files_are_parsed() {
    assert_eq!(load("3.52 2.10 1.00 2/1234 5678\n"), Some(3.52));
    assert_eq!(load(""), None);

    let stat = "cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 1 2 3 4 5 6 7 8 0 0\n";
    assert_eq!(cpu_times(stat), Some((150, 1000)));
    assert_eq!(cpu_times("intr 1\n"), None);
    assert_eq!(cpu_percent((150, 1000), (250, 1200)), 50.0);
    assert_eq!(cpu_percent((150, 1000), (150, 1000)), 0.0);

    let memory = "some avg10=12.34 avg60=1.00 avg300=0.50 total=1234\n\
                  full avg10=5.60 avg60=0.10 avg300=0.00 total=99\n";
    assert_eq!(avg10(memory, "some"), Some(12.34));
    assert_eq!(avg10(memory, "full"), Some(5.6));
    assert_eq!(avg10("some avg10=0.00\n", "full"), None);
}

#[test]
fn this_host_reads_from_proc() {
    let host = ProcHost::default();
    let reading = host.read().unwrap();
    assert!(reading.memory_total > 0);
    assert!(reading.memory_available <= reading.memory_total);
    assert!(reading.load_1m >= 0.0);
    assert_eq!(reading.cpu_percent, 0.0, "a first reading has no window");
    let pressure = std::path::Path::new("/proc/pressure/memory").exists();
    assert_eq!(reading.pressure.is_some(), pressure);
}

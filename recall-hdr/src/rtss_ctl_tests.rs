use super::*;
use std::cell::RefCell;
use std::rc::Rc;

fn facts(copy: bool, start_task: bool, scan: Scan) -> Facts {
    Facts { copy, start_task, scan }
}

const NONE: Scan = Scan { ours: false, foreign: false, unknown: false, shm: false };
const OURS: Scan = Scan { ours: true, ..NONE };
const FOREIGN: Scan = Scan { foreign: true, ..NONE };

#[test]
fn decide_covers_every_state() {
    let now = Instant::now();
    let fresh = Memory::default();
    let ready = facts(true, true, NONE);
    assert_eq!(decide(true, &facts(false, true, NONE), &fresh, now), (RtssState::Missing, Action::Nothing));
    assert_eq!(decide(true, &facts(true, false, NONE), &fresh, now), (RtssState::NeedsSetup, Action::Nothing));
    assert_eq!(decide(true, &ready, &fresh, now), (RtssState::Starting, Action::RunStart));
    let sent = Memory { owned: false, started_at: Some(now), starts: 1 };
    assert_eq!(decide(true, &ready, &sent, now + Duration::from_secs(1)), (RtssState::Starting, Action::Nothing), "no second start");
    assert_eq!(decide(true, &ready, &sent, now + START_TIMEOUT), (RtssState::NeedsSetup, Action::Nothing), "never showed up");
    assert_eq!(decide(true, &facts(true, true, OURS), &sent, now), (RtssState::Running, Action::Nothing));
    let shm_only = Scan { shm: true, ..NONE };
    assert_eq!(decide(true, &facts(true, true, shm_only), &sent, now).0, RtssState::Running, "shared memory opened");
    let spent = Memory { owned: true, started_at: None, starts: MAX_STARTS };
    assert_eq!(decide(true, &ready, &spent, now), (RtssState::NeedsSetup, Action::Nothing), "keeps exiting");
}

#[test]
fn the_users_own_rtss_is_used_but_never_started_or_stopped() {
    let now = Instant::now();
    let fresh = Memory::default();
    // Running elsewhere: no start even with our copy + task ready.
    assert_eq!(decide(true, &facts(true, true, FOREIGN), &fresh, now), (RtssState::Running, Action::Nothing));
    assert_eq!(decide(true, &facts(false, false, FOREIGN), &fresh, now), (RtssState::Running, Action::Nothing), "no copy needed");
    // Stats off with only the user's RTSS running: nothing to stop.
    assert_eq!(decide(false, &facts(true, true, FOREIGN), &fresh, now), (RtssState::Off, Action::Nothing));
    let owned = Memory { owned: true, ..fresh };
    assert_eq!(decide(false, &facts(true, true, FOREIGN), &owned, now), (RtssState::Off, Action::Nothing), "ours is not running");
}

#[test]
fn stats_off_stops_only_an_owned_copy() {
    let now = Instant::now();
    let owned = Memory { owned: true, started_at: None, starts: 1 };
    assert_eq!(decide(false, &facts(true, true, OURS), &owned, now), (RtssState::Off, Action::RunStop));
    let unreadable = Scan { unknown: true, ..NONE };
    assert_eq!(decide(false, &facts(true, true, unreadable), &owned, now), (RtssState::Off, Action::RunStop));
    assert_eq!(decide(false, &facts(true, true, unreadable), &Memory::default(), now), (RtssState::Off, Action::Nothing));
    assert_eq!(decide(false, &facts(true, true, NONE), &owned, now), (RtssState::Off, Action::Nothing), "already gone");
}

/// Scripted host: the scan to return next, and every task run.
#[derive(Default)]
struct Fake {
    copy: bool,
    task: bool,
    scan: Scan,
    task_queries: u32,
    runs: Vec<String>,
}

#[derive(Clone, Default)]
struct Host(Rc<RefCell<Fake>>);

impl RtssHost for Host {
    fn copy_present(&self) -> bool {
        self.0.borrow().copy
    }
    fn task_exists(&mut self, name: &str) -> bool {
        assert_eq!(name, START_TASK);
        let mut f = self.0.borrow_mut();
        f.task_queries += 1;
        f.task
    }
    fn scan(&self) -> Scan {
        self.0.borrow().scan
    }
    fn run_task(&mut self, name: &str) -> Result<(), String> {
        self.0.borrow_mut().runs.push(name.to_owned());
        Ok(())
    }
}

fn ctl(copy: bool, task: bool, scan: Scan) -> (RtssCtl, Host) {
    let host = Host::default();
    {
        let mut f = host.0.borrow_mut();
        f.copy = copy;
        f.task = task;
        f.scan = scan;
    }
    (RtssCtl::new(Box::new(host.clone())), host)
}

#[test]
fn toggle_on_starts_then_reports_running_and_toggle_off_stops() {
    let (mut c, host) = ctl(true, true, NONE);
    let t0 = Instant::now();
    c.sync(true, t0);
    assert_eq!(c.state(), RtssState::Starting);
    assert_eq!(host.0.borrow().runs, [START_TASK]);
    c.tick(true, t0 + Duration::from_millis(100));
    assert_eq!(host.0.borrow().runs.len(), 1, "not due yet");
    host.0.borrow_mut().scan = OURS;
    c.tick(true, t0 + Duration::from_millis(600));
    assert_eq!(c.state(), RtssState::Running);
    c.sync(false, t0 + Duration::from_secs(30));
    assert_eq!(c.state(), RtssState::Off);
    assert_eq!(host.0.borrow().runs, [START_TASK, STOP_TASK]);
    c.tick(false, t0 + Duration::from_secs(60));
    c.shutdown();
    assert_eq!(host.0.borrow().runs.len(), 2, "stopped once; nothing owned afterwards");
}

#[test]
fn quit_stops_an_owned_copy_and_never_a_foreign_one() {
    let (mut c, host) = ctl(true, true, NONE);
    let t0 = Instant::now();
    c.sync(true, t0);
    host.0.borrow_mut().scan = OURS;
    c.tick(true, t0 + Duration::from_secs(1));
    c.shutdown();
    assert_eq!(host.0.borrow().runs, [START_TASK, STOP_TASK]);

    let (mut c, host) = ctl(true, true, FOREIGN);
    c.sync(true, t0);
    assert_eq!(c.state(), RtssState::Running);
    c.sync(false, t0 + Duration::from_secs(1));
    c.shutdown();
    assert!(host.0.borrow().runs.is_empty(), "the user's RTSS is never touched: {:?}", host.0.borrow().runs);
}

#[test]
fn a_leftover_private_copy_is_adopted_and_stopped_at_start_up_with_stats_off() {
    let (mut c, host) = ctl(true, true, OURS);
    c.sync(false, Instant::now());
    assert_eq!(host.0.borrow().runs, [STOP_TASK]);
    assert_eq!(c.state(), RtssState::Off);
}

#[test]
fn missing_and_needs_setup_never_run_a_task_and_ask_for_the_task_once() {
    let (mut c, host) = ctl(false, false, NONE);
    let t0 = Instant::now();
    c.sync(true, t0);
    assert_eq!(c.state(), RtssState::Missing);
    let (mut c2, host2) = ctl(true, false, NONE);
    c2.sync(true, t0);
    c2.tick(true, t0 + Duration::from_secs(6));
    c2.tick(true, t0 + Duration::from_secs(12));
    assert_eq!(c2.state(), RtssState::NeedsSetup);
    assert_eq!(host2.0.borrow().task_queries, 1, "schtasks /query once per stats-on");
    assert!(host.0.borrow().runs.is_empty() && host2.0.borrow().runs.is_empty());
    c2.sync(false, t0 + Duration::from_secs(13));
    c2.sync(true, t0 + Duration::from_secs(14));
    assert_eq!(host2.0.borrow().task_queries, 2, "asked again after a toggle");
}

#[test]
fn an_rtss_that_keeps_exiting_is_restarted_at_most_max_starts_times() {
    let (mut c, host) = ctl(true, true, NONE);
    let mut t = Instant::now();
    c.sync(true, t);
    for _ in 0..MAX_STARTS + 2 {
        host.0.borrow_mut().scan = OURS;
        t += Duration::from_secs(1);
        c.tick(true, t);
        host.0.borrow_mut().scan = NONE;
        t += Duration::from_secs(6);
        c.tick(true, t);
    }
    let starts = host.0.borrow().runs.iter().filter(|r| r.as_str() == START_TASK).count();
    assert_eq!(starts as u32, MAX_STARTS);
    assert_eq!(c.state(), RtssState::NeedsSetup);
}

#[test]
fn states_have_the_wire_tokens() {
    let all = [RtssState::Off, RtssState::Starting, RtssState::Running, RtssState::Missing, RtssState::NeedsSetup];
    let tokens: Vec<_> = all.iter().map(|s| s.as_str()).collect();
    assert_eq!(tokens, ["off", "starting", "running", "missing", "needs_setup"]);
}

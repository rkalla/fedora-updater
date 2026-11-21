#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use fedora_updater::backend::flatpak::{packages_from_flatpak_updates, FlatpakCheck};
use fedora_updater::helper_protocol::{HelperCommand, DRAW_PREFIX};
use fedora_updater::orchestrator::OrchestratorRuntime;
use fedora_updater::privilege::SessionError;

use super::fixtures::{read_fixture, read_flatpak_fixture};

/// Convert raw fixture output (possibly containing inline `\r` redraws or CRLF)
/// into the wire lines emitted by `helper_mode` (`__DRAW__ ...` for redraws,
/// plain strings for committed lines).
pub fn fixture_to_helper_lines(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw_line in raw.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let segments: Vec<&str> = line.split('\r').filter(|s| !s.is_empty()).collect();
        if let Some((last, head)) = segments.split_last() {
            for seg in head {
                out.push(format!("{DRAW_PREFIX}{seg}"));
            }
            out.push((*last).to_string());
        }
    }
    out
}

/// Split plain command stdout/stderr fixture text into committed lines.
pub fn fixture_to_local_lines(raw: &str) -> Vec<String> {
    raw.lines().map(String::from).collect()
}

/// Build a [`FlatpakCheck`] from one of the checked-in `flatpak/*.json` fixtures.
pub fn flatpak_check_from_fixture(rel: &str, warning: Option<&str>) -> FlatpakCheck {
    let fx = read_flatpak_fixture(rel);
    let packages = packages_from_flatpak_updates(fx.update_source(), &fx.refs, &fx.ops);
    FlatpakCheck {
        packages,
        warning: warning.map(str::to_string),
    }
}

pub struct EnsureSessionStep {
    pub result: Result<(), SessionError>,
    pub hook: Option<Box<dyn FnMut() + Send>>,
}

pub struct HelperStep {
    pub lines: Vec<String>,
    pub result: Result<i32, SessionError>,
    pub hook_after_line: Option<(usize, Box<dyn FnMut() + Send>)>,
}

pub struct LocalStep {
    pub lines: Vec<String>,
    pub result: Result<i32, String>,
    pub hook_after_line: Option<(usize, Box<dyn FnMut() + Send>)>,
}

struct LocalMatcher {
    program: String,
    required_args: Vec<String>,
    step: LocalStep,
}

impl LocalMatcher {
    fn matches(&self, program: &str, args: &[&str]) -> bool {
        self.program == program
            && self
                .required_args
                .iter()
                .all(|req| args.contains(&req.as_str()))
    }
}

struct FakeState {
    now: SystemTime,
    ensure_session_calls: usize,
    helper_calls: Vec<HelperCommand>,
    local_calls: Vec<(String, Vec<String>)>,
    flatpak_queries: Vec<bool>,
    ensure_session_steps: VecDeque<EnsureSessionStep>,
    helper_steps: HashMap<HelperCommand, VecDeque<HelperStep>>,
    local_steps: VecDeque<LocalMatcher>,
    flatpak_user_steps: VecDeque<Result<FlatpakCheck, String>>,
    flatpak_system_steps: VecDeque<Result<FlatpakCheck, String>>,
}

/// Deterministic offline [`OrchestratorRuntime`] for functional tests.
#[derive(Clone)]
pub struct FakeRuntime {
    inner: Arc<Mutex<FakeState>>,
}

impl Default for FakeRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeRuntime {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeState {
                now: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                ensure_session_calls: 0,
                helper_calls: Vec::new(),
                local_calls: Vec::new(),
                flatpak_queries: Vec::new(),
                ensure_session_steps: VecDeque::new(),
                helper_steps: HashMap::new(),
                local_steps: VecDeque::new(),
                flatpak_user_steps: VecDeque::new(),
                flatpak_system_steps: VecDeque::new(),
            })),
        }
    }

    pub fn set_now(&self, now: SystemTime) {
        self.inner.lock().unwrap().now = now;
    }

    pub fn current_now(&self) -> SystemTime {
        self.inner.lock().unwrap().now
    }

    pub fn advance_now(&self, delta: Duration) {
        let mut s = self.inner.lock().unwrap();
        s.now += delta;
    }

    pub fn push_ensure_session(&self, result: Result<(), SessionError>) {
        self.inner
            .lock()
            .unwrap()
            .ensure_session_steps
            .push_back(EnsureSessionStep { result, hook: None });
    }

    pub fn push_ensure_session_hook(
        &self,
        result: Result<(), SessionError>,
        hook: impl FnMut() + Send + 'static,
    ) {
        self.inner
            .lock()
            .unwrap()
            .ensure_session_steps
            .push_back(EnsureSessionStep {
                result,
                hook: Some(Box::new(hook)),
            });
    }

    pub fn push_helper_step(&self, cmd: HelperCommand, step: HelperStep) {
        self.inner
            .lock()
            .unwrap()
            .helper_steps
            .entry(cmd)
            .or_default()
            .push_back(step);
    }

    pub fn push_helper_lines(
        &self,
        cmd: HelperCommand,
        lines: Vec<String>,
        result: Result<i32, SessionError>,
    ) {
        self.push_helper_step(
            cmd,
            HelperStep {
                lines,
                result,
                hook_after_line: None,
            },
        );
    }

    pub fn push_helper_fixture(&self, cmd: HelperCommand, rel: &str, exit_code: i32) {
        let lines = fixture_to_helper_lines(&read_fixture(rel));
        self.push_helper_lines(cmd, lines, Ok(exit_code));
    }

    pub fn push_local_step(&self, program: &str, required_args: &[&str], step: LocalStep) {
        self.inner.lock().unwrap().local_steps.push_back(LocalMatcher {
            program: program.into(),
            required_args: required_args.iter().map(|s| (*s).to_string()).collect(),
            step,
        });
    }

    pub fn push_local_lines(
        &self,
        program: &str,
        required_args: &[&str],
        lines: Vec<String>,
        result: Result<i32, String>,
    ) {
        self.push_local_step(
            program,
            required_args,
            LocalStep {
                lines,
                result,
                hook_after_line: None,
            },
        );
    }

    pub fn push_local_fixture(
        &self,
        program: &str,
        required_args: &[&str],
        rel: &str,
        exit_code: i32,
    ) {
        let lines = fixture_to_local_lines(&read_fixture(rel));
        self.push_local_lines(program, required_args, lines, Ok(exit_code));
    }

    /// Enqueue empty OK appstream sync commands for both `--user` and `--system`.
    pub fn push_flatpak_appstream_ok(&self) {
        self.push_local_lines(
            "flatpak",
            &["update", "--user", "--appstream", "--noninteractive"],
            Vec::new(),
            Ok(0),
        );
        self.push_local_lines(
            "flatpak",
            &["update", "--system", "--appstream", "--noninteractive"],
            Vec::new(),
            Ok(0),
        );
    }

    pub fn push_flatpak_query(&self, user: bool, result: Result<FlatpakCheck, String>) {
        let mut s = self.inner.lock().unwrap();
        if user {
            s.flatpak_user_steps.push_back(result);
        } else {
            s.flatpak_system_steps.push_back(result);
        }
    }

    pub fn push_flatpak_empty(&self, user: bool) {
        self.push_flatpak_query(
            user,
            Ok(FlatpakCheck {
                packages: Vec::new(),
                warning: None,
            }),
        );
    }

    pub fn ensure_session_calls(&self) -> usize {
        self.inner.lock().unwrap().ensure_session_calls
    }

    pub fn helper_calls(&self) -> Vec<HelperCommand> {
        self.inner.lock().unwrap().helper_calls.clone()
    }

    pub fn local_calls(&self) -> Vec<(String, Vec<String>)> {
        self.inner.lock().unwrap().local_calls.clone()
    }

    pub fn flatpak_queries(&self) -> Vec<bool> {
        self.inner.lock().unwrap().flatpak_queries.clone()
    }
}

impl OrchestratorRuntime for FakeRuntime {
    fn ensure_session(&self) -> Result<(), SessionError> {
        let step = {
            let mut s = self.inner.lock().unwrap();
            s.ensure_session_calls += 1;
            s.ensure_session_steps.pop_front()
        };
        match step {
            Some(mut st) => {
                if let Some(mut hook) = st.hook.take() {
                    hook();
                }
                st.result
            }
            None => Ok(()),
        }
    }

    fn run_helper(
        &self,
        cmd: HelperCommand,
        on_line: &mut dyn FnMut(String),
    ) -> Result<i32, SessionError> {
        let step = {
            let mut s = self.inner.lock().unwrap();
            s.helper_calls.push(cmd);
            s.helper_steps
                .get_mut(&cmd)
                .and_then(VecDeque::pop_front)
        };
        let Some(mut step) = step else {
            panic!("FakeRuntime: unscripted helper command {cmd:?}");
        };
        let mut hook = step.hook_after_line.take();
        for (idx, line) in step.lines.into_iter().enumerate() {
            on_line(line);
            if let Some((target_idx, ref mut f)) = hook {
                if idx == target_idx {
                    f();
                }
            }
        }
        step.result
    }

    fn run_local(
        &self,
        program: &str,
        args: &[&str],
        on_line: &mut dyn FnMut(String),
    ) -> Result<i32, String> {
        let step = {
            let mut s = self.inner.lock().unwrap();
            s.local_calls.push((
                program.into(),
                args.iter().map(|a| (*a).to_string()).collect(),
            ));
            let pos = s
                .local_steps
                .iter()
                .position(|m| m.matches(program, args));
            pos.and_then(|idx| s.local_steps.remove(idx)).map(|m| m.step)
        };
        let Some(mut step) = step else {
            panic!(
                "FakeRuntime: unscripted local command `{program}` args={args:?}"
            );
        };
        let mut hook = step.hook_after_line.take();
        for (idx, line) in step.lines.into_iter().enumerate() {
            on_line(line);
            if let Some((target_idx, ref mut f)) = hook {
                if idx == target_idx {
                    f();
                }
            }
        }
        step.result
    }

    fn query_flatpak(&self, user: bool) -> Result<FlatpakCheck, String> {
        let res = {
            let mut s = self.inner.lock().unwrap();
            s.flatpak_queries.push(user);
            if user {
                s.flatpak_user_steps.pop_front()
            } else {
                s.flatpak_system_steps.pop_front()
            }
        };
        res.unwrap_or_else(|| {
            panic!("FakeRuntime: unscripted flatpak query user={user}")
        })
    }

    fn now(&self) -> SystemTime {
        self.inner.lock().unwrap().now
    }
}

#![allow(dead_code)]

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use fedora_updater::backend::flatpak::FlatpakCheck;
use fedora_updater::helper_protocol::{HelperCommand, HELLO_LINE};
use fedora_updater::orchestrator::OrchestratorRuntime;
use fedora_updater::privilege::{PrivilegedSession, SessionError, SessionEvent};
use tempfile::TempDir;

use super::fixtures::read_fixture;
use super::runtime::{fixture_to_helper_lines, FakeRuntime};

/// Prevents concurrent `Command::spawn` in other threads from inheriting an open
/// writable FD to a newly written script (`ETXTBSY`).
static HELPER_FORK_LOCK: RwLock<()> = RwLock::new(());

pub struct PausedStepGate {
    reached_path: PathBuf,
    release_path: PathBuf,
}

impl PausedStepGate {
    pub fn wait_until_reached(&self, timeout: Duration) {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if self.reached_path.exists() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "timed out after {timeout:?} waiting for fake helper to reach {}",
            self.reached_path.display()
        );
    }

    pub fn release(&self) {
        fs::write(&self.release_path, "release\n").expect("write pause release file");
    }
}

struct PipeHelperInner {
    dir: TempDir,
    script_path: PathBuf,
    local: FakeRuntime,
    session: Mutex<Option<PrivilegedSession>>,
    active_pid: AtomicU32,
    step_counts: Mutex<HashMap<HelperCommand, usize>>,
}

/// Real-pipe [`OrchestratorRuntime`] whose privileged helper commands execute
/// against a fixture-backed subprocess speaking the `fedora-updater-helper`
/// wire protocol over OS pipes via [`PrivilegedSession::start_direct`], while
/// unprivileged local commands and synthetic clocks come from [`FakeRuntime`].
#[derive(Clone)]
pub struct PipeHelperRuntime {
    inner: Arc<PipeHelperInner>,
}

impl PipeHelperRuntime {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("create temp dir for fake helper");
        let dir_str = dir
            .path()
            .to_str()
            .expect("temp dir path must be valid UTF-8");
        assert!(
            !dir_str.contains('\''),
            "unexpected single quote in temp path: {dir_str}"
        );

        let script_path = dir.path().join("fake-helper.sh");
        let body = format!(
            "#!/bin/sh
if [ \"${{1:-}}\" != \"--helper\" ]; then
  echo 'expected --helper' >&2
  exit 2
fi
STATE_DIR='{dir_str}'
if [ -f \"$STATE_DIR/deny_next\" ]; then
  rm -f \"$STATE_DIR/deny_next\"
  exit 1
fi

printf '%s\\n' \"$$\" > \"$STATE_DIR/current.pid\"
printf '%s\\n' \"$$\" >> \"$STATE_DIR/pids.log\"
printf '%s\\n' '{HELLO_LINE}'

while IFS= read -r req || [ -n \"$req\" ]; do
  req=$(printf '%s' \"$req\" | tr -d '\\r')
  case \"$req\" in
    QUIT|quit|Quit)
      printf '%s\\n' '__BYE__'
      exit 0
      ;;
    'RUN '*)
      id=${{req#RUN }}
      seq_file=\"$STATE_DIR/seq.$id\"
      if [ -f \"$seq_file\" ]; then
        idx=$(cat \"$seq_file\")
      else
        idx=0
      fi
      printf '%s\\n' \"$((idx + 1))\" > \"$seq_file\"
      printf '%s %s\\n' \"$id\" \"$$\" >> \"$STATE_DIR/invocations.log\"

      lines_file=\"$STATE_DIR/step.$id.$idx.lines\"
      if [ -f \"$lines_file\" ]; then
        cat \"$lines_file\"
      fi

      pause_file=\"$STATE_DIR/step.$id.$idx.pause\"
      if [ -f \"$pause_file\" ]; then
        printf '%s\\n' \"$$\" > \"$STATE_DIR/step.$id.$idx.reached\"
        while [ ! -f \"$STATE_DIR/step.$id.$idx.release\" ]; do
          sleep 0.02
        done
      fi

      crash_file=\"$STATE_DIR/step.$id.$idx.crash\"
      if [ -f \"$crash_file\" ]; then
        exit 1
      fi

      code_file=\"$STATE_DIR/step.$id.$idx.code\"
      if [ -f \"$code_file\" ]; then
        code=$(cat \"$code_file\")
      else
        code=2
      fi
      printf '__END__ %s\\n' \"$code\"
      ;;
    *)
      printf '__END__ 2\\n'
      ;;
  esac
done
"
        );

        {
            let _guard = HELPER_FORK_LOCK
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&script_path)
                .expect("create fake helper script");
            file.write_all(body.as_bytes())
                .expect("write fake helper script");
            file.sync_all().expect("sync fake helper script");
            drop(file);
            let mut perms = fs::metadata(&script_path)
                .expect("stat fake helper")
                .permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script_path, perms).expect("chmod fake helper");
        }

        Self {
            inner: Arc::new(PipeHelperInner {
                dir,
                script_path,
                local: FakeRuntime::new(),
                session: Mutex::new(None),
                active_pid: AtomicU32::new(0),
                step_counts: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn local(&self) -> &FakeRuntime {
        &self.inner.local
    }

    pub fn script_path(&self) -> &Path {
        &self.inner.script_path
    }

    pub fn fork_lock(&self) -> &RwLock<()> {
        &HELPER_FORK_LOCK
    }

    pub fn deny_next_start(&self) {
        fs::write(self.inner.dir.path().join("deny_next"), "deny\n").expect("write deny_next");
    }

    fn alloc_step(&self, cmd: HelperCommand) -> (String, usize) {
        let mut counts = self
            .inner
            .step_counts
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let entry = counts.entry(cmd).or_insert(0);
        let idx = *entry;
        *entry += 1;
        (cmd.as_id().to_string(), idx)
    }

    pub fn push_helper_lines(&self, cmd: HelperCommand, lines: Vec<String>, exit_code: i32) {
        let (id, idx) = self.alloc_step(cmd);
        if !lines.is_empty() {
            let body = format!("{}\n", lines.join("\n"));
            fs::write(
                self.inner.dir.path().join(format!("step.{id}.{idx}.lines")),
                body,
            )
            .expect("write helper lines");
        }
        fs::write(
            self.inner.dir.path().join(format!("step.{id}.{idx}.code")),
            format!("{exit_code}\n"),
        )
        .expect("write helper code");
    }

    pub fn push_helper_fixture(&self, cmd: HelperCommand, rel: &str, exit_code: i32) {
        let lines = fixture_to_helper_lines(&read_fixture(rel));
        self.push_helper_lines(cmd, lines, exit_code);
    }

    pub fn push_helper_paused_lines(
        &self,
        cmd: HelperCommand,
        lines: Vec<String>,
    ) -> PausedStepGate {
        let (id, idx) = self.alloc_step(cmd);
        if !lines.is_empty() {
            let body = format!("{}\n", lines.join("\n"));
            fs::write(
                self.inner.dir.path().join(format!("step.{id}.{idx}.lines")),
                body,
            )
            .expect("write paused helper lines");
        }
        fs::write(
            self.inner.dir.path().join(format!("step.{id}.{idx}.pause")),
            "pause\n",
        )
        .expect("write pause flag");
        fs::write(
            self.inner.dir.path().join(format!("step.{id}.{idx}.code")),
            "0\n",
        )
        .expect("write paused exit code");
        PausedStepGate {
            reached_path: self
                .inner
                .dir
                .path()
                .join(format!("step.{id}.{idx}.reached")),
            release_path: self
                .inner
                .dir
                .path()
                .join(format!("step.{id}.{idx}.release")),
        }
    }

    pub fn push_helper_crash_after_lines(&self, cmd: HelperCommand, lines: Vec<String>) {
        let (id, idx) = self.alloc_step(cmd);
        if !lines.is_empty() {
            let body = format!("{}\n", lines.join("\n"));
            fs::write(
                self.inner.dir.path().join(format!("step.{id}.{idx}.lines")),
                body,
            )
            .expect("write crash helper lines");
        }
        fs::write(
            self.inner.dir.path().join(format!("step.{id}.{idx}.crash")),
            "crash\n",
        )
        .expect("write crash flag");
    }

    pub fn has_live_session(&self) -> bool {
        let Ok(mut guard) = self.inner.session.lock() else {
            return false;
        };
        if let Some(session) = guard.as_mut() {
            if session.is_running() {
                return true;
            }
            *guard = None;
            self.inner.active_pid.store(0, Ordering::SeqCst);
        }
        false
    }

    pub fn abort_session(&self) {
        let pid = self.inner.active_pid.swap(0, Ordering::SeqCst);
        if pid != 0 {
            let pid = pid as i32;
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
        }
        if let Ok(mut guard) = self.inner.session.try_lock() {
            if let Some(session) = guard.take() {
                session.abort();
            }
        }
    }

    pub fn recorded_pids(&self) -> Vec<u32> {
        let Ok(raw) = fs::read_to_string(self.inner.dir.path().join("pids.log")) else {
            return Vec::new();
        };
        raw.lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
            .collect()
    }

    pub fn recorded_invocations(&self) -> Vec<(String, u32)> {
        let Ok(raw) = fs::read_to_string(self.inner.dir.path().join("invocations.log")) else {
            return Vec::new();
        };
        raw.lines()
            .filter_map(|line| {
                let (id, pid) = line.split_once(' ')?;
                Some((id.to_string(), pid.trim().parse::<u32>().ok()?))
            })
            .collect()
    }

    fn start_direct_retry(&self) -> Result<PrivilegedSession, SessionError> {
        for attempt in 0..10 {
            let res = {
                let _guard = HELPER_FORK_LOCK
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
                PrivilegedSession::start_direct(&self.inner.script_path)
            };
            match res {
                Err(SessionError::Spawn(ref msg))
                    if (msg.contains("Text file busy") || msg.contains("ETXTBSY"))
                        && attempt < 9 =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                other => return other,
            }
        }
        unreachable!("loop returns on final attempt")
    }

    fn read_current_pid(&self) -> u32 {
        let pid_path = self.inner.dir.path().join("current.pid");
        for _ in 0..50 {
            if let Ok(text) = fs::read_to_string(&pid_path) {
                if let Ok(pid) = text.trim().parse::<u32>() {
                    return pid;
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("fake helper did not record current.pid");
    }

    fn take_or_start_session(&self) -> Result<PrivilegedSession, SessionError> {
        let mut guard = self
            .inner
            .session
            .lock()
            .map_err(|_| SessionError::Io("session lock poisoned".into()))?;
        if let Some(session) = guard.as_mut() {
            if !session.is_running() {
                *guard = None;
                self.inner.active_pid.store(0, Ordering::SeqCst);
            }
        }
        if guard.is_none() {
            let session = self.start_direct_retry()?;
            let pid = self.read_current_pid();
            self.inner.active_pid.store(pid, Ordering::SeqCst);
            *guard = Some(session);
        }
        Ok(guard.take().expect("just ensured Some"))
    }
}

impl Default for PipeHelperRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl OrchestratorRuntime for PipeHelperRuntime {
    fn ensure_session(&self) -> Result<(), SessionError> {
        let session = self.take_or_start_session()?;
        let mut guard = self
            .inner
            .session
            .lock()
            .map_err(|_| SessionError::Io("session lock poisoned".into()))?;
        *guard = Some(session);
        Ok(())
    }

    fn run_helper(
        &self,
        cmd: HelperCommand,
        on_line: &mut dyn FnMut(String),
    ) -> Result<i32, SessionError> {
        let mut session = self.take_or_start_session()?;
        let result = session.run_command(cmd, |ev| {
            if let SessionEvent::Line(line) = ev {
                on_line(line);
            }
        });
        match &result {
            Err(SessionError::Closed)
            | Err(SessionError::Io(_))
            | Err(SessionError::AuthDenied) => {
                self.inner.active_pid.store(0, Ordering::SeqCst);
                drop(session);
            }
            _ => {
                if session.is_running() {
                    if let Ok(mut guard) = self.inner.session.lock() {
                        *guard = Some(session);
                    }
                } else {
                    self.inner.active_pid.store(0, Ordering::SeqCst);
                    drop(session);
                }
            }
        }
        result
    }

    fn run_local(
        &self,
        program: &str,
        args: &[&str],
        on_line: &mut dyn FnMut(String),
    ) -> Result<i32, String> {
        self.inner.local.run_local(program, args, on_line)
    }

    fn run_cancellable_local(
        &self,
        program: &str,
        args: &[&str],
        on_line: &mut dyn FnMut(String),
    ) -> Result<i32, String> {
        self.inner.local.run_cancellable_local(program, args, on_line)
    }

    fn query_flatpak(&self, user: bool) -> Result<FlatpakCheck, String> {
        self.inner.local.query_flatpak(user)
    }

    fn now(&self) -> SystemTime {
        self.inner.local.now()
    }
}

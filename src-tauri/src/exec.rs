//! Non-interactive command execution, as a job with a handle.
//!
//! `term.rs` is a pty for a human to type into. This is the other half: run a
//! command, capture its output, keep its exit code, and let somebody ask about
//! it later. It exists because a thread pinned to one machine still needs to
//! build on another, and `git.*` only covers the operations git happens to have.
//!
//! **Every job is asynchronous, including the ones that finish instantly.**
//! `exec.start` returns a job id and never blocks; `exec.status` is a cheap
//! poll. That is not over-engineering for a `ls` — it is the only shape that
//! survives the two timeouts this feature sits between. A routed request must
//! answer inside `PEER_REQUEST_TIMEOUT` (30 s) or it wedges a link shared by
//! every other user of that machine, and an MCP tool call must answer inside
//! whatever timeout the driving harness happens to use, which differs per
//! provider and is not ours to choose. A release build respects neither. So the
//! wire operation is always "start and answer immediately", and the *waiting* is
//! done by the caller — `mcp.rs` polls in-process for a bounded window so short
//! commands still feel synchronous to the agent.
//!
//! Output is bounded and front-trimmed like terminal scrollback: a build that
//! prints a million lines must not become this process's memory problem, and the
//! end of the log is the part that says what went wrong.

use crate::limits;
use crate::server::ServerState;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::watch;

/// Terminal state of a job. `Failed` is "we could not run it at all" (no such
/// directory, no shell); a command that ran and returned 1 is `Exited`, which
/// is a successful *execution* of a failing command and must not be conflated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatus {
    Running,
    Exited(i32),
    Failed(String),
    Cancelled,
    TimedOut,
}

impl JobStatus {
    fn wire(&self) -> &'static str {
        match self {
            JobStatus::Running => "running",
            JobStatus::Exited(_) => "exited",
            JobStatus::Failed(_) => "failed",
            JobStatus::Cancelled => "cancelled",
            JobStatus::TimedOut => "timedOut",
        }
    }

    fn is_done(&self) -> bool {
        !matches!(self, JobStatus::Running)
    }
}

/// One output stream, kept as a bounded tail plus a count of what fell off the
/// front. `total` is the byte offset just past the end, so a poller can ask for
/// "everything after N" and be told honestly when N is no longer available.
#[derive(Default)]
struct Stream {
    buf: String,
    /// Bytes ever written, including those trimmed away.
    total: usize,
}

impl Stream {
    fn push(&mut self, line: &str) {
        self.buf.push_str(line);
        self.buf.push('\n');
        self.total += line.len() + 1;
        // Front-trim on a char boundary; `drain` on a byte index inside a
        // multi-byte char would panic, and build logs are full of box-drawing.
        if self.buf.len() > limits::EXEC_STREAM_BUFFER {
            let excess = self.buf.len() - limits::EXEC_STREAM_BUFFER;
            let cut = (excess..=self.buf.len())
                .find(|i| self.buf.is_char_boundary(*i))
                .unwrap_or(self.buf.len());
            self.buf.drain(..cut);
        }
    }

    /// Everything after byte offset `from`, and the offset to ask for next.
    /// When `from` points at trimmed-away bytes the whole buffer comes back and
    /// `skipped` says how much was lost — silently returning the tail as though
    /// it were contiguous is how a caller ends up parsing a truncated line.
    fn since(&self, from: usize) -> (String, usize, usize) {
        let first_available = self.total - self.buf.len();
        if from >= self.total {
            return (String::new(), self.total, 0);
        }
        if from <= first_available {
            return (self.buf.clone(), self.total, first_available - from);
        }
        let offset = from - first_available;
        let offset = (offset..=self.buf.len())
            .find(|i| self.buf.is_char_boundary(*i))
            .unwrap_or(self.buf.len());
        (self.buf[offset..].to_string(), self.total, 0)
    }
}

struct JobState {
    status: JobStatus,
    stdout: Stream,
    stderr: Stream,
    finished_at: Option<String>,
}

pub struct Job {
    pub id: String,
    pub command: String,
    pub cwd: String,
    pub shell: &'static str,
    pub started_at: String,
    started: Instant,
    state: Mutex<JobState>,
    /// Flipped once when the job reaches a terminal state, so `wait` can park
    /// instead of polling. A `watch` rather than a `Notify` because a waiter
    /// that arrives *after* the job finished must still see it immediately.
    done_tx: watch::Sender<bool>,
    done_rx: watch::Receiver<bool>,
    /// OS pid of the shell, for cancellation. `None` once reaped.
    pid: Mutex<Option<u32>>,
}

impl Job {
    fn finish(&self, status: JobStatus) {
        {
            let mut state = self.state.lock().unwrap();
            if state.status.is_done() {
                return;
            }
            state.status = status;
            state.finished_at = Some(crate::protocol::now_iso());
        }
        *self.pid.lock().unwrap() = None;
        let _ = self.done_tx.send(true);
    }

    /// A status snapshot, with output deltas after the given offsets.
    pub fn snapshot(&self, stdout_from: usize, stderr_from: usize) -> Value {
        let state = self.state.lock().unwrap();
        let (stdout, stdout_offset, stdout_skipped) = state.stdout.since(stdout_from);
        let (stderr, stderr_offset, stderr_skipped) = state.stderr.since(stderr_from);
        json!({
            "jobId": self.id,
            "status": state.status.wire(),
            "exitCode": match &state.status {
                JobStatus::Exited(code) => json!(code),
                _ => Value::Null,
            },
            "error": match &state.status {
                JobStatus::Failed(message) => json!(message),
                JobStatus::TimedOut => json!("the command hit its timeout and was killed"),
                JobStatus::Cancelled => json!("cancelled"),
                _ => Value::Null,
            },
            "command": self.command,
            "cwd": self.cwd,
            "shell": self.shell,
            "startedAt": self.started_at,
            "finishedAt": state.finished_at,
            "elapsedMs": self.started.elapsed().as_millis() as u64,
            "stdout": stdout,
            "stdoutOffset": stdout_offset,
            "stdoutSkipped": stdout_skipped,
            "stderr": stderr,
            "stderrOffset": stderr_offset,
            "stderrSkipped": stderr_skipped,
        })
    }

    pub fn is_done(&self) -> bool {
        self.state.lock().unwrap().status.is_done()
    }
}

pub struct ExecRegistry {
    jobs: Mutex<HashMap<String, Arc<Job>>>,
}

impl Default for ExecRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// What to run and where. Resolved from the payload before anything is spawned,
/// so a bad path is an error rather than a job that fails a second later.
pub struct ExecSpec {
    pub command: String,
    pub cwd: PathBuf,
    pub timeout: Duration,
}

impl ExecRegistry {
    pub fn new() -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
        }
    }

    pub fn job(&self, id: &str) -> Option<Arc<Job>> {
        self.jobs.lock().unwrap().get(id).cloned()
    }

    fn live_count(&self) -> usize {
        self.jobs
            .lock()
            .unwrap()
            .values()
            .filter(|j| !j.is_done())
            .count()
    }

    /// Drop finished jobs once there are too many to be worth remembering,
    /// oldest first. Running jobs are never evicted — a handle that stops
    /// resolving while the process is still going is worse than no handle.
    fn gc(&self) {
        let mut jobs = self.jobs.lock().unwrap();
        if jobs.len() <= limits::MAX_REMEMBERED_EXEC_JOBS {
            return;
        }
        let mut finished: Vec<_> = jobs
            .values()
            .filter(|j| j.is_done())
            .map(|j| (j.started, j.id.clone()))
            .collect();
        finished.sort_by_key(|(started, _)| *started);
        let excess = jobs.len() - limits::MAX_REMEMBERED_EXEC_JOBS;
        for (_, id) in finished.into_iter().take(excess) {
            jobs.remove(&id);
        }
    }

    /// Spawn the command and return its job id immediately.
    pub fn start(&self, spec: ExecSpec) -> Result<String> {
        anyhow::ensure!(
            !spec.command.trim().is_empty(),
            "there is no command to run"
        );
        anyhow::ensure!(
            self.live_count() < limits::MAX_LIVE_EXEC_JOBS,
            "{} commands are already running on this machine, which is the limit — \
             wait for one to finish or cancel it",
            limits::MAX_LIVE_EXEC_JOBS
        );
        anyhow::ensure!(
            spec.cwd.is_dir(),
            "working directory does not exist: {}",
            spec.cwd.display()
        );

        let (done_tx, done_rx) = watch::channel(false);
        let job = Arc::new(Job {
            id: crate::protocol::new_id(),
            command: spec.command.clone(),
            cwd: spec.cwd.to_string_lossy().into_owned(),
            shell: shell_name(),
            started_at: crate::protocol::now_iso(),
            started: Instant::now(),
            state: Mutex::new(JobState {
                status: JobStatus::Running,
                stdout: Stream::default(),
                stderr: Stream::default(),
                finished_at: None,
            }),
            done_tx,
            done_rx,
            pid: Mutex::new(None),
        });
        self.jobs
            .lock()
            .unwrap()
            .insert(job.id.clone(), job.clone());
        self.gc();

        let id = job.id.clone();
        tokio::spawn(run(job, spec));
        Ok(id)
    }

    /// Park until the job finishes or `budget` runs out, then snapshot. The
    /// caller decides how long it is willing to wait, because the answer differs
    /// by three orders of magnitude between a mesh hop and an agent's tool call.
    pub async fn wait(
        &self,
        job_id: &str,
        budget: Duration,
        stdout_from: usize,
        stderr_from: usize,
    ) -> Result<Value> {
        let job = self.job(job_id).context("unknown job")?;
        if !job.is_done() {
            let mut rx = job.done_rx.clone();
            // Ignore both outcomes: a timeout and a closed channel both just
            // mean "snapshot whatever it looks like now", which is a valid
            // answer — `status` in the result says which it was.
            let _ = tokio::time::timeout(budget, rx.wait_for(|done| *done)).await;
        }
        Ok(job.snapshot(stdout_from, stderr_from))
    }

    pub fn cancel(&self, job_id: &str) -> Result<()> {
        let job = self.job(job_id).context("unknown job")?;
        let pid = *job.pid.lock().unwrap();
        if let Some(pid) = pid {
            kill_tree(pid);
        }
        job.finish(JobStatus::Cancelled);
        Ok(())
    }

    pub fn list(&self) -> Vec<Value> {
        let mut jobs: Vec<_> = self.jobs.lock().unwrap().values().cloned().collect();
        jobs.sort_by_key(|j| j.started);
        jobs.iter()
            .map(|j| {
                let mut v = j.snapshot(usize::MAX, usize::MAX);
                // The list is for "what is running", not for reading logs.
                if let Some(o) = v.as_object_mut() {
                    o.remove("stdout");
                    o.remove("stderr");
                }
                v
            })
            .collect()
    }
}

/// The shell a command string is handed to, which the agent needs to know
/// before it writes one. Reported in every result for exactly that reason: a
/// planner on Linux writing `&&` for a PowerShell host produces a parse error
/// that looks nothing like the real problem.
pub fn shell_name() -> &'static str {
    if cfg!(windows) {
        "powershell"
    } else {
        "sh"
    }
}

fn build_command(spec: &ExecSpec) -> tokio::process::Command {
    let mut cmd;
    if cfg!(windows) {
        cmd = tokio::process::Command::new("powershell.exe");
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
        ]);
        cmd.arg(&spec.command);
    } else {
        cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(&spec.command);
    }
    cmd.current_dir(&spec.cwd);
    // The same PATH the agent CLIs get: a desktop-launched app inherits a thin
    // one, and a build that cannot find `cargo` is the most confusing possible
    // failure for something whose whole job is to run builds.
    cmd.env("PATH", crate::agents::agent_path());
    // Tools that colour their output make the captured log unreadable, and an
    // agent reading escape sequences wastes context on them.
    cmd.env("NO_COLOR", "1");
    // `true`, NOT `1`. The convention is "any non-empty value means CI", and
    // most tools read it that way — but some bind the variable to a real
    // boolean flag, and those reject anything that is not `true`/`false`.
    // Tauri is one: with `CI=1` its own CLI refuses to start with
    // `invalid value '1' for '--ci'`, so `npx tauri build` fails on every
    // machine in the fleet, and the error names a flag nobody passed.
    cmd.env("CI", "true");
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    crate::agents::no_console(&mut cmd);
    // Own process group, so cancelling kills the build and not just the shell
    // that launched it. Without this, `cancel` on `cargo build` returns
    // instantly and leaves rustc pinning every core.
    #[cfg(unix)]
    cmd.process_group(0);
    cmd
}

/// Kill a whole process tree. Killing just the shell leaves the actual work
/// running, which is indistinguishable from cancellation not working.
fn kill_tree(pid: u32) {
    #[cfg(unix)]
    {
        // Negative pid = the process group, which `process_group(0)` made equal
        // to the shell's own pid.
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGTERM);
        }
        let pid = pid as libc::pid_t;
        tokio::spawn(async move {
            tokio::time::sleep(limits::EXEC_KILL_GRACE).await;
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
        });
    }
    #[cfg(windows)]
    {
        let mut cmd = tokio::process::Command::new("taskkill.exe");
        cmd.args(["/T", "/F", "/PID", &pid.to_string()]);
        crate::agents::no_console(&mut cmd);
        tokio::spawn(async move {
            let _ = cmd.output().await;
        });
    }
}

async fn run(job: Arc<Job>, spec: ExecSpec) {
    let mut child = match build_command(&spec).spawn() {
        Ok(child) => child,
        Err(e) => {
            job.finish(JobStatus::Failed(format!(
                "could not start {}: {e}",
                shell_name()
            )));
            return;
        }
    };
    *job.pid.lock().unwrap() = child.id();

    // Both streams are drained concurrently. Reading one to completion first
    // deadlocks the moment the other fills its pipe buffer — which for a build
    // is immediately, because compilers write diagnostics to stderr.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_job = job.clone();
    let out = tokio::spawn(async move {
        if let Some(pipe) = stdout {
            let mut lines = BufReader::new(pipe).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                out_job.state.lock().unwrap().stdout.push(&line);
            }
        }
    });
    let err_job = job.clone();
    let err = tokio::spawn(async move {
        if let Some(pipe) = stderr {
            let mut lines = BufReader::new(pipe).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                err_job.state.lock().unwrap().stderr.push(&line);
            }
        }
    });

    let outcome = tokio::time::timeout(spec.timeout, child.wait()).await;
    // Let the readers finish draining whatever is still buffered before the
    // status flips to done, or a fast command's last lines race the poller that
    // sees `exited` and stops asking.
    let status = match outcome {
        Ok(Ok(status)) => {
            let _ = out.await;
            let _ = err.await;
            #[cfg(unix)]
            let code = {
                use std::os::unix::process::ExitStatusExt;
                status.code().unwrap_or_else(|| {
                    // Killed by a signal: report it the way a shell does.
                    status.signal().map(|s| 128 + s).unwrap_or(-1)
                })
            };
            #[cfg(not(unix))]
            let code = status.code().unwrap_or(-1);
            JobStatus::Exited(code)
        }
        Ok(Err(e)) => JobStatus::Failed(format!("waiting for the command failed: {e}")),
        Err(_) => {
            if let Some(pid) = *job.pid.lock().unwrap() {
                kill_tree(pid);
            }
            out.abort();
            err.abort();
            JobStatus::TimedOut
        }
    };
    job.finish(status);
}

/// Where a command runs. A project id (a registered root) is the unit, with an
/// optional relative subdirectory — the same "roots only" rule thread creation
/// uses, for the same reason: an arbitrary absolute path from a caller is a
/// filesystem-wide grant wearing a working-directory costume.
fn resolve_cwd(state: &ServerState, payload: &Value) -> Result<PathBuf> {
    let store = &state.hub.store;
    let root = if let Some(project_id) = payload.get("projectId").and_then(Value::as_str) {
        let project = store.project(project_id).context("unknown project")?;
        PathBuf::from(project.path)
    } else if let Some(thread_id) = payload.get("threadId").and_then(Value::as_str) {
        let thread = store.thread(thread_id).context("unknown thread")?;
        store.thread_working_dir(&thread)?
    } else {
        anyhow::bail!("say which project (or thread) the command should run in");
    };

    let Some(subdir) = payload
        .get("subdir")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(root);
    };
    let candidate = Path::new(subdir);
    anyhow::ensure!(
        candidate.is_relative()
            && !candidate
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "subdir must be a relative path inside the project"
    );
    let joined = root.join(candidate);
    // Canonicalize both sides: a symlink inside the project pointing out of it
    // would otherwise pass the textual check above.
    let real_root = root.canonicalize().unwrap_or(root);
    let real = joined
        .canonicalize()
        .with_context(|| format!("no such directory: {subdir}"))?;
    anyhow::ensure!(
        real.starts_with(&real_root),
        "subdir resolves outside the project"
    );
    Ok(real)
}

fn timeout_from(payload: &Value) -> Duration {
    let secs = payload
        .get("timeoutSeconds")
        .and_then(Value::as_u64)
        .unwrap_or(limits::EXEC_DEFAULT_TIMEOUT.as_secs());
    Duration::from_secs(secs.clamp(1, limits::EXEC_MAX_TIMEOUT.as_secs()))
}

fn offset(payload: &Value, key: &str) -> usize {
    payload
        .get(key)
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(usize::MAX as u64) as usize
}

/// Run one command to completion on a named machine and hand back its exit code
/// and stdout. The synchronous convenience over `exec.*` for callers that need
/// a short answer rather than a job handle — the sync-ref prelude, mostly.
///
/// Still built from the same non-blocking primitives: each individual hop is
/// short, and the *loop* is here, so nothing sits on a peer link.
pub async fn run_capture(
    state: &ServerState,
    machine_id: &str,
    project_id: &str,
    command: &str,
    budget: Duration,
) -> Result<(i64, String)> {
    let local = machine_id == state.device.machine_id;
    let call = |kind: &'static str, payload: Value| async move {
        if local {
            handle(state, kind, &payload).await
        } else {
            state
                .peernet
                .request(machine_id, kind, payload, None)
                .await
        }
    };

    let mut snapshot = call(
        "exec.start",
        json!({
            "projectId": project_id,
            "command": command,
            "timeoutSeconds": budget.as_secs(),
        }),
    )
    .await?;
    let job_id = snapshot["jobId"]
        .as_str()
        .context("no job id")?
        .to_string();
    let mut out = snapshot["stdout"].as_str().unwrap_or_default().to_string();

    let began = Instant::now();
    while snapshot["status"] == "running" && began.elapsed() < budget {
        snapshot = call(
            "exec.status",
            json!({
                "jobId": job_id,
                "waitSeconds": limits::EXEC_MAX_ROUTED_WAIT.as_secs(),
                "stdoutOffset": snapshot["stdoutOffset"],
            }),
        )
        .await?;
        out.push_str(snapshot["stdout"].as_str().unwrap_or_default());
    }
    anyhow::ensure!(
        snapshot["status"] != "running",
        "`{command}` did not finish within {}s",
        budget.as_secs()
    );
    let code = snapshot["exitCode"].as_i64().unwrap_or(-1);
    if code != 0 {
        // stderr is where git says why, and the caller is about to refuse
        // something on the strength of this.
        let stderr = snapshot["stderr"].as_str().unwrap_or_default().trim().to_string();
        if !stderr.is_empty() {
            out.push('\n');
            out.push_str(&stderr);
        }
    }
    Ok((code, out.trim().to_string()))
}

/// `exec.*`. Routed by `machineId` like the git family, so a thread on one
/// machine can build on another.
pub async fn handle(state: &ServerState, kind: &str, payload: &Value) -> Result<Value> {
    let exec = &state.exec;
    match kind {
        "exec.start" => {
            let command = payload
                .get("command")
                .and_then(Value::as_str)
                .context("missing command")?
                .to_string();
            let cwd = resolve_cwd(state, payload)?;
            let job_id = exec.start(ExecSpec {
                command,
                cwd,
                timeout: timeout_from(payload),
            })?;
            // A short grace period before answering means the overwhelmingly
            // common case — a command that takes 40 ms — comes back complete in
            // one round trip instead of forcing a poll. Well inside the peer
            // request timeout, so a routed start never risks the link.
            let snapshot = exec
                .wait(&job_id, limits::EXEC_START_GRACE, 0, 0)
                .await?;
            Ok(snapshot)
        }
        "exec.status" => {
            let job_id = payload
                .get("jobId")
                .and_then(Value::as_str)
                .context("missing jobId")?;
            let wait = payload
                .get("waitSeconds")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .min(limits::EXEC_MAX_ROUTED_WAIT.as_secs());
            exec.wait(
                job_id,
                Duration::from_secs(wait),
                offset(payload, "stdoutOffset"),
                offset(payload, "stderrOffset"),
            )
            .await
        }
        "exec.cancel" => {
            let job_id = payload
                .get("jobId")
                .and_then(Value::as_str)
                .context("missing jobId")?;
            exec.cancel(job_id)?;
            Ok(json!({ "ok": true }))
        }
        "exec.list" => Ok(json!({ "jobs": exec.list() })),
        _ => anyhow::bail!("unknown request: {kind}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(command: &str) -> ExecSpec {
        ExecSpec {
            command: command.to_string(),
            cwd: std::env::temp_dir(),
            timeout: Duration::from_secs(30),
        }
    }

    #[tokio::test]
    async fn captures_output_and_exit_code() {
        let reg = ExecRegistry::new();
        let id = reg.start(spec("echo hello; echo oops 1>&2; exit 3")).unwrap();
        let v = reg.wait(&id, Duration::from_secs(20), 0, 0).await.unwrap();
        assert_eq!(v["status"], "exited");
        assert_eq!(v["exitCode"], 3);
        assert!(v["stdout"].as_str().unwrap().contains("hello"));
        assert!(v["stderr"].as_str().unwrap().contains("oops"));
    }

    /// A command that fails is a *successful execution*. Conflating the two
    /// makes "the build failed" indistinguishable from "the machine is broken".
    #[tokio::test]
    async fn a_failing_command_still_exits_rather_than_failing() {
        let reg = ExecRegistry::new();
        let id = reg.start(spec("exit 1")).unwrap();
        let v = reg.wait(&id, Duration::from_secs(20), 0, 0).await.unwrap();
        assert_eq!(v["status"], "exited");
        assert_eq!(v["exitCode"], 1);
        assert!(v["error"].is_null());
    }

    #[tokio::test]
    async fn deltas_resume_from_an_offset() {
        let reg = ExecRegistry::new();
        let id = reg.start(spec("echo one; echo two")).unwrap();
        let first = reg.wait(&id, Duration::from_secs(20), 0, 0).await.unwrap();
        let offset = first["stdoutOffset"].as_u64().unwrap() as usize;
        assert!(first["stdout"].as_str().unwrap().contains("one"));

        let job = reg.job(&id).unwrap();
        let again = job.snapshot(offset, 0);
        assert_eq!(again["stdout"], "", "nothing new after the final offset");
        assert_eq!(again["stdoutOffset"].as_u64().unwrap() as usize, offset);
    }

    #[tokio::test]
    async fn timeout_kills_the_job() {
        let reg = ExecRegistry::new();
        let id = reg
            .start(ExecSpec {
                command: "sleep 30".into(),
                cwd: std::env::temp_dir(),
                timeout: Duration::from_millis(300),
            })
            .unwrap();
        let v = reg.wait(&id, Duration::from_secs(20), 0, 0).await.unwrap();
        assert_eq!(v["status"], "timedOut");
        assert!(v["error"].as_str().unwrap().contains("timeout"));
    }

    #[tokio::test]
    async fn cancel_stops_a_running_job() {
        let reg = ExecRegistry::new();
        let id = reg.start(spec("sleep 30")).unwrap();
        // Let it actually get a pid before cancelling.
        tokio::time::sleep(Duration::from_millis(200)).await;
        reg.cancel(&id).unwrap();
        let v = reg.wait(&id, Duration::from_secs(5), 0, 0).await.unwrap();
        assert_eq!(v["status"], "cancelled");
    }

    /// Cancelling has to kill the whole tree. Killing only the shell returns
    /// instantly and leaves the actual build running, which is indistinguishable
    /// from cancellation not working at all — and is what happens without
    /// `process_group(0)` + `killpg`.
    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_kills_grandchildren_not_just_the_shell() {
        let reg = ExecRegistry::new();
        // The shell prints the background child's pid, then waits on it.
        let id = reg.start(spec("sleep 40 & echo PID=$!; wait")).unwrap();
        // Give it time to print the pid.
        let mut grandchild = None;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let v = reg.job(&id).unwrap().snapshot(0, 0);
            if let Some(line) = v["stdout"]
                .as_str()
                .and_then(|s| s.lines().find_map(|l| l.strip_prefix("PID=")))
            {
                grandchild = line.trim().parse::<i32>().ok();
                break;
            }
        }
        let grandchild = grandchild.expect("the shell never reported its child pid");
        let alive = || unsafe { libc::kill(grandchild, 0) == 0 };
        assert!(alive(), "precondition: the grandchild is running");

        reg.cancel(&id).unwrap();
        // SIGTERM to the group should reach it well before the SIGKILL grace.
        for _ in 0..40 {
            if !alive() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the grandchild survived cancellation — the process group was not killed");
    }

    #[tokio::test]
    async fn waiting_on_an_already_finished_job_returns_at_once() {
        let reg = ExecRegistry::new();
        let id = reg.start(spec("true")).unwrap();
        reg.wait(&id, Duration::from_secs(20), 0, 0).await.unwrap();
        // The second wait must not park for the full budget: `watch` starts
        // with the terminal value already set, which is why it is a watch.
        let began = Instant::now();
        reg.wait(&id, Duration::from_secs(10), 0, 0).await.unwrap();
        assert!(began.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn stream_front_trims_on_a_char_boundary() {
        let mut s = Stream::default();
        // Multi-byte content well past the buffer, so trimming has to land on a
        // boundary or this panics.
        for _ in 0..(limits::EXEC_STREAM_BUFFER / 10) {
            s.push("─────ok");
        }
        assert!(s.buf.len() <= limits::EXEC_STREAM_BUFFER + 32);
        let (text, total, skipped) = s.since(0);
        assert_eq!(total, s.total);
        assert!(skipped > 0, "the caller is told bytes were lost");
        assert!(text.ends_with("ok\n"));
    }

    #[test]
    fn since_reports_loss_rather_than_pretending_to_be_contiguous() {
        let mut s = Stream::default();
        s.push("first");
        let (_, after_first, _) = s.since(0);
        for _ in 0..limits::EXEC_STREAM_BUFFER / 4 {
            s.push("padding-padding");
        }
        let (_, _, skipped) = s.since(after_first);
        assert!(skipped > 0);
    }
}

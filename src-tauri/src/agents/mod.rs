//! Agent session hub: one driver task per active thread, normalized event fan-out.

pub mod claude;
pub mod codex;
pub mod content;
pub mod hermes;
pub mod kimi;
pub mod repair;
mod title;
pub mod transcript;

use crate::protocol::*;
use crate::store::Store;
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio::sync::{broadcast, mpsc};

/// PATH for spawning agent CLIs. Desktop launches (app icon) don't inherit the
/// user's shell PATH, so `claude` (~/.local/bin) and `codex` (nvm bin) would be
/// invisible; augment with the places user-level CLIs actually live.
pub fn agent_path() -> &'static str {
    static AGENT_PATH: OnceLock<String> = OnceLock::new();
    AGENT_PATH.get_or_init(|| {
        let mut parts: Vec<String> = Vec::new();
        if let Ok(p) = std::env::var("PATH") {
            parts.push(p);
        }
        if let Some(home) = dirs::home_dir() {
            parts.push(home.join(".local/bin").to_string_lossy().into_owned());
            // Official Kimi Code installer location.
            parts.push(
                home.join(".kimi-code/bin")
                    .to_string_lossy()
                    .into_owned(),
            );
            parts.push(home.join("bin").to_string_lossy().into_owned());
            if let Ok(entries) = std::fs::read_dir(home.join(".nvm/versions/node")) {
                for e in entries.flatten() {
                    parts.push(e.path().join("bin").to_string_lossy().into_owned());
                }
            }
            parts.push(home.join(".cargo/bin").to_string_lossy().into_owned());
            parts.push(home.join(".bun/bin").to_string_lossy().into_owned());
        }
        if cfg!(windows) {
            // npm global shims (claude.cmd / codex.cmd) live here.
            if let Ok(appdata) = std::env::var("APPDATA") {
                parts.push(format!("{appdata}\\npm"));
            }
        } else {
            // Intel Homebrew + system-wide npm/native installs.
            parts.push("/usr/local/bin".into());
            // Apple-Silicon Homebrew lives here and is NOT on a GUI-launched
            // (Finder/Dock/launchd) app's PATH, so a brew-installed
            // claude/codex is otherwise invisible to the resolver.
            if cfg!(target_os = "macos") {
                parts.push("/opt/homebrew/bin".into());
                parts.push("/opt/homebrew/sbin".into());
            }
        }
        // Windows PATH entries use ';' and may contain drive-letter colons, so
        // the separator must be platform-correct.
        let sep = if cfg!(windows) { ';' } else { ':' };
        let mut seen = std::collections::HashSet::new();
        parts
            .iter()
            .flat_map(|p| p.split(sep))
            .filter(|s| !s.is_empty() && seen.insert(s.to_string()))
            .collect::<Vec<_>>()
            .join(&sep.to_string())
    })
}

/// Absolute path of an agent CLI, resolved against [`agent_path`].
///
/// On Windows, prefer a real executable (`claude.exe`) over the npm batch shim
/// (`claude.cmd`): spawning the `.cmd` inserts a `cmd.exe` layer whose child
/// gets its own visible console window. Spawning the native `.exe` directly with
/// CREATE_NO_WINDOW stays hidden — the same approach the Claude Agent SDK takes.
/// We search the whole path for the `.exe` first so a native install in
/// `~/.local/bin` wins even when an npm shim sits earlier in PATH.
pub fn resolve_bin(name: &str) -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    #[cfg(windows)]
    {
        for ext in ["exe", "com"] {
            if let Ok(p) = which::which_in(format!("{name}.{ext}"), Some(agent_path()), &cwd) {
                return Some(p);
            }
        }
    }
    which::which_in(name, Some(agent_path()), cwd).ok()
}

/// Stop Windows from popping a console window for each child process. A
/// GUI-subsystem app shows a flashing empty `cmd`-like window for every console
/// child (agent CLIs, curl) unless spawned with CREATE_NO_WINDOW. No-op elsewhere.
pub fn no_console(cmd: &mut tokio::process::Command) {
    #[cfg(windows)]
    {
        // winbase CREATE_NO_WINDOW.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

/// A resolved attachment handed to a driver: the bytes already live on disk, so
/// the driver reads + inlines them (base64 image block / data-url) at send time.
/// Non-image files are instead copied into the agent's workspace and referenced
/// by path in the prompt (see [`materialize_docs`]).
#[derive(Debug, Clone)]
pub struct AttachmentRef {
    /// Original filename as uploaded, used for the in-workspace copy + prompt.
    pub name: String,
    pub mime_type: String,
    pub path: PathBuf,
}

/// A non-image attachment copied into the agent's workspace so a sandboxed
/// agent (Claude's `Read`, Codex's shell) can open it by relative path.
#[derive(Debug, Clone)]
pub struct MaterializedDoc {
    /// Workspace-relative path, e.g. `.threadknot/attachments/<id>/report.pdf`.
    pub rel_path: String,
    pub mime_type: String,
}

/// Copy every non-image attachment into `<cwd>/.threadknot/attachments/<id>/<name>`
/// (git-ignored, inside the sandbox root) and return workspace-relative refs.
/// Images are skipped — drivers inline those natively. A copy that fails is
/// dropped silently; the footer simply won't mention it.
pub fn materialize_docs(cwd: &str, attachments: &[AttachmentRef]) -> Vec<MaterializedDoc> {
    let mut out = Vec::new();
    let mut ignore_ready = false;
    for att in attachments {
        if att.mime_type.starts_with("image/") {
            continue;
        }
        // The on-disk stem is the attachment's UUID; use it as a collision-free
        // subdir so two files sharing a name never clobber each other.
        let Some(id) = att.path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let safe_name = safe_file_name(&att.name);
        let rel = format!(".threadknot/attachments/{id}/{safe_name}");
        let dest = std::path::Path::new(cwd).join(&rel);
        if let Some(parent) = dest.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                continue;
            }
        }
        if !ignore_ready {
            // Keep the materialized files out of the user's repo: a `*` ignore
            // inside `.threadknot/` (which also ignores itself) hides the whole dir
            // from `git status`.
            let gitignore = std::path::Path::new(cwd).join(".threadknot").join(".gitignore");
            if !gitignore.exists() {
                let _ = std::fs::write(&gitignore, "*\n");
            }
            ignore_ready = true;
        }
        if std::fs::copy(&att.path, &dest).is_ok() {
            out.push(MaterializedDoc {
                rel_path: rel,
                mime_type: att.mime_type.clone(),
            });
        }
    }
    out
}

/// A prompt footer listing materialized attachments so the agent knows they
/// exist and can read them on demand. Empty when there are no docs.
pub fn attachment_footer(docs: &[MaterializedDoc]) -> String {
    if docs.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n\n[Attached files — read them from the workspace as needed]");
    for d in docs {
        s.push_str(&format!("\n- ./{} ({})", d.rel_path, d.mime_type));
    }
    s
}

/// Artifact `source` tag for a producing agent.
fn agent_source(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "claude",
        Agent::Codex => "codex",
        Agent::Kimi => "kimi",
        Agent::Hermes => "hermes",
        Agent::Claudex => "claudex",
    }
}

/// Identity of the provider process a LANE's driver runs.
///
/// A live driver is only reusable for a lane whose key matches. Keying by
/// participant id rather than agent kind is what lets two lanes run the same
/// agent independently (Claude reviewing Claude) instead of the second one
/// silently inheriting the first one's live session and context. The profile
/// component stays because Claudex pins a gateway and `CLAUDE_CONFIG_DIR` per
/// profile, so switching profiles has to respawn rather than talk to a process
/// still bound to the previous one.
fn session_key(participant: &Participant) -> (String, Option<String>) {
    match participant.agent {
        Agent::Claudex => (
            participant.id.clone(),
            Some(participant.settings.model.clone()),
        ),
        _ => (participant.id.clone(), None),
    }
}

/// The sub-provider a lane is pinned to, if its agent kind has more than one.
fn lane_profile(participant: &Participant) -> Option<String> {
    session_key(participant).1
}

/// A lane's resumable native session, if any. An anchor recorded under a
/// different Claudex profile is unusable: the session id only exists inside the
/// config home that produced it.
fn usable_anchor<'a>(thread: &'a Thread, participant: &Participant) -> Option<&'a SessionAnchor> {
    let anchor = thread.session_anchors.get(&participant.id)?;
    (anchor.profile == lane_profile(participant)).then_some(anchor)
}

/// Reduce a user-supplied filename to a single safe path component: strip any
/// directory parts and forbid separators / control chars so it can't escape
/// the attachments dir.
fn safe_file_name(name: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim();
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('.').trim();
    if cleaned.is_empty() {
        "file".to_string()
    } else {
        cleaned.to_string()
    }
}

/// Model a review runs on when the client doesn't name one. Profile-backed
/// kinds have no guessable default — their "model" is a registry entry.
fn default_review_model(agent: Agent) -> Result<String> {
    Ok(match agent {
        Agent::Claude => claude::DEFAULT_MODEL.to_string(),
        Agent::Codex => "gpt-5.5".to_string(),
        Agent::Kimi => kimi::DEFAULT_MODEL.to_string(),
        Agent::Hermes | Agent::Claudex => anyhow::bail!(
            "pick a {} profile to review with",
            agent.display_name()
        ),
    })
}

/// The permissions paragraph every reviewer brief carries, matched to the
/// lane's access level: a brief must never forbid editing a lane that can
/// write, or it would be instructing the agent against its actual capabilities.
fn review_permissions(writable: bool) -> &'static str {
    if writable {
        "You have write access to this repository — the user granted it deliberately. \
         Investigate as much as you need, and you may apply a fix directly when it is \
         small and unambiguous, but your job is still the review: report first, patch \
         second."
    } else {
        "You are READ-ONLY: investigate the repository as much as you need, but do \
         not edit files and do not try to fix anything yourself."
    }
}

/// The reviewer's brief, delivered as the first message of its turn (the
/// handoff seed with the real transcript precedes it).
///
/// Every line here is load-bearing. A model asked to "review" defaults to
/// summarizing and agreeing, so the brief demands evidence, forces a severity
/// ranking, and — critically — makes "no material objection" an explicitly
/// acceptable answer. Without that last permission the model invents nits to
/// look useful, which is how adversarial review turns into noise.
///
/// The permissions paragraph tracks the lane's access level (see
/// [`review_permissions`]); `personality` (a persona's voice) is folded in
/// right after the framing so it colors everything below it.
fn review_brief(
    builder: &Participant,
    lane: &Participant,
    instructions: Option<&str>,
    personality: Option<&str>,
) -> String {
    let permissions = review_permissions(lane.settings.access != Access::Read);
    let persona = personality
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|p| format!("You review as {}: {p}\n\n", lane.name))
        .unwrap_or_default();
    let mut brief = format!(
        "=== ADVERSARIAL REVIEW ===\n\
         You are joining this thread as an independent reviewer. The work above was \
         done by {}, not by you. Your job is not to summarize it and not to be \
         agreeable — it is to find what is actually wrong with it.\n\n\
         {persona}{permissions}\n\n\
         Work in this order:\n\
         1. Verify rather than trust. Open the files and read the code that was \
         actually written or proposed — do not take the transcript's word for what \
         it says or that it works.\n\
         2. Lead with your strongest objection: the one you could defend to the \
         author with evidence. Cite `file:line` or real command output for each \
         claim you make.\n\
         3. Rank every finding — BLOCKER (wrong, unsafe, or breaks existing \
         behavior), MAJOR (will cause real problems), MINOR, NIT.\n\
         4. Say what you checked and found sound. A reviewer who reports only \
         faults can't be calibrated.\n\n\
         If, after genuinely investigating, you have no material objection, say \
         exactly that in one line and stop. A manufactured nit is worse than \
         nothing. Do not restate the plan back, and do not ask for permission to \
         begin.",
        builder.name
    );
    if let Some(extra) = instructions.map(str::trim).filter(|s| !s.is_empty()) {
        brief.push_str(&format!(
            "\n\nThe user asked you to focus this review on:\n{extra}"
        ));
    }
    brief
}

/// A parley reviewer's brief for one round of a debate: the same adversarial
/// spine as [`review_brief`], plus the debate rules — named fellow reviewers,
/// a round number, and the machine-readable `VERDICT:` line the scheduler
/// parses (see [`parley_verdict`]).
fn parley_review_brief(
    builder_name: &str,
    lane: &Participant,
    others: &[String],
    round: u32,
    max_rounds: u32,
    instructions: Option<&str>,
    personality: Option<&str>,
) -> String {
    let permissions = review_permissions(lane.settings.access != Access::Read);
    let count = others.len() + 1;
    let others = if others.is_empty() {
        "none — you are the only reviewer".to_string()
    } else {
        others.join(", ")
    };
    let persona = personality
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|p| format!("You review as {}: {p}\n\n", lane.name))
        .unwrap_or_default();
    let round_line = if round > 1 {
        format!(
            "This is round {round} of {max_rounds}. {builder_name} has answered the previous \
             round's objections — the answers are in the transcript above. RE-VERIFY them \
             against the actual code: concede what is genuinely resolved, and object only \
             where an answer is wrong, incomplete, or broke something new. Do not repeat a \
             settled point to seem thorough."
        )
    } else {
        format!(
            "This is round 1 of up to {max_rounds}. After every reviewer has spoken, \
             {builder_name} answers each objection by name; the debate runs until every \
             reviewer concedes or the round cap is hit."
        )
    };
    let mut brief = format!(
        "=== ADVERSARIAL REVIEW — PARLEY ===\n\
         You are {lane_name}, one of {count} independent reviewer(s) debating the work in \
         this thread. The work above was done by {builder_name}, not by you; your fellow \
         reviewers: {others}. Your job is not to summarize it and not to be agreeable — it \
         is to find what is actually wrong with it.\n\n\
         {persona}{permissions}\n\n\
         {round_line}\n\n\
         Work in this order:\n\
         1. Verify rather than trust. Open the files and read the code that was actually \
         written — do not take the transcript's word for what it says or that it works.\n\
         2. Lead with your strongest objection: the one you could defend to the author \
         with evidence. Cite `file:line` or real command output for each claim you make.\n\
         3. Rank every finding — BLOCKER (wrong, unsafe, or breaks existing behavior), \
         MAJOR (will cause real problems), MINOR, NIT.\n\
         4. Say what you checked and found sound. A reviewer who reports only faults \
         can't be calibrated.\n\n\
         End your review with a final line containing EXACTLY one of:\n\
           VERDICT: CONCEDED — you have no material objection left\n\
           VERDICT: OBJECTING — at least one objection still stands\n\
         Never omit that line: it, not your prose, is what moves the debate to the next \
         speaker. Conceding when you are genuinely satisfied is how the debate ends; a \
         manufactured nit is worse than nothing. Do not ask for permission to begin.",
        lane_name = lane.name,
    );
    if let Some(extra) = instructions.map(str::trim).filter(|s| !s.is_empty()) {
        brief.push_str(&format!(
            "\n\nThe user asked every reviewer to focus on:\n{extra}"
        ));
    }
    brief
}

/// The Builder's brief for its answer turn: respond to the round's objectors
/// by name, conceding or rebutting — but implement nothing yet.
fn parley_answer_brief(objector_names: &str, round: u32, max_rounds: u32) -> String {
    format!(
        "=== PARLEY — YOUR ANSWER (round {round} of {max_rounds}) ===\n\
         The reviewers ({objector_names}) raised objections to your work, above. Answer \
         every one, by reviewer name:\n\
         - CONCEDE where they are right: state the exact fix you will make.\n\
         - REBUT where they are wrong: with evidence — `file:line` or real command \
         output, not preference.\n\
         Do not implement anything yet. If the reviewers accept your answers, the fixes \
         you concede here are precisely what you will be asked to implement afterward, \
         so concede only what you mean to do. Do not ask for permission."
    )
}

/// The Builder's brief for the execution turn after convergence.
fn parley_execute_brief() -> String {
    "=== PARLEY — EXECUTE ===\n\
     The debate converged: every reviewer is satisfied. Implement exactly the fixes you \
     conceded during it — nothing more, nothing adjacent — then run the project's own \
     checks and report what changed and how you verified it."
        .to_string()
}

/// The Builder's closing turn after convergence without an execution pass:
/// the debate's product, written down — the plan everyone signed off on.
fn parley_plan_brief() -> String {
    "=== PARLEY — THE PLAN ===\n\
     The debate converged: every reviewer is satisfied. Write the plan you all agreed \
     to, as this thread's record of the decision: numbered, concrete steps with real \
     file and function names, in the order they should happen, and one line per \
     non-obvious choice saying who raised it and why the answer won. Do not implement \
     anything in this turn — this document is what the work will be measured against."
        .to_string()
}

/// The Builder's closing turn at the round cap: not a verdict, an honest
/// handoff of everything still open so the user can settle it in one read.
fn parley_escalation_brief(max_rounds: u32) -> String {
    format!(
        "=== PARLEY — OPEN QUESTIONS ===\n\
         The debate did not converge in {max_rounds} rounds. Write the closing summary \
         the user will decide from:\n\
         1. AGREED — everything that was settled, one line each.\n\
         2. OPEN — each unresolved objection: the reviewer's claim, your position, and \
         the evidence on both sides.\n\
         3. YOUR CALL — for every open item, the exact question the user must answer \
         and your recommendation. Each question must be answerable in one sentence.\n\
         Do not implement anything in this turn."
    )
}

/// Whether a reviewer lane's latest turn conceded. The parley brief demands a
/// machine-readable `VERDICT:` line; the phrase fallback covers a model that
/// forgot it. Absent either, the safe answer is "still objecting" — that keeps
/// the debate going (bounded by the round cap) instead of ending it on a maybe.
fn parley_verdict(events: &[PersistedEvent], lane_id: &str) -> bool {
    let text = events
        .iter()
        .rev()
        .filter(|e| e.speaker.as_deref() == Some(lane_id))
        .find_map(|e| match &e.event {
            AgentEvent::AssistantMessage { text } => Some(text),
            _ => None,
        });
    let Some(text) = text else { return false };
    for line in text.lines().rev() {
        let line = line
            .trim()
            .trim_start_matches(['*', '#', '>']);
        if let Some(rest) = line.strip_prefix("VERDICT:") {
            let verdict = rest
                .trim()
                .trim_end_matches('*')
                .trim()
                .to_ascii_uppercase();
            return verdict.starts_with("CONCEDED");
        }
    }
    text.to_ascii_lowercase().contains("no material objection")
}

/// One reviewer to seat: the agent that powers it plus optional model, effort
/// and access overrides (see [`Hub::join_reviewer`] for the defaults), a
/// display name, and a personality folded into its briefs. Personas are just
/// named, reusable specs (see `personas.rs`).
#[derive(Debug, Clone)]
pub struct ReviewerSpec {
    pub agent: Agent,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub access: Option<Access>,
    /// Lane display name; defaults to "{Agent} (reviewer)".
    pub name: Option<String>,
    /// The persona (`personas.json` id) this reviewer belongs to. Personas are
    /// stable lane identities: two personas on the same setup seat TWO lanes.
    pub persona: Option<String>,
    /// Folded into every review brief this reviewer runs.
    pub personality: Option<String>,
}

impl ReviewerSpec {
    pub fn new(agent: Agent) -> Self {
        Self {
            agent,
            model: None,
            effort: None,
            access: None,
            name: None,
            persona: None,
            personality: None,
        }
    }
}

/// What the parley scheduler seats next. A pure decision, split from the
/// effectful [`Hub::advance_parley`] so the whole state machine is testable
/// without spawning a provider.
#[derive(Debug, PartialEq)]
enum ParleyAction {
    /// Seat reviewer lane `lane_id` with a fresh-round brief.
    Review { lane_id: String },
    /// The Builder answers this round's objectors.
    Answer,
    /// Converged with prior objections: the Builder implements what it conceded.
    Execute,
    /// The closing turn: the agreed plan, or the open questions at the cap.
    Verdict,
    /// A queued user interjection takes the floor on the Builder lane.
    UserTurn { text: String },
    /// The debate is over; the note says why.
    End { note: String },
}

/// Score the turn that just ended and decide what happens next, mutating
/// `parley` to the state that must be persisted BEFORE the action runs.
/// `flight` is what the ended turn was seated as, `speaker_id` the lane that
/// produced it, and `conceded` the reviewer's parsed verdict when the flight
/// was `Reviewer`.
///
/// A parley never just stops: once its rounds end it moves to a WRAP — the
/// execution turn, the agreed-plan summary, or the cap's open-questions
/// summary — so the thread is always left with a deliverable.
fn parley_decide(
    parley: &mut ParleyState,
    flight: Option<ParleyFlight>,
    speaker_id: &str,
    conceded: Option<bool>,
) -> ParleyAction {
    match flight {
        Some(ParleyFlight::Reviewer) => {
            if conceded.unwrap_or(false) {
                parley.objectors.retain(|id| id != speaker_id);
            } else {
                if !parley.objectors.iter().any(|id| id == speaker_id) {
                    parley.objectors.push(speaker_id.to_string());
                }
                parley.had_objections = true;
            }
            parley.next += 1;
        }
        Some(ParleyFlight::Answer) => {
            parley.round += 1;
            parley.next = 0;
            parley.objectors.clear();
            if parley.round > parley.max_rounds {
                // Out of rounds: the closing turn hands the open questions
                // to the user instead of trailing off.
                parley.wrap = Some(ParleyWrap::Escalation);
            }
        }
        // The closing turn finished: the parley is done — but a message the
        // user queued mid-debate still speaks before we pack up.
        Some(ParleyFlight::Execute) | Some(ParleyFlight::Verdict) => {
            if let Some(text) = parley.pending_user.take() {
                parley.in_flight = Some(ParleyFlight::User);
                return ParleyAction::UserTurn { text };
            }
            return ParleyAction::End {
                note: match parley.wrap {
                    Some(ParleyWrap::Plan) => "parley: the agreed plan is above".to_string(),
                    Some(ParleyWrap::Escalation) => {
                        "parley: the open questions are above — answer any of them and the builder continues"
                            .to_string()
                    }
                    _ => "parley: done — the agreed fixes are in".to_string(),
                },
            };
        }
        // A user interjection is neutral to the schedule: the debate resumes below.
        Some(ParleyFlight::User) | None => {}
    }
    parley.in_flight = None;

    // The user's queued message always speaks next.
    if let Some(text) = parley.pending_user.take() {
        parley.in_flight = Some(ParleyFlight::User);
        return ParleyAction::UserTurn { text };
    }
    // A parley in its wrap phase seats the closing turn (again, if the user
    // just interjected — their message belongs in the summary).
    match parley.wrap {
        Some(ParleyWrap::Execute) => {
            parley.in_flight = Some(ParleyFlight::Execute);
            return ParleyAction::Execute;
        }
        Some(ParleyWrap::Plan) | Some(ParleyWrap::Escalation) => {
            parley.in_flight = Some(ParleyFlight::Verdict);
            return ParleyAction::Verdict;
        }
        None => {}
    }
    // Next reviewer this round.
    if parley.next < parley.lanes.len() {
        let lane_id = parley.lanes[parley.next].clone();
        parley.in_flight = Some(ParleyFlight::Reviewer);
        return ParleyAction::Review { lane_id };
    }
    // Every reviewer has spoken: objections send the Builder in to answer; a
    // clean round ends the rounds and starts the wrap.
    if !parley.objectors.is_empty() {
        parley.in_flight = Some(ParleyFlight::Answer);
        return ParleyAction::Answer;
    }
    if parley.execute && parley.had_objections {
        parley.wrap = Some(ParleyWrap::Execute);
        parley.in_flight = Some(ParleyFlight::Execute);
        return ParleyAction::Execute;
    }
    if parley.had_objections {
        parley.wrap = Some(ParleyWrap::Plan);
        parley.in_flight = Some(ParleyFlight::Verdict);
        return ParleyAction::Verdict;
    }
    ParleyAction::End {
        note: "parley: no material objections — nothing to debate".to_string(),
    }
}

#[derive(Debug)]
pub enum AgentCommand {
    User {
        text: String,
        settings: ThreadSettings,
        attachments: Vec<AttachmentRef>,
    },
    /// Deliver extra user context without interrupting the active work.
    /// Claude writes it onto stream-json stdin, Codex uses `turn/steer`, and
    /// Kimi queues it for the next ACP prompt boundary.
    Steer {
        text: String,
    },
    /// Apply settings to an already-live provider process. Claude supports
    /// model/permission changes over its control channel; Codex consumes the
    /// latest settings on the next turn instead.
    Settings {
        settings: ThreadSettings,
    },
    /// End an idle driver without emitting a turn boundary. Used when a
    /// process-launch setting changes and the next turn needs a fresh child.
    Retire,
    Interrupt,
    Approval {
        approval_id: String,
        option_id: String,
    },
    /// answers: question id -> selected labels / free text (single-element for
    /// single-select, may hold a free-text "Other" answer).
    Question {
        request_id: String,
        answers: HashMap<String, Vec<String>>,
    },
}

struct SessionHandle {
    cmd_tx: mpsc::UnboundedSender<AgentCommand>,
    /// Which provider process this live driver runs (see [`session_key`]) — a
    /// thread that has since switched lane, agent, or Claudex profile must not
    /// reuse it.
    key: (String, Option<String>),
}

enum RestartRecovery {
    Continue(Thread),
    ReattachHermes { thread: Thread, run_id: String },
}

const RESTART_CONTINUATION_PROMPT: &str =
    "Threadknot Rebooted - Continue where you left off.";

pub struct Hub {
    pub store: Arc<Store>,
    pub broadcast: broadcast::Sender<ServerMessage>,
    /// Raw frames relayed from PEER machines (already-serialized event /
    /// state.changed JSON with an `origin` machine id injected). Kept apart
    /// from `broadcast` so peernet can forward without re-typing, and so the
    /// origin tag can break relay loops: only origin-less (locally produced)
    /// frames ever get relayed onward.
    pub relay: broadcast::Sender<String>,
    pub usage: crate::usage::UsageState,
    /// Last known answer to "is there a newer master than this build?" plus the
    /// kick channel its poller waits on.
    pub updates: crate::update::UpdateState,
    pub sched: crate::schedules::SchedState,
    /// Per-thread MCP credentials for the agent-driven browser (see `mcp.rs`).
    pub mcp: Arc<crate::mcp::McpRegistry>,
    /// Registered remote Hermes gateways (settings + the hermes driver's
    /// URL/key lookup). Lives next to the store's other files.
    pub hermes: Arc<crate::hermes::HermesRegistry>,
    /// Live Online/Offline presence for each registered Hermes gateway,
    /// maintained by `hermes::spawn_status_poller`.
    pub hermes_status: crate::hermes::HermesStatusState,
    /// Claudex profiles: the Claude harness pointed at a non-Anthropic model.
    pub claudex: Arc<crate::claudex::ClaudexRegistry>,
    /// Named reviewer presets for Parley debates (personas.json).
    pub personas: Arc<crate::personas::PersonaRegistry>,
    /// User-installed MCP servers (mcp-servers.json). Every local driver reads
    /// this at spawn and injects the enabled entries alongside Threadknot's own
    /// browser server — see `library.rs`.
    pub library: Arc<crate::library::McpLibrary>,
    /// User-crafted appearance themes (Settings → Appearance). Machine-local,
    /// stored beside the other data-dir files.
    pub themes: Arc<crate::themes::ThemeStore>,
    /// Bridge processes started on behalf of Claudex profiles.
    pub claudex_sidecars: Arc<crate::claudex::SidecarSupervisor>,
    sessions: Mutex<HashMap<String, SessionHandle>>,
    /// Deliverable-file snapshot captured at each turn's start, keyed by thread.
    /// Diffed at the turn boundary to detect artifacts the agent produced.
    artifact_baselines: Mutex<HashMap<String, crate::artifacts::DeliverableSnapshot>>,
    /// Project-relative paths the agent explicitly registered via the
    /// `publish_artifact` MCP tool this turn, keyed by thread. The turn-end
    /// diff skips these so a published file is not re-emitted as "detected".
    artifact_published: Mutex<HashMap<String, std::collections::HashSet<String>>>,
    /// Expo push dispatcher for paired mobile devices. Attached after
    /// construction (it needs the tokio runtime); absent in unit tests.
    push: OnceLock<Arc<crate::push::PushService>>,
    /// Weak handle back to this hub. `emit_as` takes `&self`, but the parley
    /// scheduler it invokes at a turn boundary must call `start_turn_as`,
    /// which needs `Arc<Self>` — this is how it gets one.
    self_weak: Weak<Self>,
}

impl Hub {
    pub fn new(store: Arc<Store>, port: u16) -> Arc<Self> {
        let (tx, _) = broadcast::channel(4096);
        let (relay, _) = broadcast::channel(4096);
        let hermes = Arc::new(
            crate::hermes::HermesRegistry::open(store.dir())
                .expect("open hermes registry"),
        );
        let claudex = Arc::new(
            crate::claudex::ClaudexRegistry::open(store.dir()).expect("open claudex registry"),
        );
        let personas = Arc::new(
            crate::personas::PersonaRegistry::open(store.dir()).expect("open persona registry"),
        );
        let themes = Arc::new(
            crate::themes::ThemeStore::open(store.dir()).expect("open theme store"),
        );
        let library = Arc::new(
            crate::library::McpLibrary::open(store.dir()).expect("open MCP library"),
        );
        Arc::new_cyclic(|self_weak| Self {
            self_weak: self_weak.clone(),
            store,
            broadcast: tx,
            relay,
            hermes,
            hermes_status: crate::hermes::HermesStatusState::default(),
            claudex,
            personas,
            library,
            themes,
            claudex_sidecars: Arc::new(crate::claudex::SidecarSupervisor::default()),
            usage: crate::usage::UsageState::default(),
            updates: crate::update::UpdateState::default(),
            sched: crate::schedules::SchedState::default(),
            mcp: Arc::new(crate::mcp::McpRegistry::new(port)),
            sessions: Mutex::new(HashMap::new()),
            artifact_baselines: Mutex::new(HashMap::new()),
            artifact_published: Mutex::new(HashMap::new()),
            push: OnceLock::new(),
        })
    }

    pub fn attach_push(&self, push: Arc<crate::push::PushService>) {
        let _ = self.push.set(push);
    }

    pub fn push(&self) -> Option<&Arc<crate::push::PushService>> {
        self.push.get()
    }

    /// Mirror attention-worthy events to paired phones. Fired at the same
    /// boundary where the event is persisted, so delivery does not depend on
    /// any client holding a WebSocket open.
    fn push_for_event(
        &self,
        thread_id: &str,
        event: &AgentEvent,
        notice: Option<&crate::protocol::EventNotice>,
    ) {
        let Some(push) = self.push.get() else { return };
        let kind = match event {
            AgentEvent::TurnCompleted { .. } => crate::push::PushKind::TurnCompleted,
            AgentEvent::ApprovalRequest { .. } => crate::push::PushKind::ApprovalRequest,
            AgentEvent::QuestionRequest { .. } => crate::push::PushKind::QuestionRequest,
            AgentEvent::Error { .. } => crate::push::PushKind::Error,
            _ => return,
        };
        let Some(thread) = self.store.thread(thread_id) else { return };
        let project_name = self
            .store
            .project(&thread.project_id)
            .map(|p| p.name)
            .unwrap_or_default();
        // Devices subscribe per workspace, not per project: a workspace can
        // span machines, and its membership can change after a phone last
        // saved its list.
        let workspace_id = self
            .store
            .workspace_for_project(&thread.project_id)
            .unwrap_or_else(|| thread.project_id.clone());
        push.enqueue(crate::push::PushJob {
            kind,
            project_id: thread.project_id,
            workspace_id,
            project_name,
            thread_id: thread_id.to_string(),
            thread_title: thread.title,
            notice: notice.cloned(),
            only_device: None,
        });
    }

    /// Build notification copy only after persistence: a completion can then
    /// safely read the final assistant message and artifact/diff events that
    /// precede its boundary.
    fn notice_for_event(
        &self,
        thread_id: &str,
        event: &AgentEvent,
    ) -> Option<crate::protocol::EventNotice> {
        if !matches!(
            event,
            AgentEvent::TurnCompleted { .. }
                | AgentEvent::ApprovalRequest { .. }
                | AgentEvent::QuestionRequest { .. }
                | AgentEvent::Error { .. }
        ) {
            return None;
        }
        let thread = self.store.thread(thread_id)?;
        let project_name = self
            .store
            .project(&thread.project_id)
            .map(|project| project.name)
            .unwrap_or_default();
        let completion = matches!(event, AgentEvent::TurnCompleted { .. })
            .then(|| self.store.completion_notice_context(thread_id));
        crate::notices::for_event(
            &thread.title,
            &project_name,
            event,
            completion.as_ref(),
        )
    }

    /// Classify and close work orphaned by the previous Threadknot process.
    ///
    /// This is deliberately separate from [`Hub::new`]: the server calls it
    /// only AFTER successfully binding its port, so an accidental second
    /// instance cannot interrupt or duplicate the real instance's work.
    fn prepare_restart_recovery(&self) -> Vec<RestartRecovery> {
        let stale = self
            .store
            .list_projects()
            .into_iter()
            .flat_map(|project| self.store.list_threads(&project.id))
            .filter(|thread| thread.status != ThreadStatus::Idle)
            .collect::<Vec<_>>();
        let mut recoveries = Vec::new();
        for thread in stale {
            // Hermes runs live on a remote gateway and survive Threadknot itself.
            // Keep the existing status/card intact and reconnect to that exact
            // run rather than synthesizing a second user turn.
            if thread.agent == Agent::Hermes {
                if let Some(run_id) = thread.provider_run_id.clone() {
                    recoveries.push(RestartRecovery::ReattachHermes { thread, run_id });
                    continue;
                }
            }

            let was_running = thread.status == ThreadStatus::Running;
            tracing::warn!("closing orphaned turn {}", thread.id);
            self.emit(&thread.id, AgentEvent::TurnAborted);

            // Approval/question gates need the user's decision, never an
            // automatic answer. Local turns that were actively working get a
            // fresh continuation turn against their persisted native session.
            if was_running && thread.agent != Agent::Hermes {
                recoveries.push(RestartRecovery::Continue(thread));
            }
        }
        recoveries
    }

    /// Resume every turn that was actively working when Threadknot stopped.
    /// Called exactly once by the server after it owns the listening socket.
    pub fn recover_orphaned_threads(self: &Arc<Self>) {
        for recovery in self.prepare_restart_recovery() {
            match recovery {
                RestartRecovery::Continue(thread) => {
                    tracing::warn!("automatically continuing interrupted thread {}", thread.id);
                    if let Err(error) = self.start_turn(
                        &thread.id,
                        RESTART_CONTINUATION_PROMPT.into(),
                        Vec::new(),
                    ) {
                        self.emit(
                            &thread.id,
                            AgentEvent::Error {
                                message: format!(
                                    "could not automatically resume after restart: {error:#}"
                                ),
                            },
                        );
                    }
                }
                RestartRecovery::ReattachHermes { thread, run_id } => {
                    tracing::warn!(
                        "reattaching Hermes thread {} to remote run {run_id}",
                        thread.id
                    );
                    let Some(project) = self.store.project(&thread.project_id) else {
                        self.emit(
                            &thread.id,
                            AgentEvent::Error {
                                message: "could not reattach after restart: project is missing"
                                    .into(),
                            },
                        );
                        continue;
                    };
                    // A reattach resumes the lane that was mid-run when the app
                    // died, which `active_speaker` still names.
                    let lane = thread.speaking_participant();
                    let cwd = self
                        .store
                        .thread_working_dir(&thread)
                        .unwrap_or_else(|_| std::path::PathBuf::from(project.path));
                    self.session_cmd_tx(
                        &thread,
                        &lane,
                        cwd.to_string_lossy().into_owned(),
                        None,
                        None,
                        Some(run_id),
                    );
                }
            }
        }
    }

    /// Persist (unless transient) and fan out one agent event; keep thread
    /// status in sync. Unattributed — for the user's own messages and anything
    /// the server itself produces.
    pub fn emit(&self, thread_id: &str, event: AgentEvent) {
        self.emit_as(thread_id, None, event)
    }

    /// As [`Hub::emit`], attributing the event to the lane that produced it.
    /// Every driver reaches this through [`DriverCtx::emit`], so attribution
    /// costs the drivers nothing.
    pub fn emit_as(&self, thread_id: &str, speaker: Option<&str>, event: AgentEvent) {
        // Artifact detection brackets a turn: snapshot deliverables when it
        // begins, diff at its boundary. Producing runs BEFORE the boundary
        // event is persisted so artifact cards get lower seqs than the divider.
        match &event {
            // A mid-turn steering note must not re-baseline: files produced
            // before the note would silently vanish from artifact detection.
            AgentEvent::UserMessage { mid_turn: false, .. } => {
                self.capture_artifact_baseline(thread_id)
            }
            AgentEvent::TurnCompleted { .. } | AgentEvent::TurnAborted => {
                self.detect_artifacts(thread_id)
            }
            _ => {}
        }

        let (seq, ts) = if event.is_transient() {
            (-1, now_iso())
        } else {
            match self.store.append_event(thread_id, speaker, &event) {
                Ok((s, ts)) => (s as i64, ts),
                Err(e) => {
                    tracing::error!("persist event failed: {e:#}");
                    (-1, now_iso())
                }
            }
        };

        let status = match &event {
            // mid_turn notes don't change status: a steer while WaitingApproval
            // must not flip the thread back to Running.
            AgentEvent::TurnStarted { .. }
            | AgentEvent::UserMessage { mid_turn: false, .. } => Some(ThreadStatus::Running),
            AgentEvent::ApprovalRequest { .. } | AgentEvent::QuestionRequest { .. } => {
                Some(ThreadStatus::WaitingApproval)
            }
            AgentEvent::ApprovalResolved { .. } | AgentEvent::QuestionResolved { .. } => {
                Some(ThreadStatus::Running)
            }
            AgentEvent::TurnCompleted { .. }
            | AgentEvent::TurnAborted
            | AgentEvent::Error { .. } => Some(ThreadStatus::Idle),
            _ => None,
        };
        // Any lifecycle movement un-parks a settled thread — endings very
        // much included. The client's unread marker is live-only and
        // per-device, so a client that was disconnected when a settled
        // thread finished or crashed has nothing else to surface it; without
        // this, a failed run stays invisible in a collapsed shelf. Restart
        // recovery is not a counter-example: it re-enters through
        // `start_turn`, whose UserMessage/TurnStarted already un-park.
        let unparks = matches!(
            &event,
            AgentEvent::UserMessage { mid_turn: false, .. }
                | AgentEvent::TurnStarted { .. }
                | AgentEvent::ApprovalRequest { .. }
                | AgentEvent::QuestionRequest { .. }
                | AgentEvent::TurnCompleted { .. }
                | AgentEvent::TurnAborted
                | AgentEvent::Error { .. }
        );
        // Every unparking event also yields a status today, so the clear
        // below always runs. Adding one that doesn't would silently leave a
        // thread parked with news in it — fail loudly in dev instead.
        debug_assert!(
            !unparks || status.is_some(),
            "an unparking event must also set a status, or the un-park never runs",
        );
        if let Some(status) = status {
            // Folded into the status write so new activity costs one lock,
            // one projects.json flush and one broadcast rather than two.
            let updated = self.store.update_thread(thread_id, |t| {
                t.status = status;
                if unparks {
                    t.settled_at = None;
                    t.kept_active_at = None;
                }
            });
            // Scope the nudge to the owning project. An unscoped "threads"
            // change makes every client re-pull the ENTIRE catalog — project
            // list, workspaces (sidebar art and all), and a thread list per
            // project — and this fires dozens of times per turn. Naming the
            // project narrows that to one small thread.list.
            self.broadcast_state("threads", updated.ok().map(|t| t.project_id));
        }
        // Which lane owns this event's bookkeeping: the stamped speaker when
        // there is one, else whichever lane the thread has on the floor.
        let speaker_id = speaker.map(|s| s.to_string());
        let lane_of = move |t: &Thread| -> Participant {
            speaker_id
                .as_deref()
                .and_then(|id| t.participant(id))
                .unwrap_or_else(|| t.speaking_participant())
        };
        match &event {
            AgentEvent::SessionStarted {
                provider_session_id,
                agent,
                ..
            } => {
                let sid = provider_session_id.clone();
                let agent = *agent;
                let _ = self.store.update_thread(thread_id, |t| {
                    t.provider_session_id = Some(sid.clone());
                    let lane = lane_of(t);
                    // Stamp the sub-provider that owns this session id so a
                    // later profile switch can tell the anchor is not its own.
                    // A session reported for some OTHER agent kind than the
                    // lane runs (mid-switch) has no knowable profile — leaving
                    // it unstamped costs a seed, which is the safe direction.
                    let profile = (agent.unwrap_or(lane.agent) == lane.agent)
                        .then(|| lane_profile(&lane))
                        .flatten();
                    t.session_anchors
                        .entry(lane.id.clone())
                        .and_modify(|anchor| {
                            anchor.session_id = sid.clone();
                            anchor.profile = profile.clone();
                        })
                        .or_insert(SessionAnchor {
                            session_id: sid.clone(),
                            covered_until_seq: 0,
                            profile,
                        });
                });
            }
            // On any turn boundary the live provider session has absorbed every
            // event persisted so far — advance its coveredUntil so a later
            // handoff back to it only seeds the delta.
            AgentEvent::TurnCompleted { .. } | AgentEvent::TurnAborted if seq >= 0 => {
                let covered = seq as u64;
                let _ = self.store.update_thread(thread_id, |t| {
                    let lane = lane_of(t);
                    let primary = t.primary_participant();
                    if let Some(anchor) = t.session_anchors.get_mut(&lane.id) {
                        // Only the profile that ran the turn may claim it.
                        if anchor.profile == lane_profile(&lane) {
                            anchor.covered_until_seq = covered;
                        }
                    }
                    // A Reviewer never keeps the floor: hand it back to the
                    // Builder so the user's next composer message reaches the
                    // lane doing the work rather than the critic. This is the
                    // seed of the round scheduler.
                    if lane.role == ParticipantRole::Reviewer && lane.id != primary.id {
                        t.agent = primary.agent;
                        t.settings = primary.settings.clone();
                        t.active_speaker = Some(primary.id);
                    }
                });
                // A finished turn moved the subscription meters — refresh soon.
                self.usage.kick(false);
            }
            _ => {}
        }
        // A fatal driver error ends the turn with NO boundary event: restore
        // the floor to the Builder exactly as a boundary would — but WITHOUT
        // advancing any anchor, since the failed turn absorbed nothing — so
        // the thread doesn't keep wearing a dead reviewer's identity (and its
        // settings) into the next turn.
        if matches!(&event, AgentEvent::Error { .. }) {
            let _ = self.store.update_thread(thread_id, |t| {
                let primary = t.primary_participant();
                if t.active_speaker.as_deref() != Some(primary.id.as_str()) {
                    t.agent = primary.agent;
                    t.settings = primary.settings.clone();
                    t.active_speaker = Some(primary.id);
                }
            });
        }

        let notice = self.notice_for_event(thread_id, &event);
        self.push_for_event(thread_id, &event, notice.as_ref());

        let _ = self.broadcast.send(ServerMessage::Event {
            thread_id: thread_id.to_string(),
            seq,
            ts,
            speaker: speaker.map(|s| s.to_string()),
            event: event.clone(),
            notice,
        });

        // Parley scheduler: a turn boundary (or a fatal error) scores the
        // finished turn and seats the next speaker. Dead last, so every write
        // and broadcast for THIS turn has landed before the next one starts.
        let boundary = match &event {
            AgentEvent::TurnCompleted { .. } => Some(true),
            AgentEvent::TurnAborted | AgentEvent::Error { .. } => Some(false),
            _ => None,
        };
        if let (Some(completed), Some(hub)) = (boundary, self.self_weak.upgrade()) {
            hub.advance_parley(thread_id, completed, speaker);
        }
    }

    pub fn broadcast_state(&self, scope: &str, project_id: Option<String>) {
        let _ = self.broadcast.send(ServerMessage::StateChanged {
            scope: scope.to_string(),
            project_id,
        });
    }

    /// Record the deliverable files present when a turn starts, so the turn
    /// boundary can tell which the agent newly produced or changed.
    pub(super) fn capture_artifact_baseline(&self, thread_id: &str) {
        let Some(thread) = self.store.thread(thread_id) else {
            return;
        };
        let Ok(root) = self.store.thread_working_dir(&thread) else {
            return;
        };
        let baseline = crate::artifacts::scan_deliverables(&root);
        self.artifact_baselines
            .lock()
            .unwrap()
            .insert(thread_id.to_string(), baseline);
        // Fresh turn, fresh set of explicitly published paths.
        self.artifact_published.lock().unwrap().remove(thread_id);
    }

    /// Diff the deliverable files against the turn's baseline and register any
    /// plausible deliverables as artifacts (emitting a card + refresh per file).
    /// Fallback behind `publish_artifact`; see `artifacts::select_artifacts`.
    pub(super) fn detect_artifacts(&self, thread_id: &str) {
        let Some(baseline) = self.artifact_baselines.lock().unwrap().remove(thread_id) else {
            return;
        };
        let published = self
            .artifact_published
            .lock()
            .unwrap()
            .remove(thread_id)
            .unwrap_or_default();
        let Some(thread) = self.store.thread(thread_id) else {
            return;
        };
        let Some(project) = self.store.project(&thread.project_id) else {
            return;
        };
        let Ok(root) = self.store.thread_working_dir(&thread) else {
            return;
        };
        let current = crate::artifacts::scan_deliverables(&root);
        let indexed: std::collections::HashSet<String> = self
            .store
            .list_artifacts_for_thread(thread_id)
            .into_iter()
            .map(|a| a.rel_path)
            .collect();
        let mut candidates: Vec<crate::artifacts::Candidate> = current
            .iter()
            .filter(|(rel, now)| baseline.get(*rel).map(|b| b != *now).unwrap_or(true))
            .filter(|(rel, _)| !published.contains(*rel))
            .map(|(rel, _)| crate::artifacts::Candidate {
                rel: rel.clone(),
                existed: baseline.contains_key(rel),
                known: indexed.contains(rel),
            })
            .collect();
        candidates.sort_by(|a, b| a.rel.cmp(&b.rel));
        let (user_text, assistant_text) = self.artifact_turn_text(thread_id);
        let selected =
            crate::artifacts::select_artifacts(candidates, &user_text, &assistant_text);
        let source = agent_source(thread.agent);
        for rel in selected {
            self.produce_artifact(
                thread_id,
                &project,
                &root,
                &rel,
                source,
                None,
                None,
                "detected",
            );
        }
    }

    /// Explicit deliverable registration — the `publish_artifact` MCP tool
    /// (`mcp.rs`). The agent names a file it produced for the user; we snapshot
    /// and index it immediately (mid-turn card) and exempt it from the
    /// turn-end diff. Returns the indexed record.
    pub fn publish_artifact(
        &self,
        thread_id: &str,
        path: &str,
        title: Option<&str>,
        description: Option<&str>,
    ) -> anyhow::Result<crate::protocol::ArtifactRecord> {
        let thread = self
            .store
            .thread(thread_id)
            .ok_or_else(|| anyhow::anyhow!("unknown thread"))?;
        let project = self
            .store
            .project(&thread.project_id)
            .ok_or_else(|| anyhow::anyhow!("unknown project"))?;
        let working_dir = self.store.thread_working_dir(&thread)?;
        let project_root = std::fs::canonicalize(&working_dir).unwrap_or(working_dir);
        let raw = std::path::Path::new(path);
        let abs = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            project_root.join(raw)
        };
        // Resolve symlinks/`..` before deriving the display path; the file must
        // exist to be a deliverable anyway.
        let abs = std::fs::canonicalize(&abs)
            .map_err(|e| anyhow::anyhow!("cannot publish {path}: {e}"))?;
        let rel = abs
            .strip_prefix(&project_root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| abs.to_string_lossy().into_owned());
        let record = self
            .produce_artifact(
                thread_id,
                &project,
                &project_root,
                &rel,
                agent_source(thread.agent),
                title,
                description,
                "published",
            )
            .ok_or_else(|| {
                anyhow::anyhow!("cannot publish {path}: not a readable file within size limits")
            })?;
        self.artifact_published
            .lock()
            .unwrap()
            .entry(thread_id.to_string())
            .or_default()
            .insert(rel);
        Ok(record)
    }

    /// Conversation evidence for the current turn. The boundary event has not
    /// yet been persisted when detection runs, so walk backward to the previous
    /// boundary and collect the current user request and final agent prose.
    fn artifact_turn_text(&self, thread_id: &str) -> (String, String) {
        let mut user = Vec::new();
        let mut assistant = Vec::new();
        let events = self.store.read_events(thread_id);
        for persisted in events.iter().rev() {
            match &persisted.event {
                AgentEvent::TurnCompleted { .. } | AgentEvent::TurnAborted => break,
                AgentEvent::UserMessage { text, .. } => user.push(text.clone()),
                AgentEvent::AssistantMessage { text } => assistant.push(text.clone()),
                _ => {}
            }
        }
        (user.join("\n"), assistant.join("\n"))
    }

    /// Snapshot one produced deliverable, index it, and emit its chat card.
    /// `rel` is project-relative (or absolute for a published out-of-project
    /// file — `join` passes an absolute path through unchanged).
    #[allow(clippy::too_many_arguments)]
    fn produce_artifact(
        &self,
        thread_id: &str,
        project: &Project,
        root: &std::path::Path,
        rel: &str,
        source: &str,
        title: Option<&str>,
        description: Option<&str>,
        origin: &str,
    ) -> Option<crate::protocol::ArtifactRecord> {
        let abs = root.join(rel);
        let meta = match std::fs::metadata(&abs) {
            Ok(m) if m.is_file() => m,
            _ => return None,
        };
        if meta.len() > crate::artifacts::MAX_ARTIFACT_BYTES {
            return None;
        }
        let bytes = std::fs::read(&abs).ok()?;
        let ext = crate::artifacts::ext_of(std::path::Path::new(rel));
        let mime = crate::artifacts::deliverable_mime(&ext).unwrap_or("application/octet-stream");
        let name = title
            .map(str::to_string)
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| {
                std::path::Path::new(rel)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| rel.to_string())
            });

        let record = match self.store.upsert_artifact(
            thread_id,
            &project.id,
            rel,
            &name,
            mime,
            meta.len(),
            source,
            origin,
            description,
        ) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("artifact index failed: {e:#}");
                return None;
            }
        };
        if let Err(e) = self
            .store
            .write_artifact_snapshot(thread_id, &record.id, &ext, &bytes)
        {
            tracing::error!("artifact snapshot failed: {e:#}");
            return None;
        }
        self.emit(
            thread_id,
            AgentEvent::Artifact {
                id: record.id.clone(),
                name: record.name.clone(),
                rel_path: record.rel_path.clone(),
                mime_type: record.mime_type.clone(),
                size_bytes: record.size_bytes,
                op: record.op.clone(),
                origin: record.origin.clone(),
                description: record.description.clone(),
            },
        );
        self.broadcast_state("artifacts", Some(project.id.clone()));
        Some(record)
    }

    fn session_cmd_tx(
        self: &Arc<Self>,
        thread: &Thread,
        lane: &Participant,
        cwd: String,
        seed: Option<String>,
        resume_fallback_seed: Option<String>,
        recover_provider_run_id: Option<String>,
    ) -> mpsc::UnboundedSender<AgentCommand> {
        let key = session_key(lane);
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(h) = sessions.get(&thread.id) {
            if !h.cmd_tx.is_closed() && h.key == key {
                return h.cmd_tx.clone();
            }
        }
        let (tx, rx) = mpsc::unbounded_channel();
        // Fresh spawns resume this LANE's own session (per-participant anchor);
        // the seed carries whatever that session hasn't absorbed.
        let resume_session_id = usable_anchor(thread, lane).map(|a| a.session_id.clone());
        // Mint a per-thread MCP credential so this driver can reach the
        // browser tools (thread id doubles as the browser session key).
        let mcp_token = self.mcp.mint(&thread.id);
        let mcp_endpoint = self.mcp.endpoint();
        // Resolve the Claudex profile ONCE per spawn: the child's whole
        // environment (endpoint, token, config home) comes from it, so a later
        // registry edit must not half-apply to a running process.
        let claudex = match lane.agent {
            Agent::Claudex => self.claudex.profile(&lane.settings.model),
            _ => None,
        };
        let ctx = DriverCtx {
            hub: Arc::clone(self),
            thread_id: thread.id.clone(),
            participant_id: lane.id.clone(),
            agent: lane.agent,
            claudex,
            cwd,
            resume_session_id,
            seed,
            resume_fallback_seed,
            recover_provider_run_id,
            mcp_token,
            mcp_endpoint,
        };
        let agent = lane.agent;
        let thread_id = thread.id.clone();
        let hub = Arc::clone(self);
        let my_tx = tx.clone();
        tokio::spawn(async move {
            let result = match agent {
                // Claudex is the same harness on a different backend: one
                // driver, and the profile in `ctx` decides the environment.
                Agent::Claude | Agent::Claudex => claude::run(ctx, rx).await,
                Agent::Codex => codex::run(ctx, rx).await,
                Agent::Kimi => kimi::run(ctx, rx).await,
                Agent::Hermes => hermes::run(ctx, rx).await,
            };
            if let Err(e) = result {
                tracing::error!("driver for {thread_id} died: {e:#}");
                hub.emit(
                    &thread_id,
                    AgentEvent::Error {
                        message: format!("{e:#}"),
                    },
                );
            }
            // Only clean up if the registered handle is still OURS — an agent
            // switch may already have replaced it with a live driver.
            let mut sessions = hub.sessions.lock().unwrap();
            let ours = sessions
                .get(&thread_id)
                .map(|h| h.cmd_tx.same_channel(&my_tx))
                .unwrap_or(false);
            if ours {
                sessions.remove(&thread_id);
            }
            drop(sessions);
            if ours {
                let _ = hub
                    .store
                    .update_thread(&thread_id, |t| t.status = ThreadStatus::Idle);
                hub.broadcast_state("threads", None);
            }
        });
        sessions.insert(
            thread.id.clone(),
            SessionHandle {
                cmd_tx: tx.clone(),
                key,
            },
        );
        tx
    }

    /// Point the thread's NEXT turn at a different provider (Traycer-style
    /// mid-chat switch). The live driver (if any) keeps running until the next
    /// turn routes away from it; context carries over via the handoff seed.
    pub fn set_agent(
        self: &Arc<Self>,
        thread_id: &str,
        agent: Agent,
        settings: ThreadSettings,
    ) -> Result<Thread> {
        let thread = self
            .store
            .thread(thread_id)
            .ok_or_else(|| anyhow::anyhow!("unknown thread"))?;
        anyhow::ensure!(
            thread.status == ThreadStatus::Idle,
            "thread is busy (interrupt it first)"
        );
        let thread = self.store.update_thread(thread_id, |t| {
            t.agent = agent;
            t.settings = settings.clone();
            // Switching the thread's agent re-points the lane doing the work,
            // so the Builder follows it. Reviewer lanes are untouched — they
            // are separate participants with their own sessions.
            let primary_id = t.primary_participant().id;
            if let Some(p) = t.participants.iter_mut().find(|p| p.id == primary_id) {
                p.agent = agent;
                p.settings = settings.clone();
                p.name = agent.display_name().to_string();
            }
        })?;
        self.broadcast_state("threads", Some(thread.project_id.clone()));
        Ok(thread)
    }

    /// Persist thread settings and immediately forward them to the live driver.
    /// This matters for Claude: a stopped turn leaves its CLI process alive, so
    /// access/model changes must reach that process before the next prompt.
    pub fn set_settings(&self, thread_id: &str, settings: ThreadSettings) -> Result<Thread> {
        let before = self
            .store
            .thread(thread_id)
            .ok_or_else(|| anyhow::anyhow!("unknown thread"))?;
        // A Claudex "model" is a profile: changing it swaps the endpoint, the
        // credentials and the CLI's config home, none of which a running child
        // can adopt over the control channel. Retire the driver so the next
        // turn spawns against the new profile — but never mid-turn, where that
        // would silently drop work in flight.
        let switching_profile =
            before.agent == Agent::Claudex && before.settings.model != settings.model;
        // Claude in Chrome is selected at process launch; there is no live
        // control-channel equivalent. Retire an idle native-Claude child so
        // the next turn resumes with the newly selected launch flag.
        let switching_claude_chrome = before.agent == Agent::Claude
            && before.settings.claude_chrome != settings.claude_chrome;
        let respawn_required = switching_profile || switching_claude_chrome;
        if respawn_required {
            anyhow::ensure!(
                before.status == ThreadStatus::Idle,
                "thread is busy (interrupt it first)"
            );
        }
        let thread = self.store.update_thread(thread_id, |t| {
            t.settings = settings.clone();
            // The settings UI edits whichever lane currently holds the floor,
            // so persist onto that participant rather than always the Builder.
            let lane_id = t.speaking_participant().id;
            if let Some(p) = t.participants.iter_mut().find(|p| p.id == lane_id) {
                p.settings = settings.clone();
            }
        })?;
        if respawn_required {
            if let Some(handle) = self.sessions.lock().unwrap().remove(thread_id) {
                let _ = handle.cmd_tx.send(AgentCommand::Retire);
            }
        } else if let Some(handle) = self.sessions.lock().unwrap().get(thread_id) {
            let _ = handle.cmd_tx.send(AgentCommand::Settings { settings });
        }
        self.broadcast_state("threads", Some(thread.project_id.clone()));
        Ok(thread)
    }

    /// Run a turn on the thread's primary lane — what the composer does.
    ///
    /// A message typed by the user always addresses the Builder, never a
    /// Reviewer that happens to have spoken last: a reviewer is invoked, says
    /// its piece, and yields the floor (see the turn boundary in [`Hub::emit_as`]).
    ///
    /// During a parley the thread is busy almost continuously, so a message
    /// typed mid-debate would bounce off the idle check. Instead it QUEUES on
    /// the parley state: the scheduler pauses the debate at the next turn
    /// boundary, gives the user the floor on the Builder lane, and resumes.
    pub fn start_turn(
        self: &Arc<Self>,
        thread_id: &str,
        text: String,
        attachments: Vec<IncomingAttachment>,
    ) -> Result<()> {
        if let Some(thread) = self.store.thread(thread_id) {
            if thread.status != ThreadStatus::Idle && thread.parley.is_some() {
                anyhow::ensure!(
                    attachments.is_empty(),
                    "attachments can't queue mid-parley — wait for the floor"
                );
                self.store.update_thread(thread_id, |t| {
                    if let Some(parley) = t.parley.as_mut() {
                        parley.pending_user = Some(text.clone());
                    }
                })?;
                self.emit(
                    thread_id,
                    AgentEvent::Status {
                        text: "you have the floor next — the parley pauses after this turn"
                            .into(),
                    },
                );
                return Ok(());
            }
        }
        self.start_turn_as(thread_id, None, text, attachments, false)
    }

    /// Run a turn on a specific lane. `lane` of `None` means the primary
    /// Builder; `injected` marks the prompt as machine-issued (a role brief)
    /// rather than something the human typed.
    fn start_turn_as(
        self: &Arc<Self>,
        thread_id: &str,
        lane: Option<Participant>,
        text: String,
        attachments: Vec<IncomingAttachment>,
        injected: bool,
    ) -> Result<()> {
        let thread = self
            .store
            .thread(thread_id)
            .ok_or_else(|| anyhow::anyhow!("unknown thread"))?;
        let cwd = self
            .store
            .thread_working_dir(&thread)?
            .to_string_lossy()
            .into_owned();
        anyhow::ensure!(
            thread.status == ThreadStatus::Idle,
            "thread is busy (interrupt it first)"
        );

        // Give the lane the floor before anything reads the thread again:
        // `agent`/`settings` mirror the speaking lane so usage meters, titles,
        // artifact provenance and the UI badge all stay truthful mid-turn.
        let lane = lane.unwrap_or_else(|| thread.primary_participant());
        let thread = self.store.update_thread(thread_id, |t| {
            t.active_speaker = Some(lane.id.clone());
            t.agent = lane.agent;
            t.settings = lane.settings.clone();
        })?;

        let events = self.store.read_events(thread_id);

        // Give a brand-new thread an immediate, useful fallback title, then
        // replace it asynchronously with a generated one. Custom titles (for
        // example scheduled runs, or a manual rename before sending) are left
        // alone. The async job also compares against this exact seed before it
        // writes, so a manual rename while generation is running always wins.
        let title_seed = if events.is_empty() && thread.title == "New thread" {
            let seed = title::fallback_title(&text);
            let _ = self
                .store
                .update_thread(thread_id, |t| t.title = seed.clone());
            self.broadcast_state("threads", Some(thread.project_id.clone()));
            Some(seed)
        } else {
            None
        };
        let title_prompt = title_seed.as_ref().map(|_| {
            let labels = attachments
                .iter()
                .map(|attachment| {
                    format!(
                        "- {} ({}, {} bytes encoded)",
                        attachment.name,
                        attachment.mime_type,
                        attachment.data.len()
                    )
                })
                .collect::<Vec<_>>();
            title::build_prompt(&text, &labels)
        });

        // Legacy threads predate per-lane anchors: their single native session
        // has seen the whole (single-provider) history. The implicit Builder's
        // id is the agent's wire name, which is exactly the key those anchors
        // already used, so this keeps resolving after the re-key.
        let thread = if thread.session_anchors.is_empty() && thread.provider_session_id.is_some() {
            let covered = events.last().map(|e| e.seq).unwrap_or(0);
            self.store.update_thread(thread_id, |t| {
                if let Some(sid) = t.provider_session_id.clone() {
                    t.session_anchors.insert(
                        lane.id.clone(),
                        SessionAnchor {
                            session_id: sid,
                            covered_until_seq: covered,
                            profile: lane_profile(&lane),
                        },
                    );
                }
            })?
        } else {
            thread
        };

        // A dead or switched-away driver means the next spawn needs a handoff
        // seed: everything past what this lane's session absorbed.
        // Computed BEFORE emitting this turn's UserMessage (which is sent live).
        let needs_spawn = {
            let key = session_key(&lane);
            let sessions = self.sessions.lock().unwrap();
            !sessions
                .get(thread_id)
                .map(|h| !h.cmd_tx.is_closed() && h.key == key)
                .unwrap_or(false)
        };
        // Lane display names so the seed can attribute each assistant block to
        // the participant that produced it, not just to an agent kind.
        let lane_names: HashMap<String, String> = thread
            .participants_resolved()
            .into_iter()
            .map(|p| (p.id, p.name))
            .collect();
        let seed = if needs_spawn {
            let covered = usable_anchor(&thread, &lane).map(|a| a.covered_until_seq);
            match covered {
                // Anchor present and fully caught up -> native resume, no seed.
                Some(c) if events.last().map(|e| e.seq <= c).unwrap_or(true) => None,
                // Anchor behind (or brand-new provider) -> seed the delta.
                after => transcript::render(&events, after, &lane_names),
            }
        } else {
            None
        };
        // Native provider history is the preferred source of context after a
        // restart. Keep a complete Threadknot transcript alongside it so a stale,
        // deleted, or otherwise unresumable provider session can transparently
        // start fresh without stranding the conversation. This is computed
        // before the new UserMessage is persisted, avoiding a duplicate prompt.
        let resume_fallback_seed = if needs_spawn && usable_anchor(&thread, &lane).is_some() {
            transcript::render(&events, None, &lane_names)
        } else {
            None
        };

        // Persist attachment bytes to disk; the event log keeps only metadata so
        // the transcript can re-render thumbnails (served token-gated) on reload.
        use base64::Engine as _;
        let mut meta = Vec::new();
        let mut refs = Vec::new();
        for att in attachments {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(att.data.as_bytes())
                .map_err(|_| anyhow::anyhow!("invalid attachment encoding"))?;
            let m = self
                .store
                .write_attachment(thread_id, &att.name, &att.mime_type, &bytes)?;
            if let Some(path) = self.store.attachment_path(thread_id, &m.id) {
                refs.push(AttachmentRef {
                    name: m.name.clone(),
                    mime_type: m.mime_type.clone(),
                    path,
                });
            }
            meta.push(m);
        }

        self.emit(
            thread_id,
            AgentEvent::UserMessage {
                text: text.clone(),
                attachments: meta,
                mid_turn: false,
                injected,
            },
        );
        let tx = self.session_cmd_tx(
            &thread,
            &lane,
            cwd,
            seed,
            resume_fallback_seed,
            None,
        );
        tx.send(AgentCommand::User {
            text,
            settings: lane.settings.clone(),
            attachments: refs,
        })
        .map_err(|_| anyhow::anyhow!("agent session unavailable"))?;

        if let (Some(seed), Some(prompt)) = (title_seed, title_prompt) {
            let claudex = match lane.agent {
                Agent::Claudex => self.claudex.profile(&lane.settings.model),
                _ => None,
            };
            title::spawn_generation(
                Arc::clone(self),
                thread_id.to_string(),
                lane.agent,
                claudex,
                seed,
                prompt,
            );
        }
        Ok(())
    }

    /// Add (or reuse) a reviewer lane and hand it exactly one turn to argue
    /// with what the thread has produced so far.
    ///
    /// The reviewer reads the thread through the ordinary handoff seed, so it
    /// sees the real transcript — messages, tools that actually ran, diffs that
    /// actually landed — rather than a summary. Reusing an existing reviewer
    /// lane is deliberate: it keeps its own provider session, so a second review
    /// of the same thread only seeds what happened since the first.
    ///
    /// `access` omitted seats the reviewer with FULL control (no permission
    /// prompts); `read` / `edits` is the deliberate restriction.
    pub fn review(
        self: &Arc<Self>,
        thread_id: &str,
        spec: ReviewerSpec,
        instructions: Option<String>,
    ) -> Result<Participant> {
        let builder = self
            .store
            .thread(thread_id)
            .ok_or_else(|| anyhow::anyhow!("unknown thread"))?
            .primary_participant();
        let personality = spec.personality.clone();
        let lane = self.join_reviewer(thread_id, spec)?;
        self.start_turn_as(
            thread_id,
            Some(lane.clone()),
            review_brief(&builder, &lane, instructions.as_deref(), personality.as_deref()),
            Vec::new(),
            true,
        )?;
        Ok(lane)
    }

    /// Add (or find) the reviewer lane for a given setup without running a
    /// turn. Split from [`Hub::review`] so lane construction is testable without
    /// spawning a provider — and because the round scheduler will need to seat
    /// participants ahead of giving any of them the floor.
    pub fn join_reviewer(&self, thread_id: &str, spec: ReviewerSpec) -> Result<Participant> {
        let agent = spec.agent;
        let thread = self
            .store
            .thread(thread_id)
            .ok_or_else(|| anyhow::anyhow!("unknown thread"))?;
        anyhow::ensure!(
            thread.status == ThreadStatus::Idle,
            "thread is busy (interrupt it first)"
        );
        anyhow::ensure!(
            !self.store.read_events(thread_id).is_empty(),
            "nothing to review yet — this thread has no history"
        );
        let model = match spec.model {
            Some(m) if !m.trim().is_empty() => m,
            _ => default_review_model(agent)?,
        };
        let builder = thread.primary_participant();
        let name = spec
            .name
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("{} (reviewer)", agent.display_name()));

        // The lane's full settings, resolved up front so an identical setup
        // reuses its lane (and provider session) while a changed one seats a
        // fresh lane with its own context.
        let mut want = builder.settings.clone();
        want.model = model;
        // Reviewers default to FULL access — the same hands-off control the
        // builder runs with — because a reviewer that must ask permission for
        // every command stalls the whole debate on a human click. `read` /
        // `edits` is the deliberate opt-in restriction that makes it ask.
        want.access = spec.access.unwrap_or(Access::Full);
        // Plan mode would have it raise plan-approval cards, which a critic
        // has no business doing.
        want.mode = Mode::Build;
        match spec.effort {
            Some(e) => want.effort = Some(e),
            // Effort vocabularies are per-provider; a value inherited from a
            // different agent kind may not exist there.
            None if agent != builder.agent => want.effort = None,
            None => {}
        }

        let mut lane = match thread.participants_resolved().into_iter().find(|p| {
            p.role == ParticipantRole::Reviewer
                && p.agent == agent
                && p.settings.model == want.model
                && p.settings.access == want.access
                && p.settings.effort == want.effort
                // Personas are stable identities ON TOP of the setup: two
                // personas with identical settings are still two lanes.
                && p.persona == spec.persona
        }) {
            Some(existing) => existing,
            None => Participant {
                id: new_id(),
                agent,
                settings: want,
                role: ParticipantRole::Reviewer,
                persona: spec.persona.clone(),
                name: name.clone(),
                color: default_lane_color(thread.participants_resolved().len()),
            },
        };
        // A persona rename (or naming a previously anonymous lane) follows the
        // lane: same session, fresh display name.
        lane.name = name;

        let joining = lane.clone();
        self.store.update_thread(thread_id, |t| {
            // Materialize the implicit Builder on first use, so the room has
            // named lanes from here on.
            if t.participants.is_empty() {
                let resolved = t.participants_resolved();
                t.participants = resolved;
            }
            match t.participants.iter_mut().find(|p| p.id == joining.id) {
                Some(existing) => existing.name = joining.name.clone(),
                None => t.participants.push(joining.clone()),
            }
        })?;
        self.broadcast_state("threads", Some(thread.project_id.clone()));
        Ok(lane)
    }

    /// Seat one or more reviewer lanes and run a structured debate: each round
    /// every reviewer attacks the thread's work, the Builder answers the
    /// objectors, and the loop repeats until every reviewer concedes — at
    /// which point, with `execute`, the Builder implements what it conceded —
    /// or the round cap escalates the leftovers to the user.
    ///
    /// The scheduler is the deterministic state machine in [`parley_decide`],
    /// driven from the turn boundary in [`Hub::advance_parley`]; no LLM
    /// decides who speaks next.
    pub fn start_parley(
        self: &Arc<Self>,
        thread_id: &str,
        reviewers: Vec<ReviewerSpec>,
        max_rounds: u32,
        execute: bool,
        instructions: Option<String>,
    ) -> Result<Thread> {
        anyhow::ensure!(!reviewers.is_empty(), "pick at least one reviewer");
        anyhow::ensure!(reviewers.len() <= 4, "at most 4 reviewers");
        let max_rounds = max_rounds.clamp(1, 6);
        let thread = self
            .store
            .thread(thread_id)
            .ok_or_else(|| anyhow::anyhow!("unknown thread"))?;
        anyhow::ensure!(
            thread.parley.is_none(),
            "a parley is already running on this thread"
        );
        let mut lanes: Vec<Participant> = Vec::new();
        let mut personalities: HashMap<String, String> = HashMap::new();
        for spec in reviewers {
            let personality = spec
                .personality
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            let lane = self.join_reviewer(thread_id, spec)?;
            // The same setup twice is ONE lane, not a reviewer arguing with itself.
            if !lanes.iter().any(|l| l.id == lane.id) {
                if let Some(p) = personality {
                    personalities.insert(lane.id.clone(), p);
                }
                lanes.push(lane);
            }
        }
        anyhow::ensure!(!lanes.is_empty(), "pick at least one distinct reviewer");
        let builder = thread.primary_participant();
        let parley = ParleyState {
            lanes: lanes.iter().map(|l| l.id.clone()).collect(),
            round: 1,
            max_rounds,
            next: 0,
            objectors: Vec::new(),
            had_objections: false,
            execute,
            pending_user: None,
            in_flight: Some(ParleyFlight::Reviewer),
            personalities: personalities.clone(),
            wrap: None,
        };
        let thread = self.store.update_thread(thread_id, |t| t.parley = Some(parley))?;
        self.broadcast_state("threads", Some(thread.project_id.clone()));
        let names = lanes
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        self.emit(
            thread_id,
            AgentEvent::Status {
                text: format!(
                    "parley: {names} reviewing {} — up to {max_rounds} round{}{}",
                    builder.name,
                    if max_rounds == 1 { "" } else { "s" },
                    if execute {
                        " · implements whatever they agree on"
                    } else {
                        ""
                    },
                ),
            },
        );
        self.emit(
            thread_id,
            AgentEvent::Status {
                text: format!("— parley · round 1 of {max_rounds} —"),
            },
        );
        let first = lanes[0].clone();
        let others: Vec<String> = lanes[1..].iter().map(|l| l.name.clone()).collect();
        let brief = parley_review_brief(
            &builder.name,
            &first,
            &others,
            1,
            max_rounds,
            instructions.as_deref(),
            personalities.get(&first.id).map(|s| s.as_str()),
        );
        self.start_turn_as(thread_id, Some(first), brief, Vec::new(), true)?;
        Ok(thread)
    }

    /// A turn boundary (or a fatal error) with an active parley: score the
    /// finished turn against the schedule, then seat the next speaker. Runs at
    /// the END of `emit_as`, after the boundary event is persisted and
    /// broadcast, so clients observe turns in order and the next turn starts
    /// from an Idle thread. `speaker` is the lane the ENDED event belonged to
    /// — the thread's active_speaker has already been restored to the Builder
    /// by the boundary bookkeeping, so it cannot be trusted for scoring.
    fn advance_parley(self: &Arc<Self>, thread_id: &str, completed: bool, speaker: Option<&str>) {
        let Some(thread) = self.store.thread(thread_id) else {
            return;
        };
        let Some(mut parley) = thread.parley.clone() else {
            return;
        };
        let builder = thread.primary_participant();
        let speaker_id = speaker
            .map(|s| s.to_string())
            .unwrap_or_else(|| thread.speaking_participant().id);

        // Aborted or errored: the debate stops here. A message the user queued
        // mid-parley still gets its turn rather than vanishing with it.
        if !completed {
            let queued = parley.pending_user.take();
            self.end_parley_with_note(thread_id, "parley: stopped");
            if let Some(text) = queued {
                let _ = self.start_turn(thread_id, text, Vec::new());
            }
            return;
        }

        let conceded = (parley.in_flight == Some(ParleyFlight::Reviewer))
            .then(|| parley_verdict(&self.store.read_events(thread_id), &speaker_id));
        let flight = parley.in_flight;
        let action = parley_decide(&mut parley, flight, &speaker_id, conceded);

        // Persist the decision BEFORE seating the next turn, so a crash
        // between the two can never double-seat a speaker.
        let persisted = match &action {
            ParleyAction::End { .. } => self.store.update_thread(thread_id, |t| {
                t.parley = None;
                t.agent = builder.agent;
                t.settings = builder.settings.clone();
                t.active_speaker = Some(builder.id.clone());
            }),
            _ => self
                .store
                .update_thread(thread_id, |t| t.parley = Some(parley.clone())),
        };
        if let Err(e) = persisted {
            tracing::error!("parley state persist failed: {e:#}");
            return;
        }

        match action {
            ParleyAction::End { note } => {
                self.emit(thread_id, AgentEvent::Status { text: note });
                self.broadcast_state("threads", Some(thread.project_id.clone()));
            }
            ParleyAction::Review { lane_id } => {
                let Some(lane) = thread.participant(&lane_id) else {
                    self.end_parley_with_note(
                        thread_id,
                        "parley: ended — a reviewer lane vanished",
                    );
                    return;
                };
                if parley.next == 0 {
                    self.emit(
                        thread_id,
                        AgentEvent::Status {
                            text: format!(
                                "— parley · round {} of {} —",
                                parley.round, parley.max_rounds
                            ),
                        },
                    );
                }
                let others: Vec<String> = parley
                    .lanes
                    .iter()
                    .filter(|id| *id != &lane_id)
                    .filter_map(|id| thread.participant(id).map(|p| p.name))
                    .collect();
                let brief = parley_review_brief(
                    &builder.name,
                    &lane,
                    &others,
                    parley.round,
                    parley.max_rounds,
                    None,
                    parley.personalities.get(&lane_id).map(|s| s.as_str()),
                );
                if let Err(e) = self.start_turn_as(thread_id, Some(lane), brief, Vec::new(), true)
                {
                    self.end_parley_with_note(thread_id, &format!("parley: ended — {e:#}"));
                }
            }
            ParleyAction::Answer => {
                let names = parley
                    .objectors
                    .iter()
                    .filter_map(|id| thread.participant(id).map(|p| p.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                let brief = parley_answer_brief(&names, parley.round, parley.max_rounds);
                if let Err(e) =
                    self.start_turn_as(thread_id, Some(builder.clone()), brief, Vec::new(), true)
                {
                    self.end_parley_with_note(thread_id, &format!("parley: ended — {e:#}"));
                }
            }
            ParleyAction::Execute => {
                self.emit(
                    thread_id,
                    AgentEvent::Status {
                        text: "parley: converged — implementing the agreed fixes".into(),
                    },
                );
                if let Err(e) = self.start_turn_as(
                    thread_id,
                    Some(builder.clone()),
                    parley_execute_brief(),
                    Vec::new(),
                    true,
                ) {
                    self.end_parley_with_note(thread_id, &format!("parley: ended — {e:#}"));
                }
            }
            ParleyAction::Verdict => {
                let escalation = parley.wrap == Some(ParleyWrap::Escalation);
                self.emit(
                    thread_id,
                    AgentEvent::Status {
                        text: if escalation {
                            format!(
                                "parley: round cap ({}) reached — summarizing the open questions",
                                parley.max_rounds
                            )
                        } else {
                            "parley: converged — writing the agreed plan".into()
                        },
                    },
                );
                let brief = if escalation {
                    parley_escalation_brief(parley.max_rounds)
                } else {
                    parley_plan_brief()
                };
                if let Err(e) =
                    self.start_turn_as(thread_id, Some(builder.clone()), brief, Vec::new(), true)
                {
                    self.end_parley_with_note(thread_id, &format!("parley: ended — {e:#}"));
                }
            }
            ParleyAction::UserTurn { text } => {
                if let Err(e) = self.start_turn_as(thread_id, None, text, Vec::new(), false) {
                    self.end_parley_with_note(thread_id, &format!("parley: ended — {e:#}"));
                }
            }
        }
    }

    /// Clear the parley and hand the floor back to the Builder, with a note in
    /// the feed saying why. Idempotent and safe to call without one active.
    fn end_parley_with_note(&self, thread_id: &str, note: &str) {
        let Some(thread) = self.store.thread(thread_id) else {
            return;
        };
        let builder = thread.primary_participant();
        let project_id = thread.project_id.clone();
        let _ = self.store.update_thread(thread_id, |t| {
            t.parley = None;
            t.agent = builder.agent;
            t.settings = builder.settings.clone();
            t.active_speaker = Some(builder.id.clone());
        });
        self.emit(
            thread_id,
            AgentEvent::Status {
                text: note.to_string(),
            },
        );
        self.broadcast_state("threads", Some(project_id));
    }

    pub fn interrupt(&self, thread_id: &str) -> Result<()> {
        let delivered = self
            .sessions
            .lock()
            .unwrap()
            .get(thread_id)
            .map(|handle| handle.cmd_tx.send(AgentCommand::Interrupt).is_ok())
            .unwrap_or(false);
        if !delivered {
            if let Some(thread) = self.store.thread(thread_id) {
                if thread.status != ThreadStatus::Idle {
                    tracing::warn!("interrupt repaired orphaned busy thread {thread_id}");
                    self.emit(thread_id, AgentEvent::TurnAborted);
                } else if thread.parley.is_some() {
                    // Between parley turns (or a stalled schedule): cancel it.
                    self.end_parley_with_note(thread_id, "parley: stopped");
                }
            }
        }
        Ok(())
    }

    /// Deliver extra context without interrupting the running turn. Claude's
    /// stream-json stdin and Codex's `turn/steer` inject it immediately; Kimi's
    /// ACP surface has no steer method, so its driver queues the note and starts
    /// it automatically at the current prompt boundary.
    /// If the turn finished while the user was typing, degrade to a normal
    /// turn instead of surfacing a busy/idle race error.
    pub fn steer_turn(self: &Arc<Self>, thread_id: &str, text: String) -> Result<()> {
        let thread = self
            .store
            .thread(thread_id)
            .ok_or_else(|| anyhow::anyhow!("unknown thread"))?;
        anyhow::ensure!(
            matches!(
                thread.agent,
                Agent::Claude | Agent::Claudex | Agent::Codex | Agent::Kimi
            ),
            "this agent cannot accept a message while it is working — press Stop to interrupt"
        );
        if thread.status == ThreadStatus::Idle {
            return self.start_turn(thread_id, text, Vec::new());
        }
        let tx = self.live_handle(thread_id).ok_or_else(|| {
            anyhow::anyhow!("agent session unavailable — stop the turn and resend")
        })?;
        // Persist immediately for every provider. For Kimi this is also the
        // durable record of its locally queued follow-up; the ACP prompt is
        // launched later without emitting a duplicate user message.
        self.emit(
            thread_id,
            AgentEvent::UserMessage {
                text: text.clone(),
                attachments: vec![],
                mid_turn: true,
                injected: false,
            },
        );
        tx.send(AgentCommand::Steer { text })
            .map_err(|_| anyhow::anyhow!("agent session unavailable"))?;
        Ok(())
    }

    /// Sender for a driver that can still answer a pending request over the
    /// wire: it must be alive AND the thread mid-turn. A pending approval or
    /// question keeps the thread out of Idle for its whole life, so an Idle
    /// thread's card is a leftover from a driver that died (app restart) —
    /// even a live driver spawned since then has no matching control request.
    fn live_handle(&self, thread_id: &str) -> Option<mpsc::UnboundedSender<AgentCommand>> {
        let busy = self
            .store
            .thread(thread_id)
            .map(|t| t.status != ThreadStatus::Idle)
            .unwrap_or(false);
        if !busy {
            return None;
        }
        self.sessions
            .lock()
            .unwrap()
            .get(thread_id)
            .filter(|h| !h.cmd_tx.is_closed())
            .map(|h| h.cmd_tx.clone())
    }

    pub fn respond_approval(
        self: &Arc<Self>,
        thread_id: &str,
        approval_id: String,
        option_id: String,
    ) -> Result<()> {
        if let Some(tx) = self.live_handle(thread_id) {
            tx.send(AgentCommand::Approval {
                approval_id,
                option_id,
            })
            .map_err(|_| anyhow::anyhow!("agent session unavailable"))?;
            return Ok(());
        }
        self.respond_stale_approval(thread_id, approval_id, option_id)
    }

    /// Answer an approval whose owning driver is gone. The pending
    /// `can_use_tool` control request died with the process, so the answer
    /// cannot travel the wire; instead resolve the card in the event log and
    /// relay the decision as a fresh turn, which resumes the provider session
    /// (or seeds a new one) through the normal `start_turn` machinery.
    fn respond_stale_approval(
        self: &Arc<Self>,
        thread_id: &str,
        approval_id: String,
        option_id: String,
    ) -> Result<()> {
        let thread = self
            .store
            .thread(thread_id)
            .ok_or_else(|| anyhow::anyhow!("unknown thread"))?;
        anyhow::ensure!(
            thread.status == ThreadStatus::Idle,
            "thread is busy (interrupt it first)"
        );
        let mut request = None;
        let mut resolved = false;
        for rec in self.store.read_events(thread_id) {
            match rec.event {
                AgentEvent::ApprovalRequest {
                    approval_id: ref id,
                    ref approval_kind,
                    ref options,
                    ..
                } if *id == approval_id => request = Some((approval_kind.clone(), options.clone())),
                AgentEvent::ApprovalResolved {
                    approval_id: ref id,
                    ..
                } if *id == approval_id => resolved = true,
                _ => {}
            }
        }
        let (kind, options) = request.ok_or_else(|| anyhow::anyhow!("unknown approval"))?;
        anyhow::ensure!(!resolved, "approval already resolved");
        let tone = options
            .iter()
            .find(|o| o.id == option_id)
            .map(|o| o.tone.clone())
            .ok_or_else(|| anyhow::anyhow!("unknown approval option"))?;

        // Clear the card first so the UI recovers even if the relay turn fails.
        self.emit(
            thread_id,
            AgentEvent::ApprovalResolved {
                approval_id,
                option_id,
            },
        );
        // emit() treats a resolution as "turn continues"; there is no live
        // turn here, so put the thread back where start_turn expects it.
        let _ = self
            .store
            .update_thread(thread_id, |t| t.status = ThreadStatus::Idle);
        self.broadcast_state("threads", None);

        let approved = tone != "deny";
        let text = if kind == "plan" {
            if !approved {
                // "Keep planning": the user's next message IS the planning
                // conversation; nothing to relay.
                return Ok(());
            }
            let _ = self
                .store
                .update_thread(thread_id, |t| t.settings.mode = Mode::Build);
            "I approved the plan you presented, but the app restarted before the approval could \
             reach you. Exit plan mode and implement the plan now."
                .to_string()
        } else if approved {
            "The app restarted before the tool call you requested approval for could run. I \
             approve it — pick up where you left off and run it now."
                .to_string()
        } else {
            // Declined: the requesting turn is already dead; nothing to relay.
            return Ok(());
        };
        self.start_turn(thread_id, text, Vec::new())
    }

    pub fn respond_question(
        self: &Arc<Self>,
        thread_id: &str,
        request_id: String,
        answers: HashMap<String, Vec<String>>,
    ) -> Result<()> {
        if let Some(tx) = self.live_handle(thread_id) {
            tx.send(AgentCommand::Question {
                request_id,
                answers,
            })
            .map_err(|_| anyhow::anyhow!("agent session unavailable"))?;
            return Ok(());
        }
        self.respond_stale_question(thread_id, request_id, answers)
    }

    /// Same recovery as [`respond_stale_approval`], for AskUserQuestion /
    /// requestUserInput cards orphaned by a dead driver: resolve the card and
    /// relay the answers as a fresh (resumed) turn.
    fn respond_stale_question(
        self: &Arc<Self>,
        thread_id: &str,
        request_id: String,
        answers: HashMap<String, Vec<String>>,
    ) -> Result<()> {
        let thread = self
            .store
            .thread(thread_id)
            .ok_or_else(|| anyhow::anyhow!("unknown thread"))?;
        anyhow::ensure!(
            thread.status == ThreadStatus::Idle,
            "thread is busy (interrupt it first)"
        );
        let mut questions = None;
        let mut resolved = false;
        for rec in self.store.read_events(thread_id) {
            match rec.event {
                AgentEvent::QuestionRequest {
                    request_id: ref id,
                    questions: ref qs,
                } if *id == request_id => questions = Some(qs.clone()),
                AgentEvent::QuestionResolved {
                    request_id: ref id, ..
                } if *id == request_id => resolved = true,
                _ => {}
            }
        }
        let questions = questions.ok_or_else(|| anyhow::anyhow!("unknown question"))?;
        anyhow::ensure!(!resolved, "question already answered");

        self.emit(
            thread_id,
            AgentEvent::QuestionResolved {
                request_id,
                answers: Some(answers.clone()),
            },
        );
        let _ = self
            .store
            .update_thread(thread_id, |t| t.status = ThreadStatus::Idle);
        self.broadcast_state("threads", None);

        // Answer keys are provider answer ids (question text for Claude, ids
        // for Codex) — map them back to readable question text for the relay.
        let mut lines = vec![
            "The app restarted before my answers to your questions could reach you. My answers:"
                .to_string(),
        ];
        for (key, labels) in &answers {
            let text = questions
                .iter()
                .find(|q| q.id == *key)
                .map(|q| q.question.as_str())
                .unwrap_or(key.as_str());
            lines.push(format!("- {text}: {}", labels.join(", ")));
        }
        self.start_turn(thread_id, lines.join("\n"), Vec::new())
    }

    pub fn stop_session(&self, thread_id: &str) {
        self.sessions.lock().unwrap().remove(thread_id);
        self.mcp.revoke_thread(thread_id);
    }
}

/// Context handed to a driver task.
pub struct DriverCtx {
    pub hub: Arc<Hub>,
    pub thread_id: String,
    /// The lane this driver is: keys its session anchor and stamps every event
    /// it emits. Equal to [`Agent::wire_id`] for a thread that never gained a
    /// second participant.
    pub participant_id: String,
    /// The lane's agent kind. Drivers shared by more than one kind (Claude
    /// and Claudex) tag their events with this rather than a constant.
    pub agent: Agent,
    /// The Claudex profile backing this session, resolved at spawn. `None` for
    /// every other agent kind, including a genuinely Anthropic Claude.
    pub claudex: Option<crate::claudex::ClaudexProfile>,
    pub cwd: String,
    pub resume_session_id: Option<String>,
    /// Handoff transcript to prepend to the FIRST user message (mid-thread
    /// agent switch, or catching a resumed session up on missed turns).
    pub seed: Option<String>,
    /// Complete transcript used only when a requested native provider session
    /// cannot be resumed. This keeps a persisted Threadknot thread usable even if
    /// the provider's own session store was removed or reset.
    pub resume_fallback_seed: Option<String>,
    /// Remote Hermes run to reconnect to after Threadknot restarts. Local drivers
    /// ignore this because their child processes cannot survive the restart.
    pub recover_provider_run_id: Option<String>,
    /// Bearer token this driver presents to the `/mcp` browser-tools endpoint.
    pub mcp_token: String,
    /// URL of the `/mcp` endpoint (loopback).
    pub mcp_endpoint: String,
}

impl DriverCtx {
    pub fn emit(&self, event: AgentEvent) {
        self.hub
            .emit_as(&self.thread_id, Some(&self.participant_id), event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store(label: &str) -> (PathBuf, Arc<Store>, Project, ThreadSettings) {
        let root = std::env::temp_dir().join(format!("threadknot-{label}-{}", uuid::Uuid::new_v4()));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let store = Arc::new(Store::open_for_test(root.join("data")).unwrap());
        let project = store
            .create_project(workspace.to_string_lossy().into_owned(), Some(label.into()))
            .unwrap();
        let settings = ThreadSettings {
            model: "claude-fable-5".into(),
            effort: Some("low".into()),
            wide_context: false,
            claude_chrome: false,
            access: Access::Read,
            mode: Mode::Build,
            browser_profile_id: None,
        };
        (root, store, project, settings)
    }

    #[test]
    fn materialize_docs_copies_non_images_and_skips_images() {
        let dir = std::env::temp_dir().join(format!("threadknot-mat-{}", uuid::Uuid::new_v4()));
        let src = dir.join("src");
        let cwd = dir.join("workspace");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();

        // The on-disk stem is the attachment id (UUID), mirroring the store.
        let pdf_path = src.join("11111111-1111-1111-1111-111111111111.bin");
        std::fs::write(&pdf_path, b"%PDF-1.7 hello").unwrap();
        let png_path = src.join("22222222-2222-2222-2222-222222222222.png");
        std::fs::write(&png_path, [0x89u8, 0x50, 0x4e, 0x47]).unwrap();

        let refs = vec![
            AttachmentRef {
                name: "report.pdf".into(),
                mime_type: "application/pdf".into(),
                path: pdf_path,
            },
            AttachmentRef {
                name: "shot.png".into(),
                mime_type: "image/png".into(),
                path: png_path,
            },
        ];

        let docs = materialize_docs(cwd.to_str().unwrap(), &refs);
        // Image skipped; only the PDF is materialized.
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].mime_type, "application/pdf");
        assert_eq!(
            docs[0].rel_path,
            ".threadknot/attachments/11111111-1111-1111-1111-111111111111/report.pdf"
        );
        // The bytes actually land inside the workspace at that relative path.
        let landed = cwd.join(&docs[0].rel_path);
        assert_eq!(std::fs::read(&landed).unwrap(), b"%PDF-1.7 hello");

        let footer = attachment_footer(&docs);
        assert!(footer.contains("[Attached files"));
        assert!(footer.contains("./.threadknot/attachments/11111111-1111-1111-1111-111111111111/report.pdf (application/pdf)"));
        assert!(attachment_footer(&[]).is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn changing_claude_chrome_retires_an_idle_driver() {
        let (root, store, project, settings) = test_store("claude-chrome-retire");
        let thread = store
            .create_thread(project.id, Agent::Claude, settings.clone())
            .unwrap();
        let hub = Hub::new(Arc::clone(&store), 42800);
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        hub.sessions.lock().unwrap().insert(
            thread.id.clone(),
            SessionHandle {
                cmd_tx,
                key: ("claude".into(), None),
            },
        );

        let mut enabled = settings;
        enabled.claude_chrome = true;
        let updated = hub.set_settings(&thread.id, enabled).unwrap();

        assert!(updated.settings.claude_chrome);
        assert!(matches!(cmd_rx.try_recv(), Ok(AgentCommand::Retire)));
        assert!(!hub.sessions.lock().unwrap().contains_key(&thread.id));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn safe_file_name_strips_paths_and_separators() {
        assert_eq!(safe_file_name("report.pdf"), "report.pdf");
        assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
        // Only the last path component survives; its ':' is neutralized.
        assert_eq!(safe_file_name("a/b\\c:d"), "c_d");
        assert_eq!(safe_file_name("   "), "file");
        assert_eq!(safe_file_name("..."), "file");
    }

    /// A Claudex "model" is a profile, and each profile pins its own gateway
    /// and CLAUDE_CONFIG_DIR. The session id from one profile does not exist
    /// in another's config home, so an anchor stamped with a different profile
    /// must read as absent rather than produce an unresumable `--resume`.
    #[test]
    fn claudex_anchors_and_drivers_are_scoped_to_one_profile() {
        let (root, store, project, mut settings) = test_store("claudex-anchor");
        settings.model = "profile-a".into();
        let thread = store
            .create_thread(project.id.clone(), Agent::Claudex, settings.clone())
            .unwrap();
        // The implicit lane's id is the agent wire name, which is exactly the
        // key pre-Parley anchors used.
        let lane = thread.primary_participant();
        assert_eq!(lane.id, "claudex");
        assert_eq!(
            session_key(&lane),
            ("claudex".to_string(), Some("profile-a".into()))
        );

        let thread = store
            .update_thread(&thread.id, |t| {
                t.session_anchors.insert(
                    "claudex".into(),
                    SessionAnchor {
                        session_id: "sess-b".into(),
                        covered_until_seq: 7,
                        profile: Some("profile-b".into()),
                    },
                );
            })
            .unwrap();
        assert!(usable_anchor(&thread, &lane).is_none());

        let thread = store
            .update_thread(&thread.id, |t| {
                t.session_anchors.get_mut("claudex").unwrap().profile =
                    Some("profile-a".into());
            })
            .unwrap();
        assert_eq!(
            usable_anchor(&thread, &lane).map(|a| a.covered_until_seq),
            Some(7)
        );

        // Agents with a single backend keep the plain, profile-less key — and
        // a legacy anchor written before this field still resumes.
        let claude = store
            .create_thread(project.id, Agent::Claude, settings)
            .unwrap();
        let claude_lane = claude.primary_participant();
        assert_eq!(session_key(&claude_lane), ("claude".to_string(), None));
        let claude = store
            .update_thread(&claude.id, |t| {
                t.session_anchors.insert(
                    "claude".into(),
                    SessionAnchor {
                        session_id: "sess-legacy".into(),
                        covered_until_seq: 3,
                        profile: None,
                    },
                );
            })
            .unwrap();
        assert!(usable_anchor(&claude, &claude_lane).is_some());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The re-key from agent-kind to participant id has to be a NO-OP on disk:
    /// a `threads.json` written before Parley keys anchors by the agent's wire
    /// name, and those threads must keep resuming natively rather than silently
    /// falling back to a seeded fresh session.
    #[test]
    fn pre_parley_anchors_still_resolve_after_the_rekey() {
        let legacy = serde_json::json!({
            "id": "t1",
            "projectId": "p1",
            "machineId": "m1",
            "agent": "claude",
            "title": "old thread",
            "settings": {
                "model": "claude-opus-5",
                "wideContext": false,
                "access": "full",
                "mode": "build"
            },
            "providerSessionId": "sess-legacy",
            // Exactly how anchors were persisted before participants existed.
            "sessionAnchors": {
                "claude": { "sessionId": "sess-legacy", "coveredUntilSeq": 12 }
            },
            "status": "idle",
            "createdAt": "2026-07-01T00:00:00Z",
            "updatedAt": "2026-07-01T00:00:00Z"
        });
        let thread: Thread = serde_json::from_value(legacy).unwrap();
        assert!(thread.participants.is_empty(), "no participants on disk");
        let lane = thread.primary_participant();
        assert_eq!(lane.id, "claude");
        assert_eq!(lane.role, ParticipantRole::Builder);
        let anchor = usable_anchor(&thread, &lane).expect("legacy anchor resolves");
        assert_eq!(anchor.session_id, "sess-legacy");
        assert_eq!(anchor.covered_until_seq, 12);
    }

    /// `thread.review` must produce a lane that is genuinely SEPARATE from the
    /// builder — its own id, its own session key — even when it runs the very
    /// same agent and model. Sharing a session key would hand the reviewer the
    /// builder's live process and its context, which is the one thing an
    /// independent review cannot have.
    #[test]
    fn review_adds_a_full_access_lane_that_never_shares_the_builders_session() {
        let (root, store, project, settings) = test_store("review-lane");
        let thread = store
            .create_thread(project.id, Agent::Claude, settings.clone())
            .unwrap();
        let hub = Hub::new(Arc::clone(&store), 42800);

        // Nothing to review yet.
        assert!(hub
            .join_reviewer(&thread.id, ReviewerSpec::new(Agent::Claude))
            .is_err());

        // Give the thread a turn of history.
        hub.emit(
            &thread.id,
            AgentEvent::UserMessage {
                text: "add the feature".into(),
                attachments: vec![],
                mid_turn: false,
                injected: false,
            },
        );
        hub.emit_as(
            &thread.id,
            Some("claude"),
            AgentEvent::AssistantMessage {
                text: "Done.".into(),
            },
        );
        hub.emit_as(&thread.id, Some("claude"), AgentEvent::TurnCompleted { usage: None });

        // Same agent kind AND same model as the builder — the hard case.
        let reviewer = hub
            .join_reviewer(
                &thread.id,
                ReviewerSpec {
                    model: Some(settings.model.clone()),
                    ..ReviewerSpec::new(Agent::Claude)
                },
            )
            .unwrap();

        let after = store.thread(&thread.id).unwrap();
        let builder = after.primary_participant();
        assert_eq!(builder.id, "claude", "builder keeps the legacy anchor key");
        assert_ne!(reviewer.id, builder.id);
        assert_eq!(reviewer.role, ParticipantRole::Reviewer);
        assert_eq!(
            reviewer.settings.access,
            Access::Full,
            "reviewers run hands-off unless the client restricts them"
        );
        assert_eq!(reviewer.settings.mode, Mode::Build, "no plan cards from a critic");
        assert_ne!(
            session_key(&reviewer),
            session_key(&builder),
            "a shared session key would leak the builder's context into its own review"
        );
        // Both lanes are now materialized and named.
        assert_eq!(after.participants.len(), 2);

        // The read-only brief names the builder, forbids editing, and — the
        // line that keeps reviews honest — permits finding nothing.
        let mut read_only = reviewer.clone();
        read_only.settings.access = Access::Read;
        let brief = review_brief(&builder, &read_only, Some("focus on error handling"), None);
        assert!(brief.contains("ADVERSARIAL REVIEW"));
        assert!(brief.contains("READ-ONLY"));
        assert!(brief.contains(&builder.name));
        assert!(brief.contains("no material objection"));
        assert!(brief.contains("focus this review on:\nfocus on error handling"));
        assert!(
            !review_brief(&builder, &read_only, Some("   "), None).contains("focus this review on")
        );

        // A full-access reviewer gets a brief that doesn't lie about its powers.
        let writable = review_brief(&builder, &reviewer, None, None);
        assert!(!writable.contains("READ-ONLY"));
        assert!(writable.contains("write access"));

        // A persona's voice is folded in right after the framing.
        let persona_brief = review_brief(
            &builder,
            &reviewer,
            None,
            Some("You doubt everything."),
        );
        assert!(persona_brief.contains("You review as Claude Code (reviewer): You doubt everything."));

        // Joining again reuses the SAME lane, so its provider session (and
        // therefore its coveredUntil) survives instead of starting over.
        let again = hub
            .join_reviewer(
                &thread.id,
                ReviewerSpec {
                    model: Some(settings.model.clone()),
                    ..ReviewerSpec::new(Agent::Claude)
                },
            )
            .unwrap();
        assert_eq!(again.id, reviewer.id);
        assert_eq!(store.thread(&thread.id).unwrap().participants.len(), 2);

        // A different model is a different critic: its own lane and session.
        let other = hub
            .join_reviewer(
                &thread.id,
                ReviewerSpec {
                    model: Some("gpt-5.5".into()),
                    ..ReviewerSpec::new(Agent::Codex)
                },
            )
            .unwrap();
        assert_ne!(other.id, reviewer.id);
        assert_eq!(store.thread(&thread.id).unwrap().participants.len(), 3);

        // A different access level is ALSO a different lane: re-running the
        // same critic restricted to read-only must not silently reuse the
        // full-access lane (or its session) under the old settings.
        let restricted_lane = hub
            .join_reviewer(
                &thread.id,
                ReviewerSpec {
                    model: Some(settings.model.clone()),
                    effort: Some("high".into()),
                    access: Some(Access::Read),
                    ..ReviewerSpec::new(Agent::Claude)
                },
            )
            .unwrap();
        assert_ne!(restricted_lane.id, reviewer.id);
        assert_eq!(restricted_lane.settings.access, Access::Read);
        assert_eq!(restricted_lane.settings.effort.as_deref(), Some("high"));
        assert_eq!(store.thread(&thread.id).unwrap().participants.len(), 4);

        drop(hub);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A persona names its lane ("The Skeptic", not "Claude Code (reviewer)"),
    /// and its ID is the lane's identity: re-running the same persona reuses
    /// the lane under its latest name, while two personas on the IDENTICAL
    /// setup seat two separate lanes with two separate sessions.
    #[test]
    fn a_persona_names_its_lane_and_owns_its_identity() {
        let (root, store, project, settings) = test_store("review-persona");
        let thread = store
            .create_thread(project.id, Agent::Claude, settings.clone())
            .unwrap();
        let hub = Hub::new(Arc::clone(&store), 42800);
        hub.emit(
            &thread.id,
            AgentEvent::UserMessage {
                text: "add the feature".into(),
                attachments: vec![],
                mid_turn: false,
                injected: false,
            },
        );
        hub.emit_as(&thread.id, Some("claude"), AgentEvent::TurnCompleted { usage: None });

        let skeptic = hub
            .join_reviewer(
                &thread.id,
                ReviewerSpec {
                    model: Some(settings.model.clone()),
                    name: Some("The Skeptic".into()),
                    persona: Some("persona-skeptic".into()),
                    personality: Some("You doubt everything.".into()),
                    ..ReviewerSpec::new(Agent::Claude)
                },
            )
            .unwrap();
        assert_eq!(skeptic.name, "The Skeptic");
        assert_eq!(skeptic.persona.as_deref(), Some("persona-skeptic"));

        // Same persona id, renamed: same lane, fresh name.
        let renamed = hub
            .join_reviewer(
                &thread.id,
                ReviewerSpec {
                    model: Some(settings.model.clone()),
                    name: Some("Chief Doubter".into()),
                    persona: Some("persona-skeptic".into()),
                    ..ReviewerSpec::new(Agent::Claude)
                },
            )
            .unwrap();
        assert_eq!(renamed.id, skeptic.id, "same persona reuses its lane");
        assert_eq!(renamed.name, "Chief Doubter");

        // A DIFFERENT persona on the identical setup: its own lane. This is
        // the case that makes "The Skeptic" and "The Auditor" two voices
        // instead of one renamed lane.
        let auditor = hub
            .join_reviewer(
                &thread.id,
                ReviewerSpec {
                    model: Some(settings.model.clone()),
                    name: Some("The Auditor".into()),
                    persona: Some("persona-auditor".into()),
                    ..ReviewerSpec::new(Agent::Claude)
                },
            )
            .unwrap();
        assert_ne!(auditor.id, skeptic.id);
        assert_ne!(session_key(&auditor), session_key(&skeptic));
        assert_eq!(
            store.thread(&thread.id).unwrap().participants.len(),
            3,
            "builder + two persona lanes"
        );

        drop(hub);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A reviewer says its piece and yields: the turn boundary hands the floor
    /// back to the builder so the user's next composer message reaches the lane
    /// doing the work, not the critic. The reviewer's own anchor still advances.
    #[test]
    fn a_reviewer_yields_the_floor_at_the_turn_boundary() {
        let (root, store, project, settings) = test_store("review-yield");
        let thread = store
            .create_thread(project.id, Agent::Claude, settings.clone())
            .unwrap();
        let hub = Hub::new(Arc::clone(&store), 42800);

        let builder = thread.primary_participant();
        let mut reviewer_settings = settings.clone();
        reviewer_settings.access = Access::Read;
        reviewer_settings.model = "gpt-5.5".into();
        let reviewer = Participant {
            id: "rev-1".into(),
            agent: Agent::Codex,
            settings: reviewer_settings,
            role: ParticipantRole::Reviewer,
            persona: None,
            name: "Codex (reviewer)".into(),
            color: default_lane_color(1),
        };
        store
            .update_thread(&thread.id, |t| {
                t.participants = vec![builder.clone(), reviewer.clone()];
                // The reviewer is mid-turn: thread-level agent/settings mirror it.
                t.active_speaker = Some("rev-1".into());
                t.agent = Agent::Codex;
                t.settings = reviewer.settings.clone();
                t.session_anchors.insert(
                    "rev-1".into(),
                    SessionAnchor {
                        session_id: "codex-sess".into(),
                        covered_until_seq: 0,
                        profile: None,
                    },
                );
            })
            .unwrap();

        hub.emit_as(&thread.id, Some("rev-1"), AgentEvent::TurnCompleted { usage: None });

        let after = store.thread(&thread.id).unwrap();
        assert_eq!(after.agent, Agent::Claude, "floor returns to the builder");
        assert_eq!(after.settings.model, settings.model);
        assert_eq!(after.settings.access, settings.access);
        assert_eq!(after.active_speaker.as_deref(), Some("claude"));
        // The reviewer's session still absorbed the turn it just ran.
        let seq = store.read_events(&thread.id).last().unwrap().seq;
        assert_eq!(
            after.session_anchors.get("rev-1").unwrap().covered_until_seq,
            seq
        );
        // ...and the builder's lane was not credited with the reviewer's turn.
        assert!(!after.session_anchors.contains_key("claude"));

        drop(hub);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    // ---- Parley: the round scheduler ------------------------------------

    fn parley_state(lanes: &[&str]) -> ParleyState {
        ParleyState {
            lanes: lanes.iter().map(|s| s.to_string()).collect(),
            round: 1,
            max_rounds: 2,
            next: 0,
            objectors: Vec::new(),
            had_objections: false,
            execute: false,
            pending_user: None,
            in_flight: Some(ParleyFlight::Reviewer),
            personalities: HashMap::new(),
            wrap: None,
        }
    }

    /// The whole debate arc: reviewers object, the builder answers, the next
    /// round re-verifies, and the round cap — not an endless loop — ends a
    /// debate that never converges.
    #[test]
    fn parley_decide_runs_rounds_until_the_cap() {
        let mut p = parley_state(&["r1", "r2"]);

        // Round 1: r1 objects → r2 speaks next.
        let a = parley_decide(&mut p, Some(ParleyFlight::Reviewer), "r1", Some(false));
        assert_eq!(a, ParleyAction::Review { lane_id: "r2".into() });
        assert_eq!(p.next, 1);
        assert!(p.had_objections);
        assert_eq!(p.objectors, vec!["r1".to_string()]);

        // r2 concedes → the round is over, and r1's objection sends the
        // builder in to answer.
        let a = parley_decide(&mut p, Some(ParleyFlight::Reviewer), "r2", Some(true));
        assert_eq!(a, ParleyAction::Answer);
        assert_eq!(p.objectors, vec!["r1".to_string()]);

        // The answer completes round 1 → round 2 re-seats r1 with a clean
        // objector slate.
        let a = parley_decide(&mut p, Some(ParleyFlight::Answer), "claude", None);
        assert_eq!(a, ParleyAction::Review { lane_id: "r1".into() });
        assert_eq!(p.round, 2);
        assert!(p.objectors.is_empty());

        // Round 2: r1 concedes, r2 raises something new → another answer.
        let a = parley_decide(&mut p, Some(ParleyFlight::Reviewer), "r1", Some(true));
        assert_eq!(a, ParleyAction::Review { lane_id: "r2".into() });
        let a = parley_decide(&mut p, Some(ParleyFlight::Reviewer), "r2", Some(false));
        assert_eq!(a, ParleyAction::Answer);

        // The round-2 answer hits the cap: not a stop — the closing turn that
        // summarizes the open questions.
        let a = parley_decide(&mut p, Some(ParleyFlight::Answer), "claude", None);
        assert_eq!(a, ParleyAction::Verdict);
        assert_eq!(p.wrap, Some(ParleyWrap::Escalation));

        // The user interjecting during the closing turn speaks first, and the
        // summary is re-seated so their message is part of it.
        p.pending_user = Some("go with option 2".into());
        let a = parley_decide(&mut p, Some(ParleyFlight::User), "claude", None);
        assert_eq!(a, ParleyAction::UserTurn { text: "go with option 2".into() });
        let a = parley_decide(&mut p, Some(ParleyFlight::User), "claude", None);
        assert_eq!(a, ParleyAction::Verdict);

        // The closing turn completes → the escalation note, not silence.
        match parley_decide(&mut p, Some(ParleyFlight::Verdict), "claude", None) {
            ParleyAction::End { note } => assert!(note.contains("open questions"), "{note}"),
            other => panic!("expected End after the closing turn, got {other:?}"),
        }
    }

    /// Convergence: a clean round after objections (with execute on) seats the
    /// builder's implement-what-you-conceded turn; a debate nobody ever
    /// objected in ends with a note; and execute=off seats the closing turn
    /// that writes the agreed plan — the parley always leaves a deliverable.
    #[test]
    fn parley_decide_converges_into_execution() {
        let mut p = parley_state(&["r1"]);
        p.execute = true;
        let a = parley_decide(&mut p, Some(ParleyFlight::Reviewer), "r1", Some(false));
        assert_eq!(a, ParleyAction::Answer);
        let a = parley_decide(&mut p, Some(ParleyFlight::Answer), "claude", None);
        assert_eq!(a, ParleyAction::Review { lane_id: "r1".into() });
        let a = parley_decide(&mut p, Some(ParleyFlight::Reviewer), "r1", Some(true));
        assert_eq!(a, ParleyAction::Execute, "converged with conceded fixes to make");
        let a = parley_decide(&mut p, Some(ParleyFlight::Execute), "claude", None);
        match a {
            ParleyAction::End { note } => assert!(note.contains("agreed fixes"), "{note}"),
            other => panic!("expected End after execution, got {other:?}"),
        }

        // Nobody ever objected: convergence with nothing to wrap up.
        let mut clean = parley_state(&["r1"]);
        clean.execute = true;
        match parley_decide(&mut clean, Some(ParleyFlight::Reviewer), "r1", Some(true)) {
            ParleyAction::End { note } => assert!(note.contains("no material objections"), "{note}"),
            other => panic!("expected End, got {other:?}"),
        }

        // execute=off: a post-objection clean round seats the closing turn
        // that writes THE PLAN, then ends with it as the deliverable.
        let mut noexec = parley_state(&["r1"]);
        let _ = parley_decide(&mut noexec, Some(ParleyFlight::Reviewer), "r1", Some(false));
        let _ = parley_decide(&mut noexec, Some(ParleyFlight::Answer), "claude", None);
        let a = parley_decide(&mut noexec, Some(ParleyFlight::Reviewer), "r1", Some(true));
        assert_eq!(a, ParleyAction::Verdict);
        assert_eq!(noexec.wrap, Some(ParleyWrap::Plan));
        match parley_decide(&mut noexec, Some(ParleyFlight::Verdict), "claude", None) {
            ParleyAction::End { note } => assert!(note.contains("agreed plan"), "{note}"),
            other => panic!("expected End after the plan, got {other:?}"),
        }
    }

    /// The user's queued message takes the floor at the next boundary, and the
    /// debate resumes exactly where it paused.
    #[test]
    fn parley_decide_gives_a_queued_user_message_the_floor() {
        let mut p = parley_state(&["r1", "r2"]);
        p.pending_user = Some("hold on — try the other approach".into());
        let a = parley_decide(&mut p, Some(ParleyFlight::Reviewer), "r1", Some(false));
        assert_eq!(
            a,
            ParleyAction::UserTurn {
                text: "hold on — try the other approach".into()
            }
        );
        assert!(p.pending_user.is_none());
        // The interjection is neutral: r2 was next, and r2 is still next.
        let a = parley_decide(&mut p, Some(ParleyFlight::User), "claude", None);
        assert_eq!(a, ParleyAction::Review { lane_id: "r2".into() });
        assert_eq!(p.next, 1, "the mid-round position survived the pause");
    }

    /// Verdict parsing: the VERDICT marker wins, markdown decorations are
    /// tolerated, the "no material objection" phrase is the fallback, and
    /// anything else keeps the debate going.
    #[test]
    fn parley_verdict_parses_the_marker_and_its_fallbacks() {
        let ev = |speaker: &str, text: &str| PersistedEvent {
            seq: 1,
            ts: "2026-01-01T00:00:00Z".into(),
            speaker: Some(speaker.into()),
            event: AgentEvent::AssistantMessage { text: text.into() },
        };
        assert!(parley_verdict(&[ev("r1", "all good.\nVERDICT: CONCEDED")], "r1"));
        assert!(parley_verdict(&[ev("r1", "**VERDICT: CONCEDED**")], "r1"));
        assert!(!parley_verdict(&[ev("r1", "VERDICT: OBJECTING")], "r1"));
        assert!(parley_verdict(&[ev("r1", "I have no material objection.")], "r1"));
        assert!(!parley_verdict(&[ev("r1", "prose with no verdict at all")], "r1"));
        // The lane's LATEST message is the one that counts.
        let log = vec![ev("r1", "VERDICT: OBJECTING"), ev("r1", "VERDICT: CONCEDED")];
        assert!(parley_verdict(&log, "r1"));
        // Another lane's verdict is irrelevant.
        assert!(!parley_verdict(&[ev("r2", "VERDICT: CONCEDED")], "r1"));
    }

    /// Validation happens before anyone is seated: no reviewers, an empty
    /// thread, and an already-running parley all fail without side effects.
    #[test]
    fn start_parley_validates_before_seating_anyone() {
        let (root, store, project, settings) = test_store("parley-validate");
        let thread = store
            .create_thread(project.id, Agent::Claude, settings)
            .unwrap();
        let hub = Hub::new(Arc::clone(&store), 42800);

        assert!(hub.start_parley(&thread.id, vec![], 2, false, None).is_err());
        assert!(
            hub.start_parley(&thread.id, vec![ReviewerSpec::new(Agent::Claude)], 2, false, None)
                .is_err(),
            "no history yet — nothing to debate"
        );
        store
            .update_thread(&thread.id, |t| t.parley = Some(parley_state(&["r1"])))
            .unwrap();
        assert!(
            hub.start_parley(&thread.id, vec![ReviewerSpec::new(Agent::Claude)], 2, false, None)
                .is_err(),
            "one parley at a time"
        );
        assert_eq!(store.thread(&thread.id).unwrap().participants.len(), 0, "nobody was seated");

        drop(hub);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A message typed while the parley holds the floor must not bounce off
    /// the busy check: it queues onto the parley state instead of spawning a
    /// turn (which is why this test needs no tokio runtime).
    #[test]
    fn a_message_typed_mid_parley_queues_for_the_floor() {
        let (root, store, project, settings) = test_store("parley-queue");
        let thread = store
            .create_thread(project.id, Agent::Claude, settings)
            .unwrap();
        let hub = Hub::new(Arc::clone(&store), 42800);
        store
            .update_thread(&thread.id, |t| {
                t.status = ThreadStatus::Running;
                t.parley = Some(parley_state(&["r1"]));
            })
            .unwrap();

        hub.start_turn(&thread.id, "wait — use postgres instead".into(), vec![])
            .unwrap();
        let after = store.thread(&thread.id).unwrap();
        assert_eq!(
            after.parley.as_ref().unwrap().pending_user.as_deref(),
            Some("wait — use postgres instead")
        );
        // Nothing was emitted as a user turn yet: the log holds only the
        // queue acknowledgment.
        let events = store.read_events(&thread.id);
        assert!(
            !events.iter().any(|e| matches!(&e.event, AgentEvent::UserMessage { .. })),
            "the queued message is not a turn until the scheduler seats it"
        );

        drop(hub);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// End-to-end through the real boundary: a single reviewer who concedes
    /// (nothing objected, nothing to execute) ends the parley, returns the
    /// floor to the builder, and leaves a verdict note in the feed — all from
    /// `emit_as`, with no scheduler entry point besides the event.
    #[test]
    fn a_conceded_single_reviewer_ends_the_parley_at_the_boundary() {
        let (root, store, project, settings) = test_store("parley-end");
        let thread = store
            .create_thread(project.id, Agent::Claude, settings.clone())
            .unwrap();
        let hub = Hub::new(Arc::clone(&store), 42800);
        hub.emit(
            &thread.id,
            AgentEvent::UserMessage {
                text: "add the feature".into(),
                attachments: vec![],
                mid_turn: false,
                injected: false,
            },
        );
        hub.emit_as(&thread.id, Some("claude"), AgentEvent::AssistantMessage { text: "Done.".into() });
        hub.emit_as(&thread.id, Some("claude"), AgentEvent::TurnCompleted { usage: None });
        let reviewer = hub
            .join_reviewer(
                &thread.id,
                ReviewerSpec {
                    model: Some(settings.model.clone()),
                    ..ReviewerSpec::new(Agent::Claude)
                },
            )
            .unwrap();

        store
            .update_thread(&thread.id, |t| {
                t.status = ThreadStatus::Running;
                t.active_speaker = Some(reviewer.id.clone());
                t.parley = Some(ParleyState {
                    in_flight: Some(ParleyFlight::Reviewer),
                    ..parley_state(&[&reviewer.id])
                });
            })
            .unwrap();

        hub.emit_as(
            &thread.id,
            Some(&reviewer.id),
            AgentEvent::AssistantMessage {
                text: "Checked the diff and the tests — all sound.\nVERDICT: CONCEDED".into(),
            },
        );
        hub.emit_as(&thread.id, Some(&reviewer.id), AgentEvent::TurnCompleted { usage: None });

        let after = store.thread(&thread.id).unwrap();
        assert!(after.parley.is_none(), "a clean round ends the debate");
        let primary = after.primary_participant();
        assert_eq!(after.active_speaker.as_deref(), Some(primary.id.as_str()));
        assert!(store
            .read_events(&thread.id)
            .iter()
            .any(|e| matches!(&e.event, AgentEvent::Status { text } if text.contains("no material objections"))));

        drop(hub);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Switching profile cannot be applied to a live child (it is bound to the
    /// old endpoint and config home), so it retires the driver — and is
    /// refused outright while a turn is in flight.
    #[test]
    fn switching_claudex_profile_retires_the_driver_and_never_interrupts_a_turn() {
        let (root, store, project, mut settings) = test_store("claudex-switch");
        settings.model = "profile-a".into();
        let thread = store
            .create_thread(project.id, Agent::Claudex, settings.clone())
            .unwrap();
        let hub = Hub::new(Arc::clone(&store), 42800);

        let (tx, _rx) = mpsc::unbounded_channel();
        hub.sessions.lock().unwrap().insert(
            thread.id.clone(),
            SessionHandle {
                cmd_tx: tx,
                key: ("claudex".to_string(), Some("profile-a".into())),
            },
        );

        // Busy: the switch is refused and the live driver survives.
        store
            .update_thread(&thread.id, |t| t.status = ThreadStatus::Running)
            .unwrap();
        let mut switched = settings.clone();
        switched.model = "profile-b".into();
        assert!(hub.set_settings(&thread.id, switched.clone()).is_err());
        assert!(hub.sessions.lock().unwrap().contains_key(&thread.id));
        assert_eq!(
            store.thread(&thread.id).unwrap().settings.model,
            "profile-a"
        );

        // Idle: the switch lands and the driver is retired for a respawn.
        store
            .update_thread(&thread.id, |t| t.status = ThreadStatus::Idle)
            .unwrap();
        hub.set_settings(&thread.id, switched).unwrap();
        assert!(!hub.sessions.lock().unwrap().contains_key(&thread.id));

        // A non-profile change still reaches the live driver as before.
        let (tx, mut rx) = mpsc::unbounded_channel();
        hub.sessions.lock().unwrap().insert(
            thread.id.clone(),
            SessionHandle {
                cmd_tx: tx,
                key: ("claudex".to_string(), Some("profile-b".into())),
            },
        );
        let mut access = store.thread(&thread.id).unwrap().settings;
        access.access = Access::Read;
        hub.set_settings(&thread.id, access).unwrap();
        assert!(matches!(
            rx.try_recv(),
            Ok(AgentCommand::Settings { .. })
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restart_recovery_waits_for_bind_and_classifies_busy_threads() {
        let (root, store, project, settings) = test_store("startup-recovery");
        let running = store
            .create_thread(project.id.clone(), Agent::Claude, settings.clone())
            .unwrap();
        store
            .update_thread(&running.id, |t| t.status = ThreadStatus::Running)
            .unwrap();
        let waiting = store
            .create_thread(project.id.clone(), Agent::Codex, settings.clone())
            .unwrap();
        store
            .update_thread(&waiting.id, |t| t.status = ThreadStatus::WaitingApproval)
            .unwrap();
        let hermes = store
            .create_thread(project.id, Agent::Hermes, settings)
            .unwrap();
        store
            .update_thread(&hermes.id, |t| {
                t.status = ThreadStatus::Running;
                t.provider_run_id = Some("run-123".into());
            })
            .unwrap();

        let hub = Hub::new(Arc::clone(&store), 42800);

        // Constructing a second server must not touch the real one's turns.
        assert_eq!(
            store.thread(&running.id).unwrap().status,
            ThreadStatus::Running
        );
        assert!(store.read_events(&running.id).is_empty());

        let recoveries = hub.prepare_restart_recovery();
        assert_eq!(recoveries.len(), 2);
        assert!(recoveries.iter().any(
            |recovery| matches!(recovery, RestartRecovery::Continue(t) if t.id == running.id)
        ));
        assert!(recoveries.iter().any(|recovery| matches!(
            recovery,
            RestartRecovery::ReattachHermes { thread, run_id }
                if thread.id == hermes.id && run_id == "run-123"
        )));

        assert_eq!(store.thread(&running.id).unwrap().status, ThreadStatus::Idle);
        assert_eq!(store.thread(&waiting.id).unwrap().status, ThreadStatus::Idle);
        assert_eq!(
            store.thread(&hermes.id).unwrap().status,
            ThreadStatus::Running
        );
        assert!(matches!(
            store.read_events(&running.id).last().map(|e| &e.event),
            Some(AgentEvent::TurnAborted)
        ));
        assert!(matches!(
            store.read_events(&waiting.id).last().map(|e| &e.event),
            Some(AgentEvent::TurnAborted)
        ));
        assert!(store.read_events(&hermes.id).is_empty());
        drop(hub);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restart_continuation_prompt_stays_short_and_exact() {
        assert_eq!(
            RESTART_CONTINUATION_PROMPT,
            "Threadknot Rebooted - Continue where you left off."
        );
    }

    #[test]
    fn interrupt_repairs_busy_thread_without_live_session() {
        let (root, store, project, settings) = test_store("interrupt-recovery");
        let thread = store
            .create_thread(project.id, Agent::Claude, settings)
            .unwrap();
        let hub = Hub::new(Arc::clone(&store), 42800);
        store
            .update_thread(&thread.id, |t| t.status = ThreadStatus::WaitingApproval)
            .unwrap();

        hub.interrupt(&thread.id).unwrap();

        assert_eq!(store.thread(&thread.id).unwrap().status, ThreadStatus::Idle);
        assert!(matches!(
            store.read_events(&thread.id).last().map(|e| &e.event),
            Some(AgentEvent::TurnAborted)
        ));
        drop(hub);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A deliverable written during a turn becomes an artifact (chat card +
    /// index + durable snapshot); a source edit does not; a re-write updates
    /// the same record to "modified" rather than duplicating it.
    #[test]
    fn detects_deliverable_artifacts_across_a_turn() {
        let (root, store, project, settings) = test_store("artifacts");
        let thread = store
            .create_thread(project.id.clone(), Agent::Claude, settings)
            .unwrap();
        let hub = Hub::new(Arc::clone(&store), 42800);
        let ws = std::path::PathBuf::from(&project.path);

        // Turn begins → baseline snapshot of deliverables.
        hub.emit(
            &thread.id,
            AgentEvent::UserMessage {
                text: "make a report".into(),
                attachments: vec![],
                mid_turn: false,
                injected: false,
            },
        );
        // Agent produces a deliverable (.csv), edits source (.ts), and touches
        // project documentation (.md). Only the explicitly presented report is
        // a user-facing artifact.
        std::fs::write(ws.join("report.csv"), b"a,b\n1,2\n").unwrap();
        std::fs::write(ws.join("main.ts"), b"export const x = 1;\n").unwrap();
        std::fs::write(ws.join("README.md"), b"# Project\n").unwrap();
        hub.emit(
            &thread.id,
            AgentEvent::AssistantMessage {
                text: "Your report is ready: `report.csv`.".into(),
            },
        );
        // Turn ends → detection runs before the boundary is persisted.
        hub.emit(&thread.id, AgentEvent::TurnCompleted { usage: None });

        // Only the .csv counts; source and project-doc edits are excluded.
        let arts = store.list_artifacts_for_project(&project.id);
        assert_eq!(arts.len(), 1, "one deliverable, source excluded");
        let a = arts[0].clone();
        assert_eq!(a.name, "report.csv");
        assert_eq!(a.rel_path, "report.csv");
        assert_eq!(a.op, "created");
        assert_eq!(a.thread_id, thread.id);
        assert!(a.mime_type.starts_with("text/csv"));
        assert_eq!(store.list_artifacts_for_thread(&thread.id).len(), 1);

        // Snapshot is durable: it survives deleting the working-tree file.
        let snap = store.artifact_snapshot_path(&thread.id, &a.id).unwrap();
        assert_eq!(std::fs::read(&snap).unwrap(), b"a,b\n1,2\n");
        std::fs::remove_file(ws.join("report.csv")).unwrap();
        assert!(store.artifact_snapshot_path(&thread.id, &a.id).is_some());
        assert!(store.artifact_by_id(&a.id).is_some());

        // The Artifact event landed in the thread log (renders as a chat card).
        assert!(store.read_events(&thread.id).iter().any(|e| matches!(
            &e.event,
            AgentEvent::Artifact { name, .. } if name == "report.csv"
        )));

        // A re-write on a later turn updates the same record to "modified".
        hub.emit(
            &thread.id,
            AgentEvent::UserMessage {
                text: "update the report".into(),
                attachments: vec![],
                mid_turn: false,
                injected: false,
            },
        );
        std::fs::write(ws.join("report.csv"), b"a,b\n33,44\n").unwrap();
        hub.emit(
            &thread.id,
            AgentEvent::AssistantMessage {
                text: "The updated report is in `report.csv`.".into(),
            },
        );
        hub.emit(&thread.id, AgentEvent::TurnCompleted { usage: None });
        let arts = store.list_artifacts_for_project(&project.id);
        assert_eq!(arts.len(), 1, "re-write dedupes, no duplicate");
        assert_eq!(arts[0].op, "modified");
        assert_eq!(arts[0].id, a.id, "stable id across updates");

        // Deleting removes Threadknot's index, snapshot, and any materialized
        // clipboard copy, but never the original project file.
        let clipboard_dir = root
            .join("data")
            .join("artifacts")
            .join(&thread.id)
            .join("clipboard")
            .join(&a.id);
        std::fs::create_dir_all(&clipboard_dir).unwrap();
        std::fs::write(clipboard_dir.join("report.csv"), b"clipboard copy").unwrap();
        let deleted = store.delete_artifact(&a.id).unwrap();
        assert_eq!(deleted.id, a.id);
        assert!(store.artifact_by_id(&a.id).is_none());
        assert!(store.list_artifacts_for_project(&project.id).is_empty());
        assert!(store.artifact_snapshot_path(&thread.id, &a.id).is_none());
        assert!(!clipboard_dir.exists());
        assert!(
            ws.join("report.csv").exists(),
            "workspace file is preserved"
        );

        drop(hub);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// `publish_artifact` registers a deliverable immediately (origin
    /// "published", mid-turn card) and the turn-end diff does not duplicate
    /// it; a user attachment materialized under `.threadknot/` is never detected.
    #[test]
    fn published_artifacts_and_attachments_are_handled_by_provenance() {
        let (root, store, project, settings) = test_store("artifacts-publish");
        let thread = store
            .create_thread(project.id.clone(), Agent::Claude, settings)
            .unwrap();
        let hub = Hub::new(Arc::clone(&store), 42800);
        let ws = std::path::PathBuf::from(&project.path);

        hub.emit(
            &thread.id,
            AgentEvent::UserMessage {
                text: "analyze the pdf I sent and export a summary".into(),
                attachments: vec![],
                mid_turn: false,
                injected: false,
            },
        );
        // A user attachment lands in the workspace mid-turn (materialize_docs)
        // — the old scanner promoted this to an artifact.
        std::fs::create_dir_all(ws.join(".threadknot/attachments/att1")).unwrap();
        std::fs::write(ws.join(".threadknot/attachments/att1/input.pdf"), b"%PDF-1.4").unwrap();
        // The agent produces a summary and explicitly publishes it.
        std::fs::write(ws.join("summary.md"), b"# Summary\n").unwrap();
        let published = hub
            .publish_artifact(
                &thread.id,
                "summary.md",
                Some("Q3 summary"),
                Some("Key findings from the PDF"),
            )
            .unwrap();
        assert_eq!(published.origin, "published");
        assert_eq!(published.name, "Q3 summary");
        assert_eq!(published.description.as_deref(), Some("Key findings from the PDF"));
        hub.emit(
            &thread.id,
            AgentEvent::AssistantMessage {
                text: "Published the summary as `summary.md`.".into(),
            },
        );
        hub.emit(&thread.id, AgentEvent::TurnCompleted { usage: None });

        // Exactly one artifact: the published summary. The mention of
        // `summary.md` in the prose must NOT re-detect it as a second record,
        // and the attachment PDF must not appear at all.
        let arts = store.list_artifacts_for_project(&project.id);
        assert_eq!(arts.len(), 1, "published only; no duplicate, no attachment");
        assert_eq!(arts[0].id, published.id);
        assert_eq!(arts[0].op, "created");

        // Publishing a nonexistent file fails cleanly.
        assert!(hub
            .publish_artifact(&thread.id, "missing.bin", None, None)
            .is_err());

        drop(hub);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn plan_approval_request(approval_id: &str) -> AgentEvent {
        AgentEvent::ApprovalRequest {
            approval_id: approval_id.into(),
            approval_kind: "plan".into(),
            title: "Claude has a plan ready".into(),
            detail: "the plan".into(),
            options: vec![
                ApprovalOption {
                    id: "approve_build".into(),
                    label: "Approve & build".into(),
                    tone: "allow".into(),
                },
                ApprovalOption {
                    id: "keep_planning".into(),
                    label: "Keep planning".into(),
                    tone: "deny".into(),
                },
            ],
        }
    }

    /// A plan card orphaned by an app restart must still be answerable: with
    /// no live driver, responding resolves the card instead of erroring with
    /// "no active session".
    #[test]
    fn stale_plan_approval_resolves_without_live_session() {
        let (root, store, project, mut settings) = test_store("stale-plan");
        settings.mode = Mode::Plan;
        let thread = store
            .create_thread(project.id, Agent::Claude, settings)
            .unwrap();
        let hub = Hub::new(Arc::clone(&store), 42800);
        hub.emit(&thread.id, plan_approval_request("appr-1"));
        // What restart recovery does to the interrupted turn.
        hub.emit(&thread.id, AgentEvent::TurnAborted);

        hub.respond_approval(&thread.id, "appr-1".into(), "keep_planning".into())
            .unwrap();

        let events = store.read_events(&thread.id);
        assert!(matches!(
            events.last().map(|e| &e.event),
            Some(AgentEvent::ApprovalResolved { approval_id, option_id })
                if approval_id == "appr-1" && option_id == "keep_planning"
        ));
        let thread = store.thread(&thread.id).unwrap();
        assert_eq!(thread.status, ThreadStatus::Idle);
        assert_eq!(thread.settings.mode, Mode::Plan);

        // Second click on the now-resolved card must not re-relay.
        assert!(hub
            .respond_approval(&thread.id, "appr-1".into(), "keep_planning".into())
            .is_err());
        // Unknown ids stay errors.
        assert!(hub
            .respond_approval(&thread.id, "nope".into(), "approve_build".into())
            .is_err());
        drop(hub);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}

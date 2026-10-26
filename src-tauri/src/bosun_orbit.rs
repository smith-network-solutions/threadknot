//! Closing the Orbit ticket behind a to-do (docs/BOSUN.md §10.8).
//!
//! Orbit closing a ticket already closes its to-do (the sweep's reconcile).
//! This is the other direction: a to-do the person closes takes its ticket
//! with it, so a pile closed in Bosun leaves no orphans in Orbit. The write
//! goes through the org's own Orbit lookout, run exactly as it polls plus
//! `ORBIT_CLOSE_TASK_ID`, so it uses that lookout's key and token cache;
//! Threadknot holds no Orbit credentials of its own.
//!
//! Tickets the Bosun filed (`taskOrigin: agent`) close on their own. Any other
//! ticket came from a client or a teammate, and closing it may email them, so
//! it is held back and the UI asks first (`bosun.task.orbitClose`).

use super::*;

const ORBIT_CLOSE_TIMEOUT: Duration = Duration::from_secs(45);

/// What happened to the linked ticket when its to-do closed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrbitCloseOutcome {
    pub closed: bool,
    /// Left open because it is not one the Bosun filed; ask the person.
    #[serde(default)]
    pub needs_confirm: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `bosun.task.update { orbit }`: what to do with a linked ticket when the
/// update closes the to-do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum OrbitClosePolicy {
    /// Close tickets the Bosun filed; hold back the rest for a confirm.
    #[default]
    Auto,
    /// Close whatever its origin (the confirm said yes).
    Force,
    /// Leave Orbit alone.
    Skip,
}

impl OrbitClosePolicy {
    pub(crate) fn parse(p: &Value) -> Result<Self> {
        Ok(match p.get("orbit").and_then(Value::as_str).map(str::trim) {
            None | Some("") | Some("auto") => Self::Auto,
            Some("force") | Some("close") => Self::Force,
            Some("skip") => Self::Skip,
            Some(other) => anyhow::bail!("orbit must be auto, force or skip (got {other})"),
        })
    }
}

fn mentions_orbit_tasks(command: &str, args: &[String]) -> bool {
    command.contains("orbit-tasks") || args.iter().any(|a| a.contains("orbit-tasks"))
}

/// Whether a lookout's command line sets `ORBIT_ORG_SLUG` to `org` itself
/// (`bash -c '…; export ORBIT_ORG_SLUG=org …'`), quoted or not.
fn script_sets_slug(command: &str, args: &[String], org: &str) -> bool {
    std::iter::once(command).chain(args.iter().map(String::as_str)).any(|a| {
        a.match_indices("ORBIT_ORG_SLUG=").any(|(at, m)| {
            let rest = a[at + m.len()..].trim_start_matches(['\'', '"']);
            rest.strip_prefix(org)
                .is_some_and(|after| after.is_empty() || after.starts_with(|c: char| c.is_whitespace() || "'\";".contains(c)))
        })
    })
}

/// The command lookout that can write to Orbit org `org` (its slug): one
/// running orbit-tasks whose `ORBIT_ORG_SLUG` is that slug, in its env map or
/// set by its own wrapper script. An enabled one wins over a paused one; both
/// hold the same key.
pub(crate) fn orbit_lookout<'a>(bosun: &'a Bosun, org: &str) -> Option<&'a Lookout> {
    let org = org.trim();
    if org.is_empty() {
        return None;
    }
    bosun
        .lookouts
        .iter()
        .filter(|l| match &l.kind {
            LookoutKind::Command { command, args, env, .. } => {
                mentions_orbit_tasks(command, args)
                    && (env.get("ORBIT_ORG_SLUG").map(|s| s.trim()) == Some(org) || script_sets_slug(command, args, org))
            }
            _ => false,
        })
        .max_by_key(|l| l.enabled)
}

/// The closed status Orbit gets for a closed to-do.
pub(crate) fn orbit_status_for(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Dropped => "cancelled",
        _ => "done",
    }
}

/// The environment that turns an orbit-tasks run into a close
/// (`ORBIT_CLOSE_TASK_ID` and friends, lookouts/orbit-tasks.py).
pub(crate) fn close_env(orbit_task_id: &str, status: TaskStatus, force: bool) -> Vec<(&'static str, String)> {
    vec![
        ("ORBIT_CLOSE_TASK_ID", orbit_task_id.to_string()),
        ("ORBIT_CLOSE_STATUS", orbit_status_for(status).to_string()),
        ("ORBIT_CLOSE_ONLY_ORIGIN", if force { String::new() } else { "agent".to_string() }),
    ]
}

/// The script's one JSON line → an outcome. `reason: "origin"` is the
/// held-back client ticket; `"already"` means Orbit closed it first, which
/// counts as closed.
pub(crate) fn parse_close_output(stdout: &str) -> Result<OrbitCloseOutcome> {
    let line = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with('{'))
        .context("the Orbit lookout printed no result")?;
    let v: Value = serde_json::from_str(line).context("the Orbit lookout printed something that is not JSON")?;
    let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let reason = text("reason");
    let closed = v.get("closed").and_then(Value::as_bool).unwrap_or(false) || reason.as_deref() == Some("already");
    Ok(OrbitCloseOutcome {
        closed,
        needs_confirm: !closed && reason.as_deref() == Some("origin"),
        ticket: text("ticket"),
        origin: text("origin"),
        error: None,
    })
}

/// The history line a close attempt leaves on the to-do.
pub(crate) fn close_note(outcome: &OrbitCloseOutcome, ticket: &str) -> String {
    if outcome.closed {
        format!("Closed Orbit {ticket}")
    } else if outcome.needs_confirm {
        let origin = outcome.origin.as_deref().unwrap_or("client");
        format!("Orbit {ticket} left open: a {origin} ticket, closing it may email them")
    } else {
        format!(
            "Could not close Orbit {ticket}: {}",
            outcome.error.as_deref().unwrap_or("unknown error")
        )
    }
}

/// Close the ticket linked to `task`, if any. None when there is nothing to
/// close (no link, or the link has no Orbit task id).
pub(crate) async fn close_linked(state: &ServerState, task: &BosunTask, force: bool) -> Option<OrbitCloseOutcome> {
    let link = task.orbit.as_ref().filter(|o| !o.task_id.trim().is_empty() && !o.org.trim().is_empty())?;
    let hub = &state.hub;
    let ticket = link
        .ticket_number
        .as_deref()
        .map(|n| format!("#{n}"))
        .unwrap_or_else(|| "ticket".into());
    let status = if task.status.is_closed() { task.status } else { TaskStatus::Done };
    let outcome = match run_close(hub, task, link, status, force).await {
        Ok(o) => o,
        Err(e) => OrbitCloseOutcome {
            error: Some(format!("{e:#}")),
            ticket: link.ticket_number.clone(),
            ..Default::default()
        },
    };
    let note = close_note(&outcome, &ticket);
    let now = now_iso();
    if hub
        .bosuns
        .update_task(&task.id, |t| {
            push_history(t, None, &note, "bosun", &now);
            Ok(())
        })
        .is_ok()
    {
        hub.broadcast_state("bosunTasks", None);
    }
    Some(outcome)
}

async fn run_close(hub: &Hub, task: &BosunTask, link: &OrbitLink, status: TaskStatus, force: bool) -> Result<OrbitCloseOutcome> {
    let registry = &hub.bosuns;
    let bosun = registry.get(&task.bosun_id).context("this to-do's Bosun no longer exists")?;
    let lookout = orbit_lookout(&bosun, &link.org)
        .with_context(|| format!("no Orbit lookout for {} to close it with", link.org))?;
    let mut cmd = lookout_process(registry, hub.store.dir(), &bosun, lookout, &close_env(&link.task_id, status, force))
        .context("the Orbit lookout is not a command")?;
    let child = cmd.spawn().context("could not start the Orbit lookout")?;
    let output = tokio::time::timeout(ORBIT_CLOSE_TIMEOUT, child.wait_with_output())
        .await
        .context("the Orbit lookout timed out")??;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail = stderr.lines().last().unwrap_or("").trim().to_string();
        anyhow::bail!("Orbit lookout exited with {}{}", output.status, if tail.is_empty() { String::new() } else { format!(": {tail}") });
    }
    let mut outcome = parse_close_output(&String::from_utf8_lossy(&output.stdout))?;
    if outcome.ticket.is_none() {
        outcome.ticket = link.ticket_number.clone();
    }
    Ok(outcome)
}

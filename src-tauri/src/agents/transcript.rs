//! Handoff seed rendering ("fake context", after Traycer's design): turns the
//! provider-agnostic event log into a deterministic text transcript that a
//! freshly spawned provider session reads as its first user message, so a
//! mid-thread agent/model switch keeps the whole conversation.
//!
//! The rendered text is a pure function of the events past `after_seq`
//! (the target provider's `SessionAnchor::covered_until_seq`), so each
//! provider is only ever seeded with the delta it hasn't absorbed.

use crate::protocol::{AgentEvent, PersistedEvent};
use std::collections::HashMap;

/// Character budgets. Tool output/diffs are narrative context, not replayable
/// state — trim aggressively so the seed stays a fraction of the window.
const MAX_TOOL_OUTPUT: usize = 700;
const MAX_DIFF: usize = 2_500;
const MAX_TOTAL: usize = 48_000;

const HEADER: &str = "=== CONVERSATION HANDOFF ===\n\
You are joining an ongoing conversation in this workspace mid-stream. Earlier \
turns may have been handled by a different AI coding assistant. The transcript \
below is an accurate record of what already happened here: messages exchanged, \
tools that were actually run, and file changes that were actually applied to \
the working tree. Treat it as ground truth — do not redo completed work and do \
not re-verify it unless something looks inconsistent.\n";

const FOOTER: &str = "=== END OF HANDOFF ===\n\
The user's next message follows. Respond to it, continuing the conversation \
naturally.\n";

fn trim(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}… [truncated]", &s[..end])
    }
}

/// Render the events after `after_seq` (all events when `None`) into a seed.
/// Returns `None` when there is nothing to hand off.
///
/// `lane_names` maps participant id -> display name. It is what keeps a debate
/// legible when two lanes run the SAME agent and model (Claude reviewing
/// Claude): turn provenance alone would label both "Claude Code", and a lane
/// reading the seed back could mistake its critic's output for its own.
pub fn render(
    events: &[PersistedEvent],
    after_seq: Option<u64>,
    lane_names: &HashMap<String, String>,
) -> Option<String> {
    let mut blocks: Vec<String> = Vec::new();
    // Fallback speaker label for assistant output, tracked from turn provenance.
    let mut provenance: Option<String> = None;
    let mut provenance_model: Option<String> = None;
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut approvals: HashMap<String, String> = HashMap::new();

    for pe in events {
        if let Some(after) = after_seq {
            if pe.seq <= after {
                continue;
            }
        }
        match &pe.event {
            // An injected brief is not the user talking — it is the instruction
            // a lane was handed. Labeling it as `[user]` would make a later
            // reader answer to it as though the human had asked.
            AgentEvent::UserMessage { text, injected: true, .. } => {
                blocks.push(format!("[brief issued to another assistant]\n{text}"))
            }
            AgentEvent::UserMessage { text, .. } => blocks.push(format!("[user]\n{text}")),
            AgentEvent::TurnStarted { agent, model } => {
                provenance = match (agent, model) {
                    (Some(a), Some(m)) => Some(format!("{} · {m}", a.display_name())),
                    (Some(a), None) => Some(a.display_name().to_string()),
                    _ => None,
                };
                provenance_model = model.clone();
            }
            AgentEvent::AssistantMessage { text } => {
                let lane = pe.speaker.as_deref().and_then(|id| lane_names.get(id));
                let label = match lane {
                    Some(name) => match &provenance_model {
                        Some(model) => format!("{name} · {model}"),
                        None => name.clone(),
                    },
                    None => provenance.clone().unwrap_or_else(|| "assistant".into()),
                };
                blocks.push(format!("[assistant — {label}]\n{text}"));
            }
            AgentEvent::ToolStart {
                call_id,
                name,
                detail,
            } => {
                tool_names.insert(call_id.clone(), name.clone());
                blocks.push(format!("[tool run] {name}: {}", trim(detail, 300)));
            }
            AgentEvent::ToolEnd {
                call_id,
                name,
                output,
                is_error,
                ..
            } => {
                let name = tool_names
                    .remove(call_id)
                    .unwrap_or_else(|| name.clone());
                if let Some(out) = output {
                    let flag = if *is_error { " (errored)" } else { "" };
                    blocks.push(format!(
                        "[tool result{flag}] {name}:\n{}",
                        trim(out, MAX_TOOL_OUTPUT)
                    ));
                } else if *is_error {
                    blocks.push(format!("[tool result] {name}: errored"));
                }
            }
            AgentEvent::FileDiff { path, unified } => blocks.push(format!(
                "[file changed] {path}\n```diff\n{}\n```",
                trim(unified, MAX_DIFF)
            )),
            AgentEvent::ApprovalRequest {
                approval_id, title, ..
            } => {
                approvals.insert(approval_id.clone(), title.clone());
            }
            AgentEvent::ApprovalResolved {
                approval_id,
                option_id,
            } => {
                let title = approvals
                    .remove(approval_id)
                    .unwrap_or_else(|| "action".into());
                let verdict = if option_id.starts_with("accept") || option_id == "approve_build" {
                    "approved"
                } else {
                    "declined"
                };
                blocks.push(format!("[user {verdict}] {title}"));
            }
            AgentEvent::QuestionResolved {
                answers: Some(answers),
                ..
            } => {
                let mut lines: Vec<String> = answers
                    .iter()
                    .map(|(q, a)| format!("- {q}: {}", a.join(", ")))
                    .collect();
                lines.sort();
                blocks.push(format!(
                    "[user answered the assistant's questions]\n{}",
                    lines.join("\n")
                ));
            }
            AgentEvent::Error { message, .. } => {
                blocks.push(format!("[error] {}", trim(message, 300)));
            }
            // Thinking is provider-internal; deltas never persist; the rest is
            // lifecycle noise a new provider doesn't need.
            _ => {}
        }
    }

    if blocks.is_empty() {
        return None;
    }

    // Keep the newest blocks when over budget — recency matters most.
    let mut total: usize = blocks.iter().map(|b| b.len() + 2).sum();
    let mut start = 0;
    while total > MAX_TOTAL && start < blocks.len() - 1 {
        total -= blocks[start].len() + 2;
        start += 1;
    }
    let mut body = blocks[start..].join("\n\n");
    if start > 0 {
        body = format!("[… earlier conversation omitted for length …]\n\n{body}");
    }

    Some(format!("{HEADER}\n{body}\n\n{FOOTER}"))
}

/// Combine a seed with the user's actual message into the single first message
/// of a fresh provider session (a bare seed would itself trigger a turn).
pub fn seeded_message(seed: Option<&str>, text: &str) -> String {
    match seed {
        Some(seed) => format!("{seed}\n{text}"),
        None => text.to_string(),
    }
}

/// Chat-history budget for stateless providers: keep only the newest messages
/// under this many total content bytes so a long thread never trips the Hermes
/// gateway's 10 MB request cap. The gateway also compresses server-side; this
/// is just a hard backstop.
const MAX_HISTORY_BYTES: usize = 600_000;

/// Render the event log as role/content chat messages for providers that keep
/// NO server-side turn context (the Hermes runs API): every turn must resend
/// the prior conversation. Only durable message events map to turns
/// (`user_message` → user, `assistant_message` → assistant); tool and lifecycle
/// events are dropped. When `drop_trailing_user` is set, a final user turn whose
/// text matches it is removed — that is the current turn, already appended to
/// the log before the driver runs, which is sent separately as the run `input`.
/// The oldest messages are trimmed to stay within [`MAX_HISTORY_BYTES`].
pub fn render_messages(
    events: &[PersistedEvent],
    drop_trailing_user: Option<&str>,
) -> Vec<(String, String)> {
    let mut msgs: Vec<(String, String)> = Vec::new();
    for pe in events {
        match &pe.event {
            AgentEvent::UserMessage { text, .. } => msgs.push(("user".into(), text.clone())),
            AgentEvent::AssistantMessage { text } => msgs.push(("assistant".into(), text.clone())),
            _ => {}
        }
    }
    if let Some(cur) = drop_trailing_user {
        if msgs.last().is_some_and(|(role, text)| role == "user" && text == cur) {
            msgs.pop();
        }
    }
    // Keep the newest messages within budget (recency matters most).
    let mut total: usize = msgs.iter().map(|(_, c)| c.len()).sum();
    let mut start = 0;
    while total > MAX_HISTORY_BYTES && start < msgs.len() {
        total -= msgs[start].1.len();
        start += 1;
    }
    if start > 0 {
        msgs.drain(0..start);
    }
    msgs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Agent;

    fn ev(seq: u64, event: AgentEvent) -> PersistedEvent {
        PersistedEvent {
            seq,
            ts: String::new(),
            speaker: None,
            event,
        }
    }

    /// An event attributed to a lane, as a driver's emit stamps it.
    fn ev_by(seq: u64, speaker: &str, event: AgentEvent) -> PersistedEvent {
        PersistedEvent {
            seq,
            ts: String::new(),
            speaker: Some(speaker.into()),
            event,
        }
    }

    fn user(text: &str) -> AgentEvent {
        AgentEvent::UserMessage {
            text: text.into(),
            attachments: Vec::new(),
            mid_turn: false,
            injected: false,
        }
    }

    fn assistant(text: &str) -> AgentEvent {
        AgentEvent::AssistantMessage { text: text.into() }
    }

    fn names(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(id, name)| (id.to_string(), name.to_string()))
            .collect()
    }

    /// The case turn provenance cannot express: two lanes running the SAME
    /// agent and model. Without per-lane labels the builder reads its critic's
    /// output back as its own earlier message.
    #[test]
    fn render_labels_two_lanes_of_the_same_model_apart() {
        let turn = |agent| AgentEvent::TurnStarted {
            agent: Some(agent),
            model: Some("claude-opus-5".into()),
        };
        let events = vec![
            ev(0, user("build the thing")),
            ev_by(1, "claude", turn(Agent::Claude)),
            ev_by(2, "claude", assistant("Built it.")),
            ev_by(3, "rev-1", turn(Agent::Claude)),
            ev_by(4, "rev-1", assistant("It leaks a file handle.")),
        ];
        let seed = render(
            &events,
            None,
            &names(&[("claude", "Claude Code"), ("rev-1", "Claude Code (reviewer)")]),
        )
        .unwrap();
        assert!(seed.contains("[assistant — Claude Code · claude-opus-5]\nBuilt it."));
        assert!(seed.contains(
            "[assistant — Claude Code (reviewer) · claude-opus-5]\nIt leaks a file handle."
        ));
    }

    /// A role brief occupies the user slot on the wire, but a later lane must
    /// not read it as the human's own request.
    #[test]
    fn render_marks_an_injected_brief_as_a_brief_not_the_user() {
        let events = vec![
            ev(0, user("ship it")),
            ev(
                1,
                AgentEvent::UserMessage {
                    text: "=== ADVERSARIAL REVIEW ===".into(),
                    attachments: Vec::new(),
                    mid_turn: false,
                    injected: true,
                },
            ),
        ];
        let seed = render(&events, None, &HashMap::new()).unwrap();
        assert!(seed.contains("[user]\nship it"));
        assert!(seed.contains("[brief issued to another assistant]\n=== ADVERSARIAL REVIEW ==="));
        // The brief must not be attributed to the human.
        assert!(!seed.contains("[user]\n=== ADVERSARIAL REVIEW ==="));
    }

    /// Unattributed history (every thread written before Parley) still labels
    /// assistant output from turn provenance.
    #[test]
    fn render_falls_back_to_provenance_without_a_speaker() {
        let events = vec![
            ev(0, user("hi")),
            ev(
                1,
                AgentEvent::TurnStarted {
                    agent: Some(Agent::Codex),
                    model: Some("gpt-5.5".into()),
                },
            ),
            ev(2, assistant("hello")),
        ];
        let seed = render(&events, None, &HashMap::new()).unwrap();
        assert!(seed.contains("[assistant — Codex · gpt-5.5]\nhello"));
    }

    #[test]
    fn render_messages_keeps_role_alternation_and_drops_current_turn() {
        let events = vec![
            ev(0, user("first question")),
            ev(1, AgentEvent::Thinking { text: "hmm".into() }),
            ev(
                2,
                AgentEvent::AssistantMessage {
                    text: "first answer".into(),
                },
            ),
            // The current turn: already persisted before the driver runs.
            ev(3, user("do you remember?")),
        ];
        let msgs = render_messages(&events, Some("do you remember?"));
        assert_eq!(
            msgs,
            vec![
                ("user".into(), "first question".into()),
                ("assistant".into(), "first answer".into()),
            ]
        );
    }

    #[test]
    fn render_messages_without_trailing_match_keeps_everything() {
        let events = vec![
            ev(0, user("hello")),
            ev(
                1,
                AgentEvent::AssistantMessage {
                    text: "hi".into(),
                },
            ),
        ];
        // Current text does not match the last user turn -> nothing dropped.
        let msgs = render_messages(&events, Some("something else"));
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn render_messages_trims_oldest_over_budget() {
        let big = "x".repeat(MAX_HISTORY_BYTES / 2 + 10);
        let events = vec![
            ev(
                0,
                AgentEvent::AssistantMessage { text: big.clone() },
            ),
            ev(
                1,
                AgentEvent::AssistantMessage { text: big.clone() },
            ),
            ev(
                2,
                AgentEvent::AssistantMessage { text: big.clone() },
            ),
        ];
        let msgs = render_messages(&events, None);
        // Three ~half-budget messages can't all fit; the oldest is dropped.
        assert!(msgs.len() < 3);
        assert!(!msgs.is_empty());
    }
}

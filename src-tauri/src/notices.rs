//! Human-readable notification copy derived from normalized agent events.
//!
//! This is deliberately server-side: the server is the one component present
//! for desktop WebSocket alerts and sleeping-phone pushes, and composing once
//! keeps those surfaces from drifting into different wording.

use crate::protocol::{AgentEvent, EventNotice};

const TITLE_CHARS: usize = 96;
const BODY_CHARS: usize = 220;

/// Useful facts collected from the current turn while scanning its persisted
/// events. Tool output and thinking are intentionally excluded: notifications
/// should preview user-visible results, not leak noisy internals.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CompletionContext {
    assistant: Option<String>,
    last_user: Option<String>,
    changed_files: std::collections::HashSet<String>,
    artifacts: Vec<String>,
    closed: bool,
}

impl CompletionContext {
    /// Fold one persisted event into the current-turn summary.
    pub fn observe(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::UserMessage {
                text,
                mid_turn: false,
                injected,
                ..
            } => {
                self.reset();
                if !injected {
                    self.last_user = nonempty(text);
                }
            }
            // Recovery and a few server-driven paths can start a turn without a
            // fresh human message. A boundary tells us the prior turn's result
            // must not be reused for the new notification.
            AgentEvent::TurnStarted { .. } if self.closed => self.reset(),
            AgentEvent::AssistantMessage { text } => self.assistant = nonempty(text),
            AgentEvent::FileDiff { path, .. } => {
                self.changed_files.insert(path.clone());
            }
            AgentEvent::Artifact { name, .. } => {
                if !name.trim().is_empty() && !self.artifacts.iter().any(|item| item == name) {
                    self.artifacts.push(name.clone());
                }
            }
            AgentEvent::TurnCompleted { .. }
            | AgentEvent::TurnAborted
            | AgentEvent::Error { .. } => {
                self.closed = true;
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.assistant = None;
        self.last_user = None;
        self.changed_files.clear();
        self.artifacts.clear();
        self.closed = false;
    }

    fn detail(&self) -> String {
        if let Some(answer) = self.assistant.as_deref() {
            return answer.to_string();
        }
        match (self.changed_files.len(), self.artifacts.as_slice()) {
            (files, [artifact]) if files > 0 => {
                format!(
                    "Changed {files} {} and produced {artifact}.",
                    plural(files, "file", "files")
                )
            }
            (files, artifacts) if files > 0 && !artifacts.is_empty() => format!(
                "Changed {files} {} and produced {} artifacts.",
                plural(files, "file", "files"),
                artifacts.len()
            ),
            (files, _) if files > 0 => {
                format!("Changed {files} {}.", plural(files, "file", "files"))
            }
            (_, [artifact]) => format!("Produced {artifact}."),
            (_, artifacts) if !artifacts.is_empty() => {
                format!("Produced {} artifacts.", artifacts.len())
            }
            _ => self
                .last_user
                .as_deref()
                .map(|task| format!("Finished: {task}"))
                .unwrap_or_else(|| "Turn completed.".into()),
        }
    }
}

/// Compose the copy for an attention-worthy event. `completion` is needed only
/// for a turn boundary; the other event variants already carry their best text.
pub fn for_event(
    thread_title: &str,
    project_name: &str,
    event: &AgentEvent,
    completion: Option<&CompletionContext>,
) -> Option<EventNotice> {
    let thread = compact(thread_title, TITLE_CHARS).unwrap_or_else(|| "Threadknot".into());
    let (label, detail) = match event {
        AgentEvent::TurnCompleted { .. } => (
            "Finished",
            completion
                .map(CompletionContext::detail)
                .unwrap_or_else(|| "Turn completed.".into()),
        ),
        AgentEvent::ApprovalRequest { title, detail, .. } => {
            let title = compact(title, BODY_CHARS).unwrap_or_else(|| "Approval requested".into());
            let detail = compact(detail, BODY_CHARS);
            let message = match detail {
                Some(detail) if detail != title => format!("{title} — {detail}"),
                _ => title,
            };
            ("Approval needed", message)
        }
        AgentEvent::QuestionRequest { questions, .. } => {
            let first = questions.first()?;
            let mut message = nonempty(&first.question)
                .or_else(|| nonempty(&first.header))
                .unwrap_or_else(|| "The agent is waiting for your answer.".into());
            if questions.len() > 1 {
                message.push_str(&format!(" (+{} more)", questions.len() - 1));
            }
            ("Question", message)
        }
        // A structured failure leads with its own headline ("Workspace folder
        // missing") instead of a generic "Failed".
        AgentEvent::Error { message, title, .. } => {
            (title.as_deref().unwrap_or("Failed"), message.clone())
        }
        _ => return None,
    };

    let title = truncate(&format!("{label} · {thread}"), TITLE_CHARS);
    let project = compact(project_name, 64);
    let body = match project {
        Some(project) => format!("{project} · {detail}"),
        None => detail,
    };
    Some(EventNotice {
        title,
        body: compact(&body, BODY_CHARS).unwrap_or_else(|| label.to_string()),
    })
}

fn nonempty(text: &str) -> Option<String> {
    (!text.trim().is_empty()).then(|| text.to_string())
}

fn plural<'a>(count: usize, one: &'a str, many: &'a str) -> &'a str {
    if count == 1 {
        one
    } else {
        many
    }
}

/// Collapse Markdown-ish presentation into one OS-notification-sized line.
/// Backticks/fences are presentation here; retaining their contents keeps file
/// names and commands understandable without showing raw Markdown chrome.
fn compact(text: &str, max_chars: usize) -> Option<String> {
    let without_ticks = text.replace("```", "").replace('`', "");
    let collapsed = without_ticks
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!collapsed.is_empty()).then(|| truncate(&collapsed, max_chars))
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    format!("{}…", text.chars().take(keep).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ApprovalOption, Question};

    fn completed() -> AgentEvent {
        AgentEvent::TurnCompleted { usage: None }
    }

    #[test]
    fn completion_uses_the_current_turns_final_answer() {
        let mut context = CompletionContext::default();
        context.observe(&AgentEvent::UserMessage {
            text: "Fix login".into(),
            attachments: vec![],
            mid_turn: false,
            injected: false,
        });
        context.observe(&AgentEvent::AssistantMessage {
            text: "Added `state` validation.\n\nAll 42 tests pass.".into(),
        });
        context.observe(&completed());

        assert_eq!(
            for_event(
                "OAuth callback",
                "threadknot-app",
                &completed(),
                Some(&context)
            ),
            Some(EventNotice {
                title: "Finished · OAuth callback".into(),
                body: "threadknot-app · Added state validation. All 42 tests pass.".into(),
            })
        );
    }

    #[test]
    fn a_new_turn_never_reuses_the_previous_answer() {
        let mut context = CompletionContext::default();
        context.observe(&AgentEvent::AssistantMessage {
            text: "Old answer".into(),
        });
        context.observe(&completed());
        context.observe(&AgentEvent::TurnStarted {
            agent: None,
            model: None,
        });
        context.observe(&completed());

        let notice = for_event("Chat", "Project", &completed(), Some(&context)).unwrap();
        assert_eq!(notice.body, "Project · Turn completed.");
    }

    #[test]
    fn tool_only_completion_summarizes_files_and_artifacts() {
        let mut context = CompletionContext::default();
        for path in ["src/a.rs", "src/b.rs", "src/a.rs"] {
            context.observe(&AgentEvent::FileDiff {
                path: path.into(),
                unified: String::new(),
            });
        }
        context.observe(&AgentEvent::Artifact {
            id: "a".into(),
            name: "report.pdf".into(),
            rel_path: "report.pdf".into(),
            mime_type: "application/pdf".into(),
            size_bytes: 10,
            op: "created".into(),
            origin: "detected".into(),
            description: None,
        });

        let notice = for_event("Report", "Project", &completed(), Some(&context)).unwrap();
        assert_eq!(
            notice.body,
            "Project · Changed 2 files and produced report.pdf."
        );
    }

    #[test]
    fn approval_question_and_error_use_their_actual_text() {
        let approval = AgentEvent::ApprovalRequest {
            approval_id: "a".into(),
            approval_kind: "exec".into(),
            title: "Run deployment".into(),
            detail: "Allow npm publish to access the network?".into(),
            options: vec![ApprovalOption {
                id: "yes".into(),
                label: "Allow".into(),
                tone: "allow".into(),
            }],
        };
        assert_eq!(
            for_event("Release", "Web", &approval, None).unwrap().body,
            "Web · Run deployment — Allow npm publish to access the network?"
        );

        let question = AgentEvent::QuestionRequest {
            request_id: "q".into(),
            questions: vec![
                Question {
                    id: "1".into(),
                    header: "Provider".into(),
                    question: "Which payment provider?".into(),
                    options: vec![],
                    multi_select: false,
                    allow_other: true,
                    is_secret: false,
                },
                Question {
                    id: "2".into(),
                    header: "Region".into(),
                    question: "Which region?".into(),
                    options: vec![],
                    multi_select: false,
                    allow_other: true,
                    is_secret: false,
                },
            ],
        };
        assert_eq!(
            for_event("Checkout", "Shop", &question, None).unwrap().body,
            "Shop · Which payment provider? (+1 more)"
        );

        let error = AgentEvent::error("Migration 042 conflicts with an existing version.".into());
        assert_eq!(
            for_event("Database", "API", &error, None).unwrap().body,
            "API · Migration 042 conflicts with an existing version."
        );
    }

    #[test]
    fn notification_copy_is_single_line_and_bounded() {
        let long = "word ".repeat(100);
        let notice = for_event(
            &long,
            "Project",
            &AgentEvent::error(long.clone()),
            None,
        )
        .unwrap();
        assert!(notice.title.chars().count() <= TITLE_CHARS);
        assert!(notice.body.chars().count() <= BODY_CHARS);
        assert!(!notice.body.contains('\n'));
        assert!(notice.body.ends_with('…'));
    }
}

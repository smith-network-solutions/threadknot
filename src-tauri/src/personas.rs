//! Named, reusable reviewer presets (Parley personas). A persona is a display
//! name, the agent/model/effort/access that powers it, and a personality that
//! is folded into every review brief it runs — "The Skeptic" stays skeptical
//! no matter which thread you throw it at.
//!
//! Stored in `personas.json` beside the other registries (hermes, claudex);
//! machine-local like them. The shipped panel is seeded on first run so a new
//! user can start a debate with one click, and edited freely after.

use crate::protocol::{new_id, now_iso, Access, Agent};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewerPersona {
    pub id: String,
    pub name: String,
    pub agent: Agent,
    /// Absent = the agent's current default model at seat time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Absent = the parley/review default (full control).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<Access>,
    /// Folded into every review brief this persona runs.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub personality: String,
    /// Seeded by Threadknot rather than written by the user. Informational only —
    /// built-ins are editable and deletable like any other persona.
    #[serde(default)]
    pub builtin: bool,
    pub created_at: String,
}

#[derive(Serialize, Deserialize)]
struct PersonaFile {
    personas: Vec<ReviewerPersona>,
}

/// The shipped panel. Model/effort left unset so they track each agent's
/// current default instead of pinning an id that ages.
fn default_personas() -> Vec<ReviewerPersona> {
    let mk = |name: &str, agent: Agent, personality: &str| ReviewerPersona {
        id: new_id(),
        name: name.to_string(),
        agent,
        model: None,
        effort: None,
        access: None,
        personality: personality.to_string(),
        builtin: true,
        created_at: now_iso(),
    };
    vec![
        mk(
            "The Skeptic",
            Agent::Claude,
            "You doubt everything. Assume the work is wrong until the code itself \
             proves otherwise, and make the builder defend every claim with evidence. \
             Praise is just suspicion you haven't verified yet.",
        ),
        mk(
            "Second Opinion",
            Agent::Codex,
            "You are a pragmatic senior engineer. Focus on correctness, edge cases, \
             and whether the tests actually prove the behavior. Disagree only when \
             you can demonstrate the failure concretely.",
        ),
        mk(
            "The Contrarian",
            Agent::Kimi,
            "You argue the other side. Whatever approach the builder chose, champion \
             the credible alternative — a different design, different trade-offs — \
             and make them justify the choice they made.",
        ),
    ]
}

pub struct PersonaRegistry {
    path: PathBuf,
    personas: Mutex<Vec<ReviewerPersona>>,
}

impl PersonaRegistry {
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("personas.json");
        let (personas, fresh) = if path.exists() {
            let file: PersonaFile = serde_json::from_str(
                &std::fs::read_to_string(&path).context("read personas.json")?,
            )
            .context("parse personas.json")?;
            (file.personas, false)
        } else {
            (default_personas(), true)
        };
        let registry = Self {
            path,
            personas: Mutex::new(personas),
        };
        // Persist a freshly seeded panel so its ids are stable from here on.
        if fresh {
            registry.flush(&registry.personas.lock().unwrap())?;
        }
        Ok(registry)
    }

    fn flush(&self, personas: &[ReviewerPersona]) -> Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&PersonaFile {
            personas: personas.to_vec(),
        })?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn list(&self) -> Vec<ReviewerPersona> {
        self.personas.lock().unwrap().clone()
    }

    /// Create or replace a persona (matched by `id`; an empty id mints one).
    pub fn save(&self, mut persona: ReviewerPersona) -> Result<ReviewerPersona> {
        anyhow::ensure!(!persona.name.trim().is_empty(), "persona needs a name");
        persona.name = persona.name.trim().to_string();
        persona.personality = persona.personality.trim().to_string();
        let mut personas = self.personas.lock().unwrap();
        if persona.id.is_empty() {
            persona.id = new_id();
            persona.created_at = now_iso();
            persona.builtin = false;
            personas.push(persona.clone());
        } else {
            match personas.iter_mut().find(|p| p.id == persona.id) {
                Some(existing) => {
                    persona.created_at = existing.created_at.clone();
                    *existing = persona.clone();
                }
                None => {
                    persona.created_at = now_iso();
                    personas.push(persona.clone());
                }
            }
        }
        self.flush(&personas)?;
        Ok(persona)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let mut personas = self.personas.lock().unwrap();
        let before = personas.len();
        personas.retain(|p| p.id != id);
        anyhow::ensure!(personas.len() < before, "unknown persona");
        self.flush(&personas)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_the_shipped_panel_and_round_trips_edits() {
        let dir = std::env::temp_dir().join(format!("threadknot-personas-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let registry = PersonaRegistry::open(&dir).unwrap();

        // First run seeds the three built-ins and persists them.
        let seeded = registry.list();
        assert_eq!(seeded.len(), 3);
        assert!(seeded.iter().all(|p| p.builtin));
        assert!(dir.join("personas.json").exists());

        // Edit one, add one, delete one.
        let mut skeptic = seeded[0].clone();
        skeptic.personality = "edited".into();
        let saved = registry.save(skeptic.clone()).unwrap();
        assert_eq!(saved.id, skeptic.id, "upsert keeps the id");

        let custom = registry
            .save(ReviewerPersona {
                id: String::new(),
                name: "Security Hawk".into(),
                agent: Agent::Claude,
                model: Some("claude-opus-5".into()),
                effort: Some("high".into()),
                access: Some(Access::Read),
                personality: "Every input is hostile.".into(),
                builtin: false,
                created_at: String::new(),
            })
            .unwrap();
        assert!(!custom.id.is_empty());
        registry.delete(&seeded[1].id).unwrap();

        // A fresh open sees exactly the edited world.
        let reopened = PersonaRegistry::open(&dir).unwrap();
        let list = reopened.list();
        assert_eq!(list.len(), 3);
        assert_eq!(
            list.iter().find(|p| p.id == skeptic.id).unwrap().personality,
            "edited"
        );
        assert!(list.iter().any(|p| p.name == "Security Hawk"));
        assert!(!list.iter().any(|p| p.id == seeded[1].id));

        drop(reopened);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

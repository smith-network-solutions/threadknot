//! People: who is using this installation, so several teammates can share one
//! machine without sharing one sidebar.
//!
//! Threadknot has always assumed one owner per machine. That is still the
//! security model — everything here is *convenience*, not a boundary. Agent
//! processes run as the same OS user, so a person with `files` or `terminal`
//! can read another person's transcripts no matter what this file says. What
//! this buys is the day-to-day thing: a shared dev box where three people each
//! see their own threads by default, can look at each other's on purpose, and
//! do not un-star each other's workspaces by accident.
//!
//! Two records live here, both in `people.json`:
//!
//! * **[`Person`]** — a name, a face, and optionally a private
//!   `CLAUDE_CONFIG_DIR` so each person's turns run on their own subscription
//!   login rather than all three sharing one seat.
//! * **[`PersonPrefs`]** — the per-person half of preferences that used to be
//!   global: which workspaces are starred or stashed, which threads are
//!   starred, and what is on the settled shelf.
//!
//! ## Why the preferences are HERE and not on the records
//!
//! `Workspace.favorite` and `Workspace.hidden` participate in the whole-record
//! last-write-wins mesh replica: starring a workspace stars it on every paired
//! machine, which is exactly right when the paired machines are all yours. Had
//! the per-person values gone on the workspace, every teammate's sidebar
//! opinion would have replicated across the mesh as a workspace edit and fought
//! every other teammate's. Keeping the overlay in a machine-local file means
//! the mesh sees precisely what it saw before this feature existed.
//!
//! ## Backwards compatibility
//!
//! The overlay is a *sparse* layer over the existing fields, never a
//! replacement. Reading a preference consults the acting person's overlay and
//! falls back to the record's own field when they have not expressed an
//! opinion. Writing always records the overlay, and when the writer is the
//! owner it *also* writes the legacy field, so mesh replication, older clients
//! and a desktop holding the master token behave exactly as they did.
//!
//! The practical consequences, all deliberate:
//!
//! * An install that never creates a second person behaves identically. Every
//!   principal resolves to [`OWNER_ID`], every read falls through to the
//!   record, every write updates the record.
//! * A person added later inherits the current sidebar — the owner's stashed
//!   workspaces stay stashed for them — until they express their own opinion.
//!   That is the useful default: "hidden" usually means the project is done,
//!   which is a shared fact rather than a personal one.
//! * Nothing is migrated and no existing field changes meaning, so downgrading
//!   to an older build reads the same `projects.json` it wrote.

use crate::protocol::{new_id, now_iso};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The person a master credential acts as, and the fallback for any paired
/// device that predates this feature. Reserved: `person.create` refuses it and
/// `person.delete` will not remove it, because every legacy record on disk is
/// implicitly attributed here.
pub const OWNER_ID: &str = "owner";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    pub id: String,
    pub name: String,
    /// Small square data URL, shown on their threads and in the people row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// CSS accent color, e.g. "#e0a34c".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// `CLAUDE_CONFIG_DIR` for turns this person starts. Absent means the
    /// machine's own `~/.claude`, which is what every existing install uses and
    /// what a single-person install should keep using.
    ///
    /// Set it and that person logs in separately (`CLAUDE_CONFIG_DIR=<dir>
    /// claude /login`), which is the difference between three developers
    /// sharing one subscription seat and three developers each holding their
    /// own. It is a billing and rate-limit fix, not an isolation one: the
    /// directory is readable by anything running as this OS user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_config_dir: Option<String>,
    /// True only for the seeded owner record.
    #[serde(default)]
    pub builtin: bool,
    pub created_at: String,
}

/// One person's opinion about a thread's place on the settled shelf. Held as a
/// pair because the two fields are read together: `kept_active_at` restarts the
/// idle clock rather than pinning the thread active forever, so dropping it
/// would make a thread pulled off the shelf re-settle on the next render.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShelfState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_active_at: Option<String>,
}

/// The sparse overlay for one person. Every map is "targets this person has an
/// opinion about"; anything absent falls back to the stored record.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonPrefs {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub workspace_favorite: HashMap<String, bool>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub workspace_hidden: HashMap<String, bool>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub thread_favorite: HashMap<String, bool>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub thread_shelf: HashMap<String, ShelfState>,
}

impl PersonPrefs {
    fn is_empty(&self) -> bool {
        self.workspace_favorite.is_empty()
            && self.workspace_hidden.is_empty()
            && self.thread_favorite.is_empty()
            && self.thread_shelf.is_empty()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PeopleFile {
    #[serde(default)]
    people: Vec<Person>,
    /// Keyed by person id.
    #[serde(default)]
    prefs: HashMap<String, PersonPrefs>,
}

fn owner_record() -> Person {
    Person {
        id: OWNER_ID.to_string(),
        name: "Owner".to_string(),
        avatar: None,
        color: None,
        claude_config_dir: None,
        builtin: true,
        created_at: now_iso(),
    }
}

pub struct PeopleRegistry {
    path: PathBuf,
    dir: PathBuf,
    people: Mutex<Vec<Person>>,
    prefs: Mutex<HashMap<String, PersonPrefs>>,
    /// Serializes snapshot-then-write. Two clients toggling at once is the
    /// normal case on a shared box, and write-to-temp-then-rename is not safe
    /// to run concurrently against one temp path: the second rename finds the
    /// file the first one already moved and fails with ENOENT. Taking the
    /// snapshot *inside* this lock also means the last writer through wins with
    /// the freshest state rather than with whatever it read on the way in.
    io: Mutex<()>,
}

impl PeopleRegistry {
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("people.json");
        let mut file: PeopleFile = if path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&path).context("read people.json")?)
                .context("parse people.json")?
        } else {
            PeopleFile::default()
        };
        // The owner is not optional: every thread written before this feature
        // is attributed to it, so a file that somehow lost the record would
        // orphan the entire history.
        let seeded = !file.people.iter().any(|p| p.id == OWNER_ID);
        if seeded {
            file.people.insert(0, owner_record());
        }
        let registry = Self {
            path,
            dir: dir.to_path_buf(),
            people: Mutex::new(file.people),
            prefs: Mutex::new(file.prefs),
            io: Mutex::new(()),
        };
        if seeded {
            registry.flush()?;
        }
        Ok(registry)
    }

    fn flush(&self) -> Result<()> {
        let _io = self.io.lock().unwrap();
        let file = PeopleFile {
            people: self.people.lock().unwrap().clone(),
            prefs: self
                .prefs
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, v)| !v.is_empty())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        };
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&file)?)
            .with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("replace {}", self.path.display()))?;
        Ok(())
    }

    pub fn list(&self) -> Vec<Person> {
        self.people.lock().unwrap().clone()
    }

    pub fn person(&self, id: &str) -> Option<Person> {
        self.people.lock().unwrap().iter().find(|p| p.id == id).cloned()
    }

    /// Whether `id` names a real person. Used to reject an author filter or a
    /// device assignment that points at a deleted record.
    pub fn exists(&self, id: &str) -> bool {
        self.people.lock().unwrap().iter().any(|p| p.id == id)
    }

    pub fn create(&self, name: &str) -> Result<Person> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "a person needs a name");
        let person = Person {
            id: new_id(),
            name: name.to_string(),
            avatar: None,
            color: None,
            claude_config_dir: None,
            builtin: false,
            created_at: now_iso(),
        };
        self.people.lock().unwrap().push(person.clone());
        self.flush()?;
        Ok(person)
    }

    pub fn update(&self, id: &str, f: impl FnOnce(&mut Person)) -> Result<Person> {
        let out = {
            let mut people = self.people.lock().unwrap();
            let person = people
                .iter_mut()
                .find(|p| p.id == id)
                .context("unknown person")?;
            f(person);
            person.clone()
        };
        self.flush()?;
        Ok(out)
    }

    /// Remove a person and everything keyed to them. Their threads keep the
    /// `author` stamp: an id with no record renders as "someone who has left"
    /// rather than silently becoming the owner's work.
    pub fn delete(&self, id: &str) -> Result<()> {
        anyhow::ensure!(id != OWNER_ID, "the owner cannot be removed");
        {
            let mut people = self.people.lock().unwrap();
            let before = people.len();
            people.retain(|p| p.id != id);
            anyhow::ensure!(people.len() < before, "unknown person");
        }
        self.prefs.lock().unwrap().remove(id);
        self.flush()
    }

    /// The default private login directory for a person. Kept under the store
    /// dir so `THREADKNOT_DATA_DIR` isolates it like everything else.
    pub fn default_claude_config_dir(&self, person_id: &str) -> PathBuf {
        self.dir.join("people").join(person_id).join("claude")
    }

    /// Give this person their own Claude login (or hand them back the
    /// machine's). Creating the directory here rather than at spawn time means
    /// `claude /login` has somewhere to write the moment the UI shows the path.
    pub fn set_claude_isolation(&self, id: &str, isolated: bool) -> Result<Person> {
        let dir = self.default_claude_config_dir(id);
        if isolated {
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("create {}", dir.display()))?;
            crate::store::restrict_dir(&dir);
        }
        self.update(id, |p| {
            p.claude_config_dir = isolated.then(|| dir.to_string_lossy().into_owned());
        })
    }

    // ---- preference overlay ----

    fn read<T>(&self, person_id: &str, pick: impl Fn(&PersonPrefs) -> Option<T>) -> Option<T> {
        self.prefs.lock().unwrap().get(person_id).and_then(pick)
    }

    fn write(&self, person_id: &str, f: impl FnOnce(&mut PersonPrefs)) -> Result<()> {
        f(self
            .prefs
            .lock()
            .unwrap()
            .entry(person_id.to_string())
            .or_default());
        self.flush()
    }

    /// Whether `person_id` has starred this workspace, or `None` when they have
    /// no opinion and the stored flag should stand.
    pub fn workspace_favorite(&self, person_id: &str, workspace_id: &str) -> Option<bool> {
        self.read(person_id, |p| p.workspace_favorite.get(workspace_id).copied())
    }

    pub fn workspace_hidden(&self, person_id: &str, workspace_id: &str) -> Option<bool> {
        self.read(person_id, |p| p.workspace_hidden.get(workspace_id).copied())
    }

    pub fn thread_favorite(&self, person_id: &str, thread_id: &str) -> Option<bool> {
        self.read(person_id, |p| p.thread_favorite.get(thread_id).copied())
    }

    pub fn thread_shelf(&self, person_id: &str, thread_id: &str) -> Option<ShelfState> {
        self.read(person_id, |p| p.thread_shelf.get(thread_id).cloned())
    }

    pub fn set_workspace_favorite(
        &self,
        person_id: &str,
        workspace_id: &str,
        favorite: bool,
    ) -> Result<()> {
        self.write(person_id, |p| {
            p.workspace_favorite.insert(workspace_id.to_string(), favorite);
        })
    }

    pub fn set_workspace_hidden(
        &self,
        person_id: &str,
        workspace_id: &str,
        hidden: bool,
    ) -> Result<()> {
        self.write(person_id, |p| {
            p.workspace_hidden.insert(workspace_id.to_string(), hidden);
        })
    }

    pub fn set_thread_favorite(
        &self,
        person_id: &str,
        thread_id: &str,
        favorite: bool,
    ) -> Result<()> {
        self.write(person_id, |p| {
            p.thread_favorite.insert(thread_id.to_string(), favorite);
        })
    }

    pub fn set_thread_shelf(
        &self,
        person_id: &str,
        thread_id: &str,
        shelf: ShelfState,
    ) -> Result<()> {
        self.write(person_id, |p| {
            p.thread_shelf.insert(thread_id.to_string(), shelf);
        })
    }

    /// Drop one person's shelf opinion, handing them back the stored value.
    pub fn clear_thread_shelf(&self, person_id: &str, thread_id: &str) -> Result<()> {
        self.write(person_id, |p| {
            p.thread_shelf.remove(thread_id);
        })
    }

    /// New activity in a thread un-parks it for EVERYONE who had filed it away.
    ///
    /// The mirror of the `settled_at`/`kept_active_at` clear the hub already
    /// does on the record. Without it a person's overlay would outlive the
    /// event that should have cleared it, and a thread with news in it would
    /// stay buried in their collapsed shelf — the exact failure the record-side
    /// clear exists to prevent, reintroduced per person.
    pub fn unpark_thread(&self, thread_id: &str) {
        let changed = {
            let mut prefs = self.prefs.lock().unwrap();
            let mut changed = false;
            for entry in prefs.values_mut() {
                if entry.thread_shelf.remove(thread_id).is_some() {
                    changed = true;
                }
            }
            changed
        };
        if changed {
            let _ = self.flush();
        }
    }

    /// Drop every trace of a deleted thread, so its ids cannot come back
    /// attached to a recycled record.
    pub fn forget_thread(&self, thread_id: &str) {
        let changed = {
            let mut prefs = self.prefs.lock().unwrap();
            let mut changed = false;
            for entry in prefs.values_mut() {
                changed |= entry.thread_shelf.remove(thread_id).is_some();
                changed |= entry.thread_favorite.remove(thread_id).is_some();
            }
            changed
        };
        if changed {
            let _ = self.flush();
        }
    }

    pub fn forget_workspace(&self, workspace_id: &str) {
        let changed = {
            let mut prefs = self.prefs.lock().unwrap();
            let mut changed = false;
            for entry in prefs.values_mut() {
                changed |= entry.workspace_favorite.remove(workspace_id).is_some();
                changed |= entry.workspace_hidden.remove(workspace_id).is_some();
            }
            changed
        };
        if changed {
            let _ = self.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_registry() -> (PathBuf, PeopleRegistry) {
        let dir = std::env::temp_dir().join(format!("threadknot-people-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let registry = PeopleRegistry::open(&dir).unwrap();
        (dir, registry)
    }

    #[test]
    fn seeds_the_owner_and_reopens_with_it() {
        let (dir, registry) = temp_registry();
        let people = registry.list();
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].id, OWNER_ID);
        assert!(people[0].builtin);
        // The seed is persisted, not re-derived, so ids stay stable.
        assert!(dir.join("people.json").exists());

        let reopened = PeopleRegistry::open(&dir).unwrap();
        assert_eq!(reopened.list().len(), 1);
        assert_eq!(reopened.list()[0].id, OWNER_ID);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_absent_overlay_reads_as_no_opinion() {
        let (dir, registry) = temp_registry();
        // The whole backwards-compatibility story rests on this: a person who
        // has never expressed an opinion must return None so the caller falls
        // back to the stored record.
        assert_eq!(registry.workspace_hidden(OWNER_ID, "w1"), None);
        assert_eq!(registry.workspace_favorite(OWNER_ID, "w1"), None);
        assert_eq!(registry.thread_favorite(OWNER_ID, "t1"), None);
        assert_eq!(registry.thread_shelf(OWNER_ID, "t1"), None);

        let intern = registry.create("Intern").unwrap();
        assert_eq!(registry.workspace_hidden(&intern.id, "w1"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn overlays_are_per_person_and_survive_a_reopen() {
        let (dir, registry) = temp_registry();
        let intern = registry.create("Intern").unwrap();

        registry.set_workspace_hidden(&intern.id, "w1", true).unwrap();
        registry.set_workspace_favorite(OWNER_ID, "w1", true).unwrap();

        // Neither person's opinion leaks into the other's.
        assert_eq!(registry.workspace_hidden(&intern.id, "w1"), Some(true));
        assert_eq!(registry.workspace_hidden(OWNER_ID, "w1"), None);
        assert_eq!(registry.workspace_favorite(OWNER_ID, "w1"), Some(true));
        assert_eq!(registry.workspace_favorite(&intern.id, "w1"), None);

        let reopened = PeopleRegistry::open(&dir).unwrap();
        assert_eq!(reopened.workspace_hidden(&intern.id, "w1"), Some(true));
        assert_eq!(reopened.workspace_favorite(OWNER_ID, "w1"), Some(true));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_explicit_false_is_an_opinion_not_an_absence() {
        let (dir, registry) = temp_registry();
        let intern = registry.create("Intern").unwrap();
        // The owner stashed a workspace; the intern wants it back. Storing
        // `false` rather than removing the key is what stops the fallback from
        // handing them the owner's `hidden: true` again on the next read.
        registry.set_workspace_hidden(&intern.id, "w1", false).unwrap();
        assert_eq!(registry.workspace_hidden(&intern.id, "w1"), Some(false));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn activity_unparks_the_thread_for_every_person() {
        let (dir, registry) = temp_registry();
        let intern = registry.create("Intern").unwrap();
        let shelf = ShelfState {
            settled_at: Some(now_iso()),
            kept_active_at: None,
        };
        registry.set_thread_shelf(OWNER_ID, "t1", shelf.clone()).unwrap();
        registry.set_thread_shelf(&intern.id, "t1", shelf.clone()).unwrap();
        registry.set_thread_shelf(&intern.id, "t2", shelf).unwrap();

        registry.unpark_thread("t1");

        assert_eq!(registry.thread_shelf(OWNER_ID, "t1"), None);
        assert_eq!(registry.thread_shelf(&intern.id, "t1"), None);
        // Untouched threads keep their shelf state.
        assert!(registry.thread_shelf(&intern.id, "t2").is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn deleting_a_person_takes_their_overlay_but_not_the_owner() {
        let (dir, registry) = temp_registry();
        let intern = registry.create("Intern").unwrap();
        registry.set_thread_favorite(&intern.id, "t1", true).unwrap();

        assert!(registry.delete(OWNER_ID).is_err());

        registry.delete(&intern.id).unwrap();
        assert!(!registry.exists(&intern.id));
        assert_eq!(registry.thread_favorite(&intern.id, "t1"), None);

        let reopened = PeopleRegistry::open(&dir).unwrap();
        assert_eq!(reopened.list().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn isolating_a_claude_login_creates_the_directory_and_is_reversible() {
        let (dir, registry) = temp_registry();
        let intern = registry.create("Intern").unwrap();

        let isolated = registry.set_claude_isolation(&intern.id, true).unwrap();
        let config = isolated.claude_config_dir.clone().unwrap();
        assert!(std::path::Path::new(&config).is_dir());
        assert!(config.contains(&intern.id));

        // Handing them back the machine login must clear the field, or the
        // driver keeps pointing at an empty config dir with no login in it.
        let shared = registry.set_claude_isolation(&intern.id, false).unwrap();
        assert_eq!(shared.claude_config_dir, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn forgetting_a_thread_clears_every_persons_entry() {
        let (dir, registry) = temp_registry();
        let intern = registry.create("Intern").unwrap();
        registry.set_thread_favorite(OWNER_ID, "t1", true).unwrap();
        registry.set_thread_favorite(&intern.id, "t1", true).unwrap();

        registry.forget_thread("t1");

        assert_eq!(registry.thread_favorite(OWNER_ID, "t1"), None);
        assert_eq!(registry.thread_favorite(&intern.id, "t1"), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}

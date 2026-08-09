//! Disk persistence: ~/.threadknot/{server.json, projects.json, threads/<id>.jsonl}

use crate::protocol::*;
use anyhow::{Context, Result};

/// Reserved project hosting threads with remote Hermes agents. Chats with a
/// gateway have no folder, so they live in this hidden home project instead
/// of a workspace: it is created lazily on the first Hermes thread, never
/// wrapped in a workspace by `migrate_mesh`, and the frontend renders its
/// threads in the sidebar's dedicated Hermes section.
pub const HERMES_HOME_PROJECT_ID: &str = "hermes-home";
/// Prefix for the hidden, machine-local project that owns folderless Quick
/// Chats. Unlike Hermes, local coding agents still need a real cwd, so each
/// thread gets an isolated directory beneath the project's private root.
pub const QUICK_HOME_PROJECT_PREFIX: &str = "quick-home:";

pub fn quick_home_project_id(machine_id: &str) -> String {
    format!("{QUICK_HOME_PROJECT_PREFIX}{machine_id}")
}

pub fn is_quick_home_project_id(project_id: &str) -> bool {
    project_id.starts_with(QUICK_HOME_PROJECT_PREFIX)
}
const RETIRED_CLAUDE_OPUS_MODEL: &str = "claude-opus-4-8";
const CURRENT_CLAUDE_OPUS_MODEL: &str = "claude-opus-5";
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub port: u16,
    pub token: String,
    /// Stable identity for this Threadknot install. Mobile push routing keys on it,
    /// so it must survive token rotation and port changes. Auto-migrated into
    /// existing server.json files on first load.
    #[serde(default)]
    pub server_id: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    projects: Vec<Project>,
    threads: Vec<Thread>,
    #[serde(default)]
    workspaces: Vec<Workspace>,
    #[serde(default)]
    schedules: Vec<Schedule>,
    #[serde(default)]
    terminals: Vec<TerminalRecord>,
    #[serde(default)]
    artifacts: Vec<ArtifactRecord>,
    #[serde(default)]
    repos: Vec<RepoRecord>,
    #[serde(default)]
    archive_dir: Option<String>,
    /// Deleted workspace ids + when: keeps a peer's stale copy from
    /// resurrecting a workspace at the next resync (mesh-wide catalog sync).
    #[serde(default)]
    workspace_tombstones: Vec<WorkspaceTombstone>,
}

/// Replace the retired Opus picker entry without silently changing users who
/// selected another Claude model. Existing provider session ids remain valid;
/// the driver asks Claude Code to resume them using the new selected model.
fn migrate_retired_models(data: &mut StoreFile) -> bool {
    let mut changed = false;
    for thread in data.threads.iter_mut() {
        if thread.agent == Agent::Claude && thread.settings.model == RETIRED_CLAUDE_OPUS_MODEL {
            thread.settings.model = CURRENT_CLAUDE_OPUS_MODEL.into();
            thread.settings.wide_context = false;
            changed = true;
        }
    }
    for schedule in data.schedules.iter_mut() {
        if schedule.agent == Agent::Claude && schedule.settings.model == RETIRED_CLAUDE_OPUS_MODEL {
            schedule.settings.model = CURRENT_CLAUDE_OPUS_MODEL.into();
            schedule.settings.wide_context = false;
            changed = true;
        }
    }
    changed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkspaceTombstone {
    id: String,
    deleted_at: String,
}

/// Workspace fallout of a project deletion, for the caller to replicate:
/// records that shrank, and records that emptied out and were tombstoned.
#[derive(Debug, Default)]
pub struct WorkspaceCascade {
    pub updated: Vec<Workspace>,
    pub deleted: Vec<String>,
    pub deleted_at: String,
}

/// Record `id` as dead at `deleted_at`, keeping the newest timestamp if a
/// tombstone already exists.
fn note_tombstone(tombstones: &mut Vec<WorkspaceTombstone>, id: &str, deleted_at: &str) {
    match tombstones.iter_mut().find(|t| t.id == id) {
        Some(t) => {
            if t.deleted_at.as_str() < deleted_at {
                t.deleted_at = deleted_at.to_string();
            }
        }
        None => tombstones.push(WorkspaceTombstone {
            id: id.to_string(),
            deleted_at: deleted_at.to_string(),
        }),
    }
}

pub struct Store {
    dir: PathBuf,
    data: Mutex<StoreFile>,
    /// Per-thread next seq, loaded lazily from the JSONL file length.
    seqs: Mutex<HashMap<String, u64>>,
    /// This machine's id (== server_id), set once by [`Store::migrate_mesh`]
    /// at startup so thread creation can stamp records without every call
    /// site threading the id through.
    machine_id: std::sync::OnceLock<String>,
}

impl Store {
    pub fn open() -> Result<Self> {
        Self::open_at(data_dir())
    }

    fn open_at(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(dir.join("threads"))?;
        // The data dir holds the master token, paired-device hashes, peer master
        // tokens and signed-in browser profiles. Its mode otherwise depends on
        // whatever umask happened to be set when it was first created.
        restrict_dir(&dir);
        let path = dir.join("projects.json");
        let mut data = if path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&path)?).context("parse projects.json")?
        } else {
            StoreFile::default()
        };
        let migrated_models = migrate_retired_models(&mut data);
        let store = Self {
            dir,
            data: Mutex::new(data),
            seqs: Mutex::new(HashMap::new()),
            machine_id: std::sync::OnceLock::new(),
        };
        if migrated_models {
            let data = store.data.lock().unwrap();
            store.flush(&data)?;
        }
        Ok(store)
    }

    /// This machine's id, or "" before [`Store::migrate_mesh`] has run.
    pub fn local_machine_id(&self) -> String {
        self.machine_id.get().cloned().unwrap_or_default()
    }

    /// One-time mesh migration, run at startup once the server config (and
    /// thus the machine id) is known. Idempotent — flushes only on change:
    /// 1. Stamp every thread with this machine's id (EXPLICITLY — the mesh is
    ///    symmetric, so "no id = local" is never assumed anywhere).
    /// 2. Wrap every project without a workspace in a same-name workspace,
    ///    reusing the project id as the workspace id so nothing keyed on ids
    ///    moves. Zero peers ⇒ the sidebar looks exactly like before.
    pub fn migrate_mesh(&self, machine_id: &str) -> Result<()> {
        let _ = self.machine_id.set(machine_id.to_string());
        let mut data = self.data.lock().unwrap();
        let mut changed = false;
        for t in data.threads.iter_mut() {
            if t.machine_id.is_empty() {
                t.machine_id = machine_id.to_string();
                changed = true;
            }
        }
        let wrapped: Vec<Workspace> = data
            .projects
            .iter()
            // The Hermes home project is deliberately workspace-less: its
            // threads render in the sidebar's dedicated Hermes section.
            .filter(|p| p.id != HERMES_HOME_PROJECT_ID && !is_quick_home_project_id(&p.id))
            .filter(|p| {
                !data
                    .workspaces
                    .iter()
                    .any(|w| w.members.iter().any(|m| m.project_id == p.id))
            })
            .map(|p| Workspace {
                id: p.id.clone(),
                name: p.name.clone(),
                image: None,
                created_at: p.created_at.clone(),
                updated_at: now_iso(),
                favorite: None,
                hidden: None,
                members: vec![WorkspaceMember {
                    machine_id: machine_id.to_string(),
                    project_id: p.id.clone(),
                    name: Some(p.name.clone()),
                    path: Some(p.path.clone()),
                }],
            })
            .collect();
        if !wrapped.is_empty() {
            // A startup re-wrap outlives any tombstone for those ids (same
            // rule as wrap_project_in_workspace).
            data.workspace_tombstones
                .retain(|t| !wrapped.iter().any(|w| w.id == t.id));
            // Keep sidebar order: workspaces mirror the projects list.
            data.workspaces.extend(wrapped);
            changed = true;
        }
        if changed {
            self.flush(&data)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn open_for_test(dir: PathBuf) -> Result<Self> {
        Self::open_at(dir)
    }

    /// Data directory this store lives in (sibling files: server.json,
    /// mobile.json, hermes.json).
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    pub fn server_config(&self) -> Result<ServerConfig> {
        let path = self.dir.join("server.json");
        // SEC-013: this file IS the master credential. Repair its mode on every
        // open, not just at creation — an install that predates this (or one
        // restored from a permissive backup) is exactly the case that matters.
        restrict_file(&path);
        let env_port = std::env::var("THREADKNOT_PORT")
            .or_else(|_| std::env::var("ARMADA_PORT"))
            .ok()
            .and_then(|p| p.parse().ok());
        if path.exists() {
            if let Ok(mut cfg) =
                serde_json::from_str::<ServerConfig>(&std::fs::read_to_string(&path)?)
            {
                // Pre-mobile installs lack a server id — mint one ON DISK first
                // (not just in memory) so it stays stable across restarts.
                if cfg.server_id.is_empty() {
                    cfg.server_id = uuid::Uuid::new_v4().to_string();
                    write_private(&path, &serde_json::to_string_pretty(&cfg)?)?;
                }
                // Env override wins (lets a second instance run on another port).
                if let Some(port) = env_port {
                    cfg.port = port;
                }
                return Ok(cfg);
            }
        }
        let cfg = ServerConfig {
            port: env_port.unwrap_or(42800),
            token: generate_token(),
            server_id: uuid::Uuid::new_v4().to_string(),
        };
        write_private(&path, &serde_json::to_string_pretty(&cfg)?)?;
        Ok(cfg)
    }

    fn flush(&self, data: &StoreFile) -> Result<()> {
        let tmp = self.dir.join("projects.json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(data)?)?;
        std::fs::rename(&tmp, self.dir.join("projects.json"))?;
        Ok(())
    }

    // ---- projects ----

    pub fn create_project(&self, path: String, name: Option<String>) -> Result<Project> {
        let canonical = std::fs::canonicalize(&path)
            .with_context(|| format!("folder not found: {path}"))?;
        anyhow::ensure!(canonical.is_dir(), "not a directory: {path}");
        let name = name.unwrap_or_else(|| {
            canonical
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| canonical.to_string_lossy().into_owned())
        });
        let project = Project {
            id: new_id(),
            name,
            path: canonical.to_string_lossy().into_owned(),
            created_at: now_iso(),
        };
        let mut data = self.data.lock().unwrap();
        data.projects.insert(0, project.clone());
        // Every new project gets its own same-id workspace (the sidebar's
        // top-level element). Attaching a project to an EXISTING workspace is
        // a separate operation (workspace.attachRoot, mesh phases).
        data.workspaces.insert(
            0,
            Workspace {
                id: project.id.clone(),
                name: project.name.clone(),
                image: None,
                created_at: project.created_at.clone(),
                updated_at: now_iso(),
                favorite: None,
                hidden: None,
                members: vec![WorkspaceMember {
                    machine_id: self.local_machine_id(),
                    project_id: project.id.clone(),
                    name: Some(project.name.clone()),
                    path: Some(project.path.clone()),
                }],
            },
        );
        self.flush(&data)?;
        Ok(project)
    }

    /// Wrap a project created on a PEER (via mesh.createProject) in the same
    /// 1:1 same-id workspace create_project makes for a local folder. The
    /// project record itself lives on its owner machine; only the workspace
    /// (with the member snapshot) is stored here, then replicated out.
    /// mesh.createProject REUSES a project by canonical path, so its 1:1
    /// workspace may already be here via the catalog sync — then this is a
    /// no-op returning that record, not a duplicate insert.
    pub fn create_workspace_for_remote_root(
        &self,
        project: &Project,
        machine_id: &str,
    ) -> Result<Workspace> {
        let mut data = self.data.lock().unwrap();
        if let Some(ws) = data.workspaces.iter().find(|w| w.id == project.id) {
            return Ok(ws.clone());
        }
        let ws = Workspace {
            id: project.id.clone(),
            name: project.name.clone(),
            image: None,
            created_at: project.created_at.clone(),
            updated_at: now_iso(),
            favorite: None,
            hidden: None,
            members: vec![WorkspaceMember {
                machine_id: machine_id.to_string(),
                project_id: project.id.clone(),
                name: Some(project.name.clone()),
                path: Some(project.path.clone()),
            }],
        };
        data.workspaces.insert(0, ws.clone());
        self.flush(&data)?;
        Ok(ws)
    }

    pub fn list_projects(&self) -> Vec<Project> {
        self.data.lock().unwrap().projects.clone()
    }

    pub fn delete_project(&self, project_id: &str) -> Result<WorkspaceCascade> {
        let mut data = self.data.lock().unwrap();
        // Snapshot dirs are keyed by thread; collect this project's threads
        // before their records are dropped so we can clean the bytes up.
        let thread_ids: Vec<String> = data
            .threads
            .iter()
            .filter(|t| t.project_id == project_id)
            .map(|t| t.id.clone())
            .collect();
        data.projects.retain(|p| p.id != project_id);
        // Drop the project's workspace membership; a workspace with no
        // members left goes too (keeps the migrated 1:1 world identical to
        // pre-workspace behavior). The caller pushes the fallout to peers:
        // shrunk records replicate, emptied ones tombstone.
        let mut cascade = WorkspaceCascade::default();
        let now = now_iso();
        for w in data.workspaces.iter_mut() {
            let before = w.members.len();
            w.members.retain(|m| m.project_id != project_id);
            if w.members.len() < before && !w.members.is_empty() {
                w.updated_at = now.clone();
                cascade.updated.push(w.clone());
            }
        }
        cascade.deleted = data
            .workspaces
            .iter()
            .filter(|w| w.members.is_empty())
            .map(|w| w.id.clone())
            .collect();
        for id in &cascade.deleted {
            note_tombstone(&mut data.workspace_tombstones, id, &now);
        }
        cascade.deleted_at = now;
        data.workspaces.retain(|w| !w.members.is_empty());
        data.threads.retain(|t| t.project_id != project_id);
        data.schedules.retain(|s| s.project_id != project_id);
        data.terminals.retain(|t| t.project_id != project_id);
        data.artifacts.retain(|a| a.project_id != project_id);
        data.repos.retain(|r| r.project_id != project_id);
        self.flush(&data)?;
        for tid in thread_ids {
            let _ = std::fs::remove_dir_all(self.thread_artifacts_dir(&tid));
        }
        Ok(cascade)
    }

    pub fn project(&self, project_id: &str) -> Option<Project> {
        self.data
            .lock()
            .unwrap()
            .projects
            .iter()
            .find(|p| p.id == project_id)
            .cloned()
    }

    // ---- workspaces ----

    /// Create a project WITHOUT wrapping it in its own workspace — used when
    /// a folder is attached as an additional root of an EXISTING workspace
    /// (locally or via a peer's `mesh.createProject`). Reuses a project that
    /// already points at the same canonical path so repeated attaches can't
    /// pile up duplicates.
    pub fn create_project_raw(&self, path: String, name: Option<String>) -> Result<Project> {
        let canonical = std::fs::canonicalize(&path)
            .with_context(|| format!("folder not found: {path}"))?;
        anyhow::ensure!(canonical.is_dir(), "not a directory: {path}");
        let canonical = canonical.to_string_lossy().into_owned();
        let mut data = self.data.lock().unwrap();
        if let Some(existing) = data.projects.iter().find(|p| p.path == canonical) {
            return Ok(existing.clone());
        }
        let project = Project {
            id: new_id(),
            name: name.unwrap_or_else(|| {
                std::path::Path::new(&canonical)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| canonical.clone())
            }),
            path: canonical,
            created_at: now_iso(),
        };
        data.projects.insert(0, project.clone());
        self.flush(&data)?;
        Ok(project)
    }

    /// Add a root to a workspace (idempotent on machine+project); bumps the
    /// LWW clock.
    pub fn add_workspace_member(
        &self,
        workspace_id: &str,
        member: WorkspaceMember,
    ) -> Result<Workspace> {
        let mut data = self.data.lock().unwrap();
        let ws = data
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .context("unknown workspace")?;
        if !ws
            .members
            .iter()
            .any(|m| m.machine_id == member.machine_id && m.project_id == member.project_id)
        {
            ws.members.push(member);
            ws.updated_at = now_iso();
        }
        let out = ws.clone();
        self.flush(&data)?;
        Ok(out)
    }

    /// Remove a root from a workspace; bumps the LWW clock. The last member
    /// cannot be detached (delete the project instead).
    pub fn remove_workspace_member(
        &self,
        workspace_id: &str,
        project_id: &str,
    ) -> Result<Workspace> {
        let mut data = self.data.lock().unwrap();
        let ws = data
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .context("unknown workspace")?;
        anyhow::ensure!(
            ws.members.len() > 1,
            "a workspace keeps at least one root — remove the project instead"
        );
        let before = ws.members.len();
        ws.members.retain(|m| m.project_id != project_id);
        anyhow::ensure!(ws.members.len() < before, "that root is not in this workspace");
        ws.updated_at = now_iso();
        let out = ws.clone();
        self.flush(&data)?;
        Ok(out)
    }

    /// Store a workspace record replicated FROM another machine. The whole
    /// catalog syncs mesh-wide — records are accepted whether or not this
    /// machine is a member, so every workspace on every paired machine shows
    /// up everywhere (remote-only ones route through their owner).
    /// Reconciliation is whole-record last-write-wins by `updated_at`; a
    /// record no newer than a local tombstone stays dead. Returns true when
    /// the local store changed.
    pub fn upsert_workspace_replica(&self, incoming: Workspace) -> Result<bool> {
        let mut data = self.data.lock().unwrap();
        if let Some(i) = data
            .workspace_tombstones
            .iter()
            .position(|t| t.id == incoming.id)
        {
            if data.workspace_tombstones[i].deleted_at >= incoming.updated_at {
                return Ok(false);
            }
            // Edited after we deleted it — the edit wins, the grave reopens.
            data.workspace_tombstones.remove(i);
        }
        match data.workspaces.iter_mut().find(|w| w.id == incoming.id) {
            Some(local) => {
                if incoming.updated_at <= local.updated_at {
                    return Ok(false);
                }
                *local = incoming;
            }
            None => data.workspaces.insert(0, incoming),
        }
        self.flush(&data)?;
        Ok(true)
    }

    /// A peer deleted a workspace. Honored unless our copy was edited after
    /// the delete (the edit wins; our next resync revives it on the peer).
    /// Returns true when a local record was removed.
    pub fn apply_workspace_tombstone(&self, id: &str, deleted_at: &str) -> Result<bool> {
        let mut data = self.data.lock().unwrap();
        if data
            .workspaces
            .iter()
            .any(|w| w.id == id && w.updated_at.as_str() > deleted_at)
        {
            return Ok(false);
        }
        let before = data.workspaces.len();
        data.workspaces.retain(|w| w.id != id);
        let removed = data.workspaces.len() < before;
        note_tombstone(&mut data.workspace_tombstones, id, deleted_at);
        self.flush(&data)?;
        Ok(removed)
    }

    /// (id, deleted_at) of every dead workspace, pushed alongside the catalog
    /// at peer resync so deletes reconcile like edits do.
    pub fn workspace_tombstones(&self) -> Vec<(String, String)> {
        self.data
            .lock()
            .unwrap()
            .workspace_tombstones
            .iter()
            .map(|t| (t.id.clone(), t.deleted_at.clone()))
            .collect()
    }

    /// Forget workspaces that live entirely on `machine_id` (used when that
    /// peer is removed). No tombstone — re-pairing brings them back.
    pub fn purge_peer_workspaces(&self, machine_id: &str) -> Result<bool> {
        let mut data = self.data.lock().unwrap();
        let before = data.workspaces.len();
        data.workspaces.retain(|w| {
            w.members.is_empty() || w.members.iter().any(|m| m.machine_id != machine_id)
        });
        if data.workspaces.len() == before {
            return Ok(false);
        }
        self.flush(&data)?;
        Ok(true)
    }

    /// Re-wrap a project into its own same-id workspace (detached roots must
    /// never become invisible). No-op if any workspace already lists it.
    pub fn wrap_project_in_workspace(&self, project_id: &str) -> Result<()> {
        let machine_id = self.local_machine_id();
        let mut data = self.data.lock().unwrap();
        if data
            .workspaces
            .iter()
            .any(|w| w.members.iter().any(|m| m.project_id == project_id))
        {
            return Ok(());
        }
        let project = data
            .projects
            .iter()
            .find(|p| p.id == project_id)
            .context("unknown project")?
            .clone();
        // The fresh wrap outlives any tombstone for this id (a peer deleting
        // the shared workspace must not keep re-deleting our re-wrap).
        data.workspace_tombstones.retain(|t| t.id != project.id);
        data.workspaces.insert(
            0,
            Workspace {
                id: project.id.clone(),
                name: project.name.clone(),
                image: None,
                created_at: project.created_at.clone(),
                updated_at: now_iso(),
                favorite: None,
                hidden: None,
                members: vec![WorkspaceMember {
                    machine_id,
                    project_id: project.id,
                    name: Some(project.name),
                    path: Some(project.path),
                }],
            },
        );
        self.flush(&data)
    }

    pub fn list_workspaces(&self) -> Vec<Workspace> {
        self.data.lock().unwrap().workspaces.clone()
    }

    pub fn workspace(&self, workspace_id: &str) -> Option<Workspace> {
        self.data
            .lock()
            .unwrap()
            .workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .cloned()
    }

    /// The workspace containing `project_id`. Migration wrapped each existing
    /// project 1:1 reusing its id, so callers can safely fall back to the
    /// project id when a member lookup misses.
    pub fn workspace_for_project(&self, project_id: &str) -> Option<String> {
        self.data
            .lock()
            .unwrap()
            .workspaces
            .iter()
            .find(|w| w.members.iter().any(|m| m.project_id == project_id))
            .map(|w| w.id.clone())
    }

    pub fn rename_workspace(&self, workspace_id: &str, name: String) -> Result<Workspace> {
        let name = name.trim().to_string();
        anyhow::ensure!(!name.is_empty(), "name cannot be empty");
        let mut data = self.data.lock().unwrap();
        let ws = data
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .context("unknown workspace")?;
        ws.name = name;
        // updated_at is the LWW clock for cross-machine rename reconciliation.
        ws.updated_at = now_iso();
        let out = ws.clone();
        self.flush(&data)?;
        Ok(out)
    }

    pub fn set_workspace_image(
        &self,
        workspace_id: &str,
        image: Option<String>,
    ) -> Result<Workspace> {
        let mut data = self.data.lock().unwrap();
        let ws = data
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .context("unknown workspace")?;
        ws.image = image;
        // The image participates in the whole-record LWW mesh replica.
        ws.updated_at = now_iso();
        let out = ws.clone();
        self.flush(&data)?;
        Ok(out)
    }

    pub fn set_workspace_favorite(&self, workspace_id: &str, favorite: bool) -> Result<Workspace> {
        let mut data = self.data.lock().unwrap();
        let ws = data
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .context("unknown workspace")?;
        ws.favorite = favorite.then_some(true);
        // The favorite flag participates in the whole-record LWW mesh replica.
        ws.updated_at = now_iso();
        let out = ws.clone();
        self.flush(&data)?;
        Ok(out)
    }

    /// Stash a workspace out of the sidebar (or bring it back). Nothing is
    /// deleted or detached — its projects, roots and chats are untouched and its
    /// agents keep running; only the sidebar stops listing it.
    pub fn set_workspace_hidden(&self, workspace_id: &str, hidden: bool) -> Result<Workspace> {
        let mut data = self.data.lock().unwrap();
        let ws = data
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .context("unknown workspace")?;
        ws.hidden = hidden.then_some(true);
        // The hidden flag participates in the whole-record LWW mesh replica, so
        // putting a project away here puts it away on every paired machine.
        ws.updated_at = now_iso();
        let out = ws.clone();
        self.flush(&data)?;
        Ok(out)
    }

    // ---- repos ----

    /// Sync persisted repo records with a fresh discovery scan: keep records
    /// whose relPath was found again (stable ids), create records for new
    /// repos, drop records whose folder is gone. Returns them sorted by path.
    pub fn reconcile_repos(&self, project_id: &str, rel_paths: &[String]) -> Result<Vec<RepoRecord>> {
        let mut data = self.data.lock().unwrap();
        let before = data.repos.len();
        data.repos.retain(|r| {
            r.project_id != project_id || rel_paths.contains(&r.rel_path)
        });
        let mut created = false;
        for rel in rel_paths {
            let exists = data
                .repos
                .iter()
                .any(|r| r.project_id == project_id && &r.rel_path == rel);
            if !exists {
                created = true;
                data.repos.push(RepoRecord {
                    id: new_id(),
                    project_id: project_id.to_string(),
                    rel_path: rel.clone(),
                    created_at: now_iso(),
                });
            }
        }
        if created || data.repos.len() != before {
            self.flush(&data)?;
        }
        let mut out: Vec<RepoRecord> = data
            .repos
            .iter()
            .filter(|r| r.project_id == project_id)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        Ok(out)
    }

    pub fn repo(&self, repo_id: &str) -> Option<RepoRecord> {
        self.data
            .lock()
            .unwrap()
            .repos
            .iter()
            .find(|r| r.id == repo_id)
            .cloned()
    }

    // ---- threads ----

    /// Lazily create the hidden Hermes home project (see
    /// [`HERMES_HOME_PROJECT_ID`]). Returns true when it was just created so
    /// the caller can broadcast a `projects` refresh; deliberately does NOT
    /// wrap it in a workspace.
    pub fn ensure_hermes_home(&self) -> Result<bool> {
        let mut data = self.data.lock().unwrap();
        if data.projects.iter().any(|p| p.id == HERMES_HOME_PROJECT_ID) {
            return Ok(false);
        }
        // Appended (not inserted at 0) so it never displaces real projects in
        // sidebar-order-sensitive lists; path is empty — there is no folder.
        data.projects.push(Project {
            id: HERMES_HOME_PROJECT_ID.into(),
            name: "Hermes agents".into(),
            path: String::new(),
            created_at: now_iso(),
        });
        self.flush(&data)?;
        Ok(true)
    }

    /// Lazily create this machine's hidden Quick Chats home. The project gives
    /// the existing thread protocol a stable owner while remaining absent from
    /// workspaces; its path is private application data, never a user-chosen
    /// project folder.
    pub fn ensure_quick_home(&self) -> Result<bool> {
        let machine_id = self.local_machine_id();
        anyhow::ensure!(!machine_id.is_empty(), "machine identity is not ready");
        let id = quick_home_project_id(&machine_id);
        let root = self.dir.join("quick");
        std::fs::create_dir_all(&root)?;
        restrict_dir(&root);
        let mut data = self.data.lock().unwrap();
        if data.projects.iter().any(|p| p.id == id) {
            return Ok(false);
        }
        data.projects.push(Project {
            id,
            name: "Quick chats".into(),
            path: root.to_string_lossy().into_owned(),
            created_at: now_iso(),
        });
        self.flush(&data)?;
        Ok(true)
    }

    /// Effective cwd for a thread. Folder threads use their project's real
    /// root; Quick Chats get one private scratch directory apiece so files and
    /// attachments from unrelated questions cannot bleed into each other.
    pub fn thread_working_dir(&self, thread: &Thread) -> Result<PathBuf> {
        if is_quick_home_project_id(&thread.project_id) {
            let dir = self.dir.join("quick").join(&thread.id);
            std::fs::create_dir_all(&dir)?;
            restrict_dir(&dir);
            return Ok(dir);
        }
        let project = self
            .project(&thread.project_id)
            .context("unknown project")?;
        Ok(PathBuf::from(project.path))
    }

    pub fn create_thread(
        &self,
        project_id: String,
        agent: Agent,
        settings: ThreadSettings,
    ) -> Result<Thread> {
        anyhow::ensure!(self.project(&project_id).is_some(), "unknown project");
        let now = now_iso();
        let id = new_id();
        if is_quick_home_project_id(&project_id) {
            let dir = self.dir.join("quick").join(&id);
            std::fs::create_dir_all(&dir)?;
            restrict_dir(&dir);
        }
        let thread = Thread {
            id,
            project_id,
            machine_id: self.local_machine_id(),
            agent,
            title: "New thread".into(),
            settings,
            provider_session_id: None,
            dispatch: None,
            session_anchors: HashMap::new(),
            provider_run_id: None,
            status: ThreadStatus::Idle,
            settled_at: None,
            kept_active_at: None,
            favorite: None,
            // Left empty on purpose: one implicit Builder derived from `agent`
            // (see `Thread::participants_resolved`). Materialized only when a
            // second lane joins, so ordinary threads carry no extra state.
            participants: Vec::new(),
            active_speaker: None,
            parley: None,
            created_at: now.clone(),
            updated_at: now,
        };
        let mut data = self.data.lock().unwrap();
        data.threads.insert(0, thread.clone());
        self.flush(&data)?;
        Ok(thread)
    }

    pub fn list_threads(&self, project_id: &str) -> Vec<Thread> {
        self.data
            .lock()
            .unwrap()
            .threads
            .iter()
            .filter(|t| t.project_id == project_id)
            .cloned()
            .collect()
    }

    pub fn thread(&self, thread_id: &str) -> Option<Thread> {
        self.data
            .lock()
            .unwrap()
            .threads
            .iter()
            .find(|t| t.id == thread_id)
            .cloned()
    }

    pub fn update_thread(&self, thread_id: &str, f: impl FnOnce(&mut Thread)) -> Result<Thread> {
        self.write_thread(thread_id, true, f)
    }

    /// `update_thread` without the `updated_at` bump. For changes that are
    /// about how the user FILES a thread — settling it into the sidebar shelf,
    /// or pulling it back out — rather than work happening inside it. Bumping
    /// the activity clock there would make a thread untouched for ten days
    /// report as fresh, and would reset the idle window that auto-settle
    /// measures.
    pub fn update_thread_quiet(
        &self,
        thread_id: &str,
        f: impl FnOnce(&mut Thread),
    ) -> Result<Thread> {
        self.write_thread(thread_id, false, f)
    }

    /// `update_thread_quiet` whose closure may refuse the write. The check runs
    /// under the SAME lock as the mutation, which a read-then-write pair can't
    /// promise: between two acquisitions an agent event can flip the thread to
    /// running and clear its settled fields, and the write would then re-park a
    /// thread that just started work.
    pub fn try_update_thread_quiet(
        &self,
        thread_id: &str,
        f: impl FnOnce(&mut Thread) -> Result<()>,
    ) -> Result<Thread> {
        let mut data = self.data.lock().unwrap();
        let thread = data
            .threads
            .iter_mut()
            .find(|t| t.id == thread_id)
            .context("unknown thread")?;
        f(thread)?;
        let out = thread.clone();
        self.flush(&data)?;
        Ok(out)
    }

    fn write_thread(
        &self,
        thread_id: &str,
        touch: bool,
        f: impl FnOnce(&mut Thread),
    ) -> Result<Thread> {
        let mut data = self.data.lock().unwrap();
        let thread = data
            .threads
            .iter_mut()
            .find(|t| t.id == thread_id)
            .context("unknown thread")?;
        f(thread);
        if touch {
            thread.updated_at = now_iso();
        }
        let out = thread.clone();
        self.flush(&data)?;
        Ok(out)
    }

    /// Toggle a thread's favorite flag WITHOUT touching `updated_at`. Starring
    /// is not chat activity, so it must not bump recency: the thread would jump
    /// in the sidebar's recency order and "Xm ago" would lie. Threads are
    /// machineId-routed to their owner (not LWW-replicated), so preserving the
    /// old timestamp is safe.
    pub fn set_thread_favorite(&self, thread_id: &str, favorite: bool) -> Result<Thread> {
        let mut data = self.data.lock().unwrap();
        let thread = data
            .threads
            .iter_mut()
            .find(|t| t.id == thread_id)
            .context("unknown thread")?;
        thread.favorite = favorite.then_some(true);
        let out = thread.clone();
        self.flush(&data)?;
        Ok(out)
    }

    pub fn delete_thread(&self, thread_id: &str) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        let quick_dir = data
            .threads
            .iter()
            .find(|t| t.id == thread_id && is_quick_home_project_id(&t.project_id))
            .map(|t| self.dir.join("quick").join(&t.id));
        data.threads.retain(|t| t.id != thread_id);
        data.artifacts.retain(|a| a.thread_id != thread_id);
        self.flush(&data)?;
        let _ = std::fs::remove_file(self.events_path(thread_id));
        let _ = std::fs::remove_dir_all(self.thread_artifacts_dir(thread_id));
        if let Some(dir) = quick_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
        Ok(())
    }

    // ---- schedules ----

    pub fn create_schedule(&self, schedule: Schedule) -> Result<Schedule> {
        anyhow::ensure!(self.project(&schedule.project_id).is_some(), "unknown project");
        let mut data = self.data.lock().unwrap();
        data.schedules.insert(0, schedule.clone());
        self.flush(&data)?;
        Ok(schedule)
    }

    pub fn list_schedules(&self) -> Vec<Schedule> {
        self.data.lock().unwrap().schedules.clone()
    }

    pub fn schedule(&self, schedule_id: &str) -> Option<Schedule> {
        self.data
            .lock()
            .unwrap()
            .schedules
            .iter()
            .find(|s| s.id == schedule_id)
            .cloned()
    }

    pub fn update_schedule(
        &self,
        schedule_id: &str,
        f: impl FnOnce(&mut Schedule),
    ) -> Result<Schedule> {
        let mut data = self.data.lock().unwrap();
        let schedule = data
            .schedules
            .iter_mut()
            .find(|s| s.id == schedule_id)
            .context("unknown schedule")?;
        f(schedule);
        let out = schedule.clone();
        self.flush(&data)?;
        Ok(out)
    }

    pub fn delete_schedule(&self, schedule_id: &str) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        data.schedules.retain(|s| s.id != schedule_id);
        self.flush(&data)
    }

    // ---- terminals ----

    pub fn create_terminal(&self, project_id: String, name: Option<String>) -> Result<TerminalRecord> {
        anyhow::ensure!(self.project(&project_id).is_some(), "unknown project");
        let mut data = self.data.lock().unwrap();
        let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| {
            let n = data.terminals.iter().filter(|t| t.project_id == project_id).count();
            format!("Terminal {}", n + 1)
        });
        let terminal = TerminalRecord {
            id: new_id(),
            project_id,
            name,
            created_at: now_iso(),
        };
        data.terminals.push(terminal.clone());
        self.flush(&data)?;
        Ok(terminal)
    }

    pub fn list_terminals(&self, project_id: &str) -> Vec<TerminalRecord> {
        self.data
            .lock()
            .unwrap()
            .terminals
            .iter()
            .filter(|t| t.project_id == project_id)
            .cloned()
            .collect()
    }

    pub fn terminal(&self, terminal_id: &str) -> Option<TerminalRecord> {
        self.data
            .lock()
            .unwrap()
            .terminals
            .iter()
            .find(|t| t.id == terminal_id)
            .cloned()
    }

    pub fn rename_terminal(&self, terminal_id: &str, name: String) -> Result<TerminalRecord> {
        let name = name.trim().to_string();
        anyhow::ensure!(!name.is_empty(), "name cannot be empty");
        let mut data = self.data.lock().unwrap();
        let terminal = data
            .terminals
            .iter_mut()
            .find(|t| t.id == terminal_id)
            .context("unknown terminal")?;
        terminal.name = name;
        let out = terminal.clone();
        self.flush(&data)?;
        Ok(out)
    }

    pub fn delete_terminal(&self, terminal_id: &str) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        data.terminals.retain(|t| t.id != terminal_id);
        self.flush(&data)
    }

    // ---- archives ----

    /// The directory archive files live in: the configured override or the
    /// default `~/.threadknot/archives`.
    pub fn archive_dir(&self) -> PathBuf {
        self.data
            .lock()
            .unwrap()
            .archive_dir
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir().join("archives"))
    }

    /// Point the archive directory at `dir` (created if absent) and persist it.
    pub fn set_archive_dir(&self, dir: &str) -> Result<PathBuf> {
        let path = PathBuf::from(dir);
        std::fs::create_dir_all(&path)
            .with_context(|| format!("create archive dir: {dir}"))?;
        let mut data = self.data.lock().unwrap();
        data.archive_dir = Some(path.to_string_lossy().into_owned());
        self.flush(&data)?;
        Ok(path)
    }

    /// Resolve `<archiveDir>/<id>.json`, rejecting any id that could escape the
    /// archive directory. Archive ids are server-minted UUIDs, so a legitimate
    /// id only ever contains `[A-Za-z0-9_-]`; anything else (a path separator,
    /// `..`, or an empty string) is a traversal attempt via a client-supplied
    /// `archiveId` — e.g. `"../projects"` would otherwise resolve to the live
    /// store. Reject rather than sanitize so a bad id can never map to a real
    /// neighbouring file.
    fn archive_path(&self, id: &str) -> Result<PathBuf> {
        anyhow::ensure!(
            !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "invalid archive id"
        );
        Ok(self.archive_dir().join(format!("{id}.json")))
    }

    /// Atomically write an archive file to `<archiveDir>/<id>.json`.
    pub fn write_archive(&self, file: &ArchiveFile) -> Result<()> {
        let dir = self.archive_dir();
        std::fs::create_dir_all(&dir).context("create archive dir")?;
        let path = dir.join(format!("{}.json", file.header.id));
        let tmp = dir.join(format!("{}.json.tmp", file.header.id));
        std::fs::write(&tmp, serde_json::to_string_pretty(file)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// List archive headers, newest first. Foreign/corrupt files are skipped and
    /// a missing archive dir yields an empty list (never panics).
    pub fn list_archives(&self) -> Vec<ArchiveHeader> {
        let Ok(entries) = std::fs::read_dir(self.archive_dir()) else {
            return Vec::new();
        };
        let mut headers: Vec<ArchiveHeader> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
            .filter_map(|p| std::fs::read_to_string(&p).ok())
            .filter_map(|t| serde_json::from_str::<ArchiveFile>(&t).ok())
            .map(|f| f.header)
            .collect();
        headers.sort_by(|a, b| b.archived_at.cmp(&a.archived_at));
        headers
    }

    /// Read and parse a single archive file.
    pub fn read_archive(&self, id: &str) -> Result<ArchiveFile> {
        let path = self.archive_path(id)?;
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("archive not found: {id}"))?;
        serde_json::from_str(&text).with_context(|| format!("parse archive: {id}"))
    }

    /// Delete an archive file, ignoring the case where it is already gone.
    pub fn delete_archive(&self, id: &str) -> Result<()> {
        match std::fs::remove_file(self.archive_path(id)?) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("delete archive: {id}")),
        }
    }

    /// Re-insert an archived thread and rewrite its full event log. Fails if the
    /// thread's project no longer exists.
    pub fn restore_thread(
        &self,
        mut thread: Thread,
        events: Vec<PersistedEvent>,
    ) -> Result<Thread> {
        // Pre-mesh archives carry no machine id; restoring adopts the thread
        // onto this machine (its project exists here, so it IS local).
        if thread.machine_id.is_empty() {
            thread.machine_id = self.local_machine_id();
        }
        // Restoring is a deliberate "I want this back", so it must not land in
        // the sidebar's settled shelf. `updated_at` is whatever it was when
        // archived — often months old — which auto-settle would park on sight.
        // Restart the idle clock (the field's whole purpose) and drop any
        // stale park stamp rather than lying about when the thread last moved.
        thread.settled_at = None;
        thread.kept_active_at = Some(now_iso());
        anyhow::ensure!(
            self.project(&thread.project_id).is_some(),
            "the archived thread's project no longer exists. Re-add the project, then restore"
        );
        {
            let mut data = self.data.lock().unwrap();
            data.threads.retain(|t| t.id != thread.id);
            data.threads.insert(0, thread.clone());
            self.flush(&data)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(self.events_path(&thread.id))?;
        for e in &events {
            writeln!(file, "{}", serde_json::to_string(e)?)?;
        }
        let next_seq = events.last().map(|e| e.seq + 1).unwrap_or(0);
        self.seqs
            .lock()
            .unwrap()
            .insert(thread.id.clone(), next_seq);
        Ok(thread)
    }

    // ---- events ----

    fn events_path(&self, thread_id: &str) -> PathBuf {
        self.dir.join("threads").join(format!("{thread_id}.jsonl"))
    }

    pub fn read_events(&self, thread_id: &str) -> Vec<PersistedEvent> {
        let Ok(text) = std::fs::read_to_string(self.events_path(thread_id)) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// The last thing the assistant actually said, if anything.
    ///
    /// The salvage path for a dispatched worker that finished without calling
    /// `report_result`. Reuses `thread_preview`, which already scans the log
    /// line-by-line for exactly this value rather than materializing the
    /// transcript — a worker's log can be long, and this runs at every turn
    /// boundary on the machine that just did the work.
    pub fn last_assistant_text(&self, thread_id: &str) -> Option<String> {
        self.thread_preview(thread_id)
            .summary
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Summarize a thread's persisted log for a hover card (`thread.preview`):
    /// the last `assistant_message` text, the last `user_message` text, and how
    /// many turns completed. Scans the JSONL line-by-line rather than materializing
    /// the whole transcript, and yields an empty preview (`turn_count: 0`, no
    /// text) for an unknown or empty thread.
    pub fn thread_preview(&self, thread_id: &str) -> ThreadPreview {
        use std::io::BufRead as _;
        let mut summary: Option<String> = None;
        let mut last_user: Option<String> = None;
        let mut turn_count: u64 = 0;
        if let Ok(file) = std::fs::File::open(self.events_path(thread_id)) {
            for line in std::io::BufReader::new(file).lines() {
                let Ok(line) = line else { break };
                let Ok(persisted) = serde_json::from_str::<PersistedEvent>(&line) else {
                    continue;
                };
                match persisted.event {
                    AgentEvent::AssistantMessage { text } => summary = Some(text),
                    AgentEvent::UserMessage { text, .. } => last_user = Some(text),
                    AgentEvent::TurnCompleted { .. } => turn_count += 1,
                    _ => {}
                }
            }
        }
        ThreadPreview {
            thread_id: thread_id.to_string(),
            summary: summary.map(|t| truncate_chars(&t, 300)),
            last_user: last_user.map(|t| truncate_chars(&t, 200)),
            turn_count,
        }
    }

    /// Collect just enough current-turn context to write a useful completion
    /// notification. The scan stays streaming so a long-running chat does not
    /// need to materialize its transcript at the turn boundary.
    pub fn completion_notice_context(&self, thread_id: &str) -> crate::notices::CompletionContext {
        use std::io::BufRead as _;
        let mut context = crate::notices::CompletionContext::default();
        if let Ok(file) = std::fs::File::open(self.events_path(thread_id)) {
            for line in std::io::BufReader::new(file).lines() {
                let Ok(line) = line else { break };
                let Ok(persisted) = serde_json::from_str::<PersistedEvent>(&line) else {
                    continue;
                };
                context.observe(&persisted.event);
            }
        }
        context
    }

    /// Find the requested threads whose persisted transcript contains `query`.
    ///
    /// Only user-visible text is searched (messages, reasoning, tool cards,
    /// questions, diffs, status/error text, and artifacts), rather than raw
    /// JSON keys or ids. Thread ids are intersected with this store's catalog
    /// before any path is opened, so an authenticated client cannot turn this
    /// read-only endpoint into an arbitrary-file probe.
    pub fn search_thread_content(&self, thread_ids: &[String], query: &str) -> Vec<String> {
        use std::collections::HashSet;
        use std::io::BufRead as _;

        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let requested: HashSet<&str> = thread_ids.iter().map(String::as_str).collect();
        let allowed: Vec<String> = self
            .data
            .lock()
            .unwrap()
            .threads
            .iter()
            .filter(|thread| requested.contains(thread.id.as_str()))
            .map(|thread| thread.id.clone())
            .collect();

        allowed
            .into_iter()
            .filter(|thread_id| {
                let Ok(file) = std::fs::File::open(self.events_path(thread_id)) else {
                    return false;
                };
                for line in std::io::BufReader::new(file).lines() {
                    let Ok(line) = line else { break };
                    let Ok(persisted) = serde_json::from_str::<PersistedEvent>(&line) else {
                        continue;
                    };
                    if event_contains(&persisted.event, &needle) {
                        return true;
                    }
                }
                false
            })
            .collect()
    }

    /// Append an event, returning its assigned seq and authoritative timestamp.
    /// `speaker` is the producing participant id (`None` for the user).
    pub fn append_event(
        &self,
        thread_id: &str,
        speaker: Option<&str>,
        event: &AgentEvent,
    ) -> Result<(u64, String)> {
        let mut seqs = self.seqs.lock().unwrap();
        let next = seqs.entry(thread_id.to_string()).or_insert_with(|| {
            self.read_events(thread_id)
                .last()
                .map(|e| e.seq + 1)
                .unwrap_or(0)
        });
        let seq = *next;
        *next += 1;
        let ts = now_iso();
        let persisted = PersistedEvent {
            seq,
            ts: ts.clone(),
            speaker: speaker.map(|s| s.to_string()),
            event: event.clone(),
        };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.events_path(thread_id))?;
        writeln!(file, "{}", serde_json::to_string(&persisted)?)?;
        Ok((seq, ts))
    }

    // ---- attachments ----

    fn thread_attachments_dir(&self, thread_id: &str) -> PathBuf {
        self.dir.join("attachments").join(safe_segment(thread_id))
    }

    /// Persist attachment bytes to `~/.threadknot/attachments/<thread>/<id>.<ext>`
    /// and return the metadata to embed in the event log.
    pub fn write_attachment(
        &self,
        thread_id: &str,
        name: &str,
        mime_type: &str,
        bytes: &[u8],
    ) -> Result<AttachmentMeta> {
        let id = uuid::Uuid::new_v4().to_string();
        let dir = self.thread_attachments_dir(thread_id);
        std::fs::create_dir_all(&dir).context("create attachments dir")?;
        std::fs::write(dir.join(format!("{id}.{}", ext_for_mime(mime_type))), bytes)
            .context("write attachment")?;
        Ok(AttachmentMeta {
            id,
            name: name.to_string(),
            mime_type: mime_type.to_string(),
            size_bytes: bytes.len() as u64,
        })
    }

    /// Resolve a stored attachment file, guarding against path traversal.
    pub fn attachment_path(&self, thread_id: &str, id: &str) -> Option<PathBuf> {
        if !is_safe_id(id) {
            return None;
        }
        let dir = self.thread_attachments_dir(thread_id);
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.file_stem().and_then(|s| s.to_str()) == Some(id))
    }

    // ---- artifacts ----

    fn thread_artifacts_dir(&self, thread_id: &str) -> PathBuf {
        self.dir.join("artifacts").join(safe_segment(thread_id))
    }

    /// Insert (or update, deduped by `thread_id` + `rel_path`) an artifact
    /// record. On update the stable `id` is reused, `op` becomes "modified",
    /// and `created_at` is bumped so it sorts to the top. Returns the record.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_artifact(
        &self,
        thread_id: &str,
        project_id: &str,
        rel_path: &str,
        name: &str,
        mime_type: &str,
        size_bytes: u64,
        source: &str,
        origin: &str,
        description: Option<&str>,
    ) -> Result<ArtifactRecord> {
        let mut data = self.data.lock().unwrap();
        let record = if let Some(existing) = data
            .artifacts
            .iter_mut()
            .find(|a| a.thread_id == thread_id && a.rel_path == rel_path)
        {
            existing.name = name.to_string();
            existing.mime_type = mime_type.to_string();
            existing.size_bytes = size_bytes;
            existing.source = source.to_string();
            existing.op = "modified".to_string();
            existing.origin = origin.to_string();
            if let Some(desc) = description {
                existing.description = Some(desc.to_string());
            }
            existing.created_at = now_iso();
            existing.clone()
        } else {
            let record = ArtifactRecord {
                id: new_id(),
                thread_id: thread_id.to_string(),
                project_id: project_id.to_string(),
                name: name.to_string(),
                rel_path: rel_path.to_string(),
                mime_type: mime_type.to_string(),
                size_bytes,
                source: source.to_string(),
                op: "created".to_string(),
                origin: origin.to_string(),
                description: description.map(str::to_string),
                created_at: now_iso(),
            };
            data.artifacts.insert(0, record.clone());
            record
        };
        self.flush(&data)?;
        Ok(record)
    }

    /// Persist a snapshot of the produced file's bytes so the artifact stays
    /// downloadable even if the working-tree file later moves or is deleted.
    pub fn write_artifact_snapshot(
        &self,
        thread_id: &str,
        id: &str,
        ext: &str,
        bytes: &[u8],
    ) -> Result<()> {
        let dir = self.thread_artifacts_dir(thread_id);
        std::fs::create_dir_all(&dir).context("create artifacts dir")?;
        let name = if ext.is_empty() {
            id.to_string()
        } else {
            format!("{id}.{ext}")
        };
        std::fs::write(dir.join(name), bytes).context("write artifact snapshot")?;
        Ok(())
    }

    /// Resolve a stored artifact snapshot file, guarding against path traversal.
    pub fn artifact_snapshot_path(&self, thread_id: &str, id: &str) -> Option<PathBuf> {
        if !is_safe_id(id) {
            return None;
        }
        let dir = self.thread_artifacts_dir(thread_id);
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.file_stem().and_then(|s| s.to_str()) == Some(id))
    }

    pub fn artifact_by_id(&self, id: &str) -> Option<ArtifactRecord> {
        self.data
            .lock()
            .unwrap()
            .artifacts
            .iter()
            .find(|a| a.id == id)
            .cloned()
    }

    /// Remove one artifact from Threadknot's index and delete its durable copies.
    /// The original file in the project workspace is deliberately untouched.
    pub fn delete_artifact(&self, id: &str) -> Result<ArtifactRecord> {
        let mut data = self.data.lock().unwrap();
        let index = data
            .artifacts
            .iter()
            .position(|artifact| artifact.id == id)
            .context("unknown artifact")?;
        let artifact = data.artifacts.remove(index);
        self.flush(&data)?;

        if let Some(path) = self.artifact_snapshot_path(&artifact.thread_id, &artifact.id) {
            let _ = std::fs::remove_file(path);
        }
        // Native clipboard copies are materialized beneath this folder. Remove
        // them too so deleting an artifact doesn't leave a second durable copy.
        let clipboard_copy = self
            .thread_artifacts_dir(&artifact.thread_id)
            .join("clipboard")
            .join(safe_segment(&artifact.id));
        let _ = std::fs::remove_dir_all(clipboard_copy);

        Ok(artifact)
    }

    /// All of a project's artifacts, newest first.
    pub fn list_artifacts_for_project(&self, project_id: &str) -> Vec<ArtifactRecord> {
        let mut v: Vec<ArtifactRecord> = self
            .data
            .lock()
            .unwrap()
            .artifacts
            .iter()
            .filter(|a| a.project_id == project_id)
            .cloned()
            .collect();
        v.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        v
    }

    /// One thread's artifacts, newest first.
    pub fn list_artifacts_for_thread(&self, thread_id: &str) -> Vec<ArtifactRecord> {
        let mut v: Vec<ArtifactRecord> = self
            .data
            .lock()
            .unwrap()
            .artifacts
            .iter()
            .filter(|a| a.thread_id == thread_id)
            .cloned()
            .collect();
        v.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        v
    }
}

/// Map an image mime type to a file extension (defaults to `bin`).
pub fn ext_for_mime(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "bin",
    }
}

/// Content type to serve for a stored attachment extension.
pub fn mime_for_ext(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

/// Trim `s` to at most `max` characters, appending "..." (three dots, not the
/// ellipsis char) when it was actually shortened. Cuts on a char boundary via
/// `char_indices`, so a multi-byte UTF-8 char is never split.
fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((byte_idx, _)) => {
            let mut out = s[..byte_idx].trim_end().to_string();
            out.push_str("...");
            out
        }
        None => s.to_string(),
    }
}

fn text_contains(text: &str, lowercase_needle: &str) -> bool {
    text.to_lowercase().contains(lowercase_needle)
}

/// Search every textual field that can be rendered as transcript content.
/// Transient delta variants are included for completeness even though the
/// store never persists them.
fn event_contains(event: &AgentEvent, needle: &str) -> bool {
    let contains = |text: &str| text_contains(text, needle);
    match event {
        AgentEvent::UserMessage {
            text, attachments, ..
        } => contains(text) || attachments.iter().any(|a| contains(&a.name)),
        AgentEvent::AssistantDelta { text }
        | AgentEvent::AssistantMessage { text }
        | AgentEvent::ThinkingDelta { text }
        | AgentEvent::Thinking { text }
        | AgentEvent::ToolOutputDelta { text, .. }
        | AgentEvent::Status { text }
        | AgentEvent::SubagentProgress { text, .. } => contains(text),
        AgentEvent::ToolStart { name, detail, .. } => contains(name) || contains(detail),
        AgentEvent::ToolEnd { name, output, .. } => {
            contains(name) || output.as_deref().is_some_and(contains)
        }
        AgentEvent::FileDiff { path, unified } => contains(path) || contains(unified),
        AgentEvent::Artifact {
            name,
            rel_path,
            description,
            ..
        } => contains(name) || contains(rel_path) || description.as_deref().is_some_and(contains),
        AgentEvent::ApprovalRequest {
            title,
            detail,
            options,
            ..
        } => {
            contains(title)
                || contains(detail)
                || options.iter().any(|option| contains(&option.label))
        }
        AgentEvent::QuestionRequest { questions, .. } => questions.iter().any(|question| {
            contains(&question.header)
                || contains(&question.question)
                || question
                    .options
                    .iter()
                    .any(|option| contains(&option.label) || contains(&option.description))
        }),
        AgentEvent::QuestionResolved { answers, .. } => answers
            .as_ref()
            .is_some_and(|answers| answers.values().flatten().any(|answer| contains(answer))),
        AgentEvent::TurnStarted { model, .. } => model.as_deref().is_some_and(contains),
        AgentEvent::SessionStarted { model, .. } => contains(model),
        AgentEvent::SubagentStarted {
            description,
            subagent_type,
            ..
        } => contains(description) || contains(subagent_type),
        AgentEvent::SubagentCompleted {
            status, summary, ..
        } => contains(status) || summary.as_deref().is_some_and(contains),
        AgentEvent::Error { message } => contains(message),
        AgentEvent::ApprovalResolved { option_id, .. } => contains(option_id),
        AgentEvent::TurnCompleted { .. }
        | AgentEvent::TurnAborted
        | AgentEvent::ContextUsage { .. } => false,
    }
}

fn safe_segment(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect()
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Write a secret-bearing file so only its owner can read it.
///
/// Created owner-only from the start — writing first and chmod-ing after leaves
/// a window where the master token is world-readable, and on a shared box that
/// window is all anyone needs. On Windows the file inherits the user profile's
/// ACL, which is already owner-scoped for the default layout; a dedicated ACL
/// pass is tracked in docs/REMOTE-ACCESS-SECURITY.md (SEC-013).
pub fn write_private(path: &std::path::Path, contents: &str) -> Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("write {}", path.display()))?;
    file.write_all(contents.as_bytes())?;
    // An existing file keeps its old mode through `open`, so repair it too.
    restrict_file(path);
    Ok(())
}

/// Force an existing secret-bearing file to owner-only. No-op off Unix and
/// best-effort: a store on a filesystem without Unix modes (a VFAT stick, a
/// network share) must still start.
pub fn restrict_file(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.exists() {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Same, for the directory holding those files.
pub fn restrict_dir(dir: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if dir.exists() {
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
}

pub fn data_dir() -> PathBuf {
    // Override lets a second instance run against its own store on the same
    // box (mesh testing: two ports, two data dirs, one machine). The pre-rename
    // spelling still works so an existing launcher or script keeps running.
    if let Some(dir) = std::env::var_os("THREADKNOT_DATA_DIR")
        .or_else(|| std::env::var_os("ARMADA_DATA_DIR"))
    {
        return PathBuf::from(dir);
    }
    data_dir_in(&dirs::home_dir().expect("no home dir"))
}

/// Armada's store was `~/.armada`. Nothing is moved on rename: an install that
/// predates Threadknot keeps using the directory it already filled — 500+ MB of
/// browser profiles with live logins, paired phones and the LAN token is not
/// worth the risk of a half-finished move. Only a fresh install starts life in
/// `~/.threadknot`, and preferring the new path means a user who *does* migrate
/// by hand is picked up immediately.
fn data_dir_in(home: &Path) -> PathBuf {
    let new = home.join(".threadknot");
    let legacy = home.join(".armada");
    // Key off `projects.json` — the store's actual index — not the directory.
    // A bare directory proves nothing: a log file, an `mkdir -p` in a launcher
    // script, or a half-finished copy all create one, and treating that as
    // "already migrated" strands the user's real store in `~/.armada` and
    // silently starts them with an empty app. That exact bug shipped once.
    if new.join("projects.json").exists() {
        return new;
    }
    if legacy.join("projects.json").exists() {
        return legacy;
    }
    new
}

pub fn generate_token() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    (0..32)
        .map(|_| {
            let chars = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
            chars[rng.random_range(0..chars.len())] as char
        })
        .collect()
}

/// SEC-013: the master token at rest.
#[cfg(all(test, unix))]
mod secret_permissions_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("threadknot-perm-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn mode(path: &std::path::Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_fresh_server_json_is_owner_only() {
        let dir = scratch();
        let store = Store::open_at(dir.clone()).unwrap();
        let cfg = store.server_config().unwrap();
        assert!(!cfg.token.is_empty());
        assert_eq!(mode(&dir.join("server.json")), 0o600);
        assert_eq!(mode(&dir), 0o700, "the directory holding it, too");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The case that actually matters: an install that predates this, or one
    /// restored from a permissive backup, already has a world-readable master
    /// token on disk. Creating new files correctly does nothing for it — the
    /// mode has to be repaired when the file is read.
    #[test]
    fn an_existing_permissive_server_json_is_repaired() {
        let dir = scratch();
        let store = Store::open_at(dir.clone()).unwrap();
        let first = store.server_config().unwrap();

        let path = dir.join("server.json");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        let reopened = Store::open_at(dir.clone()).unwrap();
        let again = reopened.server_config().unwrap();
        assert_eq!(again.token, first.token, "repair must not rotate the token");
        assert_eq!(mode(&path), 0o600, "world-readable master token repaired");
        assert_eq!(mode(&dir), 0o700);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A rewrite (the server-id backfill path) must not hand the file back to
    /// the umask.
    #[test]
    fn rewriting_the_config_keeps_it_owner_only() {
        let dir = scratch();
        std::fs::create_dir_all(&dir).unwrap();
        // `server_id`, not `serverId`: `ServerConfig` has no
        // `rename_all = "camelCase"`, so the camelCase spelling is an unknown
        // field that serde silently ignores — the fixture then exercised a
        // *default* empty id rather than the blank one it looked like it was
        // supplying, and the test passed for the wrong reason.
        std::fs::write(
            dir.join("server.json"),
            r#"{"port":42800,"token":"legacy-token","server_id":""}"#,
        )
        .unwrap();
        std::fs::set_permissions(
            dir.join("server.json"),
            std::fs::Permissions::from_mode(0o666),
        )
        .unwrap();

        let store = Store::open_at(dir.clone()).unwrap();
        let cfg = store.server_config().unwrap();
        assert_eq!(cfg.token, "legacy-token");
        assert!(!cfg.server_id.is_empty(), "server id was minted on disk");
        assert_eq!(mode(&dir.join("server.json")), 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod quick_home_tests {
    use super::*;

    fn settings() -> ThreadSettings {
        ThreadSettings {
            model: "test-model".into(),
            effort: None,
            wide_context: false,
            claude_chrome: false,
            access: Access::Read,
            mode: Mode::Build,
            browser_profile_id: None,
        }
    }

    #[test]
    fn quick_threads_are_hidden_and_get_isolated_scratch_directories() {
        let dir = std::env::temp_dir().join(format!(
            "threadknot-quick-home-test-{}",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open_at(dir.clone()).unwrap();
        store.migrate_mesh("machine-quick").unwrap();
        assert!(store.ensure_quick_home().unwrap());
        assert!(!store.ensure_quick_home().unwrap());

        let project_id = quick_home_project_id("machine-quick");
        assert!(store.project(&project_id).is_some());
        // Even a later migration must not turn the private home into a visible
        // workspace section.
        store.migrate_mesh("machine-quick").unwrap();
        assert!(store.workspace_for_project(&project_id).is_none());

        let first = store
            .create_thread(project_id.clone(), Agent::Claude, settings())
            .unwrap();
        let second = store
            .create_thread(project_id, Agent::Codex, settings())
            .unwrap();
        let first_dir = store.thread_working_dir(&first).unwrap();
        let second_dir = store.thread_working_dir(&second).unwrap();
        assert_ne!(first_dir, second_dir);
        assert!(first_dir.is_dir());
        assert!(second_dir.is_dir());

        std::fs::write(first_dir.join("only-here.txt"), b"private").unwrap();
        assert!(!second_dir.join("only-here.txt").exists());
        store.delete_thread(&first.id).unwrap();
        assert!(!first_dir.exists());
        assert!(second_dir.exists());

        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod rename_compat_tests {
    use super::*;

    fn scratch_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("threadknot-home-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A store is a directory with an index in it, which is what these tests
    /// must model. Creating a bare directory instead is what let the empty-app
    /// bug through: the tests agreed with the broken code.
    fn store_at(dir: PathBuf) -> PathBuf {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("projects.json"), r#"{"projects":[],"threads":[]}"#).unwrap();
        dir
    }

    /// The upgrade case that matters: an Armada install has `~/.armada` full of
    /// threads, browser logins and paired phones, and no `~/.threadknot`. It
    /// must keep reading the old directory rather than silently starting empty.
    #[test]
    fn armada_install_keeps_its_existing_store() {
        let home = scratch_home();
        store_at(home.join(".armada"));
        assert_eq!(data_dir_in(&home), home.join(".armada"));
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// A machine that never ran Armada starts life under the new name.
    #[test]
    fn fresh_install_uses_the_new_directory() {
        let home = scratch_home();
        assert_eq!(data_dir_in(&home), home.join(".threadknot"));
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// Once a user migrates by hand, the new directory wins even though the old
    /// one is still sitting there — otherwise the move would appear to do
    /// nothing and they would migrate twice.
    #[test]
    fn a_hand_migrated_store_takes_precedence() {
        let home = scratch_home();
        store_at(home.join(".armada"));
        store_at(home.join(".threadknot"));
        assert_eq!(data_dir_in(&home), home.join(".threadknot"));
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// Regression: the dev launcher wrote its log to `~/.threadknot/` and did
    /// `mkdir -p` on it before the app started. The old rule saw a directory,
    /// called it migrated, and launched Spencer into an empty app while 242
    /// threads sat untouched in `~/.armada`. An empty directory is not a store.
    #[test]
    fn an_empty_new_directory_does_not_strand_the_real_store() {
        let home = scratch_home();
        store_at(home.join(".armada"));
        std::fs::create_dir_all(home.join(".threadknot")).unwrap();
        std::fs::write(home.join(".threadknot/dev-launcher.log"), b"starting\n").unwrap();
        assert_eq!(data_dir_in(&home), home.join(".armada"));
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// The same directory once the app has run against it: server.json and
    /// device.json exist, but no index. Still not a store worth preferring.
    #[test]
    fn a_new_directory_with_no_index_still_falls_back() {
        let home = scratch_home();
        store_at(home.join(".armada"));
        let stray = home.join(".threadknot");
        std::fs::create_dir_all(stray.join("threads")).unwrap();
        std::fs::write(stray.join("server.json"), r#"{"port":42800}"#).unwrap();
        std::fs::write(stray.join("device.json"), r#"{"id":"x"}"#).unwrap();
        assert_eq!(data_dir_in(&home), home.join(".armada"));
        std::fs::remove_dir_all(&home).unwrap();
    }
}

#[cfg(test)]
mod migrate_mesh_tests {
    use super::*;

    /// A pre-mesh projects.json: no `workspaces` key, threads without
    /// `machineId` — exactly what an existing install has on disk.
    const OLD_STORE: &str = r#"{
      "projects": [
        { "id": "proj-b", "name": "Beta", "path": "/tmp/b", "createdAt": "2026-02-01T00:00:00Z" },
        { "id": "proj-a", "name": "Alpha", "path": "/tmp/a", "createdAt": "2026-01-01T00:00:00Z" }
      ],
      "threads": [
        {
          "id": "t1", "projectId": "proj-a", "agent": "claude", "title": "old thread",
          "settings": { "model": "claude-opus-4-8", "access": "full", "mode": "build" },
          "status": "idle", "createdAt": "2026-01-02T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z"
        }
      ]
    }"#;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("threadknot-store-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn migration_stamps_threads_and_wraps_projects() {
        let dir = temp_dir();
        std::fs::write(dir.join("projects.json"), OLD_STORE).unwrap();
        let store = Store::open_at(dir.clone()).unwrap();
        assert_eq!(
            store.thread("t1").unwrap().settings.model,
            CURRENT_CLAUDE_OPUS_MODEL
        );
        assert!(!store.thread("t1").unwrap().settings.wide_context);
        store.migrate_mesh("machine-1").unwrap();

        // Threads are stamped EXPLICITLY (symmetric mesh: no "empty = local").
        assert_eq!(store.thread("t1").unwrap().machine_id, "machine-1");

        // Each project wrapped 1:1, reusing the project id as workspace id.
        let ws = store.list_workspaces();
        assert_eq!(
            ws.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(),
            vec!["proj-b", "proj-a"],
        );
        assert_eq!(ws[0].name, "Beta");
        assert_eq!(
            ws[0].members,
            vec![WorkspaceMember {
                machine_id: "machine-1".into(),
                project_id: "proj-b".into(),
                name: Some("Beta".into()),
                path: Some("/tmp/b".into()),
            }],
        );

        // Migration persisted (a reload sees it) and is idempotent.
        let reloaded = Store::open_at(dir.clone()).unwrap();
        reloaded.migrate_mesh("machine-1").unwrap();
        assert_eq!(reloaded.thread("t1").unwrap().machine_id, "machine-1");
        assert_eq!(reloaded.list_workspaces().len(), 2);

        // New threads stamp the local machine id automatically.
        let t = reloaded
            .create_thread(
                "proj-a".into(),
                Agent::Claude,
                reloaded.thread("t1").unwrap().settings,
            )
            .unwrap();
        assert_eq!(t.machine_id, "machine-1");

        // Rename bumps the LWW clock and persists.
        let before = reloaded.workspace("proj-a").unwrap().updated_at;
        let renamed = reloaded
            .rename_workspace("proj-a", " ServiceStorm ".into())
            .unwrap();
        assert_eq!(renamed.name, "ServiceStorm");
        assert!(renamed.updated_at >= before);
        assert!(reloaded.rename_workspace("proj-a", "  ".into()).is_err());
        assert!(reloaded.rename_workspace("nope", "x".into()).is_err());

        // Deleting a project drops its membership and the now-empty
        // workspace, tombstoning it so peers don't resurrect it at resync.
        let cascade = reloaded.delete_project("proj-b").unwrap();
        assert_eq!(cascade.deleted, vec!["proj-b"]);
        assert!(cascade.updated.is_empty());
        assert!(reloaded.workspace("proj-b").is_none());
        assert_eq!(reloaded.list_workspaces().len(), 1);
        assert_eq!(
            reloaded.workspace_tombstones(),
            vec![("proj-b".to_string(), cascade.deleted_at.clone())],
        );

        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod catalog_sync_tests {
    use super::*;

    fn temp_store() -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("threadknot-store-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        (Store::open_at(dir.clone()).unwrap(), dir)
    }

    fn ws(id: &str, machine: &str, updated_at: &str) -> Workspace {
        Workspace {
            id: id.into(),
            name: format!("ws {id}"),
            image: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: updated_at.into(),
            favorite: None,
            hidden: None,
            members: vec![WorkspaceMember {
                machine_id: machine.into(),
                project_id: format!("{id}-root"),
                name: Some("root".into()),
                path: Some("/tmp/root".into()),
            }],
        }
    }

    #[test]
    fn foreign_workspaces_are_accepted_and_lww_merged() {
        let (store, dir) = temp_store();
        store.migrate_mesh("machine-local").unwrap();

        // A workspace we're not a member of lands in the catalog (that's the
        // whole point of mesh-wide sync)…
        let remote = ws("w1", "machine-remote", "2026-07-01T00:00:00Z");
        assert!(store.upsert_workspace_replica(remote.clone()).unwrap());
        assert_eq!(store.workspace("w1").unwrap().name, "ws w1");

        // …an older copy is ignored, a newer one wins whole-record.
        let mut stale = remote.clone();
        stale.updated_at = "2026-06-01T00:00:00Z".into();
        stale.name = "stale".into();
        assert!(!store.upsert_workspace_replica(stale).unwrap());
        let mut fresh = remote;
        fresh.updated_at = "2026-07-02T00:00:00Z".into();
        fresh.name = "fresh".into();
        assert!(store.upsert_workspace_replica(fresh).unwrap());
        assert_eq!(store.workspace("w1").unwrap().name, "fresh");

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn workspace_image_persists_and_clears() {
        let (store, dir) = temp_store();
        store
            .upsert_workspace_replica(ws("art", "machine-a", "2026-01-01T00:00:00Z"))
            .unwrap();
        let image = "data:image/webp;base64,aW1hZ2U=".to_string();
        let updated = store
            .set_workspace_image("art", Some(image.clone()))
            .unwrap();
        assert_eq!(updated.image, Some(image));
        assert_eq!(store.set_workspace_image("art", None).unwrap().image, None);
        drop(store);
        assert_eq!(Store::open_at(dir.clone()).unwrap().workspace("art").unwrap().image, None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Hiding is a workspace-level flag on purpose: `ws()` builds a workspace
    /// whose only root lives on `machine-remote`, so this also pins down that a
    /// project you have no folder for locally can still be put away from here —
    /// and that the flag rides the whole-record LWW replica to the machines that
    /// do own it.
    #[test]
    fn hiding_a_remote_rooted_workspace_persists_and_replicates() {
        let (store, dir) = temp_store();
        store
            .upsert_workspace_replica(ws("w1", "machine-remote", "2026-07-01T00:00:00Z"))
            .unwrap();

        let hidden = store.set_workspace_hidden("w1", true).unwrap();
        assert_eq!(hidden.hidden, Some(true));
        // Bumping the clock is what makes the peer accept our copy.
        assert!(hidden.updated_at.as_str() > "2026-07-01T00:00:00Z");
        // Nothing about the workspace itself is lost — this is not a delete.
        assert_eq!(hidden.members.len(), 1);

        drop(store);
        let store = Store::open_at(dir.clone()).unwrap();
        assert_eq!(store.workspace("w1").unwrap().hidden, Some(true));

        // Unhiding clears the field entirely so it serializes away again.
        assert_eq!(store.set_workspace_hidden("w1", false).unwrap().hidden, None);

        // A peer that hid it later wins, flag and all.
        let mut from_peer = ws("w1", "machine-remote", "2027-01-01T00:00:00Z");
        from_peer.hidden = Some(true);
        assert!(store.upsert_workspace_replica(from_peer).unwrap());
        assert_eq!(store.workspace("w1").unwrap().hidden, Some(true));

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tombstones_block_stale_records_but_not_newer_edits() {
        let (store, dir) = temp_store();
        store.migrate_mesh("machine-local").unwrap();
        store
            .upsert_workspace_replica(ws("w1", "machine-remote", "2026-07-01T00:00:00Z"))
            .unwrap();

        // Peer says deleted → record goes, tombstone stays.
        assert!(store
            .apply_workspace_tombstone("w1", "2026-07-03T00:00:00Z")
            .unwrap());
        assert!(store.workspace("w1").is_none());

        // A resync pushing the pre-delete copy must NOT resurrect it…
        assert!(!store
            .upsert_workspace_replica(ws("w1", "machine-remote", "2026-07-01T00:00:00Z"))
            .unwrap());
        assert!(store.workspace("w1").is_none());

        // …but an edit made after the delete reopens the grave.
        assert!(store
            .upsert_workspace_replica(ws("w1", "machine-remote", "2026-07-04T00:00:00Z"))
            .unwrap());
        assert!(store.workspace("w1").is_some());
        assert!(store.workspace_tombstones().is_empty());

        // Symmetrically: a delete older than our local edit is refused.
        assert!(!store
            .apply_workspace_tombstone("w1", "2026-07-03T12:00:00Z")
            .unwrap());
        assert!(store.workspace("w1").is_some());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn removing_a_peer_purges_only_its_exclusive_workspaces() {
        let (store, dir) = temp_store();
        store.migrate_mesh("machine-local").unwrap();
        store
            .upsert_workspace_replica(ws("only-theirs", "machine-remote", "2026-07-01T00:00:00Z"))
            .unwrap();
        let mut shared = ws("shared", "machine-remote", "2026-07-01T00:00:00Z");
        shared.members.push(WorkspaceMember {
            machine_id: "machine-local".into(),
            project_id: "local-root".into(),
            name: None,
            path: None,
        });
        store.upsert_workspace_replica(shared).unwrap();

        assert!(store.purge_peer_workspaces("machine-remote").unwrap());
        assert!(store.workspace("only-theirs").is_none());
        assert!(store.workspace("shared").is_some());
        // No tombstone: re-pairing must bring their catalog straight back.
        assert!(store.workspace_tombstones().is_empty());

        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod archive_path_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_store() -> Store {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("threadknot-store-test-{uniq}"));
        Store::open_at(dir).expect("open temp store")
    }

    #[test]
    fn archive_path_accepts_uuid_ids() {
        let store = temp_store();
        let id = uuid::Uuid::new_v4().to_string();
        let path = store.archive_path(&id).expect("uuid id accepted");
        assert!(path.starts_with(store.archive_dir()));
        assert_eq!(path.file_name().unwrap(), format!("{id}.json").as_str());
    }

    #[test]
    fn archive_path_rejects_traversal_ids() {
        let store = temp_store();
        // A leaked LAN token must not let a client escape the archive dir and
        // delete e.g. ~/.threadknot/projects.json via archiveId "../projects".
        for bad in ["../projects", "..", "a/b", "a\\b", "", "foo/../bar"] {
            assert!(
                store.archive_path(bad).is_err(),
                "archive_path should reject {bad:?}"
            );
        }
    }
}

#[cfg(test)]
mod thread_search_tests {
    use super::*;

    fn temp_store() -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "threadknot-thread-search-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (Store::open_at(dir.clone()).unwrap(), dir)
    }

    fn settings() -> ThreadSettings {
        ThreadSettings {
            model: "test-model".into(),
            effort: None,
            wide_context: false,
            claude_chrome: false,
            access: Access::Full,
            mode: Mode::Build,
            browser_profile_id: None,
        }
    }

    #[test]
    fn searches_visible_transcript_text_case_insensitively() {
        let (store, dir) = temp_store();
        let project = store
            .create_project(dir.clone().to_string_lossy().into(), None)
            .unwrap();
        let first = store
            .create_thread(project.id.clone(), Agent::Claude, settings())
            .unwrap();
        let second = store
            .create_thread(project.id, Agent::Codex, settings())
            .unwrap();
        store
            .append_event(
                &first.id,
                None,
                &AgentEvent::UserMessage {
                    text: "Please remember the Blue Narwhal".into(),
                    attachments: Vec::new(),
                    mid_turn: false,
                    injected: false,
                },
            )
            .unwrap();
        store
            .append_event(
                &second.id,
                Some("codex"),
                &AgentEvent::ToolEnd {
                    call_id: "call-1".into(),
                    name: "search".into(),
                    output: Some("Found the copper lighthouse".into()),
                    is_error: false,
                    truncated: false,
                },
            )
            .unwrap();

        let ids = vec![first.id.clone(), second.id.clone()];
        assert_eq!(
            store.search_thread_content(&ids, "blue NARWHAL"),
            vec![first.id]
        );
        assert_eq!(
            store.search_thread_content(&ids, "copper lighthouse"),
            vec![second.id]
        );
        // Raw JSON/event metadata is deliberately not searchable content.
        assert!(store.search_thread_content(&ids, "tool_end").is_empty());
        // Unknown ids never become filesystem paths.
        assert!(store
            .search_thread_content(&["../../projects".into()], "projects")
            .is_empty());

        std::fs::remove_dir_all(dir).unwrap();
    }
}

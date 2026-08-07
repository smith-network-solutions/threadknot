//! The Library: **skills** and **MCP servers** the user installs once and every
//! driven agent picks up.
//!
//! Two halves, deliberately kept in one module because the UI presents them as
//! one shelf:
//!
//! * **Skills** are plain `SKILL.md` folders. Every CLI Threadknot drives already
//!   discovers them from a well-known directory on its own — Threadknot does not
//!   inject them, it just *manages the folders*:
//!   `~/.claude/skills`, `~/.codex/skills`, `~/.kimi-code/skills`. Installing
//!   means writing the same folder into each selected agent's directory;
//!   removing means deleting it. That is the whole mechanism, and it is why a
//!   skill installed here works in a plain terminal `claude` session too.
//! * **MCP servers** are the opposite: no CLI-shared location exists, so Threadknot
//!   keeps its own registry (`mcp-servers.json`) and **injects** every enabled
//!   server into each driver at spawn — `--mcp-config` for Claude, repeated
//!   `-c mcp_servers.*` for Codex, the ACP `mcpServers` array for Kimi. The
//!   registry is the single source of truth; the drivers never read config
//!   files we did not write.
//!
//! ## Trust
//!
//! An MCP server is code the agent can call, and often a credential it can
//! spend — a stdio server is a process launched with Threadknot's privileges.
//! Installing one is therefore an authority change, gated to the machine's
//! master token in `server.rs` exactly like a signed-in browser profile. The
//! registry stores tokens in plaintext under `~/.threadknot` (0700), matching where
//! `server.json` already keeps the master token.
//!
//! ## Licensing
//!
//! Catalog skills are fetched from their upstream repository at install time
//! rather than vendored into Threadknot, and the four Anthropic document skills
//! (`docx`, `pdf`, `pptx`, `xlsx`) are refused outright: their license forbids
//! retaining copies outside Anthropic's own services, so Threadknot must not be the
//! thing that copies them. See [`LICENSE_BLOCKED`].

use crate::protocol::{new_id, now_iso, Agent};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Upstream `repo/path` pairs Threadknot refuses to copy. Anthropic's document
/// skills are source-available, not open source: the license prohibits
/// retaining copies outside their services, so we point the user at the
/// official installer instead of becoming the copier.
pub const LICENSE_BLOCKED: &[(&str, &str)] = &[
    ("anthropics/skills", "skills/docx"),
    ("anthropics/skills", "skills/pdf"),
    ("anthropics/skills", "skills/pptx"),
    ("anthropics/skills", "skills/xlsx"),
];

/// The tool namespace Threadknot's own browser server occupies. A library server
/// may not take it — the agent would lose the browser.
pub const RESERVED_MCP_NAME: &str = "threadknot-browser";

/// Ceilings for a catalog/URL install. A skill is documentation plus a few
/// scripts; anything past this is a repository, not a skill.
const MAX_SKILL_FILES: usize = 300;
const MAX_SKILL_BYTES: u64 = 8 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Skills
// ---------------------------------------------------------------------------

/// An agent whose skills directory Threadknot manages.
///
/// Not every [`Agent`] appears here. Hermes gateways serve their own skills
/// remotely (`/v1/skills`), and Claudex runs the Claude harness against a
/// private `CLAUDE_CONFIG_DIR`, so neither has a local folder we own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillTarget {
    Claude,
    Codex,
    Kimi,
}

impl SkillTarget {
    pub const ALL: [SkillTarget; 3] = [SkillTarget::Claude, SkillTarget::Codex, SkillTarget::Kimi];

    /// Where this CLI auto-discovers user-level skills. These paths are the
    /// CLIs' own conventions, not Threadknot's: `claude` reads them because the
    /// driver passes `--setting-sources user,project,local`, `codex` and
    /// `kimi` scan them unconditionally.
    pub fn skills_dir(&self) -> Option<PathBuf> {
        let home = dirs::home_dir()?;
        Some(match self {
            SkillTarget::Claude => home.join(".claude").join("skills"),
            SkillTarget::Codex => home.join(".codex").join("skills"),
            SkillTarget::Kimi => home.join(".kimi-code").join("skills"),
        })
    }

    pub fn label(&self) -> &'static str {
        match self {
            SkillTarget::Claude => "Claude Code",
            SkillTarget::Codex => "Codex",
            SkillTarget::Kimi => "Kimi Code",
        }
    }
}

/// Where an installed skill came from, which decides whether Threadknot may delete
/// it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SkillOrigin {
    /// Written by Threadknot from the catalog or a repo URL. Safe to remove.
    Library,
    /// A folder the user (or another tool) put in the skills directory. Threadknot
    /// will still remove it on request, but says so in the UI.
    Local,
    /// Supplied by an installed Claude Code plugin. Read-only here — the
    /// owning marketplace manages its lifecycle, so removal routes to
    /// `claude plugin`, not to us.
    Plugin { plugin: String },
}

/// One skill as it exists on this machine, folded across every agent that has
/// it. The same folder installed for Claude and Codex is ONE row with two
/// badges, which is the whole point of the viewer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledSkill {
    /// Directory name — the stable id, and what removal takes.
    pub id: String,
    /// `name:` from the frontmatter, falling back to the directory name.
    pub title: String,
    pub description: String,
    /// Agents whose skills directory currently holds this folder.
    pub targets: Vec<SkillTarget>,
    pub origin: SkillOrigin,
    /// Catalog entry id, when Threadknot installed it from the catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_id: Option<String>,
    /// `owner/repo` + subpath this came from, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_at: Option<String>,
    pub files: usize,
    pub bytes: u64,
    /// Absolute path of one copy, for the "reveal" affordance.
    pub path: String,
    /// False for plugin-provided skills.
    pub removable: bool,
    /// A Claude Code plugin ALSO ships this skill under the same name. The row
    /// still describes the folder Threadknot manages (which stays removable), but
    /// the user needs to know a second copy exists — otherwise removing theirs
    /// looks like it failed when Claude keeps finding the plugin's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub also_from_plugin: Option<String>,
}

/// Provenance breadcrumb Threadknot drops inside every skill folder it writes, so a
/// later listing can tell a catalog install from something the user hand-made.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillMarker {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    catalog_id: Option<String>,
    source: String,
    installed_at: String,
}

const MARKER_FILE: &str = ".threadknot-library.json";

/// The marker Armada wrote before the rename. Only ever read, never written:
/// without this a skill installed by the old build would look like a folder
/// some other tool owns, and the Library would refuse to delete its own work.
const LEGACY_MARKER_FILE: &str = ".armada-library.json";

fn is_marker(name: &str) -> bool {
    name == MARKER_FILE || name == LEGACY_MARKER_FILE
}

/// Minimal YAML frontmatter reader: enough for `name:` and `description:`,
/// including the folded (`>`/`|`) and plain multi-line continuation forms that
/// real skills use. Deliberately not a YAML parser — a skill's frontmatter is a
/// flat string map by spec, and pulling in a YAML dependency to read two keys
/// would be the wrong trade.
fn parse_frontmatter(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let body = match text.strip_prefix("---") {
        Some(rest) => rest.trim_start_matches(['\r', '\n']),
        None => return out,
    };
    // Frontmatter ends at the first `---` sitting alone on a line.
    let end = body
        .lines()
        .scan(0usize, |offset, line| {
            let start = *offset;
            *offset += line.len() + 1;
            Some((start, line))
        })
        .find(|(_, line)| line.trim_end() == "---")
        .map(|(start, _)| start);
    let block = match end {
        Some(e) => &body[..e],
        None => return out,
    };

    let lines: Vec<&str> = block.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || line.starts_with(' ') {
            continue;
        }
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let mut value = rest.trim().to_string();
        // Block scalars and empty values continue on the following indented
        // lines; join them into one logical string.
        if value.is_empty() || value == ">" || value == "|" || value == ">-" || value == "|-" {
            let mut parts: Vec<String> = Vec::new();
            while i < lines.len() && (lines[i].starts_with(' ') || lines[i].trim().is_empty()) {
                let cont = lines[i].trim();
                i += 1;
                if !cont.is_empty() {
                    parts.push(cont.to_string());
                }
            }
            value = parts.join(" ");
        }
        let value = value
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .to_string();
        out.insert(key, value);
    }
    out
}

/// Recursive size/count of a skill folder, ignoring Threadknot's own marker.
fn measure(dir: &Path) -> (usize, u64) {
    let mut files = 0;
    let mut bytes = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (files, bytes);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            let (f, b) = measure(&path);
            files += f;
            bytes += b;
        } else if !path.file_name().and_then(|n| n.to_str()).is_some_and(is_marker) {
            files += 1;
            bytes += meta.len();
        }
    }
    (files, bytes)
}

/// Read one skill folder. `None` when it has no `SKILL.md` — the directory is
/// then something else that happens to live there, and we leave it alone.
fn read_skill_dir(dir: &Path) -> Option<(String, String, String, usize, u64, Option<SkillMarker>)> {
    let id = dir.file_name()?.to_str()?.to_string();
    if id.starts_with('.') {
        return None;
    }
    let manifest = dir.join("SKILL.md");
    let text = std::fs::read_to_string(&manifest).ok()?;
    let front = parse_frontmatter(&text);
    let title = front.get("name").cloned().filter(|s| !s.is_empty()).unwrap_or_else(|| id.clone());
    let description = front.get("description").cloned().unwrap_or_default();
    let (files, bytes) = measure(dir);
    let marker = std::fs::read_to_string(dir.join(MARKER_FILE))
        .or_else(|_| std::fs::read_to_string(dir.join(LEGACY_MARKER_FILE)))
        .ok()
        .and_then(|raw| serde_json::from_str::<SkillMarker>(&raw).ok());
    Some((id, title, description, files, bytes, marker))
}

/// Every Claude Code plugin skill on this machine, as read-only rows. Layout is
/// `~/.claude/plugins/cache/<marketplace>/<plugin>/<version>/skills/<name>`.
/// Included because the viewer's promise is "everything installed", and a user
/// who cannot find `frontend-design` in the list will assume Threadknot is lying
/// rather than that it came from a plugin.
fn scan_plugin_skills(out: &mut Vec<InstalledSkill>) {
    let Some(home) = dirs::home_dir() else { return };
    let cache = home.join(".claude").join("plugins").join("cache");
    let Ok(marketplaces) = std::fs::read_dir(&cache) else {
        return;
    };
    for marketplace in marketplaces.flatten() {
        let Ok(plugins) = std::fs::read_dir(marketplace.path()) else {
            continue;
        };
        for plugin in plugins.flatten() {
            let plugin_name = plugin.file_name().to_string_lossy().to_string();
            let Ok(versions) = std::fs::read_dir(plugin.path()) else {
                continue;
            };
            for version in versions.flatten() {
                let Ok(skills) = std::fs::read_dir(version.path().join("skills")) else {
                    continue;
                };
                for skill in skills.flatten() {
                    let path = skill.path();
                    let Some((id, title, description, files, bytes, _)) = read_skill_dir(&path)
                    else {
                        continue;
                    };
                    // A plugin may ship several versions side by side, and the
                    // user may also have their own copy in a skills directory.
                    // Either way the skill gets ONE row; note the plugin on the
                    // existing one rather than adding a duplicate.
                    if let Some(existing) = out.iter_mut().find(|s| s.id == id) {
                        existing
                            .also_from_plugin
                            .get_or_insert_with(|| plugin_name.clone());
                        continue;
                    }
                    out.push(InstalledSkill {
                        id,
                        title,
                        description,
                        targets: vec![SkillTarget::Claude],
                        origin: SkillOrigin::Plugin {
                            plugin: plugin_name.clone(),
                        },
                        catalog_id: None,
                        source: None,
                        installed_at: None,
                        files,
                        bytes,
                        path: path.to_string_lossy().to_string(),
                        removable: false,
                        also_from_plugin: None,
                    });
                }
            }
        }
    }
}

/// Every skill on this machine, folded across agents and sorted by title.
pub fn list_skills() -> Vec<InstalledSkill> {
    let mut folded: Vec<InstalledSkill> = Vec::new();
    for target in SkillTarget::ALL {
        let Some(dir) = target.skills_dir() else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some((id, title, description, files, bytes, marker)) = read_skill_dir(&path) else {
                continue;
            };
            if let Some(existing) = folded.iter_mut().find(|s| s.id == id) {
                existing.targets.push(target);
                continue;
            }
            folded.push(InstalledSkill {
                id,
                title,
                description,
                targets: vec![target],
                origin: if marker.is_some() {
                    SkillOrigin::Library
                } else {
                    SkillOrigin::Local
                },
                catalog_id: marker.as_ref().and_then(|m| m.catalog_id.clone()),
                source: marker.as_ref().map(|m| m.source.clone()),
                installed_at: marker.as_ref().map(|m| m.installed_at.clone()),
                files,
                bytes,
                path: path.to_string_lossy().to_string(),
                removable: true,
                also_from_plugin: None,
            });
        }
    }
    scan_plugin_skills(&mut folded);
    for skill in folded.iter_mut() {
        skill.targets.sort();
        skill.targets.dedup();
    }
    folded.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    folded
}

/// Install a skill Threadknot ships inside its own binary (see `bundled.rs`).
///
/// Same publishing path as a downloaded skill — staged, then copied into each
/// target — but with no network, so it works offline and cannot half-install
/// because GitHub was slow.
pub fn install_bundled_skill(
    skill: &crate::bundled::BundledSkill,
    targets: &[SkillTarget],
) -> Result<InstalledSkill> {
    anyhow::ensure!(!targets.is_empty(), "pick at least one agent");
    let staging = std::env::temp_dir().join(format!("threadknot-skill-{}", new_id()));
    std::fs::create_dir_all(&staging).context("create staging dir")?;
    let cleanup = StagingDir(staging.clone());

    for file in skill.files {
        let dest = safe_join(&staging, file.path)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, file.contents)
            .with_context(|| format!("write {}", file.path))?;
        if file.executable {
            set_executable(&dest)?;
        }
    }

    let marker = SkillMarker {
        catalog_id: Some(skill.id.to_string()),
        source: format!("bundled with Threadknot {}", env!("CARGO_PKG_VERSION")),
        installed_at: now_iso(),
    };
    std::fs::write(
        staging.join(MARKER_FILE),
        serde_json::to_string_pretty(&marker)?,
    )?;

    publish(&staging, skill.id, targets)?;
    drop(cleanup);

    list_skills()
        .into_iter()
        .find(|s| s.id == skill.id)
        .context("installed skill vanished before it could be listed")
}

/// Give a file the owner-execute bit. A script installed without it fails the
/// first time an agent runs it, with an error that looks like the skill is
/// broken rather than merely unexecutable.
fn set_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        let mode = perms.mode();
        perms.set_mode(mode | 0o111);
        std::fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    let _ = path; // Windows has no execute bit; the shebang is honoured by uv.
    Ok(())
}

/// Copy a fully staged skill folder into each target's skills directory,
/// replacing any existing copy.
fn publish(staging: &Path, id: &str, targets: &[SkillTarget]) -> Result<()> {
    for target in targets {
        let Some(dir) = target.skills_dir() else {
            bail!("no home directory — cannot install skills")
        };
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let dest = dir.join(id);
        if dest.exists() {
            std::fs::remove_dir_all(&dest)
                .with_context(|| format!("replace existing {}", dest.display()))?;
        }
        copy_tree(staging, &dest).with_context(|| format!("install into {}", dest.display()))?;
    }
    Ok(())
}

/// Copy a skill that is already installed for one agent into others.
///
/// The reason this exists rather than "just install it again from upstream":
/// most skills on a real machine were never downloaded. They were hand-written,
/// or came from a plugin, or predate the Library entirely — there is no URL to
/// re-fetch. Spreading the folder that is already there is the only operation
/// that works for all of them, and it is what "make my skills work in Codex
/// too" actually means.
pub fn copy_skill(id: &str, targets: &[SkillTarget]) -> Result<usize> {
    anyhow::ensure!(
        !id.is_empty() && !id.contains('/') && !id.contains('\\') && id != "." && id != "..",
        "invalid skill id"
    );
    anyhow::ensure!(!targets.is_empty(), "pick at least one agent");

    // Any existing copy is a valid donor; they are the same folder by
    // construction, and a plugin-provided one works just as well as a managed
    // one.
    let source = SkillTarget::ALL
        .iter()
        .filter_map(|t| t.skills_dir())
        .map(|dir| dir.join(id))
        .find(|path| path.join("SKILL.md").is_file())
        .or_else(|| {
            list_skills()
                .into_iter()
                .find(|s| s.id == id)
                .map(|s| PathBuf::from(s.path))
                .filter(|path| path.join("SKILL.md").is_file())
        })
        .ok_or_else(|| anyhow::anyhow!("`{id}` is not installed anywhere to copy from"))?;

    let mut copied = 0;
    for target in targets {
        let Some(dir) = target.skills_dir() else {
            continue;
        };
        let dest = dir.join(id);
        if dest == source {
            continue;
        }
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        if dest.exists() {
            std::fs::remove_dir_all(&dest)
                .with_context(|| format!("replace existing {}", dest.display()))?;
        }
        copy_tree(&source, &dest).with_context(|| format!("copy into {}", dest.display()))?;
        copied += 1;
    }
    anyhow::ensure!(copied > 0, "it is already installed for those agents");
    Ok(copied)
}

/// Delete a skill folder from each named agent's directory.
///
/// The id is a single directory name, resolved against the target directory and
/// then checked to still be a direct child of it — a skill id may never escape
/// into the rest of the home directory.
pub fn remove_skill(id: &str, targets: &[SkillTarget]) -> Result<usize> {
    anyhow::ensure!(
        !id.is_empty() && !id.contains('/') && !id.contains('\\') && id != "." && id != "..",
        "invalid skill id"
    );
    let mut removed = 0;
    for target in targets {
        let Some(dir) = target.skills_dir() else {
            continue;
        };
        let path = dir.join(id);
        if !path.is_dir() {
            continue;
        }
        anyhow::ensure!(
            path.parent() == Some(dir.as_path()),
            "skill path escaped {}",
            dir.display()
        );
        std::fs::remove_dir_all(&path)
            .with_context(|| format!("remove {}", path.display()))?;
        removed += 1;
    }
    anyhow::ensure!(removed > 0, "that skill is not installed for those agents");
    Ok(removed)
}

// ---------------------------------------------------------------------------
// Fetching a skill from GitHub
// ---------------------------------------------------------------------------

/// A resolved "where do the files come from" for an install.
#[derive(Debug, Clone)]
pub struct SkillSource {
    /// `owner/repo`
    pub repo: String,
    pub git_ref: String,
    /// Subdirectory inside the repo, no leading or trailing slash. Empty means
    /// the repository root is itself the skill.
    pub path: String,
}

impl SkillSource {
    pub fn describe(&self) -> String {
        if self.path.is_empty() {
            format!("{}@{}", self.repo, self.git_ref)
        } else {
            format!("{}/{}@{}", self.repo, self.path, self.git_ref)
        }
    }

    /// Accepts what a user actually has in their clipboard: a GitHub tree URL,
    /// a plain repo URL, or the bare `owner/repo/sub/path` shorthand.
    pub fn parse(input: &str) -> Result<Self> {
        let raw = input.trim().trim_end_matches('/');
        anyhow::ensure!(!raw.is_empty(), "paste a GitHub URL or owner/repo path");
        let rest = raw
            .strip_prefix("https://github.com/")
            .or_else(|| raw.strip_prefix("http://github.com/"))
            .or_else(|| raw.strip_prefix("github.com/"))
            .unwrap_or(raw)
            .trim_end_matches(".git");
        let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
        anyhow::ensure!(
            parts.len() >= 2,
            "expected owner/repo — for example anthropics/skills/skills/skill-creator"
        );
        let repo = format!("{}/{}", parts[0], parts[1]);
        // `.../tree/<ref>/<path>` is what GitHub's own "copy link" produces.
        if parts.len() >= 4 && (parts[2] == "tree" || parts[2] == "blob") {
            return Ok(Self {
                repo,
                git_ref: parts[3].to_string(),
                path: parts[4..].join("/"),
            });
        }
        Ok(Self {
            repo,
            git_ref: "HEAD".into(),
            path: parts[2..].join("/"),
        })
    }
}

#[derive(Deserialize)]
struct GhTree {
    #[serde(default)]
    tree: Vec<GhEntry>,
    #[serde(default)]
    truncated: bool,
}

#[derive(Deserialize)]
struct GhEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    size: u64,
    /// Git file mode: "100755" for an executable blob. Preserved on install so
    /// a skill's scripts stay runnable.
    #[serde(default)]
    mode: String,
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        // GitHub rejects API calls without one.
        .user_agent(concat!("threadknot/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .expect("build http client")
}

/// Download a skill folder from a public GitHub repo and install it into each
/// target's skills directory.
///
/// One tree call plus one raw fetch per file: no git binary, no archive
/// dependency, and nothing executes during install.
pub async fn install_skill(
    source: &SkillSource,
    targets: &[SkillTarget],
    catalog_id: Option<String>,
    rename_to: Option<String>,
) -> Result<InstalledSkill> {
    anyhow::ensure!(!targets.is_empty(), "pick at least one agent");
    for (repo, path) in LICENSE_BLOCKED {
        if source.repo.eq_ignore_ascii_case(repo) && source.path.trim_matches('/') == *path {
            bail!(
                "Anthropic's document skills are source-available, not open source — their \
                 license forbids keeping copies outside Anthropic's own services, so Threadknot \
                 will not copy them. Install them with Claude Code's own plugin marketplace \
                 instead: `claude plugin install document-skills@anthropic-agent-skills`."
            );
        }
    }

    let client = http();
    let tree_url = format!(
        "https://api.github.com/repos/{}/git/trees/{}?recursive=1",
        source.repo, source.git_ref
    );
    let response = client
        .get(&tree_url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .with_context(|| format!("fetch {tree_url}"))?;
    anyhow::ensure!(
        response.status().is_success(),
        "GitHub returned {} for {}/{} — is the repository public and the branch right?",
        response.status(),
        source.repo,
        source.git_ref
    );
    let tree: GhTree = response.json().await.context("parse GitHub tree")?;

    let prefix = if source.path.is_empty() {
        String::new()
    } else {
        format!("{}/", source.path.trim_matches('/'))
    };
    let files: Vec<&GhEntry> = tree
        .tree
        .iter()
        .filter(|e| e.kind == "blob" && e.path.starts_with(&prefix))
        .collect();
    if files.is_empty() {
        if tree.truncated {
            bail!("that repository is too large for Threadknot to index — point at the skill folder directly");
        }
        bail!("no files found at {}", source.describe());
    }
    anyhow::ensure!(
        files.iter().any(|e| e.path[prefix.len()..].eq_ignore_ascii_case("SKILL.md")),
        "{} has no SKILL.md at its top level — that path is not a skill",
        source.describe()
    );
    anyhow::ensure!(
        files.len() <= MAX_SKILL_FILES,
        "that folder has {} files — more than a skill should carry",
        files.len()
    );
    let total: u64 = files.iter().map(|e| e.size).sum();
    anyhow::ensure!(
        total <= MAX_SKILL_BYTES,
        "that folder is {:.1} MB — more than a skill should carry",
        total as f64 / (1024.0 * 1024.0)
    );

    // The folder name the CLIs will see. Default to the last path segment.
    let id = match rename_to {
        Some(name) => name,
        None => source
            .path
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| source.repo.rsplit('/').next().unwrap_or("skill"))
            .to_string(),
    };
    let id = sanitize_dir_name(&id)?;

    // Stage the whole download in one temp directory, then publish it into each
    // agent's folder. A half-downloaded skill never becomes visible to a CLI.
    let staging = std::env::temp_dir().join(format!("threadknot-skill-{}", new_id()));
    std::fs::create_dir_all(&staging).context("create staging dir")?;
    let cleanup = StagingDir(staging.clone());

    for entry in &files {
        let relative = &entry.path[prefix.len()..];
        let dest = safe_join(&staging, relative)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).context("create skill subdirectory")?;
        }
        let raw = format!(
            "https://raw.githubusercontent.com/{}/{}/{}",
            source.repo, source.git_ref, entry.path
        );
        let bytes = client
            .get(&raw)
            .send()
            .await
            .with_context(|| format!("download {relative}"))?
            .error_for_status()
            .with_context(|| format!("download {relative}"))?
            .bytes()
            .await
            .with_context(|| format!("read {relative}"))?;
        std::fs::write(&dest, &bytes).with_context(|| format!("write {relative}"))?;
        if entry.mode.ends_with("755") {
            set_executable(&dest)?;
        }
    }

    let marker = SkillMarker {
        catalog_id: catalog_id.clone(),
        source: source.describe(),
        installed_at: now_iso(),
    };
    std::fs::write(
        staging.join(MARKER_FILE),
        serde_json::to_string_pretty(&marker)?,
    )
    .context("write provenance marker")?;

    publish(&staging, &id, targets)?;
    drop(cleanup);

    list_skills()
        .into_iter()
        .find(|s| s.id == id)
        .context("installed skill vanished before it could be listed")
}

/// Deletes its directory on drop so an aborted install leaves no debris.
struct StagingDir(PathBuf);

impl Drop for StagingDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A directory name safe to create inside a skills folder.
fn sanitize_dir_name(name: &str) -> Result<String> {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(['-', '.']).to_string();
    anyhow::ensure!(!cleaned.is_empty(), "that skill has no usable folder name");
    Ok(cleaned)
}

/// Join a repo-relative path under `root`, refusing anything that would land
/// outside it. The tree API returns attacker-influenceable strings, so this is
/// the boundary that keeps an install inside its own folder.
fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    let mut out = root.to_path_buf();
    for part in relative.split('/') {
        anyhow::ensure!(
            !part.is_empty() && part != "." && part != ".." && !part.contains('\\'),
            "refusing suspicious path in skill: {relative}"
        );
        out.push(part);
    }
    anyhow::ensure!(out.starts_with(root), "refusing path outside skill: {relative}");
    Ok(out)
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// MCP servers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum McpTransport {
    /// A local process spoken to over stdio. This is code running with Threadknot's
    /// privileges — the reason the whole family is master-gated.
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    /// A remote streamable-HTTP endpoint.
    Http {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    pub id: String,
    /// The tool namespace agents see (`mcp__<name>__<tool>`). Constrained to
    /// what all three CLIs accept unquoted in their own config formats.
    pub name: String,
    /// Human label for the list; falls back to `name` when empty.
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
    pub transport: McpTransport,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Which drivers receive this server. Empty means every agent Threadknot can
    /// inject into — the common case, and what the catalog installs.
    #[serde(default)]
    pub agents: Vec<Agent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_id: Option<String>,
    #[serde(default)]
    pub created_at: String,
}

fn yes() -> bool {
    true
}

impl McpServer {
    /// Whether this server should be injected into a driver of `agent`.
    pub fn applies_to(&self, agent: Agent) -> bool {
        if !self.enabled {
            return false;
        }
        // Claudex is the Claude harness on another model; a server chosen for
        // Claude belongs there too, and it has no separate row in the UI.
        let effective = if agent == Agent::Claudex {
            Agent::Claude
        } else {
            agent
        };
        self.agents.is_empty() || self.agents.contains(&effective)
    }

    pub fn display(&self) -> &str {
        if self.label.is_empty() {
            &self.name
        } else {
            &self.label
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct McpFile {
    #[serde(default)]
    servers: Vec<McpServer>,
}

pub struct McpLibrary {
    path: PathBuf,
    servers: Mutex<Vec<McpServer>>,
}

impl McpLibrary {
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("mcp-servers.json");
        let servers = if path.exists() {
            serde_json::from_str::<McpFile>(&std::fs::read_to_string(&path)?)
                .context("parse mcp-servers.json")?
                .servers
        } else {
            Vec::new()
        };
        Ok(Self {
            path,
            servers: Mutex::new(servers),
        })
    }

    fn flush(&self, servers: &[McpServer]) -> Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_string_pretty(&McpFile {
                servers: servers.to_vec(),
            })?,
        )?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn list(&self) -> Vec<McpServer> {
        self.servers.lock().unwrap().clone()
    }

    /// Enabled servers that apply to `agent`, in registry order. This is what
    /// the drivers call at spawn.
    pub fn for_agent(&self, agent: Agent) -> Vec<McpServer> {
        self.servers
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.applies_to(agent))
            .cloned()
            .collect()
    }

    /// Create or replace a server (matched by `id`; an empty id mints one).
    pub fn save(&self, mut server: McpServer) -> Result<McpServer> {
        server.name = server.name.trim().to_string();
        server.label = server.label.trim().to_string();
        anyhow::ensure!(!server.name.is_empty(), "the server needs a name");
        anyhow::ensure!(
            server
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "the name may use letters, numbers, dashes and underscores only — it becomes the agent's tool prefix"
        );
        anyhow::ensure!(
            server.name != RESERVED_MCP_NAME,
            "`{RESERVED_MCP_NAME}` is Threadknot's own browser server"
        );
        match &server.transport {
            McpTransport::Stdio { command, .. } => {
                anyhow::ensure!(!command.trim().is_empty(), "a local server needs a command");
            }
            McpTransport::Http { url, .. } => {
                let parsed = url::Url::parse(url.trim())
                    .map_err(|_| anyhow::anyhow!("that URL is not valid"))?;
                anyhow::ensure!(
                    matches!(parsed.scheme(), "http" | "https"),
                    "a remote server must be http or https"
                );
            }
        }

        let mut servers = self.servers.lock().unwrap();
        anyhow::ensure!(
            !servers
                .iter()
                .any(|s| s.name == server.name && s.id != server.id),
            "another server already uses the name `{}`",
            server.name
        );
        if server.id.is_empty() {
            server.id = new_id();
            server.created_at = now_iso();
            servers.push(server.clone());
        } else {
            match servers.iter_mut().find(|s| s.id == server.id) {
                Some(existing) => {
                    server.created_at = existing.created_at.clone();
                    *existing = server.clone();
                }
                None => {
                    server.created_at = now_iso();
                    servers.push(server.clone());
                }
            }
        }
        self.flush(&servers)?;
        Ok(server)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let mut servers = self.servers.lock().unwrap();
        let before = servers.len();
        servers.retain(|s| s.id != id);
        anyhow::ensure!(servers.len() < before, "unknown MCP server");
        self.flush(&servers)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Driver wire formats
// ---------------------------------------------------------------------------

/// Claude Code `--mcp-config` entry for one server.
pub fn claude_entry(server: &McpServer) -> serde_json::Value {
    match &server.transport {
        McpTransport::Stdio { command, args, env } => serde_json::json!({
            "type": "stdio",
            "command": command,
            "args": args,
            "env": env,
        }),
        McpTransport::Http { url, headers } => serde_json::json!({
            "type": "http",
            "url": url,
            "headers": headers,
        }),
    }
}

/// Kimi's ACP `session/new` `mcpServers` entry. ACP carries environment and
/// headers as name/value pair arrays rather than objects.
pub fn kimi_entry(server: &McpServer) -> serde_json::Value {
    match &server.transport {
        McpTransport::Stdio { command, args, env } => serde_json::json!({
            "name": server.name,
            "command": command,
            "args": args,
            "env": env.iter().map(|(name, value)| serde_json::json!({ "name": name, "value": value }))
                .collect::<Vec<_>>(),
        }),
        McpTransport::Http { url, headers } => serde_json::json!({
            "type": "http",
            "name": server.name,
            "url": url,
            "headers": headers.iter().map(|(name, value)| serde_json::json!({ "name": name, "value": value }))
                .collect::<Vec<_>>(),
        }),
    }
}

/// Codex `-c` overrides for one server, plus the environment the child process
/// needs. Codex parses each value as TOML, so strings are JSON-quoted (a valid
/// TOML basic string) and lists use TOML array syntax.
///
/// Codex's HTTP transport takes a bearer token from an environment variable
/// rather than arbitrary headers, so an `Authorization: Bearer …` header is
/// translated into that; any other header cannot be expressed and is reported
/// by [`codex_unsupported_headers`] so the UI can say so instead of silently
/// dropping it.
pub fn codex_overrides(server: &McpServer) -> (Vec<String>, Vec<(String, String)>) {
    let name = &server.name;
    let mut args = Vec::new();
    let mut env = Vec::new();
    let quote = |value: &str| serde_json::Value::String(value.to_string()).to_string();
    match &server.transport {
        McpTransport::Stdio {
            command,
            args: cmd_args,
            env: cmd_env,
        } => {
            args.push(format!("mcp_servers.{name}.command={}", quote(command)));
            if !cmd_args.is_empty() {
                let list = cmd_args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(", ");
                args.push(format!("mcp_servers.{name}.args=[{list}]"));
            }
            for (key, value) in cmd_env {
                args.push(format!("mcp_servers.{name}.env.{key}={}", quote(value)));
            }
        }
        McpTransport::Http { url, headers } => {
            args.push(format!("mcp_servers.{name}.url={}", quote(url)));
            if let Some(token) = bearer_token(headers) {
                // One env var per server so two servers never share a token.
                let var = format!("THREADKNOT_MCP_TOKEN_{}", env_suffix(name));
                args.push(format!(
                    "mcp_servers.{name}.bearer_token_env_var={}",
                    quote(&var)
                ));
                env.push((var, token));
            }
        }
    }
    (args, env)
}

/// Headers Codex cannot carry for this server (everything but `Authorization:
/// Bearer …`). Empty for stdio servers.
pub fn codex_unsupported_headers(server: &McpServer) -> Vec<String> {
    match &server.transport {
        McpTransport::Http { headers, .. } => headers
            .keys()
            .filter(|k| !k.eq_ignore_ascii_case("authorization"))
            .cloned()
            .collect(),
        McpTransport::Stdio { .. } => Vec::new(),
    }
}

fn bearer_token(headers: &BTreeMap<String, String>) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .map(|(_, v)| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
                .unwrap_or(v)
                .to_string()
        })
}

fn env_suffix(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A skill Armada installed carries the old marker. If we stopped reading
    /// it, the Library would classify its own past work as a foreign folder —
    /// visible but undeletable, with no way for the user to clean it up.
    #[test]
    fn both_marker_spellings_are_recognized() {
        assert!(is_marker(".threadknot-library.json"));
        assert!(is_marker(".armada-library.json"));
        assert!(!is_marker("SKILL.md"));
        assert!(!is_marker(".git"));
    }

    #[test]
    fn reads_frontmatter_including_folded_descriptions() {
        let front = parse_frontmatter(
            "---\nname: skill-creator\ndescription: >-\n  Helps author new skills.\n  Use when asked to make one.\nlicense: Apache-2.0\n---\n\n# Body\ntext\n",
        );
        assert_eq!(front.get("name").unwrap(), "skill-creator");
        assert_eq!(
            front.get("description").unwrap(),
            "Helps author new skills. Use when asked to make one."
        );
        assert_eq!(front.get("license").unwrap(), "Apache-2.0");
    }

    #[test]
    fn ignores_a_body_that_has_no_frontmatter() {
        assert!(parse_frontmatter("# Just a heading\n").is_empty());
        assert!(parse_frontmatter("---\nname: unterminated\n").is_empty());
    }

    #[test]
    fn parses_the_shapes_users_actually_paste() {
        let tree = SkillSource::parse(
            "https://github.com/anthropics/skills/tree/main/skills/skill-creator",
        )
        .unwrap();
        assert_eq!(tree.repo, "anthropics/skills");
        assert_eq!(tree.git_ref, "main");
        assert_eq!(tree.path, "skills/skill-creator");

        let short = SkillSource::parse("anthropics/skills/skills/mcp-builder").unwrap();
        assert_eq!(short.repo, "anthropics/skills");
        assert_eq!(short.git_ref, "HEAD");
        assert_eq!(short.path, "skills/mcp-builder");

        let root = SkillSource::parse("https://github.com/owner/repo.git").unwrap();
        assert_eq!(root.repo, "owner/repo");
        assert_eq!(root.path, "");

        assert!(SkillSource::parse("notarepo").is_err());
    }

    #[test]
    fn refuses_paths_that_escape_the_skill_folder() {
        let root = std::env::temp_dir().join("threadknot-safe-join");
        assert!(safe_join(&root, "scripts/run.py").is_ok());
        assert!(safe_join(&root, "../escape").is_err());
        assert!(safe_join(&root, "a/../../b").is_err());
        assert!(safe_join(&root, "/etc/passwd").is_err());
    }

    #[test]
    fn removal_rejects_ids_that_are_paths() {
        assert!(remove_skill("../../.ssh", &[SkillTarget::Claude]).is_err());
        assert!(remove_skill("", &[SkillTarget::Claude]).is_err());
    }

    fn stdio(name: &str) -> McpServer {
        McpServer {
            id: String::new(),
            name: name.into(),
            label: String::new(),
            description: String::new(),
            transport: McpTransport::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), "@modelcontextprotocol/server-memory".into()],
                env: BTreeMap::from([("MEMORY_FILE".into(), "/tmp/m.json".into())]),
            },
            enabled: true,
            agents: Vec::new(),
            catalog_id: None,
            created_at: String::new(),
        }
    }

    #[test]
    fn codex_overrides_are_valid_toml_assignments() {
        let (args, env) = codex_overrides(&stdio("memory"));
        assert_eq!(args[0], r#"mcp_servers.memory.command="npx""#);
        assert_eq!(
            args[1],
            r#"mcp_servers.memory.args=["-y", "@modelcontextprotocol/server-memory"]"#
        );
        assert_eq!(args[2], r#"mcp_servers.memory.env.MEMORY_FILE="/tmp/m.json""#);
        assert!(env.is_empty());
    }

    #[test]
    fn codex_http_translates_a_bearer_header_into_its_env_var() {
        let mut server = stdio("gh");
        server.transport = McpTransport::Http {
            url: "https://api.githubcopilot.com/mcp/".into(),
            headers: BTreeMap::from([("Authorization".into(), "Bearer ghp_secret".into())]),
        };
        let (args, env) = codex_overrides(&server);
        assert!(args.contains(&r#"mcp_servers.gh.url="https://api.githubcopilot.com/mcp/""#.to_string()));
        assert!(args
            .iter()
            .any(|a| a == r#"mcp_servers.gh.bearer_token_env_var="THREADKNOT_MCP_TOKEN_GH""#));
        assert_eq!(env, vec![("THREADKNOT_MCP_TOKEN_GH".into(), "ghp_secret".into())]);
        assert!(codex_unsupported_headers(&server).is_empty());

        server.transport = McpTransport::Http {
            url: "https://example.com/mcp".into(),
            headers: BTreeMap::from([("X-Api-Key".into(), "k".into())]),
        };
        assert_eq!(codex_unsupported_headers(&server), vec!["X-Api-Key"]);
    }

    #[test]
    fn registry_round_trips_and_guards_names() {
        let dir = std::env::temp_dir().join(format!("threadknot-mcp-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lib = McpLibrary::open(&dir).unwrap();

        let saved = lib.save(stdio("memory")).unwrap();
        assert!(!saved.id.is_empty());
        assert!(lib.save(stdio("memory")).is_err(), "duplicate name refused");
        assert!(lib.save(stdio(RESERVED_MCP_NAME)).is_err());
        assert!(lib.save(stdio("bad name")).is_err());

        // Targeting: empty list means every agent; a named list is honoured,
        // and Claudex rides along with Claude.
        assert_eq!(lib.for_agent(Agent::Codex).len(), 1);
        let mut scoped = saved.clone();
        scoped.agents = vec![Agent::Claude];
        lib.save(scoped.clone()).unwrap();
        assert!(lib.for_agent(Agent::Codex).is_empty());
        assert_eq!(lib.for_agent(Agent::Claudex).len(), 1);

        // Disabled servers never reach a driver.
        let mut off = scoped.clone();
        off.enabled = false;
        lib.save(off).unwrap();
        assert!(lib.for_agent(Agent::Claude).is_empty());

        let reopened = McpLibrary::open(&dir).unwrap();
        assert_eq!(reopened.list().len(), 1);
        reopened.delete(&saved.id).unwrap();
        assert!(reopened.delete(&saved.id).is_err());
        assert!(reopened.list().is_empty());

        std::fs::remove_dir_all(dir).unwrap();
    }
}

//! The shipped Library catalog: a curated shelf of skills and MCP servers a
//! user can install with one click.
//!
//! Entries are **pointers, not payloads**. A catalog skill names an upstream
//! repository and path that `library::install_skill` fetches at install time;
//! nothing is vendored into the Threadknot binary. That keeps the app small, keeps
//! skills current, and — the reason it is not merely a preference — keeps
//! Threadknot from becoming the thing that redistributes someone else's work.
//!
//! Every skill listed here was checked to carry an Apache-2.0 `LICENSE.txt` in
//! its own folder. Anthropic's document skills (`docx`/`pdf`/`pptx`/`xlsx`) are
//! deliberately absent and are refused by the installer as well; see
//! `library::LICENSE_BLOCKED`.
//!
//! MCP entries carry a template rather than a finished server: `${key}`
//! placeholders in the command, arguments, URL, environment, or headers are
//! filled from the inputs the user types at install time. That is how one entry
//! covers "filesystem server, but rooted where *you* say" and "GitHub, with
//! *your* token" without the catalog holding secrets.

use crate::library::{McpServer, McpTransport};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Where a catalog skill's files come from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "source", rename_all = "camelCase")]
pub enum SkillSourceRef {
    /// Fetched from a public GitHub repository at install time.
    Github {
        /// `owner/repo`
        repo: String,
        git_ref: String,
        /// Folder inside the repo holding `SKILL.md`.
        path: String,
    },
    /// Written by Threadknot and embedded in the binary (`bundled.rs`). Installs
    /// with no network.
    Bundled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSkill {
    pub id: String,
    pub title: String,
    pub description: String,
    pub category: String,
    #[serde(flatten)]
    pub origin: SkillSourceRef,
    pub license: String,
    pub homepage: String,
}

/// One value the user supplies when installing a catalog MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogInput {
    /// Substituted for `${key}` throughout the template.
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub placeholder: String,
    #[serde(default)]
    pub help: String,
    /// Render as a password field; the value is a credential.
    #[serde(default)]
    pub secret: bool,
    /// A blank value is allowed and simply leaves the placeholder unfilled.
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogMcp {
    pub id: String,
    /// Default tool namespace. The user may rename before saving.
    pub name: String,
    pub title: String,
    pub description: String,
    pub category: String,
    pub transport: McpTransport,
    #[serde(default)]
    pub inputs: Vec<CatalogInput>,
    /// Executable this server needs on PATH (`npx`, `uvx`, `docker`). Reported
    /// as available/missing so a user learns why a server would fail to start
    /// BEFORE they install it and wonder why the agent has no tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prereq: Option<String>,
    /// Whether `prereq` is actually on PATH right now.
    #[serde(default)]
    pub prereq_ok: bool,
    pub homepage: String,
}

/// The full shelf, with prerequisite availability resolved for this machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub skills: Vec<CatalogSkill>,
    pub mcp: Vec<CatalogMcp>,
}

fn skill(id: &str, title: &str, category: &str, description: &str) -> CatalogSkill {
    CatalogSkill {
        id: id.to_string(),
        title: title.to_string(),
        description: description.to_string(),
        category: category.to_string(),
        origin: SkillSourceRef::Github {
            repo: "anthropics/skills".into(),
            git_ref: "main".into(),
            path: format!("skills/{id}"),
        },
        license: "Apache-2.0".into(),
        homepage: format!("https://github.com/anthropics/skills/tree/main/skills/{id}"),
    }
}

/// A skill Threadknot wrote and ships in its own binary.
fn bundled(id: &str, title: &str, description: &str) -> CatalogSkill {
    CatalogSkill {
        id: id.to_string(),
        title: title.to_string(),
        description: description.to_string(),
        category: "Documents (built in)".into(),
        origin: SkillSourceRef::Bundled,
        license: "Apache-2.0".into(),
        homepage: "https://github.com/smith-network-solutions/threadknot".into(),
    }
}

/// Threadknot's own document skills.
///
/// These exist because Anthropic's `docx`/`pdf`/`pptx`/`xlsx` skills are the
/// most-wanted ones and the ones the installer refuses (source-available, not
/// open source). Rather than leave the gap, these are clean-room equivalents
/// written against the public documentation of python-docx, openpyxl,
/// python-pptx, pypdf, pdfplumber and reportlab — every one MIT or BSD — and
/// against the file-format specs. Same capability, unencumbered.
fn document_skills() -> Vec<CatalogSkill> {
    vec![
        bundled(
            "docx",
            "Word documents",
            "Create, read and edit .docx — headings, tables, images, styles, headers and footers, template filling, plus the raw OOXML route and a render-to-PDF check.",
        ),
        bundled(
            "xlsx",
            "Excel workbooks",
            "Create, read and edit .xlsx — formulas, number formats, multiple sheets, charts and frozen panes, with the formula recalculation step openpyxl cannot do on its own.",
        ),
        bundled(
            "pptx",
            "PowerPoint decks",
            "Create, read and edit .pptx — build on a corporate template's layouts, add slides, tables, images and notes, then render slides to images to check they look right.",
        ),
        bundled(
            "pdf",
            "PDF files",
            "Extract text and tables, merge, split, rotate, fill AcroForm fields, set metadata, encrypt, OCR scans, and generate PDFs from HTML or Office sources.",
        ),
    ]
}

/// Curated skills. Descriptions are shortened from each skill's own
/// frontmatter — the full text ships with the folder at install time.
pub fn skills() -> Vec<CatalogSkill> {
    // Threadknot's own document skills lead: they are the capability people come
    // looking for, and they install offline.
    let mut all = document_skills();
    all.extend([
        skill(
            "skill-creator",
            "Skill Creator",
            "Authoring",
            "Create new skills from scratch, improve existing ones, and run evals to measure how reliably a skill triggers.",
        ),
        skill(
            "mcp-builder",
            "MCP Builder",
            "Authoring",
            "Guide for building high-quality MCP servers in Python (FastMCP) or Node/TypeScript, with well-designed tools.",
        ),
        skill(
            "webapp-testing",
            "Web App Testing",
            "Development",
            "Drive and test a local web app with Playwright: verify frontend behaviour, capture screenshots, read browser logs.",
        ),
        skill(
            "claude-api",
            "Claude API Reference",
            "Development",
            "Model ids, pricing, params, streaming, tool use, caching and migration for the Claude API — so answers are looked up, not remembered.",
        ),
        skill(
            "web-artifacts-builder",
            "Web Artifacts Builder",
            "Development",
            "Build elaborate multi-component HTML artifacts with React, Tailwind and shadcn/ui.",
        ),
        skill(
            "frontend-design",
            "Frontend Design",
            "Design",
            "Aesthetic direction, typography and layout guidance for UI that does not read as a template.",
        ),
        skill(
            "canvas-design",
            "Canvas Design",
            "Design",
            "Create original posters, art and static designs as .png and .pdf, driven by an explicit design philosophy.",
        ),
        skill(
            "algorithmic-art",
            "Algorithmic Art",
            "Design",
            "Generative art with p5.js — flow fields, particle systems, seeded randomness and an interactive parameter viewer.",
        ),
        skill(
            "theme-factory",
            "Theme Factory",
            "Design",
            "Apply one of ten preset colour/font themes to slides, docs and landing pages, or generate a new theme on the fly.",
        ),
        skill(
            "brand-guidelines",
            "Brand Guidelines",
            "Design",
            "Applies Anthropic's official brand colours and typography to any artifact that should match their look.",
        ),
        skill(
            "internal-comms",
            "Internal Comms",
            "Writing",
            "Status reports, leadership updates, newsletters, FAQs and incident reports in the formats companies actually use.",
        ),
        skill(
            "slack-gif-creator",
            "Slack GIF Creator",
            "Writing",
            "Animated GIFs sized and validated for Slack's upload constraints.",
        ),
    ]);
    all
}

fn stdio(command: &str, args: &[&str]) -> McpTransport {
    McpTransport::Stdio {
        command: command.into(),
        args: args.iter().map(|a| a.to_string()).collect(),
        env: BTreeMap::new(),
    }
}

fn http(url: &str, headers: &[(&str, &str)]) -> McpTransport {
    McpTransport::Http {
        url: url.into(),
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

/// Curated MCP servers: the official reference set plus a few well-known remote
/// endpoints. `prereq_ok` is filled in by [`catalog`].
fn mcp_templates() -> Vec<CatalogMcp> {
    vec![
        CatalogMcp {
            id: "filesystem".into(),
            name: "filesystem".into(),
            title: "Filesystem".into(),
            description: "Read, write and search files under a directory you nominate — useful for giving an agent a data folder that is not its project.".into(),
            category: "Official reference".into(),
            transport: stdio("npx", &["-y", "@modelcontextprotocol/server-filesystem", "${root}"]),
            inputs: vec![CatalogInput {
                key: "root".into(),
                label: "Directory".into(),
                placeholder: "/home/you/documents".into(),
                help: "The server refuses everything outside this path.".into(),
                secret: false,
                optional: false,
            }],
            prereq: Some("npx".into()),
            prereq_ok: false,
            homepage: "https://github.com/modelcontextprotocol/servers/tree/main/src/filesystem".into(),
        },
        CatalogMcp {
            id: "memory".into(),
            name: "memory".into(),
            title: "Memory".into(),
            description: "A knowledge graph the agent can write to and read back, so facts survive between chats.".into(),
            category: "Official reference".into(),
            transport: stdio("npx", &["-y", "@modelcontextprotocol/server-memory"]),
            inputs: Vec::new(),
            prereq: Some("npx".into()),
            prereq_ok: false,
            homepage: "https://github.com/modelcontextprotocol/servers/tree/main/src/memory".into(),
        },
        CatalogMcp {
            id: "sequential-thinking".into(),
            name: "sequential-thinking".into(),
            title: "Sequential Thinking".into(),
            description: "A scratchpad tool for stepwise problem decomposition, letting the agent revise earlier steps as it goes.".into(),
            category: "Official reference".into(),
            transport: stdio("npx", &["-y", "@modelcontextprotocol/server-sequential-thinking"]),
            inputs: Vec::new(),
            prereq: Some("npx".into()),
            prereq_ok: false,
            homepage: "https://github.com/modelcontextprotocol/servers/tree/main/src/sequentialthinking".into(),
        },
        CatalogMcp {
            id: "fetch".into(),
            name: "fetch".into(),
            title: "Fetch".into(),
            description: "Retrieve a URL and convert it to markdown. Lighter than driving the browser when the agent only needs to read a page.".into(),
            category: "Official reference".into(),
            transport: stdio("uvx", &["mcp-server-fetch"]),
            inputs: Vec::new(),
            prereq: Some("uvx".into()),
            prereq_ok: false,
            homepage: "https://github.com/modelcontextprotocol/servers/tree/main/src/fetch".into(),
        },
        CatalogMcp {
            id: "git".into(),
            name: "git".into(),
            title: "Git".into(),
            description: "Read and search a repository's history through tools instead of shell commands.".into(),
            category: "Official reference".into(),
            transport: stdio("uvx", &["mcp-server-git", "--repository", "${repo}"]),
            inputs: vec![CatalogInput {
                key: "repo".into(),
                label: "Repository path".into(),
                placeholder: "/home/you/projects/thing".into(),
                help: String::new(),
                secret: false,
                optional: false,
            }],
            prereq: Some("uvx".into()),
            prereq_ok: false,
            homepage: "https://github.com/modelcontextprotocol/servers/tree/main/src/git".into(),
        },
        CatalogMcp {
            id: "time".into(),
            name: "time".into(),
            title: "Time".into(),
            description: "Current time and timezone conversion — small, but it stops an agent guessing today's date.".into(),
            category: "Official reference".into(),
            transport: stdio("uvx", &["mcp-server-time"]),
            inputs: Vec::new(),
            prereq: Some("uvx".into()),
            prereq_ok: false,
            homepage: "https://github.com/modelcontextprotocol/servers/tree/main/src/time".into(),
        },
        CatalogMcp {
            id: "context7".into(),
            name: "context7".into(),
            title: "Context7".into(),
            description: "Up-to-date documentation and code examples for thousands of libraries, fetched per version. Works without a key; add one to raise rate limits.".into(),
            category: "Remote".into(),
            transport: http("https://mcp.context7.com/mcp", &[("Authorization", "Bearer ${apiKey}")]),
            inputs: vec![CatalogInput {
                key: "apiKey".into(),
                label: "API key".into(),
                placeholder: "leave blank for anonymous access".into(),
                help: "Optional. Anonymous use is rate limited.".into(),
                secret: true,
                optional: true,
            }],
            prereq: None,
            prereq_ok: true,
            homepage: "https://context7.com".into(),
        },
        CatalogMcp {
            id: "deepwiki".into(),
            name: "deepwiki".into(),
            title: "DeepWiki".into(),
            description: "Ask questions about any public GitHub repository and get answers grounded in its generated docs. No account needed.".into(),
            category: "Remote".into(),
            transport: http("https://mcp.deepwiki.com/mcp", &[]),
            inputs: Vec::new(),
            prereq: None,
            prereq_ok: true,
            homepage: "https://deepwiki.com".into(),
        },
        CatalogMcp {
            id: "github".into(),
            name: "github".into(),
            title: "GitHub".into(),
            description: "Issues, pull requests, code search and Actions on your own repositories, through GitHub's hosted server.".into(),
            category: "Remote".into(),
            transport: http("https://api.githubcopilot.com/mcp/", &[("Authorization", "Bearer ${token}")]),
            inputs: vec![CatalogInput {
                key: "token".into(),
                label: "Personal access token".into(),
                placeholder: "ghp_…".into(),
                help: "A fine-grained token scoped to the repositories you want the agent to reach.".into(),
                secret: true,
                optional: false,
            }],
            prereq: None,
            prereq_ok: true,
            homepage: "https://github.com/github/github-mcp-server".into(),
        },
    ]
}

/// The catalog as this machine sees it: prerequisite binaries resolved against
/// the same PATH the agent drivers use, so the answer matches what would
/// actually happen at spawn.
pub fn catalog() -> Catalog {
    let mut mcp = mcp_templates();
    for entry in mcp.iter_mut() {
        entry.prereq_ok = match &entry.prereq {
            // Same resolver the drivers use, so the answer matches what the
            // child process would actually find at spawn.
            Some(bin) => crate::agents::resolve_bin(bin).is_some(),
            None => true,
        };
    }
    Catalog {
        skills: skills(),
        mcp,
    }
}

/// Build a saveable server from a catalog entry and the values the user typed.
///
/// A blank optional input removes what it fills: an unset Context7 key drops
/// the whole `Authorization` header rather than sending `Bearer ${apiKey}`
/// literally, which would fail authentication in a way that looks like the
/// server is broken.
pub fn render(entry: &CatalogMcp, inputs: &BTreeMap<String, String>) -> anyhow::Result<McpServer> {
    for input in &entry.inputs {
        if !input.optional
            && inputs
                .get(&input.key)
                .map(|v| v.trim().is_empty())
                .unwrap_or(true)
        {
            anyhow::bail!("{} is required", input.label);
        }
    }

    let blank: Vec<&str> = entry
        .inputs
        .iter()
        .filter(|i| {
            inputs
                .get(&i.key)
                .map(|v| v.trim().is_empty())
                .unwrap_or(true)
        })
        .map(|i| i.key.as_str())
        .collect();
    let unfilled = |text: &str| blank.iter().any(|key| text.contains(&format!("${{{key}}}")));
    let fill = |text: &str| {
        let mut out = text.to_string();
        for (key, value) in inputs {
            out = out.replace(&format!("${{{key}}}"), value.trim());
        }
        out
    };

    let transport = match &entry.transport {
        McpTransport::Stdio { command, args, env } => McpTransport::Stdio {
            command: fill(command),
            args: args
                .iter()
                .filter(|a| !unfilled(a))
                .map(|a| fill(a))
                .collect(),
            env: env
                .iter()
                .filter(|(_, v)| !unfilled(v))
                .map(|(k, v)| (k.clone(), fill(v)))
                .collect(),
        },
        McpTransport::Http { url, headers } => McpTransport::Http {
            url: fill(url),
            headers: headers
                .iter()
                .filter(|(_, v)| !unfilled(v))
                .map(|(k, v)| (k.clone(), fill(v)))
                .collect(),
        },
    };

    Ok(McpServer {
        id: String::new(),
        name: entry.name.clone(),
        label: entry.title.clone(),
        description: entry.description.clone(),
        transport,
        enabled: true,
        agents: Vec::new(),
        catalog_id: Some(entry.id.clone()),
        created_at: String::new(),
    })
}

pub fn find_mcp(id: &str) -> Option<CatalogMcp> {
    mcp_templates().into_iter().find(|e| e.id == id)
}

pub fn find_skill(id: &str) -> Option<CatalogSkill> {
    skills().into_iter().find(|e| e.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_skill_points_at_a_licensed_folder() {
        for entry in skills() {
            assert_eq!(entry.license, "Apache-2.0");
            match &entry.origin {
                SkillSourceRef::Bundled => {
                    // A bundled entry must actually be in the binary, or its
                    // tile installs nothing.
                    assert!(
                        crate::bundled::find(&entry.id).is_some(),
                        "{} is listed as bundled but is not embedded",
                        entry.id
                    );
                }
                SkillSourceRef::Github { repo, path, .. } => {
                    assert_eq!(repo, "anthropics/skills");
                    assert_eq!(*path, format!("skills/{}", entry.id));
                    // The four source-available document skills must never be
                    // fetched: the installer refuses them, and a dead catalog
                    // tile is worse than no tile. Threadknot's own replacements are
                    // Bundled entries, which is why they do not trip this.
                    assert!(
                        !crate::library::LICENSE_BLOCKED
                            .iter()
                            .any(|(_, blocked)| blocked == path),
                        "{} is license-blocked but appears as a fetched entry",
                        entry.id
                    );
                }
            }
        }
    }

    #[test]
    fn threadknot_ships_a_replacement_for_every_blocked_skill() {
        let bundled_ids: Vec<String> = skills()
            .into_iter()
            .filter(|s| s.origin == SkillSourceRef::Bundled)
            .map(|s| s.id)
            .collect();
        for (_, path) in crate::library::LICENSE_BLOCKED {
            let name = path.rsplit('/').next().unwrap();
            assert!(
                bundled_ids.iter().any(|id| id == name),
                "{name} is refused but the catalog offers no alternative"
            );
        }
    }

    #[test]
    fn fills_placeholders_and_drops_blank_optional_ones() {
        let filesystem = find_mcp("filesystem").unwrap();
        let rendered = render(
            &filesystem,
            &BTreeMap::from([("root".to_string(), "/srv/data".to_string())]),
        )
        .unwrap();
        match rendered.transport {
            McpTransport::Stdio { args, .. } => {
                assert_eq!(args.last().unwrap(), "/srv/data");
            }
            _ => panic!("expected stdio"),
        }

        // Required input missing.
        assert!(render(&filesystem, &BTreeMap::new()).is_err());

        // Optional key left blank drops the header it would have filled.
        let context7 = find_mcp("context7").unwrap();
        let anonymous = render(&context7, &BTreeMap::new()).unwrap();
        match anonymous.transport {
            McpTransport::Http { headers, .. } => assert!(headers.is_empty()),
            _ => panic!("expected http"),
        }
        let keyed = render(
            &context7,
            &BTreeMap::from([("apiKey".to_string(), "abc123".to_string())]),
        )
        .unwrap();
        match keyed.transport {
            McpTransport::Http { headers, .. } => {
                assert_eq!(headers.get("Authorization").unwrap(), "Bearer abc123");
            }
            _ => panic!("expected http"),
        }
    }

    #[test]
    fn catalog_ids_are_unique() {
        let mut ids: Vec<String> = skills().into_iter().map(|s| s.id).collect();
        ids.extend(mcp_templates().into_iter().map(|m| m.id));
        let before = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), before);
    }
}

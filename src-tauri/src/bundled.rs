//! Skills Threadknot ships inside its own binary.
//!
//! Catalog skills are normally *pointers* fetched from GitHub at install time
//! (see `catalog.rs`). These four are different: Threadknot wrote them, so they are
//! embedded with `include_str!` and install with no network at all.
//!
//! Why they exist. Anthropic's `docx`/`pdf`/`pptx`/`xlsx` skills are the ones
//! everybody looks for and the ones Threadknot refuses to copy — their license
//! forbids retaining copies outside Anthropic's services. Rather than leave a
//! hole where the most-wanted capability should be, these are **clean-room**
//! equivalents: written against the public documentation of python-docx,
//! openpyxl, python-pptx, pypdf, pdfplumber and reportlab (all MIT or BSD), and
//! against the file-format specifications. Anthropic's skills were never read
//! while writing them, and no text is derived from them. They provide the same
//! *capability*, which is not something a license can reserve.
//!
//! Adding a file here means adding an `include_str!` line — deliberately
//! explicit rather than a build-script directory walk, so what ships in the
//! binary is greppable.

/// One file inside a bundled skill. `executable` gets the +x bit on install;
/// `include_str!` cannot carry file modes, and a script without it fails with a
/// permission error the first time an agent tries to run it.
pub struct BundledFile {
    pub path: &'static str,
    pub contents: &'static str,
    pub executable: bool,
}

pub struct BundledSkill {
    pub id: &'static str,
    pub files: &'static [BundledFile],
}

macro_rules! doc {
    ($skill:literal, $file:literal) => {
        BundledFile {
            path: $file,
            contents: include_str!(concat!("../../skills/", $skill, "/", $file)),
            executable: false,
        }
    };
}

macro_rules! script {
    ($skill:literal, $file:literal) => {
        BundledFile {
            path: $file,
            contents: include_str!(concat!("../../skills/", $skill, "/", $file)),
            executable: true,
        }
    };
}

static DOCX: &[BundledFile] = &[
    doc!("docx", "SKILL.md"),
    doc!("docx", "LICENSE.txt"),
    script!("docx", "scripts/inspect_docx.py"),
    script!("docx", "scripts/replace.py"),
    script!("docx", "scripts/ooxml.py"),
    script!("docx", "scripts/topdf.py"),
];

static XLSX: &[BundledFile] = &[
    doc!("xlsx", "SKILL.md"),
    doc!("xlsx", "LICENSE.txt"),
    script!("xlsx", "scripts/inspect_xlsx.py"),
    script!("xlsx", "scripts/recalc.py"),
    script!("xlsx", "scripts/autofit.py"),
    script!("xlsx", "scripts/ooxml.py"),
    script!("xlsx", "scripts/topdf.py"),
];

static PPTX: &[BundledFile] = &[
    doc!("pptx", "SKILL.md"),
    doc!("pptx", "LICENSE.txt"),
    script!("pptx", "scripts/inspect_pptx.py"),
    script!("pptx", "scripts/checkoverflow.py"),
    script!("pptx", "scripts/ooxml.py"),
    script!("pptx", "scripts/topdf.py"),
];

static PDF: &[BundledFile] = &[
    doc!("pdf", "SKILL.md"),
    doc!("pdf", "LICENSE.txt"),
    script!("pdf", "scripts/pdftool.py"),
    script!("pdf", "scripts/extract.py"),
    script!("pdf", "scripts/topdf.py"),
];

pub static SKILLS: &[BundledSkill] = &[
    BundledSkill { id: "docx", files: DOCX },
    BundledSkill { id: "xlsx", files: XLSX },
    BundledSkill { id: "pptx", files: PPTX },
    BundledSkill { id: "pdf", files: PDF },
];

pub fn find(id: &str) -> Option<&'static BundledSkill> {
    SKILLS.iter().find(|s| s.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_skill_is_installable() {
        assert_eq!(SKILLS.len(), 4);
        for skill in SKILLS {
            // A skill with no SKILL.md is not a skill — no CLI would load it.
            let manifest = skill
                .files
                .iter()
                .find(|f| f.path == "SKILL.md")
                .unwrap_or_else(|| panic!("{} has no SKILL.md", skill.id));
            assert!(
                manifest.contents.starts_with("---\n"),
                "{} SKILL.md has no frontmatter",
                skill.id
            );
            assert!(
                manifest.contents.contains(&format!("name: {}", skill.id)),
                "{} frontmatter name must match its directory",
                skill.id
            );
            assert!(
                skill.files.iter().any(|f| f.path == "LICENSE.txt"),
                "{} ships without its license",
                skill.id
            );
            // Scripts must be marked executable or the agent's first run of one
            // fails with a permission error.
            for file in skill.files {
                if file.path.starts_with("scripts/") {
                    assert!(file.executable, "{} is not marked executable", file.path);
                }
                assert!(!file.contents.is_empty(), "{} is empty", file.path);
                assert!(
                    !file.path.contains("..") && !file.path.starts_with('/'),
                    "{} is not a safe relative path",
                    file.path
                );
            }
        }
    }

    #[test]
    fn bundled_ids_match_the_license_blocked_names() {
        // These four exist precisely because the upstream ones are refused. If
        // that list ever changes, this is the reminder to revisit them.
        for (_, path) in crate::library::LICENSE_BLOCKED {
            let name = path.rsplit('/').next().unwrap();
            assert!(
                find(name).is_some(),
                "{name} is license-blocked but Threadknot ships no replacement"
            );
        }
    }
}

//! User-crafted appearance themes, persisted atomically to
//! `~/.threadknot/themes.json`.
//!
//! A theme is a named palette (a preset base + accent) with optional neutral
//! slot overrides and an optional background-image data URL. The background can
//! be sizeable, so themes live server-side rather than in the client's
//! localStorage; the whole set is machine-local (never mesh-replicated) and is
//! read back via `theme.list`. Modeled on `hermes.rs`: a Mutex-guarded vec with
//! a tmp-rename flush. The server owns the timestamps and enforces the sanity
//! bounds, so a leaked LAN token cannot forge a record or write an unbounded
//! one.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::protocol::{new_id, now_iso, CustomTheme};

/// Most themes one machine may keep.
const MAX_THEMES: usize = 40;
/// Trimmed name length, in characters.
const MAX_NAME_CHARS: usize = 64;
/// Longest a preset id / accent string may be (a `#rrggbb` or a short id).
const MAX_PRESET_CHARS: usize = 64;
/// Cap the background data URL. The client downscales to roughly 300 KB; this
/// is the hard ceiling for a hand-written RPC.
const MAX_BACKGROUND_BYTES: usize = 2 * 1024 * 1024;
/// Cap the WHOLE serialized store. `theme.list` ships every record to every
/// client on each broadcast, so an unbounded set (40 themes x a 2 MB wallpaper
/// each) would make each list ~80 MB. A save that would push the store past
/// this ceiling is refused with a readable "remove a wallpaper or theme"
/// message; per-image and per-count caps still apply on top.
const MAX_STORE_BYTES: usize = 24 * 1024 * 1024;
/// Most neutral-slot overrides a theme may carry.
const MAX_COLOR_ENTRIES: usize = 24;
/// Longest a single color override value may be (a CSS color string).
const MAX_COLOR_VALUE_CHARS: usize = 32;

#[derive(Debug, Default, Serialize, Deserialize)]
struct ThemeFile {
    #[serde(default)]
    themes: Vec<CustomTheme>,
}

pub struct ThemeStore {
    path: PathBuf,
    themes: Mutex<Vec<CustomTheme>>,
}

impl ThemeStore {
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("themes.json");
        let themes = if path.exists() {
            let file: ThemeFile = serde_json::from_str(&std::fs::read_to_string(&path)?)
                .context("parse themes.json")?;
            let mut themes = file.themes;
            // Saves sanitize, but the file on disk answers to no one: clamp
            // each record's numeric fields on load so a hand-edited store
            // cannot feed the clients out-of-range values.
            for theme in &mut themes {
                clamp_numeric_fields(theme);
            }
            themes
        } else {
            Vec::new()
        };
        Ok(Self {
            path,
            themes: Mutex::new(themes),
        })
    }

    fn flush(&self, themes: &[CustomTheme]) -> Result<()> {
        let file = ThemeFile {
            themes: themes.to_vec(),
        };
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&file)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn list(&self) -> Vec<CustomTheme> {
        self.themes.lock().unwrap().clone()
    }

    /// Upsert by id: replaces the theme with the same id (keeping its original
    /// `created_at`), otherwise inserts a new one — an unknown id therefore
    /// creates. The server always stamps `updated_at`, and `created_at` on a
    /// fresh record, so the client cannot forge either. Sanity-bounds the whole
    /// record first.
    pub fn save(&self, mut theme: CustomTheme) -> Result<CustomTheme> {
        sanitize(&mut theme)?;
        let mut themes = self.themes.lock().unwrap();
        let now = now_iso();
        theme.updated_at = now.clone();
        // Mutate a COPY, flush it, and only swap it into memory once the disk
        // write succeeds — so a failed flush (or an over-cap store) leaves the
        // in-memory vec and `themes.json` identical, never diverged.
        let mut next = themes.clone();
        match next.iter_mut().find(|t| t.id == theme.id) {
            Some(existing) => {
                // Preserve the original creation stamp across edits.
                theme.created_at = existing.created_at.clone();
                *existing = theme.clone();
            }
            None => {
                anyhow::ensure!(
                    next.len() < MAX_THEMES,
                    "theme limit reached (at most {MAX_THEMES} custom themes)"
                );
                theme.created_at = now;
                next.push(theme.clone());
            }
        }
        // Reject before writing anything when the whole store would blow the
        // total-size ceiling (a save is the only op that can grow it).
        let bytes = serialized_len(&next)?;
        anyhow::ensure!(
            bytes <= MAX_STORE_BYTES,
            "theme storage is full ({} MB of {} MB used) — remove a wallpaper or a theme, then save again",
            bytes / (1024 * 1024),
            MAX_STORE_BYTES / (1024 * 1024)
        );
        self.flush(&next)?;
        *themes = next;
        Ok(theme)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let mut themes = self.themes.lock().unwrap();
        // Same transactional shape as `save`: shrink a copy, flush, then swap.
        let mut next = themes.clone();
        let before = next.len();
        next.retain(|t| t.id != id);
        anyhow::ensure!(next.len() < before, "unknown theme");
        self.flush(&next)?;
        *themes = next;
        Ok(())
    }
}

/// Byte length of the whole store serialized as it rides `theme.list` (compact
/// JSON), used to enforce the total-size ceiling before a save is committed.
fn serialized_len(themes: &[CustomTheme]) -> Result<usize> {
    let file = ThemeFile {
        themes: themes.to_vec(),
    };
    Ok(serde_json::to_string(&file)?.len())
}

/// Enforce the server-side bounds and normalize the record in place. A blank id
/// is minted (the client normally supplies one); the name is trimmed and length
/// checked; the background image, color map, and dim are bounded/clamped.
fn sanitize(theme: &mut CustomTheme) -> Result<()> {
    if theme.id.trim().is_empty() {
        theme.id = new_id();
    }

    let name = theme.name.trim().to_string();
    anyhow::ensure!(!name.is_empty(), "theme name cannot be empty");
    anyhow::ensure!(
        name.chars().count() <= MAX_NAME_CHARS,
        "theme name is too long (at most {MAX_NAME_CHARS} characters)"
    );
    theme.name = name;

    anyhow::ensure!(
        theme.base.chars().count() <= MAX_PRESET_CHARS,
        "theme base is too long"
    );
    anyhow::ensure!(
        theme.accent.chars().count() <= MAX_PRESET_CHARS,
        "theme accent is too long"
    );

    if let Some(image) = &theme.background_image {
        anyhow::ensure!(
            image.len() <= MAX_BACKGROUND_BYTES,
            "background image is too large (at most 2 MB)"
        );
    }

    clamp_numeric_fields(theme);

    if let Some(colors) = &theme.colors {
        anyhow::ensure!(
            colors.len() <= MAX_COLOR_ENTRIES,
            "too many color overrides (at most {MAX_COLOR_ENTRIES})"
        );
        for (key, value) in colors {
            anyhow::ensure!(
                value.chars().count() <= MAX_COLOR_VALUE_CHARS,
                "color value for `{key}` is too long (at most {MAX_COLOR_VALUE_CHARS} characters)"
            );
        }
    }

    Ok(())
}

/// Clamp the numeric wallpaper fields into their legal ranges. Runs inside
/// `sanitize` on every save, and per record on `open`, so a hand-edited or
/// corrupted themes.json cannot drive the client with out-of-range values
/// (the renderer clamps too, but the studio preview and drag math read the
/// record as-is).
fn clamp_numeric_fields(theme: &mut CustomTheme) {
    // Clamp to the readable range; a non-finite value falls back to no dim.
    if let Some(dim) = theme.background_dim {
        theme.background_dim = Some(if dim.is_finite() {
            dim.clamp(0.0, 0.9)
        } else {
            0.0
        });
    }

    // Clamp zoom into 1..3; a non-finite value falls back to plain cover (1).
    if let Some(zoom) = theme.background_zoom {
        theme.background_zoom = Some(if zoom.is_finite() {
            zoom.clamp(1.0, 3.0)
        } else {
            1.0
        });
    }

    // Clamp the pan offsets into -100..100; a non-finite value falls back to
    // centered (0).
    let clamp_pan = |v: f64| if v.is_finite() { v.clamp(-100.0, 100.0) } else { 0.0 };
    if let Some(x) = theme.background_x {
        theme.background_x = Some(clamp_pan(x));
    }
    if let Some(y) = theme.background_y {
        theme.background_y = Some(clamp_pan(y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn theme(id: &str, name: &str) -> CustomTheme {
        CustomTheme {
            id: id.into(),
            name: name.into(),
            base: "midnight".into(),
            accent: "#5b8def".into(),
            colors: None,
            background_image: None,
            background_dim: None,
            background_zoom: None,
            background_x: None,
            background_y: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("threadknot-themes-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_upsert_remove_roundtrip() {
        let dir = temp_dir();
        let store = ThemeStore::open(&dir).unwrap();

        // Save stamps both timestamps and preserves the client id.
        let saved = store.save(theme("t1", "  Ocean  ")).unwrap();
        assert_eq!(saved.id, "t1");
        assert_eq!(saved.name, "Ocean"); // trimmed
        assert!(!saved.created_at.is_empty());
        assert!(!saved.updated_at.is_empty());
        let created = saved.created_at.clone();

        // Upsert by id preserves created_at, refreshes updated_at.
        let mut edit = theme("t1", "Ocean Deep");
        edit.updated_at = "forged".into();
        edit.created_at = "forged".into();
        let updated = store.save(edit).unwrap();
        assert_eq!(updated.name, "Ocean Deep");
        assert_eq!(updated.created_at, created);
        assert_ne!(updated.updated_at, "forged");
        assert_eq!(store.list().len(), 1);

        // Unknown id creates.
        store.save(theme("t2", "Sand")).unwrap();
        assert_eq!(store.list().len(), 2);

        // Reload from disk sees both.
        let reloaded = ThemeStore::open(&dir).unwrap();
        assert_eq!(reloaded.list().len(), 2);

        // Remove is exact and errors on an unknown id.
        reloaded.remove("t1").unwrap();
        assert_eq!(reloaded.list().len(), 1);
        assert!(reloaded.remove("t1").is_err());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn enforces_bounds() {
        let dir = temp_dir();
        let store = ThemeStore::open(&dir).unwrap();

        // Empty name rejected.
        assert!(store.save(theme("t", "   ")).is_err());

        // Oversize name rejected.
        let mut long = theme("t", "");
        long.name = "x".repeat(65);
        assert!(store.save(long).is_err());

        // Dim is clamped into 0..0.9.
        let mut dim = theme("dim", "Dim");
        dim.background_dim = Some(5.0);
        assert_eq!(store.save(dim).unwrap().background_dim, Some(0.9));

        // Zoom is clamped into 1..3; pan offsets into -100..100.
        let mut place = theme("place", "Place");
        place.background_zoom = Some(9.0);
        place.background_x = Some(500.0);
        place.background_y = Some(-500.0);
        let saved_place = store.save(place).unwrap();
        assert_eq!(saved_place.background_zoom, Some(3.0));
        assert_eq!(saved_place.background_x, Some(100.0));
        assert_eq!(saved_place.background_y, Some(-100.0));

        // Non-finite zoom/pan fall back to the neutral defaults (1 / 0).
        let mut nan = theme("nan", "NaN");
        nan.background_zoom = Some(f64::NAN);
        nan.background_x = Some(f64::INFINITY);
        let saved_nan = store.save(nan).unwrap();
        assert_eq!(saved_nan.background_zoom, Some(1.0));
        assert_eq!(saved_nan.background_x, Some(0.0));

        // Oversize background rejected.
        let mut big = theme("big", "Big");
        big.background_image = Some("x".repeat(2 * 1024 * 1024 + 1));
        assert!(store.save(big).is_err());

        // Too many color entries rejected.
        let mut many = theme("many", "Many");
        let mut map = HashMap::new();
        for i in 0..25 {
            map.insert(format!("k{i}"), "#000".into());
        }
        many.colors = Some(map);
        assert!(store.save(many).is_err());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_when_store_exceeds_total_cap() {
        let dir = temp_dir();
        let store = ThemeStore::open(&dir).unwrap();

        // Each theme carries a ~1.9 MB wallpaper (comfortably under the 2 MB
        // per-image cap); the whole store tips past the 24 MB total ceiling
        // well before the 40-theme count would.
        let big_image = "x".repeat(1_900_000);
        let mut hit_cap = false;
        for i in 0..40 {
            let mut t = theme(&format!("t{i}"), &format!("Theme {i}"));
            t.background_image = Some(big_image.clone());
            match store.save(t) {
                Ok(_) => {}
                Err(e) => {
                    assert!(e.to_string().contains("storage is full"));
                    hit_cap = true;
                    break;
                }
            }
        }
        assert!(hit_cap, "the total-size cap should reject before 40 themes");

        // The rejected save is transactional: the in-memory vec matches disk.
        let reloaded = ThemeStore::open(&dir).unwrap();
        assert_eq!(reloaded.list().len(), store.list().len());

        std::fs::remove_dir_all(dir).unwrap();
    }
}

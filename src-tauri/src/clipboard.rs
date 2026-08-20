//! Native file clipboard support for the desktop shell.
//!
//! Browsers can copy text and images, but they cannot place an ordinary file
//! reference on the operating-system clipboard. These Tauri commands resolve
//! known Threadknot files and publish them in the platform's native file-list
//! format so they paste into Files/Nautilus, Finder, Explorer, email clients,
//! and other desktop apps as files rather than as paths or raw text.

use crate::store::Store;
use clipboard_rs::{Clipboard, ClipboardContext};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone)]
pub struct ClipboardState {
    store: Arc<Store>,
}

impl ClipboardState {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }
}

/// The one clipboard context this process ever builds.
///
/// `ClipboardContext::new` opens two X server connections and detaches a thread
/// that keeps an `Arc` to them, and the type has no `Drop` — so a context built
/// per call never gives either connection back, even after the value goes out
/// of scope. Xorg caps a *session* at 256 clients, shared by every application
/// on the desktop, so a few dozen copies here take the clipboard away from the
/// whole machine ("Maximum number of clients reached") until Threadknot exits.
///
/// A single long-lived context is also what X selection ownership wants: the
/// owner has to still be running to answer paste requests, which arrive long
/// after the copy call returns.
pub(crate) fn context() -> Result<&'static ClipboardContext, String> {
    static CONTEXT: OnceLock<ClipboardContext> = OnceLock::new();
    static INIT: Mutex<()> = Mutex::new(());

    if let Some(context) = CONTEXT.get() {
        return Ok(context);
    }
    // Hold the lock across the connect so two racing callers cannot each build
    // a context and leak the loser's connections — the very thing above.
    let _guard = INIT.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(context) = CONTEXT.get() {
        return Ok(context);
    }
    let context = ClipboardContext::new()
        .map_err(|error| format!("Couldn't open the system clipboard: {error}"))?;
    Ok(CONTEXT.get_or_init(|| context))
}

/// Put an existing project file on the native file clipboard.
#[tauri::command]
pub async fn copy_project_file(
    state: tauri::State<'_, ClipboardState>,
    project_id: String,
    path: String,
) -> Result<(), String> {
    let project = state
        .store
        .project(&project_id)
        .ok_or_else(|| "Unknown project.".to_string())?;
    let resolved = crate::files::confine(Path::new(&project.path), &path)
        .map_err(|error| error.to_string())?;
    if !resolved.is_file() {
        return Err("The selected path is not a file.".into());
    }
    set_file(resolved).await
}

/// Put the durable artifact snapshot on the native file clipboard. The stored
/// snapshot is UUID-named, so first materialize a hard-link/copy carrying the
/// artifact's original filename. This preserves both the exact viewed bytes
/// and the useful name when the user pastes it elsewhere.
#[tauri::command]
pub async fn copy_artifact_file(
    state: tauri::State<'_, ClipboardState>,
    artifact_id: String,
) -> Result<(), String> {
    let artifact = state
        .store
        .artifact_by_id(&artifact_id)
        .ok_or_else(|| "Unknown artifact.".to_string())?;
    let snapshot = state
        .store
        .artifact_snapshot_path(&artifact.thread_id, &artifact.id)
        .ok_or_else(|| "The artifact snapshot is no longer available.".to_string())?;
    // Not `artifact.name`: for a published artifact that is the agent's display
    // title, which usually carries no extension — pasting it elsewhere would
    // hand the user a file nothing can open. See `artifact_file_name`.
    let file_name = crate::protocol::artifact_file_name(&artifact);
    let path = materialize_artifact(&snapshot, &artifact.id, &file_name)?;
    set_file(path).await
}

fn materialize_artifact(snapshot: &Path, artifact_id: &str, name: &str) -> Result<PathBuf, String> {
    let file_name = Path::new(name)
        .file_name()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| "The artifact has an invalid filename.".to_string())?;
    let parent = snapshot
        .parent()
        .ok_or_else(|| "The artifact snapshot has no parent folder.".to_string())?;
    let dir = parent.join("clipboard").join(artifact_id);
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("Couldn't prepare the artifact for copying: {error}"))?;
    let destination = dir.join(file_name);

    // A hard link is instant even for a large artifact. Fall back to a byte
    // copy on filesystems that do not permit links, or when replacing an old
    // materialized version of an updated artifact.
    let _ = std::fs::remove_file(&destination);
    if std::fs::hard_link(snapshot, &destination).is_err() {
        std::fs::copy(snapshot, &destination)
            .map_err(|error| format!("Couldn't prepare the artifact for copying: {error}"))?;
    }
    Ok(destination)
}

async fn set_file(path: PathBuf) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = path
            .canonicalize()
            .map_err(|error| format!("Couldn't resolve the file: {error}"))?;
        let context = context()?;
        let path_text = path.to_string_lossy().into_owned();

        #[cfg(target_os = "linux")]
        {
            use clipboard_rs::ClipboardContent;

            // GNOME Files uses its custom target to distinguish a copy from a
            // cut. Also publish the standard URI list through `Files` for
            // other Linux desktops and general-purpose applications.
            let uri = url::Url::from_file_path(&path)
                .map_err(|_| "Couldn't convert the file path to a clipboard URI.".to_string())?;
            context
                .set(vec![
                    ClipboardContent::Files(vec![path_text]),
                    ClipboardContent::Other(
                        "x-special/gnome-copied-files".into(),
                        format!("copy\n{uri}").into_bytes(),
                    ),
                ])
                .map_err(|error| format!("Couldn't copy the file: {error}"))
        }

        #[cfg(not(target_os = "linux"))]
        context
            .set_files(vec![path_text])
            .map_err(|error| format!("Couldn't copy the file: {error}"))
    })
    .await
    .map_err(|error| format!("Clipboard worker failed: {error}"))?
}

#[cfg(test)]
mod tests {
    use super::materialize_artifact;

    #[test]
    fn artifact_clipboard_copy_keeps_original_name_and_bytes() {
        let root = std::env::temp_dir().join(format!("threadknot-clipboard-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let snapshot = root.join("artifact-id.pdf");
        std::fs::write(&snapshot, b"snapshot bytes").unwrap();

        let copied = materialize_artifact(&snapshot, "artifact-id", "Quarterly brief.pdf").unwrap();
        assert_eq!(copied.file_name().unwrap(), "Quarterly brief.pdf");
        assert_eq!(std::fs::read(copied).unwrap(), b"snapshot bytes");

        std::fs::remove_dir_all(root).unwrap();
    }
}

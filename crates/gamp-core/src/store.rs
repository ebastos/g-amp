//! Settings and small state files in the app's data folder, each replaced
//! atomically (written beside, then renamed) so a crash never leaves half.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::sync::RwLock;

static DATA: OnceLock<PathBuf> = OnceLock::new();
static DOCUMENTS: OnceLock<PathBuf> = OnceLock::new();

pub fn init(dir: PathBuf, documents: Option<PathBuf>) {
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::create_dir_all(dir.join("skins"));
    let _ = DATA.set(dir);
    if let Some(d) = documents {
        let _ = std::fs::create_dir_all(&d);
        let _ = DOCUMENTS.set(d);
    }
}

/// The user-visible documents folder, where it matters (iOS: the folder
/// the Files app shows for this app).
pub fn documents_dir() -> Option<&'static Path> {
    DOCUMENTS.get().map(PathBuf::as_path)
}

pub fn data_dir() -> &'static Path {
    DATA.get().expect("store::init runs first")
}

pub async fn read_json<T: DeserializeOwned>(name: &str) -> Option<T> {
    let bytes = tokio::fs::read(data_dir().join(name)).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub async fn write_json<T: Serialize>(name: &str, value: &T, secret: bool) -> anyhow::Result<()> {
    let path = data_dir().join(name);
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    tokio::fs::write(&tmp, serde_json::to_vec_pretty(value)?).await?;
    #[cfg(unix)]
    if secret {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).await?;
    }
    #[cfg(not(unix))]
    let _ = secret;
    tokio::fs::rename(&tmp, &path).await?;
    Ok(())
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub folders: Vec<PathBuf>,
    /// Your SoundCloud profile link: its public likes and playlists show.
    pub soundcloud_profile: String,
    /// Its user id, found when it was saved (0: look it up).
    pub soundcloud_user: u64,
    /// Your Dropbox app's key (Dropbox gives every app its own).
    pub dropbox_key: String,
    /// Source ids left out of the library tree (a DLNA server you hid).
    pub hidden_sources: Vec<String>,
}

static CONFIG: OnceLock<RwLock<Config>> = OnceLock::new();

async fn cell() -> &'static RwLock<Config> {
    if let Some(c) = CONFIG.get() {
        return c;
    }
    let mut loaded = read_json::<Config>("config.json").await.unwrap_or_default();
    // On iOS the app can only read its own folder; start with it.
    if cfg!(target_os = "ios") && loaded.folders.is_empty() {
        if let Some(d) = documents_dir() {
            loaded.folders.push(d.to_path_buf());
        }
    }
    let _ = CONFIG.set(RwLock::new(loaded));
    CONFIG.get().unwrap()
}

pub async fn config() -> Config {
    cell().await.read().await.clone()
}

pub async fn save_config(next: Config) -> anyhow::Result<()> {
    write_json("config.json", &next, true).await?;
    *cell().await.write().await = next;
    Ok(())
}

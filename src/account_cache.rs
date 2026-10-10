//! Account-scoped snapshots of what Spotify last showed.
//!
//! A restart shows the last good answer for the verified account at once,
//! while the app asks Spotify again behind it. A snapshot is metadata only;
//! a missing, corrupt, older-format or other account's file is ignored and
//! the data is fetched as if there were none.

use serde::{Deserialize, Serialize, de::DeserializeOwned};

const VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cache<T> {
    version: u32,
    pub account_id: String,
    pub data: T,
}

impl<T> Cache<T> {
    pub fn new(account_id: String, data: T) -> Self {
        Self {
            version: VERSION,
            account_id,
            data,
        }
    }
}

pub async fn read<T: DeserializeOwned>(path: &std::path::Path, account: &str) -> Option<T> {
    let bytes = tokio::fs::read(path).await.ok()?;
    let cache: Cache<T> = serde_json::from_slice(&bytes).ok()?;
    (cache.version == VERSION && cache.account_id == account).then_some(cache.data)
}

/// Replaces the snapshot atomically, so a failed write leaves the previous
/// one readable.
pub async fn write<T: Serialize>(path: &std::path::Path, cache: &Cache<T>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let bytes = serde_json::to_vec(cache).map_err(std::io::Error::other)?;
    let temporary = path.with_extension("json.tmp");
    tokio::fs::write(&temporary, bytes).await?;
    crate::util::replace_file(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::models::{Image, Playlist};

    fn root(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "spotifast-account-cache-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    #[tokio::test]
    async fn a_playlist_list_round_trips_for_its_account_only() {
        let root = root("round-trip");
        let path = root.join("library").join("listener.json");
        let playlists = vec![Playlist {
            id: "one".into(),
            uri: "spotify:playlist:one".into(),
            name: "Morning".into(),
            images: vec![Image {
                url: "https://i.scdn.co/image/one".into(),
                ..Image::default()
            }],
            snapshot_id: Some("snap".into()),
            ..Playlist::default()
        }];
        write(&path, &Cache::new("listener".into(), playlists.clone()))
            .await
            .unwrap();
        assert_eq!(
            read::<Vec<Playlist>>(&path, "listener").await,
            Some(playlists)
        );
        assert_eq!(read::<Vec<Playlist>>(&path, "someone-else").await, None);
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn corrupt_and_other_format_snapshots_are_ignored() {
        let root = root("ignored");
        std::fs::create_dir_all(&root).unwrap();
        let corrupt = root.join("corrupt.json");
        std::fs::write(&corrupt, b"{\"version\":1,\"account_id\":\"listener\",").unwrap();
        let newer = root.join("newer.json");
        std::fs::write(
            &newer,
            br#"{"version":99,"account_id":"listener","data":[]}"#,
        )
        .unwrap();
        assert_eq!(read::<Vec<Playlist>>(&corrupt, "listener").await, None);
        assert_eq!(read::<Vec<Playlist>>(&newer, "listener").await, None);
        assert_eq!(
            read::<Vec<Playlist>>(&root.join("missing.json"), "listener").await,
            None
        );
        let _ = std::fs::remove_dir_all(root);
    }
}

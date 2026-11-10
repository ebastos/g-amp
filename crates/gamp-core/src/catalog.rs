//! Every place music comes from, as one tree for the native library
//! window: the folder library, SoundCloud and Spotify alongside the
//! sources in `sources` (servers, cloud, radio, podcasts, Archive). Each
//! has top-level lists (`roots`), folders that open further lists, and
//! tracks with a `url` on the local server to play them from.

use serde_json::{Value, json};

use crate::http::ApiError;
use crate::sources::{self, Item, Listing, arr, f, s, u};
use crate::{library, spotify};

/// The tree: (id, name, group, roots as (id, title), searchable).
#[derive(Clone, Debug)]
pub struct Source {
    pub id: String,
    pub name: String,
    pub group: String,
    pub roots: Vec<(String, String)>,
    pub search: bool,
    /// Needs connecting in Settings first.
    pub needs_setup: bool,
    /// Left out of the library tree.
    pub hidden: bool,
    /// A saved account the user can remove (not DLNA, cloud folders, or a built-in).
    pub removable: bool,
    /// Kind label from an account (`sources::info`), empty otherwise.
    pub kind: String,
    /// Server address from an account, empty otherwise.
    pub address: String,
}

pub async fn sources() -> Vec<Source> {
    let mut out = vec![Source { id: "local".into(), name: "Local Media".into(), group: "local".into(), roots: vec![("audio".into(), "Audio".into())], search: false, needs_setup: false, hidden: false, removable: false, kind: String::new(), address: String::new() }];
    for v in sources::list().await.as_array().cloned().unwrap_or_default() {
        out.push(Source {
            id: s(&v["id"]),
            name: s(&v["name"]),
            group: s(&v["group"]),
            roots: arr(&v["roots"]).iter().map(|r| (s(&r["id"]), s(&r["title"]))).collect(),
            search: v["search"] == true,
            needs_setup: false,
            hidden: v["hidden"] == true,
            removable: v["removable"] == true,
            kind: s(&v["kindLabel"]),
            address: s(&v["address"]),
        });
    }
    // Dropbox, Google Drive and OneDrive, through their desktop apps' folders.
    for f in crate::cloud::folders() {
        out.push(Source { id: f.id, name: f.name, group: "network".into(), roots: vec![("files".into(), "Files".into())], search: false, needs_setup: false, hidden: false, removable: false, kind: String::new(), address: String::new() });
    }
    let sc = !crate::store::config().await.soundcloud_profile.is_empty();
    out.push(Source {
        id: "soundcloud".into(),
        name: "SoundCloud".into(),
        group: "soundcloud".into(),
        roots: if sc { vec![("likes".into(), "Likes".into()), ("playlists".into(), "Playlists".into())] } else { vec![] },
        // Links and search work without an account.
        search: true,
        needs_setup: !sc,
        hidden: false,
        removable: false,
        kind: String::new(),
        address: String::new(),
    });
    let sp = spotify::connected().await;
    out.push(Source {
        id: "spotify".into(),
        name: "Spotify".into(),
        group: "spotify".into(),
        roots: if sp { vec![("liked".into(), "Liked Songs".into()), ("playlists".into(), "Playlists".into())] } else { vec![] },
        search: sp,
        needs_setup: !sp,
        hidden: false,
        removable: false,
        kind: String::new(),
        address: String::new(),
    });
    out.push(Source {
        id: "skins".into(),
        name: "Skin Museum".into(),
        group: "skins".into(),
        roots: vec![("popular".into(), "Most popular".into())],
        search: true,
        needs_setup: false,
        hidden: false,
        removable: false,
        kind: String::new(),
        address: String::new(),
    });
    out
}

/// Whether a source still answers. Cloud folders are here on disk; every
/// other id is an account, checked through `sources::check`.
pub async fn check(sid: &str) -> Result<(), ApiError> {
    if sid.starts_with("cloud-") {
        return if crate::cloud::folders().iter().any(|f| f.id == sid) {
            Ok(())
        } else {
            Err(crate::http::err(axum::http::StatusCode::NOT_FOUND, "That cloud folder isn't on this computer any more"))
        };
    }
    sources::check(sid).await
}

fn local_items() -> impl std::future::Future<Output = Vec<Item>> {
    async {
        let mut tracks = library::list().await;
        tracks.sort_by(|a, b| a.artist.to_lowercase().cmp(&b.artist.to_lowercase()).then(a.album.cmp(&b.album)).then(a.track_no.cmp(&b.track_no)));
        tracks
            .into_iter()
            .map(|t| {
                let mut i = Item::track(t.id.clone(), t.title).by(t.artist).on(t.album).secs(t.duration.map(f64::from)).no(t.track_no.map(u64::from));
                i.info = t.path.to_string_lossy().into_owned();
                i.url = Some(format!("/media/local/{}", t.id));
                i
            })
            .collect()
    }
}

// ---- Spotify's library (read through librespot, Web API-shaped) -----------

fn sp_list(p: &Value) -> Item {
    Item::folder(format!("playlist:{}", s(&p["id"])), s(&p["name"])).by(s(&p["owner"]["display_name"])).items(u(&p["items"]["total"]))
}

fn sp_track(t: &Value) -> Option<Item> {
    let id = t["id"].as_str()?;
    if t["is_local"] == true || t["type"].as_str().is_some_and(|ty| ty != "track") {
        return None;
    }
    let artists: Vec<String> = arr(&t["artists"]).iter().map(|a| s(&a["name"])).collect();
    let mut i = Item::track(id, s(&t["name"])).by(artists.join(", ")).on(s(&t["album"]["name"])).secs(f(&t["duration_ms"]).map(|ms| (ms / 1000.0).round()));
    i.url = Some(format!("/media/spotify/{id}.wav"));
    Some(i)
}

// ---- the common interface ---------------------------------------------------

pub async fn browse(sid: &str, node: &str) -> Result<Listing, ApiError> {
    match sid {
        "local" => Ok(Listing::of(local_items().await).hint("Add folders in Settings › Music folders.")),
        "favorites" => crate::favorites::browse(node).await,
        "soundcloud" => {
            let (kind, arg) = node.split_once(':').unwrap_or((node, ""));
            // The profile's public lists, read like the web player.
            use crate::soundcloud_web as web;
            let rows = match (kind, arg.strip_prefix("web:")) {
                ("playlist", Some(id)) => web::playlist(id).await?,
                (k @ ("likes" | "playlists"), _) => {
                    let c = crate::store::config().await;
                    let user = if c.soundcloud_user > 0 { c.soundcloud_user } else { web::user_id(&c.soundcloud_profile).await? };
                    if k == "likes" { web::likes(user).await? } else { web::playlists(user).await? }
                }
                _ => return Err(sources::not_found("That list")),
            };
            Ok(Listing::of(rows))
        }
        cloud if cloud.starts_with("cloud-") => {
            let (sid, node) = (sid.to_owned(), node.to_owned());
            tokio::task::spawn_blocking(move || crate::cloud::browse(&sid, &node)).await?
        }
        "spotify" => {
            let (kind, arg) = node.split_once(':').unwrap_or((node, ""));
            let rows: Vec<Item> = match kind {
                "liked" => spotify::liked().await?.iter().filter_map(sp_track).collect(),
                "playlists" => spotify::playlists().await?.iter().map(sp_list).collect(),
                "playlist" => spotify::playlist(arg).await?.iter().filter_map(sp_track).collect(),
                _ => return Err(sources::not_found("That list")),
            };
            Ok(Listing::of(rows))
        }
        "skins" => Ok(Listing::of(crate::skin_museum::popular().await?)),
        _ => sources::browse(sid, node).await,
    }
}

pub async fn search(sid: &str, q: &str) -> Result<Listing, ApiError> {
    // A SoundCloud link plays from any search box, no account needed.
    if crate::soundcloud_web::is_link(q) {
        return Ok(Listing::of(crate::soundcloud_web::resolve(q).await?));
    }
    match sid {
        "soundcloud" => Ok(Listing::of(crate::soundcloud_web::search(q).await?)),
        "skins" => Ok(Listing::of(crate::skin_museum::search(q).await?)),
        "spotify" => {
            // Your own playlists whose names match come first.
            let ql = q.to_lowercase();
            let mut rows: Vec<Item> = spotify::playlists().await.unwrap_or_default().iter().filter(|p| s(&p["name"]).to_lowercase().contains(&ql)).map(sp_list).collect();
            rows.extend(spotify::search(q).await?.iter().filter_map(sp_track));
            Ok(Listing::of(rows))
        }
        "local" => {
            let q = q.to_lowercase();
            Ok(Listing::of(local_items().await.into_iter().filter(|i| format!("{} {} {}", i.artist, i.title, i.album).to_lowercase().contains(&q)).collect()))
        }
        _ => sources::search(sid, q).await,
    }
}

/// For tests and the page: the same as JSON.
pub fn to_json(l: &Listing) -> Value {
    json!(l.items)
}

//! Music beyond the local folders: media servers on your network or the
//! internet (Plex, Jellyfin and Emby, Subsonic servers such as Navidrome,
//! Synology Audio Station, WebDAV and Nextcloud, DLNA), Dropbox, and the
//! free internet (radio, podcasts, the Internet Archive).
//!
//! Every source answers the same three questions, so the library window
//! treats them all alike: what are your top-level lists (`roots`), what is
//! in this list (`browse`), and what matches this text (`search`). A list
//! holds folders, which open further lists, and tracks. A track plays
//! through this server (`/media/remote/<source>/<id>`), which fetches it
//! from the source with byte ranges, so seeking, the equalizer and the
//! visualizers work as they do for local files.

pub mod archive;
pub mod audius;
pub mod dlna;
pub mod dropbox;
pub mod icy;
pub mod jellyfin;
pub mod plex;
pub mod podcasts;
pub mod radio;
pub mod subsonic;
pub mod synology;
pub mod tags;
pub mod webdav;

use std::sync::OnceLock;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;

use crate::http::{ApiError, client, err, insecure_client};
use crate::library::AUDIO_TYPES;
use crate::store;

const ACCOUNTS: &str = "accounts.json";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Subsonic,
    Jellyfin,
    Plex,
    Synology,
    Webdav,
    Dropbox,
    Dlna,
    Radio,
    Podcasts,
    Archive,
    Audius,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Subsonic => "Subsonic",
            Kind::Jellyfin => "Jellyfin",
            Kind::Plex => "Plex",
            Kind::Synology => "Synology",
            Kind::Webdav => "WebDAV",
            Kind::Dropbox => "Dropbox",
            Kind::Dlna => "DLNA",
            Kind::Radio => "Internet Radio",
            Kind::Podcasts => "Podcasts",
            Kind::Archive => "Internet Archive",
            Kind::Audius => "Audius",
        }
    }
}

/// A server or service you added. Passwords and tokens stay in this file
/// (readable by you alone) and never go back to the page.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub kind: Kind,
    pub name: String,
    /// The server's address, without a trailing slash.
    pub url: String,
    /// Other addresses for the same server, tried when `url` stops
    /// answering (Plex: at home and away).
    pub alt_urls: Vec<String>,
    pub username: String,
    pub password: String,
    /// What the server gave back at sign-in (Jellyfin and Plex access
    /// tokens, Dropbox's refresh token, Synology's trusted-device id).
    pub token: String,
    pub user_id: String,
    /// Accept a self-signed certificate (many NAS boxes have one).
    pub insecure: bool,
}

impl Account {
    pub fn http(&self) -> &'static reqwest::Client {
        if self.insecure { insecure_client() } else { client() }
    }

    pub fn base(&self) -> &str {
        self.url.trim_end_matches('/')
    }

    fn builtin(kind: Kind) -> Account {
        let id = match kind {
            Kind::Radio => "radio",
            Kind::Podcasts => "podcasts",
            Kind::Audius => "audius",
            _ => "archive",
        };
        Account { id: id.into(), kind, name: kind.label().into(), ..Default::default() }
    }
}

#[derive(Serialize, Clone)]
pub struct Root {
    pub id: &'static str,
    pub title: &'static str,
}

/// One row in a list: a folder (album, playlist, directory, station
/// genre...) that opens another list, or a track.
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Item {
    pub kind: String,
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_no: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
    /// Shown as the row's tooltip (file type, folder, date...).
    pub info: String,
    /// Where the page plays it from; filled in for tracks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Endless (radio): the player shows no length.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub live: bool,
    /// When it was posted, "2024-05-01" (SoundCloud).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub date: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plays: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub likes: Option<u64>,
    /// The uploader's page, for "Latest from…" (SoundCloud).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub artist_link: String,
}

impl Item {
    pub fn folder(id: impl Into<String>, title: impl Into<String>) -> Item {
        Item { kind: "folder".into(), id: id.into(), title: title.into(), ..Default::default() }
    }

    pub fn track(id: impl Into<String>, title: impl Into<String>) -> Item {
        Item { kind: "track".into(), id: id.into(), title: title.into(), ..Default::default() }
    }

    pub fn by(mut self, artist: impl Into<String>) -> Item {
        self.artist = artist.into();
        self
    }

    pub fn on(mut self, album: impl Into<String>) -> Item {
        self.album = album.into();
        self
    }

    pub fn secs(mut self, d: Option<f64>) -> Item {
        self.duration = d.filter(|d| *d > 0.0);
        self
    }

    pub fn no(mut self, n: Option<u64>) -> Item {
        self.track_no = n.map(|n| n as u32).filter(|n| *n > 0);
        self
    }

    pub fn items(mut self, n: Option<u64>) -> Item {
        self.count = n;
        self
    }

    pub fn info(mut self, s: impl Into<String>) -> Item {
        self.info = s.into();
        self
    }
}

#[derive(Serialize, Clone)]
pub struct Action {
    pub id: &'static str,
    pub label: &'static str,
}

#[derive(Serialize, Default)]
pub struct Listing {
    pub items: Vec<Item>,
    /// Buttons for the selected rows (Add to Favorites, Subscribe...).
    pub actions: Vec<Action>,
    /// Said when the list is empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Listing {
    pub fn of(items: Vec<Item>) -> Listing {
        Listing { items, ..Default::default() }
    }

    pub fn with(mut self, action: &'static str, label: &'static str) -> Listing {
        self.actions.push(Action { id: action, label });
        self
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Listing {
        self.hint = Some(hint.into());
        self
    }
}

/// Where a track's bytes come from.
pub struct Upstream {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub insecure: bool,
    /// An endless radio stream (maybe from an old SHOUTcast server).
    pub live: bool,
}

impl Upstream {
    pub fn get(url: impl Into<String>) -> Upstream {
        Upstream { url: url.into(), headers: Vec::new(), insecure: false, live: false }
    }

    pub fn header(mut self, k: &str, v: impl Into<String>) -> Upstream {
        self.headers.push((k.to_owned(), v.into()));
        self
    }
}

// ---- helpers for the source modules ----------------------------------

pub fn bad_gateway(msg: impl Into<String>) -> ApiError {
    err(StatusCode::BAD_GATEWAY, msg)
}

/// A network failure, said plainly.
pub fn unreachable(name: &str, e: reqwest::Error) -> ApiError {
    let why = if e.is_timeout() {
        "it took too long to answer".to_owned()
    } else if e.is_connect() {
        "nothing answered at that address".to_owned()
    } else {
        let mut s = e.to_string();
        let mut src = std::error::Error::source(&e);
        while let Some(inner) = src {
            s = format!("{s}: {inner}");
            src = inner.source();
        }
        s
    };
    bad_gateway(format!("Can't reach {name}: {why}"))
}

pub fn not_found(what: &str) -> ApiError {
    err(StatusCode::NOT_FOUND, format!("{what} isn't there"))
}

pub fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

pub fn f(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

pub fn u(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str().and_then(|s| s.split('/').next()?.trim().parse().ok()))
}

pub fn arr(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}

pub fn is_audio(name: &str) -> bool {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    AUDIO_TYPES.iter().any(|(e, _)| *e == ext)
}

/// "Song.flac" → "Song".
pub fn stem(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((s, e)) if e.len() <= 4 && !s.is_empty() => s.to_owned(),
        _ => name.to_owned(),
    }
}

/// "1:02:03", "62:03", "3723" or "3723.5" → seconds.
pub fn clock(text: &str) -> Option<f64> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let mut total = 0.0;
    for part in t.split(':') {
        total = total * 60.0 + part.trim().parse::<f64>().ok()?;
    }
    Some(total)
}

pub fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

// ---- accounts ---------------------------------------------------------

fn accounts_lock() -> &'static AsyncMutex<()> {
    static L: OnceLock<AsyncMutex<()>> = OnceLock::new();
    L.get_or_init(|| AsyncMutex::new(()))
}

pub async fn accounts() -> Vec<Account> {
    store::read_json::<Vec<Account>>(ACCOUNTS).await.unwrap_or_default()
}

/// Adds or replaces (same id) an account.
pub async fn save_account(acc: Account) -> Result<(), ApiError> {
    let _g = accounts_lock().lock().await;
    let mut all = accounts().await;
    all.retain(|a| a.id != acc.id);
    all.push(acc);
    store::write_json(ACCOUNTS, &all, true).await?;
    Ok(())
}

pub async fn remove_account(id: &str) -> Result<(), ApiError> {
    let _g = accounts_lock().lock().await;
    let mut all = accounts().await;
    all.retain(|a| a.id != id);
    store::write_json(ACCOUNTS, &all, true).await?;
    Ok(())
}

pub async fn find(id: &str) -> Result<Account, ApiError> {
    match id {
        "radio" => return Ok(Account::builtin(Kind::Radio)),
        "podcasts" => return Ok(Account::builtin(Kind::Podcasts)),
        "archive" => return Ok(Account::builtin(Kind::Archive)),
        "audius" => return Ok(Account::builtin(Kind::Audius)),
        _ => {}
    }
    if let Some(a) = dlna::find(id) {
        return Ok(a);
    }
    accounts().await.into_iter().find(|a| a.id == id).ok_or_else(|| not_found("That source"))
}

/// What you type in Settings to add a server.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewAccount {
    pub kind: Kind,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    /// Synology's two-step code, when the account has one.
    #[serde(default)]
    pub otp: String,
    #[serde(default)]
    pub insecure: bool,
}

/// "nas.local:4533" → "http://nas.local:4533"; keeps a path (Nextcloud's
/// /remote.php/dav/files/you).
pub fn normalize_url(raw: &str) -> Result<String, ApiError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "Type the server's address"));
    }
    let with_scheme = if raw.contains("://") { raw.to_owned() } else { format!("http://{raw}") };
    let url = url::Url::parse(&with_scheme).map_err(|e| err(StatusCode::BAD_REQUEST, format!("That address doesn't look right: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(err(StatusCode::BAD_REQUEST, "Use an http:// or https:// address"));
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// Signs in to check the details, then keeps the account.
pub async fn add(new: NewAccount) -> Result<Value, ApiError> {
    let mut acc = Account {
        id: format!("{}-{}", serde_json::to_value(new.kind)?.as_str().unwrap_or("src"), random_hex(4)),
        kind: new.kind,
        name: new.name.trim().to_owned(),
        url: normalize_url(&new.url)?,
        username: new.username.trim().to_owned(),
        password: new.password,
        insecure: new.insecure,
        ..Default::default()
    };
    let named = !acc.name.is_empty();
    if !named {
        acc.name = url::Url::parse(&acc.url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_else(|| acc.kind.label().into());
    }
    match acc.kind {
        Kind::Subsonic => subsonic::connect(&mut acc).await?,
        Kind::Jellyfin => jellyfin::connect(&mut acc, !named).await?,
        Kind::Plex => plex::connect_manual(&mut acc).await?,
        Kind::Synology => synology::connect(&mut acc, new.otp.trim()).await?,
        Kind::Webdav => webdav::connect(&mut acc).await?,
        _ => return Err(err(StatusCode::BAD_REQUEST, "That kind of source is added another way")),
    }
    let info = info(&acc, false);
    save_account(acc).await?;
    Ok(info)
}

/// The saved account's display name. A blank name is refused; a source that
/// isn't a saved account (DLNA, radio) isn't there to rename.
pub async fn rename(id: &str, name: &str) -> Result<Value, ApiError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "Type a name"));
    }
    let mut acc = accounts().await.into_iter().find(|a| a.id == id).ok_or_else(|| not_found("That source"))?;
    acc.name = name.to_owned();
    let hidden = store::config().await.hidden_sources.iter().any(|h| h == &acc.id);
    let body = info(&acc, hidden);
    save_account(acc).await?;
    Ok(body)
}

// ---- what the page sees ------------------------------------------------

pub fn roots(kind: Kind) -> Vec<Root> {
    let r = |id, title| Root { id, title };
    match kind {
        Kind::Subsonic => vec![r("starred", "Starred"), r("albums", "Recently Added"), r("artists", "Artists"), r("playlists", "Playlists")],
        Kind::Jellyfin => vec![r("favorites", "Favorites"), r("albums", "Recently Added"), r("artists", "Artists"), r("playlists", "Playlists")],
        Kind::Plex => vec![r("albums", "Recently Added"), r("artists", "Artists"), r("playlists", "Playlists")],
        Kind::Synology => vec![r("songs", "All Songs"), r("albums", "Albums"), r("folders", "Folders"), r("playlists", "Playlists")],
        Kind::Webdav | Kind::Dropbox => vec![r("files", "Files")],
        Kind::Dlna => vec![r("c:0", "Browse")],
        Kind::Radio => vec![r("favorites", "Favorites"), r("top", "Top Stations"), r("genres", "Genres"), r("countries", "Countries")],
        Kind::Podcasts => vec![r("subscriptions", "Subscriptions"), r("top", "Top Podcasts")],
        Kind::Archive => vec![
            r("c:etree", "Live Music Archive"),
            r("c:georgeblood", "78 RPM Records"),
            r("c:netlabels", "Netlabels"),
            r("c:oldtimeradio", "Old Time Radio"),
        ],
        Kind::Audius => vec![r("trending", "Trending"), r("underground", "Underground Trending")],
    }
}

fn searchable(kind: Kind) -> bool {
    !matches!(kind, Kind::Webdav | Kind::Dlna)
}

fn info(acc: &Account, hidden: bool) -> Value {
    let group = match acc.kind {
        Kind::Radio | Kind::Podcasts | Kind::Archive | Kind::Audius => "internet",
        _ => "network",
    };
    json!({
        "id": acc.id,
        "kind": acc.kind,
        "kindLabel": acc.kind.label(),
        "name": acc.name,
        "address": if acc.kind == Kind::Dropbox { acc.username.clone() } else { acc.url.clone() },
        "group": group,
        "removable": !matches!(acc.kind, Kind::Radio | Kind::Podcasts | Kind::Archive | Kind::Audius | Kind::Dlna),
        "hidden": hidden,
        "roots": roots(acc.kind),
        "search": searchable(acc.kind),
    })
}

pub async fn list() -> Value {
    dlna::refresh_soon();
    let hidden = store::config().await.hidden_sources;
    let mark = |acc: &Account| info(acc, hidden.iter().any(|h| h == &acc.id));
    let mut out: Vec<Value> = accounts().await.iter().map(mark).collect();
    out.extend(dlna::servers().iter().map(mark));
    for k in [Kind::Radio, Kind::Podcasts, Kind::Archive, Kind::Audius] {
        out.push(mark(&Account::builtin(k)));
    }
    Value::Array(out)
}

// ---- media URLs ---------------------------------------------------------

/// Track URLs carry a signature so that the server fetches only what it
/// listed itself: a web page can't use it to reach into your network.
fn key() -> &'static [u8] {
    static K: OnceLock<Vec<u8>> = OnceLock::new();
    K.get_or_init(|| {
        let path = store::data_dir().join("media.key");
        if let Ok(k) = std::fs::read(&path) {
            if k.len() == 32 {
                return k;
            }
        }
        let mut k = vec![0u8; 32];
        rand::thread_rng().fill_bytes(&mut k);
        let _ = std::fs::write(&path, &k);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        k
    })
}

pub fn sign(sid: &str, id: &str) -> String {
    let mut h = Sha256::new();
    h.update(key());
    h.update(sid.as_bytes());
    h.update([0]);
    h.update(id.as_bytes());
    hex::encode(&h.finalize()[..16])
}

fn media_url(sid: &str, item: &Item) -> String {
    // A file name at the end helps the player guess the format.
    let name = item.id.split(['?', '#']).next().unwrap_or("");
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).filter(|e| is_audio(&format!("x.{e}")));
    format!(
        "/media/remote/{}/{}?s={}{}",
        urlencoding::encode(sid),
        urlencoding::encode(&item.id),
        sign(sid, &item.id),
        ext.map(|e| format!("&f=.{e}")).unwrap_or_default()
    )
}

/// A plain stream address (a station, or a file on the web) as a signed
/// local media link, the way the library lists it: `/media/remote/...`.
/// Addresses ending in an audio extension are seekable; others play live.
pub fn link_media_url(url: &str) -> (String, bool) {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let file = is_audio(path);
    let sid = if file { "podcasts" } else { "radio" };
    (media_url(sid, &Item::track(url, "")), !file)
}

/// The `/media/...` part of a link to this app's own server (any port),
/// if that's what it is.
pub fn own_media_path(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    let local = matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    if !local || !u.path().starts_with("/media/") {
        return None;
    }
    Some(match u.query() {
        Some(q) => format!("{}?{q}", u.path()),
        None => u.path().to_owned(),
    })
}

fn finish(sid: &str, mut listing: Listing) -> Listing {
    for item in &mut listing.items {
        if item.kind == "track" {
            item.url = Some(media_url(sid, item));
        }
    }
    listing
}

pub async fn browse(sid: &str, node: &str) -> Result<Listing, ApiError> {
    let acc = find(sid).await?;
    let listing = match acc.kind {
        Kind::Subsonic => subsonic::browse(&acc, node).await?,
        Kind::Jellyfin => jellyfin::browse(&acc, node).await?,
        Kind::Plex => plex::browse(&acc, node).await?,
        Kind::Synology => synology::browse(&acc, node).await?,
        Kind::Webdav | Kind::Dropbox => {
            let mut l = if acc.kind == Kind::Webdav { webdav::browse(&acc, node).await? } else { dropbox::browse(&acc, node).await? };
            // File storage lists names only; read the tags from the files.
            let a = &acc;
            tags::enrich(sid, &mut l.items, |id| async move { upstream(a, &id).await.ok() }).await;
            l
        }
        Kind::Dlna => dlna::browse(&acc, node).await?,
        Kind::Radio => radio::browse(node).await?,
        Kind::Podcasts => podcasts::browse(node).await?,
        Kind::Archive => archive::browse(node).await?,
        Kind::Audius => audius::browse(node).await?,
    };
    Ok(finish(sid, listing))
}

pub async fn search(sid: &str, q: &str) -> Result<Listing, ApiError> {
    let q = q.trim();
    if q.is_empty() {
        return Ok(Listing::default());
    }
    let acc = find(sid).await?;
    let listing = match acc.kind {
        Kind::Subsonic => subsonic::search(&acc, q).await?,
        Kind::Jellyfin => jellyfin::search(&acc, q).await?,
        Kind::Plex => plex::search(&acc, q).await?,
        Kind::Synology => synology::search(&acc, q).await?,
        Kind::Dropbox => {
            let mut l = dropbox::search(&acc, q).await?;
            let a = &acc;
            tags::enrich(sid, &mut l.items, |id| async move { upstream(a, &id).await.ok() }).await;
            l
        }
        Kind::Radio => radio::search(q).await?,
        Kind::Podcasts => podcasts::search(q).await?,
        Kind::Archive => archive::search(q).await?,
        Kind::Audius => audius::search(q).await?,
        Kind::Webdav | Kind::Dlna => Listing::default(),
    };
    Ok(finish(sid, listing))
}

pub async fn action(sid: &str, action: &str, items: Vec<Item>) -> Result<Value, ApiError> {
    if items.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "Select something first"));
    }
    let acc = find(sid).await?;
    let message = match acc.kind {
        Kind::Radio => radio::action(action, items).await?,
        Kind::Podcasts => podcasts::action(action, items).await?,
        _ => return Err(err(StatusCode::BAD_REQUEST, "Nothing to do here")),
    };
    Ok(json!({ "message": message }))
}

async fn upstream(acc: &Account, id: &str) -> Result<Upstream, ApiError> {
    let mut up = match acc.kind {
        Kind::Subsonic => subsonic::media(acc, id),
        Kind::Jellyfin => jellyfin::media(acc, id),
        Kind::Plex => plex::media(acc, id),
        Kind::Synology => synology::media(acc, id).await?,
        Kind::Webdav => webdav::media(acc, id),
        Kind::Dropbox => dropbox::media(acc, id).await?,
        Kind::Dlna | Kind::Podcasts | Kind::Archive | Kind::Audius => Upstream::get(id),
        Kind::Radio => Upstream { live: true, ..Upstream::get(id) },
    };
    up.insecure |= acc.insecure;
    Ok(up)
}

pub async fn media(sid: &str, id: &str, sig: &str, headers: &HeaderMap) -> Result<Response, ApiError> {
    if sign(sid, id) != sig {
        return Err(err(StatusCode::FORBIDDEN, "Not a track this app listed"));
    }
    let acc = find(sid).await?;
    let up = upstream(&acc, id).await?;
    proxy(&acc.name, up, headers).await
}

// ---- the stream proxy ---------------------------------------------------

/// Browsers don't know SHOUTcast's codec name for AAC+.
fn fix_type(t: &str, url: &str) -> String {
    let t = t.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    match t.as_str() {
        "audio/aacp" | "audio/x-aac" | "audio/aac" => "audio/aac".into(),
        "audio/mp3" | "audio/x-mp3" | "audio/mpeg3" => "audio/mpeg".into(),
        "" | "application/octet-stream" | "binary/octet-stream" => {
            let path = url.split(['?', '#']).next().unwrap_or("");
            crate::library::mime_for(std::path::Path::new(path)).replace("application/octet-stream", "audio/mpeg")
        }
        _ => t,
    }
}

pub async fn proxy(name: &str, up: Upstream, incoming: &HeaderMap) -> Result<Response, ApiError> {
    let http = if up.insecure { insecure_client() } else { client() };
    let mut req = http.get(&up.url);
    for (k, v) in &up.headers {
        req = req.header(k, v);
    }
    if let Some(r) = incoming.get(header::RANGE).filter(|_| !up.live) {
        req = req.header(header::RANGE, r);
    }
    if up.live {
        req = req.header("Icy-MetaData", "1");
    }
    let res = match req.send().await {
        Ok(r) => r,
        // Old SHOUTcast servers answer "ICY 200 OK", which isn't HTTP.
        Err(e) if up.live && up.url.starts_with("http://") && !e.is_connect() && !e.is_timeout() => return icy(&up.url).await,
        Err(e) => return Err(unreachable(name, e)),
    };
    let status = res.status();
    if !(status.is_success() || status == StatusCode::RANGE_NOT_SATISFIABLE) {
        let why = match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => "it refused (sign in again in Settings)".to_owned(),
            StatusCode::NOT_FOUND => "the track is gone".to_owned(),
            s => format!("it answered {s}"),
        };
        return Err(bad_gateway(format!("{name} couldn't play this: {why}")));
    }
    let h = res.headers().clone();
    let mut out = Response::builder().status(status);
    let ctype = h.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    out = out.header(header::CONTENT_TYPE, fix_type(ctype, &up.url)).header(header::CACHE_CONTROL, "no-store");
    for k in [header::CONTENT_LENGTH, header::CONTENT_RANGE, header::ACCEPT_RANGES] {
        if let Some(v) = h.get(&k) {
            if !(up.live && k != header::CONTENT_LENGTH) {
                out = out.header(k, v.clone());
            }
        }
    }
    if !up.live && !h.contains_key(header::ACCEPT_RANGES) && status == StatusCode::PARTIAL_CONTENT {
        out = out.header(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    }
    let every = h.get("icy-metaint").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<usize>().ok()).filter(|n| *n > 0);
    match every {
        Some(every) if up.live => Ok(out.body(Body::from_stream(icy::strip(res.bytes_stream(), every, up.url.clone())))?),
        _ => Ok(out.body(Body::from_stream(res.bytes_stream()))?),
    }
}

/// A SHOUTcast v1 stream, read over a plain socket.
async fn icy(url: &str) -> Result<Response, ApiError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let u = url::Url::parse(url)?;
    let host = u.host_str().ok_or_else(|| bad_gateway("No host in the stream address"))?.to_owned();
    let port = u.port_or_known_default().unwrap_or(80);
    let mut path = u.path().to_owned();
    if let Some(q) = u.query() {
        path = format!("{path}?{q}");
    }
    let mut sock = tokio::time::timeout(std::time::Duration::from_secs(15), tokio::net::TcpStream::connect((host.as_str(), port)))
        .await
        .map_err(|_| bad_gateway("The station took too long to answer"))?
        .map_err(|e| bad_gateway(format!("Can't reach the station: {e}")))?;
    sock.write_all(format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nUser-Agent: Gamp/0.1\r\nAccept: */*\r\nIcy-MetaData: 1\r\n\r\n").as_bytes()).await?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && !head.ends_with(b"\n\n") {
        if head.len() > 16 * 1024 || sock.read(&mut byte).await? == 0 {
            return Err(bad_gateway("The station sent no audio"));
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).to_string();
    if !head.lines().next().unwrap_or("").contains(" 200") {
        return Err(bad_gateway(format!("The station said: {}", head.lines().next().unwrap_or("nothing"))));
    }
    let field = |name: &str| {
        head.lines()
            .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_owned()))
            .unwrap_or_default()
    };
    let ctype = field("content-type");
    let every = field("icy-metaint").parse::<usize>().ok().filter(|n| *n > 0);
    let audio = tokio_util::io::ReaderStream::new(sock);
    let body = match every {
        Some(every) => Body::from_stream(icy::strip(audio, every, url.to_owned())),
        None => Body::from_stream(audio),
    };
    Ok(Response::builder().header(header::CONTENT_TYPE, fix_type(&ctype, url)).header(header::CACHE_CONTROL, "no-store").body(body)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks() {
        assert_eq!(clock("1:02:03"), Some(3723.0));
        assert_eq!(clock("62:03"), Some(3723.0));
        assert_eq!(clock("3723"), Some(3723.0));
        assert_eq!(clock(""), None);
        assert_eq!(clock("abc"), None);
    }

    #[test]
    fn addresses() {
        assert_eq!(normalize_url("nas.local:4533/").unwrap(), "http://nas.local:4533");
        assert_eq!(normalize_url("https://cloud.example.com/remote.php/dav/files/me/").unwrap(), "https://cloud.example.com/remote.php/dav/files/me");
        assert!(normalize_url("ftp://x").is_err());
        assert!(normalize_url(" ").is_err());
    }

    #[test]
    fn links() {
        store::init(std::env::temp_dir().join(format!("gamp-core-links-{}", std::process::id())), None);
        let (u, live) = link_media_url("http://ice.somafm.com/groovesalad");
        assert!(u.starts_with("/media/remote/radio/") && u.contains("?s=") && live);
        let (u, live) = link_media_url("https://example.com/a%20song.mp3?x=1");
        assert!(u.starts_with("/media/remote/podcasts/") && u.ends_with("&f=.mp3") && !live);
        assert_eq!(own_media_path("http://127.0.0.1:8742/media/local/42?s=abc").as_deref(), Some("/media/local/42?s=abc"));
        assert_eq!(own_media_path("http://localhost:9000/media/spotify/x.ogg").as_deref(), Some("/media/spotify/x.ogg"));
        assert_eq!(own_media_path("http://127.0.0.1:8742/api/x"), None);
        assert_eq!(own_media_path("http://example.com/media/x"), None);
    }

    #[tokio::test]
    async fn rename_updates_a_saved_account_and_refuses_a_blank_name() {
        store::init(std::env::temp_dir().join(format!("gamp-rename-{}", std::process::id())), None);
        let id = format!("plex-{}", random_hex(4));
        save_account(Account { id: id.clone(), kind: Kind::Plex, name: "Old Name".into(), url: "http://nas:32400".into(), ..Default::default() }).await.unwrap();
        assert!(rename(&id, "  ").await.is_err());
        let still = accounts().await.into_iter().find(|a| a.id == id).unwrap();
        assert_eq!(still.name, "Old Name");
        let v = rename(&id, "  Living Room ").await.unwrap();
        assert_eq!(v["name"], "Living Room");
        assert_eq!(v["hidden"], false);
        assert_eq!(v["removable"], true);
        assert!(rename("no-such-source", "X").await.is_err());
        let saved = accounts().await.into_iter().find(|a| a.id == id).unwrap();
        assert_eq!(saved.name, "Living Room");
        remove_account(&id).await.unwrap();
    }

    #[test]
    fn types() {
        assert_eq!(fix_type("audio/aacp", ""), "audio/aac");
        assert_eq!(fix_type("application/octet-stream", "http://x/a.flac?y=1"), "audio/flac");
        assert_eq!(fix_type("audio/mpeg; charset=x", ""), "audio/mpeg");
        assert_eq!(stem("Song.flac"), "Song");
        assert!(is_audio("a.MP3") && !is_audio("cover.jpg"));
    }
}

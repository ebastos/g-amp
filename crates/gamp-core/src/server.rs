//! The local server the Winamp page runs on. It listens on 127.0.0.1 only:
//! it can read every folder you add and holds your sign-ins.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axum::Router;
use axum::body::Body;
use axum::extract::{Path as UrlPath, Query, Request};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{delete, get, post};
use axum::Json;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::http::{ApiError, err, parse_range};
use crate::sources::{self, Item, NewAccount};
use crate::{library, soundcloud, spotify, store};

pub const HOST: &str = "127.0.0.1";

fn origin(port: u16) -> String {
    format!("http://{HOST}:{port}")
}

/// The API and media routes; `page` serves everything else (the player page).
pub fn router(port: u16, page: Router) -> Router {
    let o = origin(port);
    let dbx = format!("{o}/callback/dropbox");
    let dbx2 = dbx.clone();
    Router::new()
        .route("/api/config", get(move || get_config(dbx.clone())).post(move |b| post_config(b, dbx2.clone())))
        .route("/api/browse", get(browse))
        .route("/api/library", get(library_list))
        .route("/api/library/status", get(|| async { Json(library::status().await) }))
        .route("/api/library/rescan", post(|| async { Json(library::rescan().await) }))
        .route("/api/skins", get(skins).post(add_skin).layer(axum::extract::DefaultBodyLimit::max(30 * 1024 * 1024)))
        .route("/skins/{file}", get(skin_file))
        .route("/media/local/{id}", get(media_local))
        .route("/media/soundcloud/{file}", get(media_soundcloud))
        .route("/media/spotify/{file}", get(media_spotify))
        .route("/media/cloud/{sid}", get(media_cloud))
        .route("/api/sources", get(|| async { Json(sources::list().await) }))
        .route("/api/sources/discover", post(|| async {
            sources::dlna::refresh(true);
            sources::lan::refresh(true);
            StatusCode::NO_CONTENT
        }))
        .route("/api/radio/now", get(|Query(q): Query<HashMap<String, String>>| async move {
            Json(json!({ "title": q.get("id").and_then(|id| sources::icy::now_playing(id)) }))
        }))
        .route("/api/sources/{sid}/browse", get(|UrlPath(sid): UrlPath<String>, Query(q): Query<HashMap<String, String>>| async move {
            sources::browse(&sid, q.get("node").map(String::as_str).unwrap_or_default()).await.map(Json)
        }))
        .route("/api/sources/{sid}/search", get(|UrlPath(sid): UrlPath<String>, Query(q): Query<HashMap<String, String>>| async move {
            sources::search(&sid, q.get("q").map(String::as_str).unwrap_or_default()).await.map(Json)
        }))
        .route("/api/sources/{sid}/action", post(|UrlPath(sid): UrlPath<String>, Json(b): Json<ActionBody>| async move {
            sources::action(&sid, &b.action, b.items).await.map(Json)
        }))
        .route("/api/accounts", post(|Json(new): Json<NewAccount>| async move { sources::add(new).await.map(Json) }))
        .route("/api/accounts/{id}", delete(|UrlPath(id): UrlPath<String>| async move {
            sources::remove_account(&id).await.map(|_| StatusCode::NO_CONTENT)
        }).post(|UrlPath(id): UrlPath<String>, Json(body): Json<AccountName>| async move {
            sources::rename(&id, &body.name).await.map(Json)
        }))
        .route("/media/remote/{sid}/{id}", get(|UrlPath((sid, id)): UrlPath<(String, String)>, Query(q): Query<HashMap<String, String>>, headers: HeaderMap| async move {
            sources::media(&sid, &id, q.get("s").map(String::as_str).unwrap_or_default(), &headers).await
        }))
        .route("/auth/dropbox/login", get(move || async move {
            match sources::dropbox::login_url(&format!("{}/callback/dropbox", origin(port))).await {
                Ok(url) => Redirect::to(&url),
                Err(e) => Redirect::to(&crate::shell::after_sign_in("Dropbox", Err(e.1))),
            }
        }))
        .route("/callback/dropbox", get(|Query(q): Query<HashMap<String, String>>| async move {
            let result = match q.get("error") {
                Some(e) => Err(q.get("error_description").unwrap_or(e).clone()),
                None => sources::dropbox::finish_login(q.get("code").map(String::as_str).unwrap_or(""), q.get("state").map(String::as_str).unwrap_or(""))
                    .await
                    .map_err(|e| e.1),
            };
            Redirect::to(&crate::shell::after_sign_in("Dropbox", result))
        }))
        .route("/auth/plex/login", get(move || async move {
            match sources::plex::login_url(&format!("{}/callback/plex", origin(port))).await {
                Ok(url) => Redirect::to(&url),
                Err(e) => Redirect::to(&crate::shell::after_sign_in("Plex", Err(e.1))),
            }
        }))
        .route("/callback/plex", get(|Query(q): Query<HashMap<String, String>>| async move {
            let result = sources::plex::finish_login(q.get("state").map(String::as_str).unwrap_or("")).await.map(|_| ()).map_err(|e| e.1);
            Redirect::to(&crate::shell::after_sign_in("Plex", result))
        }))
        .route("/api/mount", post(mount))
        .route("/auth/spotify/login", get(spotify_login))
        .route("/api/spotify/logout", post(|| async { spotify::logout().await.map(|_| Json(json!({"ok": true}))).map_err(ApiError::from) }))
        .route("/media/test-tone.wav", get(test_tone))
        .route("/api/overlay", get(|| async {
            let h = crate::shell::hit().lock().unwrap().clone();
            Json(json!({ "rects": h.rects, "pressed": h.pressed }))
        }).post(|Json(v): Json<Value>| async move {
            let rects = v["rects"].as_array().map(|a| {
                a.iter()
                    .filter_map(|r| {
                        let r = r.as_array()?;
                        Some([r.first()?.as_f64()?, r.get(1)?.as_f64()?, r.get(2)?.as_f64()?, r.get(3)?.as_f64()?])
                    })
                    .collect()
            });
            let mut hit = crate::shell::hit().lock().unwrap();
            if let Some(rects) = rects {
                hit.rects = rects;
            }
            if let Some(p) = v["pressed"].as_bool() {
                hit.pressed = p;
            }
            StatusCode::NO_CONTENT
        }))
        .route("/api/app/{action}", post(|UrlPath(action): UrlPath<String>, body: axum::body::Bytes| async move {
            let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            if crate::shell::control(&action, &body) { StatusCode::NO_CONTENT } else { StatusCode::NOT_FOUND }
        }))
        .route("/api/diagnostics", post(|Json(v): Json<Value>| async move {
            log::info!("page report: {v}");
            StatusCode::NO_CONTENT
        }))
        .fallback_service(page)
        .layer(middleware::from_fn(move |req: Request, next: Next| {
            let o = o.clone();
            async move { same_origin_only(o, req, next).await }
        }))
}

/// A web page you happen to visit must not drive this server through your
/// browser, so requests that name another origin are refused.
async fn same_origin_only(own: String, req: Request, next: Next) -> Response {
    if let Some(o) = req.headers().get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if o != own && o != "null" {
            return err(StatusCode::FORBIDDEN, "Forbidden origin").into_response();
        }
    }
    next.run(req).await
}

// ---- config -----------------------------------------------------------

#[derive(Deserialize)]
struct ActionBody {
    action: String,
    #[serde(default)]
    items: Vec<Item>,
}

async fn public_config(dropbox_redirect: String) -> Value {
    let c = store::config().await;
    json!({
        "platform": std::env::consts::OS,
        "shell": crate::shell::shell(),
        "folders": c.folders,
        "spotify": { "connected": spotify::connected().await },
        "soundcloud": { "profile": c.soundcloud_profile },
        "cloud": crate::cloud::folders().iter().map(|f| json!({ "name": f.name, "path": f.path })).collect::<Vec<_>>(),
        "dropbox": { "key": c.dropbox_key, "builtIn": !sources::dropbox::APP_KEY.is_empty(), "redirectUri": dropbox_redirect },
        "dlnaSearching": sources::dlna::searching(),
        "hiddenSources": c.hidden_sources,
        "found": sources::lan::found().await,
        "lanSearching": sources::lan::searching(),
    })
}

async fn get_config(dropbox_redirect: String) -> Json<Value> {
    Json(public_config(dropbox_redirect).await)
}

#[derive(Deserialize)]
struct AccountName {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfigPatch {
    folders: Option<Vec<String>>,
    soundcloud_profile: Option<String>,
    dropbox_key: Option<String>,
    hidden_sources: Option<Vec<String>>,
}

async fn post_config(Json(patch): Json<ConfigPatch>, dropbox_redirect: String) -> Result<Json<Value>, ApiError> {
    let mut c = store::config().await;
    if let Some(folders) = patch.folders {
        let mut out: Vec<PathBuf> = Vec::new();
        for f in folders {
            let p = expand_home(f.trim());
            if !out.contains(&p) {
                out.push(p);
            }
        }
        c.folders = out;
    }
    if let Some(p) = patch.soundcloud_profile {
        let p = p.trim().to_owned();
        c.soundcloud_user = if p.is_empty() { 0 } else { crate::soundcloud_web::user_id(&p).await? };
        c.soundcloud_profile = p;
    }
    if let Some(k) = patch.dropbox_key {
        c.dropbox_key = k.trim().to_owned();
    }
    if let Some(hidden) = patch.hidden_sources {
        c.hidden_sources = hidden;
    }
    store::save_config(c).await?;
    Ok(Json(public_config(dropbox_redirect).await))
}

fn home() -> PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default()
}

fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') => home().join(rest.trim_start_matches(['/', '\\'])),
        _ => PathBuf::from(p),
    }
}

// ---- folder picker ----------------------------------------------------

/// Starting places: your home and Music folders, and wherever this system
/// puts drives and network shares.
fn roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if cfg!(target_os = "ios") {
        // The app's Documents folder, which the Files app can fill.
        roots.extend(store::documents_dir().map(Path::to_path_buf));
        return roots;
    }
    roots.push(home());
    roots.push(home().join("Music"));
    if cfg!(target_os = "macos") {
        roots.extend(subdirs(Path::new("/Volumes")));
    } else if cfg!(windows) {
        for letter in b'A'..=b'Z' {
            let d = PathBuf::from(format!("{}:\\", letter as char));
            if d.exists() {
                roots.push(d);
            }
        }
    } else {
        for base in ["/mnt", "/media", "/run/media"] {
            roots.extend(subdirs(Path::new(base)));
        }
    }
    roots.into_iter().filter(|p| p.exists()).collect()
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir() || t.is_symlink()).unwrap_or(false))
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            !(n.starts_with('.') || n.starts_with('@') || n.starts_with('#') || n.starts_with('$'))
        })
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    out.sort_by_key(|p| p.to_string_lossy().to_lowercase());
    out
}

async fn browse(Query(q): Query<HashMap<String, String>>) -> Result<Json<Value>, ApiError> {
    let Some(dir) = q.get("path").filter(|p| !p.is_empty()) else {
        return Ok(Json(json!({ "path": null, "dirs": roots() })));
    };
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        return Err(err(StatusCode::BAD_REQUEST, format!("Can't open {}", dir.display())));
    }
    let parent = dir.parent().map(Path::to_path_buf);
    let d = dir.clone();
    let dirs = tokio::task::spawn_blocking(move || subdirs(&d)).await?;
    Ok(Json(json!({ "path": dir, "parent": parent, "dirs": dirs })))
}

/// Mounts a network share the system's own way (on a Mac, Finder asks
/// for the password and the share appears under /Volumes).
async fn mount(Json(v): Json<Value>) -> Result<StatusCode, ApiError> {
    let url = v["url"].as_str().unwrap_or("").trim().to_owned();
    let ok_scheme = ["smb://", "afp://", "nfs://", "cifs://"].iter().any(|p| url.to_ascii_lowercase().starts_with(p));
    let unc = url.starts_with("\\\\");
    if !(ok_scheme || unc) || url.chars().any(char::is_control) {
        return Err(err(StatusCode::BAD_REQUEST, "Type an address like smb://diskstation/music"));
    }
    let status = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(&url).status()
    } else if cfg!(windows) {
        let path = if unc { url.clone() } else { format!("\\\\{}", url.split_once("://").map(|(_, r)| r).unwrap_or(&url).replace('/', "\\")) };
        std::process::Command::new("explorer").arg(path).status()
    } else if cfg!(target_os = "linux") {
        std::process::Command::new("gio").args(["mount", &url]).status()
    } else {
        return Err(err(StatusCode::NOT_IMPLEMENTED, "Use the Files app to connect to servers on this device"));
    };
    status.map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("Couldn't connect: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- library and media ------------------------------------------------

async fn library_list() -> Json<Value> {
    let tracks: Vec<Value> = library::list()
        .await
        .into_iter()
        .map(|t| {
            json!({
                "id": t.id, "source": "local", "title": t.title, "artist": t.artist, "album": t.album,
                "trackNo": t.track_no, "year": t.year, "genre": t.genre, "duration": t.duration,
                "folder": t.path.parent().map(|p| p.to_string_lossy().into_owned()),
            })
        })
        .collect();
    Json(json!({ "status": library::status().await, "tracks": tracks }))
}

/// Serves a file with byte ranges, so the player can seek.
async fn send_file(path: &Path, mime: &str, headers: &HeaderMap) -> Result<Response, ApiError> {
    let mut file = tokio::fs::File::open(path).await.map_err(|e| err(StatusCode::NOT_FOUND, format!("Can't open the file: {e}")))?;
    let size = file.metadata().await?.len();
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let (start, end, partial) = match parse_range(range, size) {
        Ok(Some((s, e))) => (s, e, true),
        Ok(None) => (0, size.saturating_sub(1), false),
        Err(()) => {
            return Ok(Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                .body(Body::empty())?);
        }
    };
    file.seek(std::io::SeekFrom::Start(start)).await?;
    let len = if size == 0 { 0 } else { end - start + 1 };
    let body = Body::from_stream(ReaderStream::new(file.take(len)));
    let mut res = Response::builder()
        .status(if partial { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK })
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, len)
        .header(header::ACCEPT_RANGES, "bytes");
    if partial {
        res = res.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"));
    }
    Ok(res.body(body)?)
}

async fn media_local(UrlPath(id): UrlPath<String>, headers: HeaderMap) -> Result<Response, ApiError> {
    let Some(t) = library::get(&id).await else {
        return Err(err(StatusCode::NOT_FOUND, "Not in the library"));
    };
    send_file(&t.path, library::mime_for(&t.path), &headers).await
}

async fn media_cloud(UrlPath(sid): UrlPath<String>, Query(q): Query<HashMap<String, String>>, headers: HeaderMap) -> Result<Response, ApiError> {
    let get = |k: &str| q.get(k).map(String::as_str).unwrap_or_default();
    let path = crate::cloud::file(&sid, get("p"), get("s"))?;
    send_file(&path, library::mime_for(&path), &headers).await
}

async fn media_soundcloud(UrlPath(file): UrlPath<String>, Query(q): Query<std::collections::HashMap<String, String>>, headers: HeaderMap) -> Result<Response, ApiError> {
    let urn = file.strip_suffix(".mp3").unwrap_or(&file).to_owned();
    soundcloud::stream(urn, headers, q.get("from").and_then(|f| f.parse().ok())).await
}

async fn media_spotify(UrlPath(file): UrlPath<String>, headers: HeaderMap) -> Result<Response, ApiError> {
    let id = file.strip_suffix(".wav").unwrap_or(&file).to_owned();
    spotify::stream(id, headers).await
}

async fn skins() -> Json<Value> {
    let dir = store::data_dir().join("skins");
    let mut list = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.to_lowercase().ends_with(".wsz") {
                list.push(json!({ "url": format!("/skins/{}", urlencoding::encode(&name)), "name": name[..name.len() - 4] }));
            }
        }
    }
    Json(Value::Array(list))
}

/// Keeps a skin dropped on the player.
async fn add_skin(Query(q): Query<HashMap<String, String>>, body: axum::body::Bytes) -> Result<Json<Value>, ApiError> {
    let raw = q.get("name").map(String::as_str).unwrap_or("skin.wsz");
    let name: String = Path::new(raw).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default().chars().filter(|c| !"/\\:*?\"<>|".contains(*c)).collect();
    // A skin is a zip file.
    if name.is_empty() || !body.starts_with(b"PK") {
        return Err(err(StatusCode::BAD_REQUEST, "Not a Winamp skin"));
    }
    let file = format!("{name}.wsz");
    tokio::fs::write(store::data_dir().join("skins").join(&file), &body).await?;
    Ok(Json(json!({ "url": format!("/skins/{}", urlencoding::encode(&file)) })))
}

async fn skin_file(UrlPath(file): UrlPath<String>, headers: HeaderMap) -> Result<Response, ApiError> {
    let name = Path::new(&file).file_name().ok_or_else(|| err(StatusCode::BAD_REQUEST, "bad name"))?;
    send_file(&store::data_dir().join("skins").join(name), "application/octet-stream", &headers).await
}

#[cfg(debug_assertions)]
async fn test_tone(headers: HeaderMap) -> Result<Response, ApiError> {
    spotify::test_tone(headers).await
}

#[cfg(not(debug_assertions))]
async fn test_tone() -> StatusCode {
    StatusCode::NOT_FOUND
}

// ---- sign-ins ---------------------------------------------------------

async fn spotify_login() -> Redirect {
    match spotify::login_url().await {
        Ok(url) => Redirect::to(&url),
        Err(e) => Redirect::to(&crate::shell::after_sign_in("Spotify", Err(e.1))),
    }
}

pub async fn serve(port: u16) -> anyhow::Result<tokio::net::TcpListener> {
    Ok(tokio::net::TcpListener::bind((HOST, port)).await?)
}

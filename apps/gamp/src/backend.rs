//! The music sources, sign-ins and streaming, from gamp-core, running on
//! an async runtime beside the windows. The local server (127.0.0.1)
//! streams every source to the audio engine and catches sign-in
//! callbacks; it serves no player page.

use std::sync::mpsc;

use axum::http::header;
use axum::response::IntoResponse;
use gamp_core::catalog::{self, Source};
use gamp_core::sources::Listing;
use winit::event_loop::EventLoopProxy;

static SIGNED_IN: &str = include_str!("../assets/signed-in.html");

/// Things sent to the window thread.
#[derive(Debug)]
pub enum UserEvent {
    /// A sign-in finished: bring Gamp forward and refresh.
    Focus,
    /// Background work finished.
    Wake,
    /// Files opened from the Finder (double-click, Open With, the Dock).
    Open(Vec<std::path::PathBuf>),
    /// The Dock icon was clicked.
    Reopen,
    /// A dialog was answered (macOS: dialogs open after the event that asked).
    Dialog(crate::dialog::Answer),
}

pub enum Reply {
    Sources(Vec<Source>),
    Listing { key: String, result: Result<Listing, String> },
    Config(serde_json::Value),
    Message(String),
    Scan(gamp_core::library::Status),
    /// A museum skin, downloaded (or why not).
    Skin(Result<std::path::PathBuf, String>),
}

pub struct Backend {
    rt: tokio::runtime::Runtime,
    pub port: u16,
    tx: mpsc::Sender<Reply>,
    pub rx: mpsc::Receiver<Reply>,
    proxy: Option<EventLoopProxy<UserEvent>>,
}

fn page() -> axum::Router {
    axum::Router::new()
        .route("/signed-in.html", axum::routing::get(|| async { ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], SIGNED_IN).into_response() }))
        .fallback(|| async { ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "Gamp is running.").into_response() })
}

impl Backend {
    pub fn start(data: std::path::PathBuf, proxy: Option<EventLoopProxy<UserEvent>>) -> anyhow::Result<Backend> {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().thread_name("gamp-net").build()?;
        gamp_core::adopt_old_data(&data);
        gamp_core::shell::set_shell("floating");
        if let Some(p) = proxy.clone() {
            gamp_core::shell::set_control(Box::new(move |action, _| {
                if action == "focus" {
                    let _ = p.send_event(UserEvent::Focus);
                }
            }));
        }
        let port = rt.block_on(async move { gamp_core::start_server(data, None, page()).await })?;
        let (tx, rx) = mpsc::channel();
        Ok(Backend { rt, port, tx, rx, proxy })
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn send(&self, r: Reply) {
        let _ = self.tx.send(r);
        if let Some(p) = &self.proxy {
            let _ = p.send_event(UserEvent::Wake);
        }
    }

    fn spawn<F>(&self, work: F)
    where
        F: std::future::Future<Output = Reply> + Send + 'static,
    {
        let tx = self.tx.clone();
        let proxy = self.proxy.clone();
        self.rt.spawn(async move {
            let _ = tx.send(work.await);
            if let Some(p) = proxy {
                let _ = p.send_event(UserEvent::Wake);
            }
        });
    }

    pub fn refresh_sources(&self) {
        self.spawn(async { Reply::Sources(catalog::sources().await) });
    }

    pub fn browse(&self, key: String, sid: String, node: String) {
        self.spawn(async move { Reply::Listing { key, result: catalog::browse(&sid, &node).await.map_err(|e| e.1) } });
    }

    pub fn search(&self, key: String, sid: String, q: String) {
        self.spawn(async move { Reply::Listing { key, result: catalog::search(&sid, &q).await.map_err(|e| e.1) } });
    }

    /// Fetches a museum skin into `dir`: this one, or one at random.
    pub fn skin(&self, item: Option<gamp_core::sources::Item>, dir: std::path::PathBuf) {
        use gamp_core::skin_museum as museum;
        self.spawn(async move {
            let got = async {
                let i = match item {
                    Some(i) => i,
                    None => museum::random().await?,
                };
                museum::fetch(&i, &dir).await
            };
            Reply::Skin(got.await.map_err(|e| e.1))
        });
    }

    pub fn action(&self, sid: String, action: String, items: Vec<gamp_core::sources::Item>) {
        self.spawn(async move {
            if action.starts_with("fav:") {
                return Reply::Message(gamp_core::favorites::action(&sid, &action, items).await.unwrap_or_else(|e| e.1));
            }
            match gamp_core::sources::action(&sid, &action, items).await {
                Ok(v) => Reply::Message(v["message"].as_str().unwrap_or("Done").to_owned()),
                Err(e) => Reply::Message(e.1),
            }
        });
    }

    pub fn config(&self) {
        let port = self.port;
        self.spawn(async move {
            let res = reqwest_json(port, "/api/config", None).await;
            Reply::Config(res.unwrap_or_default())
        });
    }

    /// POSTs to the local server's API (same checks as the page had).
    pub fn post(&self, path: &'static str, body: serde_json::Value, then_refresh: bool) {
        let port = self.port;
        let tx = self.tx.clone();
        let proxy = self.proxy.clone();
        self.rt.spawn(async move {
            let r = reqwest_json(port, path, Some(body)).await;
            let msg = match &r {
                Ok(v) if v.get("error").is_some() => v["error"].as_str().unwrap_or("Failed").to_owned(),
                Ok(_) => String::new(),
                Err(e) => e.clone(),
            };
            if !msg.is_empty() {
                let _ = tx.send(Reply::Message(msg));
            }
            if then_refresh {
                let _ = tx.send(Reply::Sources(catalog::sources().await));
                let _ = tx.send(Reply::Config(reqwest_json(port, "/api/config", None).await.unwrap_or_default()));
            }
            if let Some(p) = proxy {
                let _ = p.send_event(UserEvent::Wake);
            }
        });
    }

    pub fn delete_account(&self, id: String) {
        let tx = self.tx.clone();
        let proxy = self.proxy.clone();
        self.rt.spawn(async move {
            let _ = gamp_core::sources::remove_account(&id).await;
            let _ = tx.send(Reply::Sources(catalog::sources().await));
            if let Some(p) = proxy {
                let _ = p.send_event(UserEvent::Wake);
            }
        });
    }

    /// The saved account's display name, then the source list again.
    /// The tree menu grows a Rename item in a later phase; nothing calls this yet.
    #[allow(dead_code)]
    pub fn rename(&self, id: String, name: String) {
        let tx = self.tx.clone();
        let proxy = self.proxy.clone();
        self.rt.spawn(async move {
            let _ = gamp_core::sources::rename(&id, &name).await;
            let _ = tx.send(Reply::Sources(catalog::sources().await));
            if let Some(p) = proxy {
                let _ = p.send_event(UserEvent::Wake);
            }
        });
    }

    pub fn rescan(&self) {
        self.spawn(async { Reply::Scan(gamp_core::library::rescan().await) });
    }

    pub fn scan_status(&self) {
        self.spawn(async { Reply::Scan(gamp_core::library::status().await) });
    }

    pub fn note(&self, msg: impl Into<String>) {
        self.send(Reply::Message(msg.into()));
    }
}

/// The local server, over loopback (no Origin header, so it's ours).
async fn reqwest_json(port: u16, path: &str, body: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
    let c = gamp_core::http::client();
    let url = format!("http://127.0.0.1:{port}{path}");
    let req = match body {
        Some(b) => c.post(url).json(&b),
        None => c.get(url),
    };
    let res = req.send().await.map_err(|e| e.to_string())?;
    let text = res.text().await.map_err(|e| e.to_string())?;
    Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
}

/// Opens a page in the system's web browser (sign-ins happen there).
pub fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(windows)]
    let r = {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).creation_flags(0x0800_0000).spawn()
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    if let Err(e) = r {
        log::warn!("couldn't open the browser: {e}");
    }
}

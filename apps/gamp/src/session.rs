//! Coming back as you left it: windows (where, which are open, sizes),
//! double size, always on top, equalizer, volume, balance, shuffle,
//! repeat, the playlist and its current song, the skin, G-Drop's preset.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::app::{App, Kind};
use crate::playlist::Entry;

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Session {
    pub windows: HashMap<String, (i32, i32, bool)>,
    /// `windows` are in desktop units (on a Mac, points; older sessions
    /// saved a Mac's physical pixels).
    pub units: bool,
    /// Older sessions: double size (now `size`).
    pub double: bool,
    pub size: f64,
    pub on_top: bool,
    pub volume: f32,
    pub balance: f32,
    pub shuffle: bool,
    pub repeat: bool,
    pub eq_on: bool,
    pub eq_auto: bool,
    pub preamp: f32,
    pub bands: [f32; 10],
    pub tracks: Vec<SavedEntry>,
    pub current: usize,
    pub skin: Option<PathBuf>,
    pub playlist_size: (u32, u32),
    pub library_size: (u32, u32),
    pub vis_size: (u32, u32),
    pub vis_preset: usize,
    pub vis_mode: u8,
    /// G-Drop stays on its preset instead of changing by itself.
    pub vis_hold: bool,
    pub time_remaining: bool,
    pub shade: (bool, bool, bool),
    /// Where the current track was (seconds; 0 at its start or stopped).
    pub position: f64,
    /// The library's recent searches, by source.
    pub searches: HashMap<String, Vec<String>>,
    /// Library groups that start open and were closed.
    #[serde(default)]
    pub collapsed: BTreeSet<String>,
    /// Library groups that start closed (a server, DLNA) and were opened.
    #[serde(default)]
    pub opened: BTreeSet<String>,
    /// The language picked in Options › Language (none: the system's).
    pub lang: Option<String>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct SavedEntry {
    pub path: Option<PathBuf>,
    pub url: Option<String>,
    pub live: bool,
    pub artist: String,
    pub title: String,
    pub duration: Option<f64>,
}

fn name(k: Kind) -> &'static str {
    match k {
        Kind::Main => "main",
        Kind::Eq => "equalizer",
        Kind::Playlist => "playlist",
        Kind::Library => "library",
        Kind::Vis => "gdrop",
        Kind::Jump => "jump",
        Kind::Info => "info",
        Kind::Menu(_) => "menu",
    }
}

const FILE: &str = "session.json";

impl App {
    pub fn save_session(&self) {
        let mut windows = HashMap::new();
        for k in [Kind::Main, Kind::Eq, Kind::Playlist, Kind::Library, Kind::Vis] {
            if let Some(w) = self.win(k) {
                if w.window.is_some() {
                    windows.insert(name(k).to_owned(), (w.pos.0, w.pos.1, w.visible));
                }
            }
        }
        let s = Session {
            windows,
            units: true,
            double: self.size == 2.0,
            size: self.size,
            on_top: self.on_top,
            volume: self.main.volume,
            balance: self.main.balance,
            shuffle: self.main.shuffle,
            repeat: self.main.repeat,
            eq_on: self.eq.on,
            eq_auto: self.eq.auto,
            preamp: self.eq.preamp,
            bands: self.eq.bands,
            tracks: self
                .playlist
                .entries
                .iter()
                .map(|e| SavedEntry {
                    path: e.url.is_none().then(|| e.path.clone()),
                    url: e.url.clone(),
                    live: e.live,
                    artist: e.artist.clone(),
                    title: e.title.clone(),
                    duration: e.duration,
                })
                .collect(),
            current: self.playlist.current,
            skin: self.skin_path.clone(),
            playlist_size: (self.pl.width, self.pl.height),
            library_size: (self.lib.view.width, self.lib.view.height),
            vis_size: (self.drop.width, self.drop.height),
            vis_preset: gamp_vis::PRESETS.iter().position(|p| p.name == self.drop.engine.preset_name()).unwrap_or(0),
            vis_mode: match self.main.vis_mode {
                gamp_ui::vis::VisMode::Analyzer => 0,
                gamp_ui::vis::VisMode::Oscilloscope => 1,
                gamp_ui::vis::VisMode::Off => 2,
            },
            vis_hold: !self.drop.engine.auto,
            time_remaining: self.main.show_remaining,
            shade: (self.main.shade, self.eq.shade, self.pl.shade),
            position: self.resume_point(),
            searches: self.lib.recent.clone(),
            collapsed: self.lib.collapsed.clone(),
            opened: self.lib.opened.clone(),
            lang: self.lang.clone(),
        };
        if let Ok(json) = serde_json::to_vec_pretty(&s) {
            let path = self.data_dir.join(FILE);
            let tmp = path.with_extension("tmp");
            let _ = std::fs::create_dir_all(&self.data_dir);
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }

    pub fn load_session(&mut self) -> Option<Session> {
        let bytes = std::fs::read(self.data_dir.join(FILE)).ok()?;
        let s: Session = serde_json::from_slice(&bytes).ok()?;
        self.size = if s.size >= 1.0 { s.size.min(3.0) } else if s.double { 2.0 } else { 1.0 };
        self.on_top = s.on_top;
        self.main.volume = s.volume.clamp(0.0, 1.0);
        self.main.balance = s.balance.clamp(-1.0, 1.0);
        self.eq.volume = self.main.volume;
        self.eq.balance = self.main.balance;
        self.player.set_volume(self.main.volume);
        self.player.set_balance(self.main.balance);
        self.main.shuffle = s.shuffle;
        self.main.repeat = s.repeat;
        self.playlist.shuffle = s.shuffle;
        self.playlist.repeat = s.repeat;
        self.eq.on = s.eq_on;
        self.eq.auto = s.eq_auto;
        self.eq.preamp = s.preamp;
        self.eq.bands = s.bands;
        self.player.set_eq(gamp_audio::EqSettings { on: s.eq_on, preamp: s.preamp, bands: s.bands });
        self.main.show_remaining = s.time_remaining;
        self.lib.recent = s.searches.clone();
        self.lib.collapsed = s.collapsed.clone();
        self.lib.opened = s.opened.clone();
        self.set_lang(s.lang.clone());
        self.main.vis_mode = match s.vis_mode {
            1 => gamp_ui::vis::VisMode::Oscilloscope,
            2 => gamp_ui::vis::VisMode::Off,
            _ => gamp_ui::vis::VisMode::Analyzer,
        };
        (self.main.shade, self.eq.shade, self.pl.shade) = s.shade;
        if s.playlist_size.0 > 0 {
            (self.pl.width, self.pl.height) = s.playlist_size;
        }
        if s.library_size.0 > 0 {
            (self.lib.view.width, self.lib.view.height) = s.library_size;
        }
        if s.vis_size.0 > 0 {
            (self.drop.width, self.drop.height) = s.vis_size;
        }
        self.drop.engine.select(s.vis_preset, true);
        self.drop.engine.auto = !s.vis_hold;
        if let Some(p) = s.skin.clone().filter(|p| p.exists()) {
            self.load_skin(Some(p));
        }
        let entries: Vec<Entry> = s
            .tracks
            .iter()
            .filter(|t| t.url.is_some() || t.path.as_ref().is_some_and(|p| p.exists()))
            .map(|t| Entry { path: t.path.clone().unwrap_or_default(), url: t.url.clone(), live: t.live, artist: t.artist.clone(), title: t.title.clone(), duration: t.duration })
            .collect();
        if !entries.is_empty() {
            self.playlist.set(entries);
            self.playlist.current = s.current.min(self.playlist.entries.len() - 1);
            self.sync_playlist();
            self.pl.reveal(self.playlist.current);
            self.update_title();
            // Back where you were: the track loaded, paused at its place
            // (the display shows it), and Play goes on from there.
            log::info!("resume: saved place {:.1}s in track {}", s.position, self.playlist.current);
            if let Some(e) = self.playlist.current().filter(|_| s.position >= 1.0) {
                self.resume = Some((self.playlist.current, e.path.clone(), e.url.clone(), s.position));
                self.load_current(false);
                self.reopened = true;
                self.main.blink_on = true;
            }
        }
        self.main.eq_open = s.windows.get("equalizer").is_none_or(|w| w.2);
        self.main.playlist_open = s.windows.get("playlist").is_none_or(|w| w.2);
        Some(s)
    }

    /// Where to pick up next time: where it was to resume, while it isn't
    /// there yet; else the last place heard in the current track (even after
    /// Stop); its start if it's live or was heard to the end.
    fn resume_point(&self) -> f64 {
        let Some(e) = self.playlist.current().filter(|e| !e.live) else { return 0.0 };
        let this = |r: &&(usize, PathBuf, Option<String>, f64)| r.0 == self.playlist.current && r.1 == e.path && r.2 == e.url;
        let at = self.resume.as_ref().filter(this).or(self.heard.as_ref().filter(this)).map_or(0.0, |r| r.3);
        if e.duration.is_some_and(|d| at > d - 5.0) { 0.0 } else { at }
    }

    /// Puts the windows where they were (if that's still on a screen).
    pub fn place_windows(&mut self, s: &Session) {
        for k in [Kind::Main, Kind::Eq, Kind::Playlist, Kind::Library, Kind::Vis] {
            let Some(&(mut x, mut y, visible)) = s.windows.get(name(k)) else { continue };
            let monitors: Vec<_> = self.win(k).and_then(|w| w.window.as_ref()).map(|w| w.available_monitors().collect()).unwrap_or_default();
            if !s.units && cfg!(target_os = "macos") {
                // Saved in physical pixels of the display it was on.
                let on = monitors.iter().find(|m| {
                    let (p, z) = (m.position(), m.size());
                    x >= p.x && y >= p.y && x < p.x + z.width as i32 && y < p.y + z.height as i32
                });
                let sf = on.map_or(1.0, |m| m.scale_factor());
                (x, y) = ((x as f64 / sf).round() as i32, (y as f64 / sf).round() as i32);
            }
            let on_screen = monitors.iter().any(|m| {
                let (px, py, w, h) = crate::win::monitor_area(m);
                x >= px - 50 && y >= py - 10 && x < px + w - 40 && y < py + h - 20
            });
            if let Some(w) = self.win_mut(k) {
                if on_screen {
                    w.move_to(x, y);
                }
                w.show(visible || k == Kind::Main);
            }
        }
        if self.on_top {
            self.on_top = false;
            self.toggle_on_top();
        }
    }
}

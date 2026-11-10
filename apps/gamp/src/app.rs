//! The player: its windows (main, equalizer, playlist, pop-up menus),
//! what clicks and keys do, and keeping the audio engine and the windows
//! in step.

use crate::dialog::Ask;
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gamp_audio::analysis::{self, Spectrum};
use gamp_audio::{EqSettings, Event as AudioEvent, Input, OutputKind, Player, State};
use gamp_skin::Skin;
use gamp_ui::lang::{tf, tm, tmf};
use gamp_ui::main_window::{self, Control, PlayState};
use gamp_ui::menu::{self, Item};
use gamp_ui::playlist_window::{self as plw, Menu as PlMenu, Mini};
use gamp_ui::vis::VisMode;
use gamp_ui::{eq_window, playlist_window};
use winit::dpi::LogicalSize;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::WindowLevel;

use crate::eqf;
use crate::layout::{self, Rect};
use crate::playlist::{self, Entry, Playlist};
use crate::win::Win;

pub const TICK: Duration = Duration::from_millis(33);
/// G-Drop animates at 60 frames a second.
pub const VIS_TICK: Duration = Duration::from_millis(16);
const MARQUEE_STEP: Duration = Duration::from_millis(220);
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Main,
    Eq,
    Playlist,
    Library,
    /// G-Drop, the visualizer.
    Vis,
    /// Jump to File.
    Jump,
    /// File Info.
    Info,
    /// A pop-up menu; the number is its depth (sub-menus are deeper).
    Menu(usize),
}

pub(crate) struct MenuLevel {
    items: Vec<Item>,
    hover: Option<usize>,
    win: Win,
    /// The sub-menu open from this level, by item.
    child_of: Option<usize>,
}

pub(crate) struct Drag {
    kind: Kind,
    start: (f64, f64),
    group: Vec<(Kind, (i32, i32))>,
}

/// The sleep timer fades out over its last this many seconds.
/// Shown while a stream opens, its dots counting up (… then . .. ... again).
const CONNECTING: &str = "CONNECTING";
const SEEKING: &str = "SEEKING...";
const SLEEP_FADE: f32 = 30.0;

/// The size choices (right-click › Options › Size).
const SIZES: [(f64, &str); 10] = [
    (1.0, "Normal (100%)"),
    (1.1, "110%"),
    (1.2, "120%"),
    (1.3, "130%"),
    (1.4, "140%"),
    (1.5, "150%"),
    (1.75, "175%"),
    (2.0, "Double (200%)"),
    (2.5, "250%"),
    (3.0, "300%"),
];

#[derive(Default)]
pub(crate) struct Resize {
    pub(crate) start: (f64, f64),
    pub(crate) size: (u32, u32),
    /// Which edges move (G-Drop): -1 left/top, 1 right/bottom, 0 neither.
    pub(crate) dir: (i8, i8),
    /// The window's position when it started (moving the left/top edge).
    pub(crate) origin: (i32, i32),
}

pub struct App {
    pub skin: Skin,
    pub player: Player,
    pub playlist: Playlist,
    pub(crate) ids: HashMap<u64, usize>,
    pub main: main_window::View,
    pub eq: eq_window::View,
    pub pl: plw::View,
    pub wins: HashMap<Kind, Win>,
    pub(crate) menus: Vec<MenuLevel>,
    /// How big Gamp is drawn: 1.0 (Winamp's own size) to 3.0.
    pub size: f64,
    /// Stop when the playing track ends (Ctrl+V).
    pub stop_after: bool,
    /// The sleep timer (or Shift+V): when playback stops, and over how
    /// many seconds before that it fades out.
    pub sleep_at: Option<(Instant, f32)>,
    /// A seek still on its way: (target, when).
    seek_wait: Option<(f64, Instant)>,
    pub on_top: bool,
    /// The language picked in Options › Language (none: the system's).
    pub lang: Option<String>,
    pub modifiers: ModifiersState,
    pub(crate) drag: Option<Drag>,
    pub(crate) resize: Option<Resize>,
    /// The displays, and where the windows docked to the main window sit
    /// from it; kept for a moment after the displays change.
    docked: (Vec<Rect>, Vec<(Kind, i32, i32)>, Option<Instant>),
    pub(crate) last_click: Option<(Instant, Kind, i32, i32)>,
    pub(crate) marquee_at: Instant,
    /// What the display showed last tick, to scroll a new text from its start.
    pub(crate) shown: String,
    /// Where the title had scrolled to when a message took its place.
    pub(crate) title_scroll: Option<i32>,
    pub(crate) blink_at: Instant,
    pub(crate) spectrum: Spectrum,
    pub(crate) wave: Vec<f32>,
    pub(crate) spec: Vec<f32>,
    pub(crate) bars: Vec<f32>,
    pub data_dir: PathBuf,
    /// Set when the app should quit.
    pub quit: bool,
    /// The row a playlist drag started on (reordering).
    pub(crate) pl_drag: Option<usize>,
    pub(crate) backend: Option<crate::backend::Backend>,
    pub(crate) lib: crate::library::LibState,
    pub(crate) jump: crate::jump::JumpState,
    pub(crate) info: crate::info::InfoState,
    pub(crate) drop: crate::drop::DropState,
    /// The skin file in use (None: the built-in skin).
    pub(crate) skin_path: Option<PathBuf>,
    /// A pasted radio address: play it when the search answers.
    pub(crate) pending_stream: bool,
    /// When to refresh the source list (after a DLNA search).
    pub(crate) pending_refresh: Option<Instant>,
    /// When to ask how a folder scan is going.
    pub(crate) pending_scan: Option<Instant>,
    /// Where the track was when Gamp last closed: (playlist index, what it
    /// plays, seconds). Kept, and saved as the place, until the track is
    /// really there: a stream that's slow to seek, or can't open yet (just
    /// after waking up), doesn't lose it.
    pub(crate) resume: Option<(usize, PathBuf, Option<String>, f64)>,
    /// The last place heard in the current track, kept after Stop, so
    /// Gamp opens again where you were.
    pub(crate) heard: Option<(usize, PathBuf, Option<String>, f64)>,
    /// Waiting, paused, where Gamp last closed (until it plays).
    pub(crate) reopened: bool,
    /// Files dropped in one go (each arrives as its own event).
    pub(crate) drops: Vec<PathBuf>,
    pub(crate) drop_target: Option<Kind>,
    /// Show the library at start (first run, nothing to play yet).
    pub(crate) lib_open_at_start: bool,
    /// The saved session, waiting for the windows to exist.
    pub(crate) session: Option<crate::session::Session>,
    pub(crate) last_save: Instant,
    /// The last thing that went wrong, for Report a problem….
    pub(crate) last_error: Option<String>,
    /// For dialogs to answer through (macOS opens them after the event).
    pub(crate) proxy: Option<winit::event_loop::EventLoopProxy<crate::backend::UserEvent>>,
    pub(crate) media: Option<crate::media::Media>,
    pub(crate) media_at: Instant,
    pub(crate) update: Option<crate::update::Release>,
    pub(crate) update_rx: Option<std::sync::mpsc::Receiver<crate::update::Msg>>,
    /// `gamp --update`: install a newer version without asking, or quit.
    pub(crate) update_now: bool,
    pub(crate) update_checked: Instant,
    /// When a display message should go away by itself.
    pub(crate) message_until: Option<Instant>,
}

/// Where settings and skins live (the same folder the earlier app used).
pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("WINAMP_DATA") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default();
    let base = if cfg!(target_os = "macos") {
        home.join("Library/Application Support")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| home.join("AppData/Roaming"))
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"))
    };
    base.join("co.fillinthe.gamp")
}

impl App {
    pub fn new(output: OutputKind, backend: Option<crate::backend::Backend>) -> anyhow::Result<App> {
        // The data folder is normally opened by the backend; without one
        // (tests, or if the server failed) things that read it, like signed
        // media addresses, must not find it missing.
        if backend.is_none() {
            let dir = if cfg!(test) { std::env::temp_dir().join(format!("gamp-test-{}", std::process::id())) } else { data_dir() };
            gamp_core::store::init(dir, None);
        }
        let player = Player::new(output).or_else(|e| {
            log::warn!("no sound output ({e}); playing silently");
            Player::new(OutputKind::Null)
        })?;
        let mut main = main_window::View { volume: 0.8, focused: true, vis_mode: VisMode::Analyzer, title: "GAMP".into(), eq_open: true, playlist_open: true, ..Default::default() };
        main.balance = 0.0;
        player.set_volume(main.volume);
        let eq = eq_window::View { on: true, ..Default::default() };
        let pl = plw::View { width: plw::MIN_WIDTH, height: plw::MIN_HEIGHT + 29 * 2, running: "0:00/0:00".into(), ..Default::default() };
        let mut wins = HashMap::new();
        for k in [Kind::Main, Kind::Eq, Kind::Playlist, Kind::Library, Kind::Vis] {
            wins.insert(k, Win::headless());
        }
        Ok(App {
            skin: Skin::default_skin(),
            player,
            playlist: Playlist::default(),
            ids: HashMap::new(),
            main,
            eq,
            pl,
            wins,
            menus: Vec::new(),
            size: 1.0,
            stop_after: false,
            lang: None,
            sleep_at: None,
            seek_wait: None,
            on_top: false,
            modifiers: ModifiersState::empty(),
            drag: None,
            resize: None,
            docked: Default::default(),
            last_click: None,
            marquee_at: Instant::now(),
            shown: String::new(),
            title_scroll: None,
            blink_at: Instant::now(),
            spectrum: Spectrum::new(512),
            wave: vec![0.0; 512],
            spec: Vec::new(),
            bars: Vec::new(),
            data_dir: data_dir(),
            quit: false,
            pl_drag: None,
            drops: Vec::new(),
            drop_target: None,
            backend,
            lib: Default::default(),
            jump: Default::default(),
            info: Default::default(),
            drop: Default::default(),
            skin_path: None,
            pending_stream: false,
            pending_refresh: None,
            pending_scan: None,
            resume: None,
            heard: None,
            reopened: false,
            lib_open_at_start: false,
            session: None,
            last_save: Instant::now(),
            last_error: None,
            proxy: None,
            media: None,
            media_at: Instant::now(),
            update: None,
            update_rx: None,
            update_now: false,
            update_checked: Instant::now(),
            message_until: None,
        })
    }

    // ---- windows --------------------------------------------------------

    pub(crate) fn zoom(&self) -> f64 {
        self.size
    }

    fn logical_size(&self, k: Kind) -> LogicalSize<u32> {
        let z = self.zoom();
        let (w, h) = match k {
            Kind::Main => (main_window::WIDTH, if self.main.shade { main_window::SHADE_HEIGHT } else { main_window::HEIGHT }),
            Kind::Eq => (eq_window::WIDTH, if self.eq.shade { eq_window::SHADE_HEIGHT } else { eq_window::HEIGHT }),
            Kind::Playlist => self.pl.size(),
            // Every window grows with the size setting (Winamp's double
            // size left the library small; bigger is for reading).
            Kind::Library => self.lib.view.size(),
            Kind::Vis => (self.drop.width, self.drop.height),
            Kind::Jump => (gamp_ui::jump_window::WIDTH, gamp_ui::jump_window::HEIGHT),
            Kind::Info => (gamp_ui::info_window::WIDTH, gamp_ui::info_window::height(&self.info.view)),
            Kind::Menu(i) => self.menus.get(i).map(|m| menu::size(&m.items)).unwrap_or((1, 1)),
        };
        LogicalSize::new((w as f64 * z).round() as u32, (h as f64 * z).round() as u32)
    }

    /// Opens the three windows stacked, the way Winamp starts.
    pub fn open_windows(&mut self, el: &ActiveEventLoop) {
        let scale = crate::win::units(el.primary_monitor().map(|m| m.scale_factor()).unwrap_or(1.0));
        let (x, y) = ((200.0 * scale) as i32, (160.0 * scale) as i32);
        let step = |h: u32| (h as f64 * scale * self.zoom()) as i32;
        let positions = [(Kind::Main, y), (Kind::Eq, y + step(main_window::HEIGHT)), (Kind::Playlist, y + step(main_window::HEIGHT) + step(eq_window::HEIGHT))];
        for (k, py) in positions {
            let title = match k {
                Kind::Main => "Gamp",
                Kind::Eq => "Gamp Equalizer",
                _ => "Gamp Playlist",
            };
            let mut w = Win::open(Some(el), title, self.logical_size(k), (x, py), false);
            let visible = match k {
                Kind::Eq => self.main.eq_open,
                Kind::Playlist => self.main.playlist_open,
                _ => true,
            };
            w.show(visible);
            self.wins.insert(k, w);
        }
        // The library opens beside the stack when you ask for it.
        let lx = x + (main_window::WIDTH as f64 * scale * self.zoom()) as i32;
        let mut w = Win::open(Some(el), "Gamp Media Library", self.logical_size(Kind::Library), (lx, y), false);
        w.show(self.lib_open_at_start);
        self.wins.insert(Kind::Library, w);
        // G-Drop opens beside the main window on demand.
        let mut w = Win::open(Some(el), "G-Drop", self.logical_size(Kind::Vis), (lx, y), false);
        w.show(false);
        self.wins.insert(Kind::Vis, w);
        // Jump to File opens over the main window when you press J.
        let mut w = Win::open(Some(el), "Gamp Jump to File", self.logical_size(Kind::Jump), (x + (20.0 * scale) as i32, y + (40.0 * scale) as i32), false);
        w.show(false);
        self.wins.insert(Kind::Jump, w);
        let mut w = Win::open(Some(el), "Gamp File Info", self.logical_size(Kind::Info), (x + (30.0 * scale) as i32, y + (50.0 * scale) as i32), false);
        w.show(false);
        self.wins.insert(Kind::Info, w);
        if let Some(s) = self.session.take() {
            self.place_windows(&s);
        }
        let main_window = self.win(Kind::Main).and_then(|w| w.window.clone());
        self.media = crate::media::Media::start(main_window.as_deref());
        self.lib_start();
    }

    pub fn win_mut(&mut self, k: Kind) -> Option<&mut Win> {
        match k {
            Kind::Menu(i) => self.menus.get_mut(i).map(|m| &mut m.win),
            _ => self.wins.get_mut(&k),
        }
    }

    pub fn win(&self, k: Kind) -> Option<&Win> {
        match k {
            Kind::Menu(i) => self.menus.get(i).map(|m| &m.win),
            _ => self.wins.get(&k),
        }
    }

    /// Which of our windows an OS window id is.
    pub fn kind_of(&self, id: winit::window::WindowId) -> Option<Kind> {
        for (k, w) in &self.wins {
            if w.window.as_ref().is_some_and(|w| w.id() == id) {
                return Some(*k);
            }
        }
        self.menus.iter().position(|m| m.win.window.as_ref().is_some_and(|w| w.id() == id)).map(Kind::Menu)
    }

    pub fn mark(&mut self, k: Kind) {
        if let Some(w) = self.win_mut(k) {
            w.dirty = true;
        }
    }

    pub fn occluded(&mut self, k: Kind, o: bool) {
        if let Some(w) = self.win_mut(k) {
            w.occluded = o;
            w.dirty |= !o;
        }
    }

    /// Uses language `code`, or the system's for none.
    pub fn set_lang(&mut self, code: Option<String>) {
        // Tests read English, whatever the computer's language.
        let system = || if cfg!(test) { "en" } else { sys_locale::get_locale().map_or("en", |l| gamp_ui::lang::matching(&l)) };
        gamp_ui::lang::set(code.as_deref().unwrap_or_else(system));
        self.lang = code;
        self.mark_all();
    }

    fn mark_all(&mut self) {
        for w in self.wins.values_mut() {
            w.dirty = true;
        }
    }

    pub fn redraw(&mut self, k: Kind) {
        let (skin, main, eq, pl) = (&self.skin, &self.main, &self.eq, &self.pl);
        match k {
            Kind::Main => {
                if let Some(w) = self.wins.get_mut(&k) {
                    main_window::draw(&mut w.frame, skin, main);
                    w.present();
                }
            }
            Kind::Eq => {
                if let Some(w) = self.wins.get_mut(&k) {
                    eq_window::draw(&mut w.frame, skin, eq);
                    w.present();
                }
            }
            Kind::Playlist => {
                if let Some(w) = self.wins.get_mut(&k) {
                    playlist_window::draw(&mut w.frame, skin, pl);
                    w.present();
                }
            }
            Kind::Library => {
                if let Some(w) = self.wins.get_mut(&k) {
                    self.lib.hots = gamp_ui::library_window::draw(&mut w.frame, &self.skin, &self.lib.view);
                    w.present();
                }
            }
            Kind::Vis => self.drop_redraw(),
            Kind::Jump => self.jump_redraw(),
            Kind::Info => self.info_redraw(),
            Kind::Menu(i) => {
                if let Some(m) = self.menus.get_mut(i) {
                    menu::draw(&mut m.win.frame, &m.items, m.hover);
                    m.win.present();
                }
            }
        }
    }

    pub fn dirty_kinds(&self) -> Vec<Kind> {
        let mut out: Vec<Kind> = self.wins.iter().filter(|(_, w)| w.dirty && w.visible && !w.occluded).map(|(k, _)| *k).collect();
        out.extend(self.menus.iter().enumerate().filter(|(_, m)| m.win.dirty).map(|(i, _)| Kind::Menu(i)));
        out
    }

    pub(crate) fn resize_window(&mut self, k: Kind) {
        let size = self.logical_size(k);
        if let Some(w) = self.win_mut(k) {
            w.set_size(size);
        }
    }

    fn rect(&self, k: Kind) -> Option<Rect> {
        let w = self.win(k)?;
        if !w.visible {
            return None;
        }
        let (sw, sh) = w.size_px();
        Some(Rect { x: w.pos.0, y: w.pos.1, w: sw, h: sh })
    }

    pub(crate) fn toggle_window(&mut self, k: Kind) {
        let on = match k {
            Kind::Eq => {
                self.main.eq_open = !self.main.eq_open;
                self.main.eq_open
            }
            Kind::Playlist => {
                self.main.playlist_open = !self.main.playlist_open;
                self.main.playlist_open
            }
            Kind::Library | Kind::Vis => !self.wins.get(&k).is_some_and(|w| w.visible),
            Kind::Jump => return self.jump_toggle(),
            Kind::Info => return if self.info.open { self.info_hide() } else { self.info_show_selected() },
            _ => return,
        };
        let before = self.rect(k);
        if let Some(w) = self.wins.get_mut(&k) {
            w.show(on);
        }
        match (on, before) {
            (true, _) => self.attach(k),
            // Closing the equalizer or playlist pulls what was docked under it up.
            (false, Some(r)) if matches!(k, Kind::Eq | Kind::Playlist) => self.shift_below(k, r, -r.h),
            _ => {}
        }
        self.mark(Kind::Main);
    }

    /// A window that opens away from the others docks to the main window's
    /// group: the equalizer right under the main window, the playlist at the
    /// bottom of the stack, the library and G-Drop to the right of it.
    fn attach(&mut self, k: Kind) {
        let Some(me) = self.rect(k) else { return };
        let kinds: Vec<Kind> = Self::DOCKABLE.iter().copied().filter(|o| *o != k && self.rect(*o).is_some()).collect();
        let rects: Vec<Rect> = kinds.iter().filter_map(|o| self.rect(*o)).collect();
        let Some(mi) = kinds.iter().position(|o| *o == Kind::Main) else { return };
        let m = rects[mi];
        let group: Vec<Rect> = layout::docked_group(&rects, mi).into_iter().map(|i| rects[i]).collect();
        // Already docked to the main window's group (and not on top of anything): stay.
        let over = |r: &Rect| me.x < r.right() && r.x < me.right() && me.y < r.bottom() && r.y < me.bottom();
        if group.iter().any(|r| layout::touching(&me, r)) && !rects.iter().any(over) {
            return;
        }
        let (x, y) = match k {
            Kind::Eq => {
                self.shift_below(k, Rect { y: m.bottom(), h: 0, ..m }, me.h);
                (m.x, m.bottom())
            }
            Kind::Playlist => (m.x, group.iter().filter(|r| (r.x - m.x).abs() <= 1).map(Rect::bottom).max().unwrap_or(m.bottom())),
            Kind::Library | Kind::Vis => (layout::beside(&group, me.w, self.screen()), m.y),
            _ => return,
        };
        if let Some(w) = self.wins.get_mut(&k) {
            w.move_to(x, y);
        }
    }

    /// Puts every open window back together, for when some got lost: the
    /// main window fully on a screen, the equalizer and playlist stacked
    /// under it, the library and G-Drop beside them, all in front.
    pub(crate) fn gather(&mut self) {
        let Some(m) = self.rect(Kind::Main) else { return };
        let stack: Vec<Kind> = [Kind::Eq, Kind::Playlist].into_iter().filter(|k| self.rect(*k).is_some()).collect();
        let side: Vec<Kind> = [Kind::Library, Kind::Vis].into_iter().filter(|k| self.rect(*k).is_some()).collect();
        let height = m.h + stack.iter().filter_map(|k| self.rect(*k)).map(|r| r.h).sum::<i32>();
        let screens = self.screens();
        let s = screens.iter().find(|s| s.x <= m.x && m.x < s.right() && s.y <= m.y && m.y < s.bottom()).or(screens.first()).copied();
        let (x, y) = match s {
            Some(s) => (m.x.min(s.right() - m.w).max(s.x), m.y.min(s.bottom() - height).max(s.y)),
            None => (m.x, m.y),
        };
        let mut placed = vec![Kind::Main];
        let mut at = (x, y);
        if let Some(w) = self.wins.get_mut(&Kind::Main) {
            w.move_to(x, y);
        }
        for k in stack {
            at.1 += self.rect(*placed.last().unwrap()).map_or(0, |r| r.h);
            if let Some(w) = self.wins.get_mut(&k) {
                w.move_to(at.0, at.1);
            }
            placed.push(k);
        }
        for k in side {
            let group: Vec<Rect> = placed.iter().filter_map(|k| self.rect(*k)).collect();
            let w = self.rect(k).map_or(0, |r| r.w);
            let bx = layout::beside(&group, w, s);
            if let Some(win) = self.wins.get_mut(&k) {
                win.move_to(bx, y);
            }
            placed.push(k);
        }
        for k in placed {
            if let Some(w) = self.win(k).and_then(|w| w.window.as_ref()) {
                crate::win::order_front(w, self.on_top);
            }
        }
        if let Some(w) = self.win(Kind::Main).and_then(|w| w.window.as_ref()) {
            w.focus_window();
        }
    }

    /// When a display is plugged in, unplugged or changes resolution, the
    /// system can push windows one by one; the ones that were docked to the
    /// main window go back around it, for a couple of seconds while it
    /// settles.
    fn keep_docked(&mut self) {
        let screens = self.screens();
        self.redock(screens);
    }

    fn redock(&mut self, screens: Vec<Rect>) {
        // A full-screen G-Drop gets its own Space, which hides the menu bar
        // and changes the visible area: that's not a display change, and
        // pulling it back beside the main window flips Spaces back and forth.
        if self.drag.is_some() || self.resize.is_some() || screens.is_empty() || self.drop.fullscreen {
            return;
        }
        if screens != self.docked.0 && !self.docked.0.is_empty() {
            self.docked.2 = Some(Instant::now() + Duration::from_secs(2));
        }
        self.docked.0 = screens;
        let Some(m) = self.rect(Kind::Main) else { return };
        if self.docked.2.is_some_and(|t| Instant::now() < t) {
            for (k, dx, dy) in self.docked.1.clone() {
                if self.rect(k).is_some_and(|r| (r.x, r.y) != (m.x + dx, m.y + dy)) {
                    if let Some(w) = self.wins.get_mut(&k) {
                        w.move_to(m.x + dx, m.y + dy);
                    }
                }
            }
            if self.window_lost() {
                self.gather();
            }
            return;
        }
        self.docked.2 = None;
        let kinds: Vec<Kind> = [Kind::Main, Kind::Eq, Kind::Playlist, Kind::Library, Kind::Vis].into_iter().filter(|k| self.rect(*k).is_some()).collect();
        let rects: Vec<Rect> = kinds.iter().filter_map(|k| self.rect(*k)).collect();
        self.docked.1 = layout::docked_group(&rects, 0).into_iter().skip(1).map(|i| (kinds[i], rects[i].x - m.x, rects[i].y - m.y)).collect();
    }

    /// Whether any open window is off every screen (a display unplugged,
    /// a window dragged away).
    pub(crate) fn window_lost(&self) -> bool {
        let screens = self.screens();
        !screens.is_empty()
            && [Kind::Main, Kind::Eq, Kind::Playlist, Kind::Library, Kind::Vis].into_iter().filter_map(|k| self.rect(k)).any(|r| {
                // Its title bar must show enough to grab.
                !screens.iter().any(|s| r.x < s.right() - 40 && r.right() > s.x + 40 && r.y >= s.y - 10 && r.y < s.bottom() - 20)
            })
    }

    /// Every display, in desktop units; the main one first.
    fn screens(&self) -> Vec<Rect> {
        let Some(w) = self.win(Kind::Main).and_then(|w| w.window.as_ref()) else { return Vec::new() };
        let primary = w.primary_monitor();
        primary.iter().cloned().chain(w.available_monitors().filter(|m| Some(m) != primary.as_ref())).map(|m| {
            let (x, y, w, h) = crate::win::monitor_area(&m);
            Rect { x, y, w, h }
        }).collect()
    }

    /// The display the main window is on, in desktop units.
    fn screen(&self) -> Option<Rect> {
        let m = self.win(Kind::Main)?.window.as_ref()?.current_monitor()?;
        let (x, y, w, h) = crate::win::monitor_area(&m);
        Some(Rect { x, y, w, h })
    }

    /// Moves the main-stack windows at or below `from`'s bottom edge by `dy`.
    /// Only windows docked into the same stack as `from` (`k`'s place
    /// before the change) move; one parked elsewhere stays put.
    fn shift_below(&mut self, k: Kind, from: Rect, dy: i32) {
        let stack: Vec<(Kind, Rect)> = [Kind::Main, Kind::Eq, Kind::Playlist].into_iter().filter(|o| *o != k).filter_map(|o| Some((o, self.rect(o)?))).collect();
        let mut rects: Vec<Rect> = stack.iter().map(|s| s.1).collect();
        rects.push(from);
        let start = if k == Kind::Main { rects.len() - 1 } else { stack.iter().position(|s| s.0 == Kind::Main).unwrap_or(rects.len() - 1) };
        let group = layout::docked_group(&rects, start);
        if !group.contains(&(rects.len() - 1)) {
            return;
        }
        let below: Vec<Kind> = group.into_iter().filter_map(|i| stack.get(i)).filter(|(_, r)| r.y >= from.bottom() - 1).map(|s| s.0).collect();
        for o in below {
            if let Some(w) = self.wins.get_mut(&o) {
                let (x, y) = w.pos;
                w.move_to(x, y + dy);
            }
        }
    }

    /// Rolls a window up to its title bar, or back down. Windows docked
    /// below follow, as in Winamp.
    fn toggle_shade(&mut self, k: Kind) {
        let before = self.rect(k);
        match k {
            Kind::Main => self.main.shade = !self.main.shade,
            Kind::Eq => self.eq.shade = !self.eq.shade,
            Kind::Playlist => self.pl.shade = !self.pl.shade,
            _ => return,
        }
        self.resize_window(k);
        let (Some(before), Some(w)) = (before, self.win(k)) else { return };
        let scale = w.scale();
        let new_h = (self.logical_size(k).height as f64 * scale) as i32;
        // Everything docked directly below moves by the change in height.
        self.shift_below(k, before, new_h - before.h);
    }

    /// Ctrl+D, Winamp's double size: between normal and twice as big.
    fn toggle_double(&mut self) {
        self.set_size(if self.size == 2.0 { 1.0 } else { 2.0 });
    }

    /// ⌘+ / ⌘− (Ctrl on Windows and Linux), as in a browser: 10% a step.
    fn zoom_step(&mut self, steps: i32) {
        let tenths = ((self.size * 10.0).round() as i32 + steps).clamp(10, 30);
        self.set_size(tenths as f64 / 10.0);
        self.say_f("SIZE: {}%", &[&(self.size * 100.0).round()]);
    }

    pub(crate) fn set_size(&mut self, size: f64) {
        self.size = size.clamp(1.0, 3.0);
        self.close_menus();
        let top = self.win(Kind::Main).map(|w| w.pos);
        for k in [Kind::Main, Kind::Eq, Kind::Playlist, Kind::Library, Kind::Vis, Kind::Jump, Kind::Info] {
            self.resize_window(k);
        }
        // Keep the stack docked under the main window.
        if let (Some((x, y)), Some(scale)) = (top, self.win(Kind::Main).map(Win::scale)) {
            let mut next = y + (self.logical_size(Kind::Main).height as f64 * scale) as i32;
            for k in [Kind::Eq, Kind::Playlist] {
                if self.win(k).is_some_and(|w| w.visible) {
                    if let Some(w) = self.wins.get_mut(&k) {
                        w.move_to(x, next);
                    }
                    next += (self.logical_size(k).height as f64 * scale) as i32;
                }
            }
        }
        // The library and G-Drop stay docked beside the stack.
        for k in [Kind::Library, Kind::Vis] {
            if self.win(k).is_some_and(|w| w.visible) {
                self.attach(k);
            }
        }
        self.mark_all();
    }

    pub(crate) fn toggle_on_top(&mut self) {
        self.on_top = !self.on_top;
        let level = if self.on_top { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal };
        for w in self.wins.values() {
            if let Some(w) = &w.window {
                w.set_window_level(level);
            }
        }
    }

    // ---- playback -------------------------------------------------------

    /// What the audio engine plays for an entry: a file, or a network
    /// track read from the local server (opened on the decoder thread).
    fn input_for(&self, entry: &Entry) -> Result<Input, String> {
        match &entry.url {
            Some(path) => {
                let Some(url) = self.backend.as_ref().map(|b| b.url(path)) else { return Err("no network".into()) };
                let duration = entry.duration.filter(|_| !entry.live);
                Ok(Input::Later(Box::new(move || {
                    let open = move |at: Option<f64>| -> anyhow::Result<Input> {
                        let url = match at {
                            Some(secs) => format!("{url}{}from={secs:.3}", if url.contains('?') { '&' } else { '?' }),
                            None => url.clone(),
                        };
                        let src = crate::stream::HttpSource::open(&url)?;
                        let ext = src.ext().or_else(|| crate::stream::ext_of(&url));
                        // SoundCloud's AAC is HLS fragments joined into one file.
                        let fragmented = ext.as_deref() == Some("m4a") && url.contains("/media/soundcloud/");
                        let start = src.start();
                        Ok(Input::Media { source: Box::new(src), ext, duration, fragmented, start, reopen: None })
                    };
                    let mut input = open(None)?;
                    // Its pieces can be opened from part way: a far seek jumps there.
                    if let Input::Media { fragmented: true, reopen, .. } = &mut input {
                        let open = std::sync::Arc::new(open);
                        *reopen = Some(std::sync::Arc::new(move |secs: f64| open(Some(secs))));
                    }
                    Ok(input)
                })))
            }
            None => Input::file(&entry.path).map_err(|e| e.to_string()),
        }
    }

    pub(crate) fn load_current(&mut self, play: bool) {
        let Some(entry) = self.playlist.current().cloned() else { return };
        // The list follows the track that plays, as Winamp's did.
        self.pl.reveal(self.playlist.current);
        self.mark(Kind::Playlist);
        // A seek still on its way was for the track before.
        self.seek_wait = None;
        match self.input_for(&entry) {
            Ok(input) => {
                if play && entry.url.is_some() {
                    // Until the first sound (cleared on Started); a long mix can take a few seconds.
                    self.main.message = Some(tm(CONNECTING).into());
                    self.message_until = Some(Instant::now() + Duration::from_secs(90));
                }
                let id = self.player.load(input, play);
                self.ids.insert(id, self.playlist.current);
                match self.resume.clone() {
                    // The engine opens the track before it takes the seek, so it starts there.
                    Some((i, path, url, at)) if i == self.playlist.current && path == entry.path && url == entry.url => {
                        log::info!("resume: opening track {i} at {at:.1}s");
                        self.player.seek(at);
                        self.seek_wait = Some((at, Instant::now()));
                        if entry.duration.is_some() {
                            self.main.duration = entry.duration;
                        }
                    }
                    _ => {
                        if self.resume.take().is_some() {
                            log::info!("resume: another track was picked");
                        }
                    }
                }
                // An endless stream has no "next"; anything else lines up the next
                // (when it plays: lined up behind a track that can't open, the
                // next would start by itself; Started lines it up later).
                if play && !entry.live {
                    self.queue_after(self.playlist.current);
                }
            }
            Err(e) => self.say_f("CAN'T OPEN: {}", &[&e]),
        }
        self.update_title();
    }

    pub(crate) fn queue_after(&mut self, index: usize) {
        if self.stop_after {
            return;
        }
        let Some(next) = self.playlist.after(index) else { return };
        let Some(entry) = self.playlist.entries.get(next).cloned() else { return };
        if entry.live {
            return;
        }
        if let Ok(input) = self.input_for(&entry) {
            let id = self.player.queue(input);
            self.ids.insert(id, next);
        }
    }

    pub(crate) fn update_title(&mut self) {
        self.main.title = match self.playlist.current() {
            Some(e) => {
                let len = e.duration.map(|d| format!(" ({})", clock(d))).unwrap_or_default();
                format!("{}. {}{}", self.playlist.current + 1, e.display(), len)
            }
            None => "GAMP".into(),
        };
        self.main.scroll = 0;
        self.title_scroll = None;
        self.sync_playlist();
        self.mark(Kind::Main);
    }

    /// Copies the play queue into the playlist window's view.
    pub(crate) fn sync_playlist(&mut self) {
        if self.jump.open {
            self.jump_filter();
        }
        self.pl.tracks = self.playlist.entries.iter().map(|e| plw::Track { title: e.display(), length: e.duration.map(clock).unwrap_or_default() }).collect();
        self.pl.current = (!self.playlist.entries.is_empty()).then_some(self.playlist.current);
        self.pl.selected.retain(|i| *i < self.playlist.entries.len());
        self.pl.top = self.pl.top.min(self.pl.max_top());
        let total: f64 = self.playlist.entries.iter().filter_map(|e| e.duration).sum();
        let sel: f64 = self.pl.selected.iter().filter_map(|i| self.playlist.entries.get(*i)?.duration).sum();
        self.pl.running = format!("{}/{}", clock(sel), clock(total));
        self.mark(Kind::Playlist);
    }

    pub fn open_files(&mut self, paths: Vec<PathBuf>, replace: bool) {
        if let Some(skin) = paths.iter().find(|p| p.extension().is_some_and(|e| matches!(e.to_string_lossy().to_lowercase().as_str(), "wsz" | "zip"))) {
            self.load_skin(Some(skin.clone()));
            return;
        }
        if let Some(eq) = paths.iter().find(|p| p.extension().is_some_and(|e| matches!(e.to_string_lossy().to_lowercase().as_str(), "eqf" | "q1"))) {
            self.import_presets(&eq.clone());
            return;
        }
        if let Some(list) = paths.iter().find(|p| p.extension().is_some_and(|e| matches!(e.to_string_lossy().to_lowercase().as_str(), "m3u" | "m3u8" | "pls"))) {
            let entries: Vec<Entry> = playlist::read_list(list).into_iter().map(|l| self.listed_entry(l)).collect();
            if entries.is_empty() {
                self.main.message = Some(tm("EMPTY PLAYLIST").into());
                return;
            }
            if replace || self.playlist.entries.is_empty() {
                self.playlist.set(entries);
                self.sync_playlist();
                self.load_current(true);
            } else {
                self.playlist.append(entries);
                self.sync_playlist();
                self.queue_after(self.playlist.current);
            }
            return;
        }
        let entries: Vec<Entry> = playlist::expand(&paths).iter().map(|p| Entry::read(p)).collect();
        if entries.is_empty() {
            return;
        }
        if replace || self.playlist.entries.is_empty() {
            self.playlist.set(entries);
            self.sync_playlist();
            self.load_current(true);
        } else {
            self.playlist.append(entries);
            self.sync_playlist();
            // Something new after the current track: line it up.
            self.queue_after(self.playlist.current);
        }
    }

    /// A playlist line as a queue entry: a file is read for its tags; a
    /// link to this app's own server is kept as it is (the session saves
    /// the same `/media/...` paths); any other address plays through the
    /// server like a pasted stream, so saved station lists work.
    fn listed_entry(&self, l: playlist::Listed) -> Entry {
        match l {
            playlist::Listed::File(p) => Entry::read(&p),
            playlist::Listed::Link { url, title, duration } => {
                let (media, live) = match gamp_core::sources::own_media_path(&url) {
                    Some(path) => (path.clone(), path.starts_with("/media/remote/radio/")),
                    None => gamp_core::sources::link_media_url(&url),
                };
                let (artist, title) = match title.split_once(" - ") {
                    Some((a, t)) if !live => (a.trim().to_owned(), t.trim().to_owned()),
                    _ => (String::new(), title),
                };
                Entry::remote(media, title, artist, duration, live)
            }
        }
    }

    pub fn load_skin(&mut self, path: Option<PathBuf>) {
        let result = match &path {
            None => Ok(Skin::default_skin()),
            Some(p) => std::fs::read(p).map_err(anyhow::Error::from).and_then(|b| Skin::from_wsz(&b, &Skin::default_skin())),
        };
        match result {
            Ok(s) => {
                self.skin = s;
                self.skin_path = path.as_ref().map(|p| {
                    // Remember the kept copy in the skins folder.
                    let kept = self.data_dir.join("skins").join(p.file_name().unwrap_or_default());
                    if kept.exists() { kept } else { p.clone() }
                });
                // Keep it, so it is in the Skins menu next time.
                if let Some(p) = &path {
                    let dir = self.data_dir.join("skins");
                    if !p.starts_with(&dir) && !p.starts_with(self.data_dir.join(crate::library::MUSEUM_DIR)) {
                        let _ = std::fs::create_dir_all(&dir);
                        if let Some(name) = p.file_name() {
                            let _ = std::fs::copy(p, dir.join(name));
                        }
                    }
                }
                self.mark_all();
            }
            Err(e) => self.main.message = Some(tmf("NOT A SKIN: {}", &[&e])),
        }
    }

    fn pick_files(&mut self, replace: bool) {
        self.ask(Ask::OpenFiles { replace });
    }

    fn pick_folder(&mut self, replace: bool) {
        self.ask(Ask::OpenFolder { replace });
    }

    /// Opens a dialog; its answer arrives in `answered`.
    pub(crate) fn ask(&mut self, ask: Ask) {
        let data = self.data_dir.clone();
        #[cfg(target_os = "macos")]
        if let Some(proxy) = self.proxy.clone() {
            dispatch2::DispatchQueue::main().exec_async(move || {
                let _ = proxy.send_event(crate::backend::UserEvent::Dialog(crate::dialog::run(ask, &data)));
            });
            return;
        }
        let answer = crate::dialog::run(ask, &data);
        self.answered(answer);
    }

    pub(crate) fn answered(&mut self, a: crate::dialog::Answer) {
        let first = a.paths.first().cloned();
        match a.ask {
            Ask::OpenFiles { replace } | Ask::OpenFolder { replace } if !a.paths.is_empty() => self.open_files(a.paths, replace),
            Ask::PlaylistFile if first.is_some() => self.open_files(a.paths, true),
            Ask::SkinFile => {
                if let Some(f) = first {
                    self.load_skin(Some(f));
                }
            }
            Ask::PresetFile => {
                if let Some(f) = first {
                    self.import_presets(&f);
                }
            }
            Ask::MusicFolder => {
                if let (Some(dir), Some(b)) = (first, &self.backend) {
                    let mut f: Vec<String> = self.lib.config["folders"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
                    f.push(dir.to_string_lossy().into_owned());
                    b.post("/api/config", serde_json::json!({ "folders": f }), true);
                    b.post("/api/library/rescan", serde_json::json!({}), false);
                    b.scan_status();
                }
            }
            Ask::SavePlaylist => {
                if let Some(path) = first {
                    self.playlist_saved(path);
                }
            }
            Ask::SavePreset => {
                if let Some(path) = first {
                    self.preset_saved(path);
                }
            }
            Ask::Update(_) if a.yes => self.update_install(),
            _ => {}
        }
        self.mark_all();
    }

    fn play(&mut self) {
        match self.player.status().state {
            State::Paused => self.player.play(),
            // Play while playing pauses (Winamp restarted the track).
            State::Playing => self.player.pause(),
            State::Stopped => {
                if self.playlist.entries.is_empty() {
                    self.pick_files(true);
                } else if self.player.status().track.is_some() {
                    self.player.play();
                } else {
                    self.load_current(true);
                }
            }
        }
    }

    /// Stop, by hand: also lets go of a place Gamp reopened at.
    pub(crate) fn stop(&mut self) {
        self.seek_wait = None;
        self.resume = None;
        self.reopened = false;
        self.player.stop();
    }

    fn pause(&mut self) {
        match self.player.status().state {
            State::Playing => self.player.pause(),
            State::Paused => self.player.play(),
            State::Stopped => {}
        }
    }

    fn step(&mut self, forward: bool) {
        let moved = if forward { self.playlist.next() } else { self.playlist.previous() };
        if moved {
            self.load_current(self.main.state != PlayState::Stopped);
        }
    }

    pub(crate) fn play_index(&mut self, i: usize) {
        // The track Gamp reopened on, picked: it goes on from its place.
        if i == self.playlist.current && self.reopened && self.player.status().state == State::Paused {
            return self.player.play();
        }
        if i < self.playlist.entries.len() {
            self.playlist.current = i;
            self.load_current(true);
        }
    }

    fn seek_by(&mut self, secs: f64) {
        let s = self.player.status();
        self.seek((s.position + secs).max(0.0));
    }

    /// A seek; a slow one (far into a long stream) shows SEEKING... until it lands.
    pub(crate) fn seek(&mut self, secs: f64) {
        // A place picked by hand replaces the one Gamp reopened at.
        if self.resume.take().is_some() {
            log::info!("resume: a new place was picked ({secs:.1}s)");
        }
        self.player.seek(secs);
        if self.player.status().format.is_some_and(|f| f.seekable) {
            self.seek_wait = Some((secs, Instant::now()));
        }
    }

    fn seek_tick(&mut self) {
        let Some((to, at)) = self.seek_wait else { return };
        let s = self.player.status();
        let waited = at.elapsed();
        let landed = (s.position - to).abs() < 2.0 && s.track.is_some();
        if landed || waited > Duration::from_secs(60) {
            self.seek_wait = None;
            if self.main.message.as_deref() == Some(tm(SEEKING)) {
                self.main.message = None;
                self.mark(Kind::Main);
            }
        } else if waited > Duration::from_millis(600) && self.main.message.is_none() {
            self.main.message = Some(tm(SEEKING).into());
            self.mark(Kind::Main);
        }
    }

    fn set_volume(&mut self, v: f32, say: bool) {
        self.main.volume = v.clamp(0.0, 1.0);
        self.eq.volume = self.main.volume;
        self.player.set_volume(self.main.volume);
        if say {
            self.main.message = Some(tmf("VOLUME: {}%", &[&(self.main.volume * 100.0).round()]));
        }
        self.mark(Kind::Main);
        self.mark(Kind::Eq);
    }

    fn set_balance(&mut self, b: f32, say: bool) {
        self.main.balance = b.clamp(-1.0, 1.0);
        self.eq.balance = self.main.balance;
        self.player.set_balance(self.main.balance);
        if say {
            let pct = (b.abs() * 100.0).round();
            self.main.message = Some(if pct == 0.0 { tm("BALANCE: CENTER").into() } else { tmf(if b < 0.0 { "BALANCE: {}% LEFT" } else { "BALANCE: {}% RIGHT" }, &[&pct]) });
        }
        self.mark(Kind::Main);
        self.mark(Kind::Eq);
    }

    fn apply_eq(&mut self) {
        self.player.set_eq(EqSettings { on: self.eq.on, preamp: self.eq.preamp, bands: self.eq.bands });
        self.mark(Kind::Eq);
    }

    fn set_shuffle(&mut self, on: bool) {
        self.main.shuffle = on;
        self.playlist.shuffle = on;
        self.playlist.reshuffle();
        self.mark(Kind::Main);
    }

    fn set_repeat(&mut self, on: bool) {
        self.main.repeat = on;
        self.playlist.repeat = on;
        self.mark(Kind::Main);
    }

    fn poll_audio(&mut self) {
        while let Ok(e) = self.player.events().try_recv() {
            match e {
                AudioEvent::Started { id, format } => {
                    if !format.seekable {
                        // It can't be taken to a place: it plays from where it is.
                        self.seek_wait = None;
                        self.resume = None;
                    }
                    if let Some(&i) = self.ids.get(&id) {
                        self.playlist.current = i;
                        self.update_title();
                        self.queue_after(i);
                    }
                    self.main.kbps = format.kbps;
                    self.main.khz = Some((format.rate as f32 / 1000.0).round() as u32);
                    self.main.stereo = Some(format.channels >= 2);
                    self.main.duration = format.duration;
                    if self.main.message.as_ref().is_some_and(|m| m.starts_with(tm(CONNECTING))) {
                        self.main.message = None;
                        self.message_until = None;
                    }
                }
                AudioEvent::Finished { .. } => {
                    self.seek_wait = None;
                    if std::mem::take(&mut self.stop_after) {
                        self.say("STOPPED AFTER THE TRACK");
                    }
                }
                AudioEvent::Error { id, message } => {
                    log::warn!("playback: {message}");
                    if self.ids.get(&id) == Some(&self.playlist.current) {
                        self.seek_wait = None;
                        // Said even when the next track takes over, so a skip isn't a mystery.
                        // The track Gamp reopened on stays (Play tries it again, at its place).
                        if self.resume.is_none() && self.playlist.next() {
                            self.load_current(true);
                        }
                        self.say_f("CAN'T PLAY: {}", &[&message]);
                    }
                }
            }
            self.mark(Kind::Main);
        }
        let s = self.player.status();
        let state = match s.state {
            State::Playing => PlayState::Playing,
            State::Paused => PlayState::Paused,
            // Reopened where it was, still opening (a stream takes a moment):
            // it shows paused at that place from the start.
            State::Stopped if self.reopened && self.resume.is_some() => PlayState::Paused,
            State::Stopped => PlayState::Stopped,
        };
        if state != self.main.state || (s.position as u64) != (self.main.elapsed as u64) {
            self.mark(Kind::Main);
            self.pl.mini_time = if state == PlayState::Stopped { String::new() } else { format!("{:>5}", clock(s.position)) };
            self.mark(Kind::Playlist);
        }
        self.main.state = state;
        self.main.elapsed = match &self.resume {
            Some(r) if s.track.is_none() => r.3,
            _ => s.position,
        };
        if state == PlayState::Playing {
            self.reopened = false;
        }
        // While a seek is on its way (slow, far into a long stream), the time
        // and the position bar show where it's going, and that's the place.
        if self.main.pressed != Some(Control::Position) {
            let target = self.seek_wait.map(|w| w.0).or(self.resume.as_ref().map(|r| r.3));
            let to = match (target, self.main.duration) {
                (Some(to), Some(d)) if d > 0.0 => Some((to / d) as f32),
                _ => None,
            };
            if to != self.main.seeking {
                self.main.seeking = to;
                self.mark(Kind::Main);
            }
        }
        // The place Gamp reopened at, reached: from now on, what plays is the place.
        // Or it never got there (a stream that can't seek) and it plays anyway.
        if let Some(r) = &self.resume {
            let there = s.track.is_some() && (s.position - r.3).abs() < 2.0;
            if there || state == PlayState::Playing && self.seek_wait.is_none() {
                log::info!("resume: {} ({:.1}s)", if there { "there" } else { "gave up, playing" }, s.position);
                self.resume = None;
            }
        }
        if state != PlayState::Stopped && s.track.is_some() && self.resume.is_none() {
            if let Some(e) = self.playlist.current() {
                self.heard = Some((self.playlist.current, e.path.clone(), e.url.clone(), self.seek_wait.map_or(s.position, |w| w.0)));
            }
        }
        if let Some(f) = s.format {
            self.main.duration = f.duration;
        }
    }

    pub(crate) fn toggle_stop_after(&mut self) {
        self.stop_after = !self.stop_after;
        if self.stop_after {
            self.player.unqueue();
            self.say("STOP AFTER THIS TRACK: ON");
        } else {
            self.queue_after(self.playlist.current);
            self.say("STOP AFTER THIS TRACK: OFF");
        }
    }

    /// Starts (minutes > 0) or cancels the sleep timer.
    pub(crate) fn set_sleep(&mut self, minutes: u64) {
        self.player.set_volume(self.main.volume);
        self.sleep_at = (minutes > 0).then(|| (Instant::now() + Duration::from_secs(minutes * 60), SLEEP_FADE));
        if minutes > 0 { self.say_f("SLEEP TIMER: {} MINUTES", &[&minutes]) } else { self.say("SLEEP TIMER: OFF") }
    }

    /// Shift+V, as in Winamp: fade out over a few seconds, then stop.
    pub(crate) fn fade_stop(&mut self) {
        if self.main.state == PlayState::Playing {
            self.sleep_at = Some((Instant::now() + Duration::from_secs(3), 3.0));
        } else {
            self.player.stop();
        }
    }

    /// The sleep timer: the last `fade` seconds fade out, then it stops.
    fn sleep_tick(&mut self) {
        let Some((at, fade)) = self.sleep_at else { return };
        let left = at.saturating_duration_since(Instant::now()).as_secs_f32();
        if left > 0.0 {
            if left < fade {
                self.player.set_volume(self.main.volume * left / fade);
            }
            return;
        }
        self.sleep_at = None;
        self.player.stop();
        self.player.set_volume(self.main.volume);
        if fade == SLEEP_FADE {
            self.say("SLEEP TIMER: GOOD NIGHT");
        }
    }

    /// Animation and housekeeping, about 30 times a second while busy.
    pub fn tick(&mut self) {
        self.keep_docked();
        self.poll_audio();
        self.sleep_tick();
        self.seek_tick();
        self.update_poll();
        self.update_tick();
        if self.message_until.is_some_and(|t| Instant::now() >= t) {
            self.message_until = None;
            if self.main.pressed.is_none() {
                self.main.message = None;
                self.mark(Kind::Main);
            }
        }
        self.media_tick();
        self.lib_poll();
        if self.pending_scan.is_some_and(|t| Instant::now() >= t) {
            self.pending_scan = None;
            if let Some(b) = &self.backend {
                b.scan_status();
            }
        }
        if self.pending_refresh.is_some_and(|t| Instant::now() >= t) {
            self.pending_refresh = None;
            if let Some(b) = &self.backend {
                b.refresh_sources();
                b.config();
            }
        }
        let now = Instant::now();
        if self.main.state == PlayState::Playing && self.main.vis_mode != VisMode::Off {
            self.player.waveform(&mut self.wave);
            match self.main.vis_mode {
                VisMode::Analyzer => {
                    self.spectrum.compute(&self.wave, &mut self.spec);
                    analysis::bars(&self.spec, self.player.output_rate(), 19, &mut self.bars);
                    self.main.analyzer.update(&self.bars);
                }
                _ => {
                    self.main.wave.clear();
                    self.main.wave.extend_from_slice(&self.wave[self.wave.len() - 256..]);
                }
            }
            self.mark(Kind::Main);
        } else if self.main.analyzer.peaks.iter().any(|p| *p > 0.0) {
            self.main.analyzer.decay();
            self.mark(Kind::Main);
        }
        // A message scrolls from its start; the title picks up where it was.
        let shown = self.main.message.as_ref().unwrap_or(&self.main.title);
        if *shown != self.shown {
            let was_title = self.shown == self.main.title;
            self.shown = shown.clone();
            if self.main.message.is_some() {
                if was_title {
                    self.title_scroll = Some(self.main.scroll);
                }
                self.main.scroll = 0;
            } else if let Some(s) = self.title_scroll.take() {
                self.main.scroll = s;
            }
            self.mark(Kind::Main);
        }
        if now.duration_since(self.marquee_at) >= MARQUEE_STEP {
            self.marquee_at = now;
            if self.shown.chars().count() * 5 > 154 {
                self.main.scroll += 5;
                self.mark(Kind::Main);
            }
            if self.wins.get(&Kind::Library).is_some_and(|w| w.visible) {
                self.lib_dots();
            }
            // Still connecting: the dots go round, so it's plainly working.
            let connecting = tm(CONNECTING);
            if let Some(m) = self.main.message.as_mut().filter(|m| m.starts_with(connecting)) {
                let dots = (m.len() - connecting.len() + 1) % 4;
                *m = format!("{connecting}{}", ".".repeat(dots));
            }
        }
        if now.duration_since(self.blink_at) >= Duration::from_millis(1000) {
            self.blink_at = now;
            // Reopened where it was: the time stays up, plain to see, until it plays.
            self.main.blink_on = self.reopened || !self.main.blink_on;
            if self.main.state == PlayState::Paused {
                self.mark(Kind::Main);
            }
        }
    }

    pub fn busy(&self) -> bool {
        self.pending_refresh.is_some() || self.pending_scan.is_some() || self.main.state != PlayState::Stopped || self.main.analyzer.peaks.iter().any(|p| *p > 0.0) || self.main.message.as_ref().unwrap_or(&self.main.title).chars().count() * 5 > 154 || self.drag.is_some()
    }

    // ---- pointer --------------------------------------------------------

    pub fn cursor_moved(&mut self, k: Kind, x: f64, y: f64, el: Option<&ActiveEventLoop>) {
        if let Some(w) = self.win_mut(k) {
            w.set_cursor(x, y);
        }
        if self.drag.as_ref().is_some_and(|d| d.kind == k) {
            return self.drag_windows();
        }
        if self.resize.is_some() && k == Kind::Playlist {
            return self.resize_playlist();
        }
        if self.resize.is_some() && k == Kind::Library {
            return self.resize_library();
        }
        if self.resize.is_some() && k == Kind::Vis {
            return self.drop_resize();
        }
        let (cx, cy) = self.win(k).map(|w| w.cursor).unwrap_or((0, 0));
        match k {
            Kind::Main if self.main.pressed.is_some() => self.main_drag(cx),
            Kind::Eq if self.eq.pressed.is_some() => self.eq_drag(cx, cy),
            Kind::Playlist => self.pl_move(cx, cy),
            Kind::Library => self.lib_drag(cx, cy),
            Kind::Menu(i) => self.menu_hover(i, cx, cy, el),
            _ => {}
        }
    }

    pub(crate) fn double_click(&mut self, k: Kind, x: i32, y: i32) -> bool {
        let dbl = self.last_click.is_some_and(|(t, lk, lx, ly)| lk == k && t.elapsed() < DOUBLE_CLICK && (lx - x).abs() < 4 && (ly - y).abs() < 4);
        self.last_click = Some((Instant::now(), k, x, y));
        dbl
    }

    /// Every window that docks and snaps to the others.
    pub(crate) const DOCKABLE: [Kind; 7] = [Kind::Main, Kind::Eq, Kind::Playlist, Kind::Library, Kind::Vis, Kind::Jump, Kind::Info];

    /// Starts moving a window (and, for the main window, everything docked
    /// to it).
    pub(crate) fn start_drag(&mut self, k: Kind) {
        let Some(start) = self.win(k).map(Win::global_cursor) else { return };
        let kinds = Self::DOCKABLE;
        let mut group = vec![(k, self.win(k).map(|w| w.pos).unwrap_or_default())];
        if k == Kind::Main {
            let visible: Vec<Kind> = kinds.iter().copied().filter(|k| self.rect(*k).is_some()).collect();
            let rects: Vec<Rect> = visible.iter().filter_map(|k| self.rect(*k)).collect();
            if let Some(me) = visible.iter().position(|v| *v == Kind::Main) {
                group = layout::docked_group(&rects, me).into_iter().map(|i| (visible[i], (rects[i].x, rects[i].y))).collect();
            }
        }
        self.drag = Some(Drag { kind: k, start, group });
    }

    fn drag_windows(&mut self) {
        let Some(d) = &self.drag else { return };
        let Some(now) = self.win(d.kind).map(Win::global_cursor) else { return };
        let (dx, dy) = ((now.0 - d.start.0).round() as i32, (now.1 - d.start.1).round() as i32);
        let group = d.group.clone();
        // Snap the moved group to the other windows and the screen.
        let moving: Vec<Rect> = group
            .iter()
            .filter_map(|(k, (x, y))| self.rect(*k).map(|r| Rect { x: x + dx, y: y + dy, ..r }))
            .collect();
        let others: Vec<Rect> = Self::DOCKABLE.iter().filter(|k| !group.iter().any(|(g, _)| g == *k)).filter_map(|k| self.rect(*k)).collect();
        let screen = self.screen();
        let scale = self.win(d.kind).map(Win::scale).unwrap_or(1.0);
        let (sx, mut sy) = layout::snap(&moving, &others, screen, (10.0 * scale) as i32);
        // macOS keeps each window below the menu bar on its own, which would
        // push the main window down over the ones docked under it: the
        // whole group stops there instead (unless a screen above takes it).
        if cfg!(target_os = "macos") {
            sy += layout::below_top(&moving.iter().map(|r| Rect { x: r.x + sx, y: r.y + sy, ..*r }).collect::<Vec<_>>(), &self.screens());
        }
        for (k, (x, y)) in group {
            if let Some(w) = self.wins.get_mut(&k) {
                w.move_to(x + dx + sx, y + dy + sy);
            }
        }
    }

    pub fn press(&mut self, k: Kind, el: Option<&ActiveEventLoop>) {
        // A click anywhere else closes an open menu.
        if !matches!(k, Kind::Menu(_)) && !self.menus.is_empty() {
            self.close_menus();
            return;
        }
        let (x, y) = self.win(k).map(|w| w.cursor).unwrap_or((0, 0));
        match k {
            Kind::Main => self.main_press(x, y, el),
            Kind::Eq => self.eq_press(x, y, el),
            Kind::Playlist => self.pl_press(x, y),
            Kind::Library => self.lib_press(x, y),
            Kind::Vis => self.drop_press(x, y),
            Kind::Jump => self.jump_press(x, y),
            Kind::Info => self.info_press(x, y),
            Kind::Menu(_) => {}
        }
    }

    pub fn release(&mut self, k: Kind, el: Option<&ActiveEventLoop>) {
        if self.drag.take().is_some() || self.resize.take().is_some() {
            return;
        }
        match k {
            Kind::Main => self.main_release(el),
            Kind::Eq => self.eq_release(el),
            Kind::Playlist => self.pl_release(el),
            Kind::Library => self.lib_release(),
            Kind::Vis => self.drop_release(),
            Kind::Jump => self.jump_release(),
            Kind::Info => self.info_release(),
            Kind::Menu(i) => self.menu_release(i, el),
        }
    }

    /// Right-click: Winamp's main menu, wherever you are.
    pub fn context_menu(&mut self, k: Kind, el: Option<&ActiveEventLoop>) {
        let at = self.win(k).map(Win::global_cursor).unwrap_or_default();
        let items = match k {
            Kind::Library => self.lib_row_menu(),
            Kind::Playlist => self.pl_row_menu(),
            Kind::Vis => Some(self.drop_menu()),
            _ => None,
        }
        .unwrap_or_else(|| self.main_menu());
        self.open_menu(items, (at.0 as i32, at.1 as i32), el);
    }

    // ---- main window ------------------------------------------------------

    fn main_press(&mut self, x: i32, y: i32, el: Option<&ActiveEventLoop>) {
        let Some(c) = main_window::hit(x, y, self.main.shade) else { return };
        if c == Control::TitleBar {
            if self.double_click(Kind::Main, x, y) {
                self.toggle_shade(Kind::Main);
            } else {
                self.start_drag(Kind::Main);
            }
            return;
        }
        if c == Control::Options {
            let at = self.win(Kind::Main).map(|w| (w.pos.0 + (6.0 * w.scale() * self.size) as i32, w.pos.1 + (14.0 * w.scale() * self.size) as i32)).unwrap_or_default();
            let items = self.main_menu();
            self.open_menu(items, at, el);
            return;
        }
        self.main.pressed = Some(c);
        self.main_drag(x);
        self.mark(Kind::Main);
    }

    fn main_drag(&mut self, x: i32) {
        match self.main.pressed {
            Some(Control::Volume) => self.set_volume(main_window::volume_at(x), true),
            Some(Control::Balance) => self.set_balance(main_window::balance_at(x), true),
            Some(Control::Position) if self.main.duration.is_some() => {
                let p = main_window::position_at(x, self.main.shade);
                self.main.seeking = Some(p);
                let d = self.main.duration.unwrap_or(0.0);
                self.main.message = Some(tmf("SEEK TO: {}/{} ({}%)", &[&clock(d * p as f64), &clock(d), &(p * 100.0).round()]));
                self.mark(Kind::Main);
            }
            _ => {}
        }
    }

    fn main_release(&mut self, el: Option<&ActiveEventLoop>) {
        let Some(c) = self.main.pressed.take() else { return };
        let (x, y) = self.win(Kind::Main).map(|w| w.cursor).unwrap_or((0, 0));
        let over = main_window::hit(x, y, self.main.shade) == Some(c);
        self.main.message = None;
        self.mark(Kind::Main);
        match c {
            Control::Position => {
                // The bar stays where it was let go until the seek lands.
                if let (Some(p), Some(d)) = (self.main.seeking, self.main.duration) {
                    self.seek(d * p as f64);
                }
            }
            Control::Volume | Control::Balance => {}
            _ if !over => {}
            Control::Previous => self.step(false),
            Control::Next => self.step(true),
            Control::Play => self.play(),
            Control::Pause => self.pause(),
            Control::Stop => self.stop(),
            Control::Eject => self.pick_files(true),
            Control::Shuffle => self.set_shuffle(!self.main.shuffle),
            Control::Repeat => self.set_repeat(!self.main.repeat),
            Control::Equalizer => self.toggle_window(Kind::Eq),
            Control::Playlist => self.toggle_window(Kind::Playlist),
            Control::Time => self.main.show_remaining = !self.main.show_remaining,
            Control::Vis => {
                self.main.vis_mode = match self.main.vis_mode {
                    VisMode::Analyzer => VisMode::Oscilloscope,
                    VisMode::Oscilloscope => VisMode::Off,
                    VisMode::Off => VisMode::Analyzer,
                }
            }
            Control::Close => self.quit = true,
            Control::Minimize => {
                for w in self.wins.values() {
                    if let Some(w) = &w.window {
                        w.set_minimized(true);
                    }
                }
            }
            Control::Shade => self.toggle_shade(Kind::Main),
            Control::Clutter('d') => self.toggle_double(),
            Control::Clutter('a') => self.toggle_on_top(),
            Control::Clutter('o') => {
                let at = self.win(Kind::Main).map(Win::global_cursor).unwrap_or_default();
                let items = self.main_menu();
                self.open_menu(items, (at.0 as i32, at.1 as i32), el);
            }
            Control::Clutter('v') => {
                let at = self.win(Kind::Main).map(Win::global_cursor).unwrap_or_default();
                let items = self.vis_menu();
                self.open_menu(items, (at.0 as i32, at.1 as i32), el);
            }
            _ => {}
        }
    }

    // ---- equalizer --------------------------------------------------------

    fn eq_press(&mut self, x: i32, y: i32, el: Option<&ActiveEventLoop>) {
        let Some(c) = eq_window::hit(x, y, self.eq.shade) else { return };
        if c == eq_window::Control::TitleBar {
            if self.double_click(Kind::Eq, x, y) {
                self.toggle_shade(Kind::Eq);
            } else {
                self.start_drag(Kind::Eq);
            }
            return;
        }
        if c == eq_window::Control::Presets {
            let at = self.win(Kind::Eq).map(|w| (w.pos.0 + (217.0 * w.scale() * self.size) as i32, w.pos.1 + (30.0 * w.scale() * self.size) as i32)).unwrap_or_default();
            let items = self.preset_menu();
            self.open_menu(items, at, el);
            return;
        }
        self.eq.pressed = Some(c);
        self.eq_drag(x, y);
        self.mark(Kind::Eq);
    }

    fn eq_drag(&mut self, x: i32, y: i32) {
        match self.eq.pressed {
            Some(eq_window::Control::Preamp) => {
                self.eq.preamp = eq_window::db_at(y);
                self.apply_eq();
            }
            Some(eq_window::Control::Band(i)) => {
                self.eq.bands[i] = eq_window::db_at(y);
                self.apply_eq();
            }
            Some(eq_window::Control::Volume) => self.set_volume(((x - 61) as f32 / 94.0).clamp(0.0, 1.0), false),
            Some(eq_window::Control::Balance) => self.set_balance((((x - 164) as f32 / 40.0).clamp(0.0, 1.0)) * 2.0 - 1.0, false),
            _ => {}
        }
    }

    fn eq_release(&mut self, _el: Option<&ActiveEventLoop>) {
        let Some(c) = self.eq.pressed.take() else { return };
        let (x, y) = self.win(Kind::Eq).map(|w| w.cursor).unwrap_or((0, 0));
        let over = eq_window::hit(x, y, self.eq.shade) == Some(c);
        self.mark(Kind::Eq);
        if !over {
            return;
        }
        match c {
            eq_window::Control::On => {
                self.eq.on = !self.eq.on;
                self.apply_eq();
            }
            eq_window::Control::Auto => self.eq.auto = !self.eq.auto,
            eq_window::Control::Close => self.toggle_window(Kind::Eq),
            eq_window::Control::Shade => self.toggle_shade(Kind::Eq),
            _ => {}
        }
    }

    // ---- playlist ---------------------------------------------------------

    fn pl_press(&mut self, x: i32, y: i32) {
        let Some(c) = plw::hit(&self.pl, x, y) else { return };
        match c {
            plw::Control::TitleBar => {
                if self.double_click(Kind::Playlist, x, y) {
                    self.toggle_shade(Kind::Playlist);
                } else {
                    self.start_drag(Kind::Playlist);
                }
                return;
            }
            plw::Control::Resize => {
                if let Some(w) = self.win(Kind::Playlist) {
                    self.resize = Some(Resize { start: w.global_cursor(), size: self.pl.size(), ..Default::default() });
                }
                return;
            }
            plw::Control::Track(i) => {
                let dbl = self.double_click(Kind::Playlist, x, y);
                if dbl {
                    self.play_index(i);
                } else if self.modifiers.shift_key() {
                    let anchor = self.pl.selected.iter().next().copied().unwrap_or(i);
                    let (a, b) = (anchor.min(i), anchor.max(i));
                    self.pl.selected = (a..=b).collect();
                } else if self.modifiers.control_key() || self.modifiers.super_key() {
                    if !self.pl.selected.remove(&i) {
                        self.pl.selected.insert(i);
                    }
                } else {
                    if !self.pl.selected.contains(&i) {
                        self.pl.selected = BTreeSet::from([i]);
                    }
                    self.pl_drag = Some(i);
                }
                self.sync_playlist();
            }
            plw::Control::Empty => {
                self.pl.selected.clear();
                self.sync_playlist();
            }
            plw::Control::Scroll => {
                self.pl.top = plw::scroll_top_at(&self.pl, y);
            }
            plw::Control::Menu(m) => {
                if self.pl.menu.is_none() {
                    // The bottom item sits over the button, so a plain
                    // click on ADD is "add files" (REM: remove selected,
                    // SEL: all, MISC: misc opts, LIST: open), like Winamp.
                    let h = self.pl.size().1 as i32;
                    self.pl.menu = Some((m, plw::menu_item(m, h, y)));
                }
            }
            _ => {}
        }
        self.pl.pressed = Some(c);
        self.mark(Kind::Playlist);
    }

    fn pl_move(&mut self, x: i32, y: i32) {
        let h = self.pl.size().1 as i32;
        if let Some((m, _)) = self.pl.menu {
            let hover = plw::menu_item(m, h, y).filter(|_| x >= m.x(self.pl.size().0 as i32) && x < m.x(self.pl.size().0 as i32) + 22);
            if self.pl.menu != Some((m, hover)) {
                self.pl.menu = Some((m, hover));
                self.mark(Kind::Playlist);
            }
            return;
        }
        match self.pl.pressed {
            Some(plw::Control::Scroll) => {
                self.pl.top = plw::scroll_top_at(&self.pl, y);
                self.mark(Kind::Playlist);
            }
            // Dragging selected tracks moves them.
            Some(plw::Control::Track(_)) => {
                let Some(from) = self.pl_drag else { return };
                let to = (((y - 23) / plw::ROW).max(0) as usize + self.pl.top).min(self.playlist.entries.len().saturating_sub(1));
                if to != from {
                    self.move_selection(to as isize - from as isize);
                    self.pl_drag = Some(to);
                }
            }
            _ => {}
        }
    }

    /// Moves the selected tracks by `delta` rows, keeping them together.
    fn move_selection(&mut self, delta: isize) {
        let n = self.playlist.entries.len() as isize;
        let sel: Vec<usize> = self.pl.selected.iter().copied().collect();
        if sel.is_empty() || delta == 0 {
            return;
        }
        let lo = *sel.first().unwrap() as isize;
        let hi = *sel.last().unwrap() as isize;
        let delta = delta.clamp(-lo, n - 1 - hi);
        if delta == 0 {
            return;
        }
        let current_path = self.playlist.current().map(Entry::key);
        let mut order: Vec<usize> = (0..n as usize).filter(|i| !self.pl.selected.contains(i)).collect();
        let insert_at = (lo + delta) as usize;
        let insert_at = insert_at.min(order.len());
        for (k, i) in sel.iter().enumerate() {
            order.insert(insert_at + k, *i);
        }
        let entries = std::mem::take(&mut self.playlist.entries);
        self.playlist.entries = order.iter().map(|i| entries[*i].clone()).collect();
        self.pl.selected = (insert_at..insert_at + sel.len()).collect();
        if let Some(p) = current_path {
            self.playlist.current = self.playlist.entries.iter().position(|e| e.key() == p).unwrap_or(0);
        }
        self.playlist.reshuffle();
        self.reordered();
    }

    fn pl_release(&mut self, el: Option<&ActiveEventLoop>) {
        self.pl_drag = None;
        let pressed = self.pl.pressed.take();
        self.mark(Kind::Playlist);
        if let Some((m, hover)) = self.pl.menu.take() {
            if let Some(i) = hover {
                self.pl_menu_action(m, i, el);
            }
            return;
        }
        let (x, y) = self.win(Kind::Playlist).map(|w| w.cursor).unwrap_or((0, 0));
        let over = plw::hit(&self.pl, x, y);
        match pressed {
            Some(plw::Control::Close) if over == pressed => self.toggle_window(Kind::Playlist),
            Some(plw::Control::Shade) if over == pressed => self.toggle_shade(Kind::Playlist),
            Some(plw::Control::Mini(m)) if over == pressed => match m {
                Mini::Previous => self.step(false),
                Mini::Play => self.play(),
                Mini::Pause => self.pause(),
                Mini::Stop => self.stop(),
                Mini::Next => self.step(true),
                Mini::Eject => self.pick_files(true),
            },
            _ => {}
        }
    }

    fn pl_menu_action(&mut self, m: PlMenu, i: usize, _el: Option<&ActiveEventLoop>) {
        let n = self.playlist.entries.len();
        match (m, i) {
            (PlMenu::Add, 0) => {
                // Winamp's "Add URL": the library's radio search takes any address.
                if !self.wins.get(&Kind::Library).is_some_and(|w| w.visible) {
                    self.toggle_window(Kind::Library);
                }
                self.lib_open(crate::library::Page::Search { sid: "radio".into(), title: "Internet Radio › Search".into() }, true);
            }
            (PlMenu::Add, 1) => self.pick_folder(false),
            (PlMenu::Add, 2) => self.pick_files(false),
            (PlMenu::Remove, 0) => self.clear_playlist(),
            (PlMenu::Remove, 1) => self.keep_only(self.pl.selected.clone()),
            (PlMenu::Remove, 2) => self.remove_selected(),
            (PlMenu::Remove, 3) => {
                // Remove files that are gone.
                let keep: BTreeSet<usize> = (0..n).filter(|i| self.playlist.entries[*i].url.is_some() || self.playlist.entries[*i].path.exists()).collect();
                self.keep_only(keep);
            }
            (PlMenu::Select, 0) => self.pl.selected = (0..n).filter(|i| !self.pl.selected.contains(i)).collect(),
            (PlMenu::Select, 1) => self.pl.selected.clear(),
            (PlMenu::Select, 2) => self.pl.selected = (0..n).collect(),
            (PlMenu::Misc, 1) => self.info_show_selected(),
            (PlMenu::Misc, 2) => {
                // Winamp's "Misc. opts" submenu.
                let at = self.win(Kind::Playlist).map(Win::global_cursor).unwrap_or_default();
                let items = vec![
                    Item::action("misc:html", "Generate HTML playlist"),
                    Item::action("misc:reread", "Read extended info on selected files"),
                ];
                self.open_menu(items, (at.0 as i32, at.1 as i32), _el);
            }
            (PlMenu::Misc, 0) => {
                // Winamp's sort choices, as a pop-up at the pointer.
                let at = self.win(Kind::Playlist).map(Win::global_cursor).unwrap_or_default();
                let items = playlist::Sort::ALL.iter().map(|s| Item::action(s.id(), s.label())).collect();
                self.open_menu(items, (at.0 as i32, at.1 as i32), _el);
            }
            (PlMenu::List, 0) => self.clear_playlist(),
            (PlMenu::List, 1) => self.save_playlist(),
            (PlMenu::List, 2) => {
                // Saved playlists by name, as a pop-up at the pointer.
                let at = self.win(Kind::Playlist).map(Win::global_cursor).unwrap_or_default();
                let mut items: Vec<Item> = self.saved_playlists().into_iter().map(|p| Item::action(format!("list:{}", p.display()), p.file_stem().unwrap_or_default().to_string_lossy())).collect();
                if !items.is_empty() {
                    items.push(Item::Separator);
                }
                items.push(Item::action("list:file", "From file…"));
                self.open_menu(items, (at.0 as i32, at.1 as i32), _el);
            }
            _ => {}
        }
        self.sync_playlist();
    }

    /// Right-click on a track: select it (unless it's in the selection),
    /// then Add to Favorites / Remove above the main menu.
    fn pl_row_menu(&mut self) -> Option<Vec<Item>> {
        let (x, y) = self.win(Kind::Playlist)?.cursor;
        let Some(plw::Control::Track(i)) = plw::hit(&self.pl, x, y) else { return None };
        if !self.pl.selected.contains(&i) {
            self.pl.selected = BTreeSet::from([i]);
            self.mark(Kind::Playlist);
        }
        let mut items = vec![Item::action("pl:fav", "Add to Favorites"), Item::action("pl:remove", "Remove"), Item::Separator];
        items.extend(self.main_menu());
        Some(items)
    }

    /// The selected tracks into Favorites, playing from where they play now.
    fn pl_favorite(&mut self) {
        let items: Vec<gamp_core::sources::Item> = (self.pl.selected.iter())
            .filter_map(|&i| self.playlist.entries.get(i))
            .map(|e| {
                // A loose file keeps its path; lib_send reads it back.
                let id = match &e.url { Some(u) => format!("playlist\u{1f}{u}"), None => format!("file\u{1f}{}", e.path.display()) };
                gamp_core::sources::Item { url: e.url.clone(), live: e.live, duration: e.duration, ..gamp_core::sources::Item::track(id, &e.title).by(&e.artist) }
            })
            .collect();
        if let (Some(b), false) = (&self.backend, items.is_empty()) {
            b.action("playlist".into(), "fav:add".into(), items);
        }
    }

    fn clear_playlist(&mut self) {
        self.player.stop();
        self.playlist.set(Vec::new());
        self.ids.clear();
        self.pl.selected.clear();
        self.update_title();
    }

    fn keep_only(&mut self, keep: BTreeSet<usize>) {
        let current = self.playlist.current().map(Entry::key);
        let entries = std::mem::take(&mut self.playlist.entries);
        self.playlist.entries = entries.into_iter().enumerate().filter(|(i, _)| keep.contains(i)).map(|(_, e)| e).collect();
        self.playlist.current = current.and_then(|p| self.playlist.entries.iter().position(|e| e.key() == p)).unwrap_or(0);
        self.playlist.reshuffle();
        self.pl.selected.clear();
        self.ids.clear();
        self.sync_playlist();
    }

    fn remove_selected(&mut self) {
        let keep: BTreeSet<usize> = (0..self.playlist.entries.len()).filter(|i| !self.pl.selected.contains(i)).collect();
        self.keep_only(keep);
    }

    /// MISC › Misc opts › Generate HTML playlist: writes the page to the
    /// data folder and shows it in the browser.
    fn html_playlist(&mut self) {
        let page = playlist::html(&self.playlist.entries);
        let path = gamp_core::store::data_dir().join("playlist.html");
        match std::fs::write(&path, page) {
            Ok(()) => {
                self.main.message = Some(tmf("HTML PLAYLIST: {} TRACKS", &[&self.playlist.entries.len()]));
                if !cfg!(test) {
                    crate::backend::open_in_browser(&format!("file://{}", path.display()));
                }
            }
            Err(e) => self.main.message = Some(tmf("COULDN'T WRITE {}: {}", &[&path.display(), &e]).to_uppercase()),
        }
    }

    /// MISC › Misc opts › Read extended info: re-reads tags and length for
    /// the selected files (all files when nothing is selected).
    fn reread_selected(&mut self) {
        let which: Vec<usize> = if self.pl.selected.is_empty() { (0..self.playlist.entries.len()).collect() } else { self.pl.selected.iter().copied().collect() };
        let mut n = 0;
        for i in which {
            let Some(e) = self.playlist.entries.get(i) else { continue };
            if e.url.is_some() || !e.path.exists() {
                continue;
            }
            let fresh = Entry::read(&e.path);
            self.playlist.entries[i] = fresh;
            n += 1;
        }
        self.main.message = Some(tmf(if n == 1 { "READ INFO ON {} FILE" } else { "READ INFO ON {} FILES" }, &[&n]));
        self.update_title();
        self.sync_playlist();
    }

    fn sort_playlist(&mut self, how: playlist::Sort) {
        self.playlist.sort(how);
        self.pl.selected.clear();
        self.reordered();
    }

    /// After the list changes order: the engine's queued next track may
    /// no longer be next, so line up the right one.
    fn reordered(&mut self) {
        self.ids.clear();
        if self.main.state != PlayState::Stopped {
            self.queue_after(self.playlist.current);
        }
        self.sync_playlist();
    }

    /// Playlists saved in Gamp's folder (LIST › Save list), by name.
    fn saved_playlists(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(self.data_dir.join("playlists"))
            .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| ["m3u", "m3u8", "pls"].iter().any(|x| e.eq_ignore_ascii_case(x)))).collect())
            .unwrap_or_default();
        out.sort_by_key(|p| p.file_stem().unwrap_or_default().to_string_lossy().to_lowercase());
        out
    }

    fn save_playlist(&mut self) {
        self.ask(Ask::SavePlaylist);
    }

    fn playlist_saved(&mut self, path: PathBuf) {
        let path = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("m3u8")) { path } else { path.with_extension("m3u8") };
        match std::fs::write(&path, self.m3u()) {
            Ok(()) => self.say_f("SAVED PLAYLIST: {}", &[&path.file_stem().unwrap_or_default().to_string_lossy()]),
            Err(e) => self.say_f("CAN'T SAVE: {}", &[&e]),
        }
    }

    /// The playlist as extended M3U. Network tracks are written as links
    /// to this app's server, which plays them while Gamp runs.
    pub(crate) fn m3u(&self) -> String {
        let port = self.backend.as_ref().map(|b| b.port).unwrap_or(8742);
        let mut out = String::from("#EXTM3U\n");
        for e in &self.playlist.entries {
            let place = match &e.url {
                Some(u) => format!("http://127.0.0.1:{port}{u}"),
                None => e.path.display().to_string(),
            };
            out.push_str(&format!("#EXTINF:{},{}\n{}\n", e.duration.map(|d| d.round() as i64).unwrap_or(-1), e.display(), place));
        }
        out
    }

    fn resize_playlist(&mut self) {
        let Some(r) = &self.resize else { return };
        let Some(w) = self.win(Kind::Playlist) else { return };
        let now = w.global_cursor();
        let scale = w.scale() * self.zoom();
        let dw = ((now.0 - r.start.0) / scale) as i32;
        let dh = ((now.1 - r.start.1) / scale) as i32;
        if self.pl.shade {
            let (nw, _) = plw::snap_size(r.size.0 as i32 + dw, plw::MIN_HEIGHT as i32);
            self.pl.width = nw;
        } else {
            let (nw, nh) = plw::snap_size(r.size.0 as i32 + dw, r.size.1 as i32 + dh);
            if (nw, nh) == (self.pl.width, self.pl.height) {
                return;
            }
            self.pl.width = nw;
            self.pl.height = nh;
        }
        self.pl.top = self.pl.top.min(self.pl.max_top());
        self.resize_window(Kind::Playlist);
    }

    fn resize_library(&mut self) {
        let Some(r) = &self.resize else { return };
        let Some(w) = self.win(Kind::Library) else { return };
        let now = w.global_cursor();
        let scale = w.scale() * self.zoom();
        let nw = (r.size.0 as f64 + (now.0 - r.start.0) / scale).max(gamp_ui::library_window::MIN_WIDTH as f64) as u32;
        let nh = (r.size.1 as f64 + (now.1 - r.start.1) / scale).max(gamp_ui::library_window::MIN_HEIGHT as f64) as u32;
        // Winamp's steps: 25 wide, 29 high.
        let (nw, nh) = (275 + (nw.saturating_sub(275) + 12) / 25 * 25, 116 + (nh.saturating_sub(116) + 14) / 29 * 29);
        if (nw, nh) != (self.lib.view.width, self.lib.view.height) {
            self.lib.view.width = nw.max(gamp_ui::library_window::MIN_WIDTH);
            self.lib.view.height = nh.max(gamp_ui::library_window::MIN_HEIGHT);
            self.resize_window(Kind::Library);
        }
    }

    pub fn wheel(&mut self, k: Kind, lines: f64) {
        match k {
            Kind::Library => self.lib_wheel(lines),
            Kind::Jump => self.jump_wheel(lines),
            Kind::Playlist => {
                let top = self.pl.top as isize - (lines * 3.0).round() as isize;
                self.pl.top = top.clamp(0, self.pl.max_top() as isize) as usize;
                self.mark(Kind::Playlist);
            }
            _ => {
                self.set_volume(self.main.volume + (lines * 0.02) as f32, false);
            }
        }
    }

    // ---- menus --------------------------------------------------------------

    fn skins(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(self.data_dir.join("skins"))
            .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("wsz") || e.eq_ignore_ascii_case("zip"))).collect())
            .unwrap_or_default();
        out.sort();
        out
    }

    fn vis_menu(&self) -> Vec<Item> {
        let m = self.main.vis_mode;
        vec![
            Item::action("vis:analyzer", "Spectrum analyzer").checked(m == VisMode::Analyzer),
            Item::action("vis:scope", "Oscilloscope").checked(m == VisMode::Oscilloscope),
            Item::action("vis:off", "Off").checked(m == VisMode::Off),
        ]
    }

    /// A new GitHub issue, filled in with the last error, the version and
    /// the system; the browser shows it all before anything is sent.
    pub fn report_url(&self) -> String {
        // Percent-encoding, as a URL query wants it.
        let enc = |t: &str| t.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
        let version = env!("CARGO_PKG_VERSION");
        let title = match &self.last_error {
            Some(e) => e.chars().take(80).collect::<String>(),
            None => format!("Problem with Gamp {version}"),
        };
        let said = self.last_error.as_deref().unwrap_or("(no error message)");
        let body = format!(
            "**What happened**\nWhat you did, what you expected, and what Gamp did instead:\n\n\n**Gamp said**\n{said}\n\n**Where**\nGamp {version} on {} ({})",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        format!("https://github.com/fillinthe/g-amp/issues/new?labels=bug&title={}&body={}", enc(&title), enc(&body))
    }

    #[cfg(test)]
    pub fn main_menu_has(&self, label: &str) -> bool {
        fn has(items: &[Item], label: &str) -> bool {
            items.iter().any(|i| match i {
                Item::Action { label: l, .. } => l == label,
                Item::Submenu { items, .. } => has(items, label),
                _ => false,
            })
        }
        has(&self.main_menu(), label)
    }

    pub fn main_menu(&self) -> Vec<Item> {
        let museum = self.data_dir.join(crate::library::MUSEUM_DIR);
        let mut skins = vec![Item::action("skin:load", "Load skin…"), Item::action("skin:museum", "Skin Museum…").shortcut("Alt+S"), Item::action("skin:surprise", "Surprise me")];
        // A museum skin you like: keep it in this menu.
        if self.skin_path.as_ref().is_some_and(|p| p.starts_with(&museum)) {
            skins.push(Item::action("skin:keep", "Keep this skin"));
        }
        skins.push(Item::Separator);
        skins.push(Item::action("skin:default", "Original skin (GAMP Classic)").checked(self.skin_path.is_none()));
        for p in self.skins() {
            let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            skins.push(Item::action(format!("skin:{}", p.display()), name).checked(self.skin_path.as_ref() == Some(&p)));
        }
        vec![
            Item::action("about", "About Gamp…"),
            Item::action("report", match &self.last_error {
                Some(e) => tf("Report a problem… ({})", &[&e.chars().take(28).collect::<String>().trim_end()]),
                None => "Report a problem…".into(),
            }),
            Item::Separator,
            Item::action("open:file", "Play file…").shortcut("L"),
            Item::action("open:folder", "Play folder…").shortcut("Shift+L"),
            Item::action("jump", "Jump to file…").shortcut("J"),
            Item::action("info", "File info…").shortcut("Alt+3"),
            Item::Separator,
            Item::action("win:main", "Main window").checked(true),
            Item::action("win:playlist", "Playlist editor").checked(self.main.playlist_open).shortcut("Alt+E"),
            Item::action("win:eq", "Equalizer").checked(self.main.eq_open).shortcut("Alt+G"),
            Item::action("win:vis", "G-Drop visualizer").checked(self.drop_visible()).shortcut("Alt+K"),
            Item::action("win:library", "Media Library").checked(self.wins.get(&Kind::Library).is_some_and(|w| w.visible)).shortcut("Alt+L"),
            Item::action("win:gather", "Gather windows").shortcut("Alt+W"),
            Item::Separator,
            Item::submenu("Skins", skins),
            Item::submenu(
                "Options",
                vec![
                    Item::action("opt:top", "Always on top").checked(self.on_top).shortcut("Ctrl+A"),
                    Item::action("opt:double", "Double size").checked(self.size == 2.0).shortcut("Ctrl+D"),
                    Item::submenu("Size", SIZES.iter().map(|&(z, label)| Item::action(format!("size:{z}"), label).checked(self.size == z)).collect()),
                    Item::Separator,
                    Item::action("opt:stopafter", "Stop after current track").checked(self.stop_after).shortcut("Ctrl+V"),
                    Item::submenu(self.sleep_label(), [0, 15, 30, 45, 60, 90].iter().map(|&m| Item::action(format!("sleep:{m}"), if m == 0 { "Off".to_owned() } else { tf("{} minutes", &[&m]) }).checked(m == 0 && self.sleep_at.is_none())).collect()),
                    Item::Separator,
                    Item::action("opt:elapsed", "Time elapsed").checked(!self.main.show_remaining),
                    Item::action("opt:remaining", "Time remaining").checked(self.main.show_remaining),
                    Item::Separator,
                    Item::submenu("Language", {
                        let mut langs = vec![Item::action("lang:auto", "Same as the system").checked(self.lang.is_none()), Item::Separator];
                        langs.extend(gamp_ui::lang::LANGS.iter().map(|&(code, name)| Item::action(format!("lang:{code}"), name).checked(self.lang.as_deref() == Some(code))));
                        langs
                    }),
                ],
            ),
            Item::submenu(
                "Playback",
                vec![
                    Item::action("play:prev", "Previous").shortcut("Z"),
                    Item::action("play:play", "Play").shortcut("X"),
                    Item::action("play:pause", "Pause").shortcut("C"),
                    Item::action("play:stop", "Stop").shortcut("V"),
                    Item::action("play:next", "Next").shortcut("B"),
                    Item::Separator,
                    Item::action("play:back", "Back 5 seconds").shortcut("Left"),
                    Item::action("play:fwd", "Forward 5 seconds").shortcut("Right"),
                    Item::Separator,
                    Item::action("play:repeat", "Repeat").checked(self.main.repeat).shortcut("R"),
                    Item::action("play:shuffle", "Shuffle").checked(self.main.shuffle).shortcut("S"),
                ],
            ),
            Item::submenu("Visualization", self.vis_menu()),
            Item::Separator,
            match &self.update {
                Some(r) => Item::action("update:install", tf("Update to Gamp {}…", &[&r.version])),
                None => Item::action("update:check", "Check for updates…"),
            },
            Item::action("exit", "Exit"),
        ]
    }

    /// "Sleep timer", with the time left while it runs.
    fn sleep_label(&self) -> String {
        match self.sleep_at {
            Some((at, _)) => tf("Sleep timer ({} min left)", &[&((at.saturating_duration_since(Instant::now()).as_secs() + 59) / 60)]),
            None => "Sleep timer".into(),
        }
    }

    fn preset_menu(&self) -> Vec<Item> {
        let mut load: Vec<Item> = eq_window::PRESETS.iter().map(|(name, _)| Item::action(format!("preset:{name}"), *name)).collect();
        load.insert(0, Item::Separator);
        load.insert(0, Item::action("preset:flat", "Flat (reset)"));
        let saved = eqf::saved(&self.data_dir.join("presets"));
        if !saved.is_empty() {
            load.push(Item::Separator);
            load.extend(saved.into_iter().map(|(path, p)| Item::action(format!("eqf:{}", path.display()), p.name)));
        }
        load.push(Item::Separator);
        load.push(Item::action("preset:file", "From file…"));
        vec![Item::submenu("Load", load), Item::action("preset:save", "Save…"), Item::action("preset:flat", "Reset")]
    }

    fn use_preset(&mut self, p: &eqf::Preset) {
        self.eq.preamp = p.preamp;
        self.eq.bands = p.bands;
        self.apply_eq();
    }

    /// Opens a Winamp `.eqf` preset or `.q1` library: its presets are kept
    /// with the saved ones (so they're in the menu) and the first is used.
    fn import_presets(&mut self, path: &std::path::Path) {
        let presets = std::fs::read(path).map(|b| eqf::parse(&b)).unwrap_or_default();
        let Some(first) = presets.first().cloned() else {
            self.say("NOT AN EQUALIZER PRESET FILE");
            return;
        };
        let dir = self.data_dir.join("presets");
        let _ = std::fs::create_dir_all(&dir);
        let fallback = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        for (i, p) in presets.iter().enumerate() {
            let mut name: String = p.name.chars().map(|c| if r#"/\:*?"<>|"#.contains(c) || c.is_control() { '-' } else { c }).collect();
            if name.trim().is_empty() {
                name = if presets.len() == 1 { fallback.clone() } else { format!("{fallback} {}", i + 1) };
            }
            let _ = std::fs::write(dir.join(format!("{}.eqf", name.trim())), eqf::write(p));
        }
        self.use_preset(&first);
        if presets.len() == 1 { self.say_f("PRESET: {}", &[&first.name]) } else { self.say_f("ADDED {} PRESETS", &[&presets.len()]) }
    }

    fn save_preset(&mut self) {
        self.ask(Ask::SavePreset);
    }

    fn preset_saved(&mut self, path: PathBuf) {
        let path = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("eqf")) { path } else { path.with_extension("eqf") };
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        match std::fs::write(&path, eqf::write(&eqf::Preset { name: name.clone(), preamp: self.eq.preamp, bands: self.eq.bands })) {
            Ok(()) => self.say_f("SAVED PRESET: {}", &[&name]),
            Err(e) => self.say_f("COULDN'T SAVE THE PRESET: {}", &[&e]),
        }
    }

    fn open_menu(&mut self, items: Vec<Item>, at: (i32, i32), el: Option<&ActiveEventLoop>) {
        self.close_menus();
        self.push_menu(items, at, el);
    }

    fn push_menu(&mut self, items: Vec<Item>, at: (i32, i32), el: Option<&ActiveEventLoop>) {
        let z = self.zoom();
        // Kept on the display it opens on: too long a list goes on in "More",
        // and one past the bottom or right edge moves up or left.
        let mut at = at;
        let mut items = items;
        let sf = self.win(Kind::Main).and_then(|w| w.window.as_ref()).map_or(1.0, |w| w.scale_factor());
        let per = z * crate::win::units(sf);
        let screens = self.screens();
        if let Some(s) = screens.iter().find(|s| at.0 >= s.x && at.0 < s.right() && at.1 >= s.y && at.1 < s.bottom()).or(screens.first()) {
            items = fit_menu(items, (s.h as f64 / per) as u32);
            let (w, h) = menu::size(&items);
            let (w, h) = ((w as f64 * per) as i32, (h as f64 * per) as i32);
            at.0 = at.0.min(s.right() - w).max(s.x);
            at.1 = at.1.min(s.bottom() - h).max(s.y);
        }
        let (w, h) = menu::size(&items);
        let win = Win::open(el, "Gamp menu", LogicalSize::new((w as f64 * z).round() as u32, (h as f64 * z).round() as u32), at, true);
        if let Some(window) = &win.window {
            window.focus_window();
        }
        self.menus.push(MenuLevel { items, hover: None, win, child_of: None });
        let i = self.menus.len() - 1;
        self.mark(Kind::Menu(i));
    }

    pub fn close_menus(&mut self) {
        self.menus.clear();
    }

    pub fn menu_lost_focus(&mut self, k: Kind) {
        // Moving into a sub-menu shifts focus between our own menus; only
        // close when none of them has it.
        if let Kind::Menu(_) = k {
            let any_focused = self.menus.iter().any(|m| m.win.window.as_ref().is_some_and(|w| w.has_focus()));
            if !any_focused {
                self.close_menus();
            }
        }
    }

    fn menu_hover(&mut self, level: usize, x: i32, y: i32, el: Option<&ActiveEventLoop>) {
        let Some(m) = self.menus.get_mut(level) else { return };
        let width = m.win.frame.width as i32;
        let hover = menu::hit(&m.items, x, y, width);
        if hover == m.hover {
            return;
        }
        m.hover = hover;
        m.win.dirty = true;
        // Hovering a sub-menu opens it beside the item.
        self.menus.truncate(level + 1);
        let m = &self.menus[level];
        if let Some(i) = hover {
            if let Item::Submenu { items, .. } = &m.items[i] {
                let scale = m.win.scale() * self.size;
                let at = (m.win.pos.0 + ((width - 3) as f64 * scale) as i32, m.win.pos.1 + (menu::item_top(&m.items, i) as f64 * scale) as i32 - (3.0 * scale) as i32);
                let items = items.clone();
                self.menus[level].child_of = Some(i);
                self.push_menu(items, at, el);
            }
        }
    }

    fn menu_release(&mut self, level: usize, el: Option<&ActiveEventLoop>) {
        let Some(m) = self.menus.get(level) else { return };
        let (x, y) = m.win.cursor;
        let Some(i) = menu::hit(&m.items, x, y, m.win.frame.width as i32) else { return };
        match m.items[i].clone() {
            Item::Action { id, enabled: true, .. } => {
                self.close_menus();
                self.menu_action(&id, el);
            }
            _ => {}
        }
    }

    pub fn menu_action(&mut self, id: &str, _el: Option<&ActiveEventLoop>) {
        match id {
            "about" => self.main.message = Some(format!("GAMP {} - FILLINTHE/G-AMP", env!("CARGO_PKG_VERSION"))),
            "report" => {
                crate::backend::open_in_browser(&self.report_url());
                self.say("REPORT: FILL IN WHAT HAPPENED IN YOUR BROWSER");
            }
            "open:file" => self.pick_files(true),
            "open:folder" => self.pick_folder(true),
            "win:playlist" => self.toggle_window(Kind::Playlist),
            "win:eq" => self.toggle_window(Kind::Eq),
            "win:library" => self.toggle_window(Kind::Library),
            "win:vis" => self.toggle_window(Kind::Vis),
            "win:main" | "win:gather" => self.gather(),
            "jump" => self.jump_toggle(),
            "info" => self.info_show_selected(),
            "skin:load" => self.ask(Ask::SkinFile),
            "skin:default" => {
                self.load_skin(None);
                self.say("SKIN: ORIGINAL");
            }
            "skin:museum" => self.skin_museum(),
            "skin:surprise" => self.skin_fetch(None),
            "skin:keep" => {
                if let Some(p) = self.skin_path.clone() {
                    let dir = self.data_dir.join("skins");
                    let kept = dir.join(p.file_name().unwrap_or_default());
                    if std::fs::create_dir_all(&dir).and_then(|_| std::fs::copy(&p, &kept)).is_ok() {
                        self.skin_path = Some(kept);
                        self.say("SKIN KEPT IN THE SKINS MENU");
                    }
                }
            }
            "lib:skin" => {
                let first = self.lib.view.table.selected.first().and_then(|i| self.lib.items.get(*i)).cloned();
                if first.is_some() {
                    self.skin_fetch(first);
                }
            }
            "opt:top" => self.toggle_on_top(),
            "lang:auto" => self.set_lang(None),
            other if other.starts_with("lang:") => self.set_lang(Some(other["lang:".len()..].to_owned())),
            "opt:double" => self.toggle_double(),
            "opt:stopafter" => self.toggle_stop_after(),
            "opt:elapsed" => self.main.show_remaining = false,
            "opt:remaining" => self.main.show_remaining = true,
            "play:prev" => self.step(false),
            "play:play" => self.play(),
            "play:pause" => self.pause(),
            "play:stop" => self.stop(),
            "play:next" => self.step(true),
            "play:back" => self.seek_by(-5.0),
            "play:fwd" => self.seek_by(5.0),
            "play:repeat" => self.set_repeat(!self.main.repeat),
            "play:shuffle" => self.set_shuffle(!self.main.shuffle),
            "vis:analyzer" => self.main.vis_mode = VisMode::Analyzer,
            "vis:scope" => self.main.vis_mode = VisMode::Oscilloscope,
            "vis:off" => self.main.vis_mode = VisMode::Off,
            "exit" => self.quit = true,
            "misc:html" => self.html_playlist(),
            "list:file" => self.ask(Ask::PlaylistFile),
            "drop:full" => self.drop_fullscreen(!self.drop.fullscreen),
            "drop:next" => self.drop_step(|e| e.next(false)),
            "drop:back" => self.drop_step(|e| e.back()),
            "drop:random" => self.drop_step(|e| e.random(false)),
            "drop:cycle" => self.drop_cycle(!self.drop.engine.auto),
            "lib:play" => self.lib_button("play"),
            "lib:enqueue" => self.lib_button("enqueue"),
            "lib:open" => self.lib_open_folder(),
            "lib:artist" => self.lib_artist(),
            "lib:fav" => self.lib_button("action:fav:add"),
            "pl:fav" => self.pl_favorite(),
            "pl:remove" => self.remove_selected(),
            "lib:unfav" => self.lib_button("action:fav:remove"),
            "misc:reread" => self.reread_selected(),
            id if id.starts_with("sort:") => {
                if let Some(how) = playlist::Sort::from_id(id) {
                    self.sort_playlist(how);
                }
            }
            "update:check" => self.update_check(true),
            "update:install" => self.update_install(),
            "preset:save" => self.save_preset(),
            "preset:file" => self.ask(Ask::PresetFile),
            "preset:flat" => {
                self.eq.preamp = 0.0;
                self.eq.bands = [0.0; 10];
                self.apply_eq();
            }
            other => {
                if let Some(m) = other.strip_prefix("sleep:").and_then(|m| m.parse().ok()) {
                    self.set_sleep(m);
                } else if let Some(z) = other.strip_prefix("size:").and_then(|z| z.parse().ok()) {
                    self.set_size(z);
                } else if let Some(i) = other.strip_prefix("drop:preset:").and_then(|i| i.parse().ok()) {
                    self.drop_step(|e| e.select(i, false));
                } else if let Some(path) = other.strip_prefix("list:") {
                    self.open_files(vec![PathBuf::from(path)], true);
                } else if let Some(path) = other.strip_prefix("eqf:") {
                    if let Some(p) = std::fs::read(path).ok().and_then(|b| eqf::parse(&b).into_iter().next()) {
                        self.use_preset(&p);
                    }
                } else if let Some(path) = other.strip_prefix("skin:") {
                    self.load_skin(Some(PathBuf::from(path)));
                } else if let Some(name) = other.strip_prefix("preset:") {
                    if let Some((_, bands)) = eq_window::PRESETS.iter().find(|(n, _)| *n == name) {
                        self.eq.bands = *bands;
                        self.apply_eq();
                    }
                }
            }
        }
        self.mark_all();
    }

    // ---- keys -----------------------------------------------------------------

    /// Winamp's keys, in any of the windows.
    pub fn key(&mut self, k: Kind, key: &Key, text: Option<&str>) {
        // Size keys work in every window, even while typing in a field.
        if self.modifiers.control_key() || self.modifiers.super_key() {
            if let Key::Character(c) = key {
                match c.as_str() {
                    "=" | "+" => return self.zoom_step(1),
                    "-" => return self.zoom_step(-1),
                    "0" => return self.zoom_step(-100),
                    _ => {}
                }
            }
        }
        if k == Kind::Library && self.lib_key(key, text) {
            return;
        }
        // G-Drop's letters alone: ⌘H (hide) mustn't also pick a random preset.
        let plain = !(self.modifiers.control_key() || self.modifiers.super_key() || self.modifiers.alt_key());
        if k == Kind::Vis && plain && self.drop_key(key) {
            return;
        }
        if k == Kind::Jump {
            return self.jump_key(key, text);
        }
        if k == Kind::Info {
            return self.info_key(key, text);
        }
        let ctrl = self.modifiers.control_key() || self.modifiers.super_key();
        let alt = self.modifiers.alt_key();
        if let Kind::Menu(_) = k {
            if matches!(key, Key::Named(NamedKey::Escape)) {
                self.close_menus();
            }
            return;
        }
        match key {
            Key::Character(c) => match c.to_lowercase().as_str() {
                "w" if alt => self.gather(),
                "q" if self.modifiers.super_key() => self.quit = true,
                "l" if alt => self.toggle_window(Kind::Library),
                "s" if alt => self.skin_museum(),
                "k" if alt || (ctrl && self.modifiers.shift_key()) => self.toggle_window(Kind::Vis),
                "e" if alt => self.toggle_window(Kind::Playlist),
                "g" if alt => self.toggle_window(Kind::Eq),
                "a" if ctrl && k == Kind::Playlist => self.pl.selected = (0..self.playlist.entries.len()).collect(),
                "a" if ctrl => self.toggle_on_top(),
                "d" if ctrl => self.toggle_double(),
                "t" if ctrl => self.main.show_remaining = !self.main.show_remaining,
                "z" => self.step(false),
                "x" => self.play(),
                "c" => self.pause(),
                "v" if ctrl => self.toggle_stop_after(),
                "v" if self.modifiers.shift_key() => self.fade_stop(),
                "v" => self.stop(),
                "b" => self.step(true),
                "l" if self.modifiers.shift_key() => self.pick_folder(true),
                "l" => self.pick_files(true),
                "s" => self.set_shuffle(!self.main.shuffle),
                "r" => self.set_repeat(!self.main.repeat),
                "j" => self.jump_toggle(),
                "3" if alt => self.info_show_selected(),
                _ => {}
            },
            Key::Named(NamedKey::ArrowLeft) => self.seek_by(-5.0),
            Key::Named(NamedKey::ArrowRight) => self.seek_by(5.0),
            Key::Named(NamedKey::ArrowUp) if k == Kind::Playlist => self.pl_cursor(-1),
            Key::Named(NamedKey::ArrowDown) if k == Kind::Playlist => self.pl_cursor(1),
            Key::Named(NamedKey::ArrowUp) => self.set_volume(self.main.volume + 0.02, false),
            Key::Named(NamedKey::ArrowDown) => self.set_volume(self.main.volume - 0.02, false),
            Key::Named(NamedKey::Delete | NamedKey::Backspace) if k == Kind::Playlist => self.remove_selected(),
            Key::Named(NamedKey::Enter) if k == Kind::Playlist => {
                if let Some(i) = self.pl.selected.iter().next().copied() {
                    self.play_index(i);
                }
            }
            _ => {}
        }
        self.mark_all();
    }

    fn pl_cursor(&mut self, d: isize) {
        let n = self.playlist.entries.len();
        if n == 0 {
            return;
        }
        let at = self.pl.selected.iter().next().copied().unwrap_or(self.playlist.current) as isize;
        let next = (at + d).clamp(0, n as isize - 1) as usize;
        self.pl.selected = BTreeSet::from([next]);
        self.pl.reveal(next);
        self.sync_playlist();
    }

    pub fn set_focus(&mut self, k: Kind, focused: bool) {
        match k {
            Kind::Main => self.main.focused = focused,
            Kind::Eq => self.eq.focused = focused,
            Kind::Playlist => self.pl.focused = focused,
            Kind::Library => self.lib.view.focused = focused,
            Kind::Vis => self.drop.focused = focused,
            Kind::Jump => {
                self.jump.view.focused = focused;
                self.jump.view.search.focused = focused;
            }
            Kind::Info => self.info.view.focused = focused,
            Kind::Menu(_) => {
                if !focused {
                    self.menu_lost_focus(k);
                }
                return;
            }
        }
        if focused {
            self.raise_all(k);
        }
        self.mark(k);
    }

    /// Like Winamp, the windows move as a group: clicking one brings every
    /// open window in front of other programs, the clicked one on top.
    fn raise_all(&self, top: Kind) {
        let others = self.wins.iter().filter(|(k, w)| **k != top && w.visible).map(|(_, w)| w);
        for w in others.chain(self.wins.get(&top)) {
            if let Some(w) = &w.window {
                crate::win::order_front(w, self.on_top);
            }
        }
    }

    pub fn dropped(&mut self, k: Kind, path: PathBuf) {
        self.drops.push(path);
        self.drop_target = Some(k);
    }

    /// A drop on the player replaces the playlist and plays it; a drop on
    /// the playlist adds to it.
    pub fn flush_drops(&mut self) {
        if self.drops.is_empty() {
            return;
        }
        let files = std::mem::take(&mut self.drops);
        let replace = self.drop_target.take() != Some(Kind::Playlist);
        self.open_files(files, replace);
    }

    pub fn moved(&mut self, k: Kind, p: winit::dpi::PhysicalPosition<i32>) {
        if let Some(w) = self.win_mut(k) {
            w.moved(p);
        }
    }
}

/// 125.0 → "2:05".
pub fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// A menu no taller than `max_h` (menu pixels): what doesn't fit goes on in "More".
fn fit_menu(mut items: Vec<Item>, max_h: u32) -> Vec<Item> {
    if menu::size(&items).1 <= max_h {
        return items;
    }
    let mut rest = Vec::new();
    while items.len() > 1 {
        rest.insert(0, items.pop().unwrap());
        items.push(Item::submenu("More", Vec::new()));
        let fits = menu::size(&items).1 <= max_h;
        items.pop();
        if fits {
            break;
        }
    }
    items.push(Item::submenu("More", fit_menu(rest, max_h)));
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(path: &std::path::Path, secs: u32) {
        let rate = 22050u32;
        let frames = secs * rate;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + frames * 2).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(frames * 2).to_le_bytes());
        for i in 0..frames {
            let v = ((i as f32 * 0.06).sin() * 9000.0) as i16;
            b.extend_from_slice(&v.to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }

    fn at(app: &mut App, k: Kind, x: i32, y: i32) {
        let w = app.win_mut(k).unwrap();
        w.frame = gamp_ui::Frame::new(w.frame.width.max(1), w.frame.height.max(1));
        w.cursor = (x, y);
    }

    fn click(app: &mut App, k: Kind, x: i32, y: i32) {
        // Headless windows: draw once so frames have their real size.
        app.redraw(k);
        at(app, k, x, y);
        app.win_mut(k).unwrap().cursor = (x, y);
        app.press(k, None);
        app.release(k, None);
    }

    fn settle(app: &mut App) {
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(25));
            app.tick();
        }
    }

    fn songs(name: &str, n: usize, secs: u32) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gamp-app-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..n {
            tone(&dir.join(format!("{:02} Song {i}.wav", i + 1)), secs);
        }
        dir
    }

    #[test]
    fn gamp_opens_where_you_were() {
        let dir = songs("resume", 2, 20);
        let data = std::env::temp_dir().join(format!("gamp-resume-{}", std::process::id()));
        std::fs::create_dir_all(&data).unwrap();
        let open = || {
            let mut app = App::new(OutputKind::Null, None).unwrap();
            app.data_dir = data.clone();
            app.load_session();
            settle(&mut app);
            app
        };
        let mut app = open();
        app.open_files(vec![dir.clone()], true);
        settle(&mut app);
        app.play_index(1);
        settle(&mut app);
        app.seek(12.0);
        settle(&mut app);
        app.save_session();
        drop(app);

        // Opens on that track, paused at its place, the time showing.
        let mut app = open();
        assert_eq!(app.playlist.current, 1);
        assert_eq!(app.main.state, PlayState::Paused);
        assert!((app.main.elapsed - 12.0).abs() < 1.0, "{}", app.main.elapsed);
        if let Ok(dir) = std::env::var("GAMP_SNAPSHOTS") {
            app.redraw(Kind::Main);
            app.win(Kind::Main).unwrap().frame.to_image().save(format!("{dir}/reopened.png")).unwrap();
        }
        // The time stays up (no pause blink) until it plays.
        for _ in 0..3 {
            app.blink_at = Instant::now() - Duration::from_secs(2);
            app.tick();
            assert!(app.main.blink_on);
        }
        // Closed again without playing: still there.
        app.save_session();
        drop(app);
        let mut app = open();
        assert!((app.main.elapsed - 12.0).abs() < 1.0, "{}", app.main.elapsed);
        // Play goes on from there (double-clicking it in the playlist too).
        app.play_index(1);
        settle(&mut app);
        assert_eq!(app.main.state, PlayState::Playing);
        assert!(app.main.elapsed >= 12.0 && app.main.elapsed < 14.0, "{}", app.main.elapsed);

        // Stopped, then closed: where it stopped.
        app.seek(8.0);
        settle(&mut app);
        app.player.stop(); // the Stop button
        settle(&mut app);
        app.save_session();
        drop(app);
        let mut app = open();
        assert!(app.main.elapsed >= 8.0 && app.main.elapsed < 10.0, "{}", app.main.elapsed);

        // Heard to the end: the start next time.
        app.play();
        app.seek(18.0);
        settle(&mut app);
        app.save_session();
        drop(app);
        let app = open();
        assert_eq!(app.main.state, PlayState::Stopped);
        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_place_survives_slow_seeks_and_tracks_that_cant_open_yet() {
        let dir = songs("resume2", 2, 20);
        let data = std::env::temp_dir().join(format!("gamp-resume2-{}", std::process::id()));
        std::fs::create_dir_all(&data).unwrap();
        let fresh = || {
            let mut app = App::new(OutputKind::Null, None).unwrap();
            app.data_dir = data.clone();
            app
        };
        let saved = || serde_json::from_slice::<crate::session::Session>(&std::fs::read(data.join("session.json")).unwrap()).unwrap().position;
        let mut app = fresh();
        app.open_files(vec![dir.clone()], true);
        settle(&mut app);
        app.seek(12.0);
        settle(&mut app);
        app.save_session();
        let place = saved();
        assert!(place >= 12.0 && place < 13.0, "{place}");
        drop(app);

        // Saved straight after opening, before the seek has landed: the place stays.
        let mut app = fresh();
        app.load_session();
        app.save_session();
        assert_eq!(saved(), place);
        settle(&mut app);
        drop(app);

        // The track can't open when Gamp starts (a stream just after waking):
        // nothing skips ahead or plays, and the place is kept...
        let song = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).min().unwrap();
        let good = std::fs::read(&song).unwrap();
        std::fs::write(&song, b"not audio yet").unwrap();
        let mut app = fresh();
        app.load_session();
        settle(&mut app);
        assert_eq!(app.playlist.current, 0);
        assert_ne!(app.main.state, PlayState::Playing);
        app.save_session();
        assert_eq!(saved(), place);
        // ...and Play, once it can, goes on from there.
        std::fs::write(&song, good).unwrap();
        app.play();
        settle(&mut app);
        assert_eq!(app.main.state, PlayState::Playing);
        assert!(app.main.elapsed >= place && app.main.elapsed < place + 2.0, "{}", app.main.elapsed);

        // A seek on its way: the time and the bar show where it's going, and that's the place.
        app.seek_wait = Some((15.0, Instant::now()));
        app.tick();
        assert_eq!(app.main.seeking, Some(0.75));
        assert_eq!(app.heard.as_ref().unwrap().3, 15.0);
        // Another track picked meanwhile: the seek doesn't follow it.
        app.play_index(0);
        app.tick();
        assert!(app.seek_wait.is_none());
        assert_eq!(app.main.seeking, None);
        // Stop lets go of a place Gamp reopened at.
        app.resume = Some((0, PathBuf::new(), None, 5.0));
        app.reopened = true;
        app.stop();
        settle(&mut app);
        assert!(app.resume.is_none() && !app.reopened);
        assert_eq!(app.main.state, PlayState::Stopped);
        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dialog_answers_do_their_job() {
        let dir = songs("dialogs", 2, 2);
        let data = std::env::temp_dir().join(format!("gamp-dialogs-{}", std::process::id()));
        std::fs::create_dir_all(&data).unwrap();
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.data_dir = data.clone();
        let answer = |ask, paths: Vec<PathBuf>| crate::dialog::Answer { ask, paths, yes: false };
        // Open files: they're in the playlist.
        app.answered(answer(Ask::OpenFolder { replace: true }, vec![dir.clone()]));
        assert_eq!(app.playlist.entries.len(), 2);
        // Save playlist: the file is written, with .m3u8 added.
        app.answered(answer(Ask::SavePlaylist, vec![data.join("Mine")]));
        let saved = std::fs::read_to_string(data.join("Mine.m3u8")).unwrap();
        assert!(saved.starts_with("#EXTM3U") && saved.contains("Song 1"));
        // Cancelled: nothing happens.
        app.answered(answer(Ask::OpenFiles { replace: true }, vec![]));
        assert_eq!(app.playlist.entries.len(), 2);
        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn connecting_counts_its_dots() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.main.message = Some(CONNECTING.into());
        let mut seen = Vec::new();
        for _ in 0..5 {
            app.marquee_at = Instant::now() - MARQUEE_STEP;
            app.tick();
            seen.push(app.main.message.clone().unwrap());
        }
        assert_eq!(seen, ["CONNECTING.", "CONNECTING..", "CONNECTING...", "CONNECTING", "CONNECTING."]);
    }

    #[test]
    fn a_problem_report_carries_the_error_and_version() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        assert!(app.main_menu_has("Report a problem…"));
        app.say("CAN'T PLAY: no stream for this track & that");
        assert!(app.main_menu_has("Report a problem… (CAN'T PLAY: no stream for th)"));
        let url = app.report_url();
        assert!(url.starts_with("https://github.com/fillinthe/g-amp/issues/new?labels=bug&title=CAN%27T%20PLAY"));
        assert!(url.contains(&format!("Gamp%20{}%20on%20", env!("CARGO_PKG_VERSION"))));
        assert!(url.contains("%26%20that") && !url.contains(' '));
    }

    #[test]
    fn g_drop_keeps_its_preset_through_command_keys() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.drop_cycle(false);
        app.drop.engine.select(31, true);
        app.modifiers = ModifiersState::SUPER;
        for c in ["h", "r", "f"] {
            app.key(Kind::Vis, &Key::Character(c.into()), Some(c));
        }
        assert_eq!(app.drop.engine.current(), 31);
        assert!(!app.drop.engine.auto);
        assert!(!app.drop.fullscreen);
        // Alone, H still picks one at random.
        app.modifiers = ModifiersState::empty();
        app.key(Kind::Vis, &Key::Character("h".into()), Some("h"));
        assert_ne!(app.drop.engine.current(), 31);
    }

    #[test]
    fn main_window_buttons_drive_the_player() {
        let dir = songs("main", 3, 4);
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.open_files(vec![dir.clone()], true);
        settle(&mut app);
        assert_eq!(app.playlist.entries.len(), 3);
        assert_eq!(app.main.state, PlayState::Playing);
        assert!(app.main.title.starts_with("1. 01 Song 0"), "{}", app.main.title);
        assert_eq!(app.main.khz, Some(22));
        assert_eq!(app.main.stereo, Some(false));
        click(&mut app, Kind::Main, 70, 95); // pause
        settle(&mut app);
        assert_eq!(app.main.state, PlayState::Paused);
        click(&mut app, Kind::Main, 45, 95); // play
        settle(&mut app);
        assert_eq!(app.main.state, PlayState::Playing);
        // Play again pauses where it is, not back to the start.
        let was = app.main.elapsed;
        click(&mut app, Kind::Main, 45, 95);
        settle(&mut app);
        assert_eq!(app.main.state, PlayState::Paused);
        assert!(app.main.elapsed >= was, "{} < {was}", app.main.elapsed);
        click(&mut app, Kind::Main, 45, 95);
        settle(&mut app);
        assert_eq!(app.main.state, PlayState::Playing);
        click(&mut app, Kind::Main, 112, 95); // next
        settle(&mut app);
        assert_eq!(app.playlist.current, 1);
        click(&mut app, Kind::Main, 20, 95); // previous
        settle(&mut app);
        assert_eq!(app.playlist.current, 0);
        // Volume slider dragged to the left is silence.
        app.redraw(Kind::Main);
        at(&mut app, Kind::Main, 110, 63);
        app.press(Kind::Main, None);
        at(&mut app, Kind::Main, 100, 63);
        app.main_drag(100);
        assert_eq!(app.player.volume(), 0.0);
        app.release(Kind::Main, None);
        // Seek to the middle (it plays on while settling; slow CI runners go further).
        click(&mut app, Kind::Main, 16 + 14 + 110, 76);
        settle(&mut app);
        assert!(app.main.elapsed > 1.6 && app.main.elapsed < 3.0, "{}", app.main.elapsed);
        // Shuffle, repeat, stop, close.
        click(&mut app, Kind::Main, 180, 95);
        click(&mut app, Kind::Main, 220, 95);
        assert!(app.main.shuffle && app.main.repeat);
        click(&mut app, Kind::Main, 90, 95);
        settle(&mut app);
        assert_eq!(app.main.state, PlayState::Stopped);
        // EQ and PL buttons hide and show their windows.
        click(&mut app, Kind::Main, 225, 62);
        assert!(!app.main.eq_open && !app.wins[&Kind::Eq].visible);
        click(&mut app, Kind::Main, 225, 62);
        assert!(app.main.eq_open);
        click(&mut app, Kind::Main, 267, 6);
        assert!(app.quit);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gdrop_clicks_move_edges_resize_and_cycling_is_kept() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.data_dir = std::env::temp_dir().join(format!("gamp-drop-{}", std::process::id()));
        app.redraw(Kind::Vis);
        let first = app.drop.engine.current();
        // A click on the picture doesn't switch presets.
        app.cursor_moved(Kind::Vis, 100.0, 100.0, None);
        app.press(Kind::Vis, None);
        app.release(Kind::Vis, None);
        assert_eq!(app.drop.engine.current(), first);
        // The left edge, dragged 50 px out: wider, the right edge stays put.
        app.wins.get_mut(&Kind::Vis).unwrap().move_to(500, 300);
        let (w0, h0) = (app.drop.width, app.drop.height);
        app.cursor_moved(Kind::Vis, 2.0, 100.0, None);
        app.press(Kind::Vis, None);
        app.cursor_moved(Kind::Vis, -48.0, 100.0, None);
        app.release(Kind::Vis, None);
        assert_eq!((app.drop.width, app.drop.height), (w0 + 50, h0));
        assert_eq!(app.win(Kind::Vis).unwrap().pos.0, 450);
        // Stay on one preset, and still after a restart.
        app.menu_action("drop:preset:5", None);
        assert_eq!(app.drop.engine.current(), 5);
        app.menu_action("drop:cycle", None);
        assert!(!app.drop.engine.auto);
        app.save_session();
        let mut again = App::new(OutputKind::Null, None).unwrap();
        again.data_dir = app.data_dir.clone();
        again.load_session();
        assert!(!again.drop.engine.auto);
        assert_eq!(again.drop.engine.current(), 5);
        let _ = std::fs::remove_dir_all(&app.data_dir);
    }

    #[test]
    fn the_size_setting_scales_every_window_and_is_kept() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.data_dir = std::env::temp_dir().join(format!("gamp-size-{}", std::process::id()));
        // ⌘+ twice, ⌘− once, ⌘0: 120%, 110%, back to 100%.
        app.modifiers = ModifiersState::SUPER;
        app.key(Kind::Library, &Key::Character("=".into()), Some("="));
        app.key(Kind::Main, &Key::Character("+".into()), Some("+"));
        assert_eq!(app.size, 1.2);
        app.key(Kind::Main, &Key::Character("-".into()), Some("-"));
        assert_eq!(app.size, 1.1);
        app.key(Kind::Eq, &Key::Character("0".into()), Some("0"));
        assert_eq!(app.size, 1.0);
        app.modifiers = ModifiersState::empty();
        let lib = app.logical_size(Kind::Library);
        app.menu_action("size:1.5", None);
        assert_eq!(app.logical_size(Kind::Main), LogicalSize::new(413, 174));
        assert_eq!(app.logical_size(Kind::Library).width, (lib.width as f64 * 1.5).round() as u32);
        app.save_session();
        let mut again = App::new(OutputKind::Null, None).unwrap();
        again.data_dir = app.data_dir.clone();
        again.load_session();
        assert_eq!(again.size, 1.5);
        // A session from before sizes: double size becomes 200%.
        std::fs::write(app.data_dir.join("session.json"), r#"{"double": true}"#).unwrap();
        again.load_session();
        assert_eq!(again.size, 2.0);
        let _ = std::fs::remove_dir_all(&app.data_dir);
    }

    #[test]
    fn sleep_timer_fades_out_then_stops_and_stop_after_skips_the_queue() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.set_volume(0.8, false);
        // Ten seconds left of a 30-second fade: a third of the volume.
        app.sleep_at = Some((Instant::now() + Duration::from_secs(10), SLEEP_FADE));
        app.tick();
        assert!((app.player.volume() - 0.8 / 3.0).abs() < 0.02, "{}", app.player.volume());
        // Time's up: stopped, the volume back for next time, timer gone.
        app.sleep_at = Some((Instant::now(), SLEEP_FADE));
        app.tick();
        assert_eq!(app.player.status().state, State::Stopped);
        assert_eq!(app.player.volume(), 0.8);
        assert!(app.sleep_at.is_none());
        app.menu_action("sleep:30", None);
        assert!(app.sleep_label().contains("30 min left"));
        app.menu_action("sleep:0", None);
        assert!(app.sleep_at.is_none());
        // Stop after current: nothing gets queued behind the track.
        app.playlist.set(vec![Entry::remote("/a".into(), "A".into(), String::new(), None, false), Entry::remote("/b".into(), "B".into(), String::new(), None, false)]);
        app.menu_action("opt:stopafter", None);
        let before = app.ids.len();
        app.queue_after(0);
        assert_eq!(app.ids.len(), before);
        app.menu_action("opt:stopafter", None);
        assert!(!app.stop_after);
    }

    #[test]
    fn soundcloud_lists_show_and_sort_by_date_plays_and_likes() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.lib_open(crate::library::Page::Search { sid: "soundcloud".into(), title: "SoundCloud › Search".into() }, true);
        let mk = |t: &str, d: &str, p: u64, l: u64| gamp_core::sources::Item { url: Some("/x".into()), date: d.into(), plays: Some(p), likes: Some(l), artist_link: "https://soundcloud.com/porter-robinson".into(), ..gamp_core::sources::Item::track("web:1", t).by("Porter Robinson") };
        let items = vec![mk("Old hit", "2012-04-10", 12_345_678, 900), mk("New", "2026-09-18", 4321, 245_000), mk("Middle", "2024-07-25", 101_838, 2714)];
        app.lib.cache.insert(app.lib.page.key(), gamp_core::sources::Listing { items, ..Default::default() });
        app.lib_show();
        let t = &app.lib.view.table;
        let heads: Vec<&str> = t.columns.iter().map(|c| c.0.as_str()).collect();
        assert_eq!(heads, ["Artist", "Title", "Posted", "Plays", "Likes", "Length"]);
        assert_eq!(t.rows[0].cells[2..5], ["2012-04-10", "12M", "900"]);
        let titles = |a: &App| a.lib.items.iter().map(|i| i.title.clone()).collect::<Vec<_>>();
        // A header click: newest / most first; again: the other way.
        for (col, first) in [(2, "New"), (3, "Old hit"), (4, "New")] {
            app.lib_click_header(col);
            assert_eq!(titles(&app)[0], first, "column {col}");
        }
        app.lib_click_header(4);
        assert_eq!(titles(&app)[0], "Old hit");
        // Right-click a track: the uploader's latest.
        app.lib.view.table.selected = std::collections::BTreeSet::from([0]);
        app.lib_artist();
        assert_eq!(app.lib.view.search.value, "https://soundcloud.com/porter-robinson");
        assert!(!app.pending_stream);
    }

    #[test]
    fn the_playlist_scrolls_to_the_track_that_plays() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        let songs: Vec<gamp_core::sources::Item> = (0..99).map(|i| gamp_core::sources::Item { url: Some(format!("/media/test-tone.wav?{i}")), ..gamp_core::sources::Item::track(i.to_string(), "Song") }).collect();
        app.lib_send(songs, true);
        app.playlist.current = 54;
        app.load_current(false);
        assert!(app.pl.top <= 54 && 54 < app.pl.top + app.pl.visible_rows());
    }

    #[test]
    fn a_long_menu_goes_on_in_more() {
        let items: Vec<Item> = (0..40).map(|i| Item::action(format!("x{i}"), "Preset")).collect();
        let fit = super::fit_menu(items, 300);
        assert!(menu::size(&fit).1 <= 300);
        assert!(matches!(fit.last(), Some(Item::Submenu { .. })));
        fn count(items: &[Item]) -> usize {
            items.iter().map(|i| if let Item::Submenu { items, .. } = i { count(items) } else { 1 }).sum()
        }
        assert_eq!(count(&fit), 40);
    }

    #[test]
    fn a_loose_file_favorited_from_the_playlist_plays_again() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        let path = std::env::temp_dir().join(format!("gamp-fav-{}.mp3", std::process::id()));
        std::fs::write(&path, b"").unwrap();
        let fav = gamp_core::sources::Item::track(format!("file\u{1f}{}", path.display()), "Song");
        app.lib_send(vec![fav], true);
        assert_eq!(app.playlist.entries[0].path, path);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn saved_playlists_keep_spotify_songs() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.data_dir = std::env::temp_dir().join(format!("gamp-lists-{}", std::process::id()));
        // Add to an empty playlist: there, but not playing.
        let song = gamp_core::sources::Item { url: Some("/media/spotify/4DIL3Xgnb9PAqewEf9FRZa.wav".into()), title: "Spineless".into(), artist: "Stick To Your Guns".into(), duration: Some(204.0), ..gamp_core::sources::Item::track("4DIL3Xgnb9PAqewEf9FRZa", "Spineless") };
        app.lib_send(vec![song], true);
        assert_eq!(app.playlist.entries.len(), 1);
        assert_eq!(app.player.status().state, State::Stopped);
        // Saved where LOAD LIST finds it, then loaded back.
        let dir = app.data_dir.join("playlists");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Road trip.m3u8"), app.m3u()).unwrap();
        let lists = app.saved_playlists();
        assert_eq!(lists.len(), 1);
        app.clear_playlist();
        app.menu_action(&format!("list:{}", lists[0].display()), None);
        let e = &app.playlist.entries[0];
        assert_eq!((e.url.as_deref(), e.artist.as_str(), e.title.as_str()), (Some("/media/spotify/4DIL3Xgnb9PAqewEf9FRZa.wav"), "Stick To Your Guns", "Spineless"));
        // A SoundCloud preview says so, with the length it really plays.
        let preview = gamp_core::sources::Item { url: Some("/media/soundcloud/web%3A1.mp3".into()), duration: Some(320.0), info: "SoundCloud only allows a 30-second preview of this track".into(), ..gamp_core::sources::Item::track("web:1", "One More Time") };
        app.lib_send(vec![preview], true);
        let e = app.playlist.entries.last().unwrap();
        assert_eq!((e.title.as_str(), e.duration), ("One More Time (preview)", Some(30.0)));
        let _ = std::fs::remove_dir_all(&app.data_dir);
    }

    #[test]
    fn equalizer_sliders_and_presets() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.redraw(Kind::Eq);
        // Drag the 1 kHz band (x 150) to the top: +12 dB.
        at(&mut app, Kind::Eq, 155, 43);
        app.press(Kind::Eq, None);
        app.release(Kind::Eq, None);
        assert_eq!(app.eq.bands[4], 12.0);
        assert_eq!(app.player.eq().bands[4], 12.0);
        // ON turns it off.
        click(&mut app, Kind::Eq, 20, 22);
        assert!(!app.eq.on && !app.player.eq().on);
        app.menu_action("preset:Rock", None);
        assert_eq!(app.eq.bands, eq_window::PRESETS.iter().find(|p| p.0 == "Rock").unwrap().1);
        app.menu_action("preset:flat", None);
        assert_eq!(app.eq.bands, [0.0; 10]);
        // A dropped .q1 library: every preset joins the menu, the first is used.
        app.data_dir = std::env::temp_dir().join(format!("gamp-eqf-{}", std::process::id()));
        let rock = eqf::Preset { name: "My Rock".into(), preamp: 3.0, bands: eq_window::PRESETS[12].1 };
        let mut lib = eqf::write(&rock);
        lib.extend_from_slice(&eqf::write(&eqf::Preset { name: "A/B".into(), ..rock.clone() })[31..]);
        let file = app.data_dir.join("lib.q1");
        std::fs::create_dir_all(&app.data_dir).unwrap();
        std::fs::write(&file, lib).unwrap();
        app.open_files(vec![file], true);
        assert!((app.eq.preamp - 3.0).abs() < 0.3 && (app.eq.bands[0] - rock.bands[0]).abs() < 0.3);
        let Item::Submenu { items, .. } = &app.preset_menu()[0] else { panic!() };
        let names: Vec<&str> = items.iter().filter_map(|i| if let Item::Action { label, .. } = i { Some(label.as_str()) } else { None }).collect();
        assert!(names.contains(&"My Rock") && names.contains(&"A-B"), "{names:?}");
        let _ = std::fs::remove_dir_all(&app.data_dir);
    }

    #[test]
    fn playlist_selection_menus_and_reordering() {
        let dir = songs("pl", 5, 2);
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.open_files(vec![dir.clone()], true);
        settle(&mut app);
        app.redraw(Kind::Playlist);
        let row = |i: i32| 23 + plw::ROW * i + 4;
        // Click row 3, shift-click row 1: rows 1..3 selected.
        click(&mut app, Kind::Playlist, 60, row(3));
        app.modifiers = ModifiersState::SHIFT;
        click(&mut app, Kind::Playlist, 60, row(1));
        app.modifiers = ModifiersState::empty();
        assert_eq!(app.pl.selected, BTreeSet::from([1, 2, 3]));
        assert!(app.pl.running.starts_with("0:06/0:10"), "{}", app.pl.running);
        // REM › SEL removes them.
        app.pl_menu_action(PlMenu::Remove, 2, None);
        assert_eq!(app.playlist.entries.len(), 2);
        assert_eq!(app.playlist.entries[1].title, "05 Song 4");
        // MISC › Sort list opens the sort menu; Reverse acts on it.
        app.pl_menu_action(PlMenu::Misc, 0, None);
        assert_eq!(app.menus.len(), 1);
        assert_eq!(app.menus[0].items.len(), 5);
        app.menu_action("sort:reverse", None);
        assert_eq!(app.playlist.entries[0].title, "05 Song 4");
        assert_eq!(app.playlist.current, 1, "the playing track keeps its place in the queue");
        app.menu_action("sort:title", None);
        assert_eq!(app.playlist.entries[1].title, "05 Song 4");
        assert_eq!(app.playlist.current, 0);
        // MISC › Misc opts: the HTML page lands in the data folder; tags re-read.
        app.pl_menu_action(PlMenu::Misc, 2, None);
        assert_eq!(app.menus[0].items.len(), 2);
        app.menu_action("misc:html", None);
        let page = std::fs::read_to_string(gamp_core::store::data_dir().join("playlist.html")).unwrap();
        assert!(page.contains("2 tracks in playlist"), "{page}");
        app.playlist.entries[1].title = "wrong".into();
        app.menu_action("misc:reread", None);
        assert_eq!(app.playlist.entries[1].title, "05 Song 4");
        assert_eq!(app.main.message.as_deref(), Some("READ INFO ON 2 FILES"));
        // A plain click on SEL is its bottom item, SEL ALL (ADD is add files).
        let (w, h) = app.pl.size();
        let sel_x = PlMenu::Select.x(w as i32) + 5;
        app.close_menus(); // the Misc opts pop-up is still open
        click(&mut app, Kind::Playlist, sel_x, h as i32 - 20);
        assert_eq!(app.pl.selected.len(), 2);
        assert!(app.pl.menu.is_none());
        // Double-click a row plays it.
        app.pl.selected.clear();
        click(&mut app, Kind::Playlist, 60, row(1));
        click(&mut app, Kind::Playlist, 60, row(1));
        settle(&mut app);
        assert_eq!(app.playlist.current, 1);
        // Reorder: select the first and move it down one.
        app.pl.selected = BTreeSet::from([0]);
        app.move_selection(1);
        assert_eq!(app.playlist.entries[0].title, "05 Song 4");
        assert_eq!(app.playlist.current, 0, "the playing track keeps its place in the queue");
        // Resizing snaps to 25×29 steps.
        assert_eq!(plw::snap_size(275 + 30, 145 + 10), (300, 145));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saved_playlists_keep_links() {
        let dir = songs("m3u", 2, 2);
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.open_files(vec![dir.clone()], true);
        settle(&mut app);
        app.playlist.append(vec![
            Entry::remote("/media/local/9?s=ab".into(), "Title".into(), "Art".into(), Some(200.0), false),
            Entry::remote("/media/remote/radio/x?s=cd".into(), "SomaFM".into(), String::new(), None, true),
        ]);
        let text = app.m3u();
        assert!(text.contains("http://127.0.0.1:8742/media/local/9?s=ab"), "{text}");
        let list = dir.join("saved.m3u8");
        std::fs::write(&list, &text).unwrap();
        // A plain station address, as another program would write it.
        std::fs::write(&list, text + "#EXTINF:-1,Groove Salad\nhttp://ice.somafm.com/groovesalad\n").unwrap();
        app.open_files(vec![list], true);
        settle(&mut app);
        let e = &app.playlist.entries;
        assert_eq!(e.len(), 5, "{e:?}");
        assert_eq!(e[0].title, "01 Song 0");
        assert_eq!((e[2].url.as_deref(), e[2].artist.as_str(), e[2].title.as_str(), e[2].duration, e[2].live), (Some("/media/local/9?s=ab"), "Art", "Title", Some(200.0), false));
        assert_eq!((e[3].url.as_deref(), e[3].title.as_str(), e[3].live), (Some("/media/remote/radio/x?s=cd"), "SomaFM", true));
        assert!(e[4].url.as_deref().unwrap().starts_with("/media/remote/radio/http"), "{:?}", e[4].url);
        assert_eq!((e[4].title.as_str(), e[4].live), ("Groove Salad", true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn jump_to_file_finds_and_plays() {
        let dir = songs("jump", 5, 2);
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.open_files(vec![dir.clone()], true);
        settle(&mut app);
        let typed = |app: &mut App, t: &str| {
            for c in t.chars() {
                app.key(Kind::Jump, &Key::Character(c.to_string().into()), Some(&c.to_string()));
            }
        };
        // J opens it listing everything, the playing track picked.
        app.key(Kind::Main, &Key::Character("j".into()), Some("j"));
        assert!(app.jump.open);
        assert_eq!(app.jump.matches.len(), 5);
        assert_eq!(app.jump.view.selected, Some(0));
        assert!(app.jump.view.rows[0].current);
        // Words match anywhere, in any case.
        typed(&mut app, "SONG 4");
        assert_eq!(app.jump.matches, vec![3, 4], "'4' is in '04 Song 3' too");
        typed(&mut app, " 05");
        assert_eq!(app.jump.matches, vec![4]);
        assert_eq!(app.jump.view.status, "1 of 5");
        app.key(Kind::Jump, &Key::Named(NamedKey::Enter), None);
        settle(&mut app);
        assert_eq!(app.playlist.current, 4);
        assert!(!app.jump.open, "Enter plays and closes");
        // No match: Enter does nothing; Escape closes.
        app.jump_toggle();
        typed(&mut app, "zzz");
        assert!(app.jump.matches.is_empty() && app.jump.view.selected.is_none());
        app.key(Kind::Jump, &Key::Named(NamedKey::Enter), None);
        assert!(app.jump.open);
        app.key(Kind::Jump, &Key::Named(NamedKey::Backspace), None);
        app.key(Kind::Jump, &Key::Named(NamedKey::Backspace), None);
        app.key(Kind::Jump, &Key::Named(NamedKey::Backspace), None);
        assert_eq!(app.jump.matches.len(), 5);
        assert_eq!(app.jump.view.selected, Some(4), "the playing track is picked");
        app.key(Kind::Jump, &Key::Named(NamedKey::ArrowDown), None);
        assert_eq!(app.jump.view.selected, Some(4), "clamped at the end");
        app.key(Kind::Jump, &Key::Named(NamedKey::ArrowUp), None);
        app.key(Kind::Jump, &Key::Named(NamedKey::ArrowUp), None);
        assert_eq!(app.jump.view.selected, Some(2));
        app.key(Kind::Jump, &Key::Named(NamedKey::Escape), None);
        assert!(!app.jump.open);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_info_shows_and_saves_tags() {
        let dir = songs("info", 2, 3);
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.open_files(vec![dir.clone()], true);
        settle(&mut app);
        app.pl.selected = BTreeSet::from([1]);
        app.pl_menu_action(PlMenu::Misc, 1, None);
        assert!(app.info.open);
        assert_eq!(app.info.view.heading, "2. 02 Song 1.wav");
        let get = |app: &App, l: &str| app.info.view.rows.iter().find(|(k, _)| k == l).map(|(_, v)| v.clone()).unwrap_or_default();
        assert!(get(&app, "Format").contains("WAV") && get(&app, "Format").contains("22.05 kHz") && get(&app, "Format").contains("mono"), "{:?}", app.info.view.rows);
        assert_eq!(get(&app, "Length"), "0:03");
        assert!(app.info.view.place.ends_with("02 Song 1.wav"));
        // The Title field has focus; type a title, Tab to Artist, Enter saves.
        assert_eq!(app.info.focus, Some(0));
        let typed = |app: &mut App, t: &str| {
            for c in t.chars() {
                app.key(Kind::Info, &Key::Character(c.to_string().into()), Some(&c.to_string()));
            }
        };
        typed(&mut app, "Nightcall");
        app.key(Kind::Info, &Key::Named(NamedKey::Tab), None);
        typed(&mut app, "Kavinsky");
        app.key(Kind::Info, &Key::Named(NamedKey::Tab), None);
        app.key(Kind::Info, &Key::Named(NamedKey::Tab), None);
        typed(&mut app, "2010");
        app.key(Kind::Info, &Key::Named(NamedKey::Tab), None);
        typed(&mut app, "3/12");
        app.key(Kind::Info, &Key::Named(NamedKey::Enter), None);
        assert_eq!(app.info.view.status, "Saved");
        assert!(app.info.open, "stays open after saving");
        assert_eq!(app.playlist.entries[1].display(), "Kavinsky - Nightcall");
        assert_eq!(app.pl.tracks[1].title, "Kavinsky - Nightcall");
        // Reopening reads the saved tags back from the file.
        app.key(Kind::Info, &Key::Named(NamedKey::Escape), None);
        assert!(!app.info.open);
        app.modifiers = ModifiersState::ALT;
        app.key(Kind::Playlist, &Key::Character("3".into()), None);
        app.modifiers = ModifiersState::empty();
        let values: Vec<&str> = app.info.view.fields.iter().map(|(_, f)| f.value.as_str()).collect();
        assert_eq!(values, ["Nightcall", "Kavinsky", "", "2010", "3/12", "", ""]);
        // A network track is read-only.
        app.playlist.append(vec![Entry::remote("/media/local/9".into(), "Title".into(), "Art".into(), Some(200.0), false)]);
        app.info_show(Some(2));
        assert!(app.info.view.fields.is_empty());
        assert!(app.info.view.rows.contains(&("Artist".to_owned(), "Art".to_owned())) && app.info.view.rows.contains(&("Length".to_owned(), "3:20".to_owned())), "{:?}", app.info.view.rows);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn menus_open_and_act() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.context_menu(Kind::Main, None);
        assert_eq!(app.menus.len(), 1);
        // Hover "Options" opens its sub-menu; pick "Double size".
        let options = app.menus[0].items.iter().position(|i| matches!(i, Item::Submenu { label, .. } if label == "Options")).unwrap();
        app.redraw(Kind::Menu(0));
        let y = menu::item_top(&app.menus[0].items, options) + 4;
        app.menu_hover(0, 20, y, None);
        assert_eq!(app.menus.len(), 2);
        app.redraw(Kind::Menu(1));
        let dbl = app.menus[1].items.iter().position(|i| matches!(i, Item::Action { id, .. } if id == "opt:double")).unwrap();
        let y = menu::item_top(&app.menus[1].items, dbl) + 4;
        app.menus[1].win.cursor = (20, y);
        app.release(Kind::Menu(1), None);
        assert_eq!(app.size, 2.0);
        assert!(app.menus.is_empty());
        // A click elsewhere closes an open menu without acting.
        app.context_menu(Kind::Main, None);
        click(&mut app, Kind::Main, 45, 95);
        assert!(app.menus.is_empty());
    }

    #[test]
    fn gapless_runs_through_the_list() {
        let dir = songs("gapless", 2, 1);
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.open_files(vec![dir.clone()], true);
        let start = Instant::now();
        let mut saw_second = false;
        // Generous: CI machines run every test at once.
        while start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(30));
            app.tick();
            saw_second |= app.playlist.current == 1 && app.main.state == PlayState::Playing;
            if saw_second && app.main.state == PlayState::Stopped {
                break;
            }
        }
        assert!(saw_second);
        assert_eq!(app.main.state, PlayState::Stopped);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clocks() {
        assert_eq!(clock(125.0), "2:05");
        assert_eq!(clock(3725.0), "1:02:05");
    }

    #[test]
    fn windows_dock_when_opened_and_close_up_when_closed() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        for k in [Kind::Main, Kind::Eq, Kind::Playlist, Kind::Library, Kind::Vis] {
            app.redraw(k);
        }
        let r = |app: &App, k| app.rect(k).unwrap();
        let (m, eq) = (r(&app, Kind::Main), r(&app, Kind::Eq).h);
        // Stack: main, eq, playlist; the library and G-Drop closed.
        app.wins.get_mut(&Kind::Vis).unwrap().show(false);
        app.wins.get_mut(&Kind::Eq).unwrap().move_to(m.x, m.bottom());
        app.wins.get_mut(&Kind::Playlist).unwrap().move_to(m.x, m.bottom() + eq);
        // Closing the equalizer pulls the playlist up under the main window.
        app.toggle_window(Kind::Eq);
        assert_eq!(r(&app, Kind::Playlist).y, m.bottom());
        // Opening it again slots it back in and pushes the playlist down.
        app.toggle_window(Kind::Eq);
        assert_eq!(r(&app, Kind::Eq).y, m.bottom());
        assert_eq!(r(&app, Kind::Playlist).y, m.bottom() + eq);
        // The library, parked far away, docks to the right of the stack.
        app.wins.get_mut(&Kind::Library).unwrap().show(false);
        app.wins.get_mut(&Kind::Library).unwrap().move_to(2000, 2000);
        app.toggle_window(Kind::Library);
        assert_eq!((r(&app, Kind::Library).x, r(&app, Kind::Library).y), (m.right(), m.y));
        // G-Drop docks beside the library, even if it last sat against a stray window.
        app.wins.get_mut(&Kind::Playlist).unwrap().move_to(3000, 3000);
        let vis_w = app.win(Kind::Vis).unwrap().size_px().0;
        app.wins.get_mut(&Kind::Vis).unwrap().move_to(3000 - vis_w, 3000);
        app.toggle_window(Kind::Vis);
        assert_eq!(r(&app, Kind::Vis).x, r(&app, Kind::Library).right());
    }

    #[test]
    fn docked_windows_go_back_when_the_displays_change() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        for k in [Kind::Main, Kind::Eq, Kind::Playlist, Kind::Library] {
            app.redraw(k);
            app.wins.get_mut(&k).unwrap().show(true);
        }
        app.wins.get_mut(&Kind::Main).unwrap().move_to(100, 100);
        let m = app.rect(Kind::Main).unwrap();
        let eq = app.rect(Kind::Eq).unwrap().h;
        app.wins.get_mut(&Kind::Eq).unwrap().move_to(m.x, m.bottom());
        app.wins.get_mut(&Kind::Playlist).unwrap().move_to(m.x, m.bottom() + eq);
        app.wins.get_mut(&Kind::Library).unwrap().move_to(2000, 2000);
        let laptop = vec![Rect { x: 0, y: 0, w: 1512, h: 982 }];
        app.redock(laptop.clone());
        // A bigger monitor: the system pushes the equalizer away.
        app.wins.get_mut(&Kind::Eq).unwrap().move_to(900, 40);
        app.redock(vec![Rect { x: 0, y: 0, w: 2560, h: 1440 }]);
        let pos = |app: &App, k| app.win(k).unwrap().pos;
        assert_eq!(pos(&app, Kind::Eq), (m.x, m.bottom()));
        assert_eq!(pos(&app, Kind::Playlist), (m.x, m.bottom() + eq));
        // Not docked before, not moved now.
        assert_eq!(pos(&app, Kind::Library), (2000, 2000));
        // Same displays: moving a window yourself stays.
        app.docked.2 = None;
        app.redock(vec![Rect { x: 0, y: 0, w: 2560, h: 1440 }]);
        app.wins.get_mut(&Kind::Eq).unwrap().move_to(900, 40);
        app.redock(vec![Rect { x: 0, y: 0, w: 2560, h: 1440 }]);
        assert_eq!(pos(&app, Kind::Eq), (900, 40));
        // Full-screen G-Drop on its own Space: the menu bar hides, nothing moves.
        app.drop.fullscreen = true;
        app.wins.get_mut(&Kind::Eq).unwrap().move_to(901, 40);
        app.redock(vec![Rect { x: 0, y: 0, w: 2560, h: 1415 }]);
        assert_eq!(pos(&app, Kind::Eq), (901, 40));
    }

    #[test]
    fn gather_puts_scattered_windows_back_around_the_main_window() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        for k in [Kind::Main, Kind::Eq, Kind::Playlist, Kind::Library, Kind::Vis] {
            app.redraw(k);
            app.wins.get_mut(&k).unwrap().show(true);
        }
        for (k, p) in [(Kind::Main, (100, 100)), (Kind::Eq, (-3000, 50)), (Kind::Playlist, (4000, 4000)), (Kind::Library, (10, 900)), (Kind::Vis, (-500, -500))] {
            app.wins.get_mut(&k).unwrap().move_to(p.0, p.1);
        }
        app.gather();
        let r = |k| app.rect(k).unwrap();
        let m = r(Kind::Main);
        assert_eq!((m.x, m.y), (100, 100));
        assert_eq!((r(Kind::Eq).x, r(Kind::Eq).y), (m.x, m.bottom()));
        assert_eq!((r(Kind::Playlist).x, r(Kind::Playlist).y), (m.x, r(Kind::Eq).bottom()));
        assert_eq!((r(Kind::Library).x, r(Kind::Library).y), (m.right(), m.y));
        assert_eq!((r(Kind::Vis).x, r(Kind::Vis).y), (r(Kind::Library).right(), m.y));
    }

    #[test]
    fn only_docked_windows_move_with_the_equalizer() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        for k in [Kind::Main, Kind::Eq, Kind::Playlist] {
            app.redraw(k);
        }
        let pos = |app: &App, k| app.win(k).unwrap().pos;
        let mh = app.rect(Kind::Main).unwrap().h;
        // The equalizer floats near the top; main and playlist are stacked lower.
        app.wins.get_mut(&Kind::Main).unwrap().move_to(0, 500);
        app.wins.get_mut(&Kind::Playlist).unwrap().move_to(0, 500 + mh);
        app.wins.get_mut(&Kind::Eq).unwrap().move_to(1000, 0);
        app.toggle_window(Kind::Eq);
        assert_eq!((pos(&app, Kind::Main), pos(&app, Kind::Playlist)), ((0, 500), (0, 500 + mh)), "closing a floating equalizer moves nothing");
        // A playlist parked off to the side isn't pushed when the equalizer docks.
        app.wins.get_mut(&Kind::Playlist).unwrap().move_to(1500, 900);
        app.toggle_window(Kind::Eq);
        assert_eq!(pos(&app, Kind::Eq), (0, 500 + mh));
        assert_eq!(pos(&app, Kind::Playlist), (1500, 900));
    }

    #[test]
    fn a_message_scrolls_from_its_start_and_the_title_resumes() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.main.title = "A TITLE FAR TOO LONG TO FIT IN THE LITTLE DISPLAY".into();
        app.tick();
        app.main.scroll = 40;
        app.say("A MESSAGE THAT IS ALSO MUCH TOO LONG FOR THE DISPLAY");
        app.tick();
        assert_eq!(app.main.scroll, 0, "the message starts at its beginning");
        app.main.message = None;
        app.tick();
        assert_eq!(app.main.scroll, 40, "the title picks up where it was");
    }
}

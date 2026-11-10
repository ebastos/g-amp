//! The Media Library window's behaviour: the source tree, lists loaded in
//! the background, the table, search, playing and queueing, and the
//! Settings forms (folders, media servers, Dropbox, Spotify, SoundCloud).

use std::collections::{BTreeSet, HashMap};

use gamp_core::catalog::Source;
use gamp_core::skin_museum as museum;
use gamp_core::sources::{Action, Item, Listing};
use gamp_ui::lang::{t, tf};
use gamp_ui::library_window::{self as lw, Field, Hot, Row, TreeRow, W};
use serde_json::{Value, json};

use crate::app::{App, Kind, clock};
use crate::backend::{Reply, open_in_browser};
use crate::playlist::Entry;

/// Where museum skins wait in the data folder (Keep this skin copies one to skins/).
pub const MUSEUM_DIR: &str = "museum";

/// Recent searches kept for each source, shown under its Search.
const RECENT: usize = 5;
/// A recent search, listed on its Search page.
const RECENT_KIND: &str = "search";

/// What a tree row opens.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// A group row. `sid` is set for a server (`src:<sid>`).
    Group { key: String, sid: Option<String> },
    Open { sid: String, node: String, title: String },
    Search { sid: String, title: String },
    /// A recent search, run again.
    Recent { sid: String, title: String, query: String },
    /// Settings, scrolled to the section whose heading starts so.
    Settings(Option<&'static str>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Page {
    List { sid: String, node: String, title: String },
    Search { sid: String, title: String },
    Settings,
}

impl Page {
    pub fn key(&self) -> String {
        match self {
            Page::List { sid, node, .. } => list_key(sid, node),
            Page::Search { sid, .. } => list_key(sid, "search"),
            Page::Settings => "settings".into(),
        }
    }
}

/// Media server kinds for the "Add server" form.
const KINDS: &[(&str, &str, &str)] = &[
    ("jellyfin", "Jellyfin or Emby", "http://nas:8096"),
    ("subsonic", "Navidrome / Subsonic", "http://nas:4533"),
    ("synology", "Synology Audio Station", "QuickConnect ID, or http://diskstation:5000"),
    ("webdav", "WebDAV / Nextcloud", "https://nas:5006/music"),
    ("plex", "Plex (address and token)", "http://nas:32400"),
];

pub struct LibState {
    pub view: lw::View,
    pub hots: Vec<(Hot, lw::R)>,
    pub sources: Vec<Source>,
    pub targets: Vec<Target>,
    /// Groups that start open (Local Media, Network & Cloud, …) and are closed.
    pub collapsed: BTreeSet<String>,
    /// Groups that start closed (a server, the DLNA group) and are open.
    pub opened: BTreeSet<String>,
    /// A network source whose last listing failed: sid → the error.
    pub offline: HashMap<String, String>,
    /// Where the tree menu was opened, so Remove… can confirm in the same place.
    pub menu_at: (i32, i32),
    pub page: Page,
    pub trail: Vec<Page>,
    pub cache: HashMap<String, Listing>,
    /// Folders open on each page (`page.key()` → folder ids). Nested ids stay when a folder closes.
    pub expanded: HashMap<String, BTreeSet<String>>,
    /// The rows shown, as items (filtered and sorted).
    pub items: Vec<Item>,
    /// Depth of each `items` row.
    pub levels: Vec<u8>,
    /// A folder play waiting on its listing (`listing key`, enqueue).
    pub pending_play: Option<(String, bool)>,
    /// Child browse that failed, so the row can say why.
    pub failed: HashMap<String, String>,
    /// Child listings already asked for.
    pub inflight: BTreeSet<String>,
    pub actions: Vec<Action>,
    pub hint: Option<String>,
    pub filter: String,
    pub sort: Option<(usize, bool)>,
    pub loading: bool,
    pub fields: HashMap<String, Field>,
    pub focus: Option<String>,
    pub config: Value,
    pub kind: usize,
    pub insecure: bool,
    pub scan: Option<gamp_core::library::Status>,
    pub anchor: Option<usize>,
    /// Each source's recent searches, newest first (kept in the session).
    pub recent: HashMap<String, Vec<String>>,
}

impl Default for LibState {
    fn default() -> Self {
        let mut fields = HashMap::new();
        for (id, ph, pw) in [
            ("srv_name", "Name (optional)", false),
            ("srv_url", "http://nas:8096", false),
            ("srv_user", "User name", false),
            ("srv_pass", "Password", true),
            ("srv_otp", "Two-step code (if you use one)", false),
            ("folder_path", "Or type a path", false),
            ("share", "smb://diskstation/music", false),
            ("sc_profile", "Your profile: soundcloud.com/you", false),
        ] {
            fields.insert(id.to_owned(), Field { placeholder: ph.into(), password: pw, ..Default::default() });
        }
        let view = lw::View { width: 550, height: 348, search_shown: true, search: Field::new("Search"), ..Default::default() };
        LibState {
            view,
            hots: Vec::new(),
            sources: Vec::new(),
            targets: Vec::new(),
            collapsed: BTreeSet::new(),
            opened: BTreeSet::new(),
            offline: HashMap::new(),
            menu_at: (0, 0),
            page: Page::List { sid: "local".into(), node: "audio".into(), title: "Local Media › Audio".into() },
            trail: Vec::new(),
            cache: HashMap::new(),
            expanded: HashMap::new(),
            items: Vec::new(),
            levels: Vec::new(),
            pending_play: None,
            failed: HashMap::new(),
            inflight: BTreeSet::new(),
            actions: Vec::new(),
            hint: None,
            filter: String::new(),
            sort: None,
            loading: false,
            fields,
            focus: None,
            config: Value::Null,
            kind: 0,
            insecure: false,
            scan: None,
            anchor: None,
            recent: HashMap::new(),
        }
    }
}

/// The Favorites page's cache key.
const FAVORITES_KEY: &str = "favorites\u{1f}favorites";

fn is_playable(i: &Item) -> bool {
    i.kind == "track"
}

/// A stand-in row: still loading, or a browse that failed.
fn is_filler(i: &Item) -> bool {
    matches!(i.kind.as_str(), "loading" | "note")
}

fn list_key(sid: &str, node: &str) -> String {
    format!("{sid}\u{1f}{node}")
}

fn listing_sid(key: &str) -> &str {
    key.split('\u{1f}').next().unwrap_or("")
}

/// A server subgroup and the DLNA group start closed. Everything else starts open.
fn group_starts_closed(key: &str) -> bool {
    key.starts_with("src:") || key == "dlna"
}

fn group_is_open(key: &str, collapsed: &BTreeSet<String>, opened: &BTreeSet<String>) -> bool {
    if group_starts_closed(key) { opened.contains(key) } else { !collapsed.contains(key) }
}

/// A saved account can be removed. Cloud folders and anything marked not removable stay.
fn can_remove(s: &Source) -> bool {
    s.removable && !s.id.starts_with("cloud-")
}

enum TreeCmd {
    Open(String),
    Refresh(String),
    Hide(String),
    /// Asks first. Does not delete.
    Confirm(String),
    Remove(String),
    Manage,
    Cancel,
}

fn tree_cmd(id: &str) -> Option<TreeCmd> {
    if id == "lib:tree:manage" {
        return Some(TreeCmd::Manage);
    }
    if id == "lib:tree:cancel" {
        return Some(TreeCmd::Cancel);
    }
    let rest = id.strip_prefix("lib:tree:")?;
    // `remove!` before `remove`, so the confirm id is not read as the ask.
    let (kind, sid) = if let Some(sid) = rest.strip_prefix("remove!:") {
        ("remove!", sid)
    } else {
        rest.split_once(':')?
    };
    if sid.is_empty() {
        return None;
    }
    let sid = sid.to_owned();
    Some(match kind {
        "open" => TreeCmd::Open(sid),
        "refresh" => TreeCmd::Refresh(sid),
        "hide" => TreeCmd::Hide(sid),
        "remove" => TreeCmd::Confirm(sid),
        "remove!" => TreeCmd::Remove(sid),
        _ => return None,
    })
}

fn source_menu(s: &Source) -> Vec<gamp_ui::menu::Item> {
    use gamp_ui::menu::Item as M;
    let mut menu = vec![M::action(format!("lib:tree:open:{}", s.id), "Open"), M::action(format!("lib:tree:refresh:{}", s.id), "Refresh")];
    if s.id.starts_with("dlna-") {
        menu.push(M::action(format!("lib:tree:hide:{}", s.id), "Hide"));
    }
    if can_remove(s) {
        menu.push(M::action(format!("lib:tree:remove:{}", s.id), "Remove…"));
    }
    menu.push(M::action("lib:tree:manage", "Manage Network & Cloud…"));
    menu
}

/// The confirm step for Remove…. Only `lib:tree:remove!:` deletes.
fn lib_remove_confirm(name: &str, sid: &str) -> Vec<gamp_ui::menu::Item> {
    use gamp_ui::menu::Item as M;
    vec![M::action(format!("lib:tree:remove!:{sid}"), tf("Remove {}", &[&name])), M::action("lib:tree:cancel", "Cancel")]
}

fn tree_row_menu(targets: &[Target], sources: &[Source], i: usize) -> Option<Vec<gamp_ui::menu::Item>> {
    use gamp_ui::menu::Item as M;
    let t = targets.get(i)?;
    match t {
        Target::Group { key, .. } if key == "network" => Some(vec![M::action("lib:tree:manage", "Manage Network & Cloud…")]),
        Target::Group { key, sid } if key.starts_with("src:") => {
            let id = sid.as_deref().or_else(|| key.strip_prefix("src:"))?;
            sources.iter().find(|s| s.id == id).map(source_menu)
        }
        Target::Open { sid, .. } | Target::Search { sid, .. } | Target::Recent { sid, .. } => {
            let s = sources.iter().find(|s| s.id == *sid)?;
            (s.group == "network" || s.group == "internet").then(|| source_menu(s))
        }
        _ => None,
    }
}

fn item_text_has(i: &Item, filter: &str) -> bool {
    format!("{} {} {} {}", i.artist, i.title, i.album, i.info).to_lowercase().contains(filter)
}

fn sort_key(i: &Item, col: &str) -> String {
    match col {
        "Artist" | "Genre" => i.artist.to_lowercase(),
        "Album" | "Format" => i.album.to_lowercase(),
        "Length" => format!("{:010}", i.duration.unwrap_or(0.0) as u64),
        "Items" => format!("{:010}", i.count.unwrap_or(0)),
        "Posted" => i.date.clone(),
        "Plays" => format!("{:012}", i.plays.unwrap_or(0)),
        "Likes" => format!("{:012}", i.likes.unwrap_or(0)),
        _ => i.title.to_lowercase(),
    }
}

/// Rows that were selected before `lib_show` rebuilt the table, matched by id and depth.
struct Kept {
    selected: Vec<(String, u8)>,
    anchor: Option<(String, u8)>,
    /// The anchor row was inside the visible window.
    visible: bool,
}

/// Inputs shared by one `lib_show` walk. Children are collected aside so a
/// folder the filter rejects can drop them.
struct Walk<'a> {
    sid: &'a str,
    open: &'a BTreeSet<String>,
    filter: &'a str,
    sort: Option<(&'a str, bool)>,
    stack: &'a mut BTreeSet<String>,
    fetches: &'a mut Vec<(String, String)>,
}

impl App {
    pub fn lib_start(&mut self) {
        if let Some(b) = &self.backend {
            b.refresh_sources();
            b.config();
            b.scan_status();
            // DLNA servers answer a few seconds after the search starts.
            self.pending_refresh = Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
        }
        self.lib_open(self.lib.page.clone(), true);
    }

    // ---- replies from the background ----------------------------------------

    pub fn lib_poll(&mut self) {
        let Some(b) = &self.backend else { return };
        let replies: Vec<Reply> = b.rx.try_iter().collect();
        // Called every tick: redraw only when something came in.
        if replies.is_empty() {
            return;
        }
        for r in replies {
            match r {
                Reply::Sources(s) => {
                    self.lib.sources = s;
                    self.lib_tree();
                }
                Reply::Listing { key, result } => self.lib_listing(key, result),
                Reply::Config(c) => {
                    if let Some(k) = c["soundcloud"]["profile"].as_str() {
                        self.lib_field_default("sc_profile", k);
                    }
                    // Still looking for DLNA servers: look at the list again shortly.
                    if c["dlnaSearching"] == true && self.pending_refresh.is_none() {
                        self.pending_refresh = Some(std::time::Instant::now() + std::time::Duration::from_secs(2));
                    }
                    self.lib.config = c;
                    if self.lib.page == Page::Settings {
                        self.lib_form();
                    }
                    self.lib_tree();
                }
                Reply::Message(m) => {
                    self.lib.view.status = m.clone();
                    self.main.message = None;
                    // The settings form covers the status line, or the library is
                    // closed (Add to Favorites from the playlist): say it in the display.
                    let lib_hidden = !self.wins.get(&Kind::Library).is_some_and(|w| w.visible);
                    if (self.lib.page == Page::Settings || lib_hidden) && !m.is_empty() {
                        self.say(&m);
                    }
                    // Favourites and subscriptions changed: reload them.
                    self.lib.cache.retain(|k, _| !(k.ends_with("favorites") || k.ends_with("subscriptions")));
                    if let Page::List { node, .. } = &self.lib.page {
                        if node == "favorites" || node == "subscriptions" {
                            let p = self.lib.page.clone();
                            self.lib_open(p, false);
                        }
                    }
                }
                Reply::Skin(Ok(path)) => {
                    let name = gamp_core::skin_museum::name(&path.file_name().unwrap_or_default().to_string_lossy());
                    self.load_skin(Some(path));
                    self.say_f("SKIN: {}", &[&name.to_uppercase()]);
                }
                Reply::Skin(Err(e)) => self.say(&e.to_uppercase()),
                Reply::Scan(s) => {
                    let scanning = s.scanning;
                    // Follow a running scan once a second.
                    self.pending_scan = scanning.then(|| std::time::Instant::now() + std::time::Duration::from_secs(1));
                    self.lib.scan = Some(s);
                    if !scanning {
                        self.lib.cache.retain(|k, _| !k.starts_with("local"));
                        if matches!(&self.lib.page, Page::List { sid, .. } if sid == "local") {
                            let p = self.lib.page.clone();
                            self.lib_open(p, false);
                        }
                    }
                    if self.lib.page == Page::Settings {
                        self.lib_form();
                    }
                }
            }
        }
        self.mark(Kind::Library);
    }

    fn lib_field_default(&mut self, id: &str, value: &str) {
        if let Some(f) = self.lib.fields.get_mut(id) {
            if f.value.is_empty() && !f.focused {
                f.value = value.to_owned();
                f.end();
            }
        }
    }

    // ---- the tree ---------------------------------------------------------------

    pub fn lib_tree(&mut self) {
        let mut rows = Vec::new();
        let mut targets = Vec::new();
        let here = self.lib.trail.first().cloned().unwrap_or_else(|| self.lib.page.clone());
        let collapsed = self.lib.collapsed.clone();
        let opened = self.lib.opened.clone();
        let offline = self.lib.offline.clone();
        // The search being shown, if it's one of the recent ones: that row lights up, not Search.
        let query = self.lib.view.search.value.trim().to_owned();
        let recent = self.lib.recent.clone();
        let on_recent = |sid: &str| recent.get(sid).is_some_and(|r| r.contains(&query));
        let group = |rows: &mut Vec<TreeRow>, targets: &mut Vec<Target>, key: &str, label: &str, depth: u8, sid: Option<&str>, dim: bool| -> bool {
            let open = group_is_open(key, &collapsed, &opened);
            rows.push(TreeRow { label: label.into(), depth, group: true, open, selected: false, dim });
            targets.push(Target::Group { key: key.into(), sid: sid.map(str::to_owned) });
            open
        };
        let leaf = |rows: &mut Vec<TreeRow>, targets: &mut Vec<Target>, t: Target, label: &str, depth: u8| {
            let selected = match (&t, &here) {
                (Target::Open { sid, node, .. }, Page::List { sid: s2, node: n2, .. }) => sid == s2 && node == n2,
                (Target::Search { sid, .. }, Page::Search { sid: s2, .. }) => sid == s2 && !on_recent(sid),
                (Target::Recent { sid, query: q, .. }, Page::Search { sid: s2, .. }) => sid == s2 && *q == query,
                (Target::Settings(None), Page::Settings) => true,
                _ => false,
            };
            rows.push(TreeRow { label: label.into(), depth, group: false, open: false, selected, dim: false });
            targets.push(t);
        };
        leaf(&mut rows, &mut targets, Target::Open { sid: "favorites".into(), node: "favorites".into(), title: "Favorites".into() }, "Favorites", 0);
        let src: Vec<Source> = self.lib.sources.iter().filter(|s| !s.hidden).cloned().collect();
        let source_rows = |rows: &mut Vec<TreeRow>, targets: &mut Vec<Target>, s: &Source, depth: u8| {
            for (node, title) in &s.roots {
                leaf(rows, targets, Target::Open { sid: s.id.clone(), node: node.clone(), title: format!("{} › {title}", s.name) }, title, depth);
            }
            if s.search {
                let title = format!("{} › Search", s.name);
                leaf(rows, targets, Target::Search { sid: s.id.clone(), title: title.clone() }, "Search", depth);
                for q in recent.get(&s.id).into_iter().flatten() {
                    leaf(rows, targets, Target::Recent { sid: s.id.clone(), title: title.clone(), query: q.clone() }, q, depth + 1);
                }
            }
            if s.needs_setup {
                let section = if s.id == "spotify" { "Spotify" } else { "SoundCloud" };
                leaf(rows, targets, Target::Settings(Some(section)), "Connect…", depth);
            }
        };
        let add_source = |rows: &mut Vec<TreeRow>, targets: &mut Vec<Target>, s: &Source, depth: u8| {
            let key = format!("src:{}", s.id);
            let dim = offline.contains_key(&s.id);
            let label = if dim { format!("{}{}", s.name, t(" · offline")) } else { s.name.clone() };
            if group(rows, targets, &key, &label, depth, Some(&s.id), dim) {
                source_rows(rows, targets, s, depth + 1);
            }
        };
        if let Some(local) = src.iter().find(|s| s.id == "local") {
            if group(&mut rows, &mut targets, "local", "Local Media", 0, None, false) {
                source_rows(&mut rows, &mut targets, local, 1);
            }
        }
        if group(&mut rows, &mut targets, "network", "Network & Cloud", 0, None, false) {
            let network: Vec<&Source> = src.iter().filter(|s| s.group == "network").collect();
            let (dlna, rest): (Vec<&Source>, Vec<&Source>) = network.into_iter().partition(|s| s.id.starts_with("dlna-"));
            if rest.is_empty() && dlna.is_empty() {
                leaf(&mut rows, &mut targets, Target::Settings(Some("Media servers")), "Add a server…", 1);
            }
            for s in &rest {
                add_source(&mut rows, &mut targets, s, 1);
            }
            if !dlna.is_empty() {
                let n = dlna.len();
                let label = tf("On this network ({})", &[&n]);
                if group(&mut rows, &mut targets, "dlna", &label, 1, None, false) {
                    for s in &dlna {
                        add_source(&mut rows, &mut targets, s, 2);
                    }
                }
            }
        }
        if group(&mut rows, &mut targets, "internet", "Internet", 0, None, false) {
            for s in src.iter().filter(|s| s.group == "internet") {
                add_source(&mut rows, &mut targets, s, 1);
            }
        }
        for id in ["soundcloud", "spotify", "skins"] {
            if let Some(s) = src.iter().find(|s| s.id == id) {
                if group(&mut rows, &mut targets, id, &s.name, 0, None, false) {
                    source_rows(&mut rows, &mut targets, s, 1);
                }
            }
        }
        leaf(&mut rows, &mut targets, Target::Settings(None), "Settings", 0);
        self.lib.view.tree = rows;
        self.lib.targets = targets;
        self.mark(Kind::Library);
    }

    // ---- pages ----------------------------------------------------------------

    pub fn lib_open(&mut self, page: Page, from_tree: bool) {
        // A page opened again fetches itself and any folder that failed last time.
        self.lib_drop_failure(&page.key());
        if let Page::List { sid, .. } | Page::Search { sid, .. } = &page {
            let sid = sid.clone();
            let ids: Vec<String> = self.lib.expanded.get(&page.key()).map(|s| s.iter().cloned().collect()).unwrap_or_default();
            for id in ids {
                self.lib_drop_failure(&list_key(&sid, &id));
            }
        }
        if from_tree {
            self.lib.trail.clear();
        }
        self.lib.page = page.clone();
        self.lib.view.table.selected.clear();
        self.lib.view.table.top = 0;
        self.lib.anchor = None;
        self.lib.sort = None;
        self.lib.filter.clear();
        self.lib.levels.clear();
        self.lib.view.back = !self.lib.trail.is_empty();
        self.lib.view.crumb = if self.lib.trail.is_empty() { String::new() } else { match &page { Page::List { title, .. } => title.clone(), _ => String::new() } };
        self.lib.view.form = None;
        self.lib_tree();
        match &page {
            Page::Settings => {
                self.lib.view.search_shown = false;
                if let Some(b) = &self.backend {
                    b.config();
                    b.scan_status();
                }
                self.lib_form();
            }
            Page::Search { sid, .. } => {
                self.lib.view.search_shown = true;
                self.lib.view.search.placeholder = match sid.as_str() {
                    "radio" => "Type a station or genre, or paste a stream address".into(),
                    "podcasts" => "Type a podcast name, or paste a feed address".into(),
                    "soundcloud" => "Paste a SoundCloud link (track, playlist or artist), or search".into(),
                    "spotify" => "Search Spotify: songs, artists, your playlists".into(),
                    "skins" => "Search the Skin Museum: a band, a game, a colour, a name".into(),
                    _ => "Type and press Enter to search".into(),
                };
                self.lib.view.search.value.clear();
                self.lib.view.search.caret = 0;
                self.lib_focus(Some("search".into()));
                // Opened fresh: the recent searches, to run again.
                if from_tree {
                    let items = self.lib.recent.get(sid).into_iter().flatten().map(|q| Item { kind: RECENT_KIND.into(), id: q.clone(), title: q.clone(), info: "Search again".into(), ..Default::default() }).collect();
                    self.lib.cache.insert(page.key(), Listing::of(items));
                }
                if self.lib.cache.contains_key(&page.key()) {
                    self.lib_show();
                } else {
                    self.lib.items.clear();
                    self.lib.levels.clear();
                    self.lib.hint = None;
                    self.lib_rows();
                }
            }
            Page::List { sid, node, .. } => {
                self.lib.view.search_shown = true;
                self.lib.view.search.placeholder = "Search".into();
                self.lib.view.search.value.clear();
                self.lib.view.search.caret = 0;
                if self.lib.cache.contains_key(&page.key()) {
                    self.lib_show();
                } else {
                    self.lib.items.clear();
                    self.lib.levels.clear();
                    self.lib.hint = None;
                    self.lib_rows();
                    self.lib.view.status = "Loading…".into();
                }
                // Always refresh; the cached copy shows meanwhile.
                self.lib.loading = true;
                if let Some(b) = &self.backend {
                    b.browse(page.key(), sid.clone(), node.clone());
                }
            }
        }
        self.mark(Kind::Library);
    }

    fn lib_page_sid(&self) -> Option<String> {
        match &self.lib.page {
            Page::List { sid, .. } | Page::Search { sid, .. } => Some(sid.clone()),
            Page::Settings => None,
        }
    }

    /// `key` is a listing opened from a folder on the page that's showing.
    fn lib_is_open_child(&self, key: &str) -> bool {
        let Some(sid) = self.lib_page_sid() else { return false };
        if key == self.lib.page.key() {
            return false;
        }
        let Some(node) = key.strip_prefix(&format!("{sid}\u{1f}")) else { return false };
        self.lib.expanded.get(&self.lib.page.key()).is_some_and(|s| s.contains(node))
    }

    /// A cached descendant matches the filter, so a collapsed folder stays listed.
    fn lib_descendant_hit(&self, sid: &str, item: &Item, filter: &str, seen: &mut BTreeSet<String>) -> bool {
        if sid.is_empty() || item.kind != "folder" || !seen.insert(item.id.clone()) {
            return false;
        }
        let Some(children) = self.lib.cache.get(&list_key(sid, &item.id)).map(|l| l.items.clone()) else { return false };
        children.iter().any(|c| (!is_filler(c) && item_text_has(c, filter)) || self.lib_descendant_hit(sid, c, filter, seen))
    }

    /// One sibling list, then each expanded folder's children at depth + 1.
    fn lib_append(&self, walk: &mut Walk<'_>, items: &[Item], depth: u8, out_items: &mut Vec<Item>, out_levels: &mut Vec<u8>) {
        let mut siblings = items.to_vec();
        if let Some((col, asc)) = walk.sort {
            siblings.sort_by_key(|i| sort_key(i, col));
            if !asc {
                siblings.reverse();
            }
        }
        for item in siblings {
            if is_filler(&item) {
                if walk.filter.is_empty() {
                    out_items.push(item);
                    out_levels.push(depth);
                }
                continue;
            }
            let id = item.id.clone();
            let expanded = !walk.sid.is_empty() && item.kind == "folder" && walk.open.contains(&id);
            let mut kids_i = Vec::new();
            let mut kids_l = Vec::new();
            if expanded && walk.stack.insert(id.clone()) {
                let key = list_key(walk.sid, &id);
                if let Some(children) = self.lib.cache.get(&key).map(|l| l.items.clone()) {
                    self.lib_append(walk, &children, depth.saturating_add(1), &mut kids_i, &mut kids_l);
                } else if let Some(err) = self.lib.failed.get(&key).cloned() {
                    if walk.filter.is_empty() {
                        kids_i.push(Item { kind: "note".into(), title: err, ..Default::default() });
                        kids_l.push(depth.saturating_add(1));
                    }
                } else {
                    if walk.filter.is_empty() {
                        kids_i.push(Item { kind: "loading".into(), title: t("Loading…").into(), ..Default::default() });
                        kids_l.push(depth.saturating_add(1));
                    }
                    if !self.lib.inflight.contains(&key) {
                        walk.fetches.push((key, id.clone()));
                    }
                }
                walk.stack.remove(&id);
            }
            let matched = walk.filter.is_empty() || item_text_has(&item, walk.filter);
            let hidden_hit = !matched
                && item.kind == "folder"
                && !expanded
                && self.lib_descendant_hit(walk.sid, &item, walk.filter, &mut BTreeSet::new());
            if matched || hidden_hit || !kids_i.is_empty() {
                out_items.push(item);
                out_levels.push(depth);
                out_items.append(&mut kids_i);
                out_levels.append(&mut kids_l);
            }
        }
    }

    /// Visible rows for the page. `apply_sort` uses `columns` for the header the user clicked.
    fn lib_flat(&self, top: &[Item], columns: &[(String, f32)], apply_sort: bool) -> (Vec<Item>, Vec<u8>, Vec<(String, String)>) {
        let sid = self.lib_page_sid().unwrap_or_default();
        let open = self.lib.expanded.get(&self.lib.page.key()).cloned().unwrap_or_default();
        let filter = self.lib.filter.to_lowercase();
        let sort = if apply_sort { self.lib.sort.and_then(|(c, asc)| columns.get(c).map(|(n, _)| (n.as_str(), asc))) } else { None };
        let mut items = Vec::new();
        let mut levels = Vec::new();
        let mut fetches = Vec::new();
        let mut stack = BTreeSet::new();
        let mut walk = Walk { sid: &sid, open: &open, filter: &filter, sort, stack: &mut stack, fetches: &mut fetches };
        self.lib_append(&mut walk, top, 0, &mut items, &mut levels);
        (items, levels, fetches)
    }

    pub fn lib_show(&mut self) {
        let Some((actions, hint, top_items)) = self.lib.cache.get(&self.lib.page.key()).map(|top| (top.actions.clone(), top.hint.clone(), top.items.clone())) else { return };
        // Selection is a row index. Rows inserted above it would otherwise point it at something else.
        let kept = self.lib_kept_rows();
        self.lib.actions = actions;
        self.lib.hint = hint;
        // Columns follow whatever is actually on screen, which the sort key also needs.
        let (mut items, mut levels, mut fetches) = self.lib_flat(&top_items, &[], false);
        let columns = self.lib_columns(&items);
        if self.lib.sort.is_some() {
            (items, levels, fetches) = self.lib_flat(&top_items, &columns, true);
        }
        self.lib.items = items;
        self.lib.levels = levels;
        if let Some(sid) = self.lib_page_sid() {
            for (key, node) in fetches {
                if self.lib.inflight.insert(key.clone())
                    && let Some(b) = &self.backend
                {
                    b.browse(key, sid.clone(), node);
                }
            }
        }
        self.lib_rows();
        self.lib_restore_rows(kept);
    }

    /// Selected rows and the anchor, as item id plus depth, and whether the anchor was on screen.
    fn lib_kept_rows(&self) -> Kept {
        let selected = self.lib.view.table.selected.iter().copied().filter_map(|i| self.lib_mark(i)).collect();
        let anchor = self.lib.anchor.and_then(|i| self.lib_mark(i));
        let vis = self.lib.view.visible_rows();
        let top = self.lib.view.table.top;
        let visible = self.lib.anchor.is_some_and(|i| i >= top && i < top + vis);
        Kept { selected, anchor, visible }
    }

    fn lib_mark(&self, i: usize) -> Option<(String, u8)> {
        let depth = self.lib.levels.get(i).copied().unwrap_or(0);
        self.lib.items.get(i).map(|it| (it.id.clone(), depth))
    }

    fn lib_restore_rows(&mut self, kept: Kept) {
        let Kept { selected, anchor, visible } = kept;
        let find = |id: &str, depth: u8| {
            let same = |n: usize, it: &Item| it.id == id && self.lib.levels.get(n).copied().unwrap_or(0) == depth;
            self.lib
                .items
                .iter()
                .enumerate()
                .find(|(n, it)| same(*n, it))
                .or_else(|| self.lib.items.iter().enumerate().find(|(_, it)| it.id == id))
                .map(|(n, _)| n)
        };
        let selected: BTreeSet<usize> = selected.iter().filter_map(|(id, depth)| find(id, *depth)).collect();
        let anchor = anchor.and_then(|(id, depth)| find(&id, depth));
        self.lib.view.table.selected = selected;
        self.lib.anchor = anchor;
        if visible && let Some(a) = self.lib.anchor {
            let rows = self.lib.view.visible_rows();
            let max = self.lib.view.max_top();
            let t = &mut self.lib.view.table;
            if a < t.top {
                t.top = a;
            } else if a >= t.top + rows {
                t.top = a + 1 - rows;
            }
            t.top = t.top.min(max);
        }
    }

    /// Forget a failed browse so the next show asks again.
    fn lib_drop_failure(&mut self, key: &str) {
        if self.lib.failed.remove(key).is_some() {
            self.lib.inflight.remove(key);
        }
    }

    fn lib_listing(&mut self, key: String, result: Result<Listing, String>) {
        self.lib.loading = false;
        self.lib.inflight.remove(&key);
        let sid = listing_sid(&key).to_owned();
        let network = self.lib.sources.iter().any(|s| s.id == sid && s.group == "network");
        let mut offline_changed = false;
        match result {
            Ok(l) => {
                if network {
                    offline_changed = self.lib.offline.remove(&sid).is_some();
                }
                // A child folder's listing must not eat a play that is waiting on this page.
                let for_page = self.lib.page.key() == key;
                let play_now = for_page && self.pending_stream && !l.items.is_empty() && l.items.iter().all(|i| i.live || i.url.is_some());
                if for_page {
                    self.pending_stream = false;
                }
                let streamed = l.items.clone();
                self.lib.failed.remove(&key);
                self.lib.cache.insert(key.clone(), l);
                if self.lib.page.key() == key || self.lib_is_open_child(&key) {
                    self.lib_show();
                }
                if play_now {
                    self.lib_send(streamed, false);
                }
                if let Some((_, enqueue)) = self.lib.pending_play.take_if(|(k, _)| k == &key) {
                    let tracks = self.lib.cache.get(&key).map(|l| l.items.clone()).unwrap_or_default();
                    self.lib_send(tracks, enqueue);
                }
            }
            Err(e) => {
                if network {
                    offline_changed = self.lib.offline.get(&sid).map(String::as_str) != Some(e.as_str());
                    self.lib.offline.insert(sid, e.clone());
                }
                if self.lib.pending_play.as_ref().is_some_and(|(k, _)| k == &key) {
                    self.lib.pending_play = None;
                }
                if self.lib_is_open_child(&key) {
                    self.lib.failed.insert(key, e.clone());
                    self.lib_show();
                } else if self.lib.page.key() == key {
                    self.lib.items.clear();
                    self.lib.levels.clear();
                    self.lib.hint = Some(e.clone());
                    self.lib_rows();
                }
                self.last_error = Some(e.clone());
                self.lib.view.status = e;
            }
        }
        if offline_changed {
            self.lib_tree();
        }
    }

    fn lib_columns(&self, items: &[Item]) -> Vec<(String, f32)> {
        let items: Vec<&Item> = items.iter().filter(|i| !is_filler(i)).collect();
        let c = |l: &str, w: f32| (l.to_owned(), w);
        if !items.is_empty() && items.iter().all(|i| i.kind == museum::KIND) {
            return vec![c("Skin", 3.0), c("File", 2.0)];
        }
        if !items.is_empty() && items.iter().all(|i| !is_playable(i)) {
            vec![c("Name", 3.0), c("Artist", 2.0), c("Items", 0.7)]
        } else if !items.is_empty() && items.iter().all(|i| i.live) {
            vec![c("Station", 3.0), c("Genre", 2.0), c("Format", 1.1)]
        } else if items.iter().any(|i| !i.date.is_empty() || i.plays.is_some()) {
            // SoundCloud: when it went up and how popular it is, in place of the album.
            vec![c("Artist", 1.2), c("Title", 2.3), c("Posted", 1.3), c("Plays", 0.8), c("Likes", 0.75), c("Length", 0.85)]
        } else {
            vec![c("Artist", 2.0), c("Title", 3.0), c("Album", 2.0), c("Length", 0.9)]
        }
    }

    /// Turns the items into table rows, buttons and status.
    fn lib_rows(&mut self) {
        let columns = self.lib_columns(&self.lib.items);
        let length_is_last = columns.last().is_some_and(|(name, _)| name == "Length");
        let open = self.lib.expanded.get(&self.lib.page.key()).cloned().unwrap_or_default();
        let levels = self.lib.levels.clone();
        let rows: Vec<Row> = self
            .lib
            .items
            .iter()
            .enumerate()
            .map(|(n, i)| {
                let cells = columns
                    .iter()
                    .map(|(c, _)| match c.as_str() {
                        "Artist" | "Genre" => i.artist.clone(),
                        "Album" | "Format" | "File" => i.album.clone(),
                        // Track columns end in Length. A folder has no length; show how many it holds.
                        "Length" if i.kind == "folder" && length_is_last => i.count.map(|n| n.to_string()).unwrap_or_default(),
                        "Length" => i.duration.map(clock).unwrap_or_default(),
                        "Items" => i.count.map(|n| n.to_string()).unwrap_or_default(),
                        "Posted" => i.date.clone(),
                        "Plays" => i.plays.map(count).unwrap_or_default(),
                        "Likes" => i.likes.map(count).unwrap_or_default(),
                        _ => i.title.clone(),
                    })
                    .collect();
                Row {
                    cells,
                    folder: i.kind == "folder",
                    dim: is_filler(i) || is_preview(i) || i.info == gamp_core::soundcloud_web::PROTECTED,
                    tip: i.info.clone(),
                    depth: levels.get(n).copied().unwrap_or(0),
                    open: i.kind == "folder" && open.contains(&i.id),
                }
            })
            .collect();
        let t = &mut self.lib.view.table;
        t.columns = columns;
        t.rows = rows;
        let max_top = self.lib.view.max_top();
        let t = &mut self.lib.view.table;
        t.top = t.top.min(max_top);
        t.empty = match (&self.lib.page, self.lib.loading) {
            (_, true) if self.lib.items.is_empty() => "Loading…".into(),
            (Page::Search { .. }, _) if self.lib.items.is_empty() => self.lib.view.search.placeholder.clone() + ".",
            _ => self.lib.hint.clone().unwrap_or_default(),
        };
        let skins = matches!(&self.lib.page, Page::List { sid, .. } | Page::Search { sid, .. } if sid == "skins");
        let mut buttons: Vec<(String, String)> = if skins { [("skin:use", "Use skin"), ("skin:surprise", "Surprise me"), ("skin:default", "Original skin")] } else { [("play", "Play"), ("enqueue", "Enqueue"), ("playall", "Play all")] }
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        for a in &self.lib.actions {
            buttons.push((format!("action:{}", a.id), a.label.to_owned()));
        }
        let listed = matches!(&self.lib.page, Page::List { .. } | Page::Search { .. });
        if listed && self.lib.page.key() != FAVORITES_KEY && !self.lib.actions.iter().any(|a| a.id == "fav:add") {
            buttons.push(("action:fav:add".into(), "Add to Favorites".into()));
        }
        self.lib.view.buttons = buttons;
        let listed_rows: Vec<&Item> = self.lib.items.iter().filter(|i| !is_filler(i)).collect();
        let total: f64 = listed_rows.iter().filter_map(|i| i.duration).sum();
        let hours = if total > 3600.0 { format!(" · {:.1} h", total / 3600.0) } else { String::new() };
        let scanning = match (&self.lib.page, &self.lib.scan) {
            (Page::List { sid, .. }, Some(s)) if sid == "local" && s.scanning => format!(" · {}", tf("scanning… {} files", &[&s.scanned])),
            _ => String::new(),
        };
        self.lib.view.status = format!("{}{hours}{scanning}", tf("{} items", &[&listed_rows.len()]));
        self.mark(Kind::Library);
    }

    // ---- clicks -------------------------------------------------------------------

    pub fn lib_hot(&self, x: i32, y: i32) -> Option<Hot> {
        // Later entries are drawn on top; search them first.
        self.lib.hots.iter().rev().find(|(_, r)| lw::inside(*r, x, y)).map(|(h, _)| h.clone())
    }

    pub fn lib_press(&mut self, x: i32, y: i32) {
        let Some(h) = self.lib_hot(x, y) else {
            if y < 20 {
                self.start_drag(Kind::Library);
            }
            return;
        };
        match &h {
            Hot::Resize => {
                if let Some(w) = self.win(Kind::Library) {
                    self.resize = Some(crate::app::Resize { start: w.global_cursor(), size: self.lib.view.size(), ..Default::default() });
                }
                return;
            }
            Hot::TreeToggle(i) => {
                if let Some(Target::Group { key, .. }) = self.lib.targets.get(*i).cloned() {
                    self.lib_toggle_group(&key);
                }
                return;
            }
            Hot::Tree(i) => {
                match self.lib.targets.get(*i).cloned() {
                    Some(Target::Group { key, sid }) => self.lib_group_click(key, sid),
                    Some(Target::Open { sid, node, title }) => self.lib_open(Page::List { sid, node, title }, true),
                    Some(Target::Search { sid, title }) => self.lib_open(Page::Search { sid, title }, true),
                    Some(Target::Recent { sid, title, query }) => self.lib_search_again(sid, title, query),
                    Some(Target::Settings(section)) => self.lib_open_settings(section),
                    None => {}
                }
                return;
            }
            Hot::Search => {
                self.lib_focus(Some("search".into()));
                return;
            }
            Hot::Disclose(i) => {
                let i = *i;
                self.lib_focus(None);
                self.lib_toggle_folder(i);
                return;
            }
            Hot::Row(i) => {
                let i = *i;
                self.lib_focus(None);
                if self.double_click(Kind::Library, x, y) {
                    self.lib_activate(i);
                    return;
                }
                let sel = &mut self.lib.view.table.selected;
                if self.modifiers.shift_key() {
                    let a = self.lib.anchor.unwrap_or(i);
                    *sel = (a.min(i)..=a.max(i)).collect();
                } else if self.modifiers.control_key() || self.modifiers.super_key() {
                    if !sel.remove(&i) {
                        sel.insert(i);
                    }
                    self.lib.anchor = Some(i);
                } else {
                    *sel = BTreeSet::from([i]);
                    self.lib.anchor = Some(i);
                }
            }
            Hot::Header(c) => self.lib_click_header(*c),
            Hot::Scroll => {
                self.lib.view.table.top = lw::scroll_top_at(&self.lib.view, y);
            }
            Hot::Form(id) => {
                if self.lib.fields.contains_key(id) {
                    self.lib_focus(Some(id.clone()));
                    return;
                }
                self.lib_focus(None);
            }
            _ => {}
        }
        if h == Hot::TitleBar {
            self.start_drag(Kind::Library);
            return;
        }
        self.lib.view.pressed = Some(h);
        self.mark(Kind::Library);
    }

    pub fn lib_drag(&mut self, _x: i32, y: i32) {
        if self.lib.view.pressed == Some(Hot::Scroll) {
            self.lib.view.table.top = lw::scroll_top_at(&self.lib.view, y);
            self.mark(Kind::Library);
        }
    }

    pub fn lib_release(&mut self) {
        let Some(pressed) = self.lib.view.pressed.take() else { return };
        self.mark(Kind::Library);
        let (x, y) = self.win(Kind::Library).map(|w| w.cursor).unwrap_or((0, 0));
        if self.lib_hot(x, y) != Some(pressed.clone()) {
            return;
        }
        match pressed {
            Hot::Close => self.toggle_window(Kind::Library),
            Hot::Back => {
                if let Some(prev) = self.lib.trail.pop() {
                    self.lib_open(prev, false);
                }
            }
            Hot::Button(id) => self.lib_button(&id),
            Hot::Form(id) => self.lib_form_action(&id),
            _ => {}
        }
    }

    pub fn lib_wheel(&mut self, lines: f64) {
        if self.lib.view.form.is_some() {
            let max = lw::max_form_scroll(&self.skin, &self.lib.view);
            self.lib.view.form_scroll = (self.lib.view.form_scroll - (lines * 30.0) as i32).clamp(0, max);
        } else {
            let (x, _) = self.win(Kind::Library).map(|w| w.cursor).unwrap_or((0, 0));
            if x < 11 + 152 {
                let top = self.lib.view.tree_top as isize - (lines * 2.0) as isize;
                let max = self.lib.view.tree.len().saturating_sub(self.lib.view.tree_rows());
                self.lib.view.tree_top = top.clamp(0, max as isize) as usize;
            } else {
                let top = self.lib.view.table.top as isize - (lines * 3.0).round() as isize;
                self.lib.view.table.top = top.clamp(0, self.lib.view.max_top() as isize) as usize;
            }
        }
        self.mark(Kind::Library);
    }

    /// Opens or closes a folder in place. The row stays put; its children come and go under it.
    fn lib_toggle_folder(&mut self, i: usize) {
        let Some(id) = self.lib.items.get(i).filter(|it| it.kind == "folder").map(|it| it.id.clone()) else { return };
        let key = self.lib.page.key();
        let opening = {
            let set = self.lib.expanded.entry(key).or_default();
            // Nested ids stay in the set, so closing and opening restores them.
            if set.remove(&id) {
                false
            } else {
                set.insert(id.clone());
                true
            }
        };
        if opening && let Some(sid) = self.lib_page_sid() {
            self.lib_drop_failure(&list_key(&sid, &id));
        }
        self.lib.view.table.selected = BTreeSet::from([i]);
        self.lib.anchor = Some(i);
        self.lib_show();
    }

    fn lib_folder_open(&self, item: &Item) -> bool {
        item.kind == "folder" && self.lib.expanded.get(&self.lib.page.key()).is_some_and(|s| s.contains(&item.id))
    }

    /// Left/right in the table: open, close, or step out to the parent row.
    fn lib_side(&mut self, right: bool) {
        let Some(i) = self.lib.view.table.selected.iter().next().copied() else { return };
        let Some(item) = self.lib.items.get(i).cloned() else { return };
        let open = self.lib_folder_open(&item);
        if right {
            if item.kind == "folder" && !open {
                self.lib_toggle_folder(i);
            }
            return;
        }
        if open {
            self.lib_toggle_folder(i);
            return;
        }
        let depth = self.lib.levels.get(i).copied().unwrap_or(0);
        let Some(p) = (0..i).rev().find(|&j| self.lib.levels.get(j).copied().unwrap_or(0) < depth) else { return };
        self.lib.view.table.selected = BTreeSet::from([p]);
        self.lib.anchor = Some(p);
        let rows = self.lib.view.visible_rows();
        let t = &mut self.lib.view.table;
        if p < t.top {
            t.top = p;
        } else if p >= t.top + rows {
            t.top = p + 1 - rows;
        }
        self.mark(Kind::Library);
    }

    /// The old double-click: this folder becomes the page.
    pub fn lib_open_folder(&mut self) {
        let Some(item) = self.lib_selected().into_iter().find(|i| i.kind == "folder") else { return };
        let Some(sid) = self.lib_page_sid() else { return };
        self.lib.trail.push(self.lib.page.clone());
        self.lib_open(Page::List { sid, node: item.id, title: item.title }, false);
    }

    /// Plays the tracks in one folder's own listing. Sub-folders are not walked.
    fn lib_play_folder(&mut self, folder: Item, enqueue: bool) {
        let Some(sid) = self.lib_page_sid() else {
            self.lib.view.status = "Open it to play what's inside.".into();
            return;
        };
        let key = list_key(&sid, &folder.id);
        if let Some(tracks) = self.lib.cache.get(&key).map(|l| l.items.iter().filter(|i| is_playable(i)).cloned().collect::<Vec<_>>()) {
            if tracks.is_empty() {
                self.lib.view.status = "Open it to play what's inside.".into();
            } else {
                self.lib_send(tracks, enqueue);
            }
            return;
        }
        self.lib.failed.remove(&key);
        self.lib.pending_play = Some((key.clone(), enqueue));
        if self.lib.inflight.insert(key.clone())
            && let Some(b) = &self.backend
        {
            b.browse(key, sid, folder.id);
        }
        self.lib_show();
        self.lib.view.status = "Loading…".into();
    }

    /// Double-click or Enter: expand a folder, or play from that track.
    fn lib_activate(&mut self, i: usize) {
        let Some(item) = self.lib.items.get(i).cloned() else { return };
        if item.kind == museum::KIND {
            return self.skin_fetch(Some(item));
        }
        if item.kind == RECENT_KIND {
            if let Page::Search { sid, title } = self.lib.page.clone() {
                self.lib_search_again(sid, title, item.title);
            }
            return;
        }
        if is_filler(&item) {
            return;
        }
        if item.kind == "folder" {
            self.lib_toggle_folder(i);
            return;
        }
        if !is_playable(&item) {
            let sid = match &self.lib.page {
                Page::List { sid, .. } | Page::Search { sid, .. } => sid.clone(),
                Page::Settings => return,
            };
            self.lib.trail.push(self.lib.page.clone());
            self.lib_open(Page::List { sid, node: item.id.clone(), title: item.title.clone() }, false);
            return;
        }
        // What's selected plays (the one clicked, usually), not the whole list.
        let sel = self.lib_selected();
        let tracks = if self.lib.view.table.selected.contains(&i) && !item.live { sel } else { vec![item] };
        self.lib_send(tracks, false);
    }

    fn lib_selected(&self) -> Vec<Item> {
        self.lib.view.table.selected.iter().filter_map(|i| self.lib.items.get(*i).cloned()).collect()
    }

    /// Puts tracks in the playlist (replacing it, or at the end).
    pub fn lib_send(&mut self, items: Vec<Item>, enqueue: bool) {
        let entries: Vec<Entry> = items
            .into_iter()
            .filter(is_playable)
            .filter(|i| i.info != gamp_core::soundcloud_web::PROTECTED)
            .filter_map(|i| {
                // A loose file favorited from the playlist.
                let Some(url) = i.url.clone() else { return i.id.strip_prefix("file\u{1f}").map(|p| Entry::read(std::path::Path::new(p))) };
                // A station shows by its name alone, as SHOUTcast did.
                let artist = if i.live { String::new() } else { i.artist.clone() };
                // A SoundCloud preview stops after 30 seconds: say so where it plays.
                let (title, secs) = if is_preview(&i) { (tf("{} (preview)", &[&i.title]), Some(30.0)) } else { (i.title.clone(), i.duration) };
                Some(Entry::remote(url, title, artist, secs, i.live))
            })
            .take(5000)
            .collect();
        if entries.is_empty() {
            self.lib.view.status = "Open it to play what's inside.".into();
            return;
        }
        if enqueue && !self.playlist.entries.is_empty() {
            self.playlist.append(entries);
            self.sync_playlist();
            self.queue_after(self.playlist.current);
        } else if enqueue {
            // Added to an empty list: ready, not playing.
            self.playlist.set(entries);
            self.sync_playlist();
            self.update_title();
        } else {
            self.playlist.set(entries);
            self.sync_playlist();
            self.load_current(true);
        }
    }

    fn lib_toggle_group(&mut self, key: &str) {
        let set = if group_starts_closed(key) { &mut self.lib.opened } else { &mut self.lib.collapsed };
        if !set.remove(key) {
            set.insert(key.to_owned());
        }
        self.lib_tree();
    }

    /// Settings, scrolled to a heading when one is named.
    fn lib_open_settings(&mut self, section: Option<&'static str>) {
        self.lib_open(Page::Settings, true);
        self.lib.view.form_scroll = section.and_then(|p| lw::heading_scroll(&self.skin, &self.lib.view, p)).unwrap_or(0);
    }

    /// Network & Cloud. A later phase opens the hub page from here.
    pub fn lib_open_network_hub(&mut self) {
        self.lib_open_settings(Some("Media servers"));
    }

    /// The group's name opens its first root when it has one, and the group with it.
    fn lib_group_click(&mut self, key: String, sid: Option<String>) {
        if key == "network" {
            self.lib_open_network_hub();
            return;
        }
        if key.starts_with("src:") {
            let sid = sid.unwrap_or_else(|| key.trim_start_matches("src:").to_owned());
            if let Some(s) = self.lib.sources.iter().find(|s| s.id == sid).cloned() {
                if let Some((node, title)) = s.roots.first().cloned() {
                    self.lib.opened.insert(key);
                    self.lib_open(Page::List { sid: s.id, node, title: format!("{} › {title}", s.name) }, true);
                    return;
                }
            }
        }
        self.lib_toggle_group(&key);
    }

    fn lib_open_first_root(&mut self, sid: &str) {
        self.lib_group_click(format!("src:{sid}"), Some(sid.to_owned()));
    }

    fn lib_refresh_source(&mut self, sid: &str) {
        let prefix = format!("{sid}\u{1f}");
        self.lib.cache.retain(|k, _| !k.starts_with(&prefix));
        self.lib.failed.retain(|k, _| !k.starts_with(&prefix));
        self.lib.inflight.retain(|k| !k.starts_with(&prefix));
        self.lib.offline.remove(sid);
        if self.lib_page_sid().as_deref() == Some(sid) {
            let page = self.lib.page.clone();
            self.lib_open(page, false);
        } else {
            self.lib_tree();
        }
    }

    fn lib_hide_source(&mut self, sid: &str) {
        let Some(b) = &self.backend else { return };
        let mut list: Vec<String> = self.lib.config["hiddenSources"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
        if !list.iter().any(|id| id == sid) {
            list.push(sid.to_owned());
        }
        b.post("/api/config", json!({ "hiddenSources": list }), true);
        b.refresh_sources();
    }

    /// What a `lib:tree:` menu id does. `Confirm` does not remove the account.
    pub fn lib_tree_action(&mut self, id: &str, el: Option<&winit::event_loop::ActiveEventLoop>) -> Option<Vec<gamp_ui::menu::Item>> {
        match tree_cmd(id)? {
            TreeCmd::Open(sid) => self.lib_open_first_root(&sid),
            TreeCmd::Refresh(sid) => self.lib_refresh_source(&sid),
            TreeCmd::Hide(sid) => self.lib_hide_source(&sid),
            TreeCmd::Remove(sid) => {
                if let Some(b) = &self.backend {
                    b.delete_account(sid);
                }
            }
            TreeCmd::Manage => self.lib_open_network_hub(),
            TreeCmd::Cancel => {}
            TreeCmd::Confirm(sid) => {
                let name = self.lib.sources.iter().find(|s| s.id == sid).map(|s| s.name.clone()).unwrap_or_else(|| sid.clone());
                let items = lib_remove_confirm(&name, &sid);
                if el.is_some() {
                    let at = self.lib.menu_at;
                    self.open_menu(items.clone(), at, el);
                }
                return Some(items);
            }
        }
        None
    }

    fn lib_tree_menu(&mut self) -> Option<Vec<gamp_ui::menu::Item>> {
        let (x, y, at) = {
            let w = self.win(Kind::Library)?;
            let at = w.global_cursor();
            (w.cursor.0, w.cursor.1, (at.0 as i32, at.1 as i32))
        };
        let i = match self.lib_hot(x, y)? {
            Hot::Tree(i) | Hot::TreeToggle(i) => i,
            _ => return None,
        };
        self.lib.menu_at = at;
        tree_row_menu(&self.lib.targets, &self.lib.sources, i)
    }

    /// Right-click on a track: select it (unless it's in the selection)
    /// and offer Play / Add to playlist. A tree row offers the source menu.
    pub fn lib_row_menu(&mut self) -> Option<Vec<gamp_ui::menu::Item>> {
        use gamp_ui::menu::Item as M;
        let (x, y) = self.win(Kind::Library)?.cursor;
        let Some(hot) = self.lib_hot(x, y) else { return None };
        let (Hot::Row(i) | Hot::Disclose(i)) = hot else {
            return self.lib_tree_menu();
        };
        if !self.lib.view.table.selected.contains(&i) {
            self.lib.view.table.selected = BTreeSet::from([i]);
            self.lib.anchor = Some(i);
            self.mark(Kind::Library);
        }
        let sel = self.lib_selected();
        if sel.iter().any(|i| i.kind == RECENT_KIND) {
            return None;
        }
        let fav = if self.lib.page.key() == FAVORITES_KEY { M::action("lib:unfav", "Remove from Favorites") } else { M::action("lib:fav", "Add to Favorites") };
        if sel.iter().any(|i| i.kind == museum::KIND) {
            return Some(vec![M::action("lib:skin", "Use skin"), M::action("skin:surprise", "Surprise me"), M::action("skin:default", "Original skin"), fav]);
        }
        let single_folder = sel.len() == 1 && sel[0].kind == "folder";
        if !sel.iter().any(is_playable) && !sel.iter().any(|i| i.kind == "folder") {
            return Some(vec![fav]);
        }
        let mut menu = vec![M::action("lib:play", "Play"), M::action("lib:enqueue", "Add to playlist (Shift+Enter)")];
        if single_folder {
            menu.push(M::action("lib:open", "Open as its own list"));
        }
        menu.push(fav);
        if let Some(t) = self.lib.items.get(i).filter(|t| !t.artist_link.is_empty()) {
            let who = t.artist_link.rsplit('/').next().unwrap_or_default();
            menu.push(M::action("lib:artist", &tf("Latest from {}", &[&who])));
        }
        Some(menu)
    }

    pub fn lib_click_header(&mut self, c: usize) {
        // Dates and counts start with the newest or biggest.
        let up = !matches!(self.lib_columns(&self.lib.items).get(c).map(|c| c.0.as_str()), Some("Posted" | "Plays" | "Likes" | "Length" | "Items"));
        self.lib.sort = match self.lib.sort {
            Some((col, asc)) if col == c => Some((c, !asc)),
            _ => Some((c, up)),
        };
        self.lib_show();
    }

    /// Downloads a museum skin (or a random one) and puts it on.
    pub fn skin_fetch(&mut self, item: Option<Item>) {
        let Some(b) = &self.backend else { return };
        b.skin(item, self.data_dir.join(MUSEUM_DIR));
        self.say("FETCHING SKIN…");
    }

    /// The Skin Museum's best-loved skins, in the library window.
    pub fn skin_museum(&mut self) {
        if !self.wins.get(&Kind::Library).is_some_and(|w| w.visible) {
            self.toggle_window(Kind::Library);
        }
        self.lib_open(Page::List { sid: "skins".into(), node: "popular".into(), title: "Skin Museum › Most popular".into() }, true);
    }

    /// An uploader's tracks, newest first (what their SoundCloud link lists).
    pub fn lib_artist(&mut self) {
        let Some(link) = self.lib.view.table.selected.first().and_then(|i| self.lib.items.get(*i)).map(|t| t.artist_link.clone()) else { return };
        if link.is_empty() {
            return;
        }
        self.lib_open(Page::Search { sid: "soundcloud".into(), title: "SoundCloud › Search".into() }, true);
        self.lib.view.search.value = link;
        self.lib_submit("search");
        // Listed to pick from, not played.
        self.pending_stream = false;
    }

    pub fn lib_button(&mut self, id: &str) {
        match id {
            "play" | "enqueue" => {
                let enqueue = id == "enqueue";
                let sel = self.lib_selected();
                if sel.is_empty() {
                    self.lib.view.status = "Select some tracks first.".into();
                } else if sel.iter().any(is_playable) {
                    self.lib_send(sel, enqueue);
                } else if sel.iter().any(|i| i.kind == "folder") {
                    let folder = sel.into_iter().find(|i| i.kind == "folder").unwrap();
                    self.lib_play_folder(folder, enqueue);
                } else {
                    self.lib_send(sel, enqueue);
                }
            }
            "playall" => self.lib_send(self.lib.items.clone(), false),
            "skin:use" => match self.lib_selected().into_iter().next() {
                Some(i) => self.skin_fetch(Some(i)),
                None => self.lib.view.status = "Select a skin first.".into(),
            },
            "skin:surprise" | "skin:default" => self.menu_action(id, None),
            other => {
                if let Some(action) = other.strip_prefix("action:") {
                    let sel = self.lib_selected();
                    let sid = match &self.lib.page {
                        Page::List { sid, .. } | Page::Search { sid, .. } => sid.clone(),
                        Page::Settings => return,
                    };
                    if sel.is_empty() {
                        self.lib.view.status = "Select something first.".into();
                    } else if let Some(b) = &self.backend {
                        b.action(sid, action.to_owned(), sel);
                    }
                }
            }
        }
        self.mark(Kind::Library);
    }

    // ---- typing -------------------------------------------------------------------

    pub fn lib_focus(&mut self, id: Option<String>) {
        self.lib.focus = id.clone();
        self.lib.view.search.focused = id.as_deref() == Some("search");
        for (k, f) in self.lib.fields.iter_mut() {
            f.focused = id.as_deref() == Some(k.as_str());
        }
        if self.lib.page == Page::Settings {
            self.lib_form();
        }
        self.mark(Kind::Library);
    }

    fn focused_field(&mut self) -> Option<&mut Field> {
        match self.lib.focus.as_deref() {
            Some("search") => Some(&mut self.lib.view.search),
            Some(id) => self.lib.fields.get_mut(id),
            None => None,
        }
    }

    /// A key in the library window; true if it was used for typing.
    pub fn lib_key(&mut self, key: &winit::keyboard::Key, text: Option<&str>) -> bool {
        use winit::keyboard::{Key, NamedKey};
        let cmd = self.modifiers.control_key() || self.modifiers.super_key();
        if self.lib.focus.is_none() {
            // Arrows and Enter in the table.
            match key {
                // Shift+Enter adds the selection to the playlist.
                Key::Named(NamedKey::Enter) if self.modifiers.shift_key() => {
                    self.lib_button("enqueue");
                    return true;
                }
                Key::Named(NamedKey::Enter) => {
                    if let Some(i) = self.lib.view.table.selected.iter().next().copied() {
                        self.lib_activate(i);
                    }
                    return true;
                }
                Key::Named(NamedKey::Backspace) if !self.lib.trail.is_empty() => {
                    let prev = self.lib.trail.pop().unwrap();
                    self.lib_open(prev, false);
                    return true;
                }
                Key::Named(NamedKey::ArrowRight) => {
                    self.lib_side(true);
                    return true;
                }
                Key::Named(NamedKey::ArrowLeft) => {
                    self.lib_side(false);
                    return true;
                }
                Key::Named(d @ (NamedKey::ArrowDown | NamedKey::ArrowUp)) => {
                    let n = self.lib.items.len();
                    if n == 0 {
                        return true;
                    }
                    let at = self.lib.anchor.unwrap_or(0) as isize + if *d == NamedKey::ArrowDown { 1 } else { -1 };
                    let i = at.clamp(0, n as isize - 1) as usize;
                    self.lib.view.table.selected = BTreeSet::from([i]);
                    self.lib.anchor = Some(i);
                    let rows = self.lib.view.visible_rows();
                    let t = &mut self.lib.view.table;
                    if i < t.top {
                        t.top = i;
                    } else if i >= t.top + rows {
                        t.top = i + 1 - rows;
                    }
                    self.mark(Kind::Library);
                    return true;
                }
                Key::Character(c) if cmd && c.as_str() == "a" => {
                    self.lib.view.table.selected = (0..self.lib.items.len()).collect();
                    self.mark(Kind::Library);
                    return true;
                }
                _ => return false,
            }
        }
        let focus = self.lib.focus.clone().unwrap_or_default();
        match key {
            Key::Named(NamedKey::Enter) => {
                self.lib_submit(&focus);
            }
            Key::Named(NamedKey::Escape) => self.lib_focus(None),
            Key::Named(NamedKey::Tab) => {
                // Next field in the form.
                let order = ["srv_name", "srv_url", "srv_user", "srv_pass", "srv_otp"];
                if let Some(i) = order.iter().position(|o| *o == focus) {
                    self.lib_focus(Some(order[(i + 1) % order.len()].into()));
                }
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(f) = self.focused_field() {
                    f.backspace();
                }
            }
            Key::Named(NamedKey::Delete) => {
                if let Some(f) = self.focused_field() {
                    f.delete();
                }
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if let Some(f) = self.focused_field() {
                    f.move_caret(-1);
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                if let Some(f) = self.focused_field() {
                    f.move_caret(1);
                }
            }
            Key::Named(NamedKey::Home) => {
                if let Some(f) = self.focused_field() {
                    f.caret = 0;
                }
            }
            Key::Named(NamedKey::End) => {
                if let Some(f) = self.focused_field() {
                    f.end();
                }
            }
            Key::Character(c) if cmd && c.as_str() == "v" => {
                if let Some(t) = crate::clipboard::get() {
                    if let Some(f) = self.focused_field() {
                        f.insert(t.lines().next().unwrap_or(""));
                    }
                }
            }
            Key::Character(c) if cmd && c.as_str() == "a" => {
                if let Some(f) = self.focused_field() {
                    f.end();
                }
            }
            _ => {
                if let Some(t) = text.filter(|t| !cmd && !t.chars().any(char::is_control)) {
                    if let Some(f) = self.focused_field() {
                        f.insert(t);
                    }
                }
            }
        }
        // Typing in a list's search box filters it as you go.
        if focus == "search" && matches!(self.lib.page, Page::List { .. }) {
            self.lib.filter = self.lib.view.search.value.clone();
            self.lib_show();
        }
        if self.lib.page == Page::Settings {
            self.lib_form();
        }
        self.mark(Kind::Library);
        true
    }

    /// A recent search, clicked: shown and run again.
    fn lib_search_again(&mut self, sid: String, title: String, query: String) {
        self.lib_open(Page::Search { sid, title }, true);
        self.lib.view.search.value = query;
        self.lib.view.search.end();
        self.lib_submit("search");
    }

    /// While it's busy (Loading…, Searching…, Connecting…, Scanning…), the
    /// dots count up, as the main window's CONNECTING does.
    pub(crate) fn lib_dots(&mut self) {
        let step = |t: &mut String| {
            let base = t.trim_end_matches(['.', '…']);
            let dots = t.len() - base.len();
            if !["Loading", "Searching", "Connecting", "Scanning"].contains(&base) {
                return false;
            }
            let n = if t.ends_with('…') { 0 } else { (dots + 1) % 4 };
            *t = format!("{base}{}", ".".repeat(n));
            true
        };
        let a = step(&mut self.lib.view.status);
        let b = step(&mut self.lib.view.table.empty);
        if a || b {
            self.mark(Kind::Library);
        }
    }

    pub(crate) fn lib_submit(&mut self, focus: &str) {
        match focus {
            "search" => {
                if let Page::Search { sid, .. } = self.lib.page.clone() {
                    let q = self.lib.view.search.value.trim().to_owned();
                    if q.is_empty() {
                        return;
                    }
                    self.lib.loading = true;
                    self.lib.view.status = "Searching…".into();
                    self.lib.cache.remove(&self.lib.page.key());
                    self.lib.items.clear();
                    self.lib.levels.clear();
                    self.lib_rows();
                    if let Some(b) = &self.backend {
                        b.search(self.lib.page.key(), sid.clone(), q.clone());
                    }
                    // A pasted stream address or SoundCloud link plays straight away.
                    let link = q.starts_with("http://") || q.starts_with("https://");
                    self.pending_stream = link && (sid == "radio" || gamp_core::soundcloud_web::is_link(&q));
                    let recent = self.lib.recent.entry(sid).or_default();
                    recent.retain(|r| !r.eq_ignore_ascii_case(&q));
                    recent.insert(0, q);
                    recent.truncate(RECENT);
                    self.lib_tree();
                }
            }
            id if id.starts_with("srv_") => self.lib_form_action("srv:add"),
            "folder_path" => self.lib_form_action("folder:path"),
            "share" => self.lib_form_action("share:connect"),
            "sc_profile" => self.lib_form_action("sc:save"),
            _ => {}
        }
    }

    // ---- settings -----------------------------------------------------------------

    fn field(&self, id: &str) -> W {
        W::Field(id.to_owned(), self.lib.fields.get(id).cloned().unwrap_or_default())
    }

    fn value(&self, id: &str) -> String {
        self.lib.fields.get(id).map(|f| f.value.trim().to_owned()).unwrap_or_default()
    }

    fn clear(&mut self, ids: &[&str]) {
        for id in ids {
            if let Some(f) = self.lib.fields.get_mut(*id) {
                f.value.clear();
                f.caret = 0;
            }
        }
    }

    pub fn lib_form(&mut self) {
        let c = self.lib.config.clone();
        let platform = c["platform"].as_str().unwrap_or(std::env::consts::OS).to_owned();
        let mut f = vec![W::Heading("Music folders and network drives".into())];
        f.push(W::Text(match platform.as_str() {
            "macos" => "Add any folder. Network drives work once mounted in Finder (Go › Connect to Server, or use Connect below); they appear under /Volumes.",
            "windows" => "Add any folder, a mapped drive, or a \\\\server\\share path.",
            _ => "Add any folder, including network shares mounted under /mnt or /media.",
        }
        .into()));
        let folders: Vec<String> = c["folders"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
        if folders.is_empty() {
            f.push(W::Text("No folders yet.".into()));
        }
        for (i, p) in folders.iter().enumerate() {
            f.push(W::Item { id: format!("folder:rm:{i}"), label: p.clone(), button: "Remove".into() });
        }
        let scan = match &self.lib.scan {
            Some(s) if s.scanning => tf("Scanning… {} files, {} tracks", &[&s.scanned, &s.found]),
            Some(s) => tf("{} tracks", &[&s.found]),
            None => String::new(),
        };
        f.push(W::Row(vec![W::button("folder:add", "Add folder…"), W::button("folder:rescan", "Rescan"), W::Text(scan)]));
        f.push(W::Row(vec![self.field("folder_path"), W::button("folder:path", "Add path")]));
        if matches!(platform.as_str(), "macos" | "windows" | "linux") {
            f.push(W::Row(vec![self.field("share"), W::button("share:connect", "Connect to share")]));
        }

        f.push(W::Heading("Media servers (at home or over the internet)".into()));
        f.push(W::Text("Plex, Jellyfin, Navidrome and other Subsonic servers, Synology Audio Station, WebDAV and Nextcloud. DLNA servers on your network appear by themselves.".into()));
        for s in self.lib.sources.iter().filter(|s| s.group == "network" && !s.id.starts_with("dlna-") && !s.id.starts_with("dropbox-") && !s.id.starts_with("cloud-")) {
            f.push(W::Item { id: format!("srv:rm:{}", s.id), label: s.name.clone(), button: "Remove".into() });
        }
        f.push(W::Row(vec![W::button("plex:signin", "Sign in with Plex"), W::Text("Adds every Plex server on your account.".into())]));
        let (kid, klabel, kph) = KINDS[self.lib.kind % KINDS.len()];
        if let Some(fl) = self.lib.fields.get_mut("srv_url") {
            fl.placeholder = kph.into();
        }
        if let Some(fl) = self.lib.fields.get_mut("srv_pass") {
            fl.placeholder = if kid == "plex" { "X-Plex-Token".into() } else { "Password".into() };
        }
        f.push(W::Row(vec![W::button("srv:kind", &tf("Type: {} ›", &[&t(klabel)])), self.field("srv_name")]));
        f.push(W::Row(vec![self.field("srv_url")]));
        if kid == "synology" {
            f.push(W::Row(vec![self.field("srv_user"), self.field("srv_pass"), self.field("srv_otp")]));
        } else if kid == "plex" {
            f.push(W::Row(vec![self.field("srv_pass")]));
        } else {
            f.push(W::Row(vec![self.field("srv_user"), self.field("srv_pass")]));
        }
        f.push(W::Row(vec![W::Check { id: "srv:insecure".into(), label: "Accept a self-signed certificate".into(), checked: self.lib.insecure }, W::button("srv:add", "Add server")]));
        f.push(W::Row(vec![W::button("dlna:search", "Search the network again")]));

        f.push(W::Heading("Dropbox, Google Drive and OneDrive".into()));
        // Accounts signed in through Dropbox's web API (older versions).
        for s in self.lib.sources.iter().filter(|s| s.id.starts_with("dropbox-")) {
            f.push(W::Item { id: format!("srv:rm:{}", s.id), label: s.name.clone(), button: "Disconnect".into() });
        }
        let cloud: Vec<String> = c["cloud"].as_array().map(|a| a.iter().filter_map(|v| v["name"].as_str().map(str::to_owned)).collect()).unwrap_or_default();
        f.push(W::Text("Install the Dropbox, Google Drive or OneDrive app and sign in: its folder shows up under Network & Cloud by itself. Songs download as you play them.".into()));
        f.push(W::Row(vec![
            W::button("cloud:get", "Get Dropbox"),
            W::button("cloud:look", "Look again"),
            W::Text(if cloud.is_empty() { "None on this computer yet".into() } else { tf("Found: {}", &[&cloud.join(", ")]) }),
        ]));

        f.push(W::Heading("Spotify (Premium)".into()));
        f.push(W::Text("Sign in once in your web browser. Your Liked Songs, playlists and search then show here, and songs play through Gamp's equalizer and visualizers.".into()));
        let sp = c["spotify"]["connected"] == true;
        f.push(W::Row(vec![
            if sp { W::button("sp:disconnect", "Disconnect Spotify") } else { W::button("sp:connect", "Connect Spotify") },
            W::Text(if sp { "Connected" } else { "Not connected" }.into()),
        ]));

        f.push(W::Heading("SoundCloud".into()));
        f.push(W::Text("No account needed: search SoundCloud, or paste any track, playlist or artist link into Search. For your likes and playlists, add your profile's address (they need to be public).".into()));
        f.push(W::Row(vec![self.field("sc_profile"), W::button("sc:save", "Save")]));

        f.push(W::Heading("Internet radio, podcasts, the Internet Archive and Audius".into()));
        f.push(W::Text("Nothing to set up: they're under Internet. Paste any stream address into Internet Radio › Search to play it.".into()));
        f.push(W::Heading("Skins".into()));
        f.push(W::Text("Drop any classic Winamp .wsz skin on the player, or right-click › Skins.".into()));
        self.lib.view.form = Some(f);
        self.mark(Kind::Library);
    }

    fn lib_form_action(&mut self, id: &str) {
        let Some(b) = &self.backend else { return };
        let folders: Vec<String> = self.lib.config["folders"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
        match id {
            "folder:add" => self.ask(crate::dialog::Ask::MusicFolder),
            "folder:path" => {
                let p = self.value("folder_path");
                if !p.is_empty() {
                    let mut f = folders.clone();
                    f.push(p);
                    b.post("/api/config", json!({ "folders": f }), true);
                    b.post("/api/library/rescan", json!({}), false);
                    self.clear(&["folder_path"]);
                }
            }
            "folder:rescan" => {
                b.rescan();
                self.lib.view.status = "Scanning…".into();
            }
            "share:connect" => {
                let url = self.value("share");
                b.post("/api/mount", json!({ "url": url }), false);
                b.note(if cfg!(target_os = "macos") { "Finder is connecting. Then Add folder… › /Volumes." } else { "Connecting. Then add the share's folder." });
            }
            "srv:kind" => self.lib.kind = (self.lib.kind + 1) % KINDS.len(),
            "srv:insecure" => self.lib.insecure = !self.lib.insecure,
            "srv:add" => {
                let (kid, _, _) = KINDS[self.lib.kind % KINDS.len()];
                let body = json!({
                    "kind": kid,
                    "name": self.value("srv_name"),
                    "url": self.value("srv_url"),
                    "username": self.value("srv_user"),
                    "password": self.lib.fields.get("srv_pass").map(|f| f.value.clone()).unwrap_or_default(),
                    "otp": self.value("srv_otp"),
                    "insecure": self.lib.insecure,
                });
                b.post("/api/accounts", body, true);
                b.note("Connecting…");
                self.clear(&["srv_pass", "srv_otp"]);
            }
            "plex:signin" => open_in_browser(&b.url("/auth/plex/login")),
            "dlna:search" => {
                b.post("/api/sources/discover", json!({}), false);
                b.note("Looking for DLNA servers…");
                self.pending_refresh = Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
            }
            "cloud:get" => open_in_browser("https://www.dropbox.com/install"),
            "cloud:look" => {
                b.refresh_sources();
                b.config();
            }
            "sp:connect" => open_in_browser(&b.url("/auth/spotify/login")),
            "sp:disconnect" => b.post("/api/spotify/logout", json!({}), true),
            "sc:save" => b.post("/api/config", json!({ "soundcloudProfile": self.value("sc_profile") }), true),
            other => {
                if let Some(i) = other.strip_prefix("folder:rm:").and_then(|i| i.parse::<usize>().ok()) {
                    let f: Vec<String> = folders.iter().enumerate().filter(|(n, _)| *n != i).map(|(_, p)| p.clone()).collect();
                    b.post("/api/config", json!({ "folders": f }), true);
                } else if let Some(sid) = other.strip_prefix("srv:rm:") {
                    b.delete_account(sid.to_owned());
                } else if let Some(text) = other.strip_prefix("copy:") {
                    crate::clipboard::set(text);
                    b.note("Copied");
                }
            }
        }
        self.lib_form();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn recent_searches_sit_under_search_and_run_again() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.lib.sources = vec![Source { id: "soundcloud".into(), name: "SoundCloud".into(), group: String::new(), roots: Vec::new(), search: true, needs_setup: false, hidden: false, removable: false }];
        app.lib_open(Page::Search { sid: "soundcloud".into(), title: "SoundCloud › Search".into() }, true);
        for q in ["deep house", "bluebird", "Deep House", "a", "b", "c", "d"] {
            app.lib.view.search.value = q.into();
            app.lib_submit("search");
        }
        // Newest first, no repeats (whatever the case), five at most.
        assert_eq!(app.lib.recent["soundcloud"], ["d", "c", "b", "a", "Deep House"]);
        let labels: Vec<&str> = app.lib.view.tree.iter().map(|r| r.label.as_str()).collect();
        let at = labels.iter().position(|l| *l == "SoundCloud").unwrap();
        assert_eq!(labels[at..at + 4], ["SoundCloud", "Search", "d", "c"]);
        let tree = &app.lib.view.tree;
        assert!(tree[at + 2].selected && !tree[at + 1].selected, "the search shown is the one lit");
        // Clicking one runs it again, and it comes back to the top.
        let row = labels.iter().position(|l| *l == "Deep House").unwrap();
        app.lib_open(Page::Settings, true);
        let Some(Target::Recent { sid, title, query }) = app.lib.targets.get(row).cloned() else { panic!("{:?}", app.lib.targets.get(row)) };
        app.lib_search_again(sid, title, query);
        assert_eq!(app.lib.recent["soundcloud"][0], "Deep House");
        assert_eq!(app.lib.view.search.value, "Deep House");
    }

    #[test]
    fn search_page_lists_recent_searches_and_a_double_click_plays_one_track() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.lib.recent.insert("soundcloud".into(), vec!["bluebird".into()]);
        let page = Page::Search { sid: "soundcloud".into(), title: "SoundCloud › Search".into() };
        app.lib_open(page.clone(), true);
        assert_eq!(app.lib.items.len(), 1);
        assert_eq!(app.lib.items[0].kind, RECENT_KIND);
        app.lib_activate(0);
        assert_eq!(app.lib.view.search.value, "bluebird");
        // Results in: double-clicking one plays it alone.
        let track = |n: &str| Item { url: Some(format!("http://x/{n}")), ..Item::track(n, n) };
        app.lib.cache.insert(page.key(), Listing::of(vec![track("a"), track("b"), track("c")]));
        app.lib_show();
        app.lib.view.table.selected = BTreeSet::from([1]);
        app.lib_activate(1);
        assert_eq!(app.playlist.entries.len(), 1);
    }

    #[test]
    fn busy_words_count_their_dots() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.lib.view.status = "Searching…".into();
        app.lib.view.table.empty = "3 items".into();
        let mut seen = Vec::new();
        for _ in 0..5 {
            app.lib_dots();
            seen.push(app.lib.view.status.clone());
        }
        assert_eq!(seen, ["Searching", "Searching.", "Searching..", "Searching...", "Searching"]);
        assert_eq!(app.lib.view.table.empty, "3 items", "other words stay as they are");
    }

    #[test]
    fn counts_read_at_a_glance() {
        let c: Vec<String> = [7, 950, 4321, 9_999, 45_600, 120_000, 1_234_567, 3_000_000, 12_345_678].into_iter().map(super::count).collect();
        assert_eq!(c, ["7", "950", "4.3K", "10K", "46K", "120K", "1.2M", "3M", "12M"]);
    }

    use std::time::{Duration, Instant};

    use gamp_audio::OutputKind;
    use gamp_ui::main_window::PlayState;

    use super::*;
    use crate::backend::Backend;
    use winit::keyboard::{Key, NamedKey};

    fn track(id: &str, title: &str) -> Item {
        Item { url: Some(format!("/m/{id}")), ..Item::track(id, title) }
    }

    fn source(id: &str, name: &str, group: &str, roots: &[(&str, &str)], removable: bool) -> Source {
        Source {
            id: id.into(),
            name: name.into(),
            group: group.into(),
            roots: roots.iter().map(|(a, b)| ((*a).into(), (*b).into())).collect(),
            search: false,
            needs_setup: false,
            hidden: false,
            removable,
        }
    }

    fn titles(app: &App) -> Vec<String> {
        app.lib.items.iter().map(|i| i.title.clone()).collect()
    }

    fn menu_ids(items: Option<Vec<gamp_ui::menu::Item>>) -> Vec<String> {
        items.into_iter().flatten().filter_map(|i| match i { gamp_ui::menu::Item::Action { id, .. } => Some(id), _ => None }).collect()
    }

    #[test]
    fn expanding_a_folder_inserts_its_children_and_collapsing_removes_them() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        let page = Page::List { sid: "plex".into(), node: "root".into(), title: "Library".into() };
        app.lib.page = page.clone();
        app.lib.cache.insert(page.key(), Listing::of(vec![Item::folder("a", "A").items(Some(2)), Item::folder("b", "B")]));
        app.lib.cache.insert(list_key("plex", "a"), Listing::of(vec![track("t1", "One"), track("t2", "Two")]));
        app.lib_show();
        assert_eq!(titles(&app), ["A", "B"]);
        assert_eq!(app.lib.levels, vec![0, 0]);

        app.lib_activate(0);
        assert_eq!(app.lib.page, page);
        assert!(app.lib.trail.is_empty());
        assert_eq!(titles(&app), ["A", "One", "Two", "B"]);
        assert_eq!(app.lib.levels, vec![0, 1, 1, 0]);
        assert!(app.lib.view.table.rows[0].open);
        assert_eq!(app.lib.view.table.rows[1].depth, 1);

        // B has no listing yet: a dim placeholder, and no second request while that one is in flight.
        app.lib_activate(3);
        assert_eq!(app.lib.items[4].kind, "loading");
        assert_eq!(app.lib.items[4].title, "Loading…");
        assert!(app.lib.view.table.rows[4].dim);
        assert_eq!(app.lib.levels, vec![0, 1, 1, 0, 1]);
        assert!(app.lib.inflight.contains(&list_key("plex", "b")));
        let asked = app.lib.inflight.clone();
        app.lib_show();
        assert_eq!(app.lib.inflight, asked);

        app.lib_listing(list_key("plex", "b"), Err("nope".into()));
        assert_eq!(app.lib.items.last().unwrap().kind, "note");
        assert_eq!(app.lib.items.last().unwrap().title, "nope");
        assert!(app.lib.view.table.rows.last().unwrap().dim);
        assert_eq!(app.lib.view.status, "nope");
        assert_eq!(app.lib.items[0].title, "A", "a failed child browse leaves the page in place");

        app.lib_activate(0);
        assert_eq!(titles(&app), ["A", "B", "nope"]);
        assert_eq!(app.lib.levels, vec![0, 0, 1]);
        assert!(!app.lib.view.table.rows[0].open);
        assert!(app.lib.expanded.get(&page.key()).is_some_and(|s| s.contains("b") && !s.contains("a")));
    }

    #[test]
    fn arrow_keys_expand_collapse_and_select_the_parent() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        let page = Page::List { sid: "plex".into(), node: "root".into(), title: "Library".into() };
        app.lib.page = page;
        let mut items = vec![Item::folder("a", "Album")];
        items.extend((0..30).map(|i| track(&format!("t{i}"), &format!("T{i}"))));
        app.lib.cache.insert(app.lib.page.key(), Listing::of(items));
        app.lib.cache.insert(list_key("plex", "a"), Listing::of(vec![track("in", "Inside")]));
        app.lib_show();
        app.lib.view.table.selected = BTreeSet::from([0]);
        app.lib.anchor = Some(0);

        assert!(app.lib_key(&Key::Named(NamedKey::ArrowRight), None));
        assert_eq!(app.lib.items[1].title, "Inside");
        assert_eq!(app.lib.levels[1], 1);

        app.lib.view.table.selected = BTreeSet::from([1]);
        app.lib.anchor = Some(1);
        app.lib.view.table.top = 10;
        assert!(app.lib_key(&Key::Named(NamedKey::ArrowLeft), None));
        assert!(app.lib.view.table.selected.contains(&0));
        let top = app.lib.view.table.top;
        let vis = app.lib.view.visible_rows();
        assert_eq!(top, 0, "the parent is scrolled into view");
        assert!(vis > 0);
        assert_eq!(app.lib.items[1].title, "Inside", "left on a child selects the parent and leaves it open");

        assert!(app.lib_key(&Key::Named(NamedKey::ArrowLeft), None));
        assert_eq!(app.lib.items[1].title, "T0");
        assert_eq!(app.lib.levels, vec![0; app.lib.items.len()]);
    }

    #[test]
    fn play_on_a_folder_waits_for_its_listing_and_sends_one_level() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        let page = Page::List { sid: "plex".into(), node: "root".into(), title: "Library".into() };
        app.lib.page = page.clone();
        app.lib.cache.insert(page.key(), Listing::of(vec![Item::folder("alb", "Abbey Road")]));
        app.lib_show();
        app.lib.view.table.selected = BTreeSet::from([0]);
        app.lib_button("play");
        let key = list_key("plex", "alb");
        assert_eq!(app.lib.pending_play, Some((key.clone(), false)));
        assert_eq!(app.lib.view.status, "Loading…");
        assert!(app.playlist.entries.is_empty());

        app.lib.cache.insert(list_key("plex", "sub"), Listing::of(vec![track("deep", "Deep")]));
        app.lib_listing(key, Ok(Listing::of(vec![track("song", "Surface"), Item::folder("sub", "Sub")])));
        assert!(app.lib.pending_play.is_none());
        assert_eq!(app.playlist.entries.len(), 1);
        assert_eq!(app.playlist.entries[0].title, "Surface");
    }

    #[test]
    fn double_clicking_a_folder_does_not_drill_in() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        let page = Page::List { sid: "plex".into(), node: "root".into(), title: "Library".into() };
        app.lib.page = page.clone();
        app.lib.trail.push(Page::Settings);
        app.lib.cache.insert(page.key(), Listing::of(vec![Item::folder("alb", "Abbey Road")]));
        app.lib_show();
        let trail = app.lib.trail.clone();
        app.lib_activate(0);
        assert_eq!(app.lib.page, page);
        assert_eq!(app.lib.trail, trail);
        assert!(app.lib.expanded.get(&page.key()).is_some_and(|s| s.contains("alb")));
        assert_eq!(app.lib.items[1].kind, "loading");
    }

    #[test]
    fn a_listing_above_keeps_the_selection_on_the_same_row() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        let page = Page::List { sid: "plex".into(), node: "root".into(), title: "Library".into() };
        app.lib.page = page.clone();
        app.lib.cache.insert(page.key(), Listing::of(vec![Item::folder("a", "A"), track("c", "C")]));
        app.lib_show();
        let c = app.lib.items.iter().position(|i| i.id == "c").unwrap();
        app.lib.view.table.selected = BTreeSet::from([c]);
        app.lib.anchor = Some(c);
        app.lib.view.table.top = 0;

        // Open A above C, then its listing arrives and pushes C down.
        app.lib.expanded.entry(page.key()).or_default().insert("a".into());
        app.lib_show();
        let c = app.lib.items.iter().position(|i| i.id == "c").unwrap();
        assert_eq!(app.lib.view.table.selected, BTreeSet::from([c]));
        assert_eq!(app.lib.anchor, Some(c));

        let kids: Vec<Item> = (0..20).map(|i| track(&format!("k{i}"), &format!("K{i}"))).collect();
        app.lib_listing(list_key("plex", "a"), Ok(Listing::of(kids)));
        let c = app.lib.items.iter().position(|i| i.id == "c").unwrap();
        assert_eq!(app.lib.items[c].title, "C");
        assert_eq!(app.lib.view.table.selected, BTreeSet::from([c]));
        assert_eq!(app.lib.anchor, Some(c));
        let top = app.lib.view.table.top;
        let vis = app.lib.view.visible_rows();
        assert!(top <= c && c < top + vis, "the anchor stays on screen");
    }

    #[test]
    fn a_failed_folder_retries_when_opened_again() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        let page = Page::List { sid: "plex".into(), node: "root".into(), title: "Library".into() };
        app.lib.page = page.clone();
        app.lib.cache.insert(page.key(), Listing::of(vec![Item::folder("a", "A")]));
        app.lib_show();
        let key = list_key("plex", "a");
        app.lib.view.table.selected = BTreeSet::from([0]);
        app.lib.anchor = Some(0);
        assert!(app.lib_key(&Key::Named(NamedKey::ArrowRight), None));
        assert!(app.lib.inflight.contains(&key));
        app.lib_listing(key.clone(), Err("nope".into()));
        assert_eq!(app.lib.items[1].kind, "note");
        assert!(!app.lib.inflight.contains(&key));

        assert!(app.lib_key(&Key::Named(NamedKey::ArrowLeft), None));
        assert_eq!(app.lib.items.len(), 1);
        app.lib.view.table.selected = BTreeSet::from([0]);
        app.lib.anchor = Some(0);
        assert!(app.lib_key(&Key::Named(NamedKey::ArrowRight), None));
        assert!(app.lib.inflight.contains(&key), "opening the folder asks again");
        assert_eq!(app.lib.items[1].kind, "loading");
        assert_eq!(app.lib.items[1].title, "Loading…");
        assert!(!app.lib.failed.contains_key(&key));

        app.lib_listing(key.clone(), Err("again".into()));
        assert_eq!(app.lib.items[1].kind, "note");
        app.lib_open(page, false);
        assert!(app.lib.inflight.contains(&key), "opening the page asks again");
        assert!(app.lib.items.iter().any(|i| i.kind == "loading" && i.title == "Loading…"));
    }

    #[test]
    fn the_disclose_glyph_has_the_same_menu_as_the_row() {
        use crate::app::Kind;
        use gamp_ui::library_window::Hot;
        let mut app = App::new(OutputKind::Null, None).unwrap();
        let page = Page::List { sid: "plex".into(), node: "root".into(), title: "Library".into() };
        app.lib.page = page.clone();
        app.lib.cache.insert(page.key(), Listing::of(vec![Item::folder("a", "A")]));
        app.lib_show();
        app.lib.hots = vec![(Hot::Row(0), (0, 0, 200, 15)), (Hot::Disclose(0), (4, 0, 12, 15))];
        app.wins.get_mut(&Kind::Library).unwrap().cursor = (160, 4);
        let on_row = menu_ids(app.lib_row_menu());
        app.wins.get_mut(&Kind::Library).unwrap().cursor = (6, 4);
        let on_glyph = menu_ids(app.lib_row_menu());
        assert_eq!(on_glyph, on_row);
        assert!(on_glyph.iter().any(|id| id == "lib:open"));
    }

    #[test]
    fn server_groups_start_closed_and_the_sets_survive_a_session() {
        let dir = std::env::temp_dir().join(format!("gamp-lib-session-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.data_dir = dir.clone();
        app.lib.sources = vec![
            source("local", "Local Media", "local", &[("audio", "Audio")], false),
            source("nas", "Navidrome", "network", &[("albums", "Albums")], true),
        ];
        app.lib_tree();
        let labels: Vec<_> = app.lib.view.tree.iter().map(|r| r.label.as_str()).collect();
        assert!(labels.contains(&"Audio"), "a top-level group starts open");
        assert!(!labels.contains(&"Albums"), "a server starts closed");
        let nas = app.lib.view.tree.iter().position(|r| r.label == "Navidrome").unwrap();
        assert!(!app.lib.view.tree[nas].open);
        let page = app.lib.page.clone();
        app.lib.hots = vec![(Hot::TreeToggle(nas), (0, 0, 16, 15))];
        app.lib_press(4, 4);
        assert!(app.lib.opened.contains("src:nas"));
        assert!(!app.lib.collapsed.contains("src:nas"));
        assert_eq!(app.lib.page, page, "the glyph only opens the group");
        assert!(app.lib.view.tree.iter().any(|r| r.label == "Albums"));

        let net = app.lib.view.tree.iter().position(|r| r.label == "Network & Cloud").unwrap();
        app.lib.hots = vec![(Hot::TreeToggle(net), (0, 0, 16, 15))];
        app.lib_press(4, 4);
        assert!(app.lib.collapsed.contains("network"));
        assert!(!app.lib.opened.contains("network"));

        app.save_session();
        let mut again = App::new(OutputKind::Null, None).unwrap();
        again.data_dir = dir.clone();
        again.load_session().unwrap();
        assert!(again.lib.opened.contains("src:nas"));
        assert!(again.lib.collapsed.contains("network"));
        let old: crate::session::Session = serde_json::from_str(r#"{"searches":{"radio":["jazz"]}}"#).unwrap();
        assert!(old.opened.is_empty() && old.collapsed.is_empty());
        assert_eq!(old.searches["radio"], ["jazz"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clicking_a_server_name_opens_its_first_root_and_the_group() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.lib.sources = vec![
            source("nas", "Navidrome", "network", &[("albums", "Albums"), ("artists", "Artists")], true),
            source("blank", "Empty", "network", &[], false),
        ];
        app.lib_tree();
        let nas = app.lib.view.tree.iter().position(|r| r.label == "Navidrome").unwrap();
        app.lib.hots = vec![(Hot::Tree(nas), (0, 0, 140, 15))];
        app.lib_press(40, 4);
        match &app.lib.page {
            Page::List { sid, node, title } => {
                assert_eq!(sid, "nas");
                assert_eq!(node, "albums");
                assert_eq!(title, "Navidrome › Albums");
            }
            other => panic!("opened {other:?}"),
        }
        assert!(app.lib.opened.contains("src:nas"));
        assert!(app.lib.view.tree.iter().any(|r| r.label == "Albums"));

        let page = app.lib.page.clone();
        let blank = app.lib.view.tree.iter().position(|r| r.label == "Empty").unwrap();
        app.lib.hots = vec![(Hot::Tree(blank), (0, 0, 140, 15))];
        app.lib_press(40, 4);
        assert_eq!(app.lib.page, page, "a server with no roots still just toggles");
        assert!(app.lib.opened.contains("src:blank"));

        let net = app.lib.view.tree.iter().position(|r| r.label == "Network & Cloud").unwrap();
        app.lib.hots = vec![(Hot::Tree(net), (0, 0, 140, 15))];
        app.lib_press(40, 4);
        assert_eq!(app.lib.page, Page::Settings);
        assert!(!app.lib.collapsed.contains("network"));
    }

    #[test]
    fn dlna_servers_nest_under_one_group_and_hidden_sources_stay_out() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        let mut secret = source("dlna-3", "Secret", "network", &[("c:0", "Browse")], false);
        secret.hidden = true;
        let mut gone = source("old", "Gone", "network", &[("albums", "Albums")], true);
        gone.hidden = true;
        app.lib.sources = vec![
            source("nas", "Navidrome", "network", &[("albums", "Albums")], true),
            source("dlna-1", "Living Room", "network", &[("c:0", "Browse")], false),
            source("dlna-2", "Office", "network", &[("c:0", "Browse")], false),
            secret,
            gone,
        ];
        app.lib_tree();
        let labels: Vec<_> = app.lib.view.tree.iter().map(|r| r.label.as_str()).collect();
        assert!(labels.contains(&"On this network (2)"));
        assert!(labels.contains(&"Navidrome"));
        for absent in ["Living Room", "Office", "Secret", "Gone", "Browse", "Albums"] {
            assert!(!labels.contains(&absent), "{absent} is showing");
        }
        let dlna = app.lib.view.tree.iter().position(|r| r.label == "On this network (2)").unwrap();
        assert!(!app.lib.view.tree[dlna].open);
        assert_eq!(app.lib.view.tree[dlna].depth, 1);
        assert_eq!(app.lib.view.tree.iter().find(|r| r.label == "Navidrome").unwrap().depth, 1);

        app.lib.opened.insert("dlna".into());
        app.lib_tree();
        let labels: Vec<_> = app.lib.view.tree.iter().map(|r| r.label.as_str()).collect();
        let at = labels.iter().position(|l| *l == "On this network (2)").unwrap();
        assert_eq!(&labels[at + 1..at + 3], &["Living Room", "Office"]);
        assert_eq!(app.lib.view.tree[at + 1].depth, 2);
        assert!(!app.lib.view.tree[at + 1].open);
        assert!(!labels.contains(&"Browse"));
    }

    #[test]
    fn a_failed_network_listing_marks_the_server_offline_until_it_answers() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.lib.sources = vec![
            source("nas", "Navidrome", "network", &[("albums", "Albums")], true),
            source("radio", "Internet Radio", "internet", &[("top", "Top Stations")], false),
        ];
        app.lib_tree();
        app.lib_listing(list_key("nas", "albums"), Err("no route".into()));
        let row = app.lib.view.tree.iter().find(|r| r.label.starts_with("Navidrome")).unwrap();
        assert!(row.dim);
        assert!(row.label.ends_with(" · offline"));
        assert_eq!(app.lib.offline.get("nas").map(String::as_str), Some("no route"));

        app.lib_listing(list_key("radio", "top"), Err("nope".into()));
        assert!(!app.lib.offline.contains_key("radio"));
        assert!(!app.lib.view.tree.iter().find(|r| r.label == "Internet Radio").unwrap().dim);

        app.lib_listing(list_key("nas", "albums"), Ok(Listing::of(vec![])));
        let row = app.lib.view.tree.iter().find(|r| r.label == "Navidrome").unwrap();
        assert!(!row.dim);
        assert!(app.lib.offline.is_empty());
    }

    #[test]
    fn the_tree_menu_removes_only_accounts_and_hides_only_dlna() {
        use crate::app::Kind;
        use gamp_ui::library_window::Hot;
        let account = source("plex-1", "Plex", "network", &[("albums", "Albums")], true);
        let dlna = source("dlna-tv", "TV", "network", &[("c:0", "Browse")], false);
        let cloud = source("cloud-dropbox", "Dropbox", "network", &[("files", "Files")], true);
        let radio = source("radio", "Internet Radio", "internet", &[("top", "Top")], false);
        let ids = |s: &Source| menu_ids(Some(source_menu(s)));
        assert!(ids(&account).iter().any(|id| id == "lib:tree:remove:plex-1"));
        assert!(!ids(&account).iter().any(|id| id.starts_with("lib:tree:hide:")));
        assert!(ids(&dlna).iter().any(|id| id == "lib:tree:hide:dlna-tv"));
        assert!(!ids(&dlna).iter().any(|id| id.starts_with("lib:tree:remove")));
        assert!(!ids(&cloud).iter().any(|id| id.starts_with("lib:tree:remove")));
        assert!(!ids(&radio).iter().any(|id| id.starts_with("lib:tree:hide:") || id.starts_with("lib:tree:remove")));
        assert!(ids(&radio).iter().any(|id| id == "lib:tree:open:radio"));

        assert!(matches!(tree_cmd("lib:tree:remove:plex-1"), Some(TreeCmd::Confirm(sid)) if sid == "plex-1"));
        assert!(matches!(tree_cmd("lib:tree:remove!:plex-1"), Some(TreeCmd::Remove(_))));
        let confirm = menu_ids(Some(lib_remove_confirm("Plex", "plex-1")));
        assert_eq!(confirm, ["lib:tree:remove!:plex-1", "lib:tree:cancel"]);

        let mut app = App::new(OutputKind::Null, None).unwrap();
        app.lib.sources = vec![account, dlna];
        app.lib_tree();
        let yielded = app.lib_tree_action("lib:tree:remove:plex-1", None).expect("asks before removing");
        assert_eq!(menu_ids(Some(yielded)), ["lib:tree:remove!:plex-1", "lib:tree:cancel"]);
        app.menu_action("lib:tree:remove:plex-1", None);
        assert_eq!(app.lib.sources.len(), 2, "asking does not delete");
        assert!(app.backend.is_none());

        // The server starts closed, so open it to reach the leaf.
        app.lib.opened.insert("src:plex-1".into());
        app.lib_tree();
        let net = app.lib.targets.iter().position(|t| matches!(t, Target::Group { key, .. } if key == "network")).unwrap();
        let plex = app.lib.targets.iter().position(|t| matches!(t, Target::Group { key, .. } if key == "src:plex-1")).unwrap();
        let leaf = app.lib.targets.iter().position(|t| matches!(t, Target::Open { sid, node, .. } if sid == "plex-1" && node == "albums")).unwrap();
        let fav = app.lib.targets.iter().position(|t| matches!(t, Target::Open { sid, .. } if sid == "favorites")).unwrap();
        app.lib.hots = vec![
            (Hot::Tree(net), (0, 0, 100, 15)),
            (Hot::TreeToggle(net), (0, 0, 12, 15)),
            (Hot::Tree(plex), (0, 20, 100, 15)),
            (Hot::Tree(leaf), (0, 40, 100, 15)),
            (Hot::Tree(fav), (0, 60, 100, 15)),
        ];
        app.wins.get_mut(&Kind::Library).unwrap().cursor = (6, 4);
        assert_eq!(menu_ids(app.lib_row_menu()), ["lib:tree:manage"]);
        app.wins.get_mut(&Kind::Library).unwrap().cursor = (40, 24);
        assert!(menu_ids(app.lib_row_menu()).iter().any(|id| id == "lib:tree:remove:plex-1"));
        app.wins.get_mut(&Kind::Library).unwrap().cursor = (40, 44);
        assert!(menu_ids(app.lib_row_menu()).iter().any(|id| id == "lib:tree:open:plex-1"));
        app.wins.get_mut(&Kind::Library).unwrap().cursor = (40, 64);
        assert!(app.lib_row_menu().is_none());
    }

    #[test]
    fn fillers_are_left_out_of_the_count_and_a_folder_keeps_its_count() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        let page = Page::List { sid: "plex".into(), node: "root".into(), title: "Library".into() };
        app.lib.page = page.clone();
        app.lib.cache.insert(page.key(), Listing::of(vec![Item::folder("a", "A").items(Some(12)), track("song", "Song").secs(Some(7200.0))]));
        app.lib.expanded.entry(page.key()).or_default().insert("a".into());
        app.lib_show();
        assert_eq!(app.lib.items[1].kind, "loading");
        assert_eq!(app.lib.view.status, "2 items · 2.0 h");
        assert_eq!(app.lib.view.table.columns.last().unwrap().0, "Length");
        assert_eq!(app.lib.view.table.rows[0].cells.last().map(String::as_str), Some("12"));
    }

    #[test]
    fn a_child_listing_does_not_take_a_pending_stream() {
        let mut app = App::new(OutputKind::Null, None).unwrap();
        let page = Page::List { sid: "plex".into(), node: "root".into(), title: "Library".into() };
        app.lib.page = page.clone();
        app.lib.cache.insert(page.key(), Listing::of(vec![Item::folder("a", "A")]));
        app.lib.expanded.entry(page.key()).or_default().insert("a".into());
        app.lib_show();
        app.pending_stream = true;
        app.lib_listing(list_key("plex", "a"), Ok(Listing::of(vec![track("child", "Child")])));
        assert!(app.pending_stream);
        assert!(app.playlist.entries.is_empty());
        app.lib_listing(page.key(), Ok(Listing::of(vec![track("page", "PageSong")])));
        assert!(!app.pending_stream);
        assert_eq!(app.playlist.entries.len(), 1);
        assert_eq!(app.playlist.entries[0].title, "PageSong");
    }

    fn tone(path: &std::path::Path) {
        let rate = 22050u32;
        let frames = rate * 3;
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
            b.extend_from_slice(&(((i as f32 * 0.05).sin() * 8000.0) as i16).to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }

    fn wait(app: &mut App, secs: u64, mut done: impl FnMut(&App) -> bool) {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            app.tick();
            if done(app) {
                return;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        panic!("timed out: config {} scan {:?} status {:?} items {} state {:?} msg {:?}", app.lib.config, app.lib.scan.as_ref().map(|s| (s.scanning, s.found)), app.lib.view.status, app.lib.items.len(), app.main.state, app.main.message);
    }

    /// Online: the Skin Museum lists, a skin downloads and goes on, Surprise
    /// me picks another, Original goes back, Keep puts one in the Skins menu.
    #[test]
    #[ignore]
    fn skin_museum_round_trip() {
        let root = std::env::temp_dir().join(format!("gamp-skin-test-{}", std::process::id()));
        // SAFETY: this test runs alone (it's ignored by default).
        unsafe { std::env::set_var("WINAMP_PORT", "8794") };
        let backend = Backend::start(root.join("data"), None).unwrap();
        let mut app = App::new(OutputKind::Null, Some(backend)).unwrap();
        app.data_dir = root.join("data");
        app.lib_start();
        app.lib_open(Page::List { sid: "skins".into(), node: "popular".into(), title: "Skin Museum".into() }, true);
        wait(&mut app, 30, |a| a.lib.items.len() > 100);
        assert_eq!(app.lib.view.table.columns[0].0, "Skin");
        assert_eq!(app.lib.view.buttons[0].1, "Use skin");
        app.lib_activate(0);
        let museum = app.data_dir.join(MUSEUM_DIR);
        wait(&mut app, 60, |a| a.skin_path.as_ref().is_some_and(|p| p.starts_with(&museum)));
        let first = app.skin_path.clone().unwrap();
        app.menu_action("skin:surprise", None);
        wait(&mut app, 60, |a| a.skin_path.as_ref().is_some_and(|p| *p != first));
        app.menu_action("skin:default", None);
        assert!(app.skin_path.is_none());
        app.load_skin(Some(first.clone()));
        assert!(app.main_menu_has("Keep this skin"));
        app.menu_action("skin:keep", None);
        assert!(app.skin_path.as_ref().unwrap().starts_with(app.data_dir.join("skins")));
        assert!(!app.main_menu_has("Keep this skin"));
        // Searching works too.
        app.lib_open(Page::Search { sid: "skins".into(), title: "Skin Museum › Search".into() }, true);
        app.lib.view.search.value = "metal".into();
        app.lib_submit("search");
        wait(&mut app, 30, |a| !a.lib.items.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_plays_through_the_library_and_the_server() {
        let root = std::env::temp_dir().join(format!("gamp-lib-test-{}", std::process::id()));
        let music = root.join("Music");
        std::fs::create_dir_all(&music).unwrap();
        tone(&music.join("Library Tone.wav"));
        // SAFETY: only this test in the binary sets the port.
        unsafe { std::env::set_var("WINAMP_PORT", "8793") };
        let backend = Backend::start(root.join("data"), None).unwrap();
        let mut app = App::new(OutputKind::Null, Some(backend)).unwrap();
        app.lib_start();
        let b = app.backend.as_ref().unwrap();
        b.post("/api/config", json!({ "folders": [music.to_string_lossy()] }), true);
        b.post("/api/library/rescan", json!({}), false);
        // The scan finishes and the local list shows the track.
        wait(&mut app, 10, |a| a.lib.config["folders"].as_array().is_some_and(|f| !f.is_empty()));
        app.backend.as_ref().unwrap().rescan();
        wait(&mut app, 15, |a| a.lib.scan.as_ref().is_some_and(|s| !s.scanning && s.found == 1));
        app.lib_open(Page::List { sid: "local".into(), node: "audio".into(), title: "Local".into() }, true);
        wait(&mut app, 10, |a| !a.lib.items.is_empty());
        assert_eq!(app.lib.items[0].title, "Library Tone");
        assert_eq!(app.lib.view.table.rows.len(), 1);
        // The tree lists the built-in internet sources and Settings.
        assert!(app.lib.view.tree.iter().any(|r| r.label == "Internet Radio"));
        assert!(app.lib.view.tree.iter().any(|r| r.label == "Settings"));
        // Play it: through the local server, the HTTP reader and the engine.
        app.lib_send(app.lib.items.clone(), false);
        wait(&mut app, 10, |a| a.main.state == PlayState::Playing && a.main.elapsed > 0.3);
        assert_eq!(app.main.khz, Some(22));
        assert!(app.main.duration.is_some_and(|d| (d - 3.0).abs() < 0.1));
        // Seeking works over HTTP too.
        app.player.seek(2.0);
        wait(&mut app, 5, |a| a.main.elapsed >= 2.0);
        // The settings form builds from the server's config.
        app.lib_open(Page::Settings, true);
        let form = app.lib.view.form.clone().unwrap();
        assert!(form.iter().any(|w| matches!(w, W::Item { label, .. } if label.ends_with("Music"))));
        let _ = std::fs::remove_dir_all(&root);
    }
}

fn is_preview(i: &Item) -> bool {
    i.info.starts_with("SoundCloud only allows")
}

/// 950 → "950"; 4321 → "4.3K"; 45600 → "46K"; 1234567 → "1.2M".
fn count(n: u64) -> String {
    let short = |v: f64, unit: &str| if v < 10.0 { format!("{v:.1}{unit}").replace(".0", "") } else { format!("{v:.0}{unit}") };
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => short(n as f64 / 1e3, "K"),
        _ => short(n as f64 / 1e6, "M"),
    }
}

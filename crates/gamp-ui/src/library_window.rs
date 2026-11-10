//! The Media Library: a generic Winamp window (GEN.BMP frame and title
//! letters) with a source tree, a track table or a settings form, a
//! search box and buttons, coloured from the skin's PLEDIT colours.

use std::collections::BTreeSet;

use gamp_skin::Skin;

use crate::font;
use crate::lang::t as tr;
use crate::frame::{Frame, argb};

pub const MIN_WIDTH: u32 = 400;
pub const MIN_HEIGHT: u32 = 232;
pub(crate) const PX: f32 = 11.0;
pub(crate) const ROW: i32 = 15;
const TREE_W: i32 = 150;
/// Classic grey for buttons and headers.
const FACE: [u8; 3] = [196, 196, 208];
const FACE_HI: [u8; 3] = [255, 255, 255];
const FACE_LO: [u8; 3] = [88, 88, 104];
const INK: [u8; 3] = [0, 0, 0];
const INK_OFF: [u8; 3] = [120, 120, 132];

pub type R = (i32, i32, i32, i32);

pub fn inside(r: R, x: i32, y: i32) -> bool {
    x >= r.0 && y >= r.1 && x < r.0 + r.2 && y < r.1 + r.3
}

/// Something you can press, with what it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hot {
    TitleBar,
    Close,
    Resize,
    Tree(usize),
    Back,
    Search,
    Header(usize),
    Row(usize),
    /// The +/− on a folder row. Drawn after `Row`, so a reverse hit-test prefers it.
    Disclose(usize),
    Scroll,
    Button(String),
    Form(String),
}

#[derive(Clone, Debug, Default)]
pub struct Field {
    pub value: String,
    pub placeholder: String,
    pub password: bool,
    pub focused: bool,
    /// Characters before the caret.
    pub caret: usize,
}

impl Field {
    pub fn new(placeholder: &str) -> Field {
        Field { placeholder: placeholder.into(), ..Default::default() }
    }

    pub fn insert(&mut self, text: &str) {
        let at = self.value.char_indices().nth(self.caret).map(|(i, _)| i).unwrap_or(self.value.len());
        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
        self.value.insert_str(at, &clean);
        self.caret += clean.chars().count();
    }

    pub fn backspace(&mut self) {
        if self.caret == 0 {
            return;
        }
        let at = self.value.char_indices().nth(self.caret - 1).map(|(i, _)| i).unwrap_or(0);
        self.value.remove(at);
        self.caret -= 1;
    }

    pub fn delete(&mut self) {
        if let Some((at, _)) = self.value.char_indices().nth(self.caret) {
            self.value.remove(at);
        }
    }

    pub fn move_caret(&mut self, d: isize) {
        let n = self.value.chars().count() as isize;
        self.caret = (self.caret as isize + d).clamp(0, n) as usize;
    }

    pub fn end(&mut self) {
        self.caret = self.value.chars().count();
    }

    fn shown(&self) -> String {
        if self.password { "•".repeat(self.value.chars().count()) } else { self.value.clone() }
    }
}

/// A settings form, laid out top to bottom.
#[derive(Clone, Debug)]
pub enum W {
    Heading(String),
    Text(String),
    /// A text field; the app keeps the Field and passes a copy.
    Field(String, Field),
    Button { id: String, label: String, enabled: bool },
    Check { id: String, label: String, checked: bool },
    /// Several widgets side by side (fields share the room left).
    Row(Vec<W>),
    /// A line of text, an optional dim second line, and buttons at the right.
    Item { id: String, label: String, detail: String, buttons: Vec<(String, String)> },
    /// A wrapped line for an inline result. Errors use the current-track colour.
    Note { text: String, error: bool },
    Gap,
}

impl W {
    pub fn button(id: &str, label: &str) -> W {
        W::Button { id: id.into(), label: label.into(), enabled: true }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TreeRow {
    pub label: String,
    pub depth: u8,
    pub group: bool,
    pub open: bool,
    pub selected: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Row {
    pub cells: Vec<String>,
    pub folder: bool,
    pub dim: bool,
    pub tip: String,
    /// Tree indent, in steps of 12px.
    pub depth: u8,
    /// Folder is expanded. Closed folders draw `+`, open ones `−`.
    pub open: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Table {
    /// Label and share of the width.
    pub columns: Vec<(String, f32)>,
    pub rows: Vec<Row>,
    pub selected: BTreeSet<usize>,
    pub top: usize,
    /// Shown when there are no rows.
    pub empty: String,
}

#[derive(Clone, Debug, Default)]
pub struct View {
    pub width: u32,
    pub height: u32,
    pub focused: bool,
    pub tree: Vec<TreeRow>,
    pub tree_top: usize,
    /// The table, or a settings form.
    pub table: Table,
    pub form: Option<Vec<W>>,
    pub form_scroll: i32,
    pub back: bool,
    pub crumb: String,
    pub search: Field,
    pub search_shown: bool,
    /// Buttons under the table: (id, label).
    pub buttons: Vec<(String, String)>,
    pub status: String,
    pub pressed: Option<Hot>,
}

impl View {
    pub fn size(&self) -> (u32, u32) {
        (self.width.max(MIN_WIDTH), self.height.max(MIN_HEIGHT))
    }

    fn inner(&self) -> R {
        let (w, h) = self.size();
        (11, 20, w as i32 - 19, h as i32 - 34)
    }

    pub fn table_rect(&self) -> R {
        let (x, y, w, h) = self.inner();
        let tx = x + TREE_W + 4;
        (tx, y + 20, x + w - tx - 2, h - 20 - 22)
    }

    pub fn visible_rows(&self) -> usize {
        ((self.table_rect().3 - ROW) / ROW).max(1) as usize
    }

    pub fn max_top(&self) -> usize {
        self.table.rows.len().saturating_sub(self.visible_rows())
    }

    pub fn tree_rows(&self) -> usize {
        ((self.inner().3 - 4) / ROW).max(1) as usize
    }
}

// ---- the generic frame -----------------------------------------------------------

/// GEN.BMP's frame around a `w`×`h` window, with `title` in its letters.
pub fn gen_frame(frame: &mut Frame, skin: &Skin, w: i32, h: i32, title: &str, focused: bool) {
    let s = if focused { "_SELECTED" } else { "" };
    // The skin's title letters are A to Z: other languages' titles show without accents, or in English.
    let title = crate::lang::tm(title);
    let letters: Vec<_> = title.chars().map(|c| if c == ' ' { None } else { skin.gen_letter(crate::frame::plain(c), focused) }).collect();
    let title_w = 7 + letters.iter().map(|l| l.map(|(_, r)| r.width as i32).unwrap_or(5)).sum::<i32>();
    // Top: corner, fill, end, title, end, fill, corner.
    let tx = (w - title_w) / 2;
    frame.tile(skin, &format!("GEN_TOP_LEFT_RIGHT_FILL{s}"), 25, 0, w - 50, 20);
    frame.sprite(skin, &format!("GEN_TOP_LEFT{s}"), 0, 0);
    frame.sprite(skin, &format!("GEN_TOP_LEFT_END{s}"), tx - 25, 0);
    frame.tile(skin, &format!("GEN_TOP_CENTER_FILL{s}"), tx, 0, title_w, 20);
    frame.sprite(skin, &format!("GEN_TOP_RIGHT_END{s}"), tx + title_w, 0);
    frame.sprite(skin, &format!("GEN_TOP_RIGHT{s}"), w - 25, 0);
    let mut x = tx + 4;
    for l in &letters {
        match l {
            Some((img, r)) => {
                for yy in 0..r.height {
                    for xx in 0..r.width {
                        let p = img.get_pixel(r.x + xx, r.y + yy).0;
                        frame.put(x + xx as i32, 2 + 3 + yy as i32, argb([p[0], p[1], p[2]]));
                    }
                }
                x += r.width as i32;
            }
            None => x += 5,
        }
    }
    // Sides: tiles, with the bottom pieces at the bottom.
    frame.tile(skin, "GEN_MIDDLE_LEFT", 0, 20, 11, h - 20 - 14 - 24);
    frame.sprite(skin, "GEN_MIDDLE_LEFT_BOTTOM", 0, h - 14 - 24);
    frame.tile(skin, "GEN_MIDDLE_RIGHT", w - 8, 20, 8, h - 20 - 14 - 24);
    frame.sprite(skin, "GEN_MIDDLE_RIGHT_BOTTOM", w - 8, h - 14 - 24);
    // Bottom.
    frame.tile(skin, "GEN_BOTTOM_FILL", 125, h - 14, w - 250, 14);
    frame.sprite(skin, "GEN_BOTTOM_LEFT", 0, h - 14);
    frame.sprite(skin, "GEN_BOTTOM_RIGHT", w - 125, h - 14);
}

pub fn close_rect(w: i32) -> R {
    (w - 11, 3, 9, 9)
}

// ---- widgets ------------------------------------------------------------------

fn bevel(frame: &mut Frame, r: R, down: bool) {
    let (x, y, w, h) = r;
    frame.fill(x, y, w, h, argb(FACE));
    let (a, b) = if down { (FACE_LO, FACE_HI) } else { (FACE_HI, FACE_LO) };
    frame.fill(x, y, w, 1, argb(a));
    frame.fill(x, y, 1, h, argb(a));
    frame.fill(x, y + h - 1, w, 1, argb(b));
    frame.fill(x + w - 1, y, 1, h, argb(b));
}

pub(crate) fn button(frame: &mut Frame, r: R, label: &str, down: bool, enabled: bool) {
    let label = tr(label);
    bevel(frame, r, down);
    let tw = font::measure(label, PX, false);
    let o = down as i32;
    font::draw(frame, r.0 + (r.2 - tw) / 2 + o, r.1 + r.3 - 4 + o, label, PX, if enabled { INK } else { INK_OFF }, r.2, false);
}

/// `s` in the language in use, a part at a time ("Internet Radio › Search"),
/// keeping trailing dots ("Loading..").
fn shown(s: &str) -> String {
    let base = s.trim_end_matches(['.', '…']);
    let parts: Vec<&str> = base.split(" › ").map(tr).collect();
    format!("{}{}", parts.join(" › "), &s[base.len()..])
}

pub(crate) fn button_w(label: &str) -> i32 {
    font::measure(tr(label), PX, false) + 16
}

pub(crate) fn field(frame: &mut Frame, skin: &Skin, r: R, f: &Field) {
    let st = &skin.playlist;
    frame.fill(r.0, r.1, r.2, r.3, argb(st.normal_bg));
    let edge = argb(mix(st.normal, st.normal_bg, 0.6));
    frame.fill(r.0, r.1, r.2, 1, edge);
    frame.fill(r.0, r.1 + r.3 - 1, r.2, 1, edge);
    frame.fill(r.0, r.1, 1, r.3, edge);
    frame.fill(r.0 + r.2 - 1, r.1, 1, r.3, edge);
    let base = r.1 + r.3 - 4;
    if f.value.is_empty() && !f.focused {
        font::draw(frame, r.0 + 4, base, tr(&f.placeholder), PX, mix(st.normal, st.normal_bg, 0.45), r.2 - 8, false);
        return;
    }
    let shown = f.shown();
    // Keep the caret in view: scroll the text left if needed.
    let before: String = shown.chars().take(f.caret).collect();
    let caret_x = font::measure(&before, PX, false);
    let shift = (caret_x - (r.2 - 10)).max(0);
    let mut sub = Frame::new((r.2 - 6).max(1) as u32, r.3 as u32);
    sub.fill(0, 0, sub.width as i32, sub.height as i32, argb(st.normal_bg));
    font::draw(&mut sub, 2 - shift, base - r.1, &shown, PX, st.normal, 4000, false);
    if f.focused {
        sub.fill(2 - shift + caret_x, 2, 1, r.3 - 4, argb(st.current));
    }
    for y in 1..r.3 - 1 {
        for x in 0..sub.width as i32 {
            frame.put(r.0 + 3 + x, r.1 + y, sub.pixels[(y as u32 * sub.width + x as u32) as usize]);
        }
    }
}

pub(crate) fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    [f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2])]
}

/// Words wrapped to `width`.
pub fn wrap(text: &str, width: i32) -> Vec<String> {
    let fits = |t: &str| font::measure(t, PX, false) <= width;
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split(' ') {
            let candidate = if line.is_empty() { word.to_owned() } else { format!("{line} {word}") };
            if fits(&candidate) {
                line = candidate;
                continue;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            // A word wider than the line (a web address, a long path): it
            // breaks where it must, after a / ? & = or - when one is near.
            let mut rest = word;
            while !fits(rest) {
                let mut cut = rest.char_indices().map(|(i, c)| i + c.len_utf8()).take_while(|&i| fits(&rest[..i])).last().unwrap_or(rest.len().min(rest.chars().next().map_or(1, char::len_utf8)));
                if let Some(i) = rest[..cut].rfind(['/', '?', '&', '=', '-', '_', '.']).filter(|&i| i + 1 >= cut / 2) {
                    cut = i + 1;
                }
                lines.push(rest[..cut].to_owned());
                rest = &rest[cut..];
            }
            line = rest.to_owned();
        }
        lines.push(line);
    }
    lines
}

/// Lays out and draws a form inside `area`; returns its hot spots and its
/// full height (for scrolling).
fn form(frame: &mut Frame, skin: &Skin, area: R, items: &[W], scroll: i32, pressed: &Option<Hot>, hots: &mut Vec<(Hot, R)>) -> i32 {
    form_marked(frame, skin, area, items, scroll, pressed, hots, &mut Vec::new())
}

/// Lays out a form, noting where each heading starts (unscrolled).
fn form_marked(frame: &mut Frame, skin: &Skin, area: R, items: &[W], scroll: i32, pressed: &Option<Hot>, hots: &mut Vec<(Hot, R)>, marks: &mut Vec<(String, i32)>) -> i32 {
    let st = &skin.playlist;
    let (ax, ay, aw, ah) = area;
    let mut y = ay + 6 - scroll;
    let clip = |y: i32, h: i32| y + h > ay && y < ay + ah;
    for item in items {
        match item {
            W::Heading(t) => {
                marks.push((t.clone(), y + scroll - ay));
                y += 4;
                if clip(y, 16) {
                    font::draw(frame, ax + 6, y + 12, tr(t), 12.0, st.current, aw - 12, true);
                }
                y += 18;
            }
            W::Text(t) => {
                for line in wrap(tr(t), aw - 12) {
                    if clip(y, ROW) {
                        font::draw(frame, ax + 6, y + 11, &line, PX, mix(st.normal, st.normal_bg, 0.15), aw - 12, false);
                    }
                    y += 14;
                }
                y += 2;
            }
            W::Note { text, error } => {
                let col = if *error { st.current } else { mix(st.normal, st.normal_bg, 0.15) };
                for line in wrap(tr(text), aw - 12) {
                    if clip(y, ROW) {
                        font::draw(frame, ax + 6, y + 11, &line, PX, col, aw - 12, false);
                    }
                    y += 14;
                }
                y += 2;
            }
            W::Gap => y += 6,
            W::Row(children) => {
                y += row(frame, skin, (ax + 6, y, aw - 12), children, pressed, hots, |y, h| clip(y, h));
            }
            other => {
                y += row(frame, skin, (ax + 6, y, aw - 12), std::slice::from_ref(other), pressed, hots, |y, h| clip(y, h));
            }
        }
    }
    y + scroll - ay
}

/// Width of an item's buttons, including the gaps between them.
fn buttons_span(buttons: &[(String, String)]) -> i32 {
    if buttons.is_empty() {
        return 0;
    }
    buttons.iter().map(|(_, label)| button_w(label)).sum::<i32>() + 6 * (buttons.len() as i32 - 1)
}

fn row(frame: &mut Frame, skin: &Skin, at: (i32, i32, i32), items: &[W], pressed: &Option<Hot>, hots: &mut Vec<(Hot, R)>, visible: impl Fn(i32, i32) -> bool) -> i32 {
    let (x0, y, width) = at;
    let st = &skin.playlist;
    let h = 19;
    // A detail line makes the row one text line taller.
    let extra = if items.iter().any(|w| matches!(w, W::Item { detail, .. } if !detail.is_empty())) { 14 } else { 0 };
    let row_h = h + extra;
    // Fixed widths first; fields share what's left.
    let fixed: i32 = items
        .iter()
        .map(|w| match w {
            W::Button { label, .. } => button_w(label),
            W::Check { label, .. } => font::measure(tr(label), PX, false) + 20,
            W::Item { buttons, .. } => buttons_span(buttons),
            _ => 0,
        })
        .sum();
    let fields = items.iter().filter(|w| matches!(w, W::Field(..) | W::Item { .. })).count().max(1) as i32;
    let gaps = 6 * (items.len() as i32 - 1).max(0);
    let field_w = ((width - fixed - gaps) / fields).max(60);
    let mut x = x0;
    for w in items {
        match w {
            W::Field(id, f) => {
                let r = (x, y, field_w, h);
                if visible(y, h) {
                    field(frame, skin, r, f);
                    hots.push((Hot::Form(id.clone()), r));
                }
                x += field_w + 6;
            }
            W::Button { id, label, enabled } => {
                let r = (x, y, button_w(label), h);
                if visible(y, h) {
                    let down = *pressed == Some(Hot::Form(id.clone()));
                    button(frame, r, label, down, *enabled);
                    if *enabled {
                        hots.push((Hot::Form(id.clone()), r));
                    }
                }
                x += r.2 + 6;
            }
            W::Check { id, label, checked } => {
                let r = (x, y, font::measure(tr(label), PX, false) + 20, h);
                if visible(y, h) {
                    let b = (x, y + 4, 11, 11);
                    frame.fill(b.0, b.1, b.2, b.3, argb(st.normal_bg));
                    let edge = argb(mix(st.normal, st.normal_bg, 0.5));
                    frame.fill(b.0, b.1, 11, 1, edge);
                    frame.fill(b.0, b.1 + 10, 11, 1, edge);
                    frame.fill(b.0, b.1, 1, 11, edge);
                    frame.fill(b.0 + 10, b.1, 1, 11, edge);
                    if *checked {
                        frame.fill(b.0 + 3, b.1 + 3, 5, 5, argb(st.current));
                    }
                    font::draw(frame, x + 16, y + 14, tr(label), PX, st.normal, r.2, false);
                    hots.push((Hot::Form(id.clone()), r));
                }
                x += r.2 + 6;
            }
            W::Item { label, detail, buttons, .. } => {
                let span = buttons_span(buttons);
                let text_w = (width - span - if span > 0 { 8 } else { 0 }).max(0);
                if visible(y, row_h) {
                    let text = font::ellipsize(&tr(label), PX, text_w, false);
                    font::draw(frame, x, y + 14, &text, PX, st.normal, text_w, false);
                    if !detail.is_empty() {
                        let d = font::ellipsize(&tr(detail), PX, text_w, false);
                        font::draw(frame, x, y + 27, &d, PX, mix(st.normal, st.normal_bg, 0.45), text_w, false);
                    }
                    // Right-aligned, in order: place from the right edge backwards.
                    let mut bx = x0 + width;
                    for (id, label) in buttons.iter().rev() {
                        let bw = button_w(label);
                        bx -= bw;
                        let r = (bx, y, bw, h);
                        let down = *pressed == Some(Hot::Form(id.clone()));
                        button(frame, r, label, down, true);
                        hots.push((Hot::Form(id.clone()), r));
                        bx -= 6;
                    }
                }
                x = x0 + width;
            }
            W::Text(t) => {
                if visible(y, h) {
                    let room = (x0 + width - x).max(0);
                    let text = font::ellipsize(&tr(t), PX, room, false);
                    font::draw(frame, x, y + 14, &text, PX, mix(st.normal, st.normal_bg, 0.15), room, false);
                }
                x += font::measure(tr(t), PX, false) + 6;
            }
            _ => {}
        }
    }
    row_h + 5
}

/// The column that carries the row's name (everything else is metadata).
fn title_at(columns: &[(String, f32)]) -> usize {
    columns
        .iter()
        .position(|(n, _)| !matches!(n.as_str(), "Artist" | "Genre" | "Album" | "Format" | "File" | "Length" | "Items" | "Posted" | "Plays" | "Likes"))
        .unwrap_or(0)
}

// ---- the window ------------------------------------------------------------------

pub fn draw(frame: &mut Frame, skin: &Skin, v: &View) -> Vec<(Hot, R)> {
    let (w, h) = v.size();
    frame.resize(w, h);
    let (w, h) = (w as i32, h as i32);
    let st = &skin.playlist;
    let mut hots: Vec<(Hot, R)> = Vec::new();
    gen_frame(frame, skin, w, h, "Media Library", v.focused);
    if v.pressed == Some(Hot::Close) {
        frame.sprite(skin, "GEN_CLOSE_SELECTED", w - 11, 3);
    }
    hots.push((Hot::Close, close_rect(w)));
    hots.push((Hot::Resize, (w - 20, h - 20, 20, 20)));

    // The window's face between the panels: the colour inside GEN.BMP's
    // left border.
    let face = skin.sprite("GEN_MIDDLE_LEFT").map(|(img, r)| {
        let p = img.get_pixel(r.x + r.width - 1, r.y + r.height / 2).0;
        argb([p[0], p[1], p[2]])
    });
    let (ix, iy, iw, ih) = v.inner();
    frame.fill(ix, iy, iw, ih, face.unwrap_or(0xFF2A_2940));

    // The tree.
    let tree = (ix + 2, iy + 2, TREE_W, ih - 4);
    frame.fill(tree.0, tree.1, tree.2, tree.3, argb(st.normal_bg));
    for (n, (i, t)) in v.tree.iter().enumerate().skip(v.tree_top).take(v.tree_rows()).enumerate() {
        let _ = n;
        let y = tree.1 + 2 + (i - v.tree_top) as i32 * ROW;
        let r = (tree.0, y, tree.2, ROW);
        if t.selected {
            frame.fill(r.0, r.1, r.2, r.3, argb(st.selected_bg));
        }
        let indent = 4 + t.depth as i32 * 10;
        let col = if t.group || t.selected { st.current } else { st.normal };
        let label = if t.group { format!("{} {}", if t.open { "−" } else { "+" }, shown(&t.label)) } else { format!("› {}", shown(&t.label)) };
        let label = font::ellipsize(&label, PX, tree.2 - indent - 2, t.group);
        font::draw(frame, r.0 + indent, y + 11, &label, PX, col, tree.2 - indent - 2, t.group && t.depth == 0);
        hots.push((Hot::Tree(i), r));
    }

    // The toolbar: back, crumb, search.
    let tb_x = ix + TREE_W + 6;
    let tb_w = ix + iw - 2 - tb_x;
    let mut x = tb_x;
    if v.back {
        let r = (x, iy + 2, button_w("‹ Back"), 16);
        button(frame, r, "‹ Back", v.pressed == Some(Hot::Back), true);
        hots.push((Hot::Back, r));
        x += r.2 + 6;
    }
    if !v.crumb.is_empty() {
        let crumb = font::ellipsize(&shown(&v.crumb), PX, tb_w / 2, true);
        x += font::draw(frame, x, iy + 14, &crumb, PX, st.current, tb_w / 2, true) + 8;
    }
    if v.search_shown {
        let r = (x, iy + 2, tb_x + tb_w - x, 16);
        field(frame, skin, r, &v.search);
        hots.push((Hot::Search, r));
    }

    if let Some(items) = &v.form {
        // A settings form fills the rest.
        let area = (tb_x, iy + 2, tb_w, ih - 4);
        frame.fill(area.0, area.1, area.2, area.3, argb(st.normal_bg));
        let mut form_hots = Vec::new();
        let mut sub = frame.clone();
        // Never past the end: the form may have shrunk (or the window grown)
        // since the scroll was set.
        let scroll = v.form_scroll.min(max_form_scroll(skin, v)).max(0);
        let total = form(&mut sub, skin, area, items, scroll, &v.pressed, &mut form_hots);
        // Clip the form to its panel.
        for y in area.1..area.1 + area.3 {
            for x in area.0..area.0 + area.2 {
                let i = (y as u32 * frame.width + x as u32) as usize;
                frame.pixels[i] = sub.pixels[i];
            }
        }
        hots.extend(form_hots.into_iter().filter(|(_, r)| r.1 >= area.1 && r.1 + r.3 <= area.1 + area.3));
        if total > area.3 {
            // A thin scroll indicator.
            let th = (area.3 * area.3 / total).max(12);
            let ty = area.1 + ((area.3 - th) as f32 * scroll as f32 / (total - area.3) as f32) as i32;
            frame.fill(area.0 + area.2 - 4, ty, 3, th, argb(mix(st.normal, st.normal_bg, 0.5)));
        }
        return hots;
    }

    // The table.
    let t = v.table_rect();
    frame.fill(t.0, t.1, t.2, t.3, argb(st.normal_bg));
    let total: f32 = v.table.columns.iter().map(|c| c.1).sum::<f32>().max(0.01);
    let mut cx = t.0;
    let mut col_x = Vec::new();
    let usable = t.2 - 10;
    for (i, (label, share)) in v.table.columns.iter().enumerate() {
        let cw = if i + 1 == v.table.columns.len() { t.0 + usable - cx } else { (usable as f32 * share / total) as i32 };
        let r = (cx, t.1, cw, ROW);
        bevel(frame, r, v.pressed == Some(Hot::Header(i)));
        font::draw(frame, cx + 4, t.1 + 11, tr(label), PX, INK, cw - 6, false);
        hots.push((Hot::Header(i), r));
        col_x.push((cx, cw));
        cx += cw;
    }
    bevel(frame, (cx, t.1, t.0 + t.2 - cx, ROW), false);
    let rows = v.visible_rows();
    for (n, i) in (v.table.top..v.table.rows.len()).take(rows).enumerate() {
        let row = &v.table.rows[i];
        let y = t.1 + ROW + n as i32 * ROW;
        let r = (t.0, y, usable, ROW);
        if v.table.selected.contains(&i) {
            frame.fill(r.0, r.1, r.2, r.3, argb(st.selected_bg));
        }
        let col = if v.table.selected.contains(&i) { st.current } else if row.dim { mix(st.normal, st.normal_bg, 0.45) } else { st.normal };
        if row.folder {
            // Glyph and title span the row; the count stays in the last column.
            let indent = row.depth as i32 * 12;
            let glyph = if row.open { "\u{2212}" } else { "+" };
            let gx = r.0 + 4 + indent;
            let gw = font::measure(glyph, PX, true).max(1);
            font::draw(frame, gx, y + 11, glyph, PX, col, gw + 2, true);
            let title_idx = title_at(&v.table.columns);
            let title = row.cells.get(title_idx).map(String::as_str).unwrap_or("");
            let last = row.cells.len().saturating_sub(1);
            let count = if row.cells.len() > 1 && last != title_idx { row.cells[last].as_str() } else { "" };
            let (lx, lw) = col_x.last().copied().unwrap_or((r.0, r.2));
            let count_w = if count.is_empty() { 0 } else { font::measure(count, PX, false) };
            let count_x = lx + lw - count_w - 4;
            let title_x = gx + gw + 4;
            let title_end = if count.is_empty() { r.0 + r.2 - 4 } else { count_x - 6 };
            let title_w = (title_end - title_x).max(0);
            let title = font::ellipsize(title, PX, title_w, true);
            font::draw(frame, title_x, y + 11, &title, PX, col, title_w, true);
            if !count.is_empty() {
                font::draw(frame, count_x, y + 11, count, PX, col, count_w.max(1), false);
            }
            hots.push((Hot::Row(i), r));
            hots.push((Hot::Disclose(i), (gx, y, gw + 2, ROW)));
        } else {
            for (c, (x, cw)) in col_x.iter().enumerate() {
                let text = row.cells.get(c).map(String::as_str).unwrap_or("");
                let extra = if c == 0 { row.depth as i32 * 12 } else { 0 };
                let room = (cw - 6 - extra).max(0);
                let text = font::ellipsize(text, PX, room, false);
                font::draw(frame, x + 4 + extra, y + 11, &text, PX, col, room, false);
            }
            hots.push((Hot::Row(i), r));
        }
    }
    if v.table.rows.is_empty() && !v.table.empty.is_empty() {
        for (n, line) in wrap(&shown(&v.table.empty), t.2 - 20).iter().enumerate() {
            font::draw(frame, t.0 + 8, t.1 + ROW + 16 + n as i32 * 14, line, PX, mix(st.normal, st.normal_bg, 0.35), t.2 - 16, false);
        }
    }
    // Scrollbar.
    let sb = (t.0 + t.2 - 10, t.1 + ROW, 10, t.3 - ROW);
    frame.fill(sb.0, sb.1, sb.2, sb.3, argb(mix(st.normal_bg, FACE, 0.25)));
    if v.max_top() > 0 {
        let th = (sb.3 * rows as i32 / v.table.rows.len() as i32).max(14);
        let ty = sb.1 + ((sb.3 - th) as f32 * v.table.top as f32 / v.max_top() as f32) as i32;
        bevel(frame, (sb.0, ty, sb.2, th), v.pressed == Some(Hot::Scroll));
    }
    hots.push((Hot::Scroll, sb));

    // Buttons and status under the table.
    let by = t.1 + t.3 + 4;
    let mut bx = t.0;
    for (id, label) in &v.buttons {
        let r = (bx, by, button_w(label), 17);
        button(frame, r, label, v.pressed == Some(Hot::Button(id.clone())), true);
        hots.push((Hot::Button(id.clone()), r));
        bx += r.2 + 4;
    }
    let status = font::ellipsize(&shown(&v.status), PX, (t.0 + t.2 - bx - 4).max(0), false);
    let sw = font::measure(&status, PX, false);
    font::draw(frame, t.0 + t.2 - sw - 2, by + 13, &status, PX, st.normal, sw + 4, false);
    hots
}

/// Lays the settings form out without drawing it: its panel, its full
/// height, and where each heading starts (unscrolled).
fn form_layout(skin: &Skin, v: &View) -> Option<(R, i32, Vec<(String, i32)>)> {
    let items = v.form.as_ref()?;
    let (w, h) = v.size();
    let (ix, iy, iw, ih) = v.inner();
    let tb_x = ix + TREE_W + 6;
    let area = (tb_x, iy + 2, ix + iw - 2 - tb_x, ih - 4);
    let mut scratch = Frame::new(w, h);
    let mut marks = Vec::new();
    let total = form_marked(&mut scratch, skin, area, items, 0, &None, &mut Vec::new(), &mut marks);
    Some((area, total, marks))
}

/// The farthest a settings form can scroll (0 when it all fits).
pub fn max_form_scroll(skin: &Skin, v: &View) -> i32 {
    form_layout(skin, v).map(|(area, total, _)| (total - area.3).max(0)).unwrap_or(0)
}

/// How far to scroll a settings form so the heading starting with
/// `prefix` is at the top.
pub fn heading_scroll(skin: &Skin, v: &View, prefix: &str) -> Option<i32> {
    let (area, total, marks) = form_layout(skin, v)?;
    let y = marks.iter().find(|(t, _)| t.starts_with(prefix))?.1;
    Some(y.min((total - area.3).max(0)).max(0))
}

/// The first row for a pointer y on the scrollbar.
pub fn scroll_top_at(v: &View, y: i32) -> usize {
    let t = v.table_rect();
    let track = (t.3 - ROW).max(1);
    let p = ((y - t.1 - ROW) as f32 / track as f32).clamp(0.0, 1.0);
    (p * v.max_top() as f32).round() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms_never_scroll_past_the_end() {
        let skin = Skin::default_skin();
        let mut v = View { width: MIN_WIDTH, height: MIN_HEIGHT, ..Default::default() };
        let mut items = Vec::new();
        for i in 0..30 {
            items.push(W::Heading(format!("Section {i}")));
            items.push(W::button(&format!("b{i}"), "Press"));
        }
        v.form = Some(items);
        let max = max_form_scroll(&skin, &v);
        assert!(max > 0);
        v.form_scroll = max + 5000;
        let mut f = Frame::new(1, 1);
        let hots = draw(&mut f, &skin, &v);
        assert!(hots.iter().any(|(h, _)| *h == Hot::Form("b29".into())), "the last button is in view");
        // The indicator stays inside the panel: nothing drawn below it.
        let (ix, iy, iw, ih) = v.inner();
        let bg = argb(skin.playlist.normal_bg);
        let below = (iy + ih + 1..MIN_HEIGHT as i32 - 14).flat_map(|y| (ix..ix + iw).map(move |x| (x, y))).filter(|(x, y)| f.pixels[(*y as u32 * f.width + *x as u32) as usize] == bg).count();
        assert_eq!(below, 0, "panel colour leaked under the panel");
        v.form = Some(vec![W::Heading("Short".into())]);
        assert_eq!(max_form_scroll(&skin, &v), 0);
    }

    #[test]
    fn fields_edit() {
        let mut f = Field::new("Address");
        f.insert("nas:4533");
        f.move_caret(-5);
        f.insert("x");
        assert_eq!(f.value, "nasx:4533");
        f.backspace();
        f.end();
        f.backspace();
        assert_eq!(f.value, "nas:453");
    }

    #[test]
    fn draws_a_table_and_a_form() {
        let skin = Skin::default_skin();
        let mut v = View { width: 560, height: 348, focused: true, search_shown: true, ..Default::default() };
        v.tree = vec![
            TreeRow { label: "Local Media".into(), group: true, open: true, ..Default::default() },
            TreeRow { label: "Audio".into(), depth: 1, selected: true, ..Default::default() },
            TreeRow { label: "Internet Radio".into(), group: true, depth: 0, ..Default::default() },
        ];
        v.search.placeholder = "Search".into();
        v.table.columns = vec![("Artist".into(), 2.0), ("Title".into(), 3.0), ("Album".into(), 2.0), ("Length".into(), 0.8)];
        v.table.rows = (0..40).map(|i| Row { cells: vec![format!("Artist {i}"), format!("Song {i}"), "Album".into(), "3:25".into()], ..Default::default() }).collect();
        v.table.selected.insert(2);
        v.buttons = vec![("play".into(), "Play".into()), ("enqueue".into(), "Enqueue".into())];
        v.status = "40 items".into();
        let mut f = Frame::new(1, 1);
        let hots = draw(&mut f, &skin, &v);
        assert!(hots.iter().any(|(h, _)| *h == Hot::Row(0)));
        assert!(hots.iter().any(|(h, _)| *h == Hot::Button("play".into())));
        if let Ok(dir) = std::env::var("GAMP_SNAPSHOTS") {
            f.to_image().save(format!("{dir}/library.png")).unwrap();
        }
        v.form = Some(vec![
            W::Heading("Music folders".into()),
            W::Text("Add any folder, including network shares mounted in Finder.".into()),
            W::Item { id: "rm:0".into(), label: "/Volumes/music".into(), detail: String::new(), buttons: vec![("rm:0".into(), "Remove".into())] },
            W::Row(vec![W::button("add", "Add folder…"), W::button("rescan", "Rescan")]),
            W::Heading("Media servers".into()),
            W::Row(vec![W::Field("url".into(), Field { placeholder: "http://nas:4533".into(), ..Default::default() }), W::Field("user".into(), Field { value: "me".into(), focused: true, caret: 2, ..Default::default() })]),
            W::Check { id: "insecure".into(), label: "Accept a self-signed certificate".into(), checked: true },
        ]);
        let hots = draw(&mut f, &skin, &v);
        assert!(hots.iter().any(|(h, _)| *h == Hot::Form("add".into())));
        assert!(hots.iter().any(|(h, _)| *h == Hot::Form("user".into())));
        if let Ok(dir) = std::env::var("GAMP_SNAPSHOTS") {
            f.to_image().save(format!("{dir}/library-form.png")).unwrap();
        }
    }

    #[test]
    fn an_open_folder_discloses_on_its_glyph() {
        let skin = Skin::default_skin();
        let mut v = View { width: 560, height: 348, ..Default::default() };
        v.table.columns = vec![("Name".into(), 3.0), ("Items".into(), 0.7)];
        v.table.rows = vec![
            Row { cells: vec!["Artists".into(), "2".into()], folder: true, open: true, depth: 0, ..Default::default() },
            Row { cells: vec!["Beatles".into(), "12".into()], folder: true, open: false, depth: 1, ..Default::default() },
        ];
        let mut f = Frame::new(1, 1);
        let hots = draw(&mut f, &skin, &v);
        let rect = |hot: Hot| hots.iter().find(|(h, _)| *h == hot).map(|(_, r)| *r).unwrap();
        let parent = rect(Hot::Disclose(0));
        let glyph = rect(Hot::Disclose(1));
        let row = rect(Hot::Row(1));
        assert_eq!(glyph.0, parent.0 + 12, "a depth-1 glyph sits 12px in from its parent");
        let hit = |x: i32, y: i32| hots.iter().rev().find(|(_, r)| inside(*r, x, y)).map(|(h, _)| h.clone());
        assert_eq!(hit(glyph.0 + 1, glyph.1 + ROW / 2), Some(Hot::Disclose(1)));
        assert_eq!(hit(row.0 + row.2 - 2, row.1 + ROW / 2), Some(Hot::Row(1)));
    }

    fn form_hots(items: Vec<W>) -> Vec<(Hot, R)> {
        let skin = Skin::default_skin();
        let mut v = View { width: 720, height: 640, ..Default::default() };
        v.form = Some(items);
        let mut f = Frame::new(1, 1);
        draw(&mut f, &skin, &v)
    }

    fn hot(hots: &[(Hot, R)], id: &str) -> R {
        hots.iter().find(|(h, _)| *h == Hot::Form(id.into())).map(|(_, r)| *r).unwrap_or_else(|| panic!("no hot {id}"))
    }

    #[test]
    fn item_buttons_are_separate_and_right_aligned() {
        let hots = form_hots(vec![W::Item {
            id: "srv".into(),
            label: "DiskStation".into(),
            detail: String::new(),
            buttons: vec![("open".into(), "Open".into()), ("rename".into(), "Rename".into()), ("rm".into(), "Remove".into())],
        }]);
        let open = hot(&hots, "open");
        let rename = hot(&hots, "rename");
        let rm = hot(&hots, "rm");
        assert!(open.0 + open.2 <= rename.0, "open and rename overlap");
        assert!(rename.0 + rename.2 <= rm.0, "rename and remove overlap");
        assert!(open.0 < rename.0 && rename.0 < rm.0, "buttons keep their order");
        // The row's right edge is the form panel's content edge.
        let v = View { width: 720, height: 640, ..Default::default() };
        let (ix, _, iw, _) = v.inner();
        let tb_x = ix + 150 + 6;
        let tb_w = ix + iw - 2 - tb_x;
        let content_right = tb_x + tb_w - 6;
        assert_eq!(rm.0 + rm.2, content_right, "the last button sits on the right edge");
        assert!(open.0 > tb_x + 6, "the first button is not left-aligned");
    }

    #[test]
    fn an_item_with_detail_is_taller_and_a_note_grows_the_form() {
        let item = |detail: &str| W::Item {
            id: "srv".into(),
            label: "DiskStation".into(),
            detail: detail.into(),
            buttons: vec![("open".into(), "Open".into())],
        };
        let y = |items: Vec<W>| hot(&form_hots(items), "tail").1;
        let plain = y(vec![item(""), W::button("tail", "Tail")]);
        let detailed = y(vec![item("Jellyfin · http://nas:8096 · OK"), W::button("tail", "Tail")]);
        assert!(detailed > plain, "a detail line pushes the next widget down");
        let bare = y(vec![W::button("tail", "Tail")]);
        let noted = y(vec![W::Note { text: "This server's certificate isn't trusted.".into(), error: true }, W::button("tail", "Tail")]);
        assert!(noted > bare, "a note makes the form taller");
    }
}

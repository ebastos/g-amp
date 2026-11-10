//! Renders the windows with demo content, for screenshots:
//! `cargo run -p gamp-ui --example shots -- out/`
use gamp_skin::Skin;
use gamp_ui::library_window::{self as lw, Row, TreeRow};
use gamp_ui::main_window::{self, PlayState};
use gamp_ui::playlist_window::{self as plw, Track};
use gamp_ui::vis::VisMode;
use gamp_ui::{Frame, eq_window};

fn save(f: &Frame, path: String) {
    let img = f.to_image();
    let big = image::imageops::resize(&img, f.width * 2, f.height * 2, image::imageops::FilterType::Nearest);
    big.save(path).unwrap();
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    std::fs::create_dir_all(&dir).unwrap();
    let skin = Skin::default_skin();
    let mut f = Frame::new(1, 1);

    let mut v = main_window::View {
        state: PlayState::Playing,
        elapsed: 263.0,
        duration: Some(611.0),
        title: "4. Grateful Dead - Scarlet Begonias (Live, May 8 1977) (10:11)".into(),
        kbps: Some(192),
        khz: Some(44),
        stereo: Some(true),
        volume: 0.72,
        focused: true,
        eq_open: true,
        playlist_open: true,
        vis_mode: VisMode::Analyzer,
        scroll: 0,
        ..Default::default()
    };
    v.analyzer.update(&[0.95, 0.9, 0.85, 0.8, 0.72, 0.78, 0.66, 0.6, 0.62, 0.55, 0.5, 0.47, 0.44, 0.38, 0.35, 0.3, 0.26, 0.2, 0.12]);
    main_window::draw(&mut f, &skin, &v);
    save(&f, format!("{dir}/main.png"));

    let eq = eq_window::View { on: true, preamp: -2.0, bands: eq_window::PRESETS.iter().find(|p| p.0 == "Rock").unwrap().1, focused: false, ..Default::default() };
    eq_window::draw(&mut f, &skin, &eq);
    save(&f, format!("{dir}/eq.png"));

    let songs = [
        ("Grateful Dead - Minglewood Blues", "6:21"),
        ("Grateful Dead - Loser", "8:52"),
        ("Grateful Dead - El Paso", "5:18"),
        ("Grateful Dead - Scarlet Begonias", "10:11"),
        ("Grateful Dead - Fire On The Mountain", "14:02"),
        ("SomaFM Groove Salad", ""),
        ("Radiolab - Math Vs Machine", "31:22"),
        ("Bonobo - Kerala", "4:14"),
        ("Khruangbin - Maria También", "3:27"),
    ];
    let pl = plw::View {
        width: 275,
        height: 116 + 29 * 2,
        tracks: songs.iter().map(|(t, l)| Track { title: (*t).into(), length: (*l).into() }).collect(),
        current: Some(3),
        focused: false,
        running: "0:00/1:23:47".into(),
        mini_time: " 4:23".into(),
        ..Default::default()
    };
    plw::draw(&mut f, &skin, &pl);
    save(&f, format!("{dir}/playlist.png"));

    let mut lib = lw::View { width: 550, height: 406, focused: false, search_shown: true, ..Default::default() };
    lib.search.placeholder = "Search".into();
    let tree = [
        ("Local Media", 0, true), ("Audio", 1, false),
        ("Network & Cloud", 0, true), ("DiskStation (Plex)", 1, true), ("Recently Added", 2, false), ("Artists", 2, false), ("Playlists", 2, false),
        ("Navidrome", 1, true), ("Starred", 2, false),
        ("Internet", 0, true), ("Internet Radio", 1, true), ("Favorites", 2, false), ("Top Stations", 2, false), ("Genres", 2, false),
        ("Podcasts", 1, true), ("Internet Archive", 1, true),
        ("SoundCloud", 0, true), ("Spotify", 0, true),
    ];
    lib.tree = tree.iter().map(|(l, d, g)| TreeRow { label: (*l).into(), depth: *d, group: *g, open: true, selected: *l == "Top Stations" }).collect();
    lib.table.columns = vec![("Station".into(), 3.0), ("Genre".into(), 2.0), ("Format".into(), 1.1)];
    let stations = [
        ("SomaFM Groove Salad", "ambient, chillout, downtempo", "MP3 128 kbps"),
        ("BBC World Service", "news, talk", "MP3 56 kbps"),
        ("Radio Paradise Main Mix", "eclectic, rock, world", "AAC 320 kbps"),
        ("KEXP 90.3 FM", "indie, alternative", "AAC 128 kbps"),
        ("Jazz24", "jazz", "AAC 128 kbps"),
        ("FIP", "eclectic, jazz, world", "AAC 192 kbps"),
        ("SomaFM Drone Zone", "ambient, space", "MP3 128 kbps"),
        ("WFMU", "freeform", "MP3 128 kbps"),
        ("Classic Vinyl HD", "1930s, 1940s, 1950s", "MP3 320 kbps"),
        ("Dance Wave!", "dance, electronic", "MP3 128 kbps"),
        ("NTS Radio 1", "underground, eclectic", "MP3 256 kbps"),
        ("Venice Classic Radio", "classical", "AAC 128 kbps"),
        ("Lofi Girl Radio", "lofi, hip hop", "AAC 128 kbps"),
        ("Deutschlandfunk", "culture, news", "MP3 128 kbps"),
        ("Radio Swiss Jazz", "jazz", "MP3 128 kbps"),
        ("SomaFM Secret Agent", "lounge, spy", "MP3 128 kbps"),
        ("181.FM The Eagle", "classic rock", "AAC 64 kbps"),
        ("CBC Music", "canadian, pop", "AAC 64 kbps"),
        ("Linn Jazz", "jazz", "AAC 320 kbps"),
    ];
    lib.table.rows = stations.iter().map(|(a, b, c)| Row { cells: vec![(*a).into(), (*b).into(), (*c).into()], ..Default::default() }).collect();
    lib.table.selected.insert(0);
    lib.buttons = vec![("p".into(), "Play".into()), ("e".into(), "Enqueue".into()), ("a".into(), "Play all".into()), ("f".into(), "Add to Favorites".into())];
    lib.status = "19 items".into();
    lw::draw(&mut f, &skin, &lib);
    save(&f, format!("{dir}/library.png"));

    lib.table.columns = vec![("Name".into(), 3.0), ("Artist".into(), 2.0), ("Items".into(), 0.7)];
    lib.table.rows = vec![
        Row { cells: vec!["The Beatles".into(), String::new(), "2".into()], folder: true, open: true, ..Default::default() },
        Row { cells: vec!["Abbey Road".into(), "The Beatles".into(), "17".into()], folder: true, depth: 1, ..Default::default() },
        Row { cells: vec!["Let It Be".into(), "The Beatles".into(), "12".into()], folder: true, open: true, depth: 1, ..Default::default() },
        Row { cells: vec!["Two of Us".into(), "The Beatles".into(), String::new()], depth: 2, ..Default::default() },
    ];
    lib.table.selected.clear();
    lib.table.selected.insert(1);
    lib.status = "4 items".into();
    lw::draw(&mut f, &skin, &lib);
    save(&f, format!("{dir}/library-expanded.png"));
}

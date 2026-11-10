# Gamp

Native Winamp 2 player for macOS, Windows, and Linux. Rust workspace, edition 2024, rust-version 1.90. Each classic window is its own borderless OS window, drawn from a `.wsz` skin. Nothing runs in a web view.

## Commands

CI (`.github/workflows/ci.yml`) runs the first two, then the selftest.

- Test: `cargo test --workspace --locked`
- Build the app: `cargo build -p gamp --locked`
- Run: `cargo run --release -p gamp`
- Headless app check (play, scan, seek, draw): `target/debug/gamp --selftest`
- Package: `cargo packager --release -p gamp` (needs `cargo install cargo-packager`)

On Linux, install the ALSA, X11/Wayland, and xkbcommon packages listed in the CI workflow before building.

## Modules

- `crates/gamp-core` — music sources, library, Spotify, SoundCloud, settings, and the local server on `127.0.0.1:8742`. No UI.
- `crates/gamp-audio` — decode, resample, 10-band EQ, gapless output. Plays a file or an HTTP URL. Does not know about Plex or Spotify.
- `crates/gamp-skin` — `.wsz` loader. Shared sprite rectangles are in `crates/gamp-skin/src/sprites.json`.
- `crates/gamp-ui` — pixel drawing and hit-testing. No `winit`, so windows can be tested headless.
- `crates/gamp-vis` — G-Drop. A preset is an entry in `PRESETS` in `crates/gamp-vis/src/presets.rs`.
- `apps/gamp` — OS windows, input, session, and a tokio runtime beside the window thread (`backend.rs`).
- `tools/skin-gen` — draws the default skin, GAMP Classic (`assets/skins/gamp-classic.wsz`).

## Where code goes

| You are adding… | Put it in |
| --- | --- |
| A browsable server or directory (an account, or a built-in like radio) | `crates/gamp-core/src/sources/<name>.rs`, then the `Kind` arms in `sources/mod.rs` |
| A source with its own sign-in, not an `Account` (Spotify, SoundCloud) | Its own module under `gamp-core`, plus a `sid` arm in `catalog.rs` |
| A folder found via a desktop sync app (Google Drive, OneDrive) | `crates/gamp-core/src/cloud.rs` |
| Pixels or hit-testing for a classic window | `crates/gamp-ui` |
| What a click, key, or menu does | `apps/gamp` (`app.rs` and the window's module) |
| Decode, EQ, or device output | `crates/gamp-audio` |
| A G-Drop look | `PRESETS` in `crates/gamp-vis/src/presets.rs` |
| A sprite rectangle every skin shares | `sprites.json`, and the pixel notes in `docs/winamp-layout.md` |

## Playback

The audio engine never talks to a music service. The UI plays a `/media/...` URL on the local server. `apps/gamp/src/stream.rs` reads that URL with `Range` requests.

| Bytes come from… | URL the playlist stores |
| --- | --- |
| A scanned file | `/media/local/<id>` |
| An `Account` source (Plex, radio, Archive, …) | `/media/remote/<sid>/<id>?s=<sig>`, written by `sources::finish` |
| Spotify | `/media/spotify/<id>.wav` |
| SoundCloud | `/media/soundcloud/...` |
| A desktop-sync cloud folder | `/media/cloud/<sid>` |

For an account source, `Item.id` is the upstream id. `finish` fills `Item.url`. `sources::media` refuses a request whose `s` is not `sign(sid, id)`.

## Adding an account source

`catalog::browse` already forwards unknown ids to `sources::browse`. Add a `catalog.rs` arm only when the source is not an `Account`.

1. Add a `Kind` variant and `label()` in `crates/gamp-core/src/sources/mod.rs` (`#[serde(rename_all = "lowercase")]`).
2. Add `pub mod <name>;` and `sources/<name>.rs`. `browse` (and `search`, when the kind is searchable) returns `Listing` of `Item::folder` / `Item::track`. Leave `url` unset.
3. Add `media(acc, id) -> Upstream`. Copy the shape of `subsonic::media`: `Upstream::get(url).header(...)` when the server needs a header.
4. Wire `roots()`, `searchable()`, and the `browse` / `search` / `upstream` matches in `sources/mod.rs`.
5. If the user types an address, call `connect` from `sources::add`. OAuth (Plex, Dropbox) is an `/auth/...` route plus a `/callback/...` route in `server.rs` instead.
6. Test parsers on fixture JSON inside the source file. `cargo test` must not hit the network.

A built-in with no saved account (radio, podcasts, Archive, Audius) is pushed from `sources::list` via `Account::builtin` and is not handled in `add`.

```rust
Item::track(id, title).by(artist).on(album).secs(Some(310.0)).info("VBR MP3")
```

## A window control

1. Take the rectangle from `docs/winamp-layout.md`.
2. Draw it in `crates/gamp-ui` (`frame.sprite` / `frame.text`) and return it from that window's `hit`.
3. Handle the `Control` in `apps/gamp` (main window: the match in `app.rs` on `Control::Play`).
4. Assert `hit(x, y, shade)` in the ui crate. Drawing tests build a `Frame`, pass `Skin::default_skin()`, and stay headless.

## Strings

English is the lookup key. `t("Play")` reads `assets/lang/<code>.tsv` (`English<TAB>translation`) and falls back to the English. `tf` fills `{}` or `{0}`, `{1}`.

- **Do** wrap user-visible UI copy in `t` / `tf` and add that key, with the same `{}` placeholders, to every `assets/lang/*.tsv`.
  **Do not** hard-code a translation in Rust. `every_lookup_is_translated` and `tables_line_up` in `crates/gamp-ui/src/lang.rs` fail when a key or a placeholder is missing. They treat `de.tsv` as the reference table.
- **Do** use `tm` / `tmf` for the main window's 5×6 skin font. `frame.text` draws a missing letter as its unaccented A–Z form.
  **Do not** use `t` for that display. `tm` leaves Japanese and Chinese in English.

## Gotchas

- The server binds `127.0.0.1` only (`server::HOST`). The port is `gamp_core::PORT` (8742) because SoundCloud matches the sign-in callback exactly. `WINAMP_PORT` overrides it.
- HTTP inside `gamp-core` uses `http::client()`, or `account.http()` when `account.insecure` (self-signed NAS certs, per account). Both are rustls with webpki roots, a 15s connect timeout, and no overall timeout, because a stream lasts as long as the music.
- **Do** run network work through `Backend::spawn` in `apps/gamp/src/backend.rs`.
  **Do not** block the winit thread with a request. It stays synchronous.
- Classic windows are 275×116; shade mode is 14 high. The playlist's minimum is 275×116 and it grows in 25×29 steps.
- Passwords and tokens stay in the app data directory. `sources::info` is what the UI may see. Do not add secrets to that JSON.
- `GAMP_NO_UPDATE_CHECK` skips the update check. `gamp --update` checks immediately.

## References

- `docs/winamp-layout.md` — read when placing or hit-testing a classic-skin pixel.
- `README.md` — what each music source needs from the user (login, desktop app, nothing). Skip it when deciding which file to edit.

# quercus-engine-rs

Safe Rust SDK for the Quercus video engine, extracted from
[Oak Video Editor](https://github.com/OakVideoEditorCommunity/oak). Oak Video
Editor is the reference consumer; other Rust applications can use the same SDK.

The SDK is a thin, owned-value layer over the engine's stable C ABI: it links
`quercus-engine` as a cdylib (`#[link(name = "quercus_engine")]`) and never uses the
engine's Rust API. A program that depends on this crate (and indirectly on
`quercus-engine`) can build a complete non-linear editor without touching any
other Quercus crate.

```toml
[dependencies]
quercus-engine-rs = "0.6"
```

## Before you build: FFmpeg is mandatory

`quercus-engine` links the **project FFmpeg 8.1 build** statically. A system,
Homebrew or distro FFmpeg is not supported: if one is picked up through
`pkg-config`/`FFMPEG_DIR`, the link fails or the resulting binary loads the
wrong libraries.

1. Clone <https://github.com/OakVideoEditorCommunity/quercus-engine> once.
2. Run `tooling/install-deps.sh` and `tooling/ffmpeg/build-ffmpeg.sh`.
3. Point `FFMPEG_DIR` at the generated prefix, e.g.
   `export FFMPEG_DIR="$PWD/.cache/ffmpeg"` (the repository's
   `.cargo/config.toml` does this automatically when building inside the
   checkout).

There is no prebuilt FFmpeg download and no system-package fallback. Without
this step the engine's build script fails by design; silently linking a
different FFmpeg would be worse.

## Quick start

```rust,ignore
use quercus_engine_rs::editor::{EditorSession, TrackKind};
use quercus_engine_rs::edit::EditCommand;

let session = EditorSession::new()?;                 // a project in memory
let sequence = session.create_sequence("Demo")?;     // default V1/V2/A1/A2 tracks
let footage = session
    .execute(EditCommand::ImportFootage { path: "clip.mp4".into() })?
    .primary
    .expect("imported footage");
let clip = session
    .execute(EditCommand::PlaceFootageClip {
        footage,
        kind: TrackKind::Video,
        track_index: 0,
        in_ts: 0,
        out_ts: 50,
        media_in_ts: 0,
    })?
    .primary
    .expect("placed clip");
session.save("demo.ove")?;
```

A complete workflow (import → edit → effects/keyframes → preview/audio →
undo/redo → save/reopen → export) is documented in the workspace README and
exercised by this crate's tests; it needs no other Quercus crate.

## Lifecycle

- **`EditorSession`** owns a project, its current sequence, the clipboard,
  markers and the work area. Create one with `new`, `open`, or
  `open_project_from_library`; `from_project` wraps an existing `Project`
  handle (the session and the handle then refer to the same document).
- `EditorSession::close` releases engine-side session state; `Drop` does the
  same. Dropping a session does not invalidate `Project` handles obtained
  from it, so background work may keep a project alive safely.
- One process may create several sessions over the same project (e.g. a
  program and a source viewer). Edits are serialized by the engine.
- Call `close` (or drop the session) before tearing down the engine. The
  engine's global undo stack is shared by all sessions by design.

## Ownership

Every value returned across the ABI owns its data:

- `Project`, `PreviewFrame`, `AudioBlock`, `VideoFrame`, `Thumbnail`,
  `Waveform`, `Curve`, `ParameterSnapshot` and `GraphSnapshot` are RAII
  handles or owned copies. `Clone` adds a reference where the engine
  supports it; `Drop` releases exactly one.
- Frame and audio buffers are copied out of engine memory at the boundary.
  No pointer returned by this crate borrows engine memory.
- Task results (`take_project`, `take_video`, `take_audio`,
  `take_thumbnail`, `take_waveform`) can be transferred **once**. The second
  `take` returns `None`.
- `Task`, `TaskCancellation` and `PreviewFrameRequest` are `Send`; a worker
  thread may own them. Do not block a UI thread on `wait` when a poll will
  do.
- `Project` and `EditorSession` are `Send + Sync` handles. `NodeId` is a
  stable packed token; identities are **not** preserved across save/reopen,
  so resolve nodes from a snapshot after loading.

## Threading

- Rendering, audio mixing and export run on engine workers. The SDK never
  silently renders on the caller's thread: `Task::wait` blocks the caller,
  and `PreviewSession::request_frame`/`request_audio` return pollable
  requests.
- Poll task state from the UI (`Task::state`, `progress`, `title`), and
  transfer results when they succeed.
- Callbacks (`edit` subscriptions, undo observers, OFX progress) are invoked
  on the engine thread that fires them; the SDK serializes Rust callbacks
  and catches panics. Keep callbacks short and never re-enter the engine
  from one.
- A preview generation (`PreviewSession::generation`) identifies stale
  results after seek/invalidate. Discard frames whose generation no longer
  matches.

## Errors

- Fallible calls return `Result<T, String>` or `Result<T, StorageError>`.
  The string is the engine's human-readable error; stable numeric codes are
  not part of the Rust surface yet.
- Queries return `None`/empty for stale ids instead of panicking.
- Null/invalid handles fail closed: a dropped engine or session yields
  errors rather than undefined behavior.

## Tasks and events

| API | Purpose |
|---|---|
| `task::export` | encode a sequence to a file; `Task::cancel` cooperatively stops it |
| `task::render_video` / `render_playback_video` / `render_track_video` | owned preview frames |
| `task::render_audio` | owned audio blocks |
| `task::extract_waveform` | waveform peaks |
| `task::render_thumbnail` | RGBA thumbnail |
| `task::load_otio` / `save_otio` | OTIO/FCPXML interchange |
| `editor::EditorSession::subscribe_edit` | per-command notifications |
| `node::add_undo_observer` | undo-stack changes |
| `ofx::set_progress_handler` | plugin progress events |

## C ABI

The C header is generated from this crate's extern declarations and the
`#[repr(C)]` mirrors:

- `crates/quercus-engine/include/quercus_engine.h`
- regenerate with `python3 tooling/gen_c_header.py --write`
- CI checks it with `python3 tooling/gen_c_header.py --check`

Layouts of the common mirrors are asserted by `_Static_assert` in the header.

## Linking and packaging

The SDK links the engine cdylib at **link time**; it does not download or
build anything at run time. `quercus-engine` is built by Cargo as a
dependency (the `engine` feature, enabled by default).

- The engine is built with a relocatable install name on macOS
  (`@rpath/libquercus_engine.dylib`) and a soname on Linux
  (`libquercus_engine.so`).
- An application must ship the dylib next to its executable (or in a `lib/`
  directory) and add an rpath:
  - macOS: `-Wl,-rpath,@loader_path` (and/or `@loader_path/../lib`)
  - Linux: `-Wl,-rpath,$ORIGIN`
  - Windows: copy `quercus_engine.dll` next to the `.exe`
- `cargo install` copies only the executable; applications that support
  `cargo install` must embed the engine or install the dylib separately.
- Disable the default feature only for docs-only builds:
  `quercus-engine-rs = { version = "0.5", default-features = false }`.

The SDK's own tests add `@loader_path`/`$ORIGIN` automatically.

## docs.rs

docs.rs builds this crate with `--no-default-features` and therefore does
not need the native engine/FFmpeg; the public Rust API is still fully
documented. The engine crate page is a stub because its implementation is a
C ABI library.

## Publishing

Publish the whole workspace in dependency order (the internal crates are
published but are implementation details):

```text
quercus-core → quercus-ffmpeg-link → quercus-undo → quercus-codec
→ quercus-node → quercus-otio → quercus-render → quercus-audio
→ quercus-timeline → quercus-storage → quercus-task
→ quercus-ofx-plugin → quercus-engine → quercus-engine-rs
```

`tooling/publish_workspace.sh` runs exactly that order. `quercus-engine`
must be published before this crate (the SDK declares a versioned path
dependency).

## License

GPL-3.0-or-later.

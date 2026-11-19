# Kinetiqa

A minimal desktop moodboard written in Rust: drop images, animated GIFs and videos onto an infinite board, add flowchart shapes connected by arrows, move, resize, and remove them. Early stage — the board/UI lives in `src/main.rs`, media loading/playback in `src/media.rs`, flowchart shapes in `src/shapes.rs`, board files in `src/save.rs`, app settings in `src/settings.rs`, and the visual theme in `src/theme.rs`; there are no tests yet.

## Commands

- `cargo run` — launch the app (native window, 1200×800)
- `cargo check` / `cargo clippy` — fast compile/lint check
- `cargo build --release` — optimized build

## Stack

- **Rust edition 2024** — the code uses let-chains (`if a && let Some(x) = y`) freely; keep that style.
- **eframe/egui 0.37** — newer API than most online examples: the app implements `eframe::App::ui(&mut self, ui, frame)` (not `update(ctx, frame)`), and panels use `egui::Panel::top(...)`. Check the 0.37 docs before copying older snippets.
- **image 0.25** — default features off; only png, jpeg, gif, webp, bmp decoders enabled. Adding a format means enabling its feature in `Cargo.toml` *and* adding the extension to `IMAGE_EXTENSIONS` in `media.rs`.
- **ffmpeg (runtime, not a crate)** — videos are decoded by spawning the system `ffprobe`/`ffmpeg` binaries. If they're missing, video loads fail with an error in the toolbar; images still work. Video extensions live in `VIDEO_EXTENSIONS`.
- **rfd 0.18** — native file pickers (Add media, Open, Save).
- **serde + serde_json** — board files.

## Architecture

### src/main.rs

- `Board` — the whole app state: `items: Vec<Item>`, `links: Vec<Link>`, `selected: Vec<u64>` (ctrl+click adds/removes items) / `selected_link` (only set with no items selected), `pan`, the in-progress `drag`, `connecting`, `editing`, the right-click `menu`, loader channel (`tx`/`rx`), `pending` count and load `errors`.
- `Item` — one board item: `id`, `content: Content` (`Media { media, path }` or `Shape(shapes::Shape)`), `pos` (top-left, **board coordinates**), `size`.
- `Link` — an arrow `from` → `to` (item ids), with its own id from the same `next_id` counter. Links whose ends are missing (removed, or media that failed to load) aren't drawn or saved; `remove` drops an item's links.
- **Z-order = order in `items`.** Last is drawn on top; clicking/dragging an item moves it to the end ("bring to front").
- **Coordinates:** board space vs. screen space. `to_screen(p) = canvas.center() + pan + p * zoom`; on-screen size is `size * zoom`; drops convert back with `(at - canvas.center() - pan) / zoom`, and drag deltas are divided by `zoom`. Item positions and sizes are always stored in board space; `pan` and `EDGE_GRAB` are in screen space. `pan` is measured from the canvas **center**, so `pan == 0` always means board (0, 0) is centered — at startup and after window resizes (don't measure it from `canvas.min`: the first frames can have a not-yet-settled window size).
- **Zoom:** `zoom` is unbounded (default 1.0); only non-finite or non-positive values are rejected. The dot grid coarsens by 4× steps when zoomed out so it never paints too many dots. Scroll wheel / pinch over the canvas calls `zoom_around`, which keeps the board point under the cursor fixed by adjusting `pan`. The toolbar's `− N% +` control sets `zoom_to` (±`ZOOM_STEP`, or 1.0 when clicking the %), applied in `board()` around the canvas center.
- **View center:** the app starts with `pan = 0`, `zoom = 1`, i.e. centered on (0, 0) at 100%. `board()` stores the board coordinates of the canvas center in `view_center` each frame; the toolbar's "Add media…" places files there, and a button at the canvas's bottom right shows the zoom and the view center (`N%   x N   y N`, y grows downward). Clicking that button sets `pan = 0` (re-centers on (0, 0)) without changing the zoom. The button is placed after the items so it stays on top of them.
- **Async loading:** `Board::load` spawns one thread per file, which runs `media::decode()` and sends a `Decoded` (holding a `Loaded`) back over an mpsc channel, then calls `ctx.request_repaint()`. `Board::receive` drains the channel each frame and calls `Loaded::upload` to create textures. Never decode on the UI thread.
- **Painting media:** for each *visible* item, `Media::update()` uploads the newest video frame, then `Media::current(time)` picks the texture to draw and how long until it changes (GIFs), which feeds `request_repaint_after`.
- **Interaction:**
  - Background drag = pan, background click = deselect, scroll = zoom.
  - Each item's hit rect is expanded by `EDGE_GRAB` so edges can be grabbed from just outside. `Grab::at` decides which edges are grabbed; no edges = move.
  - `Drag` records start pointer/pos/size; `resized()` computes the new rect anchoring the opposite edge/corner and clamping to `MIN_EDGE`. Media keeps its aspect ratio; shapes resize freely unless Shift is held (`keep_aspect`).
  - Items are painted *after* applying this frame's drag so the image doesn't lag one frame behind the cursor.
  - Right-click opens a foreground `egui::Area` options menu for a `Target` (`Item` or `Link`): shapes get "Edit text" + color swatches (`shapes::PALETTE`), arrows "Reverse direction", everything "Remove". `menu_opened` stops the opening click from immediately closing it. The menu only records a `MenuAction`, applied after it has run; picking a color keeps it open.
  - **Multi-select:** Ctrl (`modifiers.command`) + click toggles an item in `selected`; Ctrl+drag adds it. A plain press on an already-selected item keeps the group, and dragging moves all of them. The group's start positions go in `Drag::group` (filled after the item loop) and are applied in a pre-pass before the loop, so the group moves without lagging. Edges only resize when a single item is selected. Right-click's menu still targets just the one item. Delete/Backspace and the toolbar's Remove use `remove_selected`, unless egui wants keyboard input.
  - **Shapes:** the toolbar's "Shapes" menu (`shape_entry` rows) calls `add_shape` at the view center (cascading if something is already exactly there). Double-click, Enter/F2 or "Edit text" sets `editing = Some((id, focus))`; a frameless multiline `TextEdit` is laid over `shapes::text_rect` (registered after the item, so it wins clicks) and editing ends when it loses focus (click elsewhere / Escape). The painted label is skipped while editing.
  - **Arrows:** hovered/selected items show a connect handle (`draw_connect_handle`, `CONNECT_HANDLE`) just right of the item, touching its `EDGE_GRAB` zone so hover isn't lost on the way; it shares `controls_hover` with the video controls. Dragging it sets `connecting`; the preview snaps to the topmost item under the pointer and `connect` adds the link on release (no duplicates or self-links). Arrows are painted *below* all items via a `painter.add(Shape::Noop)` slot reserved before the item loop and filled after it (`painter.set`), so they use this frame's positions without lagging. They're hit-tested on the bare background only (`LINK_GRAB` from the segment): background click selects the arrow under the pointer (or deselects), right-click opens its menu. Ends are `shapes::boundary_point` (exact for ellipse/diamond, bounding rect otherwise).
  - Video items show controls (`video_controls`: play/pause button, seekable timeline, `elapsed / total`) in a pill along their bottom when hovered or selected and at least `VIDEO_CONTROLS_MIN` on screen; they're fixed screen size, registered after the item so they win its clicks/drags, and `controls_hover` remembers last frame's hover so they don't flicker (the item isn't "hovered" while its controls cover it). A paused video without controls shows a center play badge. Space toggles the selected video.
- **Auto-hiding toolbar:** `toolbar_overlay` draws the toolbar in a foreground `egui::Area` over the top of the board (not a panel, so showing it never resizes the canvas and shifts the center-based `pan`). It slides in (`animate_bool_with_time`) when the pointer is within `TOOLBAR_REVEAL` of the top, stays while the pointer is over it, and is forced visible while a dialog is open, errors are shown or media is loading; a small handle at the top edge hints at it. Ctrl+S/Ctrl+Shift+S/Ctrl+O live in `Board::shortcuts` (run every frame) since `toolbar()` doesn't run while hidden. It also stays while any popup (the Shapes menu) is open (`egui::Popup::is_any_open`), or moving down into the menu would hide it.
- Files dragged over the window get a highlight border; dropped files land at the cursor, with multiple files cascaded by 24px.

- **Look:** items get a soft `ITEM_SHADOW`, rounded corners (`ITEM_RADIUS * zoom`), an accent ring + corner handles when selected, a faint ring on hover, and a `GIF`/`▶ VIDEO` badge (`Media::badge`) when hovered/selected. Helpers at the bottom of main.rs: `draw_dots` (minor + major grid), `brand`, `primary_button`, `pill`, `draw_empty_state`. The toolbar hint hides itself when there's no room.

- **Save / Open:** toolbar buttons + Ctrl+S / Ctrl+Shift+S (save as) / Ctrl+O. All boards live in one folder, `settings.boards_dir`; a board is identified by `Board::name` and stored at `<boards_dir>/<name>.kinetiqa` (shown in the window title). The first save / save as opens the `Dialog::SaveAs` name prompt (warns before replacing an existing board); Open shows `Dialog::Open`, a list of the folder's boards (`save::list`, newest first). Save is disabled while `pending > 0`, since loading items aren't in `items` yet. `open` bumps `generation` (in-flight loads from before are discarded in `receive`), clears the board, restores pan/zoom and reserves ids in saved z-order; shapes are pushed right away (in order), links are rebuilt from saved indices, and each media load carries a `Place::Exact { id, pos, size }` and `receive` inserts it below any higher id, so z-order survives loads finishing out of order. New items use `Place::At(center)`.
- **Dialogs:** `Board::dialog` holds at most one `egui::Modal` (`SaveAs`, `Open`, `Settings`), drawn by `dialogs()` after the panels. Its UI only records a `DialogAction`, which runs after the modal so it can borrow `self`. Board keyboard shortcuts (Space/Delete) are ignored while a dialog is open.
- **Settings:** the ⚙ button at the toolbar's top right opens `Dialog::Settings`, where the boards folder can be changed (rfd folder picker) or reset to the default.

### src/save.rs

- `BoardFile` (JSON, `.kinetiqa`, `VERSION` 2): `version`, `pan`, `zoom`, `items` (bottom-most first: either `path` for media or `shape` = `{ kind, text, color: [r, g, b] }`, plus `pos`, `size`), and `links` (`{ from, to }` as indices into `items`, since ids aren't saved). Version 1 files (media only, no links) still load. Media isn't embedded. Paths inside the board file's folder are stored relative to it, others absolute; `read` resolves them back to absolute. Bump `VERSION` on incompatible changes. Video playback state isn't saved.

### src/shapes.rs

- `ShapeKind` (serde snake_case): `Process`, `Terminal` (start/end), `Decision`, `Data` (I/O), `Document`, `Database`, `Predefined` (subprocess), `Preparation`, `Manual`, `Ellipse`, `Text` (frameless label). `ALL` is the menu order; each has a `label`, `hint`, `default_size` and `default_color`. `Shape { kind, text, color }` is an item's content.
- Everything works in **screen space** (the item's on-screen rect + zoom). `paint` draws fill (a dark tint of `color`, opaque) + outline: convex outlines use `convex_polygon`; the document's wavy bottom isn't convex, so it's filled with a triangle fan from the center. `text_rect` is the label's wrap area per kind; `paint_text` centers it (`FONT_SIZE * zoom`, skipped when tiny; an empty `Text` shape shows a dim placeholder). `boundary_point`, `paint_arrow`, `distance_to_segment` serve the arrows.

### src/settings.rs

- `Settings` (currently just `boards_dir`, default `~/Kinetiqa`) as JSON in the platform config folder (`$XDG_CONFIG_HOME` or `~/.config` on Linux, `~/Library/Application Support` on macOS, `%APPDATA%` on Windows) under `kinetiqa/settings.json`. Missing/invalid settings fall back to defaults; new fields need `#[serde(default)]`-compatible defaults.

### src/theme.rs

- Palette `const`s (`CANVAS`, `PANEL`, `SURFACE*`, `BORDER`, `TEXT*`, `ACCENT*`, `ON_ACCENT`, `DANGER`, `DOT*`) plus item shadow/radius. `theme::apply` (called from the eframe creation callback) forces dark mode and writes the palette into egui's dark `Style`. Custom painting should use these constants rather than hard-coded colors.

### src/media.rs

- `decode()` dispatches by extension: videos → `open_video`, `.gif` → `decode_gif` (single-frame GIFs become stills), everything else → `decode_still`. Stills/GIF frames are downscaled to `MAX_TEXTURE_EDGE` (2048).
- `Loaded` (CPU images, built off-thread) → `Media` (GPU textures): `Still`, `Animated { frames: (texture, delay_secs), total }` (looped by global time), `Video { texture, player }`.
- **Videos:** `probe()` runs ffprobe for size, sample aspect ratio (non-square MPEG-2 pixels widen the display width), rotation tag (quarter turns swap w/h, since ffmpeg auto-rotates) and duration. Every video is decoded and played at a fixed `VIDEO_FPS` (60), whatever the source rate. A `Decoder` spawns `ffmpeg [-ss T] -i … -vf fps=F,scale=W:H -f rawvideo -pix_fmt rgba pipe:1` (no `-re`, no `-stream_loop`; constant-rate output so frame N is at `start + N/VIDEO_FPS`), scaled to fit `MAX_VIDEO_EDGE` (1280, even dims). `open_video` reads the first frame synchronously, then the `play` thread paces frames itself against a wall-clock anchor, publishes the newest one in the shared `PlayerState`, and handles pause (stops reading; ffmpeg blocks on the full pipe), seek (restarts ffmpeg at the target and shows that frame even when paused) and looping (restarts at 0 on EOF). `VideoPlayer` exposes `position`/`duration`/`is_paused`/`set_paused`/`seek`. The current `Child` lives in `PlayerState` so `Drop` can kill it while the thread is blocked reading. Videos are muted.

## Conventions

- Tunable constants live at the top of their file as documented `const`s (`DEFAULT_WIDTH`, `EDGE_GRAB`, `MIN_EDGE`, `ZOOM_SPEED`/`ZOOM_STEP`, `LINK_GRAB`, `CONNECT_HANDLE` in main.rs; `FONT_SIZE`, `PALETTE` in shapes.rs; `MAX_TEXTURE_EDGE`, `MAX_VIDEO_EDGE`, `VIDEO_FPS`, GIF delay defaults in media.rs).
- Items are referenced by stable `u64` ids (from `next_id`), never by Vec index, since the Vec is reordered for z-order.
- Short doc comments explain *why* (e.g. frame-lag, menu-closing quirks); keep them when refactoring.
- If main.rs grows much further, splitting it further (e.g. `board.rs`, `item.rs`) is a reasonable next step.

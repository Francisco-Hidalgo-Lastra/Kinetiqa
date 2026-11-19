//! Kinetiqa — a minimal moodboard: drop images, GIFs and videos on a board, add flowchart shapes
//! and arrows, move them around, remove them.

mod media;
mod save;
mod settings;
mod shapes;
mod theme;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::{self, Align2, Color32, CornerRadius, Key, Margin, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};

use media::{IMAGE_EXTENSIONS, Loaded, Media, VIDEO_EXTENSIONS, VideoPlayer};
use settings::Settings;
use shapes::ShapeKind;
use theme::ITEM_RADIUS;

/// Default on-board width for a newly placed image.
const DEFAULT_WIDTH: f32 = 260.0;
/// How fast the scroll wheel zooms: the zoom is multiplied by `exp(scroll_points * ZOOM_SPEED)`,
/// so one mouse-wheel notch is roughly ±10%.
const ZOOM_SPEED: f32 = 0.002;
/// Factor applied by the toolbar's zoom −/+ buttons.
const ZOOM_STEP: f32 = 1.25;
/// Video controls (play button + timeline) only appear on items at least this big on screen.
const VIDEO_CONTROLS_MIN: Vec2 = Vec2::new(140.0, 70.0);
/// The hidden toolbar slides in when the pointer comes within this distance of the window top.
const TOOLBAR_REVEAL: f32 = 10.0;
/// How close (screen points) the pointer must be to an arrow to hover / click it.
const LINK_GRAB: f32 = 6.0;
/// Diameter of the connect handle shown beside a hovered/selected item.
const CONNECT_HANDLE: f32 = 18.0;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Kinetiqa")
            .with_inner_size([1200.0, 800.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("Kinetiqa", options, Box::new(|cc| {
        theme::apply(&cc.egui_ctx);
        Ok(Box::new(Board::default()))
    }))
}

struct Item {
    id: u64,
    content: Content,
    /// Top-left corner in board coordinates.
    pos: Pos2,
    size: Vec2,
}

/// What a board item shows.
enum Content {
    /// `path` is the file the media was loaded from; what a saved board refers to.
    Media { media: Media, path: PathBuf },
    Shape(shapes::Shape),
}

impl Item {
    fn shape_kind(&self) -> Option<ShapeKind> {
        match &self.content {
            Content::Shape(shape) => Some(shape.kind),
            Content::Media { .. } => None,
        }
    }

    fn video(&self) -> Option<&VideoPlayer> {
        match &self.content {
            Content::Media { media, .. } => media.video(),
            Content::Shape(_) => None,
        }
    }
}

/// An arrow from one item to another. Has its own id (from the same counter as items) so it
/// can be selected; dangling links (an end that's gone or failed to load) aren't drawn or saved.
struct Link {
    id: u64,
    from: u64,
    to: u64,
}

/// What was picked in the right-click menu; applied after the menu's UI has run.
#[derive(Clone, Copy)]
enum MenuAction {
    EditText,
    Color(Color32),
    Reverse,
    Remove,
}

/// What a right-click menu is for.
#[derive(Clone, Copy, PartialEq)]
enum Target {
    Item(u64),
    Link(u64),
}

/// Thickness (in points) of the grab zone along an image's edges for resizing.
const EDGE_GRAB: f32 = 6.0;
/// Smallest width/height an image can be resized down to.
const MIN_EDGE: f32 = 40.0;

/// Which edges of an image are being grabbed. All false means "move".
#[derive(Clone, Copy, Default, PartialEq)]
struct Grab {
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
}

impl Grab {
    /// Work out which edges `pointer` is near on `rect` (the image's on-screen rect).
    fn at(rect: Rect, pointer: Pos2) -> Self {
        let near = |d: f32| d.abs() <= EDGE_GRAB;
        Self {
            left: near(pointer.x - rect.left()),
            right: near(pointer.x - rect.right()),
            top: near(pointer.y - rect.top()),
            bottom: near(pointer.y - rect.bottom()),
        }
    }

    fn is_resize(self) -> bool {
        self.left || self.right || self.top || self.bottom
    }

    fn cursor(self) -> egui::CursorIcon {
        use egui::CursorIcon as C;
        match (self.left, self.right, self.top, self.bottom) {
            (true, _, true, _) | (_, true, _, true) => C::ResizeNwSe,
            (true, _, _, true) | (_, true, true, _) => C::ResizeNeSw,
            (true, _, _, _) | (_, true, _, _) => C::ResizeHorizontal,
            (_, _, true, _) | (_, _, _, true) => C::ResizeVertical,
            _ => C::Default,
        }
    }
}

/// An in-progress drag on an image (move or resize).
#[derive(Clone)]
struct Drag {
    id: u64,
    grab: Grab,
    start_pointer: Pos2,
    start_pos: Pos2,
    start_size: Vec2,
    /// Items moved together (the whole selection) with their positions when the drag started.
    /// Empty for resizes, which only ever affect the one item.
    group: Vec<(u64, Pos2)>,
}

/// Decoded media coming back from the loader thread.
struct Decoded {
    path: PathBuf,
    result: Result<Loaded, String>,
    place: Place,
    /// `Board::generation` when the load started; loads from before opening a board are dropped.
    generation: u64,
}

/// Where a loaded item goes on the board.
#[derive(Clone, Copy)]
enum Place {
    /// Newly added: centered here (board coordinates) at the default width, and selected.
    At(Pos2),
    /// Restored from a board file, with an id reserved in saved z-order (loads finish in any
    /// order, so the id is what puts it back at the right depth).
    Exact { id: u64, pos: Pos2, size: Vec2 },
}

struct Board {
    items: Vec<Item>,
    next_id: u64,
    /// Selected items (ctrl+click adds/removes). Empty when nothing or only an arrow is selected.
    selected: Vec<u64>,
    /// Screen-space offset of the board origin from the canvas center (panning). Zero means
    /// (0, 0) is in the middle of the canvas — at startup, after re-centering, and regardless
    /// of the window size (which can still be settling during the first frames).
    pan: Vec2,
    /// Board-to-screen scale factor (1.0 = 100%). Unbounded, but always finite and positive.
    zoom: f32,
    /// Zoom requested by the toolbar's zoom buttons; applied (around the canvas center) in
    /// `board()`, which knows the canvas rect.
    zoom_to: Option<f32>,
    /// Board coordinates at the center of the canvas, updated each frame by `board()`.
    view_center: Pos2,
    tx: Sender<Decoded>,
    rx: Receiver<Decoded>,
    pending: usize,
    errors: Vec<String>,
    drag: Option<Drag>,
    /// Arrows between items, drawn below all items.
    links: Vec<Link>,
    /// Selected arrow. Only set when no items are selected.
    selected_link: Option<u64>,
    /// Item whose connect handle is being dragged to make a new arrow.
    connecting: Option<u64>,
    /// Shape whose label is being typed into, and whether its text field still needs focus.
    editing: Option<(u64, bool)>,
    /// Right-click options window: what it's for and where it opened (screen space).
    menu: Option<(Target, Pos2)>,
    /// Item whose overlay (video controls or connect handle) had the pointer last frame. The
    /// overlay isn't part of the item, so the item itself isn't "hovered" then — this keeps the
    /// overlay (and the hover ring) from flickering.
    controls_hover: Option<u64>,
    /// Whether the auto-hiding toolbar was showing last frame, and its last measured height.
    toolbar_visible: bool,
    toolbar_height: f32,
    /// Name of the board last saved or opened, which "Save" writes back to. Boards live in
    /// `settings.boards_dir`, so the file is `<boards_dir>/<name>.kinetiqa`.
    name: Option<String>,
    settings: Settings,
    /// Modal dialog on top of everything, if any.
    dialog: Option<Dialog>,
    /// Bumped when a board is opened, so media still loading for the previous one is discarded.
    generation: u64,
}

/// The modal dialogs (one at a time).
enum Dialog {
    /// Naming the board on its first save or "save as". `focus` puts the cursor in the name
    /// field on the first frame.
    SaveAs { name: String, focus: bool },
    /// Picking one of the boards in the boards folder (listed when the dialog opened).
    Open(Vec<save::Entry>),
    Settings,
}

/// What a dialog asked for; run after the dialog's UI so it can borrow `self` freely.
enum DialogAction {
    Save(String),
    Open(PathBuf),
    PickFolder,
    ResetFolder,
}

impl Default for Board {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            items: Vec::new(),
            next_id: 0,
            selected: Vec::new(),
            pan: Vec2::ZERO,
            zoom: 1.0,
            zoom_to: None,
            view_center: Pos2::ZERO,
            tx,
            rx,
            pending: 0,
            errors: Vec::new(),
            drag: None,
            links: Vec::new(),
            selected_link: None,
            connecting: None,
            editing: None,
            menu: None,
            controls_hover: None,
            toolbar_visible: false,
            toolbar_height: 46.0,
            name: None,
            settings: Settings::load(),
            dialog: None,
            generation: 0,
        }
    }
}

impl Board {
    /// Decode media off the UI thread so large files don't freeze the window.
    fn load(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>, drop_at: Pos2) {
        for (i, path) in paths.into_iter().enumerate() {
            // Cascade multiple images so they don't stack exactly on top of each other.
            self.spawn_load(ctx, path, Place::At(drop_at + Vec2::splat(24.0 * i as f32)));
        }
    }

    fn spawn_load(&mut self, ctx: &egui::Context, path: PathBuf, place: Place) {
        self.pending += 1;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let generation = self.generation;
        std::thread::spawn(move || {
            let result = media::decode(&path, &ctx);
            let _ = tx.send(Decoded { path, result, place, generation });
            ctx.request_repaint();
        });
    }

    fn receive(&mut self, ctx: &egui::Context) {
        while let Ok(decoded) = self.rx.try_recv() {
            self.pending -= 1;
            if decoded.generation != self.generation {
                continue;
            }
            let loaded = match decoded.result {
                Ok(loaded) => loaded,
                Err(e) => {
                    self.errors.push(format!("Couldn't load {}: {e}", decoded.path.display()));
                    continue;
                }
            };
            let [w, h] = loaded.size();
            let media = loaded.upload(ctx, &decoded.path.display().to_string());
            let content = Content::Media { media, path: decoded.path };
            match decoded.place {
                Place::At(center) => {
                    let size = Vec2::new(DEFAULT_WIDTH, DEFAULT_WIDTH * h as f32 / w as f32);
                    let id = self.new_id();
                    self.items.push(Item { id, content, pos: center - size / 2.0, size });
                    self.select(id);
                }
                Place::Exact { id, pos, size } => {
                    // Below everything with a higher id: later restored items and anything added
                    // since opening.
                    let idx = self.items.iter().position(|i| i.id > id).unwrap_or(self.items.len());
                    self.items.insert(idx, Item { id, content, pos, size });
                }
            }
        }
    }

    fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id - 1
    }

    fn select(&mut self, id: u64) {
        self.selected = vec![id];
        self.selected_link = None;
    }

    /// Remove everything selected: the selected items, or the selected arrow.
    fn remove_selected(&mut self) {
        for id in std::mem::take(&mut self.selected) {
            self.remove(id);
        }
        if let Some(id) = self.selected_link.take() {
            self.remove_link(id);
        }
    }

    /// Add a new shape centered on `center` (board coordinates) and select it. Repeated adds at
    /// the same spot cascade instead of stacking exactly.
    fn add_shape(&mut self, kind: ShapeKind, center: Pos2) {
        let size = kind.default_size();
        let mut pos = center - size / 2.0;
        while self.items.iter().any(|i| (i.pos - pos).length() < 1.0) {
            pos += Vec2::splat(24.0);
        }
        let id = self.new_id();
        self.items.push(Item { id, content: Content::Shape(shapes::Shape::new(kind)), pos, size });
        self.select(id);
    }

    /// Add an arrow `from` → `to` unless it already exists (or would point at itself).
    fn connect(&mut self, from: u64, to: u64) {
        if from == to || self.links.iter().any(|l| l.from == from && l.to == to) {
            return;
        }
        let id = self.new_id();
        self.links.push(Link { id, from, to });
    }

    fn remove_link(&mut self, id: u64) {
        self.links.retain(|l| l.id != id);
        if self.selected_link == Some(id) {
            self.selected_link = None;
        }
        if self.menu.is_some_and(|(target, _)| target == Target::Link(id)) {
            self.menu = None;
        }
    }

    fn clear(&mut self) {
        self.items.clear();
        self.links.clear();
        self.selected.clear();
        (self.selected_link, self.drag, self.menu, self.connecting, self.editing, self.controls_hover) = (None, None, None, None, None, None);
    }

    /// Save under the current board's name, or ask for one (always, with `save_as`).
    fn save(&mut self, ctx: &egui::Context, save_as: bool) {
        match &self.name {
            Some(name) if !save_as => self.write(ctx, name.clone()),
            _ => {
                let name = self.name.clone().unwrap_or_else(|| self.unused_name());
                self.dialog = Some(Dialog::SaveAs { name, focus: true });
            }
        }
    }

    /// "Board N" with the lowest N that isn't taken in the boards folder.
    fn unused_name(&self) -> String {
        let taken: Vec<String> = save::list(&self.settings.boards_dir).into_iter().map(|e| e.name.to_lowercase()).collect();
        (1..).map(|n| format!("Board {n}")).find(|name| !taken.contains(&name.to_lowercase())).unwrap_or_default()
    }

    /// Write the board to `<boards folder>/<name>.kinetiqa`.
    fn write(&mut self, ctx: &egui::Context, name: String) {
        let file = save::path_for(&self.settings.boards_dir, &name);
        let items = self
            .items
            .iter()
            .map(|item| {
                let (path, shape) = match &item.content {
                    Content::Media { path, .. } => (Some(path.clone()), None),
                    Content::Shape(s) => {
                        let shape = save::ShapeFile { kind: s.kind, text: s.text.clone(), color: [s.color.r(), s.color.g(), s.color.b()] };
                        (None, Some(shape))
                    }
                };
                save::ItemFile { path, shape, pos: item.pos.into(), size: item.size.into() }
            })
            .collect();
        // Links refer to items by their index in the saved list (ids aren't kept across saves).
        let index: HashMap<u64, usize> = self.items.iter().enumerate().map(|(i, item)| (item.id, i)).collect();
        let links = self
            .links
            .iter()
            .filter_map(|l| Some(save::LinkFile { from: *index.get(&l.from)?, to: *index.get(&l.to)? }))
            .collect();
        match save::write(&file, save::BoardFile::new(self.pan.into(), self.zoom, items, links)) {
            Ok(()) => self.set_name(ctx, name),
            Err(e) => self.errors.push(format!("Couldn't save {}: {e}", file.display())),
        }
    }

    /// Replace the current board with the one saved in `file`.
    fn open(&mut self, ctx: &egui::Context, file: &Path) {
        let board = match save::read(file) {
            Ok(board) => board,
            Err(e) => {
                self.errors.push(format!("Couldn't open {}: {e}", file.display()));
                return;
            }
        };
        self.generation += 1;
        self.clear();
        self.errors.clear();
        self.zoom_to = None;
        self.pan = board.pan.into();
        self.zoom = if board.zoom.is_finite() && board.zoom > 0.0 { board.zoom } else { 1.0 };
        let mut ids = Vec::with_capacity(board.items.len());
        for item in board.items {
            let id = self.new_id();
            ids.push(id);
            let (pos, size) = (Pos2::from(item.pos), Vec2::from(item.size).max(Vec2::splat(MIN_EDGE)));
            if let Some(s) = item.shape {
                // Shapes need no loading. Pushed in saved order, so media finishing later still
                // slots in at the right depth (below any higher id).
                let [r, g, b] = s.color;
                let shape = shapes::Shape { kind: s.kind, text: s.text, color: Color32::from_rgb(r, g, b) };
                self.items.push(Item { id, content: Content::Shape(shape), pos, size });
            } else if let Some(path) = item.path {
                self.spawn_load(ctx, path, Place::Exact { id, pos, size });
            }
        }
        for link in board.links {
            if let (Some(&from), Some(&to)) = (ids.get(link.from), ids.get(link.to)) {
                self.connect(from, to);
            }
        }
        self.set_name(ctx, file.file_stem().unwrap_or_default().to_string_lossy().into_owned());
    }

    fn set_name(&mut self, ctx: &egui::Context, name: String) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{name} — Kinetiqa")));
        self.name = Some(name);
    }

    fn set_boards_dir(&mut self, dir: PathBuf) {
        self.settings.boards_dir = dir;
        if let Err(e) = self.settings.save() {
            self.errors.push(format!("Couldn't save settings: {e}"));
        }
    }

    /// Items still loading aren't in `items` yet, so saving then would leave them out.
    fn can_save(&self) -> bool {
        self.pending == 0
    }

    fn remove(&mut self, id: u64) {
        self.items.retain(|item| item.id != id);
        self.links.retain(|l| l.from != id && l.to != id);
        self.selected.retain(|&s| s != id);
        if self.menu.is_some_and(|(target, _)| target == Target::Item(id)) {
            self.menu = None;
        }
        if self.editing.is_some_and(|(e, _)| e == id) {
            self.editing = None;
        }
    }

    /// Change the zoom while keeping the board point under `anchor` (screen space) fixed on
    /// screen. `center` is the canvas center. There's no zoom limit; only values that would
    /// break the coordinate math (zero, infinite, NaN) are ignored.
    fn zoom_around(&mut self, center: Pos2, anchor: Pos2, zoom: f32) {
        if !(zoom.is_finite() && zoom > 0.0) {
            return;
        }
        let board_point = (anchor - center - self.pan) / self.zoom;
        self.pan = anchor - center - board_point * zoom;
        self.zoom = zoom;
    }

    /// Open / Save / Save as shortcuts. Handled every frame rather than in `toolbar()`, which
    /// doesn't run while the toolbar is hidden.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = egui::Modifiers::COMMAND;
        let open_key = egui::KeyboardShortcut::new(cmd, Key::O);
        let save_key = egui::KeyboardShortcut::new(cmd, Key::S);
        let save_as_key = egui::KeyboardShortcut::new(cmd | egui::Modifiers::SHIFT, Key::S);
        // Check the longer shortcut first: consuming Ctrl+S would also match Ctrl+Shift+S.
        let save_as_pressed = ctx.input_mut(|i| i.consume_shortcut(&save_as_key));
        let save_pressed = ctx.input_mut(|i| i.consume_shortcut(&save_key));
        let open_pressed = ctx.input_mut(|i| i.consume_shortcut(&open_key));
        if open_pressed {
            self.dialog = Some(Dialog::Open(save::list(&self.settings.boards_dir)));
        }
        if self.can_save() && (save_pressed || save_as_pressed) {
            self.save(ctx, save_as_pressed);
        }
    }

    /// The toolbar overlays the top of the board and slides in only when the pointer is near the
    /// top edge (or something needs it visible). It's an overlay rather than a panel so showing
    /// it doesn't resize the canvas — which would shift the whole board, since `pan` is measured
    /// from the canvas center.
    fn toolbar_overlay(&mut self, ui: &egui::Ui) {
        let ctx = ui.ctx();
        let screen = ui.max_rect();
        let pointer_y = ctx.input(|i| i.pointer.hover_pos()).map(|p| p.y - screen.top());
        // Once shown, it stays while the pointer is anywhere over it.
        let reveal = if self.toolbar_visible { self.toolbar_height } else { TOOLBAR_REVEAL };
        let show = pointer_y.is_some_and(|y| y <= reveal)
            || egui::Popup::is_any_open(ctx)
            || self.dialog.is_some()
            || !self.errors.is_empty()
            || self.pending > 0;
        self.toolbar_visible = show;
        let t = ctx.animate_bool_with_time(egui::Id::unique("toolbar-shown"), show, 0.15);

        if t < 1.0 {
            // A small handle at the top edge hints that the toolbar is up there.
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("toolbar-hint")));
            let handle = Rect::from_center_size(Pos2::new(screen.center().x, screen.top() + 5.0), Vec2::new(44.0, 4.0));
            painter.rect_filled(handle, 2.0, theme::TEXT_DIM.gamma_multiply(0.45 * (1.0 - t)));
        }
        if t <= 0.0 {
            return;
        }
        let pos = screen.left_top() - Vec2::new(0.0, (1.0 - t) * self.toolbar_height);
        let area = egui::Area::new(egui::Id::unique("toolbar"))
            .order(egui::Order::Foreground)
            .constrain(false)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(Margin::symmetric(12, 9))
                    .shadow(egui::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha((90.0 * t) as u8) })
                    .show(ui, |ui| {
                        ui.set_width(screen.width() - 24.0);
                        self.toolbar(ui);
                    });
            });
        self.toolbar_height = area.response.rect.height();
    }

    /// Toolbar adds go to the center of the visible board.
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            brand(ui);
            ui.add_space(10.0);

            let all_media: Vec<&str> = IMAGE_EXTENSIONS.iter().chain(VIDEO_EXTENSIONS).copied().collect();
            if primary_button(ui, "+  Add media").on_hover_text("Add images, GIFs or videos at the view center").clicked()
                && let Some(paths) = rfd::FileDialog::new()
                    .add_filter("Images & videos", &all_media)
                    .add_filter("Images", IMAGE_EXTENSIONS)
                    .add_filter("Videos", VIDEO_EXTENSIONS)
                    .pick_files()
                {
                    self.load(ui.ctx(), paths, self.view_center);
                }
            let mut add = None;
            ui.menu_button("Shapes", |ui| {
                ui.label(RichText::new("FLOWCHART SHAPES").size(10.5).color(theme::TEXT_DIM));
                ui.add_space(2.0);
                egui::Grid::new("shape-palette").num_columns(2).spacing([4.0, 2.0]).show(ui, |ui| {
                    for (i, kind) in ShapeKind::ALL.into_iter().enumerate() {
                        if shape_entry(ui, kind).clicked() {
                            add = Some(kind);
                            ui.close();
                        }
                        if i % 2 == 1 {
                            ui.end_row();
                        }
                    }
                });
                ui.add_space(4.0);
                ui.label(RichText::new("Drag an item's → handle onto another to connect them").size(11.5).color(theme::TEXT_DIM));
            })
            .response
            .on_hover_text("Add flowchart shapes at the view center");
            if let Some(kind) = add {
                self.add_shape(kind, self.view_center);
            }
            let has_selection = !self.selected.is_empty() || self.selected_link.is_some();
            if ui.add_enabled(has_selection, egui::Button::new("🗑  Remove")).on_hover_text("Remove the selected items or arrow (Del)").clicked() {
                self.remove_selected();
            }
            if ui.add_enabled(!self.items.is_empty(), egui::Button::new("Clear")).on_hover_text("Remove everything from the board").clicked() {
                self.clear();
            }

            ui.separator();
            if ui.button("Open…").on_hover_text("Open a saved board (Ctrl+O)").clicked() {
                self.dialog = Some(Dialog::Open(save::list(&self.settings.boards_dir)));
            }
            let save_hint = if self.can_save() { "Save the board (Ctrl+S) · Ctrl+Shift+S to save under a new name" } else { "Wait for media to finish loading" };
            let save = ui.add_enabled(self.can_save(), egui::Button::new("Save")).on_hover_text(save_hint).on_disabled_hover_text(save_hint);
            if self.can_save() && save.clicked() {
                self.save(ui.ctx(), false);
            }

            ui.separator();
            // Zoom control: − / current % (click resets to 100%) / +.
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let square = Vec2::new(28.0, 0.0);
                if ui.add(egui::Button::new("−").min_size(square)).on_hover_text("Zoom out").clicked() {
                    self.zoom_to = Some(self.zoom / ZOOM_STEP);
                }
                let label = RichText::new(format!("{:.0}%", self.zoom * 100.0)).monospace();
                if ui.add(egui::Button::new(label).min_size(Vec2::new(58.0, 0.0))).on_hover_text("Scroll to zoom · click to reset to 100%").clicked() {
                    self.zoom_to = Some(1.0);
                }
                if ui.add(egui::Button::new("+").min_size(square)).on_hover_text("Zoom in").clicked() {
                    self.zoom_to = Some(self.zoom * ZOOM_STEP);
                }
            });

            if self.pending > 0 {
                ui.add_space(6.0);
                ui.spinner();
                ui.label(RichText::new(format!("Loading {}…", self.pending)).color(theme::TEXT_DIM));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(RichText::new("⚙").size(15.0)).on_hover_text("Settings").clicked() {
                    self.dialog = Some(Dialog::Settings);
                }
                ui.separator();
                let count = self.items.len();
                ui.label(RichText::new(format!("{count} item{}", if count == 1 { "" } else { "s" })).color(theme::TEXT_DIM));
                // The hint is the first thing to go when the window is narrow, so it never
                // overlaps the buttons.
                let hint = RichText::new("Drag to move · edges to resize · double-click a shape to type · right-click for options")
                    .size(12.0)
                    .color(theme::TEXT_DIM.gamma_multiply(0.8));
                let hint = egui::WidgetText::from(hint).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body);
                if ui.available_width() > hint.size().x + 40.0 {
                    ui.separator();
                    ui.label(hint);
                }
            });
        });
        if !self.errors.is_empty() {
            ui.add_space(6.0);
            egui::Frame::new()
                .fill(theme::DANGER.gamma_multiply(0.12))
                .stroke(Stroke::new(1.0, theme::DANGER.gamma_multiply(0.4)))
                .corner_radius(7)
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(theme::DANGER, format!("⚠  {}", self.errors.join("; ")));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Dismiss").clicked() {
                                self.errors.clear();
                            }
                        });
                    });
                });
        }
    }

    /// The open dialog, if any (Escape or a click outside closes it).
    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.dialog else { return };
        let dir = self.settings.boards_dir.clone();
        let current = self.name.clone();
        let mut action = None;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::unique("dialog"))
            .frame(egui::Frame::popup(&ctx.global_style()).inner_margin(Margin::same(18)).corner_radius(10))
            .show(ctx, |ui| {
                ui.set_width(400.0);
                let folder_note = |ui: &mut egui::Ui, text: &str| {
                    ui.label(RichText::new(format!("{text} {}", dir.display())).size(12.0).color(theme::TEXT_DIM));
                };
                match dialog {
                    Dialog::SaveAs { name, focus } => {
                        ui.label(RichText::new("Save board").size(17.0).strong().color(theme::TEXT));
                        folder_note(ui, "In");
                        ui.add_space(10.0);
                        let edit = ui.add(egui::TextEdit::singleline(name).hint_text("Board name").desired_width(f32::INFINITY));
                        if std::mem::take(focus) {
                            edit.request_focus();
                        }
                        let checked = save::check_name(name);
                        let replaces = checked
                            .is_ok_and(|n| current.as_deref() != Some(n) && save::path_for(&dir, n).exists());
                        match checked {
                            Err(e) if !name.trim().is_empty() => {
                                ui.label(RichText::new(e).size(12.0).color(theme::DANGER));
                            }
                            Ok(n) if replaces => {
                                ui.label(RichText::new(format!("A board named “{n}” already exists and will be replaced.")).size(12.0).color(theme::DANGER));
                            }
                            _ => {}
                        }
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let label = if replaces { "Replace" } else { "Save" };
                                let confirm = ui.add_enabled_ui(checked.is_ok(), |ui| primary_button(ui, label)).inner.clicked();
                                if ui.button("Cancel").clicked() {
                                    close = true;
                                }
                                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                                if let Ok(n) = checked && (confirm || enter) {
                                    action = Some(DialogAction::Save(n.to_owned()));
                                }
                            });
                        });
                    }
                    Dialog::Open(entries) => {
                        ui.label(RichText::new("Open board").size(17.0).strong().color(theme::TEXT));
                        folder_note(ui, "From");
                        ui.add_space(10.0);
                        if entries.is_empty() {
                            ui.label(RichText::new("No saved boards in this folder yet.").color(theme::TEXT_DIM));
                        } else {
                            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                                let now = SystemTime::now();
                                for entry in entries.iter() {
                                    let when = entry.modified.map(|t| time_ago(now, t)).unwrap_or_default();
                                    let row = egui::Button::new(RichText::new(&entry.name).color(theme::TEXT))
                                        .right_text(RichText::new(when).size(12.0).color(theme::TEXT_DIM))
                                        .selected(current.as_deref() == Some(entry.name.as_str()))
                                        .frame_when_inactive(false)
                                        .min_size(Vec2::new(ui.available_width(), 30.0));
                                    if ui.add(row).clicked() {
                                        action = Some(DialogAction::Open(entry.path.clone()));
                                    }
                                }
                            });
                        }
                        ui.add_space(12.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                        });
                    }
                    Dialog::Settings => {
                        ui.label(RichText::new("Settings").size(17.0).strong().color(theme::TEXT));
                        ui.add_space(12.0);
                        ui.label(RichText::new("Boards folder").strong().color(theme::TEXT));
                        ui.label(RichText::new("Every board is saved to and opened from this folder.").size(12.0).color(theme::TEXT_DIM));
                        ui.add_space(6.0);
                        egui::Frame::new()
                            .fill(theme::SURFACE)
                            .stroke(Stroke::new(1.0, theme::BORDER))
                            .corner_radius(6)
                            .inner_margin(Margin::symmetric(10, 7))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.add(egui::Label::new(RichText::new(dir.display().to_string()).monospace().color(theme::TEXT)).wrap());
                            });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("Change…").on_hover_text("Choose another folder for your boards").clicked() {
                                action = Some(DialogAction::PickFolder);
                            }
                            let is_default = dir == settings::default_boards_dir();
                            if ui.add_enabled(!is_default, egui::Button::new("Use default")).on_hover_text(settings::default_boards_dir().display().to_string()).clicked() {
                                action = Some(DialogAction::ResetFolder);
                            }
                        });
                        ui.add_space(14.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Close").clicked() {
                                close = true;
                            }
                        });
                    }
                }
            });
        if close || modal.should_close() {
            self.dialog = None;
        }
        match action {
            Some(DialogAction::Save(name)) => {
                self.dialog = None;
                self.write(ctx, name);
            }
            Some(DialogAction::Open(file)) => {
                self.dialog = None;
                self.open(ctx, &file);
            }
            Some(DialogAction::PickFolder) => {
                if let Some(dir) = rfd::FileDialog::new().set_directory(&dir).pick_folder() {
                    self.set_boards_dir(dir);
                }
            }
            Some(DialogAction::ResetFolder) => self.set_boards_dir(settings::default_boards_dir()),
            None => {}
        }
    }

    fn board(&mut self, ui: &mut egui::Ui) {
        let (canvas, background) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let painter = ui.painter_at(canvas);
        painter.rect_filled(canvas, CornerRadius::ZERO, theme::CANVAS);

        // Background: drag to pan. Clicks (deselect, or pick an arrow) are handled after the
        // item loop, once this frame's arrow positions are known.
        if background.dragged() {
            self.pan += background.drag_delta();
        }

        // Scroll wheel (or pinch / ctrl+scroll) zooms around the cursor. `rect_contains_pointer`
        // is layer-aware, so scrolling over the right-click menu doesn't zoom the board.
        if ui.rect_contains_pointer(canvas)
            && let Some(cursor) = ui.input(|i| i.pointer.hover_pos())
        {
            let factor = ui.input(|i| i.zoom_delta() * (i.smooth_scroll_delta.y * ZOOM_SPEED).exp());
            if factor != 1.0 {
                self.zoom_around(canvas.center(), cursor, self.zoom * factor);
            }
        }
        if let Some(zoom) = self.zoom_to.take() {
            self.zoom_around(canvas.center(), canvas.center(), zoom);
        }

        draw_dots(&painter, canvas, canvas.size() / 2.0 + self.pan, self.zoom);

        let (pan, zoom) = (self.pan, self.zoom);
        self.view_center = (-pan / zoom).to_pos2();
        let to_screen = |p: Pos2| canvas.center() + pan + p.to_vec2() * zoom;
        // Items moved together by a drag go where the pointer is now, before anything is painted.
        // Moving them here (not in the item loop) keeps the whole group in step in one frame.
        if let Some(drag) = &self.drag
            && !drag.group.is_empty()
            && let Some(pointer) = ui.input(|i| i.pointer.latest_pos())
        {
            let delta = (pointer - drag.start_pointer) / zoom;
            for item in &mut self.items {
                if let Some(&(_, start)) = drag.group.iter().find(|(id, _)| *id == item.id) {
                    item.pos = start + delta;
                }
            }
        }
        // Arrows go below every item, but where they end depends on where items are after this
        // frame's drag — so reserve their spot in the paint order now and fill it in after the loop.
        let links_slot = painter.add(egui::Shape::Noop);
        let mut bring_to_front = None;
        let mut menu_opened = false;
        let now = ui.input(|i| i.time);
        let shift = ui.input(|i| i.modifiers.shift);
        // Ctrl (Cmd on macOS) + click adds items to the selection.
        let multi = ui.input(|i| i.modifiers.command);
        let mut controls_hover = None;
        let mut start_editing = None;
        let mut start_connecting = None;

        for item in &mut self.items {
            let rect = Rect::from_min_size(to_screen(item.pos), item.size * zoom);
            // Grow the hit area slightly so edges can be grabbed from just outside the image.
            let hit = rect.expand(EDGE_GRAB);
            if !hit.intersects(canvas) {
                continue;
            }
            let response = ui.interact(hit, egui::Id::unique(("item", item.id)), Sense::click_and_drag());

            // Resize cursor while hovering an edge (or while resizing).
            // Edges only resize when this is the sole selection; a group just moves.
            let resizable = self.selected.len() <= 1;
            let active_grab = self.drag.as_ref().filter(|d| d.id == item.id).map(|d| d.grab);
            let hover_grab = response.hover_pos().filter(|_| resizable).map(|p| Grab::at(rect, p)).unwrap_or_default();
            let grab_for_cursor = active_grab.unwrap_or(hover_grab);
            if grab_for_cursor.is_resize() && (response.hovered() || response.dragged()) {
                ui.ctx().set_cursor_icon(grab_for_cursor.cursor());
            }

            let in_selection = self.selected.contains(&item.id);
            if response.clicked() && multi {
                // Ctrl+click toggles this item in the selection.
                self.selected_link = None;
                if in_selection {
                    self.selected.retain(|&s| s != item.id);
                } else {
                    self.selected.push(item.id);
                }
                bring_to_front = Some(item.id);
            } else if response.drag_started() || response.clicked() || response.secondary_clicked() {
                // Pressing an item that's already selected keeps the rest of the selection so the
                // group moves together; a plain click selects just this item. Ctrl+drag adds it.
                if multi && response.drag_started() {
                    if !in_selection {
                        self.selected.push(item.id);
                    }
                } else if !in_selection || response.clicked() {
                    self.selected = vec![item.id];
                }
                self.selected_link = None;
                bring_to_front = Some(item.id);
            }
            if response.double_clicked() && item.shape_kind().is_some() {
                start_editing = Some(item.id);
            }
            if response.drag_started()
                && let Some(pointer) = response.interact_pointer_pos()
            {
                let grab = if resizable { Grab::at(rect, pointer) } else { Grab::default() };
                // Moves are applied next frame from `group` (filled in after this loop).
                self.drag = Some(Drag {
                    id: item.id,
                    grab,
                    start_pointer: pointer,
                    start_pos: item.pos,
                    start_size: item.size,
                    group: Vec::new(),
                });
                self.menu = None;
            }
            if response.dragged()
                && let (Some(drag), Some(pointer)) = (self.drag.as_ref(), response.interact_pointer_pos())
                && drag.id == item.id
                && drag.grab.is_resize()
            {
                // Pointer moves in screen space; convert to board units.
                let delta = (pointer - drag.start_pointer) / zoom;
                // Media keeps its aspect ratio; shapes stretch freely unless Shift is held.
                let keep_aspect = item.shape_kind().is_none() || shift;
                (item.pos, item.size) = resized(drag, delta, keep_aspect);
            }
            if response.drag_stopped() {
                self.drag = None;
            }

            if response.secondary_clicked() {
                let at = response.interact_pointer_pos().unwrap_or(rect.center());
                self.menu = Some((Target::Item(item.id), at));
                menu_opened = true;
            }

            // Paint at the position updated by this frame's drag, not the stale one, so the
            // image sticks to the cursor instead of trailing a frame behind.
            let rect = Rect::from_min_size(to_screen(item.pos), item.size * zoom);
            let radius = ITEM_RADIUS * zoom;
            let editing = self.editing.is_some_and(|(id, _)| id == item.id);
            match &mut item.content {
                Content::Media { media, .. } => {
                    media.update();
                    let (texture, next_change) = media.current(now);
                    if let Some(secs) = next_change {
                        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(secs));
                    }
                    painter.add(theme::ITEM_SHADOW.as_shape(rect, radius));
                    egui::Image::new((texture, rect.size())).corner_radius(radius).paint_at(ui, rect);
                }
                Content::Shape(shape) => {
                    shapes::paint(&painter, shape.kind, rect, shape.color, zoom);
                    // While editing, the text field below shows the label instead.
                    if !editing {
                        shapes::paint_text(&painter, shape.kind, rect, &shape.text, zoom);
                    }
                }
            }

            // Selection ring + corner handles, or a faint ring on hover. Drawn right after the
            // item so items above it still cover it.
            let selected = in_selection;
            let hovered = response.hovered() || response.dragged() || self.controls_hover == Some(item.id);
            if selected {
                painter.rect_stroke(rect, radius, Stroke::new(2.0, theme::ACCENT), StrokeKind::Outside);
                for corner in [rect.left_top(), rect.right_top(), rect.left_bottom(), rect.right_bottom()] {
                    painter.circle(corner, 4.5, theme::TEXT, Stroke::new(1.5, theme::ACCENT));
                }
            } else if hovered {
                painter.rect_stroke(rect, radius, Stroke::new(1.0, Color32::from_white_alpha(70)), StrokeKind::Outside);
            }
            if (selected || hovered)
                && let Content::Media { media, .. } = &item.content
                && let Some(badge) = media.badge()
                && rect.width() > 80.0
                && rect.height() > 40.0
            {
                pill(&painter, rect.left_top() + Vec2::splat(8.0), Align2::LEFT_TOP, badge, 10.0, Color32::from_black_alpha(170), theme::TEXT);
            }
            if let Some(player) = item.video()
                && rect.width() >= VIDEO_CONTROLS_MIN.x
                && rect.height() >= VIDEO_CONTROLS_MIN.y
            {
                if selected || hovered {
                    if video_controls(ui, &painter, rect, player, item.id) {
                        controls_hover = Some(item.id);
                    }
                } else if player.is_paused() {
                    draw_paused_badge(&painter, rect.center());
                }
            }

            // Label editor, laid over the label area. Registered after the item, so typing and
            // selecting text win over the item's own clicks/drags.
            if editing && let Content::Shape(shape) = &mut item.content {
                let area = shapes::text_rect(shape.kind, rect, zoom);
                let edit = egui::TextEdit::multiline(&mut shape.text)
                    .id(egui::Id::unique(("shape-text", item.id)))
                    .frame(egui::Frame::NONE)
                    .margin(Margin::ZERO)
                    .align(Align2::CENTER_CENTER)
                    .font(egui::FontId::proportional(shapes::FONT_SIZE * zoom))
                    .text_color(theme::TEXT)
                    .desired_width(area.width())
                    .min_size(area.size());
                let edit = ui.put(area, edit);
                if let Some((_, focus)) = &mut self.editing
                    && std::mem::take(focus)
                {
                    edit.request_focus();
                } else if !edit.has_focus() {
                    // Clicked elsewhere or pressed Escape.
                    self.editing = None;
                }
            }

            // Connect handle just right of the item: drag it onto another item to add an arrow.
            // It touches the item's grab zone so the pointer can travel to it without the item
            // losing hover (which would hide the handle on the way).
            if (selected || hovered)
                && !editing
                && self.connecting.is_none()
                && self.drag.as_ref().is_none_or(|d| d.id != item.id)
                && rect.width() >= 24.0
                && rect.height() >= 16.0
            {
                let center = Pos2::new(rect.right() + EDGE_GRAB + CONNECT_HANDLE / 2.0 - 1.0, rect.center().y);
                let handle = ui
                    .interact(Rect::from_center_size(center, Vec2::splat(CONNECT_HANDLE)), egui::Id::unique(("connect", item.id)), Sense::drag())
                    .on_hover_cursor(egui::CursorIcon::Crosshair)
                    .on_hover_text("Drag onto another item to draw an arrow");
                if handle.hovered() || handle.dragged() {
                    controls_hover = Some(item.id);
                }
                if handle.drag_started() {
                    start_connecting = Some(item.id);
                }
                draw_connect_handle(&painter, center, handle.hovered());
            }
        }
        self.controls_hover = controls_hover;
        // A group drag records where each selected item started (they haven't moved yet this frame).
        if let Some(drag) = self.drag.as_mut()
            && drag.group.is_empty()
            && !drag.grab.is_resize()
        {
            drag.group = self.items.iter().filter(|i| self.selected.contains(&i.id)).map(|i| (i.id, i.pos)).collect();
        }
        if let Some(id) = start_editing {
            self.editing = Some((id, true));
            self.menu = None;
        }
        if let Some(id) = start_connecting {
            self.connecting = Some(id);
            self.menu = None;
        }

        // On-screen rects of all items, after this frame's moves: arrow ends and connect targets.
        let rects: HashMap<u64, (Rect, Option<ShapeKind>)> = self
            .items
            .iter()
            .map(|i| (i.id, (Rect::from_min_size(to_screen(i.pos), i.size * zoom), i.shape_kind())))
            .collect();
        let segments: Vec<(u64, Pos2, Pos2)> = self
            .links
            .iter()
            .filter_map(|l| {
                let (&(a, ka), &(b, kb)) = (rects.get(&l.from)?, rects.get(&l.to)?);
                Some((l.id, shapes::boundary_point(ka, a, b.center()), shapes::boundary_point(kb, b, a.center())))
            })
            .collect();
        // Closest arrow within reach of `p`. Only asked for points on the bare background, so
        // items always win over the arrows beneath them.
        let link_at = |p: Pos2| {
            segments
                .iter()
                .map(|&(id, a, b)| (id, shapes::distance_to_segment(p, a, b)))
                .filter(|&(_, d)| d <= LINK_GRAB)
                .min_by(|x, y| x.1.total_cmp(&y.1))
                .map(|(id, _)| id)
        };
        let hovered_link = if background.dragged() { None } else { background.hover_pos().and_then(link_at) };
        if hovered_link.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if background.clicked() {
            self.selected.clear();
            self.selected_link = background.interact_pointer_pos().and_then(link_at);
        }
        if background.secondary_clicked()
            && let Some(at) = background.interact_pointer_pos()
            && let Some(id) = link_at(at)
        {
            self.selected.clear();
            self.selected_link = Some(id);
            self.menu = Some((Target::Link(id), at));
            menu_opened = true;
        }
        let mut arrows = Vec::new();
        for &(id, a, b) in &segments {
            let (color, width) = if self.selected_link == Some(id) {
                (theme::ACCENT, 2.5)
            } else if hovered_link == Some(id) {
                (theme::TEXT, 2.0)
            } else {
                (theme::TEXT_DIM, 1.75)
            };
            shapes::paint_arrow(&mut arrows, a, b, (width * zoom).clamp(1.0, width * 2.0), color, zoom);
        }
        painter.set(links_slot, egui::Shape::Vec(arrows));

        // New arrow being dragged out of a connect handle: preview it (snapped to the item under
        // the pointer, if any) and create it on release.
        if let Some(from) = self.connecting {
            let pointer = ui.input(|i| i.pointer.latest_pos());
            let target = pointer.and_then(|p| self.items.iter().rev().find(|i| i.id != from && rects[&i.id].0.contains(p)).map(|i| i.id));
            if let (Some(p), Some(&(a, ka))) = (pointer, rects.get(&from)) {
                let (end, toward) = match target.and_then(|t| rects.get(&t)) {
                    Some(&(b, kb)) => {
                        painter.rect_stroke(b, ITEM_RADIUS * zoom, Stroke::new(2.0, theme::ACCENT), StrokeKind::Outside);
                        (shapes::boundary_point(kb, b, a.center()), b.center())
                    }
                    None => (p, p),
                };
                let mut preview = Vec::new();
                shapes::paint_arrow(&mut preview, shapes::boundary_point(ka, a, toward), end, 2.0, theme::ACCENT, zoom);
                painter.extend(preview);
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            let (released, down) = ui.input(|i| (i.pointer.primary_released(), i.pointer.primary_down()));
            if released || !down {
                if released && let Some(to) = target {
                    self.connect(from, to);
                }
                self.connecting = None;
            }
        }

        if let Some(id) = bring_to_front
            && let Some(idx) = self.items.iter().position(|i| i.id == id) {
                let item = self.items.remove(idx);
                self.items.push(item);
            }
        if let Some((target, at)) = self.menu {
            // A shape's current color, read up front so the menu UI doesn't borrow `self`.
            let shape_color = match target {
                Target::Item(id) => self.items.iter().find(|i| i.id == id).and_then(|i| match &i.content {
                    Content::Shape(s) => Some(s.color),
                    Content::Media { .. } => None,
                }),
                Target::Link(_) => None,
            };
            let mut action = None;
            let area = egui::Area::new(egui::Id::unique("image-options"))
                .order(egui::Order::Foreground)
                .fixed_pos(at)
                .show(ui.ctx(), |ui| {
                    egui::Frame::menu(ui.style()).show(ui, |ui| {
                        ui.set_min_width(170.0);
                        let heading = match (target, shape_color) {
                            (Target::Link(_), _) => "ARROW",
                            (_, Some(_)) => "SHAPE",
                            _ => "ITEM",
                        };
                        ui.label(RichText::new(heading).size(10.5).color(theme::TEXT_DIM));
                        ui.add_space(2.0);
                        if let Some(current) = shape_color {
                            if menu_item(ui, RichText::new("Edit text").color(theme::TEXT)).on_hover_text("Or double-click the shape · Enter").clicked() {
                                action = Some(MenuAction::EditText);
                            }
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 3.0;
                                for (name, color) in shapes::PALETTE {
                                    if color_swatch(ui, color, color == current).on_hover_text(name).clicked() {
                                        action = Some(MenuAction::Color(color));
                                    }
                                }
                            });
                            ui.separator();
                        }
                        if matches!(target, Target::Link(_)) && menu_item(ui, RichText::new("Reverse direction").color(theme::TEXT)).clicked() {
                            action = Some(MenuAction::Reverse);
                        }
                        if menu_item(ui, RichText::new("🗑  Remove").color(theme::DANGER)).clicked() {
                            action = Some(MenuAction::Remove);
                        }
                    });
                });
            // Picking a color keeps the menu open, so colors can be tried in a row.
            let close = area.response.clicked_elsewhere()
                || ui.input(|i| i.key_pressed(Key::Escape))
                || matches!(action, Some(MenuAction::EditText | MenuAction::Reverse | MenuAction::Remove));
            // Don't close on the same right-click that opened it.
            if close && !menu_opened {
                self.menu = None;
            }
            // Applied after the menu has run, since its buttons are what pick the action.
            match (action, target) {
                (Some(MenuAction::Remove), Target::Item(id)) => self.remove(id),
                (Some(MenuAction::Remove), Target::Link(id)) => self.remove_link(id),
                (Some(MenuAction::EditText), Target::Item(id)) => {
                    self.select(id);
                    self.editing = Some((id, true));
                }
                (Some(MenuAction::Color(color)), Target::Item(id)) => {
                    if let Some(Item { content: Content::Shape(shape), .. }) = self.items.iter_mut().find(|i| i.id == id) {
                        shape.color = color;
                    }
                }
                (Some(MenuAction::Reverse), Target::Link(id)) => {
                    if let Some(link) = self.links.iter_mut().find(|l| l.id == id) {
                        std::mem::swap(&mut link.from, &mut link.to);
                    }
                }
                _ => {}
            }
        }

        let files_hovered = ui.input(|i| !i.raw.hovered_files.is_empty());
        if self.items.is_empty() && self.pending == 0 && !files_hovered {
            draw_empty_state(&painter, canvas.center());
        }

        // Highlight while files are hovered over the window.
        if files_hovered {
            let area = canvas.shrink(10.0);
            painter.rect_filled(area, 14.0, theme::ACCENT.gamma_multiply(0.06));
            painter.rect_stroke(area, 14.0, Stroke::new(2.0, theme::ACCENT.gamma_multiply(0.8)), StrokeKind::Inside);
            pill(&painter, canvas.center(), Align2::CENTER_CENTER, "Drop to add to the board", 15.0, theme::ACCENT, theme::ON_ACCENT);
        }

        // Files dropped onto the window land where the cursor is (or the center).
        let dropped: Vec<PathBuf> = ui.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            let at = ui.input(|i| i.pointer.hover_pos()).unwrap_or(canvas.center());
            let board_pos = ((at - canvas.center() - self.pan) / self.zoom).to_pos2();
            self.load(ui.ctx(), dropped, board_pos);
        }

        // Zoom level + board coordinates of the view center, bottom-right; click to re-center on the origin.
        // Placed after the items so it's on top of them, both for painting and for clicks.
        let center = self.view_center;
        // `+ 0.0` turns -0.0 into 0.0, so it never reads "-0".
        let label = format!("{:.0}%   x {:.0}   y {:.0}", self.zoom * 100.0, center.x.round() + 0.0, center.y.round() + 0.0);
        let margin = 14.0;
        let size = Vec2::new(200.0, 26.0);
        let coords_rect = Rect::from_min_size(canvas.max - size - Vec2::splat(margin), size);
        let pill_radius = size.y / 2.0;
        painter.add(egui::Shadow { offset: [0, 3], blur: 12, spread: 0, color: Color32::from_black_alpha(110) }.as_shape(coords_rect, pill_radius));
        let coords_button = egui::Button::new(RichText::new(label).monospace().size(12.0).color(theme::TEXT_DIM))
            .fill(theme::SURFACE)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .corner_radius(pill_radius);
        let coords = ui
            .put(coords_rect, coords_button)
            .on_hover_text("Zoom · view center (board coordinates) · click to go back to 0, 0");
        if coords.clicked() {
            self.pan = Vec2::ZERO;
        }

        // Board keys are ignored while a text field (e.g. a shape label) has focus.
        // Keys belong to the dialog while one is open, even when it has no text field.
        let wants_keyboard = ui.ctx().egui_wants_keyboard_input() || self.dialog.is_some();
        // Space plays / pauses the selected video.
        // Space plays / pauses the selected video (only when exactly one item is selected).
        if !wants_keyboard && ui.input(|i| i.key_pressed(Key::Space))
            && let [id] = self.selected[..]
            && let Some(player) = self.items.iter().find(|i| i.id == id).and_then(|i| i.video()) {
                player.set_paused(!player.is_paused());
            }
        // Enter / F2 starts typing into the selected shape (only when exactly one item is selected).
        if !wants_keyboard && ui.input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::F2))
            && let [id] = self.selected[..]
            && let Some(item) = self.items.iter().find(|i| i.id == id)
            && item.shape_kind().is_some()
        {
            self.editing = Some((item.id, true));
        }
        // Delete / Backspace removes the selected items or arrow.
        if !wants_keyboard && ui.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace)) {
            self.remove_selected();
        }
    }
}

impl eframe::App for Board {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.receive(ui.ctx());

        self.shortcuts(ui.ctx());
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.board(ui));
        self.toolbar_overlay(ui);
        self.dialogs(ui.ctx());
    }
}

/// New (pos, size) for an item being resized by `delta` from where the drag started. The edge/
/// corner opposite the grabbed one stays put. With `keep_aspect` (media, or Shift on shapes) the
/// aspect ratio is kept; otherwise each grabbed edge follows the pointer on its own.
fn resized(drag: &Drag, delta: Vec2, keep_aspect: bool) -> (Pos2, Vec2) {
    let Drag { grab, start_pos, start_size, .. } = *drag;
    if !keep_aspect {
        let (mut min, mut max) = (start_pos, start_pos + start_size);
        if grab.left {
            min.x = (min.x + delta.x).min(max.x - MIN_EDGE);
        }
        if grab.right {
            max.x = (max.x + delta.x).max(min.x + MIN_EDGE);
        }
        if grab.top {
            min.y = (min.y + delta.y).min(max.y - MIN_EDGE);
        }
        if grab.bottom {
            max.y = (max.y + delta.y).max(min.y + MIN_EDGE);
        }
        return (min, max - min);
    }
    let dx = if grab.right { delta.x } else if grab.left { -delta.x } else { 0.0 };
    let dy = if grab.bottom { delta.y } else if grab.top { -delta.y } else { 0.0 };
    let sx = (start_size.x + dx) / start_size.x;
    let sy = (start_size.y + dy) / start_size.y;
    // On a corner, follow whichever axis moved more; on an edge, follow that edge's axis.
    let horizontal = grab.left || grab.right;
    let vertical = grab.top || grab.bottom;
    let mut scale = match (horizontal, vertical) {
        (true, true) => if (sx - 1.0).abs() > (sy - 1.0).abs() { sx } else { sy },
        (true, false) => sx,
        _ => sy,
    };
    scale = scale.max(MIN_EDGE / start_size.min_elem());
    let size = start_size * scale;

    // Anchor: grabbed side moves, opposite side stays; untouched axis stays centered.
    let x = if grab.left {
        start_pos.x + start_size.x - size.x
    } else if grab.right {
        start_pos.x
    } else {
        start_pos.x + (start_size.x - size.x) / 2.0
    };
    let y = if grab.top {
        start_pos.y + start_size.y - size.y
    } else if grab.bottom {
        start_pos.y
    } else {
        start_pos.y + (start_size.y - size.y) / 2.0
    };
    (Pos2::new(x, y), size)
}


/// Subtle dot grid that moves with the board and scales with the zoom, with a brighter dot every
/// `MAJOR_EVERY` cells in both directions. `origin` is where board (0, 0) sits relative to the
/// canvas's top-left corner.
fn draw_dots(painter: &egui::Painter, canvas: Rect, origin: Vec2, zoom: f32) {
    const BOARD_SPACING: f32 = 28.0;
    const MAJOR_EVERY: i64 = 4;
    /// Dots closer than this on screen get coarsened, so zooming far out doesn't paint millions.
    const MIN_SCREEN_SPACING: f32 = 7.0;
    // Zoom is unbounded, so step the grid up by MAJOR_EVERY until it's sparse enough; each
    // level's minor dots are the previous level's major ones, so the grid stays board-fixed.
    let mut board_spacing = BOARD_SPACING;
    while board_spacing * zoom < MIN_SCREEN_SPACING {
        board_spacing *= MAJOR_EVERY as f32;
    }
    let spacing = board_spacing * zoom;
    let ox = origin.x.rem_euclid(spacing);
    let oy = origin.y.rem_euclid(spacing);
    // Board-grid index of the first visible column/row, so major dots stay fixed to the board.
    let first_col = ((ox - origin.x) / spacing).round() as i64;
    let mut row = ((oy - origin.y) / spacing).round() as i64;
    let mut y = canvas.top() + oy;
    while y < canvas.bottom() {
        let mut col = first_col;
        let mut x = canvas.left() + ox;
        while x < canvas.right() {
            let major = col.rem_euclid(MAJOR_EVERY) == 0 && row.rem_euclid(MAJOR_EVERY) == 0;
            let (radius, color) = if major { (1.6, theme::DOT_MAJOR) } else { (1.1, theme::DOT) };
            painter.circle_filled(Pos2::new(x, y), radius, color);
            x += spacing;
            col += 1;
        }
        y += spacing;
        row += 1;
    }
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
fn time_ago(now: SystemTime, then: SystemTime) -> String {
    let secs = now.duration_since(then).map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        86400..172800 => "yesterday".into(),
        _ => format!("{} days ago", secs / 86400),
    }
}

/// App mark (two overlapping cards on an accent tile) followed by the name.
fn brand(ui: &mut egui::Ui) {
    let (tile, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(tile, 6.0, theme::ACCENT);
    let at = |x: f32, y: f32| tile.min + Vec2::new(x, y);
    painter.rect_filled(Rect::from_min_max(at(5.0, 5.0), at(13.0, 14.0)), 1.5, theme::ON_ACCENT.gamma_multiply(0.55));
    painter.rect(Rect::from_min_max(at(9.0, 9.0), at(17.0, 17.0)), 1.5, theme::ON_ACCENT, Stroke::new(1.5, theme::ACCENT), StrokeKind::Outside);
    ui.label(RichText::new("Kinetiqa").size(16.0).strong().color(theme::TEXT));
}

/// Accent-filled call-to-action button (hover/press brighten it like a normal button).
fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        for (wv, fill) in [(&mut w.inactive, theme::ACCENT), (&mut w.hovered, theme::ACCENT_HOVER), (&mut w.active, theme::ACCENT_HOVER)] {
            wv.bg_fill = fill;
            wv.weak_bg_fill = fill;
            wv.bg_stroke = Stroke::NONE;
            wv.fg_stroke = Stroke::new(1.0, theme::ON_ACCENT);
        }
        ui.button(RichText::new(text).strong())
    })
    .inner
}

/// A row of the toolbar's Shapes menu: the shape's icon and name.
fn shape_entry(ui: &mut egui::Ui, kind: ShapeKind) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(168.0, 30.0), Sense::click());
    let response = response.on_hover_text(kind.hint());
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, 6.0, theme::SURFACE_HOVER);
    }
    let icon = Rect::from_center_size(rect.left_center() + Vec2::new(22.0, 0.0), Vec2::new(30.0, 18.0));
    if kind == ShapeKind::Text {
        painter.text(icon.center(), Align2::CENTER_CENTER, "Aa", egui::FontId::proportional(14.0), theme::TEXT);
    } else {
        shapes::paint(painter, kind, icon, kind.default_color(), 0.6);
    }
    painter.text(Pos2::new(icon.right() + 12.0, rect.center().y), Align2::LEFT_CENTER, kind.label(), egui::FontId::proportional(13.5), theme::TEXT);
    response
}

/// A full-width, frameless button for the right-click menu.
fn menu_item(ui: &mut egui::Ui, text: RichText) -> egui::Response {
    ui.add(egui::Button::new(text).frame_when_inactive(false).min_size(Vec2::new(ui.available_width(), 0.0)))
}

/// A round color button; `selected` gets a ring.
fn color_swatch(ui: &mut egui::Ui, color: Color32, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
    let painter = ui.painter();
    painter.circle_filled(rect.center(), 6.5, color);
    if selected {
        painter.circle_stroke(rect.center(), 8.5, Stroke::new(1.5, theme::TEXT));
    } else if response.hovered() {
        painter.circle_stroke(rect.center(), 8.5, Stroke::new(1.0, Color32::from_white_alpha(90)));
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The round "→" handle beside an item that's dragged out to make an arrow.
fn draw_connect_handle(painter: &egui::Painter, center: Pos2, hovered: bool) {
    let fill = if hovered { theme::ACCENT } else { theme::SURFACE };
    let ink = if hovered { theme::ON_ACCENT } else { theme::ACCENT };
    painter.circle(center, CONNECT_HANDLE / 2.0 - 1.0, fill, Stroke::new(1.5, theme::ACCENT));
    let stroke = Stroke::new(1.6, ink);
    let (l, r) = (center - Vec2::new(4.0, 0.0), center + Vec2::new(4.0, 0.0));
    painter.line_segment([l, r], stroke);
    painter.line_segment([r, r + Vec2::new(-3.0, -3.0)], stroke);
    painter.line_segment([r, r + Vec2::new(-3.0, 3.0)], stroke);
}

/// Play/pause button and timeline in a pill along the bottom of a video item (`rect`, screen
/// space). Kept at a fixed on-screen size regardless of zoom so it stays usable. Returns whether
/// the pointer is on the controls (or they're being used).
fn video_controls(ui: &egui::Ui, painter: &egui::Painter, rect: Rect, player: &VideoPlayer, id: u64) -> bool {
    const HEIGHT: f32 = 30.0;
    const MARGIN: f32 = 8.0;
    let bar = Rect::from_min_max(
        Pos2::new(rect.left() + MARGIN, rect.bottom() - MARGIN - HEIGHT),
        rect.right_bottom() - Vec2::splat(MARGIN),
    );
    // Swallows presses/drags on the gaps between controls, so they don't move the item.
    let background = ui.interact(bar, egui::Id::unique(("video-bar", id)), Sense::click_and_drag());
    painter.rect_filled(bar, HEIGHT / 2.0, Color32::from_black_alpha(175));

    // Play / pause.
    let paused = player.is_paused();
    let button = Rect::from_center_size(Pos2::new(bar.left() + HEIGHT / 2.0 + 1.0, bar.center().y), Vec2::splat(HEIGHT - 6.0));
    let play = ui.interact(button, egui::Id::unique(("video-play", id)), Sense::click());
    let play = play.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(if paused { "Play (Space)" } else { "Pause (Space)" });
    if play.clicked() {
        player.set_paused(!paused);
    }
    if play.hovered() {
        painter.circle_filled(button.center(), button.width() / 2.0, Color32::from_white_alpha(28));
    }
    let icon_color = if play.hovered() { theme::TEXT } else { theme::TEXT.gamma_multiply(0.85) };
    draw_play_pause(painter, button.center(), 5.5, paused, icon_color);

    // Elapsed / total time, when there's room for it.
    let position = player.position();
    let duration = player.duration();
    let mut track_right = bar.right() - 14.0;
    if let Some(total) = duration
        && bar.width() > 220.0
    {
        let text = format!("{} / {}", format_time(position.min(total)), format_time(total));
        let galley = painter.layout_no_wrap(text, egui::FontId::monospace(10.5), theme::TEXT_DIM);
        let at = Pos2::new(bar.right() - 12.0 - galley.size().x, bar.center().y - galley.size().y / 2.0);
        track_right = at.x - 10.0;
        painter.galley(at, galley, theme::TEXT_DIM);
    }

    // Timeline: click or drag to seek. Without a known duration it can't map x to time.
    let mut active = background.hovered() || background.dragged() || play.hovered();
    if let Some(total) = duration {
        let track = Rect::from_min_max(Pos2::new(button.right() + 8.0, bar.center().y - 2.0), Pos2::new(track_right, bar.center().y + 2.0));
        if track.width() > 10.0 {
            let hit = track.expand2(Vec2::new(6.0, 8.0));
            let seek = ui.interact(hit, egui::Id::unique(("video-seek", id)), Sense::click_and_drag());
            let seek = seek.on_hover_cursor(egui::CursorIcon::PointingHand);
            let pressed = seek.is_pointer_button_down_on() && ui.input(|i| i.pointer.any_pressed() || i.pointer.delta().x != 0.0);
            if (pressed || seek.clicked())
                && let Some(pointer) = seek.interact_pointer_pos()
            {
                let t = ((pointer.x - track.left()) / track.width()).clamp(0.0, 1.0);
                player.seek(t as f64 * total);
            }
            let using = seek.hovered() || seek.dragged();
            active |= using;

            let fraction = (player.position() / total).clamp(0.0, 1.0) as f32;
            let head = Pos2::new(track.left() + track.width() * fraction, track.center().y);
            let track = if using { track.expand2(Vec2::new(0.0, 1.0)) } else { track };
            painter.rect_filled(track, track.height() / 2.0, Color32::from_white_alpha(55));
            painter.rect_filled(Rect::from_min_max(track.min, Pos2::new(head.x, track.max.y)), track.height() / 2.0, theme::ACCENT);
            if using {
                painter.circle(head, 6.0, theme::TEXT, Stroke::new(1.5, theme::ACCENT));
            }
        }
    }
    active
}

/// Round "paused" marker in the middle of a video item that isn't showing its controls.
fn draw_paused_badge(painter: &egui::Painter, center: Pos2) {
    painter.circle_filled(center, 20.0, Color32::from_black_alpha(150));
    draw_play_pause(painter, center, 8.0, true, theme::TEXT);
}

/// A play triangle (`play`) or pause bars centered on `center`, roughly `2 * size` tall.
fn draw_play_pause(painter: &egui::Painter, center: Pos2, size: f32, play: bool, color: Color32) {
    if play {
        // Nudged right so the triangle looks optically centered.
        let c = center + Vec2::new(size * 0.15, 0.0);
        let points = vec![c + Vec2::new(-size * 0.75, -size), c + Vec2::new(size, 0.0), c + Vec2::new(-size * 0.75, size)];
        painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
    } else {
        let bar = Vec2::new(size * 0.55, size * 2.0);
        for dx in [-size * 0.5, size * 0.5] {
            painter.rect_filled(Rect::from_center_size(center + Vec2::new(dx, 0.0), bar), 1.0, color);
        }
    }
}

/// `m:ss`, or `h:mm:ss` from an hour up.
fn format_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// Rounded label with a solid background, positioned by `align` relative to `anchor`.
fn pill(painter: &egui::Painter, anchor: Pos2, align: Align2, text: &str, font_size: f32, fill: Color32, color: Color32) -> Rect {
    let galley = painter.layout_no_wrap(text.to_owned(), egui::FontId::proportional(font_size), color);
    let padding = Vec2::new(font_size * 0.8, font_size * 0.4);
    let rect = align.anchor_size(anchor, galley.size() + padding * 2.0);
    painter.rect_filled(rect, rect.height() / 2.0, fill);
    painter.galley(rect.min + padding, galley, color);
    rect
}

/// Empty-board hint: an accent "+" badge with a title and a short how-to.
fn draw_empty_state(painter: &egui::Painter, center: Pos2) {
    let icon = center - Vec2::new(0.0, 46.0);
    painter.circle_filled(icon, 32.0, theme::ACCENT.gamma_multiply(0.12));
    painter.circle_stroke(icon, 32.0, Stroke::new(1.5, theme::ACCENT.gamma_multiply(0.55)));
    let plus = Stroke::new(2.5, theme::ACCENT);
    painter.line_segment([icon - Vec2::new(11.0, 0.0), icon + Vec2::new(11.0, 0.0)], plus);
    painter.line_segment([icon - Vec2::new(0.0, 11.0), icon + Vec2::new(0.0, 11.0)], plus);
    painter.text(center + Vec2::new(0.0, 8.0), Align2::CENTER_CENTER, "Start your board", egui::FontId::proportional(22.0), theme::TEXT);
    painter.text(
        center + Vec2::new(0.0, 36.0),
        Align2::CENTER_CENTER,
        "Drop images, GIFs or videos anywhere, or use “Add media” and “Shapes”",
        egui::FontId::proportional(14.0),
        theme::TEXT_DIM,
    );
}

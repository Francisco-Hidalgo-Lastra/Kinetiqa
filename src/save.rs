//! Board files: a small JSON document listing each item (a media file reference or a shape) with
//! its position and size (in z-order), the arrows between items, and the view. Media isn't
//! embedded — boards reference the original files.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::shapes::ShapeKind;

/// File extension for saved boards.
pub const EXTENSION: &str = "kinetiqa";
/// Bumped when the format changes incompatibly. 2 added shapes and arrows (version 1 files
/// still read fine; older apps refuse version 2 with a clear message instead of a parse error).
const VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
pub struct BoardFile {
    pub version: u32,
    pub pan: [f32; 2],
    pub zoom: f32,
    /// Bottom-most first, like `Board::items`.
    pub items: Vec<ItemFile>,
    /// Arrows between items.
    #[serde(default)]
    pub links: Vec<LinkFile>,
}

#[derive(Serialize, Deserialize)]
pub struct ItemFile {
    /// Media items: relative to the board file's folder when the media lives inside it (so a
    /// folder holding a board and its media can be moved together), absolute otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Shape items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<ShapeFile>,
    /// Top-left corner, board coordinates.
    pub pos: [f32; 2],
    pub size: [f32; 2],
}

#[derive(Serialize, Deserialize)]
pub struct ShapeFile {
    pub kind: ShapeKind,
    #[serde(default)]
    pub text: String,
    /// Outline color, RGB.
    pub color: [u8; 3],
}

/// An arrow from one item to another, as indices into `BoardFile::items`.
#[derive(Serialize, Deserialize)]
pub struct LinkFile {
    pub from: usize,
    pub to: usize,
    /// Label shown at the arrow's midpoint. Optional, so older version 2 files still load.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

impl BoardFile {
    pub fn new(pan: [f32; 2], zoom: f32, items: Vec<ItemFile>, links: Vec<LinkFile>) -> Self {
        Self { version: VERSION, pan, zoom, items, links }
    }
}

pub fn write(file: &Path, mut board: BoardFile) -> Result<(), String> {
    if let Some(dir) = file.parent() {
        for item in &mut board.items {
            if let Some(path) = &mut item.path
                && let Ok(rel) = path.strip_prefix(dir)
            {
                *path = rel.to_path_buf();
            }
        }
    }
    let json = serde_json::to_string_pretty(&board).map_err(|e| e.to_string())?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(file, json).map_err(|e| e.to_string())
}

/// Read a board, with item paths resolved to absolute ones.
pub fn read(file: &Path) -> Result<BoardFile, String> {
    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    let mut board: BoardFile = serde_json::from_str(&text).map_err(|e| format!("not a valid board file ({e})"))?;
    if board.version > VERSION {
        return Err(format!("made by a newer version of Kinetiqa (format {})", board.version));
    }
    if let Some(dir) = file.parent() {
        for item in &mut board.items {
            if let Some(path) = &mut item.path
                && path.is_relative()
            {
                *path = dir.join(&*path);
            }
        }
    }
    Ok(board)
}

/// A board file in the boards folder.
pub struct Entry {
    /// File name without the extension.
    pub name: String,
    pub path: PathBuf,
    pub modified: Option<SystemTime>,
}

/// Boards in `dir`, most recently modified first. A missing folder just has no boards.
pub fn list(dir: &Path) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut entries: Vec<Entry> = read
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION)) || !path.is_file() {
                return None;
            }
            let name = path.file_stem()?.to_string_lossy().into_owned();
            let modified = entry.metadata().and_then(|m| m.modified()).ok();
            Some(Entry { name, path, modified })
        })
        .collect();
    entries.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.name.cmp(&b.name)));
    entries
}

/// The file a board named `name` is stored in.
pub fn path_for(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.{EXTENSION}"))
}

/// A board name that's safe to use as a file name on every platform, or why it isn't.
pub fn check_name(name: &str) -> Result<&str, &'static str> {
    let name = name.trim();
    if name.is_empty() {
        Err("Enter a name")
    } else if name.starts_with('.') {
        Err("Names can't start with a dot")
    } else if name.chars().any(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()) {
        Err("Names can't contain / \\ : * ? \" < > |")
    } else {
        Ok(name)
    }
}

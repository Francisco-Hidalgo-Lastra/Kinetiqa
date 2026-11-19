//! Loading and playing board media: still images, animated GIFs and (via ffmpeg) videos.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, ColorImage, TextureHandle, TextureId, TextureOptions};
use image::{AnimationDecoder, DynamicImage};

/// Largest edge (in pixels) a texture is kept at; bigger images are downscaled on load.
const MAX_TEXTURE_EDGE: u32 = 2048;
/// Largest edge videos are decoded at. Lower than images since every frame is re-uploaded.
const MAX_VIDEO_EDGE: u32 = 1280;
/// GIF frame delays at or below this (seconds) are replaced by `DEFAULT_GIF_DELAY`, like
/// browsers do — many GIFs declare 0–10 ms and expect to play at ~10 fps.
const MIN_GIF_DELAY: f64 = 0.011;
const DEFAULT_GIF_DELAY: f64 = 0.1;
/// Frame rate every video is decoded and played at, whatever the source's rate: ffmpeg's `fps`
/// filter duplicates frames of slower sources and drops frames of faster ones.
const VIDEO_FPS: f64 = 60.0;
/// If playback falls further behind than this (seconds), e.g. after a slow seek, the clock is
/// re-anchored instead of rushing through frames to catch up.
const MAX_VIDEO_LAG: f64 = 0.25;

/// Extensions decoded with the `image` crate (each needs its feature enabled in Cargo.toml).
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp"];
/// Extensions handed to ffmpeg.
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mov", "webm", "mkv", "avi", // common containers
    "mpg", "mpeg", "mpe", "m1v", "m2v", "mpv", "vob", "ts", "m2ts", "mts", // MPEG-1/2 program/elementary/transport streams
];

/// Media decoded on a loader thread, turned into a [`Media`] (GPU textures) on the UI thread.
pub enum Loaded {
    Still(ColorImage),
    /// Frames with their display duration in seconds.
    Animated(Vec<(ColorImage, f64)>),
    Video { first: ColorImage, player: VideoPlayer },
}

impl Loaded {
    pub fn size(&self) -> [usize; 2] {
        match self {
            Self::Still(image) | Self::Video { first: image, .. } => image.size,
            Self::Animated(frames) => frames[0].0.size,
        }
    }

    pub fn upload(self, ctx: &egui::Context, name: &str) -> Media {
        let load = |i: usize, image: ColorImage| ctx.load_texture(format!("{name}#{i}"), image, TextureOptions::LINEAR);
        match self {
            Self::Still(image) => Media::Still(load(0, image)),
            Self::Animated(frames) => {
                let total = frames.iter().map(|(_, delay)| delay).sum();
                let frames = frames.into_iter().enumerate().map(|(i, (image, delay))| (load(i, image), delay)).collect();
                Media::Animated { frames, total }
            }
            Self::Video { first, player } => Media::Video { texture: load(0, first), player },
        }
    }
}

pub enum Media {
    Still(TextureHandle),
    /// Frames with their display duration in seconds; `total` is the loop length.
    Animated { frames: Vec<(TextureHandle, f64)>, total: f64 },
    /// One texture overwritten with each new frame from the player.
    Video { texture: TextureHandle, player: VideoPlayer },
}

impl Media {
    /// Short label for the board's media-type badge; stills don't get one.
    pub fn badge(&self) -> Option<&'static str> {
        match self {
            Self::Still(_) => None,
            Self::Animated { .. } => Some("GIF"),
            Self::Video { .. } => Some("▶ VIDEO"),
        }
    }

    pub fn video(&self) -> Option<&VideoPlayer> {
        match self {
            Self::Video { player, .. } => Some(player),
            _ => None,
        }
    }

    /// Upload the newest video frame, if one arrived. Only called for visible items, so
    /// off-screen videos don't cost texture uploads.
    pub fn update(&mut self) {
        if let Self::Video { texture, player } = self
            && let Some(frame) = player.state().latest.take()
        {
            texture.set(frame, TextureOptions::LINEAR);
        }
    }

    /// Texture to draw at time `now` (seconds), and how long until it changes, so the caller
    /// can schedule a repaint. Videos return `None`: their player thread requests repaints.
    pub fn current(&self, now: f64) -> (TextureId, Option<f64>) {
        match self {
            Self::Still(texture) | Self::Video { texture, .. } => (texture.id(), None),
            Self::Animated { frames, total } => {
                let mut t = now.rem_euclid(*total);
                for (texture, delay) in frames {
                    if t < *delay {
                        return (texture.id(), Some(delay - t));
                    }
                    t -= delay;
                }
                (frames[frames.len() - 1].0.id(), Some(0.0))
            }
        }
    }
}

/// A looping, muted video that can be paused and seeked. A player thread reads frames from
/// ffmpeg, paces them at `VIDEO_FPS` and keeps the most recent one in the shared
/// state; seeking restarts ffmpeg at the new position. Dropping the player kills ffmpeg and
/// ends that thread.
pub struct VideoPlayer {
    shared: Arc<(Mutex<PlayerState>, Condvar)>,
    /// Length in seconds, if ffprobe knows it.
    duration: Option<f64>,
}

struct PlayerState {
    /// Newest frame not yet uploaded to the texture.
    latest: Option<ColorImage>,
    /// Video time (seconds) of the newest frame, or of a requested seek.
    position: f64,
    paused: bool,
    /// Seek requested by the UI; the player thread takes it. Only the newest one matters, so
    /// dragging along the timeline doesn't queue up ffmpeg restarts.
    seek: Option<f64>,
    stop: bool,
    /// The current ffmpeg process. Kept here (not in the player thread) so `Drop` can kill it
    /// even while the thread is blocked reading its output.
    child: Child,
}

impl VideoPlayer {
    fn state(&self) -> std::sync::MutexGuard<'_, PlayerState> {
        self.shared.0.lock().unwrap()
    }

    pub fn duration(&self) -> Option<f64> {
        self.duration
    }

    pub fn position(&self) -> f64 {
        self.state().position
    }

    pub fn is_paused(&self) -> bool {
        self.state().paused
    }

    pub fn set_paused(&self, paused: bool) {
        self.state().paused = paused;
        self.shared.1.notify_all();
    }

    /// Jump to `secs` (clamped to the video). Works while paused too: the frame there is shown.
    pub fn seek(&self, secs: f64) {
        let secs = secs.clamp(0.0, self.duration.unwrap_or(f64::INFINITY));
        let mut state = self.state();
        state.seek = Some(secs);
        state.position = secs;
        drop(state);
        self.shared.1.notify_all();
    }
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        let mut state = self.state();
        state.stop = true;
        let _ = state.child.kill();
        let _ = state.child.wait();
        drop(state);
        self.shared.1.notify_all();
    }
}

/// Decode `path` (blocking — call from a loader thread). `ctx` is used by video players to
/// request repaints as frames arrive.
pub fn decode(path: &Path, ctx: &egui::Context) -> Result<Loaded, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
        open_video(path, ctx)
    } else if ext == "gif" {
        decode_gif(path)
    } else {
        decode_still(path)
    }
}

fn decode_still(path: &Path) -> Result<Loaded, String> {
    let img = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())?;
    Ok(Loaded::Still(to_color_image(img)))
}

fn decode_gif(path: &Path) -> Result<Loaded, String> {
    let file = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    let decoder = image::codecs::gif::GifDecoder::new(file).map_err(|e| e.to_string())?;
    let mut frames: Vec<(ColorImage, f64)> = decoder
        .into_frames()
        .map(|frame| {
            let frame = frame.map_err(|e| e.to_string())?;
            let (num, den) = frame.delay().numer_denom_ms();
            let delay = num as f64 / den.max(1) as f64 / 1000.0;
            let delay = if delay <= MIN_GIF_DELAY { DEFAULT_GIF_DELAY } else { delay };
            Ok((to_color_image(DynamicImage::ImageRgba8(frame.into_buffer())), delay))
        })
        .collect::<Result<_, String>>()?;
    match frames.len() {
        0 => Err("GIF has no frames".into()),
        1 => Ok(Loaded::Still(frames.pop().unwrap().0)),
        _ => Ok(Loaded::Animated(frames)),
    }
}

fn to_color_image(img: DynamicImage) -> ColorImage {
    let img = if img.width() > MAX_TEXTURE_EDGE || img.height() > MAX_TEXTURE_EDGE {
        img.thumbnail(MAX_TEXTURE_EDGE, MAX_TEXTURE_EDGE)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    ColorImage::from_rgba_unmultiplied(size, rgba.as_flat_samples().as_slice())
}

fn ffmpeg_error(tool: &str, e: std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        format!("{tool} not found — install ffmpeg to open videos")
    } else {
        format!("{tool}: {e}")
    }
}

/// What `probe` learns about a video.
struct VideoInfo {
    /// Display size, with rotation metadata applied.
    width: u32,
    height: u32,
    duration: Option<f64>,
}

/// Size and duration of the first video stream.
fn probe(path: &Path) -> Result<VideoInfo, String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg("stream=width,height,sample_aspect_ratio:stream_side_data=rotation:format=duration")
        .args(["-of", "default=nw=1", "-i"])
        .arg(path)
        .output()
        .map_err(|e| ffmpeg_error("ffprobe", e))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let raw = |key: &str| text.lines().find_map(|line| Some(line.strip_prefix(key)?.strip_prefix('=')?.trim()));
    let field = |key: &str| raw(key)?.parse::<f64>().ok();
    let (Some(w), Some(h)) = (field("width"), field("height")) else {
        return Err("no video stream found".into());
    };
    // MPEG-2 (DVD, broadcast) often stores non-square pixels, e.g. 720x480 at 32:27 for 16:9.
    // Stretch the width so the board shows the intended display aspect ratio.
    let sar = raw("sample_aspect_ratio")
        .and_then(|r| r.split_once(':'))
        .and_then(|(n, d)| Some(n.parse::<f64>().ok()? / d.parse::<f64>().ok()?))
        .filter(|r| r.is_finite() && *r > 0.0)
        .unwrap_or(1.0);
    let w = (w * sar).round();
    // Phones store portrait video as landscape plus a rotation tag; ffmpeg applies the
    // rotation when decoding, so the output frames have the swapped size.
    let quarter_turn = field("rotation").is_some_and(|r| (r.abs().round() as i64 / 90) % 2 == 1);
    let (width, height) = if quarter_turn { (h as u32, w as u32) } else { (w as u32, h as u32) };
    let duration = field("duration").filter(|d| d.is_finite() && *d > 0.0);
    Ok(VideoInfo { width, height, duration })
}

/// How to (re)start ffmpeg for one video.
struct Decoder {
    path: std::path::PathBuf,
    size: [usize; 2],
}

impl Decoder {
    /// Spawn ffmpeg streaming raw RGBA frames from `start` seconds to the end. No `-re`: the
    /// player thread does the pacing, which is what lets it pause (ffmpeg just blocks on the
    /// full pipe). The `fps` filter makes the output constant-rate, so frame N is at N / VIDEO_FPS.
    fn spawn(&self, start: f64) -> Result<(Child, ChildStdout), String> {
        let [w, h] = self.size;
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
        if start > 0.0 {
            cmd.args(["-ss", &format!("{start:.3}")]);
        }
        let mut child = cmd
            .arg("-i")
            .arg(&self.path)
            .args(["-an", "-sn", "-dn", "-vf", &format!("fps={VIDEO_FPS},scale={w}:{h}")])
            .args(["-pix_fmt", "rgba", "-f", "rawvideo", "pipe:1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            // Not piped: an unread stderr pipe could fill up and stall ffmpeg.
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| ffmpeg_error("ffmpeg", e))?;
        let stdout = child.stdout.take().expect("stdout is piped");
        Ok((child, stdout))
    }

    fn frame(&self, buf: &[u8]) -> ColorImage {
        ColorImage::from_rgba_unmultiplied(self.size, buf)
    }
}

fn open_video(path: &Path, ctx: &egui::Context) -> Result<Loaded, String> {
    let info = probe(path)?;
    let scale = (MAX_VIDEO_EDGE as f32 / info.width.max(info.height) as f32).min(1.0);
    // Even dimensions keep ffmpeg's scaler happy with chroma-subsampled sources.
    let even = |v: u32| ((v as f32 * scale) as u32 / 2 * 2).max(2) as usize;
    let decoder = Decoder { path: path.to_path_buf(), size: [even(info.width), even(info.height)] };

    // Read the first frame here so a broken file fails the load instead of showing nothing.
    let (mut child, mut stdout) = decoder.spawn(0.0)?;
    let mut buf = vec![0u8; decoder.size[0] * decoder.size[1] * 4];
    if stdout.read_exact(&mut buf).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err("ffmpeg couldn't decode the video".into());
    }
    let first = decoder.frame(&buf);

    let state = PlayerState { latest: None, position: 0.0, paused: false, seek: None, stop: false, child };
    let shared = Arc::new((Mutex::new(state), Condvar::new()));
    let thread_shared = Arc::clone(&shared);
    let ctx = ctx.clone();
    std::thread::spawn(move || play(&thread_shared, &decoder, stdout, buf, &ctx));
    Ok(Loaded::Video { first, player: VideoPlayer { shared, duration: info.duration } })
}

/// Player thread: publishes frames at `VIDEO_FPS`, handles pause/seek, and loops at
/// the end. Frame 0 has already been read and shown by `open_video`.
fn play(shared: &(Mutex<PlayerState>, Condvar), decoder: &Decoder, mut stdout: ChildStdout, mut buf: Vec<u8>, ctx: &egui::Context) {
    let (lock, wake) = shared;
    let frame_secs = 1.0 / VIDEO_FPS;
    // Video time of the current ffmpeg process's first frame, and frames read from it so far.
    let mut start = 0.0;
    let mut read: u64 = 1;
    // Wall-clock instant at which video time `anchor.1` was (or is due to be) on screen.
    let mut anchor = (Instant::now(), 0.0);

    // Replace ffmpeg with one starting at `at`. `None` when stopping or ffmpeg won't start.
    let restart = |at: f64| -> Option<ChildStdout> {
        let mut state = lock.lock().unwrap();
        if state.stop {
            return None;
        }
        let _ = state.child.kill();
        let _ = state.child.wait();
        let (child, stdout) = decoder.spawn(at).ok()?;
        state.child = child;
        Some(stdout)
    };
    let publish = |frame: ColorImage, position: f64| {
        let mut state = lock.lock().unwrap();
        state.latest = Some(frame);
        // Don't overwrite the target of a seek that hasn't been handled yet.
        if state.seek.is_none() {
            state.position = position;
        }
        drop(state);
        ctx.request_repaint();
    };

    loop {
        let due = start + read as f64 * frame_secs;
        // Wait until the next frame is due, handling stop / seek / pause meanwhile.
        let mut seek = None;
        {
            let mut state = lock.lock().unwrap();
            let mut was_paused = false;
            loop {
                if state.stop {
                    return;
                }
                if let Some(at) = state.seek.take() {
                    seek = Some(at);
                    break;
                }
                if state.paused {
                    was_paused = true;
                    state = wake.wait(state).unwrap();
                    continue;
                }
                let now = Instant::now();
                if was_paused {
                    anchor = (now, due);
                    was_paused = false;
                }
                let deadline = anchor.0 + Duration::from_secs_f64((due - anchor.1).max(0.0));
                if now >= deadline {
                    if (now - deadline).as_secs_f64() > MAX_VIDEO_LAG {
                        anchor = (now, due);
                    }
                    break;
                }
                state = wake.wait_timeout(state, deadline - now).unwrap().0;
            }
        }

        if let Some(at) = seek {
            let Some(new) = restart(at) else { return };
            stdout = new;
            // Show the frame at the seek target right away, even when paused.
            if stdout.read_exact(&mut buf).is_ok() {
                publish(decoder.frame(&buf), at);
                (start, read, anchor) = (at, 1, (Instant::now(), at));
            } else {
                // Seeked past the last frame: start over.
                let Some(new) = restart(0.0) else { return };
                stdout = new;
                (start, read, anchor) = (0.0, 0, (Instant::now(), 0.0));
            }
            continue;
        }

        if stdout.read_exact(&mut buf).is_ok() {
            publish(decoder.frame(&buf), due);
            read += 1;
        } else if read == 0 {
            // A fresh ffmpeg produced nothing; give up rather than respawn it in a tight loop.
            return;
        } else {
            // End of the video (or ffmpeg killed by `Drop`, which `restart` notices): loop.
            let Some(new) = restart(0.0) else { return };
            stdout = new;
            (start, read, anchor) = (0.0, 0, (Instant::now(), 0.0));
        }
    }
}

//! Clipboard history storage and the background thread that watches for changes.
//!
//! Only the index (per-entry metadata) is held in memory. The payloads, text or
//! PNG, are read from disk on demand, so the preview and the copy button touch
//! one file at a time.

use arboard::{Clipboard, ImageData};
use eframe::egui;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemKind {
    Text,
    Image,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipItem {
    pub id: u64,
    pub kind: ItemKind,
    pub timestamp: u64,
    pub size_bytes: u64,
    pub hash: u64,
    /// First ~200 characters, kept around so the list renders without disk reads.
    pub snippet: Option<String>,
    /// Image size in pixels.
    pub image_dims: Option<(u32, u32)>,
    /// Pinned entries are never evicted by the history limit.
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StoredIndex {
    next_id: u64,
    items: Vec<ClipItem>,
}

pub struct HistoryStore {
    dir: PathBuf,
    next_id: u64,
    items: Vec<ClipItem>, // newest first
}

impl HistoryStore {
    pub fn open(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        let index_path = dir.join("index.json");
        let (next_id, items) = std::fs::read_to_string(&index_path)
            .ok()
            .and_then(|t| serde_json::from_str::<StoredIndex>(&t).ok())
            .map(|idx| (idx.next_id.max(1), idx.items))
            .unwrap_or((1, Vec::new()));
        Self {
            dir,
            next_id,
            items,
        }
    }

    pub fn items(&self) -> &[ClipItem] {
        &self.items
    }

    pub fn get(&self, id: u64) -> Option<&ClipItem> {
        self.items.iter().find(|it| it.id == id)
    }

    fn payload_path(&self, id: u64, ext: &str) -> PathBuf {
        self.dir.join(format!("{id:010}.{ext}"))
    }

    fn has_hash(&self, hash: u64) -> bool {
        self.items.iter().any(|it| it.hash == hash)
    }

    pub fn save_index(&self) {
        let path = self.dir.join("index.json");
        let payload = StoredIndex {
            next_id: self.next_id,
            items: self.items.clone(),
        };
        if let Ok(text) = serde_json::to_string_pretty(&payload) {
            let _ = std::fs::write(path, text);
        }
    }

    pub fn add_text(&mut self, text: &str, hash: u64) -> Option<u64> {
        if text.is_empty() || self.has_hash(hash) {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        let path = self.payload_path(id, "txt");
        std::fs::write(&path, text).ok()?;
        let snippet: String = text.chars().take(200).collect();
        self.items.insert(
            0,
            ClipItem {
                id,
                kind: ItemKind::Text,
                timestamp: now_secs(),
                size_bytes: text.len() as u64,
                hash,
                snippet: Some(snippet),
                image_dims: None,
                pinned: false,
            },
        );
        Some(id)
    }

    pub fn add_image(&mut self, image: &ImageData, hash: u64) -> Option<u64> {
        if image.width == 0 || image.height == 0 || self.has_hash(hash) {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        let path = self.payload_path(id, "png");
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(
                &image.bytes,
                image.width as u32,
                image.height as u32,
                ExtendedColorType::Rgba8,
            )
            .ok()?;
        std::fs::write(&path, &png).ok()?;
        let size = std::fs::metadata(&path)
            .map(|m| m.len())
            .unwrap_or(png.len() as u64);
        self.items.insert(
            0,
            ClipItem {
                id,
                kind: ItemKind::Image,
                timestamp: now_secs(),
                size_bytes: size,
                hash,
                snippet: None,
                image_dims: Some((image.width as u32, image.height as u32)),
                pinned: false,
            },
        );
        Some(id)
    }

    pub fn load_text(&self, id: u64) -> Option<String> {
        std::fs::read_to_string(self.payload_path(id, "txt")).ok()
    }

    /// RGBA bytes plus width and height, for previewing or copying an image.
    pub fn load_image_rgba(&self, id: u64) -> Option<(Vec<u8>, u32, u32)> {
        let img = image::open(self.payload_path(id, "png")).ok()?.into_rgba8();
        let (w, h) = img.dimensions();
        Some((img.into_raw(), w, h))
    }

    pub fn delete(&mut self, id: u64) {
        let Some(pos) = self.items.iter().position(|it| it.id == id) else {
            return;
        };
        let it = self.items.remove(pos);
        let _ = std::fs::remove_file(self.payload_path(id, ext_for(it.kind)));
    }

    /// Trims the history down to the limit, oldest first. Pinned entries are
    /// skipped; they stay until unpinned or deleted by hand.
    pub fn trim_to(&mut self, max_items: usize) {
        while self.items.len() > max_items {
            let Some(pos) = self.items.iter().rposition(|it| !it.pinned) else {
                break;
            };
            let it = self.items.remove(pos);
            let _ = std::fs::remove_file(self.payload_path(it.id, ext_for(it.kind)));
        }
    }

    pub fn set_pinned(&mut self, id: u64, pinned: bool) {
        if let Some(it) = self.items.iter_mut().find(|it| it.id == id) {
            it.pinned = pinned;
        }
    }
}

fn ext_for(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Text => "txt",
        ItemKind::Image => "png",
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn format_ago(ts: u64) -> String {
    let d = now_secs().saturating_sub(ts);
    if d < 60 {
        format!("{d} с назад")
    } else if d < 3600 {
        format!("{} мин назад", d / 60)
    } else if d < 86400 {
        format!("{} ч назад", d / 3600)
    } else {
        format!("{} дн назад", d / 86400)
    }
}

pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ---------- background watcher ----------

/// State shared between the UI and the watcher thread. `last_seen_hash` is
/// bumped both by our own `set_*` calls and by polling, which is what stops the
/// app from re-capturing what it just put on the clipboard. The rest are knobs
/// the settings window can turn while the thread is running.
pub struct WatcherHandle {
    pub last_seen_hash: Arc<AtomicU64>,
    pub max_items: Arc<AtomicUsize>,
    /// Capture paused: clipboard contents stop going into the history.
    pub paused: Arc<AtomicBool>,
    pub capture_text: Arc<AtomicBool>,
    pub capture_images: Arc<AtomicBool>,
    /// Poll interval in ms. The thread re-reads it every cycle.
    pub poll_ms: Arc<AtomicU64>,
}

pub fn spawn_watcher(
    store: Arc<Mutex<HistoryStore>>,
    ctx: egui::Context,
    poll_ms: u64,
    max_items: usize,
) -> WatcherHandle {
    let handle = WatcherHandle {
        last_seen_hash: Arc::new(AtomicU64::new(0)),
        max_items: Arc::new(AtomicUsize::new(max_items)),
        paused: Arc::new(AtomicBool::new(false)),
        capture_text: Arc::new(AtomicBool::new(true)),
        capture_images: Arc::new(AtomicBool::new(true)),
        poll_ms: Arc::new(AtomicU64::new(poll_ms.max(50))),
    };

    let lsh = handle.last_seen_hash.clone();
    let max_flag = handle.max_items.clone();
    let paused = handle.paused.clone();
    let want_text = handle.capture_text.clone();
    let want_images = handle.capture_images.clone();
    let poll = handle.poll_ms.clone();
    thread::spawn(move || {
        let mut clipboard = match Clipboard::new() {
            Ok(c) => c,
            Err(_) => return,
        };
        loop {
            thread::sleep(Duration::from_millis(poll.load(Ordering::Relaxed).max(50)));
            if paused.load(Ordering::Relaxed) {
                continue;
            }
            let added = try_capture_once(
                &mut clipboard,
                &store,
                &lsh,
                max_flag.load(Ordering::Relaxed),
                want_text.load(Ordering::Relaxed),
                want_images.load(Ordering::Relaxed),
            );
            if added {
                ctx.request_repaint();
            }
        }
    });
    handle
}

fn try_capture_once(
    clipboard: &mut Clipboard,
    store: &Mutex<HistoryStore>,
    last_seen_hash: &AtomicU64,
    max_items: usize,
    want_text: bool,
    want_images: bool,
) -> bool {
    // Text wins over images, so a Win+Shift+S screenshot can't shadow it.
    if let Some(text) = clipboard.get_text().ok().filter(|_| want_text) {
        if text.is_empty() {
            return false;
        }
        let h = fnv1a(text.as_bytes());
        if h == last_seen_hash.load(Ordering::Relaxed) {
            return false;
        }
        last_seen_hash.store(h, Ordering::Relaxed);
        let mut s = match store.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        let added = s.add_text(&text, h).is_some();
        if added {
            s.trim_to(max_items);
            s.save_index();
        }
        return added;
    }
    if let Some(img) = clipboard.get_image().ok().filter(|_| want_images) {
        let h = fnv1a(&img.bytes);
        if h == last_seen_hash.load(Ordering::Relaxed) {
            return false;
        }
        last_seen_hash.store(h, Ordering::Relaxed);
        let mut s = match store.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        let added = s.add_image(&img, h).is_some();
        if added {
            s.trim_to(max_items);
            s.save_index();
        }
        return added;
    }
    false
}

pub fn copy_text_to_clipboard(text: &str, last_seen_hash: &AtomicU64) -> bool {
    let Ok(mut cb) = Clipboard::new() else {
        return false;
    };
    if cb.set_text(text).is_err() {
        return false;
    }
    last_seen_hash.store(fnv1a(text.as_bytes()), Ordering::Relaxed);
    true
}

pub fn copy_image_to_clipboard(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    last_seen_hash: &AtomicU64,
) -> bool {
    let Ok(mut cb) = Clipboard::new() else {
        return false;
    };
    let hash = fnv1a(&rgba);
    let img = ImageData {
        width: width as usize,
        height: height as usize,
        bytes: Cow::Owned(rgba),
    };
    if cb.set_image(img).is_err() {
        return false;
    }
    last_seen_hash.store(hash, Ordering::Relaxed);
    true
}

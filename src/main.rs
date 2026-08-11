#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{self, Color32, CornerRadius, RichText};
use serde::{Deserialize, Serialize};
use serde::de::DeserializeOwned;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon as TrayImgIcon, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

mod autostart;
mod buffer;

use buffer::{
    copy_image_to_clipboard, copy_text_to_clipboard, format_ago, spawn_watcher, ClipItem, HistoryStore,
    ItemKind, WatcherHandle,
};

const ACCENT: Color32 = Color32::PURPLE;
const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");
const APP_TITLE: &str = "CopyCat";

// ---------- сохранение данных ----------

/// Папка, в которой лежит запущенный exe — конфиг хранится рядом с `.exe`,
fn base_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn settings_path() -> PathBuf {
    base_dir().join("settings.json")
}

/// Загружает JSON-файл, создавая его с содержимым `default`, если файла нет.
fn load_json<T: Serialize + DeserializeOwned>(path: &Path, default: T) -> T {
    if !path.exists() {
        let text = serde_json::to_string_pretty(&default).expect("serialize default config");
        std::fs::write(path, text).expect("write default config");
        return default;
    }
    let text = std::fs::read_to_string(path).unwrap_or_else(|exc| panic!("failed to read {}: {exc}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|exc| panic!("failed to parse {}: {exc}", path.display()))
}

/// Сохраняет значение как отформатированный JSON
fn save_json<T: Serialize>(path: &Path, value: &T) {
    if let Ok(text) = serde_json::to_string_pretty(value) {
        let _ = std::fs::write(path, text);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct AppSettings {
    /// Максимум записей истории; лишние (самые старые) удаляются с диска.
    max_items: usize,
    /// Как часто фоновой поток опрашивает системный буфер обмена.
    poll_interval_ms: u64,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self { max_items: 100, poll_interval_ms: 500 }
    }
}

// ---------- иконка ----------

fn decode_icon() -> (Vec<u8>, u32, u32) {
    let image = image::load_from_memory(ICON_PNG).expect("decode embedded icon").into_rgba8();
    let (width, height) = image.dimensions();
    (image.into_raw(), width, height)
}

// ---------- тема оформления ----------

/// Тёмная тема со скруглёнными углами и одним акцентным цветом.
fn build_visuals(accent: Color32) -> egui::Visuals {
    let mut visuals = egui::Visuals::dark();

    visuals.panel_fill = Color32::from_rgb(0x1e, 0x21, 0x28);
    visuals.window_fill = Color32::from_rgb(0x24, 0x28, 0x30);
    visuals.extreme_bg_color = Color32::from_rgb(0x17, 0x19, 0x1f);
    visuals.faint_bg_color = Color32::from_rgb(0x2a, 0x2e, 0x37);
    visuals.hyperlink_color = accent;
    visuals.selection.bg_fill = accent.linear_multiply(0.55);
    visuals.window_corner_radius = CornerRadius::same(12);
    visuals.menu_corner_radius = CornerRadius::same(8);

    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = CornerRadius::same(8);
    }
    visuals.widgets.hovered.bg_fill = accent.linear_multiply(0.35);
    visuals.widgets.active.bg_fill = accent.linear_multiply(0.55);

    visuals
}

// ---------- приложение ----------

fn main() -> eframe::Result<()> {
    let (rgba, width, height) = decode_icon();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_TITLE)
            .with_inner_size([760.0, 480.0])
            .with_min_inner_size([520.0, 320.0])
            .with_icon(egui::IconData { rgba, width, height }),
        ..Default::default()
    };
    eframe::run_native("schrodingerbox-copycat", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

struct App {
    settings: AppSettings,
    icon_texture: egui::TextureHandle,
    _tray: TrayIcon,
    show_requested: Arc<AtomicBool>,
    quit_requested: Arc<AtomicBool>,
    allow_close: bool,
    store: Arc<Mutex<HistoryStore>>,
    watcher: WatcherHandle,
    selected: Option<u64>,
    /// Отмеченные чекбоксами записи для массового удаления.
    checked: HashSet<u64>,
    /// Кэш превью для выбранного элемента: id → (текст ИЛИ RGBA-текстура).
    preview: PreviewCache,
    /// Черновик значения «Хранить записей»: пока пользователь печатает,
    /// в настройки ничего не пишем — иначе промежуточные «1», «10» тут же
    /// подрезали бы историю.
    max_items_input: String,
    autostart_enabled: bool,
}

#[derive(Default)]
struct PreviewCache {
    id: Option<u64>,
    text: Option<String>,
    image: Option<(egui::TextureHandle, u32, u32)>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(build_visuals(ACCENT));

        let (rgba, width, height) = decode_icon();
        let color_image = egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba);
        let icon_texture = cc.egui_ctx.load_texture("app_icon", color_image, egui::TextureOptions::LINEAR);

        let show_requested = Arc::new(AtomicBool::new(false));
        let quit_requested = Arc::new(AtomicBool::new(false));
        let tray = build_tray(&cc.egui_ctx, rgba, width, height, show_requested.clone(), quit_requested.clone());

        let settings: AppSettings = load_json(&settings_path(), AppSettings::default());
        let store = Arc::new(Mutex::new(HistoryStore::open(base_dir().join("clipboard"))));
        let watcher = spawn_watcher(store.clone(), cc.egui_ctx.clone(), settings.poll_interval_ms, settings.max_items);

        let max_items_input = settings.max_items.to_string();
        Self {
            settings,
            icon_texture,
            _tray: tray,
            show_requested,
            quit_requested,
            allow_close: false,
            store,
            watcher,
            selected: None,
            checked: HashSet::new(),
            preview: PreviewCache::default(),
            max_items_input,
            autostart_enabled: autostart::is_enabled(),
        }
    }

    fn set_max_items(&mut self, value: usize) {
        let value = value.clamp(1, 10_000);
        if value == self.settings.max_items {
            return;
        }
        self.settings.max_items = value;
        self.max_items_input = value.to_string();
        self.apply_max_items();
        self.save_settings();
    }

    fn delete_ids(&mut self, ids: &[u64]) {
        if ids.is_empty() {
            return;
        }
        if let Ok(mut s) = self.store.lock() {
            for id in ids {
                s.delete(*id);
            }
            s.save_index();
        }
        for id in ids {
            self.checked.remove(id);
            if self.selected == Some(*id) {
                self.selected = None;
                self.preview = PreviewCache::default();
            }
        }
    }

    fn save_settings(&self) {
        save_json(&settings_path(), &self.settings);
    }

    fn show_window(ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// Синхронизирует лимит истории с watcher-ом и подрезает уже накопленное.
    fn apply_max_items(&mut self) {
        self.watcher.max_items.store(self.settings.max_items, Ordering::Relaxed);
        if let Ok(mut s) = self.store.lock() {
            s.trim_to(self.settings.max_items);
            s.save_index();
        }
    }

    /// Ленивая загрузка превью для выбранного элемента; повторный `ui`-кадр
    /// не декодирует картинку заново.
    fn ensure_preview(&mut self, ctx: &egui::Context, item: &ClipItem) {
        if self.preview.id == Some(item.id) {
            return;
        }
        self.preview = PreviewCache { id: Some(item.id), text: None, image: None };
        let store = self.store.lock().expect("store lock");
        match item.kind {
            ItemKind::Text => {
                self.preview.text = store.load_text(item.id);
            }
            ItemKind::Image => {
                if let Some((rgba, w, h)) = store.load_image_rgba(item.id) {
                    let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                    let tex = ctx.load_texture(format!("preview_{}", item.id), img, egui::TextureOptions::LINEAR);
                    self.preview.image = Some((tex, w, h));
                }
            }
        }
    }
}

fn build_tray(
    ctx: &egui::Context,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    show_requested: Arc<AtomicBool>,
    quit_requested: Arc<AtomicBool>,
) -> TrayIcon {
    let icon = TrayImgIcon::from_rgba(rgba, width, height).expect("build tray icon image");

    let menu = Menu::new();
    let show_item = MenuItem::new("Показать окно", true, None);
    let quit_item = MenuItem::new("Выход", true, None);
    let show_id = show_item.id().clone();
    let quit_id = quit_item.id().clone();
    menu.append(&show_item).expect("append show item");
    menu.append(&PredefinedMenuItem::separator()).expect("append separator");
    menu.append(&quit_item).expect("append quit item");

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(APP_TITLE)
        .with_icon(icon)
        .build()
        .expect("build tray icon");

    let ctx_menu = ctx.clone();
    let show_from_menu = show_requested.clone();
    let quit_from_menu = quit_requested.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == show_id {
            show_from_menu.store(true, Ordering::Relaxed);
        } else if event.id == quit_id {
            quit_from_menu.store(true, Ordering::Relaxed);
        }
        ctx_menu.request_repaint();
    }));

    let ctx_tray = ctx.clone();
    let show_from_tray = show_requested;
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if matches!(event, TrayIconEvent::DoubleClick { .. }) {
            show_from_tray.store(true, Ordering::Relaxed);
        }
        ctx_tray.request_repaint();
    }));

    tray
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.show_requested.swap(false, Ordering::Relaxed) {
            Self::show_window(ctx);
        }
        if self.quit_requested.swap(false, Ordering::Relaxed) {
            self.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // ---------- top bar с настройками ----------
        let mut delete_all_checked = false;
        egui::Panel::top("top_bar")
            .frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin::symmetric(16, 10)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Image::new((self.icon_texture.id(), egui::vec2(22.0, 22.0))));
                    ui.add_space(8.0);
                    ui.heading(RichText::new(APP_TITLE).strong());

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Меню настроек — прячем сюда всё, что мешало в топ-баре
                        // и убирает горизонтальный курсор от DragValue.
                        ui.menu_button("⚙", |ui| {
                            ui.set_min_width(240.0);
                            ui.label(RichText::new("Настройки").strong());
                            ui.separator();

                            ui.horizontal(|ui| {
                                ui.label("Хранить записей:");
                                let resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.max_items_input)
                                        .desired_width(64.0)
                                        .horizontal_align(egui::Align::Center),
                                );
                                // Применяем по Enter или когда пользователь
                                // ушёл из поля; мусорный ввод откатываем.
                                if resp.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                    match self.max_items_input.trim().parse::<usize>() {
                                        Ok(n) if n >= 1 => self.set_max_items(n),
                                        _ => self.max_items_input = self.settings.max_items.to_string(),
                                    }
                                }
                            });
                            ui.label(
                                RichText::new("от 1 до 10 000 · Enter чтобы применить")
                                    .small()
                                    .color(ui.visuals().weak_text_color()),
                            );

                            #[cfg(windows)]
                            {
                                let mut on = self.autostart_enabled;
                                if ui.checkbox(&mut on, "Автозапуск при старте Windows").changed() {
                                    match autostart::set(on) {
                                        Ok(()) => self.autostart_enabled = on,
                                        Err(_) => self.autostart_enabled = autostart::is_enabled(),
                                    }
                                }
                            }
                        });

                        let count = self.store.lock().map(|s| s.items().len()).unwrap_or(0);
                        ui.label(RichText::new(format!("{count} шт.")).color(ui.visuals().weak_text_color()));

                        if !self.checked.is_empty() {
                            let label = format!("Удалить выбранные ({})", self.checked.len());
                            if ui.button(RichText::new(label).color(Color32::WHITE)).clicked() {
                                delete_all_checked = true;
                            }
                            if ui.button("Снять выделение").clicked() {
                                self.checked.clear();
                            }
                        }
                    });
                });
            });

        // ---------- готовим данные под панели ----------
        let items: Vec<ClipItem> = self.store.lock().map(|s| s.items().to_vec()).unwrap_or_default();
        if self.selected.map_or(false, |id| !items.iter().any(|it| it.id == id)) {
            self.selected = None;
            self.preview = PreviewCache::default();
        }

        // ---------- список слева ----------
        let mut requested_copy: Option<u64> = None;
        let mut requested_delete: Option<u64> = None;
        egui::Panel::left("list_panel")
            .resizable(true)
            .default_size(280.0)
            .size_range(200.0..=380.0)
            .frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin::same(8)))
            .show(ui, |ui| {
                if items.is_empty() {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("История пуста").color(ui.visuals().weak_text_color()));
                        ui.label(RichText::new("Скопируйте что-нибудь (Ctrl+C, Win+Shift+S) — появится здесь.")
                            .color(ui.visuals().weak_text_color()).small());
                    });
                    return;
                }
                ui.horizontal(|ui| {
                    let all_checked = !items.is_empty() && items.iter().all(|it| self.checked.contains(&it.id));
                    let mut toggle = all_checked;
                    if ui.checkbox(&mut toggle, "Выбрать все").changed() {
                        if toggle {
                            self.checked = items.iter().map(|it| it.id).collect();
                        } else {
                            self.checked.clear();
                        }
                    }
                });
                ui.separator();

                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for item in &items {
                        let selected = self.selected == Some(item.id);
                        let mut checked = self.checked.contains(&item.id);
                        let response = draw_list_row(ui, item, selected, &mut checked);
                        if checked {
                            self.checked.insert(item.id);
                        } else {
                            self.checked.remove(&item.id);
                        }
                        if response.row_clicked {
                            self.selected = Some(item.id);
                        }
                        if response.copy_clicked {
                            requested_copy = Some(item.id);
                        }
                        if response.delete_clicked {
                            requested_delete = Some(item.id);
                        }
                    }
                });
            });

        // ---------- превью ----------
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ui.visuals().extreme_bg_color).inner_margin(egui::Margin::same(16)))
            .show(ui, |ui| {
                let Some(id) = self.selected else {
                    ui.vertical_centered(|ui| {
                        ui.add_space(24.0);
                        ui.label(RichText::new("Выберите запись слева").color(ui.visuals().weak_text_color()));
                    });
                    return;
                };
                let Some(item) = items.iter().find(|it| it.id == id).cloned() else { return };
                self.ensure_preview(&ctx, &item);

                ui.horizontal(|ui| {
                    let title = match item.kind {
                        ItemKind::Text => format!("Текст · {} B", item.size_bytes),
                        ItemKind::Image => match item.image_dims {
                            Some((w, h)) => format!("Картинка · {w}×{h} · {} КБ", item.size_bytes / 1024),
                            None => "Картинка".to_string(),
                        },
                    };
                    ui.strong(title);
                    ui.add_space(12.0);
                    ui.label(RichText::new(format_ago(item.timestamp)).color(ui.visuals().weak_text_color()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Удалить").clicked() {
                            requested_delete = Some(item.id);
                        }
                        if ui.button(RichText::new("Копировать").color(Color32::WHITE)).clicked() {
                            requested_copy = Some(item.id);
                        }
                    });
                });
                ui.separator();

                match item.kind {
                    ItemKind::Text => {
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                            if let Some(text) = &self.preview.text {
                                ui.add(egui::Label::new(text).wrap());
                            } else {
                                ui.label(RichText::new("(не удалось прочитать файл)").color(ui.visuals().weak_text_color()));
                            }
                        });
                    }
                    ItemKind::Image => {
                        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                            if let Some((tex, w, h)) = &self.preview.image {
                                let avail = ui.available_size();
                                let scale = (avail.x / *w as f32).min(avail.y / *h as f32).min(1.0);
                                let size = egui::vec2(*w as f32 * scale, *h as f32 * scale);
                                ui.add(egui::Image::new((tex.id(), size)));
                            } else {
                                ui.label(RichText::new("(не удалось декодировать PNG)").color(ui.visuals().weak_text_color()));
                            }
                        });
                    }
                }
            });

        // ---------- отложенные действия из UI-цикла ----------
        if let Some(id) = requested_copy {
            self.copy_item(id);
        }
        if let Some(id) = requested_delete {
            self.delete_ids(&[id]);
        }
        if delete_all_checked {
            let ids: Vec<u64> = self.checked.iter().copied().collect();
            self.delete_ids(&ids);
        }
        // Delete удаляет отмеченные, а если их нет — просто выбранную запись.
        if ctx.input(|i| i.key_pressed(egui::Key::Delete)) && !ctx.egui_wants_keyboard_input() {
            if !self.checked.is_empty() {
                let ids: Vec<u64> = self.checked.iter().copied().collect();
                self.delete_ids(&ids);
            } else if let Some(id) = self.selected {
                self.delete_ids(&[id]);
            }
        }
    }
}

struct RowResponse {
    /// Клик по карточке вне зоны чекбокса — значит «выбрать для превью».
    row_clicked: bool,
    copy_clicked: bool,
    delete_clicked: bool,
}

/// Максимальная длина заголовка карточки в символах. Полный текст показывается
/// в тултипе и в правом превью, здесь задача — не дать UI разъезжаться.
const MAX_TITLE_CHARS: usize = 32;

fn truncate_chars(s: &str, max: usize) -> String {
    let normalized: String = s.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect();
    if normalized.chars().count() <= max {
        return normalized;
    }
    let mut out: String = normalized.chars().take(max).collect();
    out.push('…');
    out
}

fn draw_list_row(ui: &mut egui::Ui, item: &ClipItem, selected: bool, checked: &mut bool) -> RowResponse {
    let fill = if selected { ACCENT.linear_multiply(0.25) } else { ui.visuals().faint_bg_color };

    let row_width = ui.available_width();
    let mut row_clicked = false;
    egui::Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(row_width - 4.0);
            ui.horizontal(|ui| {
                // Чекбокс — самостоятельный виджет со своей зоной клика.
                ui.checkbox(checked, "");

                // Кликабельна только оставшаяся часть карточки: если накрыть
                // Sense'ом всю строку, он регистрируется после чекбокса и
                // перехватывает у него клик — отметить запись становится нельзя.
                let rest_width = ui.available_width();
                let rest = ui.allocate_ui_with_layout(
                    egui::vec2(rest_width, 0.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        let kind_label = match item.kind {
                            ItemKind::Text => "TXT",
                            ItemKind::Image => "IMG",
                        };
                        ui.label(RichText::new(kind_label).monospace().color(ACCENT));

                        let full_title = match item.kind {
                            ItemKind::Text => item.snippet.clone().unwrap_or_default(),
                            ItemKind::Image => match item.image_dims {
                                Some((w, h)) => format!("{w}×{h}"),
                                None => "картинка".to_string(),
                            },
                        };
                        let short_title = truncate_chars(&full_title, MAX_TITLE_CHARS);

                        ui.vertical(|ui| {
                            let resp = ui.add(
                                egui::Label::new(RichText::new(&short_title).strong())
                                    .wrap_mode(egui::TextWrapMode::Extend),
                            );
                            if short_title != full_title {
                                resp.on_hover_text(&full_title);
                            }
                            ui.label(
                                RichText::new(format_ago(item.timestamp))
                                    .small()
                                    .color(ui.visuals().weak_text_color()),
                            );
                        });
                    },
                );
                row_clicked = rest.response.interact(egui::Sense::click()).clicked();
            });
        });

    ui.add_space(4.0);
    RowResponse { row_clicked, copy_clicked: false, delete_clicked: false }
}

impl App {
    fn copy_item(&mut self, id: u64) {
        let Some(item) = self.store.lock().ok().and_then(|s| s.get(id).cloned()) else { return };
        match item.kind {
            ItemKind::Text => {
                if let Some(text) = self.store.lock().ok().and_then(|s| s.load_text(id)) {
                    copy_text_to_clipboard(&text, &self.watcher.last_seen_hash);
                }
            }
            ItemKind::Image => {
                if let Some((rgba, w, h)) = self.store.lock().ok().and_then(|s| s.load_image_rgba(id)) {
                    copy_image_to_clipboard(rgba, w, h, &self.watcher.last_seen_hash);
                }
            }
        }
    }
}


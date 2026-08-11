#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{self, Color32, CornerRadius, RichText};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tray_icon::{
    Icon as TrayImgIcon, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

mod autostart;
mod buffer;
mod hotkey;
mod paste;

use buffer::{
    ClipItem, HistoryStore, ItemKind, WatcherHandle, copy_image_to_clipboard,
    copy_text_to_clipboard, format_ago, spawn_watcher,
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
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|exc| panic!("failed to read {}: {exc}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|exc| panic!("failed to parse {}: {exc}", path.display()))
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
    /// Закреплённые записи лимитом не вытесняются.
    max_items: usize,
    /// Как часто фоновой поток опрашивает системный буфер обмена.
    poll_interval_ms: u64,
    /// Комбинация показа окна, например `Ctrl+Shift+V`.
    hotkey: String,
    hotkey_enabled: bool,
    /// Показывать окно рядом с курсором, а не там, где оно было.
    show_at_cursor: bool,
    /// После выбора записи вернуть фокус прошлому окну и нажать Ctrl+V.
    auto_paste: bool,
    /// Прятать окно после копирования (без вставки).
    hide_after_copy: bool,
    capture_text: bool,
    capture_images: bool,
    /// Оформление: тёмное, светлое или «как в системе».
    theme: ThemeChoice,
}

/// Выбор темы. Отдельно от `egui::ThemePreference`, чтобы формат
/// `settings.json` не зависел от serde-фич egui.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ThemeChoice {
    #[default]
    Dark,
    Light,
    System,
}

impl ThemeChoice {
    fn label(self) -> &'static str {
        match self {
            Self::Dark => "Тёмная",
            Self::Light => "Светлая",
            Self::System => "Как в системе",
        }
    }

    fn preference(self) -> egui::ThemePreference {
        match self {
            Self::Dark => egui::ThemePreference::Dark,
            Self::Light => egui::ThemePreference::Light,
            Self::System => egui::ThemePreference::System,
        }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            max_items: 100,
            poll_interval_ms: 600,
            hotkey: "Ctrl+Shift+V".to_string(),
            hotkey_enabled: true,
            show_at_cursor: true,
            auto_paste: true,
            hide_after_copy: true,
            capture_text: true,
            capture_images: true,
            theme: ThemeChoice::Dark,
        }
    }
}

// ---------- иконка ----------

fn decode_icon() -> (Vec<u8>, u32, u32) {
    let image = image::load_from_memory(ICON_PNG)
        .expect("decode embedded icon")
        .into_rgba8();
    let (width, height) = image.dimensions();
    (image.into_raw(), width, height)
}

// ---------- тема оформления ----------

/// Оформление для одной из тем: скруглённые углы и один акцентный цвет.
/// Тёмной задаём собственную палитру, светлой хватает стандартной egui-шной.
fn build_visuals(theme: egui::Theme, accent: Color32) -> egui::Visuals {
    let mut visuals = theme.default_visuals();

    if theme == egui::Theme::Dark {
        visuals.panel_fill = Color32::from_rgb(0x1e, 0x21, 0x28);
        visuals.window_fill = Color32::from_rgb(0x24, 0x28, 0x30);
        visuals.extreme_bg_color = Color32::from_rgb(0x17, 0x19, 0x1f);
        visuals.faint_bg_color = Color32::from_rgb(0x2a, 0x2e, 0x37);
    }
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

/// Регистрирует оформление обеих тем и включает выбранную.
///
/// Одного `set_visuals` мало: он пишет в стиль *текущей* темы, а eframe затем
/// присылает системную тему и переключается на её нетронутый стиль — из-за
/// этого приложение открывалось светлым на светлой Windows.
fn apply_theme(ctx: &egui::Context, choice: ThemeChoice) {
    ctx.set_visuals_of(egui::Theme::Dark, build_visuals(egui::Theme::Dark, ACCENT));
    ctx.set_visuals_of(
        egui::Theme::Light,
        build_visuals(egui::Theme::Light, ACCENT),
    );
    ctx.set_theme(choice.preference());
    // Панели текущего кадра уже нарисованы старым стилем, а нового события не
    // будет — без явного запроса окно так и застынет наполовину перекрашенным.
    ctx.request_repaint();
}

// ---------- приложение ----------

fn main() -> eframe::Result<()> {
    let (rgba, width, height) = decode_icon();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_TITLE)
            .with_inner_size([760.0, 480.0])
            .with_min_inner_size([520.0, 320.0])
            .with_icon(egui::IconData {
                rgba,
                width,
                height,
            }),
        ..Default::default()
    };
    eframe::run_native(
        "schrodingerbox-copycat",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

struct App {
    settings: AppSettings,
    icon_texture: egui::TextureHandle,
    _tray: TrayIcon,
    show_requested: Arc<AtomicBool>,
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
    hotkey_input: String,
    autostart_enabled: bool,
    settings_open: bool,
    search: String,
    hotkeys: Option<hotkey::HotkeyService>,
    /// Окно, активное до вызова CopyCat, — цель автовставки.
    prev_window: isize,
}

#[derive(Default)]
struct PreviewCache {
    id: Option<u64>,
    text: Option<String>,
    image: Option<(egui::TextureHandle, u32, u32)>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (rgba, width, height) = decode_icon();
        let color_image =
            egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba);
        let icon_texture =
            cc.egui_ctx
                .load_texture("app_icon", color_image, egui::TextureOptions::LINEAR);

        let settings: AppSettings = load_json(&settings_path(), AppSettings::default());
        apply_theme(&cc.egui_ctx, settings.theme);
        let store = Arc::new(Mutex::new(HistoryStore::open(base_dir().join("clipboard"))));

        let show_requested = Arc::new(AtomicBool::new(false));
        let tray = build_tray(
            &cc.egui_ctx,
            rgba,
            width,
            height,
            show_requested.clone(),
            store.clone(),
        );

        let watcher = spawn_watcher(
            store.clone(),
            cc.egui_ctx.clone(),
            settings.poll_interval_ms,
            settings.max_items,
        );
        watcher
            .capture_text
            .store(settings.capture_text, Ordering::Relaxed);
        watcher
            .capture_images
            .store(settings.capture_images, Ordering::Relaxed);

        let mut hotkeys = hotkey::HotkeyService::new(cc.egui_ctx.clone());
        if let Some(service) = hotkeys.as_mut() {
            service.apply(settings.hotkey_enabled.then_some(settings.hotkey.as_str()));
        }

        let max_items_input = settings.max_items.to_string();
        let hotkey_input = settings.hotkey.clone();
        Self {
            settings,
            icon_texture,
            _tray: tray,
            show_requested,
            store,
            watcher,
            selected: None,
            checked: HashSet::new(),
            preview: PreviewCache::default(),
            max_items_input,
            hotkey_input,
            autostart_enabled: autostart::is_enabled(),
            settings_open: false,
            search: String::new(),
            hotkeys,
            prev_window: 0,
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
        self.watcher
            .max_items
            .store(self.settings.max_items, Ordering::Relaxed);
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
        self.preview = PreviewCache {
            id: Some(item.id),
            text: None,
            image: None,
        };
        let store = self.store.lock().expect("store lock");
        match item.kind {
            ItemKind::Text => {
                self.preview.text = store.load_text(item.id);
            }
            ItemKind::Image => {
                if let Some((rgba, w, h)) = store.load_image_rgba(item.id) {
                    let img =
                        egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                    let tex = ctx.load_texture(
                        format!("preview_{}", item.id),
                        img,
                        egui::TextureOptions::LINEAR,
                    );
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
    store: Arc<Mutex<HistoryStore>>,
) -> TrayIcon {
    let icon = TrayImgIcon::from_rgba(rgba, width, height).expect("build tray icon image");

    let menu = Menu::new();
    let show_item = MenuItem::new("Показать окно", true, None);
    let quit_item = MenuItem::new("Выход", true, None);
    let show_id = show_item.id().clone();
    let quit_id = quit_item.id().clone();
    menu.append(&show_item).expect("append show item");
    menu.append(&PredefinedMenuItem::separator())
        .expect("append separator");
    menu.append(&quit_item).expect("append quit item");

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(APP_TITLE)
        .with_icon(icon)
        .build()
        .expect("build tray icon");

    let ctx_menu = ctx.clone();
    let show_from_menu = show_requested.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == show_id {
            show_from_menu.store(true, Ordering::Relaxed);
        } else if event.id == quit_id {
            // Выходим прямо здесь: со спрятанным окном winit не гоняет кадры,
            // поэтому флаг + request_repaint читались бы только после того,
            // как цикл разбудит какое-нибудь стороннее событие.
            // Лок гарантирует, что фоновой поток не пишет индекс прямо сейчас.
            if let Ok(s) = store.lock() {
                s.save_index();
            }
            std::process::exit(0);
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
        // Хоткей: запоминаем активное окно ДО показа своего — потом фокус
        // уже наш, и вставлять будет некуда.
        if self.hotkeys.as_ref().is_some_and(|h| h.take_trigger()) {
            self.prev_window = paste::foreground_window();
            if self.settings.show_at_cursor {
                if let Some((x, y)) = paste::cursor_pos() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                        x as f32, y as f32,
                    )));
                }
            }
            Self::show_window(ctx);
        }
        if self.show_requested.swap(false, Ordering::Relaxed) {
            self.prev_window = paste::foreground_window();
            Self::show_window(ctx);
        }
        // Escape прячет окно — привычное поведение для вызываемых по хоткею
        // палитр; приложение при этом остаётся жить в трее.
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        // Крестик окна прячет приложение в трей, а не закрывает — выход
        // делается только через пункт меню в трее.
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // ---------- top bar с настройками ----------
        let mut delete_all_checked = false;
        egui::Panel::top("top_bar")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(16, 10)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Image::new((
                        self.icon_texture.id(),
                        egui::vec2(22.0, 22.0),
                    )));
                    ui.add_space(8.0);
                    ui.heading(RichText::new(APP_TITLE).strong());

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Настройки").clicked() {
                            self.settings_open = !self.settings_open;
                        }

                        let count = self.store.lock().map(|s| s.items().len()).unwrap_or(0);
                        ui.label(
                            RichText::new(format!("{count} шт."))
                                .color(ui.visuals().weak_text_color()),
                        );

                        if !self.checked.is_empty() {
                            let label = format!("Удалить выбранные ({})", self.checked.len());
                            if ui
                                .button(RichText::new(label).color(Color32::WHITE))
                                .clicked()
                            {
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
        let all_items: Vec<ClipItem> = self
            .store
            .lock()
            .map(|s| s.items().to_vec())
            .unwrap_or_default();
        if self
            .selected
            .is_some_and(|id| !all_items.iter().any(|it| it.id == id))
        {
            self.selected = None;
            self.preview = PreviewCache::default();
        }

        // Закреплённые всегда сверху, внутри групп — порядок хранилища (новые выше).
        let query = self.search.trim().to_lowercase();
        let mut items: Vec<ClipItem> = all_items
            .iter()
            .filter(|it| query.is_empty() || item_matches(it, &query))
            .cloned()
            .collect();
        items.sort_by_key(|it| !it.pinned);

        // ---------- список слева ----------
        let mut requested_copy: Option<u64> = None;
        let mut requested_delete: Option<u64> = None;
        let mut requested_pin: Option<u64> = None;
        egui::Panel::left("list_panel")
            .resizable(true)
            .default_size(280.0)
            .size_range(200.0..=380.0)
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::same(8)),
            )
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .desired_width(f32::INFINITY)
                        .hint_text("Поиск по истории…"),
                );
                ui.add_space(4.0);

                if all_items.is_empty() {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("История пуста").color(ui.visuals().weak_text_color()),
                        );
                        ui.label(
                            RichText::new(
                                "Скопируйте что-нибудь (Ctrl+C, Win+Shift+S) — появится здесь.",
                            )
                            .color(ui.visuals().weak_text_color())
                            .small(),
                        );
                    });
                    return;
                }
                if items.is_empty() {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("Ничего не найдено")
                                .color(ui.visuals().weak_text_color()),
                        );
                    });
                    return;
                }

                ui.horizontal(|ui| {
                    let all_checked = items.iter().all(|it| self.checked.contains(&it.id));
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

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
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
                            if response.row_double_clicked {
                                requested_copy = Some(item.id);
                            }
                        }
                    });
            });

        // ---------- превью ----------
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ui, |ui| {
                let Some(id) = self.selected else {
                    ui.vertical_centered(|ui| {
                        ui.add_space(24.0);
                        ui.label(
                            RichText::new("Выберите запись слева")
                                .color(ui.visuals().weak_text_color()),
                        );
                    });
                    return;
                };
                let Some(item) = items.iter().find(|it| it.id == id).cloned() else {
                    return;
                };
                self.ensure_preview(&ctx, &item);

                ui.horizontal(|ui| {
                    let title = match item.kind {
                        ItemKind::Text => format!("Текст · {} B", item.size_bytes),
                        ItemKind::Image => match item.image_dims {
                            Some((w, h)) => {
                                format!("Картинка · {w}×{h} · {} КБ", item.size_bytes / 1024)
                            }
                            None => "Картинка".to_string(),
                        },
                    };
                    ui.strong(title);
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(format_ago(item.timestamp))
                            .color(ui.visuals().weak_text_color()),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Удалить").clicked() {
                            requested_delete = Some(item.id);
                        }
                        let pin_label = if item.pinned {
                            "Открепить"
                        } else {
                            "Закрепить"
                        };
                        if ui.button(pin_label).clicked() {
                            requested_pin = Some(item.id);
                        }
                        let copy_label = if self.settings.auto_paste {
                            "Вставить"
                        } else {
                            "Копировать"
                        };
                        if ui
                            .button(RichText::new(copy_label).color(Color32::WHITE))
                            .clicked()
                        {
                            requested_copy = Some(item.id);
                        }
                    });
                });
                ui.separator();

                match item.kind {
                    ItemKind::Text => {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if let Some(text) = &self.preview.text {
                                    ui.add(egui::Label::new(text).wrap());
                                } else {
                                    ui.label(
                                        RichText::new("(не удалось прочитать файл)")
                                            .color(ui.visuals().weak_text_color()),
                                    );
                                }
                            });
                    }
                    ItemKind::Image => {
                        egui::ScrollArea::both()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if let Some((tex, w, h)) = &self.preview.image {
                                    let avail = ui.available_size();
                                    let scale =
                                        (avail.x / *w as f32).min(avail.y / *h as f32).min(1.0);
                                    let size = egui::vec2(*w as f32 * scale, *h as f32 * scale);
                                    ui.add(egui::Image::new((tex.id(), size)));
                                } else {
                                    ui.label(
                                        RichText::new("(не удалось декодировать PNG)")
                                            .color(ui.visuals().weak_text_color()),
                                    );
                                }
                            });
                    }
                }
            });

        self.settings_window(&ctx);

        // ---------- отложенные действия из UI-цикла ----------
        if let Some(id) = requested_copy {
            self.copy_item(&ctx, id);
        }
        if let Some(id) = requested_pin {
            self.toggle_pin(id);
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
    /// Клик по карточке вне зоны чекбокса — «выбрать для превью».
    row_clicked: bool,
    /// Двойной клик — сразу скопировать (и вставить, если включено).
    row_double_clicked: bool,
}

/// Совпадение записи с поисковым запросом (запрос уже в нижнем регистре).
fn item_matches(item: &ClipItem, query: &str) -> bool {
    match item.kind {
        ItemKind::Text => item
            .snippet
            .as_deref()
            .is_some_and(|s| s.to_lowercase().contains(query)),
        ItemKind::Image => {
            let dims = item
                .image_dims
                .map(|(w, h)| format!("{w}x{h} {w}×{h}"))
                .unwrap_or_default();
            "картинка image img".contains(query) || dims.contains(query)
        }
    }
}

/// Максимальная длина заголовка карточки в символах. Полный текст показывается
/// в тултипе и в правом превью, здесь задача — не дать UI разъезжаться.
const MAX_TITLE_CHARS: usize = 32;

fn truncate_chars(s: &str, max: usize) -> String {
    let normalized: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    if normalized.chars().count() <= max {
        return normalized;
    }
    let mut out: String = normalized.chars().take(max).collect();
    out.push('…');
    out
}

fn draw_list_row(
    ui: &mut egui::Ui,
    item: &ClipItem,
    selected: bool,
    checked: &mut bool,
) -> RowResponse {
    let fill = if selected {
        ACCENT.linear_multiply(0.25)
    } else {
        ui.visuals().faint_bg_color
    };

    let row_width = ui.available_width();
    let mut checkbox_rect = egui::Rect::NOTHING;

    let card = egui::Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(row_width - 4.0);
            ui.horizontal(|ui| {
                checkbox_rect = ui.checkbox(checked, "").rect;

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
                    ui.horizontal(|ui| {
                        if item.pinned {
                            ui.label(RichText::new("PIN").small().monospace().color(ACCENT));
                        }
                        ui.add(
                            egui::Label::new(RichText::new(&short_title).strong())
                                .wrap_mode(egui::TextWrapMode::Extend),
                        );
                    });
                    ui.label(
                        RichText::new(format_ago(item.timestamp))
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                });
            });
        });

    // Хит-тест руками по прямоугольнику карточки. Через обычный `interact`
    // клик перехватывали вложенные Label'ы (они сенсят hover ради тултипов),
    // из-за чего верхняя половина строки не реагировала на выбор.
    let card_rect = card.response.rect;
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let inside = pointer.is_some_and(|p| card_rect.contains(p) && !checkbox_rect.contains(p));
    let row_clicked = inside && ui.input(|i| i.pointer.primary_clicked());
    let row_double_clicked = inside
        && ui.input(|i| {
            i.pointer
                .button_double_clicked(egui::PointerButton::Primary)
        });

    ui.add_space(4.0);
    RowResponse {
        row_clicked,
        row_double_clicked,
    }
}

impl App {
    fn copy_item(&mut self, ctx: &egui::Context, id: u64) {
        let Some(item) = self.store.lock().ok().and_then(|s| s.get(id).cloned()) else {
            return;
        };
        let copied = match item.kind {
            ItemKind::Text => self
                .store
                .lock()
                .ok()
                .and_then(|s| s.load_text(id))
                .is_some_and(|text| copy_text_to_clipboard(&text, &self.watcher.last_seen_hash)),
            ItemKind::Image => self
                .store
                .lock()
                .ok()
                .and_then(|s| s.load_image_rgba(id))
                .is_some_and(|(rgba, w, h)| {
                    copy_image_to_clipboard(rgba, w, h, &self.watcher.last_seen_hash)
                }),
        };
        if !copied {
            return;
        }

        if self.settings.auto_paste && self.prev_window != 0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            paste::restore_and_paste(self.prev_window);
            self.prev_window = 0;
        } else if self.settings.hide_after_copy {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
    }

    fn toggle_pin(&mut self, id: u64) {
        if let Ok(mut s) = self.store.lock() {
            let pinned = s.get(id).map(|it| it.pinned).unwrap_or(false);
            s.set_pinned(id, !pinned);
            s.save_index();
        }
    }

    /// Отдельное окно настроек: в поповере `menu_button` поля ввода
    /// закрывались от первого же клика.
    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.settings_open;
        egui::Window::new("Настройки")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(340.0)
            .show(ctx, |ui| {
                ui.label(RichText::new("Оформление").strong());
                ui.horizontal(|ui| {
                    ui.label("Тема:");
                    for choice in [ThemeChoice::Dark, ThemeChoice::Light, ThemeChoice::System] {
                        if ui
                            .selectable_label(self.settings.theme == choice, choice.label())
                            .clicked()
                            && self.settings.theme != choice
                        {
                            self.settings.theme = choice;
                            apply_theme(ui.ctx(), choice);
                            self.save_settings();
                        }
                    }
                });

                ui.add_space(8.0);
                ui.separator();
                ui.label(RichText::new("История").strong());
                ui.horizontal(|ui| {
                    ui.label("Хранить записей:");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.max_items_input)
                            .desired_width(72.0)
                            .horizontal_align(egui::Align::Center),
                    );
                    if resp.lost_focus() {
                        match self.max_items_input.trim().parse::<usize>() {
                            Ok(n) if n >= 1 => self.set_max_items(n),
                            _ => self.max_items_input = self.settings.max_items.to_string(),
                        }
                    }
                    ui.label(
                        RichText::new("закреплённые не считаются")
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                });

                ui.horizontal(|ui| {
                    if ui
                        .checkbox(&mut self.settings.capture_text, "Ловить текст")
                        .changed()
                    {
                        self.watcher
                            .capture_text
                            .store(self.settings.capture_text, Ordering::Relaxed);
                        self.save_settings();
                    }
                    if ui
                        .checkbox(&mut self.settings.capture_images, "Ловить картинки")
                        .changed()
                    {
                        self.watcher
                            .capture_images
                            .store(self.settings.capture_images, Ordering::Relaxed);
                        self.save_settings();
                    }
                });

                ui.horizontal(|ui| {
                    ui.label("Опрос буфера, мс:");
                    let mut ms = self.settings.poll_interval_ms as f32;
                    if ui
                        .add(egui::Slider::new(&mut ms, 100.0..=2000.0).step_by(50.0))
                        .changed()
                    {
                        self.settings.poll_interval_ms = ms as u64;
                        self.watcher
                            .poll_ms
                            .store(self.settings.poll_interval_ms, Ordering::Relaxed);
                        self.save_settings();
                    }
                });

                ui.add_space(8.0);
                ui.separator();
                ui.label(RichText::new("Горячая клавиша").strong());

                ui.horizontal(|ui| {
                    if ui
                        .checkbox(&mut self.settings.hotkey_enabled, "Включить")
                        .changed()
                    {
                        self.reapply_hotkey();
                    }
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.hotkey_input)
                            .desired_width(150.0)
                            .hint_text("Ctrl+Shift+V"),
                    );
                    if resp.lost_focus() {
                        self.settings.hotkey = self.hotkey_input.trim().to_string();
                        self.reapply_hotkey();
                    }
                });
                ui.label(
                    RichText::new("Например: Ctrl+Shift+V, Alt+C, Ctrl+Alt+Space")
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
                if let Some(err) = self.hotkeys.as_ref().and_then(|h| h.error.clone()) {
                    ui.colored_label(Color32::from_rgb(0xe5, 0x73, 0x73), err);
                }

                ui.add_space(8.0);
                ui.separator();
                ui.label(RichText::new("Поведение").strong());

                if ui
                    .checkbox(
                        &mut self.settings.auto_paste,
                        "Вставлять сразу (Ctrl+V в прошлое окно)",
                    )
                    .changed()
                {
                    self.save_settings();
                }
                if ui
                    .checkbox(
                        &mut self.settings.hide_after_copy,
                        "Прятать окно после копирования",
                    )
                    .changed()
                {
                    self.save_settings();
                }
                if ui
                    .checkbox(
                        &mut self.settings.show_at_cursor,
                        "Показывать окно у курсора",
                    )
                    .changed()
                {
                    self.save_settings();
                }

                #[cfg(windows)]
                {
                    let mut on = self.autostart_enabled;
                    if ui
                        .checkbox(&mut on, "Автозапуск при старте Windows")
                        .changed()
                    {
                        match autostart::set(on) {
                            Ok(()) => self.autostart_enabled = on,
                            Err(_) => self.autostart_enabled = autostart::is_enabled(),
                        }
                    }
                }
            });
        self.settings_open = open;
    }

    fn reapply_hotkey(&mut self) {
        let spec = self.settings.hotkey.clone();
        let enabled = self.settings.hotkey_enabled;
        if let Some(service) = self.hotkeys.as_mut() {
            service.apply(enabled.then_some(spec.as_str()));
        }
        self.save_settings();
    }
}

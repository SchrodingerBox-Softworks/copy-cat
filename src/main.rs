#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{self, Color32, CornerRadius, RichText};
use serde::{Deserialize, Serialize};
use serde::de::DeserializeOwned;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon as TrayImgIcon, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

mod buffer;

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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AppSettings {
    click_count: u32,
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

/// Маленькая скруглённая «таблетка» с цветной точкой и текстом.
fn badge(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.linear_multiply(0.16))
        .corner_radius(CornerRadius::same(20))
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                ui.colored_label(color, "●");
                ui.colored_label(color, text);
            });
        });
}

// ---------- приложение ----------

fn main() -> eframe::Result<()> {
    let (rgba, width, height) = decode_icon();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_TITLE)
            .with_inner_size([560.0, 400.0])
            .with_min_inner_size([360.0, 280.0])
            .with_icon(egui::IconData { rgba, width, height }),
        ..Default::default()
    };
    eframe::run_native("schrodingerbox-template-app", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

struct App {
    settings: AppSettings,
    icon_texture: egui::TextureHandle,
    _tray: TrayIcon,
    show_requested: Arc<AtomicBool>,
    quit_requested: Arc<AtomicBool>,
    allow_close: bool,
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

        Self {
            settings: load_json(&settings_path(), AppSettings::default()),
            icon_texture,
            _tray: tray,
            show_requested,
            quit_requested,
            allow_close: false,
        }
    }

    fn save_settings(&self) {
        save_json(&settings_path(), &self.settings);
    }

    fn show_window(ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
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
        egui::Panel::top("top_bar")
            .frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin::symmetric(16, 14)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Image::new((self.icon_texture.id(), egui::vec2(24.0, 24.0))));
                    ui.add_space(8.0);
                    ui.heading(RichText::new(APP_TITLE).strong());
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ui.visuals().extreme_bg_color).inner_margin(egui::Margin::symmetric(16, 16)))
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Это шаблон — замените этот блок на реальный UI вашего инструмента.")
                        .color(ui.visuals().weak_text_color()),
                );
                ui.add_space(12.0);

                badge(ui, "пример статус-бейджа", Color32::from_rgb(0x4c, 0xaf, 0x50));
                ui.add_space(12.0);

                ui.horizontal(|ui| {
                    ui.label(format!("Сохранённое значение: {}", self.settings.click_count));
                    if ui.button("+1 (и сохранить в settings.json)").clicked() {
                        self.settings.click_count += 1;
                        self.save_settings();
                    }
                });
            });
    }
}


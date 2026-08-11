//! Global shortcut that shows the window. `global_hotkey` parses strings like
//! `Ctrl+Shift+V`, so the combination can be configured as plain text.

use eframe::egui;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct HotkeyService {
    manager: GlobalHotKeyManager,
    registered: Option<HotKey>,
    triggered: Arc<AtomicBool>,
    /// Last registration error, shown in the settings window.
    pub error: Option<String>,
}

impl HotkeyService {
    pub fn new(ctx: egui::Context) -> Option<Self> {
        let manager = GlobalHotKeyManager::new().ok()?;
        let triggered = Arc::new(AtomicBool::new(false));

        let flag = triggered.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            // Press only: reacting to the release too would show the window twice.
            if event.state == HotKeyState::Pressed {
                flag.store(true, Ordering::Relaxed);
                ctx.request_repaint();
            }
        }));

        Some(Self {
            manager,
            registered: None,
            triggered,
            error: None,
        })
    }

    /// Re-registers the combination. `None` turns the shortcut off entirely.
    pub fn apply(&mut self, spec: Option<&str>) {
        if let Some(old) = self.registered.take() {
            let _ = self.manager.unregister(old);
        }
        self.error = None;

        let Some(spec) = spec else { return };
        let hotkey = match HotKey::from_str(spec) {
            Ok(h) => h,
            Err(exc) => {
                self.error = Some(format!("не разобрать «{spec}»: {exc}"));
                return;
            }
        };
        match self.manager.register(hotkey) {
            Ok(()) => self.registered = Some(hotkey),
            // Usually means another app already owns the combination.
            Err(exc) => self.error = Some(format!("не зарегистрировать «{spec}»: {exc}")),
        }
    }

    pub fn take_trigger(&self) -> bool {
        self.triggered.swap(false, Ordering::Relaxed)
    }
}

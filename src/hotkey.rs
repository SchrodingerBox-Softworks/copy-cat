//! Глобальный хоткей показа окна. Строка вида `Ctrl+Shift+V` разбирается
//! `global_hotkey`, поэтому пользователь настраивает комбинацию текстом.

use eframe::egui;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct HotkeyService {
    manager: GlobalHotKeyManager,
    registered: Option<HotKey>,
    triggered: Arc<AtomicBool>,
    /// Текст последней ошибки регистрации — показывается в настройках.
    pub error: Option<String>,
}

impl HotkeyService {
    pub fn new(ctx: egui::Context) -> Option<Self> {
        let manager = GlobalHotKeyManager::new().ok()?;
        let triggered = Arc::new(AtomicBool::new(false));

        let flag = triggered.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            // Реагируем только на нажатие, иначе окно дёргалось бы дважды.
            if event.state == HotKeyState::Pressed {
                flag.store(true, Ordering::Relaxed);
                ctx.request_repaint();
            }
        }));

        Some(Self { manager, registered: None, triggered, error: None })
    }

    /// Перерегистрирует комбинацию. `None` — выключить хоткей совсем.
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
            // Чаще всего — комбинация уже занята другим приложением.
            Err(exc) => self.error = Some(format!("не зарегистрировать «{spec}»: {exc}")),
        }
    }

    pub fn take_trigger(&self) -> bool {
        self.triggered.swap(false, Ordering::Relaxed)
    }
}

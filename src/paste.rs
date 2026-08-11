//! Возврат фокуса в предыдущее окно и автоматическая вставка (Ctrl+V).
//!
//! HWND хранится как `isize`, чтобы его можно было спокойно держать в полях
//! приложения и передавать в поток (сырой указатель не `Send`).

/// Окно, которое было активно до того, как пользователь позвал CopyCat.
/// Снимается в момент срабатывания хоткея — позже фокус уже наш.
#[cfg(windows)]
pub fn foreground_window() -> isize {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    unsafe { GetForegroundWindow() as isize }
}

#[cfg(windows)]
pub fn cursor_pos() -> Option<(i32, i32)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let mut point = POINT { x: 0, y: 0 };
    let ok = unsafe { GetCursorPos(&mut point) };
    (ok != 0).then_some((point.x, point.y))
}

/// Возвращает фокус окну `hwnd` и шлёт ему Ctrl+V.
///
/// Работает в отдельном потоке с паузами: Windows не отдаёт фокус мгновенно,
/// а вставка в ещё не активированное окно уходит в пустоту.
#[cfg(windows)]
pub fn restore_and_paste(hwnd: isize) {
    use std::thread;
    use std::time::Duration;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
        VK_CONTROL, VK_V,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SetForegroundWindow;

    if hwnd == 0 {
        return;
    }

    thread::spawn(move || {
        thread::sleep(Duration::from_millis(80));
        unsafe { SetForegroundWindow(hwnd as HWND) };
        thread::sleep(Duration::from_millis(60));

        fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: vk,
                        wScan: 0,
                        dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            }
        }

        let mut inputs = [
            key(VK_CONTROL, false),
            key(VK_V, false),
            key(VK_V, true),
            key(VK_CONTROL, true),
        ];
        unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_mut_ptr(),
                size_of::<INPUT>() as i32,
            );
        }
    });
}

#[cfg(not(windows))]
pub fn foreground_window() -> isize {
    0
}

#[cfg(not(windows))]
pub fn cursor_pos() -> Option<(i32, i32)> {
    None
}

#[cfg(not(windows))]
pub fn restore_and_paste(_hwnd: isize) {}

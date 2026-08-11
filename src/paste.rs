//! Handing focus back to the previous window and pasting into it with Ctrl+V.
//!
//! The HWND is carried around as an `isize`: a raw pointer isn't `Send`, and this
//! one has to live in the app struct and cross into a thread.

/// The window that was active before the user called CopyCat up. Read the
/// moment the hotkey fires, since by the next frame the focus is ours.
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

/// Gives focus back to `hwnd` and sends it Ctrl+V.
///
/// Runs on its own thread with pauses in between: Windows doesn't hand over focus
/// instantly, and keystrokes sent to a window that isn't active yet go nowhere.
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

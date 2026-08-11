//! Автозапуск на Windows через `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//!
//! На других платформах функции-заглушки возвращают `false` / `Ok(())`, чтобы
//! галочка в UI просто ничего не делала (галочки там нет, но unified API).

#[cfg(windows)]
mod imp {
    use std::io;
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE_NAME: &str = "CopyCat";

    pub fn is_enabled() -> bool {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_READ)
            .and_then(|key| key.get_value::<String, _>(VALUE_NAME))
            .is_ok()
    }

    pub fn set(enable: bool) -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let key = hkcu.open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)?;
        if enable {
            let exe = std::env::current_exe()?;
            // Кавычки нужны, иначе пробелы в пути ломают команду.
            let value = format!("\"{}\"", exe.to_string_lossy());
            key.set_value(VALUE_NAME, &value)?;
        } else {
            match key.delete_value(VALUE_NAME) {
                Ok(_) => {}
                Err(exc) if exc.kind() == io::ErrorKind::NotFound => {}
                Err(exc) => return Err(exc),
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn is_enabled() -> bool { false }
    pub fn set(_enable: bool) -> std::io::Result<()> { Ok(()) }
}

pub use imp::{is_enabled, set};

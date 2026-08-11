<div align="center">

<img src="assets/icon.png" alt="CopyCat" width="96">

# CopyCat

**Менеджер истории буфера обмена для Windows.**
Живёт в трее, ловит всё, что вы копируете, и по `Ctrl+Shift+V` возвращает это обратно.

[![CI](https://github.com/SchrodingerBox-Softworks/copy-cat/actions/workflows/rust.yml/badge.svg)](https://github.com/SchrodingerBox-Softworks/copy-cat/actions/workflows/rust.yml)
[![Release](https://github.com/SchrodingerBox-Softworks/copy-cat/actions/workflows/release.yml/badge.svg)](https://github.com/SchrodingerBox-Softworks/copy-cat/actions/workflows/release.yml)
[![Latest release](https://img.shields.io/github/v/release/SchrodingerBox-Softworks/copy-cat?sort=semver)](https://github.com/SchrodingerBox-Softworks/copy-cat/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platform: Windows](https://img.shields.io/badge/platform-Windows-0078D6)


![Демонстрация CopyCat](assets/demo.gif)

</div>

[![Made with Rust](https://forthebadge.com/images/badges/made-with-rust.svg)](https://www.rust-lang.org)

---

## Что это

В Windows есть своя история буфера обмена по `Win+V`, но она хранит только текст,
не умеет искать и синхронизируется с облаком. CopyCat делает то же самое локально,
обычными файлами рядом с `.exe`.

- Ловит и текст, и картинки. Скриншот из `Win+Shift+S` попадёт в историю так же, как строка из `Ctrl+C`.
- `Ctrl+Shift+V` открывает окно у курсора. Комбинацию можно поменять.
- Автовставка: выбрали запись, окно спряталось, фокус вернулся прошлому окну, `Ctrl+V` нажался сам.
- Поиск по истории и закрепление записей, которые не должны вытесняться лимитом.
- Удаление по одной, по `Delete`, или пачкой через чекбоксы.
- Тёмная тема по умолчанию, есть светлая и системная.
- Автозапуск с Windows.
- Ничего не устанавливает и не пишет в `%AppData%`.

## Установка

Скачайте `copy-cat-vX.Y.Z-x86_64-windows.zip` со страницы
[Releases](https://github.com/SchrodingerBox-Softworks/copy-cat/releases/latest)
и распакуйте. Внутри готовая папка:

```
copy-cat-vX.Y.Z\
├─ copy-cat.exe
├─ README.txt
└─ LICENSE.txt
```

Запустите `copy-cat.exe`, окно свернётся в трей. Чтобы удалить программу,
удалите папку.

Класть её лучше туда, где у вас есть право на запись: `C:\Tools\CopyCat\` подойдёт,
`C:\Program Files\` нет. Рядом с `.exe` создаются `settings.json` и папка `clipboard\`.

Сверить хэш из описания релиза:

```powershell
Get-FileHash .\copy-cat-vX.Y.Z-x86_64-windows.zip -Algorithm SHA256
```

## Как пользоваться

| Действие | Как |
| --- | --- |
| Открыть окно | `Ctrl+Shift+V` или двойной клик по иконке в трее |
| Спрятать окно | `Esc` или крестик, приложение останется в трее |
| Вставить запись | Двойной клик по строке или кнопка **Вставить** |
| Посмотреть запись | Одиночный клик по строке |
| Найти | Поле поиска над списком |
| Закрепить | Кнопка **Закрепить**, запись получает метку `PIN` |
| Удалить | `Delete`, или чекбоксы и **Удалить выбранные** |
| Выйти | Правый клик по иконке в трее, **Выход** |

## Настройки

Кнопка **Настройки** в правом верхнем углу. Всё пишется в `settings.json` сразу
после изменения, перезапускать не нужно.

| Параметр | По умолчанию | Что делает |
| --- | --- | --- |
| `theme` | `dark` | `dark`, `light` или `system` |
| `max_items` | `100` | Сколько записей хранить. Закреплённые в лимит не входят |
| `poll_interval_ms` | `600` | Как часто опрашивается буфер обмена |
| `hotkey` | `Ctrl+Shift+V` | Например `Alt+C` или `Ctrl+Alt+Space`. Если комбинация занята, в настройках появится ошибка |
| `hotkey_enabled` | `true` | Выключает хоткей, не стирая комбинацию |
| `show_at_cursor` | `true` | Показывать окно у курсора, а не там, где оно было |
| `auto_paste` | `true` | Вставлять сразу. Выключите, если нужно только скопировать |
| `hide_after_copy` | `true` | Прятать окно после копирования, когда автовставка выключена |
| `capture_text` | `true` | Ловить текст |
| `capture_images` | `true` | Ловить картинки |

## Где лежат данные

```
CopyCat\
├─ copy-cat.exe
├─ settings.json          # настройки
└─ clipboard\
   ├─ index.json          # id, тип, время, размер, хэш, pinned
   ├─ 0000000001.txt      # тела текстовых записей
   └─ 0000000002.png      # тела картинок
```

В памяти держится только `index.json`. Текст или PNG читается с диска, когда
запись выбрали в списке. Дубликаты отсекаются по хэшу FNV-1a.

> [!WARNING]
> История лежит в открытом виде. Скопированный пароль окажется обычным `.txt`
> в папке `clipboard\`. Если работаете с чувствительными данными, снимите галочку
> **Ловить текст** или удалите запись сразу после использования.

## Сборка

Нужен [Rust](https://rustup.rs/) 1.85+ и MSVC-toolchain.

```powershell
git clone https://github.com/SchrodingerBox-Softworks/copy-cat.git
cd copy-cat
cargo build --release
```

Бинарь появится в `target\release\copy-cat.exe`. То, что гоняет CI:

```powershell
cargo fmt --all -- --check; cargo clippy --all-targets -- -D warnings; cargo test
```

## Как устроено

Окно и иконка в трее — [`src/main.rs`](src/main.rs). История на диске и фоновой
поток, следящий за буфером, — [`src/buffer.rs`](src/buffer.rs). Глобальный хоткей
вынесен в [`src/hotkey.rs`](src/hotkey.rs), возврат фокуса и `Ctrl+V` через WinAPI —
в [`src/paste.rs`](src/paste.rs), ключ автозапуска в реестре —
в [`src/autostart.rs`](src/autostart.rs). [`build.rs`](build.rs) вшивает иконку
в ресурсы `.exe`.

Стек: [`eframe`/`egui`](https://github.com/emilk/egui) · [`tray-icon`](https://github.com/tauri-apps/tray-icon) ·
[`arboard`](https://github.com/1Password/arboard) · [`global-hotkey`](https://github.com/tauri-apps/global-hotkey) ·
[`windows-sys`](https://github.com/microsoft/windows-rs)

## Лицензия

[MIT](LICENSE)

## Автор

<img src="https://github.com/Schrodinger71.png" width="60" height="60" alt="Schrodinger71" align="left" style="border-radius:50%; margin-right:10px">

[**Schrodinger71**](https://github.com/Schrodinger71) — Discord: schrodinger71

<br clear="left">

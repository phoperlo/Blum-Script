# Установщик BlumScript

## Быстрая установка (Windows)

1. Запустите `install.bat` двойным кликом.
   Либо из терминала:

   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1
   ```

2. Установка идёт в `%LOCALAPPDATA%\BlumScript`:

   ```
   BlumScript\
   ├── bin\blumsc.exe
   ├── docs\README.md
   ├── docs\DOCUMENTATION.md
   ├── examples\*.blm
   ├── site\index.html
   ├── uninstall.ps1
   └── version.txt
   ```

3. Каталог `bin` добавляется в пользовательский `PATH`.
   Откройте **новый** терминал и проверьте:

   ```powershell
   blumsc run "%LOCALAPPDATA%\BlumScript\examples\hello.blm"
   ```

## Параметры `install.ps1`

| Параметр | Описание |
|----------|----------|
| `-Source <path>` | путь к проекту `blumscript_rust` (по умолчанию — родитель каталога `installer`) |
| `-NoPath` | не менять пользовательский `PATH` |
| `-Build` | принудительно пересобрать компилятор через `cargo build --release` |
| `-Open` | открыть обучающий сайт после установки |

Примеры:

```powershell
.\install.ps1 -Build -Open
.\install.ps1 -Source "C:\src\blumscript_rust" -NoPath
```

## Упаковка дистрибутива

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\package.ps1
```

Создаст:

- `..\dist\` — готовый набор (`blumsc.exe`, документация, примеры, сайт, установщик);
- `..\blumscript-0.2.0-windows.zip` — архив для распространения.

В упакованном виде `install.ps1` использует `blumsc.exe`, лежащий рядом с ним,
и не требует Rust/Cargo у конечного пользователя.

## Удаление

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\BlumScript\uninstall.ps1"
```

Скрипт убирает `bin` из `PATH` и удаляет каталог установки.
Флаг `-NoPath` оставляет `PATH` без изменений.

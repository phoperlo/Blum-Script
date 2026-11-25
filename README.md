# BlumScript

Компилируемый язык с английскими ключевыми словами и возможностями C: от высокоуровневых функций и структур до указателей, `malloc/free`, `sizeof`, `cast<T>` и сырых C-вставок. Написан на Rust, генерирует C99 и собирает нативный `.exe` через MSVC / GCC / Clang.

## Сборка компилятора

```powershell
cargo build --release
```

Бинарник: `target\release\blumsc.exe`.

Нужен Rust и C-компилятор (MSVC с `vcvars64.bat`, либо `gcc`/`clang` в `PATH`).

## Быстрый старт

`hello.blm`:

```blumscript
func main() -> int {
    print("Hello, BlumScript!");
    return 0;
}
```

```powershell
blumsc run hello.blm
```

## CLI

```
blumsc build <file.blm> [-o out.exe] [--emit-c]
blumsc run   <file.blm>
blumsc check <file.blm>
blumsc tokens <file.blm>
blumsc ast    <file.blm>
```

## Возможности

- `let` / `var`, вывод типов, явные аннотации `name: type`
- `func` с типизированными параметрами и `-> тип`
- `if` / `else`, `while`, трёхчастный `for`, `break`, `continue`, `return`
- `struct` с доступом `.` и `->`
- указатели `*T`, `&`, `*`, арифметика указателей, индексация `[]`
- `malloc` / `free`, `sizeof`, `cast<T>(...)`
- встроенные `print`, `len`, `strlen`, `to_int`, `to_float`
- `import "file.blm"` с рекурсивным раскрытием
- `asm { ... }` — дословная вставка C
- компиляция в нативный код (без VM)

## Примеры

Смотрите `examples/`:

- `hello.blm` — приветствие, фибоначчи, циклы;
- `lowlevel.blm` — структуры, указатели, `malloc`, разыменование;
- `imports.blm` — импорт модуля, рекурсия, вставка C;
- `utils.blm` — модуль с функциями.

```powershell
blumsc run .\examples\hello.blm
blumsc run .\examples\lowlevel.blm
blumsc run .\examples\imports.blm
```

## Документация

Полное описание языка, типов, синтаксиса, стандартной библиотеки, архитектуры компилятора и ограничений: **[DOCUMENTATION.md](DOCUMENTATION.md)**.

## Обучающий сайт

Интерактивный курс с примерами и встроенной песочницей (мини-интерпретатор подмножества языка):

```
site/index.html
```

Откройте файл в браузере. Разделы: введение, установка, первая программа, переменные и типы, операторы, условия и циклы, функции, структуры, указатели и память, импорты и `asm`, песочница, шпаргалка.

## Установщик

Windows-установщик собирает компилятор (если нужно), копирует `blumsc.exe`, документацию, примеры и сайт в `%LOCALAPPDATA%\BlumScript`, добавляет `bin` в пользовательский `PATH` и создаёт деинсталлятор:

```
installer/install.bat        # двойной клик
installer/install.ps1        # параметры: -Source, -NoPath, -Build, -Open
installer/uninstall.ps1      # удаление
installer/package.ps1        # dist/ + zip для распространения
```

Подробности: **[installer/README.md](installer/README.md)**.

## Структура проекта

```
blumscript_rust/
├── Cargo.toml
├── DOCUMENTATION.md
├── README.md
├── src/
│   ├── main.rs      # CLI
│   ├── lexer.rs     # токенизация
│   ├── ast.rs       # AST
│   ├── parser.rs    # рекурсивный спуск
│   ├── types.rs     # система типов
│   ├── codegen.rs   # проверка типов + генерация C
│   └── driver.rs    # импорты + вызов C-компилятора
├── examples/        # примеры .blm
├── site/            # обучающий HTML-сайт с песочницей
└── installer/       # установщик, деинсталлятор, упаковка
```

## Лицензия

Учебный/исследовательский проект. Используйте свободно.

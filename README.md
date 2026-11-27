# BlumScript

Компилируемый язык с английскими ключевыми словами и возможностями C: от высокоуровневых функций и структур до указателей, `malloc/free`, `sizeof`, `cast<T>` и сырых вставок машинного кода. Написан на Rust и **сам генерирует x86-64 машинный код и PE32+ `.exe`** — без C, без ассемблера, без LLVM, без внешнего линковщика.

## Сборка компилятора

```powershell
cargo build --release
```

Бинарник: `target\release\blumsc.exe`.

Нужен только Rust. C-компилятор больше не требуется: бэкенд пишет байты инструкций и собирает PE-образ сам.

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

```
built: hello.exe
Hello, BlumScript!
```

## CLI

```
blumsc build <file.blm> [-o out.exe] [--emit-bytes]
blumsc run   <file.blm>
blumsc check <file.blm>
blumsc tokens <file.blm>
blumsc ast    <file.blm>
blumsc hex    <file.blm>
```

`--emit-bytes` / `hex` печатают сгенерированный машинный код hex-дампом.

## Возможности

- `let` / `var`, вывод типов, явные аннотации `name: type`
- `func` с типизированными параметрами и `-> тип`
- `if` / `else` / `else if`, `while`, трёхчастный `for`, `break`, `continue`, `return`
- `struct` с доступом `.` и `->`, вложенные структуры, выравнивание полей
- структуры и массивы по значению: аргументы, возврат, копирование (ABI-классификатор, скрытый указатель возврата)
- указатели `*T`, `&`, `*`, арифметика указателей, индексация `[]`
- `malloc` / `free` (через `HeapAlloc` / `HeapFree`), `sizeof`, `cast<T>(...)`
- встроенные `print`, `len`, `strlen`, `to_int`, `to_float`
- консольный ввод: `read_str()`, `read_int()`, `read_char()`
- файлы: `open`, `read`, `write`, `file_size`, `close`
- вещественные числа `float` (IEEE-754 double) на SSE2: арифметика, сравнения, `XMM0..3` в ABI, печать 6 знаков, `nan`/`inf`
- `import "file.blm"` с рекурсивным раскрытием
- `asm { ... }` — текстовый inline-ассемблер (мнемоники, метки, доступ к локальным и глобальным переменным) плюс совместимый режим raw hex-байтов
- вариативные функции `func f(a: int, ...)` с C-совместимым varargs ABI: дублирование float в GP-слоты, home space, `va_int`/`va_float`
- собственный рантайм на машинном коде: печать чисел/строк/символов/bool, `strlen`, heap
- соглашение вызова Microsoft x64, импорт только из `kernel32.dll`

## Примеры

Смотрите `examples/`:

- `hello.blm` — приветствие, фибоначчи, циклы;
- `lowlevel.blm` — структуры, указатели, `malloc`, разыменование;
- `imports.blm` — импорт модуля, рекурсия, raw-вставка машинного кода;
- `elseif.blm` — цепочка `else if`;
- `io.blm` — консольный ввод и работа с файлами;
- `floats.blm` — вещественные числа: арифметика, структуры, массивы, преобразования, `nan`/`inf`;
- `structs_by_value.blm` — структуры и массивы по значению: аргументы, возврат, копирование, рекурсия;
- `asm.blm` — inline-ассемблер: регистры, локальные метки, `[local]`/`[global]`, вызов функций, SSE;
- `varargs.blm` — вариативные функции, дублирование float в GP-слоты, `va_int`/`va_float`;
- `features.blm` — вложенные структуры, массивы, `malloc`, `sizeof`;
- `utils.blm` — модуль с функциями.

```powershell
blumsc run .\examples\hello.blm
blumsc run .\examples\lowlevel.blm
blumsc run .\examples\imports.blm
blumsc run .\examples\floats.blm
blumsc run .\examples\structs_by_value.blm
blumsc run .\examples\asm.blm
blumsc run .\examples\varargs.blm
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
│   ├── x64.rs       # кодировщик инструкций x86-64, метки, фиксапы
│   ├── native.rs    # семантика + кодоген (AST → машинный код)
│   ├── runtime.rs   # рантайм на машинном коде
│   ├── abi.rs       # смещения, индексы IAT, импорты kernel32
│   ├── pe.rs        # PE32+ writer (секции, импорт-таблица, фиксапы)
│   └── imports.rs   # раскрытие import + запуск .exe
├── examples/        # примеры .blm
├── site/            # обучающий HTML-сайт с песочницей
└── installer/       # установщик, деинсталлятор, упаковка
```

## Лицензия

Учебный/исследовательский проект. Используйте свободно.

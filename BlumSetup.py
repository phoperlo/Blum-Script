import os
import sys
import ctypes
import winreg
import json
import subprocess
import tempfile
import shutil
import random
import time

def is_admin():
    try:
        return ctypes.windll.shell32.IsUserAnAdmin()
    except:
        return False

def run_as_admin():
    ctypes.windll.shell32.ShellExecuteW(None, "runas", sys.executable, " ".join(sys.argv), None, 1)
    sys.exit()

def compile_interpreter():
    """Создает EXE интерпретатор через PyInstaller"""
    print("Creating EXE interpreter...")
    
    python_code = '''import sys
import os
import random
import time
import math

class BlumInterpreter:
    def __init__(self):
        self.variables = {}
        self.functions = {
            'random': random.randint,
            'randint': random.randint,
            'sqrt': math.sqrt,
            'abs': abs,
            'round': round,
            'int': int,
            'float': float
        }
        self.condition_stack = []
        self.skip_block = False
        self.in_else_block = False
    
    def show(self, text):
        """Выводит текст в консоль"""
        result = ""
        i = 0
        while i < len(text):
            if text[i] == '{' and i + 1 < len(text):
                j = i + 1
                brace_count = 1
                while j < len(text) and brace_count > 0:
                    if text[j] == '{':
                        brace_count += 1
                    elif text[j] == '}':
                        brace_count -= 1
                    j += 1
                
                if brace_count == 0:
                    expr = text[i+1:j-1]
                    try:
                        value = self.evaluate_expression(expr)
                        result += str(value)
                    except:
                        result += "{" + expr + "}"
                    
                    i = j
                    continue
            
            result += text[i]
            i += 1
        
        print(result)
    
    def get_input(self, prompt=""):
        """Получает ввод от пользователя"""
        if prompt:
            processed_prompt = ""
            i = 0
            while i < len(prompt):
                if prompt[i] == '{' and i + 1 < len(prompt):
                    j = i + 1
                    brace_count = 1
                    while j < len(prompt) and brace_count > 0:
                        if prompt[j] == '{':
                            brace_count += 1
                        elif prompt[j] == '}':
                            brace_count -= 1
                        j += 1
                    
                    if brace_count == 0:
                        expr = prompt[i+1:j-1]
                        try:
                            value = self.evaluate_expression(expr)
                            processed_prompt += str(value)
                        except:
                            processed_prompt += "{" + expr + "}"
                        
                        i = j
                        continue
                
                processed_prompt += prompt[i]
                i += 1
            
            return input(processed_prompt + " ")
        else:
            return input()
    
    def set_variable(self, var_name, value):
        """Устанавливает переменную"""
        self.variables[var_name] = value
    
    def evaluate_expression(self, expr):
        """Вычисляет математическое выражение"""
        expr = expr.strip()
        
        try:
            if '.' in expr:
                return float(expr)
            else:
                return int(expr)
        except:
            pass
        
        if expr in self.variables:
            return self.variables[expr]
        
        if expr.startswith('random(') and expr.endswith(')'):
            params = expr[7:-1].strip()
            if ',' in params:
                parts = params.split(',')
                min_val = int(self.evaluate_expression(parts[0].strip()))
                max_val = int(self.evaluate_expression(parts[1].strip()))
                return random.randint(min_val, max_val)
            else:
                if params:
                    max_val = int(self.evaluate_expression(params))
                    return random.randint(1, max_val)
                else:
                    return random.random()
        
        try:
            for var_name, var_value in self.variables.items():
                expr = expr.replace(var_name, str(var_value))
            
            safe_dict = {
                'random': random.randint,
                'randint': random.randint,
                'sqrt': math.sqrt,
                'abs': abs,
                'round': round,
                'int': int,
                'float': float,
                'math': math,
                '__builtins__': None
            }
            
            return eval(expr, {"__builtins__": {}}, safe_dict)
        except:
            return expr
    
    def evaluate_condition(self, condition):
        """Вычисляет условие if"""
        condition = condition.strip()
        
        for var_name, var_value in self.variables.items():
            condition = condition.replace(var_name, str(var_value))
        
        condition = condition.replace(' == ', ' == ')
        condition = condition.replace(' != ', ' != ')
        condition = condition.replace(' > ', ' > ')
        condition = condition.replace(' < ', ' < ')
        condition = condition.replace(' >= ', ' >= ')
        condition = condition.replace(' <= ', ' <= ')
        
        try:
            result = eval(condition, {"__builtins__": {}}, {})
            return bool(result)
        except:
            return False
    
    def parse_line(self, line, line_number):
        """Парсит и выполняет одну строку кода"""
        original_line = line
        line = line.strip()
        
        indent = len(original_line) - len(original_line.lstrip())
        is_indented = indent > 0
        
        if not line or line.startswith('//'):
            return
        
        if self.skip_block:
            if line == 'else' and not is_indented:
                self.skip_block = False
                self.in_else_block = True
            elif line == 'endif' and not is_indented:
                self.skip_block = False
                self.in_else_block = False
            elif is_indented:
                return
            else:
                self.skip_block = False
                self.in_else_block = False
        
        if self.in_else_block:
            if line == 'endif' and not is_indented:
                self.in_else_block = False
                return
            elif not is_indented:
                self.in_else_block = False
            else:
                pass
        
        if line.startswith('show(') and line.endswith(')'):
            content = line[5:-1].strip()
            if (content.startswith('"') and content.endswith('"')) or (content.startswith("'") and content.endswith("'")):
                content = content[1:-1]
            self.show(content)
        
        elif 'input(' in line and '=' in line:
            parts = line.split('=', 1)
            var_name = parts[0].strip()
            
            input_start = parts[1].find('input(')
            if input_start != -1:
                input_end = parts[1].rfind(')')
                if input_end != -1:
                    input_expr = parts[1][input_start:input_end+1]
                    prompt = input_expr[6:-1].strip()
                    
                    if (prompt.startswith('"') and prompt.endswith('"')) or (prompt.startswith("'") and prompt.endswith("'")):
                        prompt = prompt[1:-1]
                    
                    user_input = self.get_input(prompt)
                    
                    try:
                        if '.' in user_input:
                            value = float(user_input)
                        else:
                            value = int(user_input)
                        self.set_variable(var_name, value)
                    except:
                        self.set_variable(var_name, user_input)
        
        elif '=' in line and '{' in line and '}' in line:
            parts = line.split('=', 1)
            var_name = parts[0].strip()
            expression = parts[1].strip()
            
            if expression.startswith('{') and expression.endswith('}'):
                expr = expression[1:-1].strip()
                value = self.evaluate_expression(expr)
                self.set_variable(var_name, value)
        
        elif '=' in line:
            parts = line.split('=', 1)
            var_name = parts[0].strip()
            value = parts[1].strip()
            
            try:
                if '.' in value:
                    value_num = float(value)
                else:
                    value_num = int(value)
                self.set_variable(var_name, value_num)
            except:
                if (value.startswith('"') and value.endswith('"')) or (value.startswith("'") and value.endswith("'")):
                    value = value[1:-1]
                self.set_variable(var_name, value)
        
        elif line.startswith('if '):
            condition = line[3:].strip()
            condition_result = self.evaluate_condition(condition)
            
            if not condition_result:
                self.skip_block = True
        
        elif line == 'else':
            pass
        
        elif line == 'endif':
            pass
        
        elif line == 'wait()' or line == 'pause()':
            input("\\nНажмите Enter для продолжения...")
        
        elif line == 'clear()':
            os.system('cls' if os.name == 'nt' else 'clear')
        
        elif line == 'exit()':
            self.show("Выход из программы...")
            sys.exit(0)
        
        elif line.startswith('random(') and line.endswith(')'):
            result = self.evaluate_expression(line)
            self.show(f"Случайное число: {result}")
        
        else:
            self.show(line)
    
    def run_file(self, filename):
        """Выполняет файл с кодом"""
        try:
            with open(filename, 'r', encoding='utf-8') as f:
                lines = f.readlines()
            
            if not lines:
                self.show("Ошибка: Файл пустой")
                return False
            
            first_line = lines[0].strip()
            if first_line != 'blumscript':
                self.show("Ошибка: Первая строка должна быть 'blumscript'")
                return False
            
            self.show("╔══════════════════════════╗")
            self.show("║   BLUM SCRIPT v1.0       ║")
            self.show("╚══════════════════════════╝")
            self.show(f"Файл: {os.path.basename(filename)}")
            self.show("=" * 40)
            
            self.skip_block = False
            self.in_else_block = False
            
            for i in range(1, len(lines)):
                self.parse_line(lines[i].rstrip('\\n\\r'), i + 1)
            
            self.show("=" * 40)
            self.show("✓ Выполнение завершено!")
            
            input("\\nНажмите Enter для выхода...")
            return True
            
        except FileNotFoundError:
            self.show(f"Ошибка: Файл не найден - {filename}")
            input("Нажмите Enter для выхода...")
            return False
        except Exception as e:
            self.show(f"Ошибка: {e}")
            input("Нажмите Enter для выхода...")
            return False

def main():
    interpreter = BlumInterpreter()
    
    if len(sys.argv) < 2:
        interpreter.show("Использование: blum <файл.blum>")
        interpreter.show("")
        interpreter.show("Примеры в папке: C:\\\\Program Files\\\\BlumScript\\\\examples")
        interpreter.show("Документация: C:\\\\Program Files\\\\BlumScript\\\\docs")
        interpreter.show("")
        input("Нажмите Enter для выхода...")
        return 1
    
    filename = sys.argv[1]
    
    if not filename.endswith('.blum'):
        interpreter.show(f"Предупреждение: Ожидалось .blum, получено: {filename}")
        response = input("Продолжить? (y/n): ")
        if response.lower() != 'y':
            return 1
    
    success = interpreter.run_file(filename)
    return 0 if success else 1

if __name__ == "__main__":
    sys.exit(main())
'''
    
    temp_dir = tempfile.mkdtemp(prefix="blum_build_")
    print(f"Build directory: {temp_dir}")
    
    python_file = os.path.join(temp_dir, "blum_interpreter.py")
    with open(python_file, "w", encoding='utf-8') as f:
        f.write(python_code)
    
    try:
        import PyInstaller
        pyinstaller_available = True
        print("PyInstaller is available")
    except ImportError:
        print("PyInstaller not found, installing...")
        try:
            subprocess.check_call([sys.executable, "-m", "pip", "install", "pyinstaller"], 
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            pyinstaller_available = True
            print("PyInstaller installed successfully")
        except:
            print("Failed to install PyInstaller")
            pyinstaller_available = False
    
    if pyinstaller_available:
        try:
            print("Compiling with PyInstaller...")
            
            cmd = [
                sys.executable,
                "-m", "PyInstaller",
                "--onefile",
                "--name=blum",
                "--noconfirm",
                "--clean",
                f"--workpath={os.path.join(temp_dir, 'build')}",
                f"--distpath={os.path.join(temp_dir, 'dist')}",
                f"--specpath={temp_dir}",
                "--console",
                python_file
            ]
            
            print(f"Command: {' '.join(cmd)}")
            
            result = subprocess.run(cmd, capture_output=True, text=True, cwd=temp_dir)
            
            if result.returncode == 0:
                exe_path = os.path.join(temp_dir, "dist", "blum.exe")
                if os.path.exists(exe_path):
                    file_size = os.path.getsize(exe_path)
                    print("SUCCESS: EXE created!")
                    print(f"Location: {exe_path}")
                    print(f"Size: {file_size:,} bytes")
                    
                    final_exe = os.path.join(tempfile.gettempdir(), "blum_installer_temp.exe")
                    shutil.copy2(exe_path, final_exe)
                    
                    try:
                        shutil.rmtree(temp_dir)
                        print("Temporary files cleaned")
                    except:
                        pass
                    
                    return final_exe
                else:
                    print(f"ERROR: EXE not found after compilation")
                    print(f"Expected: {exe_path}")
            else:
                print(f"ERROR: PyInstaller compilation failed")
                if result.stderr:
                    print(f"Error: {result.stderr[:200]}...")
        
        except Exception as e:
            print(f"ERROR: PyInstaller error: {e}")
    
    print("\\nFalling back to simple interpreter...")
    return create_simple_interpreter()

def create_simple_interpreter():
    temp_dir = tempfile.gettempdir()
    
    python_code = '''import sys
import os
import random

print("Blum Script v1.0 (Simple Interpreter)")

if len(sys.argv) < 2:
    print("Usage: blum <filename.blum>")
    sys.exit(1)

filename = sys.argv[1]

if not os.path.exists(filename):
    print(f"Error: File not found: {filename}")
    sys.exit(1)

exec(open(filename, 'r', encoding='utf-8').read())'''
    
    py_file = os.path.join(temp_dir, "blum_interpreter.py")
    with open(py_file, "w", encoding='utf-8') as f:
        f.write(python_code)
    
    bat_file = os.path.join(temp_dir, "blum.bat")
    with open(bat_file, "w", encoding='utf-8') as f:
        f.write(f'@echo off\\n"{sys.executable}" "{py_file}" %*\\n')
    
    print(f"Created BAT file: {bat_file}")
    return bat_file

def install():
    print("=== Blum Script Installer ===")
    print("Installing programming language...")
    
    install_path = r"C:\\Program Files\\BlumScript"
    
    print("Creating folders...")
    os.makedirs(install_path, exist_ok=True)
    os.makedirs(os.path.join(install_path, "bin"), exist_ok=True)
    os.makedirs(os.path.join(install_path, "examples"), exist_ok=True)
    os.makedirs(os.path.join(install_path, "docs"), exist_ok=True)
    os.makedirs(os.path.join(install_path, "tutorial"), exist_ok=True)
    
    exe_path = compile_interpreter()
    
    final_exe = os.path.join(install_path, "bin", "blum.exe")
    
    if exe_path and os.path.exists(exe_path):
        try:
            shutil.copy2(exe_path, final_exe)
            print(f"SUCCESS: EXE installed: {final_exe}")
            print(f"Size: {os.path.getsize(final_exe):,} bytes")
        except Exception as e:
            print(f"ERROR: Failed to copy EXE: {e}")
            bat_file = create_simple_interpreter()
            if bat_file and os.path.exists(bat_file):
                bat_dest = os.path.join(install_path, "bin", "blum.bat")
                shutil.copy2(bat_file, bat_dest)
                final_exe = bat_dest
                print(f"SUCCESS: BAT file installed: {bat_dest}")
    else:
        print("WARNING: EXE compilation failed, using BAT interpreter")
        bat_file = create_simple_interpreter()
        if bat_file and os.path.exists(bat_file):
            bat_dest = os.path.join(install_path, "bin", "blum.bat")
            shutil.copy2(bat_file, bat_dest)
            final_exe = bat_dest
            print(f"SUCCESS: BAT file installed: {bat_dest}")
    
    print("\\nRegistering .blum extension...")
    try:
        with winreg.CreateKey(winreg.HKEY_CLASSES_ROOT, ".blum") as key:
            winreg.SetValue(key, "", winreg.REG_SZ, "BlumScriptFile")
        
        with winreg.CreateKey(winreg.HKEY_CLASSES_ROOT, "BlumScriptFile") as key:
            winreg.SetValue(key, "", winreg.REG_SZ, "Blum Script File")
            
            with winreg.CreateKey(key, r"shell\\open\\command") as cmd_key:
                winreg.SetValue(cmd_key, "", winreg.REG_SZ, f'"{final_exe}" "%1"')
        
        ctypes.windll.shell32.SHChangeNotify(0x08000000, 0x0000, None, None)
        print("SUCCESS: .blum extension registered")
    except Exception as e:
        print(f"ERROR: Could not register extension: {e}")
    
    print("\\nAdding to system PATH...")
    try:
        bin_path = os.path.join(install_path, "bin")
        
        key = winreg.OpenKey(winreg.HKEY_CURRENT_USER, "Environment", 0, winreg.KEY_READ | winreg.KEY_WRITE)
        
        try:
            current_path, reg_type = winreg.QueryValueEx(key, "Path")
        except FileNotFoundError:
            current_path = ""
            reg_type = winreg.REG_EXPAND_SZ
        
        paths = [p.strip() for p in current_path.split(';') if p.strip()]
        
        if bin_path not in paths:
            if current_path and not current_path.endswith(';'):
                current_path += ';'
            new_path = current_path + bin_path
            winreg.SetValueEx(key, "Path", 0, reg_type, new_path)
            print(f"SUCCESS: Added to PATH: {bin_path}")
        else:
            print("SUCCESS: Already in PATH")
        
        winreg.CloseKey(key)
        
    except Exception as e:
        print(f"ERROR: Could not add to PATH: {e}")
        print("\\nЧтобы команда 'blum' работала, выполните:")
        print(f'1. Откройте новый терминал')
        print(f'2. Или выполните: setx PATH "%PATH%;{bin_path}"')
    
    print("\\nCreating examples...")
    
    examples = [
        ("game.blum", '''blumscript
show("=== УГАДАЙ ЧИСЛО ===")
show("Я загадал число от 1 до 10!")
show("Попробуй угадать!")

secret = {random(1, 10)}

guess = input("Твоя догадка (1-10): ")
guess_num = {guess}

if guess_num == secret
    show("🎉 Поздравляю! Ты угадал!")
    show("Ты победил!")
else
    show("😞 Не угадал!")
    show("Секретное число было: {secret}")
endif

show("")
show("=== ИГРА ОКОНЧЕНА ===")
wait()'''),
        
        ("calculator.blum", '''blumscript
show("=== КАЛЬКУЛЯТОР ===")

a = input("Введите первое число: ")
b = input("Введите второе число: ")

num1 = {a}
num2 = {b}

show("")
show("📊 РЕЗУЛЬТАТЫ:")
show("{num1} + {num2} = {num1 + num2}")
show("{num1} - {num2} = {num1 - num2}")
show("{num1} * {num2} = {num1 * num2}")

if num2 != 0
    show("{num1} / {num2} = {num1 / num2}")
else
    show("{num1} / {num2} = 🚫 Деление на ноль!")
endif

show("")
show("✅ Калькулятор завершил работу!")
wait()'''),
        
        ("hello.blum", '''blumscript
show("Привет, мир!")
show("Это Blum Script v1.0")

name = input("Как тебя зовут? ")
show("Привет, {name}!")

age = input("Сколько тебе лет? ")
show("{name}, тебе {age} лет!")

show("")
show("Программа завершена!")
wait()''')
    ]
    
    for filename, content in examples:
        example_path = os.path.join(install_path, "examples", filename)
        with open(example_path, "w", encoding='utf-8') as f:
            f.write(content)
        print(f"Created: {filename}")
    
    print("\\nCreating documentation (docs folder)...")
    
    docs_files = [
        ("README.txt", '''BLUM SCRIPT - ЯЗЫК ПРОГРАММИРОВАНИЯ

Что такое Blum Script?
Простой интерпретируемый язык для обучения программированию.

Быстрый старт:
1. Создайте файл с расширением .blum
2. Первая строка: blumscript
3. Напишите код
4. Запустите: blum файл.blum

Пример:
blumscript
show("Привет, мир!")
name = input("Твое имя? ")
show("Привет, {name}!")
wait()

Папки:
Примеры:    C:\\Program Files\\BlumScript\\examples
Документы:  C:\\Program Files\\BlumScript\\docs
Уроки:      C:\\Program Files\\BlumScript\\tutorial'''),
        
        ("SYNTAX.txt", '''СИНТАКСИС BLUM SCRIPT

Комментарии:
// Это комментарий

Вывод:
show("Текст")          // Простой вывод
show("Значение: {x}") // Вывод с переменной

Ввод:
input("Вопрос: ")             // Простой ввод
name = input("Ваше имя: ")    // Ввод с сохранением

Переменные:
x = 10                // Число
name = "Иван"         // Строка
result = {x + 5}      // Вычисление

Условия:
if условие
    // код если условие ИСТИНА
endif

Пример:
age = input("Сколько лет? ")
a = {age}

if a >= 18
    show("Совершеннолетний")
endif

Случайные числа:
random(10)          // От 1 до 10
random(1, 100)      // От 1 до 100
num = {random(50)}  // Сохранить в переменную

Специальные команды:
wait()    // Ожидание Enter
clear()   // Очистка экрана
exit()    // Выход'''),
        
        ("TUTORIAL.txt", '''УРОКИ BLUM SCRIPT

Урок 1: Первая программа
--------------------------------
Создайте hello.blum:
blumscript
show("Привет, мир!")
wait()

Урок 2: Переменные и ввод
--------------------------------
blumscript
name = input("Как тебя зовут? ")
age = input("Сколько лет? ")
show("Привет, {name}! Тебе {age} лет.")
wait()

Урок 3: Условия
--------------------------------
blumscript
number = input("Введите число: ")
num = {number}

if num > 0
    show("Положительное")
endif

if num == 0
    show("Ноль")
endif

wait()

Урок 4: Калькулятор
--------------------------------
blumscript
show("Калькулятор")
a = input("Первое число: ")
b = input("Второе число: ")

x = {a}
y = {b}

show("{x} + {y} = {x + y}")
show("{x} - {y} = {x - y}")
wait()

Урок 5: Игра
--------------------------------
blumscript
show("Угадай число от 1 до 10")
secret = {random(1, 10)}

guess = input("Твоя догадка: ")
g = {guess}

if g == secret
    show("Победа!")
endif

if g != secret
    show("Проигрыш! Число: {secret}")
endif
wait()'''),
        
        ("QUICK_START.txt", '''БЫСТРЫЙ СТАРТ - BLUM SCRIPT

1. СОЗДАЙТЕ ФАЙЛ:
   - Откройте Блокнот
   - Сохраните как: программа.blum

2. ПИШИТЕ КОД:
   blumscript
   show("Моя программа")
   name = input("Имя: ")
   show("Привет, {name}!")
   wait()

3. ЗАПУСК:
   - Откройте командную строку
   - Введите: blum программа.blum

4. КОМАНДЫ:
   show("текст")     - Вывод
   input("вопрос")   - Ввод
   x = значение      - Переменная
   if условие        - Условие
   wait()            - Пауза

5. ПРИМЕРЫ:
   C:\\Program Files\\BlumScript\\examples
   - game.blum       - Игра
   - calculator.blum - Калькулятор
   - hello.blum      - Приветствие

6. СПРАВКА:
   blum              - Без параметров покажет помощь''')
    ]
    
    for filename, content in docs_files:
        doc_path = os.path.join(install_path, "docs", filename)
        with open(doc_path, "w", encoding='utf-8') as f:
            f.write(content)
        print(f"  Created doc: {filename}")
    
    tutorial_files = [
        ("lesson1.blum", '''blumscript
// Урок 1: Первая программа
show("Привет, мир!")
show("Это моя первая программа на Blum Script!")
wait()'''),
        
        ("lesson2.blum", '''blumscript
// Урок 2: Переменные
name = "Алексей"
age = 25
show("Имя: {name}")
show("Возраст: {age}")
wait()'''),
        
        ("lesson3.blum", '''blumscript
// Урок 3: Ввод данных
name = input("Как тебя зовут? ")
show("Привет, {name}!")
wait()''')
    ]
    
    for filename, content in tutorial_files:
        tutorial_path = os.path.join(install_path, "tutorial", filename)
        with open(tutorial_path, "w", encoding='utf-8') as f:
            f.write(content)
        print(f"  Created tutorial: {filename}")
    
    info = {
        "name": "Blum Script",
        "version": "1.0",
        "install_path": install_path,
        "install_date": time.strftime("%Y-%m-%d %H:%M:%S"),
        "docs": [doc[0] for doc in docs_files],
        "examples": [ex[0] for ex in examples],
        "tutorial": [tut[0] for tut in tutorial_files]
    }
    
    info_path = os.path.join(install_path, "blum_info.json")
    with open(info_path, "w", encoding='utf-8') as f:
        json.dump(info, f, indent=2, ensure_ascii=False)
    
    print("\\n" + "="*60)
    print("✅ УСТАНОВКА ЗАВЕРШЕНА!")
    print("="*60)
    print(f"📍 Путь: {install_path}")
    print(f"🖥️  Интерпретатор: {os.path.basename(final_exe)}")
    print(f"📁 Примеры: {install_path}\\examples")
    print(f"📚 Документация: {install_path}\\docs")
    print(f"🎓 Уроки: {install_path}\\tutorial")
    
    print("\\n🚀 Тестирование:")
    print(f'   blum "{install_path}\\examples\\hello.blum"')
    
    print("\\n⚠️  Если команда 'blum' не работает:")
    print(f'   1. Перезапустите командную строку')
    print(f'   2. Или введите: {final_exe} "{install_path}\\examples\\hello.blum"')
    
    print("\\n" + "="*60)
    
    input("\\nНажмите Enter для завершения...")

def main():
    if not is_admin():
        print("Запрос прав администратора...")
        run_as_admin()
    else:
        install()

if __name__ == "__main__":
    if os.name != 'nt':
        print("Этот установщик только для Windows")
        input("Нажмите Enter для выхода...")
    else:
        main()
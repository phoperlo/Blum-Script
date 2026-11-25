//! Lexer for BlumScript.

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Int(i64),
    Float(f64),
    Str(String),
    Char(char),
    Ident(String),

    // keywords
    Let,
    Var,
    Func,
    If,
    Else,
    While,
    For,
    Return,
    Break,
    Continue,
    True,
    False,
    Null,
    Struct,
    Import,
    Asm,
    AsmRaw(String),
    Sizeof,
    Cast,
    And,
    Or,
    Not,

    // punctuation / operators
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Semi,
    Comma,
    Colon,
    Dot,
    Arrow,
    Assign,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Lt,
    Gt,
    Le,
    Ge,
    EqEq,
    NotEq,
    AmpAmp,
    PipePipe,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Shl,
    Shr,
    Eof,
}

pub fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    let mut out: Vec<Tok> = Vec::new();

    while i < n {
        let c = chars[i];

        if c.is_whitespace() || c == '\u{feff}' {
            i += 1;
            continue;
        }

        // comments
        if c == '/' && i + 1 < n && chars[i + 1] == '/' {
            i += 2;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            i += 2;
            loop {
                if i + 1 >= n {
                    return Err("unterminated block comment".into());
                }
                if chars[i] == '*' && chars[i + 1] == '/' {
                    i += 2;
                    break;
                }
                i += 1;
            }
            continue;
        }

        // string
        if c == '"' {
            i += 1;
            let mut s = String::new();
            while i < n && chars[i] != '"' {
                if chars[i] == '\\' && i + 1 < n {
                    i += 1;
                    match chars[i] {
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        'r' => s.push('\r'),
                        '0' => s.push('\0'),
                        '\\' => s.push('\\'),
                        '"' => s.push('"'),
                        other => s.push(other),
                    }
                    i += 1;
                } else {
                    s.push(chars[i]);
                    i += 1;
                }
            }
            if i >= n {
                return Err("unterminated string literal".into());
            }
            i += 1;
            out.push(Tok::Str(s));
            continue;
        }

        // char
        if c == '\'' {
            i += 1;
            if i >= n {
                return Err("unterminated char literal".into());
            }
            let ch = if chars[i] == '\\' {
                i += 1;
                if i >= n {
                    return Err("unterminated char literal".into());
                }
                let e = match chars[i] {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    '0' => '\0',
                    '\\' => '\\',
                    '\'' => '\'',
                    other => other,
                };
                i += 1;
                e
            } else {
                let ch = chars[i];
                i += 1;
                ch
            };
            if i >= n || chars[i] != '\'' {
                return Err("unterminated char literal".into());
            }
            i += 1;
            out.push(Tok::Char(ch));
            continue;
        }

        // number
        if c.is_ascii_digit() {
            let start = i;
            while i < n && chars[i].is_ascii_digit() {
                i += 1;
            }
            // hex
            if i < n && (chars[i] == 'x' || chars[i] == 'X') && i + 1 < n && chars[i + 1].is_ascii_hexdigit() {
                i += 1;
                while i < n && chars[i].is_ascii_hexdigit() {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                let val = i64::from_str_radix(&text[2..], 16).map_err(|e| e.to_string())?;
                out.push(Tok::Int(val));
                continue;
            }
            let mut is_float = false;
            if i < n && chars[i] == '.' && i + 1 < n && chars[i + 1].is_ascii_digit() {
                is_float = true;
                i += 1;
                while i < n && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let text: String = chars[start..i].iter().collect();
            if is_float {
                out.push(Tok::Float(text.parse::<f64>().map_err(|e| e.to_string())?));
            } else {
                out.push(Tok::Int(text.parse::<i64>().map_err(|e| e.to_string())?));
            }
            continue;
        }

        // identifier / keyword
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();

            // raw asm block: asm { ... } captures verbatim text
            if word == "asm" {
                let mut j = i;
                while j < n && chars[j].is_whitespace() {
                    j += 1;
                }
                if j < n && chars[j] == '{' {
                    j += 1;
                    let body_start = j;
                    let mut depth = 1i32;
                    while j < n {
                        if chars[j] == '{' {
                            depth += 1;
                        } else if chars[j] == '}' {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        j += 1;
                    }
                    if depth != 0 {
                        return Err("unterminated asm block".into());
                    }
                    let raw: String = chars[body_start..j].iter().collect();
                    out.push(Tok::AsmRaw(raw));
                    i = j + 1;
                    continue;
                }
                out.push(Tok::Asm);
                continue;
            }

            let tok = match word.as_str() {
                "let" => Tok::Let,
                "var" => Tok::Var,
                "func" => Tok::Func,
                "if" => Tok::If,
                "else" => Tok::Else,
                "while" => Tok::While,
                "for" => Tok::For,
                "return" => Tok::Return,
                "break" => Tok::Break,
                "continue" => Tok::Continue,
                "true" => Tok::True,
                "false" => Tok::False,
                "null" => Tok::Null,
                "struct" => Tok::Struct,
                "import" => Tok::Import,
                "asm" => Tok::Asm,
                "sizeof" => Tok::Sizeof,
                "cast" => Tok::Cast,
                "and" => Tok::And,
                "or" => Tok::Or,
                "not" => Tok::Not,
                _ => Tok::Ident(word),
            };
            out.push(tok);
            continue;
        }

        // multi-char operators first
        let two: Option<Tok> = if i + 1 < n {
            match (c, chars[i + 1]) {
                ('=', '=') => Some(Tok::EqEq),
                ('!', '=') => Some(Tok::NotEq),
                ('<', '=') => Some(Tok::Le),
                ('>', '=') => Some(Tok::Ge),
                ('&', '&') => Some(Tok::AmpAmp),
                ('|', '|') => Some(Tok::PipePipe),
                ('<', '<') => Some(Tok::Shl),
                ('>', '>') => Some(Tok::Shr),
                ('-', '>') => Some(Tok::Arrow),
                _ => None,
            }
        } else {
            None
        };
        if let Some(t) = two {
            out.push(t);
            i += 2;
            continue;
        }

        let tok = match c {
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            '{' => Tok::LBrace,
            '}' => Tok::RBrace,
            '[' => Tok::LBracket,
            ']' => Tok::RBracket,
            ';' => Tok::Semi,
            ',' => Tok::Comma,
            ':' => Tok::Colon,
            '.' => Tok::Dot,
            '=' => Tok::Assign,
            '+' => Tok::Plus,
            '-' => Tok::Minus,
            '*' => Tok::Star,
            '/' => Tok::Slash,
            '%' => Tok::Percent,
            '<' => Tok::Lt,
            '>' => Tok::Gt,
            '&' => Tok::Amp,
            '|' => Tok::Pipe,
            '^' => Tok::Caret,
            '~' => Tok::Tilde,
            '!' => Tok::Not,
            other => return Err(format!("unexpected character '{}'", other)),
        };
        out.push(tok);
        i += 1;
    }

    out.push(Tok::Eof);
    Ok(out)
}

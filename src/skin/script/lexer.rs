//! Tokens. The lexer runs on demand, one token at a time, because only the
//! parser knows whether a `/` starts a regular expression.

use std::collections::HashSet;
use std::sync::Arc;

use super::{Name, ScriptError};

/// Interning gives repeated names the same pointer, which makes scope lookups
/// cheap. The cap bounds what `eval` of generated names can pin in memory;
/// past it names still work, they just compare by content.
#[derive(Default)]
pub(super) struct Interner(HashSet<Name>);

const MAX_INTERNED: usize = 1 << 16;

impl Interner {
    pub fn get(&mut self, s: &str) -> Name {
        if let Some(name) = self.0.get(s) {
            return name.clone();
        }
        let name: Name = Arc::from(s);
        if self.0.len() < MAX_INTERNED {
            self.0.insert(name.clone());
        }
        name
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Tok {
    Eof,
    Ident(Name),
    Kw(&'static str),
    P(&'static str),
    Num(f64),
    Str(Name),
}

const KEYWORDS: &[&str] = &[
    "break",
    "case",
    "catch",
    "continue",
    "default",
    "delete",
    "do",
    "else",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
];

/// Longest first, so the first prefix that matches is the right token.
const PUNCTUATORS: &[&str] = &[
    ">>>=", "===", "!==", ">>>", "<<=", ">>=", "==", "!=", "<=", ">=", "&&", "||", "++", "--",
    "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<", ">>", "{", "}", "(", ")", "[", "]", ".",
    ";", ",", "<", ">", "+", "-", "*", "/", "%", "&", "|", "^", "!", "~", "?", ":", "=",
];

pub(super) struct Lexer<'a> {
    src: &'a str,
    pub pos: usize,
    pub line: u32,
    names: &'a mut Interner,
}

fn is_line_end(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn is_ident_start(c: char) -> bool {
    c == '$' || c == '_' || c.is_alphabetic()
}

fn is_ident_part(c: char) -> bool {
    is_ident_start(c) || c.is_alphanumeric()
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str, names: &'a mut Interner) -> Self {
        Lexer {
            src,
            pos: 0,
            line: 1,
            names,
        }
    }

    pub fn name(&mut self, s: &str) -> Name {
        self.names.get(s)
    }

    fn error<T>(&self, message: &str) -> Result<T, ScriptError> {
        Err(ScriptError::Syntax {
            line: self.line,
            message: message.to_string(),
        })
    }

    fn rest(&self) -> &'a str {
        self.src.get(self.pos..).unwrap_or("")
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        if is_line_end(c) && !(c == '\r' && self.peek() == Some('\n')) {
            self.line = self.line.saturating_add(1);
        }
        Some(c)
    }

    /// Skips whitespace and comments, and says whether a line ended on the way.
    fn skip_trivia(&mut self) -> Result<bool, ScriptError> {
        let mut newline = self.pos == 0;
        while let Some(c) = self.peek() {
            let rest = self.rest();
            if is_line_end(c) {
                newline = true;
                self.bump();
            } else if c.is_whitespace() || c == '\u{feff}' {
                self.bump();
            } else if rest.starts_with("//")
                || rest.starts_with("<!--")
                || (rest.starts_with("-->") && newline)
            {
                while self.peek().is_some_and(|c| !is_line_end(c)) {
                    self.bump();
                }
            } else if rest.starts_with("/*") {
                self.pos += 2;
                loop {
                    if self.rest().starts_with("*/") {
                        self.pos += 2;
                        break;
                    }
                    match self.bump() {
                        Some(c) => newline |= is_line_end(c),
                        None => return self.error("unterminated comment"),
                    }
                }
            } else {
                break;
            }
        }
        Ok(newline)
    }

    /// The next token and whether a line ended before it.
    pub fn next(&mut self) -> Result<(Tok, bool), ScriptError> {
        let newline = self.skip_trivia()?;
        let Some(c) = self.peek() else {
            return Ok((Tok::Eof, newline));
        };
        let token = if is_ident_start(c) {
            self.word()
        } else if c.is_ascii_digit()
            || (c == '.'
                && self
                    .rest()
                    .as_bytes()
                    .get(1)
                    .is_some_and(u8::is_ascii_digit))
        {
            self.number()?
        } else if c == '"' || c == '\'' {
            self.string(c)?
        } else {
            let rest = self.rest();
            match PUNCTUATORS.iter().find(|p| rest.starts_with(**p)) {
                Some(p) => {
                    self.pos += p.len();
                    Tok::P(p)
                }
                None => return self.error("unexpected character"),
            }
        };
        Ok((token, newline))
    }

    fn word(&mut self) -> Tok {
        let start = self.pos;
        while self.peek().is_some_and(is_ident_part) {
            self.bump();
        }
        let word = self.src.get(start..self.pos).unwrap_or("");
        match KEYWORDS.iter().find(|k| **k == word) {
            Some(k) => Tok::Kw(k),
            None => Tok::Ident(self.names.get(word)),
        }
    }

    fn digits(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
        }
    }

    fn number(&mut self) -> Result<Tok, ScriptError> {
        let start = self.pos;
        let rest = self.rest();
        let value = if rest.starts_with("0x") || rest.starts_with("0X") {
            self.pos += 2;
            let mut value = 0.0;
            let mut any = false;
            while let Some(d) = self.peek().and_then(|c| c.to_digit(16)) {
                value = value * 16.0 + f64::from(d);
                any = true;
                self.bump();
            }
            if !any {
                return self.error("bad hexadecimal number");
            }
            value
        } else {
            self.digits();
            let text = self.src.get(start..self.pos).unwrap_or("");
            if text.len() > 1 && text.starts_with('0') && text.bytes().all(|b| b < b'8') {
                // A legacy octal literal.
                text.bytes().fold(0.0, |v, b| v * 8.0 + f64::from(b - b'0'))
            } else {
                if self.peek() == Some('.') {
                    self.bump();
                    self.digits();
                }
                if matches!(self.peek(), Some('e' | 'E')) {
                    let mark = self.pos;
                    self.bump();
                    if matches!(self.peek(), Some('+' | '-')) {
                        self.bump();
                    }
                    if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                        self.digits();
                    } else {
                        self.pos = mark;
                    }
                }
                let text = self.src.get(start..self.pos).unwrap_or("");
                text.parse().unwrap_or(f64::NAN)
            }
        };
        if self.peek().is_some_and(is_ident_start) {
            return self.error("identifier directly after a number");
        }
        Ok(Tok::Num(value))
    }

    fn hex(&mut self, count: usize) -> Option<u32> {
        let mark = self.pos;
        let mut value = 0;
        for _ in 0..count {
            match self.peek().and_then(|c| c.to_digit(16)) {
                Some(d) => value = value * 16 + d,
                None => {
                    self.pos = mark;
                    return None;
                }
            }
            self.bump();
        }
        Some(value)
    }

    fn escape(&mut self) -> Result<Option<u32>, ScriptError> {
        let Some(e) = self.bump() else {
            return self.error("unterminated string");
        };
        Ok(Some(match e {
            'n' => 10,
            't' => 9,
            'r' => 13,
            'b' => 8,
            'v' => 11,
            'f' => 12,
            '0'..='7' => {
                let mut value = e.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    match self.peek().and_then(|c| c.to_digit(8)) {
                        Some(d) if value * 8 + d < 256 => {
                            value = value * 8 + d;
                            self.bump();
                        }
                        _ => break,
                    }
                }
                value
            }
            'x' => self.hex(2).unwrap_or('x' as u32),
            'u' => self.hex(4).unwrap_or('u' as u32),
            '\r' => {
                if self.peek() == Some('\n') {
                    self.bump();
                }
                return Ok(None);
            }
            '\n' | '\u{2028}' | '\u{2029}' => return Ok(None),
            other => other as u32,
        }))
    }

    fn string(&mut self, quote: char) -> Result<Tok, ScriptError> {
        self.bump();
        let mut out = String::new();
        // A high surrogate escape waits here for its low half.
        let mut pending: Option<u32> = None;
        loop {
            let Some(c) = self.bump() else {
                return self.error("unterminated string");
            };
            if is_line_end(c) {
                return self.error("unterminated string");
            }
            if c == quote {
                break;
            }
            let code = if c == '\\' {
                match self.escape()? {
                    Some(code) => code,
                    None => continue,
                }
            } else {
                c as u32
            };
            if let Some(high) = pending.take() {
                if (0xdc00..0xe000).contains(&code) {
                    let pair = 0x10000 + ((high - 0xd800) << 10) + (code - 0xdc00);
                    out.push(char::from_u32(pair).unwrap_or('\u{fffd}'));
                    continue;
                }
                out.push('\u{fffd}');
            }
            if (0xd800..0xdc00).contains(&code) {
                pending = Some(code);
            } else {
                out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
            }
        }
        if pending.is_some() {
            out.push('\u{fffd}');
        }
        Ok(Tok::Str(self.names.get(&out)))
    }

    /// Rescans from `start`, where the parser saw a `/` in operand position.
    pub fn regex(&mut self, start: usize) -> Result<(Name, Name), ScriptError> {
        self.pos = start + 1;
        let mut in_class = false;
        loop {
            let Some(c) = self.bump() else {
                return self.error("unterminated regular expression");
            };
            match c {
                c if is_line_end(c) => return self.error("unterminated regular expression"),
                '\\' => {
                    if self.bump().is_none_or(is_line_end) {
                        return self.error("unterminated regular expression");
                    }
                }
                '[' => in_class = true,
                ']' => in_class = false,
                '/' if !in_class => break,
                _ => {}
            }
        }
        let body = self.src.get(start + 1..self.pos - 1).unwrap_or("");
        let body = self.names.get(body);
        let flags = self.pos;
        while self.peek().is_some_and(is_ident_part) {
            self.bump();
        }
        let flags = self.src.get(flags..self.pos).unwrap_or("");
        Ok((body, self.names.get(flags)))
    }
}

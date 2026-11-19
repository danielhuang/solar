//! Tokens retain their origin so anonymous record identities survive reimports.

use std::sync::Arc;

#[derive(Clone, Debug)]
pub(super) struct Token {
    pub text: String,
    pub file: Arc<str>,
    pub offset: usize,
    pub spaced: bool,
    pub hidden: Vec<String>,
}

impl Token {
    pub fn with_text(&self, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..self.clone()
        }
    }

    pub fn location(&self) -> String {
        format!("{}:{}", self.file, self.offset)
    }
}

pub(super) fn ident(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

pub(super) fn lex(source: &str, file: Arc<str>) -> Result<Vec<Token>, String> {
    let bytes = source.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;
    let mut spaced = false;
    while i < bytes.len() {
        let start = i;
        match bytes[i] {
            b'\n' => i += 1,
            c if c.is_ascii_whitespace() => {
                i += 1;
                spaced = true;
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                spaced = true;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                spaced = true;
                i += 2;
                while i + 1 < bytes.len() && &bytes[i..i + 2] != b"*/" {
                    i += 1;
                }
                if i + 1 >= bytes.len() {
                    return Err(format!("{file}: unterminated C comment"));
                }
                i += 2;
                continue;
            }
            b'"' | b'\'' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                if i >= bytes.len() {
                    return Err(format!("{file}: unterminated C literal"));
                }
                i += 1;
            }
            c if c.is_ascii_digit()
                || (c == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)) =>
            {
                i += 1;
                while i < bytes.len() {
                    let c = bytes[i];
                    if c.is_ascii_alphanumeric()
                        || c == b'.'
                        || c == b'_'
                        || ((c == b'+' || c == b'-')
                            && matches!(bytes[i - 1], b'e' | b'E' | b'p' | b'P'))
                    {
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
            }
            _ => {
                i += 1;
                for operator in [
                    "...", "<<=", ">>=", "##", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||",
                    "->", "++", "--", "+=", "-=", "*=", "/=", "&=", "|=", "^=",
                ] {
                    if source[start..].starts_with(operator) {
                        i = start + operator.len();
                        break;
                    }
                }
                if !source.is_char_boundary(i) {
                    return Err(format!("{file}: unsupported character in C source"));
                }
            }
        }
        result.push(Token {
            text: source[start..i].into(),
            file: file.clone(),
            offset: start,
            spaced,
            hidden: Vec::new(),
        });
        spaced = bytes[start] == b'\n';
    }
    Ok(result)
}

pub(super) struct Cursor<'a> {
    pub tokens: &'a [Token],
    pub pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(tokens: &'a [Token]) -> Self {
        Self { tokens, pos: 0 }
    }
    pub fn peek(&self) -> &str {
        self.tokens.get(self.pos).map_or("", |t| t.text.as_str())
    }
    pub fn eat(&mut self, text: &str) -> bool {
        if self.peek() == text {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    pub fn expect(&mut self, text: &str) -> Result<(), String> {
        if self.eat(text) {
            Ok(())
        } else {
            Err(format!(
                "expected `{text}` in C declaration, found `{}`",
                self.peek()
            ))
        }
    }
    pub fn take(&mut self) -> Result<&'a Token, String> {
        let token = self
            .tokens
            .get(self.pos)
            .ok_or("unexpected end of C declaration")?;
        self.pos += 1;
        Ok(token)
    }
    /// Reads up to a delimiter outside balanced brackets, leaving it unconsumed.
    pub fn until(&mut self, delimiters: &[&str]) -> Result<&'a [Token], String> {
        let start = self.pos;
        let mut stack = Vec::new();
        while self.pos < self.tokens.len() {
            let text = self.peek();
            if stack.is_empty() && delimiters.contains(&text) {
                break;
            }
            match text {
                "(" => stack.push(")"),
                "[" => stack.push("]"),
                "{" => stack.push("}"),
                ")" | "]" | "}" if stack.pop() != Some(text) => {
                    return Err(format!("unbalanced `{text}` in C declaration"));
                }
                _ => {}
            }
            self.pos += 1;
        }
        if !stack.is_empty() {
            return Err("unclosed bracket in C declaration".into());
        }
        Ok(&self.tokens[start..self.pos])
    }
}

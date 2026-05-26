//! Key-token grammar for `press`.
//!
//! Bash-style angle-bracket tokens mixed with literal text:
//!
//! ```text
//! i hello<esc>:w<cr>
//! ```
//!
//! parses to `[Lit("i hello"), Esc, Lit(":w"), Cr]` and serializes to the
//! bytes a terminal expects. Unknown `<...>` tokens are rejected — that's
//! what `KEY_FORMAT_ERROR` is for upstream.
//!
//! See `docs/RFC.md` §5.1 and `skill-data/core/references/keymap.md`.

use std::fmt;

/// One token in a parsed key sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// Literal text run.
    Lit(String),
    /// `<cr>` — carriage return (0x0D).
    Cr,
    /// `<lf>` — line feed (0x0A).
    Lf,
    /// `<tab>` — horizontal tab (0x09).
    Tab,
    /// `<bs>` — backspace (0x7F per VT convention; not 0x08).
    Bs,
    /// `<esc>` — escape (0x1B).
    Esc,
    /// `<space>` — single space, useful inside `<…>` boundaries.
    Space,
    /// `<c-X>` — Ctrl + ASCII letter.
    Ctrl(char),
    /// `<a-X>` — Alt + ASCII char (encoded as ESC prefix).
    Alt(char),
    /// `<f1>` .. `<f12>` — function keys.
    Fn(u8),
    /// `<up>` / `<down>` / `<left>` / `<right>` — cursor arrows.
    Arrow(Arrow),
    /// `<home>` / `<end>`.
    Home,
    /// See [`Token::Home`].
    End,
    /// `<pageup>` / `<pagedown>`.
    PageUp,
    /// See [`Token::PageUp`].
    PageDown,
    /// `<del>` — delete-forward.
    Del,
    /// `<ins>` — insert.
    Ins,
}

/// Cursor-arrow direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrow {
    /// `<up>`.
    Up,
    /// `<down>`.
    Down,
    /// `<left>`.
    Left,
    /// `<right>`.
    Right,
}

/// Key-format parse failure. Maps to `KEY_FORMAT_ERROR` upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyFormatError {
    /// Human-readable reason.
    pub reason: String,
    /// Byte offset in the input where parsing failed.
    pub offset: usize,
}

impl fmt::Display for KeyFormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at offset {})", self.reason, self.offset)
    }
}

impl std::error::Error for KeyFormatError {}

/// Parse a key-token string into a vector of [`Token`]s.
///
/// The escape sequence `<\<>` produces a literal `<` character so callers can
/// type the angle bracket. All other `<…>` shapes are recognized tokens.
///
/// # Errors
/// Returns [`KeyFormatError`] when an unknown `<…>` token is encountered or
/// when a `<` is not closed.
pub fn parse(input: &str) -> Result<Vec<Token>, KeyFormatError> {
    let bytes = input.as_bytes();
    let mut out: Vec<Token> = Vec::new();
    let mut i = 0;
    let mut lit = String::new();

    while i < bytes.len() {
        let b = bytes[i];
        if b == b'<' {
            // Try `<\<>` — literal `<`.
            if bytes.get(i + 1) == Some(&b'\\')
                && bytes.get(i + 2) == Some(&b'<')
                && bytes.get(i + 3) == Some(&b'>')
            {
                lit.push('<');
                i += 4;
                continue;
            }
            // Find matching `>`.
            let close = bytes[i + 1..]
                .iter()
                .position(|&c| c == b'>')
                .ok_or_else(|| KeyFormatError {
                    reason: "unterminated '<' token".into(),
                    offset: i,
                })?;
            let token_str = &input[i + 1..i + 1 + close];
            if !lit.is_empty() {
                out.push(Token::Lit(std::mem::take(&mut lit)));
            }
            out.push(parse_token(token_str, i)?);
            i += 1 + close + 1;
        } else {
            // Push the next char (UTF-8 aware via slicing on char boundaries).
            let ch = input[i..].chars().next().expect("non-empty");
            lit.push(ch);
            i += ch.len_utf8();
        }
    }
    if !lit.is_empty() {
        out.push(Token::Lit(lit));
    }
    Ok(out)
}

fn parse_token(s: &str, offset: usize) -> Result<Token, KeyFormatError> {
    let lower = s.to_ascii_lowercase();
    let tok = match lower.as_str() {
        "cr" | "enter" | "return" => Token::Cr,
        "lf" => Token::Lf,
        "tab" => Token::Tab,
        "bs" | "backspace" => Token::Bs,
        "esc" | "escape" => Token::Esc,
        "space" | "sp" => Token::Space,
        "up" => Token::Arrow(Arrow::Up),
        "down" => Token::Arrow(Arrow::Down),
        "left" => Token::Arrow(Arrow::Left),
        "right" => Token::Arrow(Arrow::Right),
        "home" => Token::Home,
        "end" => Token::End,
        "pageup" | "pgup" => Token::PageUp,
        "pagedown" | "pgdn" => Token::PageDown,
        "del" | "delete" => Token::Del,
        "ins" | "insert" => Token::Ins,
        _ => {
            if let Some(rest) = lower.strip_prefix("c-") {
                let ch = single_ascii_letter(rest, offset)?;
                return Ok(Token::Ctrl(ch));
            }
            if let Some(rest) = lower.strip_prefix("a-") {
                let ch = single_ascii_letter(rest, offset)?;
                return Ok(Token::Alt(ch));
            }
            if let Some(rest) = lower.strip_prefix('f') {
                if let Ok(n) = rest.parse::<u8>()
                    && (1..=12).contains(&n)
                {
                    return Ok(Token::Fn(n));
                }
            }
            return Err(KeyFormatError {
                reason: format!("unknown key token <{s}>"),
                offset,
            });
        }
    };
    Ok(tok)
}

fn single_ascii_letter(s: &str, offset: usize) -> Result<char, KeyFormatError> {
    let mut chars = s.chars();
    let first = chars.next().ok_or_else(|| KeyFormatError {
        reason: "missing key after modifier".into(),
        offset,
    })?;
    if chars.next().is_some() || !first.is_ascii_alphanumeric() {
        return Err(KeyFormatError {
            reason: format!("modifier expects a single ASCII letter, got '{s}'"),
            offset,
        });
    }
    Ok(first.to_ascii_lowercase())
}

/// Serialize a sequence of tokens to the bytes a terminal expects.
///
/// Encoding follows xterm conventions (DECSET-aware where needed) but the
/// daemon currently feeds bytes into the engine without DEC mode awareness;
/// agents that need cursor-key-mode toggles should send raw ANSI via
/// `send_ansi`.
#[must_use]
pub fn serialize(tokens: &[Token]) -> Vec<u8> {
    let mut out = Vec::new();
    for t in tokens {
        encode(t, &mut out);
    }
    out
}

fn encode(t: &Token, out: &mut Vec<u8>) {
    match t {
        Token::Lit(s) => out.extend_from_slice(s.as_bytes()),
        Token::Cr => out.push(b'\r'),
        Token::Lf => out.push(b'\n'),
        Token::Tab => out.push(b'\t'),
        Token::Bs => out.push(0x7F),
        Token::Esc => out.push(0x1B),
        Token::Space => out.push(b' '),
        Token::Ctrl(c) => {
            let ch = c.to_ascii_lowercase();
            // Ctrl-a (a..z) -> 0x01..0x1A; ctrl-@ -> 0x00 etc. handled below.
            if ch.is_ascii_lowercase() {
                out.push((ch as u8) - b'a' + 1);
            } else if ch.is_ascii_digit() {
                // Common mappings: ctrl-[ is 0x1b, ctrl-\ is 0x1c, etc.
                // For digits we just send the literal (no widely-agreed mapping).
                out.push(ch as u8);
            } else {
                out.push(ch as u8);
            }
        }
        Token::Alt(c) => {
            out.push(0x1B);
            out.push(c.to_ascii_lowercase() as u8);
        }
        Token::Fn(n) => out.extend_from_slice(&fn_key_bytes(*n)),
        Token::Arrow(a) => out.extend_from_slice(arrow_bytes(*a)),
        Token::Home => out.extend_from_slice(b"\x1b[H"),
        Token::End => out.extend_from_slice(b"\x1b[F"),
        Token::PageUp => out.extend_from_slice(b"\x1b[5~"),
        Token::PageDown => out.extend_from_slice(b"\x1b[6~"),
        Token::Del => out.extend_from_slice(b"\x1b[3~"),
        Token::Ins => out.extend_from_slice(b"\x1b[2~"),
    }
}

fn arrow_bytes(a: Arrow) -> &'static [u8] {
    match a {
        Arrow::Up => b"\x1b[A",
        Arrow::Down => b"\x1b[B",
        Arrow::Right => b"\x1b[C",
        Arrow::Left => b"\x1b[D",
    }
}

fn fn_key_bytes(n: u8) -> Vec<u8> {
    // xterm-compatible function-key encodings.
    match n {
        1 => b"\x1bOP".to_vec(),
        2 => b"\x1bOQ".to_vec(),
        3 => b"\x1bOR".to_vec(),
        4 => b"\x1bOS".to_vec(),
        5 => b"\x1b[15~".to_vec(),
        6 => b"\x1b[17~".to_vec(),
        7 => b"\x1b[18~".to_vec(),
        8 => b"\x1b[19~".to_vec(),
        9 => b"\x1b[20~".to_vec(),
        10 => b"\x1b[21~".to_vec(),
        11 => b"\x1b[23~".to_vec(),
        12 => b"\x1b[24~".to_vec(),
        _ => Vec::new(),
    }
}

/// Convenience: parse + serialize in one call.
///
/// # Errors
/// Bubbles up [`parse`] failures.
pub fn parse_to_bytes(input: &str) -> Result<Vec<u8>, KeyFormatError> {
    Ok(serialize(&parse(input)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Vec<Token> {
        parse(s).expect("parse ok")
    }

    #[test]
    fn literals_round_trip() {
        assert_eq!(p("hello"), vec![Token::Lit("hello".into())]);
        assert_eq!(serialize(&p("hello")), b"hello");
    }

    #[test]
    fn cr_and_esc_tokens() {
        assert_eq!(p("<cr>"), vec![Token::Cr]);
        assert_eq!(p("<esc>"), vec![Token::Esc]);
        assert_eq!(serialize(&p(":w<cr>")), b":w\r");
    }

    #[test]
    fn ctrl_combinations() {
        assert_eq!(p("<c-c>"), vec![Token::Ctrl('c')]);
        assert_eq!(serialize(&p("<c-c>")), &[0x03]);
        assert_eq!(serialize(&p("<c-z>")), &[0x1A]);
    }

    #[test]
    fn alt_combinations() {
        let bytes = serialize(&p("<a-f>"));
        assert_eq!(bytes, vec![0x1B, b'f']);
    }

    #[test]
    fn function_keys() {
        assert_eq!(p("<f1>"), vec![Token::Fn(1)]);
        assert_eq!(p("<f12>"), vec![Token::Fn(12)]);
        assert_eq!(serialize(&p("<f1>")), b"\x1bOP");
        assert_eq!(serialize(&p("<f5>")), b"\x1b[15~");
    }

    #[test]
    fn arrows_and_navigation() {
        assert_eq!(p("<up>"), vec![Token::Arrow(Arrow::Up)]);
        assert_eq!(serialize(&p("<up>")), b"\x1b[A");
        assert_eq!(serialize(&p("<home>")), b"\x1b[H");
        assert_eq!(serialize(&p("<pageup>")), b"\x1b[5~");
    }

    #[test]
    fn mixed_sequence() {
        let toks = p("i hello<esc>:w<cr>");
        assert_eq!(
            toks,
            vec![
                Token::Lit("i hello".into()),
                Token::Esc,
                Token::Lit(":w".into()),
                Token::Cr,
            ]
        );
        assert_eq!(serialize(&toks), b"i hello\x1b:w\r");
    }

    #[test]
    fn escaped_literal_angle_bracket() {
        let toks = p("a<\\<>b");
        assert_eq!(toks, vec![Token::Lit("a<b".into())]);
    }

    #[test]
    fn unknown_token_errors() {
        let err = parse("<bogus>").unwrap_err();
        assert!(err.reason.contains("unknown"), "{}", err.reason);
    }

    #[test]
    fn unterminated_angle_errors() {
        let err = parse("hello<no-close").unwrap_err();
        assert!(err.reason.contains("unterminated"), "{}", err.reason);
    }

    #[test]
    fn parse_to_bytes_shortcut() {
        let b = parse_to_bytes("a<cr>b").unwrap();
        assert_eq!(b, b"a\rb");
    }
}

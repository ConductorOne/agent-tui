//! Selector grammar + matcher for the outline tree.
//!
//! Spec: `docs/design/addressing-rfc.md` §2.2. A selector is a CSS-subset
//! expression that picks nodes out of an [`Outline`]. The parser
//! is hand-written recursive-descent because the grammar is small
//! and the error messages need to point at the offending byte.
//!
//! # Quick examples
//!
//! ```text
//! @vim.buffer[%1]                    one specific buffer (durable id)
//! [role=buffer][focused]             the focused buffer, anywhere
//! [role=status][name~=/written/]     status node whose name matches
//! @tmux pane[%2] > [role=buffer]     descendant + direct-child combo
//! ```
//!
//! # Tokenisation rule
//!
//! Dots inside a ref-path token bind tighter than whitespace.
//! `@tmux.pane[%2]` is one step; `@tmux pane[%2]` is two steps
//! joined by a descendant combinator. See §2.2 of the RFC for the
//! exact disambiguation table.

use crate::snapshot::{Outline, OutlineNode};
use regex::Regex;
use std::fmt;

// -------------------------------------------------------------------------
// Public API
// -------------------------------------------------------------------------

/// A compiled selector. Cheap to clone (the regexes are `Arc` underneath
/// in the `regex` crate).
#[derive(Debug, Clone)]
pub struct Selector {
    steps: Vec<Step>,
}

/// Selector parse error. Carries a byte offset so the CLI can render a
/// caret pointing at the bad character.
#[derive(Debug, Clone, thiserror::Error)]
#[error("selector parse error at byte {at}: {kind}")]
pub struct ParseError {
    /// 0-indexed byte offset into the input string.
    pub at: usize,
    /// Human-readable cause.
    pub kind: String,
}

/// Collect every ref in the outline (depth-first pre-order), capped at
/// `limit` for response-size safety. Useful for surfacing "here's what
/// IS in the tree" hints when a selector misses.
#[must_use]
pub fn all_refs(outline: &Outline, limit: usize) -> Vec<String> {
    fn walk(node: &OutlineNode, out: &mut Vec<String>, limit: usize) {
        if out.len() >= limit {
            return;
        }
        if !node.r#ref.is_empty() {
            out.push(node.r#ref.clone());
        }
        for child in &node.children {
            walk(child, out, limit);
        }
    }
    let mut out = Vec::with_capacity(limit.min(16));
    for root in &outline.nodes {
        walk(root, &mut out, limit);
    }
    out
}

/// Render a [`ParseError`] with a `^`-pointer at the offending byte.
/// Used by the CLI/daemon to give agents a visual cue:
///
/// ```text
/// selector parse error at byte 14: expected ']' to close predicate
///   [role=buffer
///                ^
/// ```
#[must_use]
pub fn format_parse_error(input: &str, err: &ParseError) -> String {
    // Clamp byte offset to input length.
    let at = err.at.min(input.len());
    // Build a leading-space indent of width `at` using the actual
    // characters in the input (handles wide glyphs only approximately
    // — selectors are ASCII in practice).
    let mut caret_line = String::with_capacity(at + 1);
    for _ in 0..at {
        caret_line.push(' ');
    }
    caret_line.push('^');
    format!(
        "selector parse error at byte {}: {}\n  {}\n  {}",
        err.at, err.kind, input, caret_line
    )
}

impl Selector {
    /// Parse a selector string into a compiled selector.
    pub fn parse(input: &str) -> Result<Self, ParseError> {
        let mut p = Parser::new(input);
        let steps = p.parse_selector()?;
        if steps.is_empty() {
            return Err(ParseError {
                at: 0,
                kind: "selector is empty".into(),
            });
        }
        Ok(Self { steps })
    }

    /// Returns every node in `outline` that the selector matches, in
    /// depth-first pre-order.
    #[must_use]
    pub fn matches<'a>(&self, outline: &'a Outline) -> Vec<&'a OutlineNode> {
        let mut out = Vec::new();
        for root in &outline.nodes {
            walk(root, &mut Vec::new(), &self.steps, &mut out);
        }
        out
    }

    /// First match in depth-first pre-order, or `None`.
    #[must_use]
    pub fn first<'a>(&self, outline: &'a Outline) -> Option<&'a OutlineNode> {
        self.matches(outline).into_iter().next()
    }

    /// Convenience: parse and match in one call. Returns parse errors
    /// rather than panicking so call sites can surface them.
    pub fn select<'a>(
        input: &str,
        outline: &'a Outline,
    ) -> Result<Vec<&'a OutlineNode>, ParseError> {
        Ok(Self::parse(input)?.matches(outline))
    }
}

impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, step) in self.steps.iter().enumerate() {
            if i > 0 {
                match step.combinator {
                    Combinator::Descendant => f.write_str(" ")?,
                    Combinator::DirectChild => f.write_str(" > ")?,
                }
            }
            step.pattern.fmt(f)?;
        }
        Ok(())
    }
}

// -------------------------------------------------------------------------
// Internal AST
// -------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Step {
    combinator: Combinator,
    pattern: Pattern,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Combinator {
    /// First step or whitespace-separated descendant.
    Descendant,
    /// `>` operator.
    DirectChild,
}

#[derive(Debug, Clone)]
struct Pattern {
    /// Optional ref-path prefix (e.g. `@vim.buffer[%1]`). When present,
    /// the matched node's `ref` must start with this path.
    ref_path: Option<RefPath>,
    /// Tail segment (e.g. `pane[%2]` after a combinator). Matches any
    /// node whose ref ends with this segment. Mutually exclusive with
    /// `ref_path`.
    tail_segment: Option<Segment>,
    /// Bracketed predicates, all of which must match.
    predicates: Vec<Predicate>,
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(p) = &self.ref_path {
            p.fmt(f)?;
        } else if let Some(seg) = &self.tail_segment {
            f.write_str(&seg.name)?;
            if let Some(k) = &seg.key {
                match k {
                    Key::Stable(v) => write!(f, "[%{v}]")?,
                    Key::Positional(n) => write!(f, "[{n}]")?,
                    Key::Named(s) => write!(f, "[{s}]")?,
                }
            }
        } else if self.predicates.is_empty() {
            // `*` is the only legal pattern with no ref-path/segment/predicates.
            f.write_str("*")?;
        }
        for pred in &self.predicates {
            pred.fmt(f)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct RefPath {
    /// Adapter root, e.g. `tmux`, `vim`. Always non-empty.
    head: String,
    /// Dotted segments after the head.
    segments: Vec<Segment>,
}

impl fmt::Display for RefPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{}", self.head)?;
        for s in &self.segments {
            write!(f, ".{}", s.name)?;
            if let Some(k) = &s.key {
                match k {
                    Key::Stable(v) => write!(f, "[%{v}]")?,
                    Key::Positional(n) => write!(f, "[{n}]")?,
                    Key::Named(s) => write!(f, "[{s}]")?,
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct Segment {
    name: String,
    key: Option<Key>,
}

#[derive(Debug, Clone)]
enum Key {
    /// `[%2]` — adapter-stable identifier.
    Stable(String),
    /// `[2]` — positional (not stable across snapshots).
    Positional(i64),
    /// `[main]` — adapter-chosen symbolic name.
    Named(String),
}

#[derive(Debug, Clone)]
enum Predicate {
    /// `[focused]` or `[focused=true]`.
    Focused(bool),
    /// `[durable]` — only nodes whose ref binding is `Durable`.
    Durable,
    /// `[attr=value]` — exact match.
    Eq { attr: Attr, value: String },
    /// `[attr~=/regex/]` — regex match.
    Regex { attr: Attr, regex: Regex },
    /// `[attr^=prefix]`.
    Prefix { attr: Attr, value: String },
    /// `[attr$=suffix]`.
    Suffix { attr: Attr, value: String },
}

impl fmt::Display for Predicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Focused(true) => f.write_str("[focused]"),
            Self::Focused(false) => f.write_str("[focused=false]"),
            Self::Durable => f.write_str("[durable]"),
            Self::Eq { attr, value } => write!(f, "[{attr}={value}]"),
            Self::Regex { attr, regex } => write!(f, "[{attr}~=/{}/]", regex.as_str()),
            Self::Prefix { attr, value } => write!(f, "[{attr}^={value}]"),
            Self::Suffix { attr, value } => write!(f, "[{attr}$={value}]"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attr {
    Role,
    Name,
    Value,
}

impl fmt::Display for Attr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Role => "role",
            Self::Name => "name",
            Self::Value => "value",
        })
    }
}

// -------------------------------------------------------------------------
// Parser
// -------------------------------------------------------------------------

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

struct Parser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
        }
    }

    fn err(&self, kind: impl Into<String>) -> ParseError {
        ParseError {
            at: self.pos,
            kind: kind.into(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    fn eat_ws(&mut self) -> bool {
        let start = self.pos;
        while matches!(self.peek(), Some(b) if b == b' ' || b == b'\t') {
            self.pos += 1;
        }
        self.pos > start
    }

    fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn parse_selector(&mut self) -> Result<Vec<Step>, ParseError> {
        let mut steps = Vec::new();
        self.eat_ws();
        if self.at_end() {
            return Err(self.err("selector is empty"));
        }
        // First step always has Descendant combinator semantics
        // (i.e. "matches anywhere in the tree if it isn't a ref_path,
        // or anchored to the ref-path's adapter root").
        let first = self.parse_pattern()?;
        steps.push(Step {
            combinator: Combinator::Descendant,
            pattern: first,
        });
        loop {
            let saw_ws = self.eat_ws();
            if self.at_end() {
                break;
            }
            let combinator = if self.peek() == Some(b'>') {
                self.bump();
                self.eat_ws();
                Combinator::DirectChild
            } else if saw_ws {
                Combinator::Descendant
            } else {
                // Unexpected character at top level.
                return Err(self.err(format!(
                    "unexpected character {:?} (expected combinator or end of input)",
                    self.peek().map_or(' ', |b| b as char)
                )));
            };
            let pattern = self.parse_pattern()?;
            steps.push(Step {
                combinator,
                pattern,
            });
        }
        Ok(steps)
    }

    fn parse_pattern(&mut self) -> Result<Pattern, ParseError> {
        // A pattern is one of:
        //   '@' ref_path predicate*       (ref-anchored)
        //   '*' predicate*                (wildcard, explicit)
        //   bare_segment predicate*       (tail segment, e.g. `pane[%2]`)
        //   predicate+                    (predicate-only)
        let mut ref_path = None;
        let mut tail_segment = None;
        if self.peek() == Some(b'@') {
            self.bump();
            ref_path = Some(self.parse_ref_path()?);
        } else if self.peek() == Some(b'*') {
            self.bump();
        } else if let Some(b) = self.peek() {
            if is_ident_start(b) {
                tail_segment = Some(self.parse_segment()?);
            }
        }
        let mut predicates = Vec::new();
        while self.peek() == Some(b'[') {
            self.bump();
            predicates.push(self.parse_predicate()?);
            if self.peek() != Some(b']') {
                return Err(self.err("expected ']' to close predicate"));
            }
            self.bump();
        }
        if ref_path.is_none() && tail_segment.is_none() && predicates.is_empty() {
            return Err(self.err("expected ref-path (@…), '*', tail segment, or predicate ([…])"));
        }
        Ok(Pattern {
            ref_path,
            tail_segment,
            predicates,
        })
    }

    fn parse_segment(&mut self) -> Result<Segment, ParseError> {
        let name = self.parse_ident();
        if name.is_empty() {
            return Err(self.err("segment name cannot be empty"));
        }
        let key = if self.peek() == Some(b'[') {
            // Disambiguate: `name[%2]` is a keyed segment; `name[role=x]`
            // is a segment + predicate. Peek past `[` for `%` or digit.
            let save = self.pos;
            self.bump();
            let looks_like_key = matches!(self.peek(), Some(b'%'))
                || matches!(self.peek(), Some(b) if b.is_ascii_digit());
            if looks_like_key {
                let k = self.parse_key()?;
                if self.peek() != Some(b']') {
                    return Err(self.err("expected ']' to close segment key"));
                }
                self.bump();
                Some(k)
            } else {
                // Not a key — restore for predicate parsing.
                self.pos = save;
                None
            }
        } else {
            None
        };
        Ok(Segment { name, key })
    }

    fn parse_ref_path(&mut self) -> Result<RefPath, ParseError> {
        let head = self.parse_ident();
        if head.is_empty() {
            return Err(self.err("ref-path needs a head identifier after '@'"));
        }
        let mut segments = Vec::new();
        while self.peek() == Some(b'.') {
            self.bump();
            segments.push(self.parse_segment()?);
        }
        Ok(RefPath { head, segments })
    }

    fn parse_key(&mut self) -> Result<Key, ParseError> {
        // `%N`, `N` (integer), or `name` (ident).
        if self.peek() == Some(b'%') {
            self.bump();
            let s = self.parse_ident_or_int();
            if s.is_empty() {
                return Err(self.err("stable-id key cannot be empty after '%'"));
            }
            return Ok(Key::Stable(s));
        }
        let s = self.parse_ident_or_int();
        if s.is_empty() {
            return Err(self.err("key cannot be empty"));
        }
        // Bare digits → Positional. Anything else → Named.
        if let Ok(n) = s.parse::<i64>() {
            Ok(Key::Positional(n))
        } else {
            Ok(Key::Named(s))
        }
    }

    fn parse_predicate(&mut self) -> Result<Predicate, ParseError> {
        // Already consumed '['. Predicate body is one of:
        //   focused | focused=true|false
        //   durable
        //   role=value | role~=/regex/ | role^=prefix | role$=suffix
        //   name=… | name~=… | name^=… | name$=…
        //   value=… | value~=… | value^=… | value$=…
        let ident = self.parse_ident();
        if ident.is_empty() {
            return Err(self.err("expected attribute name in predicate"));
        }
        match ident.as_str() {
            "focused" => {
                if self.peek() == Some(b'=') {
                    self.bump();
                    let v = self.parse_value_word();
                    let b = match v.as_str() {
                        "true" | "1" => true,
                        "false" | "0" => false,
                        other => {
                            return Err(
                                self.err(format!("focused expects true|false|1|0, got {other:?}"))
                            );
                        }
                    };
                    return Ok(Predicate::Focused(b));
                }
                Ok(Predicate::Focused(true))
            }
            "durable" => Ok(Predicate::Durable),
            "role" | "name" | "value" => {
                let attr = match ident.as_str() {
                    "role" => Attr::Role,
                    "name" => Attr::Name,
                    _ => Attr::Value,
                };
                self.parse_attr_op(attr)
            }
            other => Err(self.err(format!(
                "unknown attribute {other:?} (expected role|name|value|focused|durable)"
            ))),
        }
    }

    fn parse_attr_op(&mut self, attr: Attr) -> Result<Predicate, ParseError> {
        // op is one of: `=`, `~=`, `^=`, `$=`
        let b = self.peek().ok_or_else(|| self.err("expected operator"))?;
        let op: &'static str = match b {
            b'=' => {
                self.bump();
                "="
            }
            b'~' => {
                self.bump();
                if self.bump() != Some(b'=') {
                    return Err(self.err("expected '~=' operator"));
                }
                "~="
            }
            b'^' => {
                self.bump();
                if self.bump() != Some(b'=') {
                    return Err(self.err("expected '^=' operator"));
                }
                "^="
            }
            b'$' => {
                self.bump();
                if self.bump() != Some(b'=') {
                    return Err(self.err("expected '$=' operator"));
                }
                "$="
            }
            other => {
                return Err(self.err(format!(
                    "unexpected operator byte {:?} for attribute (expected = ~= ^= $=)",
                    other as char
                )));
            }
        };
        match op {
            "=" => Ok(Predicate::Eq {
                attr,
                value: self.parse_value()?,
            }),
            "~=" => {
                let regex = self.parse_regex()?;
                Ok(Predicate::Regex { attr, regex })
            }
            "^=" => Ok(Predicate::Prefix {
                attr,
                value: self.parse_value()?,
            }),
            "$=" => Ok(Predicate::Suffix {
                attr,
                value: self.parse_value()?,
            }),
            _ => unreachable!(),
        }
    }

    fn parse_value(&mut self) -> Result<String, ParseError> {
        if self.peek() == Some(b'"') {
            self.bump();
            let start = self.pos;
            let mut out = String::new();
            loop {
                match self.bump() {
                    None => return Err(self.err("unterminated quoted value")),
                    Some(b'"') => break,
                    Some(b'\\') => match self.bump() {
                        Some(b'"') => out.push('"'),
                        Some(b'\\') => out.push('\\'),
                        Some(b'n') => out.push('\n'),
                        Some(b't') => out.push('\t'),
                        Some(b) => {
                            return Err(ParseError {
                                at: self.pos - 1,
                                kind: format!("unknown escape \\{:?}", b as char),
                            });
                        }
                        None => return Err(self.err("dangling escape in quoted value")),
                    },
                    Some(b) => out.push(b as char),
                }
            }
            let _ = start;
            Ok(out)
        } else {
            Ok(self.parse_value_word())
        }
    }

    fn parse_value_word(&mut self) -> String {
        // bareword: everything up to `]` or end-of-input that's printable.
        // Only `~=` reserves `/` (regex bodies); for `=`, `^=`, `$=`
        // the slash is just a value character.
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b == b']' {
                break;
            }
            if b < 0x20 || b == b'[' {
                break;
            }
            self.bump();
        }
        self.src[start..self.pos].to_string()
    }

    fn parse_regex(&mut self) -> Result<Regex, ParseError> {
        if self.bump() != Some(b'/') {
            return Err(ParseError {
                at: self.pos - 1,
                kind: "regex body must start with '/'".into(),
            });
        }
        let start = self.pos;
        // Find the closing `/`. Allow `\/` to escape.
        let mut body = String::new();
        loop {
            match self.bump() {
                None => return Err(self.err("unterminated regex body")),
                Some(b'/') => break,
                Some(b'\\') => match self.bump() {
                    Some(b'/') => body.push('/'),
                    Some(b) => {
                        body.push('\\');
                        body.push(b as char);
                    }
                    None => return Err(self.err("dangling escape in regex body")),
                },
                Some(b) => body.push(b as char),
            }
        }
        Regex::new(&body).map_err(|e| ParseError {
            at: start,
            kind: format!("invalid regex: {e}"),
        })
    }

    fn parse_ident(&mut self) -> String {
        let start = self.pos;
        while let Some(b) = self.peek() {
            let ok = b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
            if !ok {
                break;
            }
            self.bump();
        }
        self.src[start..self.pos].to_string()
    }

    fn parse_ident_or_int(&mut self) -> String {
        // Like parse_ident but also accepts a leading `-` for negative
        // positional keys (we don't really expect those, but the
        // grammar permits ints).
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.bump();
        }
        while let Some(b) = self.peek() {
            let ok = b.is_ascii_alphanumeric() || b == b'_';
            if !ok {
                break;
            }
            self.bump();
        }
        self.src[start..self.pos].to_string()
    }
}

// -------------------------------------------------------------------------
// Matcher
// -------------------------------------------------------------------------

fn walk<'a>(
    node: &'a OutlineNode,
    ancestors: &mut Vec<&'a OutlineNode>,
    steps: &[Step],
    out: &mut Vec<&'a OutlineNode>,
) {
    if matches_path(node, ancestors, steps) {
        out.push(node);
    }
    ancestors.push(node);
    for child in &node.children {
        walk(child, ancestors, steps, out);
    }
    ancestors.pop();
}

/// True iff the path `ancestors + [node]` matches `steps`.
///
/// The match is anchored at the **end** (the candidate node must
/// satisfy the last step) and walked backward through ancestors. The
/// first step always matches as a Descendant (i.e. it can be any
/// ancestor or the node itself).
fn matches_path(node: &OutlineNode, ancestors: &[&OutlineNode], steps: &[Step]) -> bool {
    let mut path: Vec<&OutlineNode> = ancestors.to_vec();
    path.push(node);
    let n = path.len();
    let mut s = steps.len();
    let mut i = n; // exclusive upper bound

    // Last step must match the last node exactly.
    if s == 0 {
        return true;
    }
    s -= 1;
    if !matches_step_node(&steps[s], path[i - 1]) {
        return false;
    }
    i -= 1;

    while s > 0 {
        s -= 1;
        let step = &steps[s];
        match steps[s + 1].combinator {
            Combinator::DirectChild => {
                // The previous step's combinator was '>'; this step
                // must match the *direct parent* of where we matched.
                if i == 0 {
                    return false;
                }
                if !matches_step_node(step, path[i - 1]) {
                    return false;
                }
                i -= 1;
            }
            Combinator::Descendant => {
                // Walk backward looking for the first ancestor that
                // matches this step.
                let mut found = false;
                while i > 0 {
                    i -= 1;
                    if matches_step_node(step, path[i]) {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return false;
                }
            }
        }
    }
    true
}

fn matches_step_node(step: &Step, node: &OutlineNode) -> bool {
    if let Some(rp) = &step.pattern.ref_path {
        if !ref_path_matches(rp, &node.r#ref) {
            return false;
        }
    }
    if let Some(seg) = &step.pattern.tail_segment {
        if !tail_segment_matches(seg, &node.r#ref) {
            return false;
        }
    }
    for pred in &step.pattern.predicates {
        if !predicate_matches(pred, node) {
            return false;
        }
    }
    true
}

fn tail_segment_matches(seg: &Segment, node_ref: &str) -> bool {
    // Split the ref into its last segment (whatever follows the last `.`,
    // or whatever follows `@` if no dots). Compare against `seg`.
    let last = match node_ref.rsplit_once('.') {
        Some((_, last)) => last,
        None => node_ref.strip_prefix('@').unwrap_or(node_ref),
    };
    // `last` is now `name` or `name[key]`.
    let (last_name, last_key) = match last.split_once('[') {
        Some((n, rest)) => {
            let key = rest.strip_suffix(']').unwrap_or(rest);
            (n, Some(key))
        }
        None => (last, None),
    };
    if last_name != seg.name {
        return false;
    }
    match (&seg.key, last_key) {
        (None, _) => true, // segment without key matches any key (or no key)
        (Some(Key::Stable(v)), Some(k)) => k == format!("%{v}").as_str(),
        (Some(Key::Positional(n)), Some(k)) => k == n.to_string().as_str(),
        (Some(Key::Named(s)), Some(k)) => k == s.as_str(),
        (Some(_), None) => false, // segment demanded a key, ref had none
    }
}

fn ref_path_matches(rp: &RefPath, node_ref: &str) -> bool {
    // Construct the textual representation and check that `node_ref`
    // starts with it. Refs are dotted paths, so we anchor with either
    // the exact match or a path that continues with `.`.
    let candidate = format!("{rp}");
    if node_ref == candidate {
        return true;
    }
    if node_ref.starts_with(&candidate) {
        // The next byte must be `.` (continuation) — otherwise a
        // ref like `@vimexpr` would match `@vim`.
        let rest = &node_ref[candidate.len()..];
        return rest.starts_with('.');
    }
    false
}

fn predicate_matches(pred: &Predicate, node: &OutlineNode) -> bool {
    match pred {
        Predicate::Focused(want) => node.focused == *want,
        Predicate::Durable => node.durable,
        Predicate::Eq { attr, value } => attr_eq(*attr, node, value),
        Predicate::Regex { attr, regex } => {
            let s = attr_value(*attr, node);
            regex.is_match(&s)
        }
        Predicate::Prefix { attr, value } => attr_value(*attr, node).starts_with(value.as_str()),
        Predicate::Suffix { attr, value } => attr_value(*attr, node).ends_with(value.as_str()),
    }
}

fn attr_value(attr: Attr, node: &OutlineNode) -> String {
    match attr {
        Attr::Role => node.role.clone(),
        Attr::Name => node.name.clone(),
        Attr::Value => node.value.clone().unwrap_or_default(),
    }
}

fn attr_eq(attr: Attr, node: &OutlineNode, want: &str) -> bool {
    attr_value(attr, node) == want
}

// -------------------------------------------------------------------------
// Tests
// -------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::OutlineNode;

    fn node(r#ref: &str, role: &str, name: &str) -> OutlineNode {
        OutlineNode {
            r#ref: r#ref.into(),
            role: role.into(),
            name: name.into(),
            value: None,
            focused: false,
            anchor: None,
            extent: None,
            state: None,
            durable: false,
            children: Vec::new(),
        }
    }

    fn durable_node(r#ref: &str, role: &str, name: &str) -> OutlineNode {
        let mut n = node(r#ref, role, name);
        n.durable = true;
        n
    }

    fn with_children(mut n: OutlineNode, kids: Vec<OutlineNode>) -> OutlineNode {
        n.children = kids;
        n
    }

    fn outline(roots: Vec<OutlineNode>) -> Outline {
        Outline {
            adapter: "test".into(),
            nodes: roots,
        }
    }

    // ----- parser -----

    #[test]
    fn parses_simple_ref_path() {
        let s = Selector::parse("@vim.buffer[%1]").unwrap();
        assert_eq!(format!("{s}"), "@vim.buffer[%1]");
    }

    #[test]
    fn parses_predicate_only() {
        let s = Selector::parse("[role=buffer][focused]").unwrap();
        assert_eq!(format!("{s}"), "[role=buffer][focused]");
    }

    #[test]
    fn parses_descendant_and_direct_child_combinators() {
        let s = Selector::parse("@tmux pane[%2] > [role=buffer]").unwrap();
        // Re-emit canonicalises spacing.
        assert_eq!(format!("{s}"), "@tmux pane[%2] > [role=buffer]");
    }

    #[test]
    fn dotted_ref_is_one_token_not_descendant() {
        let s = Selector::parse("@tmux.pane[%2]").unwrap();
        assert_eq!(s.steps.len(), 1);
        let path = s.steps[0].pattern.ref_path.as_ref().unwrap();
        assert_eq!(path.head, "tmux");
        assert_eq!(path.segments.len(), 1);
        assert_eq!(path.segments[0].name, "pane");
    }

    #[test]
    fn whitespace_ref_is_two_steps() {
        let s = Selector::parse("@tmux pane[%2]").unwrap();
        assert_eq!(s.steps.len(), 2);
    }

    #[test]
    fn parses_regex_predicate_with_slashes() {
        let s = Selector::parse("[name~=/^\\d+ written$/]").unwrap();
        assert!(format!("{s}").contains("written"));
    }

    #[test]
    fn parses_quoted_value() {
        let s = Selector::parse("[name=\"hello world\"]").unwrap();
        // Stored without quotes; display path re-emits as `name=hello world`
        // which is intentional (we don't round-trip quoting today).
        assert!(format!("{s}").contains("hello world"));
    }

    #[test]
    fn rejects_malformed_predicate() {
        let e = Selector::parse("[role").unwrap_err();
        assert!(e.kind.contains("operator"));
    }

    #[test]
    fn rejects_unknown_attribute() {
        let e = Selector::parse("[bogus=x]").unwrap_err();
        assert!(e.kind.contains("unknown attribute"));
    }

    #[test]
    fn rejects_empty_selector() {
        let e = Selector::parse("").unwrap_err();
        assert!(e.kind.contains("empty"));
    }

    #[test]
    fn focused_with_boolean() {
        let s = Selector::parse("[focused=false]").unwrap();
        assert_eq!(format!("{s}"), "[focused=false]");
    }

    // ----- matcher -----

    #[test]
    fn matches_by_role() {
        let tree = outline(vec![
            node("@e1", "mode", "normal"),
            node("@e2", "buffer", "hello"),
        ]);
        let s = Selector::parse("[role=buffer]").unwrap();
        let m = s.matches(&tree);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].r#ref, "@e2");
    }

    #[test]
    fn matches_focused() {
        let mut n = node("@e1", "buffer", "");
        n.focused = true;
        let tree = outline(vec![n, node("@e2", "buffer", "")]);
        let s = Selector::parse("[role=buffer][focused]").unwrap();
        let m = s.matches(&tree);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].r#ref, "@e1");
    }

    #[test]
    fn matches_durable_from_inline_flag() {
        let tree = outline(vec![
            durable_node("@vim.buffer[%1]", "buffer", "a"),
            node("@vim.buffer[2]", "buffer", "b"),
        ]);
        let s = Selector::parse("[role=buffer][durable]").unwrap();
        let m = s.matches(&tree);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].r#ref, "@vim.buffer[%1]");
    }

    #[test]
    fn matches_by_ref_path_prefix() {
        let tree = outline(vec![with_children(
            node("@vim", "root", ""),
            vec![
                node("@vim.buffer[%1]", "buffer", "a"),
                node("@vim.statusline", "statusline", "x"),
            ],
        )]);
        let s = Selector::parse("@vim.buffer[%1]").unwrap();
        let m = s.matches(&tree);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "a");
    }

    #[test]
    fn ref_path_prefix_does_not_match_unrelated_head() {
        // `@vim` must not match `@vimexpr` etc.
        let tree = outline(vec![node("@vimexpr.x", "x", "")]);
        let s = Selector::parse("@vim").unwrap();
        assert!(s.matches(&tree).is_empty());
    }

    #[test]
    fn descendant_walks_through_children() {
        let tree = outline(vec![with_children(
            node("@tmux", "root", ""),
            vec![with_children(
                node("@tmux.pane[%0]", "pane", ""),
                vec![node("@tmux.pane[%0].buffer", "buffer", "hi")],
            )],
        )]);
        let s = Selector::parse("@tmux [role=buffer]").unwrap();
        let m = s.matches(&tree);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "hi");
    }

    #[test]
    fn direct_child_does_not_skip_levels() {
        // `[role=root] > [role=buffer]` must NOT match a buffer that
        // is a grandchild of root.
        let tree = outline(vec![with_children(
            node("@tmux", "root", ""),
            vec![with_children(
                node("@tmux.pane[%0]", "pane", ""),
                vec![node("@tmux.pane[%0].buffer", "buffer", "hi")],
            )],
        )]);
        let s = Selector::parse("[role=root] > [role=buffer]").unwrap();
        assert!(s.matches(&tree).is_empty());
        // Two-hop direct chain does match.
        let s2 = Selector::parse("[role=root] > [role=pane] > [role=buffer]").unwrap();
        assert_eq!(s2.matches(&tree).len(), 1);
    }

    #[test]
    fn regex_predicate_evaluates_against_name() {
        let tree = outline(vec![
            node("@e1", "status", "5 lines, 42 bytes written"),
            node("@e2", "status", "search hit"),
        ]);
        let s = Selector::parse("[role=status][name~=/written/]").unwrap();
        let m = s.matches(&tree);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].r#ref, "@e1");
    }

    #[test]
    fn prefix_and_suffix_predicates() {
        let tree = outline(vec![
            node("@e1", "file", "/work/foo.txt"),
            node("@e2", "file", "/tmp/bar.txt"),
        ]);
        assert_eq!(
            Selector::parse("[name^=/work]")
                .unwrap()
                .matches(&tree)
                .len(),
            1
        );
        assert_eq!(
            Selector::parse("[name$=bar.txt]")
                .unwrap()
                .matches(&tree)
                .len(),
            1
        );
    }

    #[test]
    fn first_returns_depth_first_pre_order() {
        let tree = outline(vec![with_children(
            node("@root", "root", ""),
            vec![node("@root.a", "x", "a"), node("@root.b", "x", "b")],
        )]);
        let s = Selector::parse("[role=x]").unwrap();
        let first = s.first(&tree).unwrap();
        assert_eq!(first.name, "a");
    }

    #[test]
    fn empty_outline_returns_empty_matches() {
        let s = Selector::parse("[role=buffer]").unwrap();
        let tree = outline(vec![]);
        assert!(s.matches(&tree).is_empty());
        assert!(s.first(&tree).is_none());
    }

    #[test]
    fn all_refs_walks_tree_depth_first() {
        let tree = outline(vec![with_children(
            node("@vim", "root", ""),
            vec![
                node("@vim.mode", "mode", ""),
                node("@vim.buffer", "buffer", ""),
            ],
        )]);
        let refs = super::all_refs(&tree, 100);
        assert_eq!(refs, vec!["@vim", "@vim.mode", "@vim.buffer"]);
    }

    #[test]
    fn all_refs_respects_limit() {
        let kids: Vec<_> = (0..50)
            .map(|i| node(&format!("@root.k{i}"), "x", ""))
            .collect();
        let tree = outline(vec![with_children(node("@root", "root", ""), kids)]);
        let refs = super::all_refs(&tree, 5);
        assert_eq!(refs.len(), 5);
        assert_eq!(refs[0], "@root");
    }

    #[test]
    fn format_parse_error_draws_caret() {
        let err = Selector::parse("[role buffer").unwrap_err();
        let out = super::format_parse_error("[role buffer", &err);
        assert!(out.contains("[role buffer"));
        // The caret line should have ^ at column `err.at`.
        let caret_line = out.lines().last().unwrap();
        let prefix = caret_line.trim_start_matches(' ');
        assert!(prefix.starts_with('^'));
        // The caret indents by 2 (response prefix) + err.at spaces.
        let lead_spaces = caret_line.len() - prefix.len();
        assert_eq!(lead_spaces, 2 + err.at);
    }

    #[test]
    fn tail_segment_without_key_matches_any_key() {
        let tree = outline(vec![with_children(
            node("@tmux", "root", ""),
            vec![
                node("@tmux.pane[%2]", "pane", "p2"),
                node("@tmux.pane[%3]", "pane", "p3"),
            ],
        )]);
        // `pane` after a descendant combinator should match every pane.
        let s = Selector::parse("@tmux pane").unwrap();
        let m = s.matches(&tree);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn tail_segment_with_stable_key_is_specific() {
        let tree = outline(vec![with_children(
            node("@tmux", "root", ""),
            vec![node("@tmux.pane[%2]", "pane", "p2")],
        )]);
        let s = Selector::parse("@tmux pane[%2]").unwrap();
        assert_eq!(s.matches(&tree).len(), 1);
        let s2 = Selector::parse("@tmux pane[%3]").unwrap();
        assert!(s2.matches(&tree).is_empty());
    }

    #[test]
    fn select_convenience_function() {
        let tree = outline(vec![node("@e1", "buffer", "")]);
        let m = Selector::select("[role=buffer]", &tree).unwrap();
        assert_eq!(m.len(), 1);
    }
}

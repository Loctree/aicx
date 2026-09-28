//! Turn-level effective scope evidence (W2-R1 follow-up).
//!
//! A Codex session's declared cwd (`session_meta` / `turn_context`) is only the
//! turn baseline: agents run executable tool calls with an explicit `workdir`
//! that can point into a different repository without any `turn_context` drift.
//! This module extracts that stronger runtime evidence and reduces it to a
//! per-turn-window verdict at REPO-ROOT identity granularity — two subdirs of
//! one checkout are one scope, two real checkouts are a conflict.
//!
//! Repo identity — never a lexical path prefix — is the single membership
//! test in this module and in every downstream project filter. A nested
//! checkout or submodule lives lexically below its parent and is still a
//! different repository; deciding membership by string prefix is exactly the
//! cross-repo leak this module exists to close.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::segmentation::discover_git_root;

/// One piece of explicit workdir evidence observed inside a turn window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkdirEvidence {
    /// A readable explicit `workdir` from an executable tool call.
    Explicit(String),
    /// A tool-call envelope was observed but its payload could not be read
    /// (a record over the bounded reader's per-record cap). Evidence exists,
    /// its value does not. Fail-closed: the window can never be attributed
    /// from evidence it could not read.
    Opaque,
}

impl WorkdirEvidence {
    /// The explicit path, when this evidence carries one.
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Explicit(path) => Some(path.as_str()),
            Self::Opaque => None,
        }
    }
}

/// Repo-level identity of one explicit tool-call working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkdirIdentity {
    /// Path resolves to a git checkout; the identity is the canonical repo
    /// root, so two spellings of one checkout (symlink, `/var` vs
    /// `/private/var`) are one identity rather than a phantom conflict.
    Resolved(PathBuf),
    /// Path does not exist locally or has no `.git` ancestor. Conservative:
    /// never guess a repo — each distinct unresolved path is its own identity.
    Unresolved(String),
}

impl WorkdirIdentity {
    /// Path representing this identity for frame stamping: the repo root when
    /// resolved, the original workdir otherwise.
    pub fn scope_path(&self) -> String {
        match self {
            Self::Resolved(root) => root.to_string_lossy().into_owned(),
            Self::Unresolved(path) => path.clone(),
        }
    }

    fn is_resolved(&self) -> bool {
        matches!(self, Self::Resolved(_))
    }
}

/// Effective scope of one turn window, from its explicit workdir evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowScope {
    /// No explicit workdir evidence, or all of it names the baseline checkout
    /// — keep the `turn_context` baseline.
    Baseline,
    /// Every explicit workdir in the window normalizes to one resolved repo
    /// identity — positive attribution at the repo root.
    Consistent,
    /// Two or more distinct resolved repo identities — proven divergence.
    Conflict,
    /// Workdir evidence exists but does not resolve to a git checkout
    /// (historical/deleted/foreign path), or could not be read at all.
    /// "I don't know" — never a positive attribution, and not proof of
    /// divergence either.
    Unattributed,
}

/// Every explicit `workdir` in an executable tool call payload, in order.
///
/// Two real shapes exist in Codex rollouts: JSON `function_call.arguments`
/// (`{"cmd": "...", "workdir": "/abs/path"}`) and a JavaScript literal inside
/// `custom_tool_call.input` (`tools.exec_command({cmd:"...",workdir:'/abs'})`).
///
/// ALL occurrences are returned, not the first: one `input` can orchestrate
/// several `tools.exec_command` calls, and reading only the first hides a
/// later hop into another repository behind a baseline-looking opener. A
/// `workdir:` object written inside a string or a comment — a shell command
/// that echoes one, an example, a commented-out call — is opaque, never a
/// path: the call beside it may name no directory at all, and that text alone
/// would then have decided the window.
///
/// A `workdir` whose value is written but cannot be read as one path — a
/// variable or an expression, a template literal that interpolates, a literal
/// that never closes — is [`WorkdirEvidence::Opaque`]: the call names a
/// directory we cannot see. So is an `exec_command` whose argument is not all
/// written at the call ([`exec_arguments_unseen`]).
///
/// Arguments that parse as a JSON object are read structurally: only their
/// top-level `workdir` is the call's directory. Arguments that parse as any
/// other JSON value name none. The JavaScript scan is for arguments that do
/// not parse.
pub fn tool_call_workdirs(payload: &Value) -> Vec<WorkdirEvidence> {
    let mut found: Vec<WorkdirEvidence> = Vec::new();
    let mut push = |evidence: WorkdirEvidence| {
        if !found.contains(&evidence) {
            found.push(evidence);
        }
    };
    if let Some(arguments) = payload.get("arguments") {
        let parsed = match arguments {
            Value::Object(_) => Some(arguments.clone()),
            Value::String(raw) => serde_json::from_str::<Value>(raw).ok(),
            _ => None,
        };
        match parsed.as_ref() {
            // Structured arguments: the call's directory is the top-level
            // `workdir` and nothing else. A nested `options.workdir`, or one
            // written inside the command string, is data the call carried, and
            // reading it re-scoped a baseline call to a checkout it never ran in.
            Some(Value::Object(body)) => match body.get("workdir") {
                Some(Value::String(workdir)) => {
                    let workdir = workdir.trim();
                    if !workdir.is_empty() {
                        push(WorkdirEvidence::Explicit(workdir.to_string()));
                    }
                }
                None | Some(Value::Null | Value::Number(_)) => {}
                Some(_) => push(WorkdirEvidence::Opaque),
            },
            // Arguments that parse to anything but an object — an array, a
            // string, a number — have no top-level `workdir`: the call named no
            // directory. Scanned as JavaScript, `[{"workdir": "/foreign"}]`
            // read as one and moved the window there.
            Some(_) => {}
            // A JS literal can also arrive as the raw `arguments` string; scan
            // it rather than declaring no evidence because JSON parsing failed.
            None => {
                if let Value::String(raw) = arguments {
                    workdirs_in_literal(raw).into_iter().for_each(&mut push);
                }
            }
        }
    }
    if let Some(input) = payload.get("input").and_then(Value::as_str) {
        workdirs_in_literal(input).into_iter().for_each(&mut push);
    }
    found
}

/// Every `workdir` key inside an object literal, quote style agnostic:
/// `workdir:"/p"`, `"workdir": "/p"`, `'workdir': '/p'`, `` workdir: `/p` ``.
///
/// Codex writes `custom_tool_call.input` as model-authored JavaScript, so the
/// quote style is whatever the model emitted. Accepting only JSON-style double
/// quotes silently dropped real evidence and left the window on its baseline
/// project — the leak this module exists to close. A template literal is a
/// string literal too: a static one is read like any other, and one that
/// interpolates (`${root}/pkg`) names a directory only runtime knew, so it is
/// opaque rather than a fabricated path.
///
/// A value runs to the quote that OPENED it, never to the first quote of any
/// kind: `"/Users/O'Brien/repo"` is one path, not `/Users/O`. A backslash
/// escapes the character after it for that search, and only an escaped
/// backslash or an escaped delimiter is unescaped in the value; every other
/// backslash stays, because a Windows workdir is `C:\repo\crate` and reading
/// `\r` as an escape would corrupt it. An escape that can spell a dot, a
/// separator or a drive colon is the exception: JavaScript runs
/// `"/repo/\x2e\x2e/foreign"` in `/foreign`, and read as written it sits
/// beneath `/repo`, so such a literal is opaque ([`escape_masks_path`]). A
/// literal that never closes is opaque too: whatever it names was cut off. So
/// is a literal that is only the first operand of an expression —
/// `"/repos/vista" + "-private"` — because the value continues past its
/// closing quote ([`value_ends_at`]).
///
/// A value that is not a literal at all — `workdir: targetDir`,
/// `workdir: path.join(root, "pkg")`, `workdir: [root, repo].join('/')`,
/// `workdir: !local ? foreign : base`, the shorthand `{cmd, workdir}` — still
/// names the directory the call ran in; only the runtime knew which. It is
/// opaque, never "no evidence": skipping it left the window on its baseline
/// while the call worked in another checkout. `null`, `undefined` and a
/// number name no directory, but only as the whole value
/// ([`names_no_directory`]).
///
/// Any `workdir`, literal or not, counts only where the key is a property of
/// an object ([`opens_property`]). Prose like `// workdir: the repo` names
/// nothing, and reading it would unplace the window. A quoted path is no
/// exception: `const example = 'workdir:"/repo/foreign"'` quotes a path no
/// call ran in, and reading it as evidence would re-scope the window to that
/// checkout. Comments are skipped wherever JavaScript allows them — before the
/// key, around the separator, before the value.
///
/// The key must be the whole property name, and a quoted one must be quoted
/// on both sides: `networkdir:`, `fallback_workdir:` and `"fallback-workdir":`
/// are other properties, and reading them would unplace a window whose call
/// never left the baseline.
///
/// A computed key is the same property written in brackets:
/// `{cmd, ["workdir"]: targetDir}` passes `workdir` exactly as the plain key
/// does, and skipping it left a call into another checkout on its baseline
/// ([`computed_key`]). Only a quoted name computes this key — `[workdir]` is
/// whatever the variable `workdir` holds — and a template-literal key
/// exists only in brackets.
///
/// This is a scanner over key/value SHAPE, not a JavaScript parser: every key
/// is found wherever it sits, and where it sits only decides what a readable
/// path is worth. A key inside a string, a template or a comment
/// ([`inert_spans`]) — `'{workdir: "/x"}'`, a shell command that echoes one,
/// a commented-out call — is text no call ran with. Read as a path, it was the
/// only evidence beside a call that named no directory and moved the whole
/// window to `/x`. It is opaque instead: never hidden, because a lexer that
/// misreads an apostrophe in prose must not hide a real key after it, and
/// never a directory the window is placed in.
fn workdirs_in_literal(input: &str) -> Vec<WorkdirEvidence> {
    static WORKDIR_KEY_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    // The leading class is the identifier boundary the regex crate has no
    // look-behind for: it consumes the character before the key, which is
    // never part of the value read after the match.
    let re = WORKDIR_KEY_RE.get_or_init(|| {
        regex::Regex::new(r#"(?:^|[^A-Za-z0-9_$])(?P<key>"workdir"|'workdir'|`workdir`|workdir)"#)
            .expect("valid regex")
    });
    let mut found = Vec::new();
    // Lexed on the first readable path only: most inputs never get there.
    let mut inert: Option<Vec<std::ops::Range<usize>>> = None;
    for caps in re.captures_iter(input) {
        let Some(key) = caps.name("key") else {
            continue;
        };
        let quoted = key.as_str().starts_with(['"', '\'', '`']);
        let mut before = &input[..key.start()];
        let mut rest = &input[key.end()..];
        // `workdirs`, `workdir_hint`: another identifier that only starts the
        // same way.
        if !quoted
            && rest.starts_with(|c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '$'))
        {
            continue;
        }
        if quoted {
            match computed_key(before, rest) {
                Some((open, close)) => (before, rest) = (open, close),
                None if key.as_str().starts_with('`') => continue,
                None => {}
            }
        }
        if !opens_property(before) {
            continue;
        }
        // A block comment that never closes, on either side of the separator,
        // hid the value.
        let Some(rest) = skip_comments(rest) else {
            found.push(WorkdirEvidence::Opaque);
            continue;
        };
        let Some(rest) = rest.strip_prefix(':') else {
            // The shorthand `{cmd, workdir}` passes the variable's value: a
            // directory only the runtime knew.
            if !quoted && rest.starts_with([',', '}']) {
                found.push(WorkdirEvidence::Opaque);
            }
            continue;
        };
        let Some(value) = skip_comments(rest) else {
            found.push(WorkdirEvidence::Opaque);
            continue;
        };
        let delimiter = match value.chars().next() {
            Some(quote @ ('"' | '\'' | '`')) => quote,
            // Not a literal. It is still the value the call ran with — an
            // identifier, a call, or an expression that opens with
            // punctuation, `[root, repo].join('/')` or `!local ? foreign :
            // base` — and only the runtime knew which directory that was.
            _ => {
                if !names_no_directory(value) {
                    found.push(WorkdirEvidence::Opaque);
                }
                continue;
            }
        };
        match literal_body(&value[1..], delimiter) {
            Some(literal) if delimiter == '`' && literal.body.contains("${") => {
                found.push(WorkdirEvidence::Opaque);
            }
            Some(literal) if literal.masked || !value_ends_at(&value[1 + literal.end..]) => {
                found.push(WorkdirEvidence::Opaque);
            }
            Some(literal) => {
                let body = literal.body.trim();
                if body.is_empty() {
                    continue;
                }
                let inert = inert.get_or_insert_with(|| inert_spans(input));
                if within(inert, key.start()) {
                    found.push(WorkdirEvidence::Opaque);
                } else {
                    found.push(WorkdirEvidence::Explicit(body.to_string()));
                }
            }
            None => found.push(WorkdirEvidence::Opaque),
        }
    }
    if input.contains("exec_command") {
        let inert = inert.get_or_insert_with(|| inert_spans(input));
        if exec_arguments_unseen(input, inert) {
            found.push(WorkdirEvidence::Opaque);
        }
    }
    found
}

/// Does an `exec_command` call in `input` take an argument whose keys are not
/// all written at the call?
///
/// The key scan reads every `workdir` written anywhere in the input, so an
/// options object built in the same script is seen whether it is spread into
/// the call or passed by name. One built at runtime is not:
/// `tools.exec_command({cmd, ...opts})` or `tools.exec_command(args)`, with
/// `opts` or `args` parsed from a tool's output or kept from an earlier call,
/// runs in a directory only the runtime knew. Such a call is opaque, never "no
/// evidence", because treating it as no evidence left the window on its
/// baseline while the call worked in another checkout. A spread nested deeper,
/// such as `{env: {...process.env}}`, feeds another property. A string argument
/// or no argument names no directory. An argument object that never closes
/// was cut off and is opaque.
///
/// `exec_command` is the one tool that takes a `workdir`. Other tools take
/// variables as a matter of course (`tools.apply_patch(patch)`), and reading
/// those as unseen directories would unplace ordinary windows.
fn exec_arguments_unseen(input: &str, inert: &[std::ops::Range<usize>]) -> bool {
    static EXEC_CALL_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = EXEC_CALL_RE.get_or_init(|| {
        regex::Regex::new(r"(?:^|[^A-Za-z0-9_$])(?P<name>exec_command)\s*\(").expect("valid regex")
    });
    for caps in re.captures_iter(input) {
        let (Some(name), Some(call)) = (caps.name("name"), caps.get(0)) else {
            continue;
        };
        if within(inert, name.start()) {
            continue;
        }
        let Some(argument) = skip_comments(&input[call.end()..]) else {
            return true;
        };
        if argument.starts_with([')', '"', '\'', '`']) {
            continue;
        }
        if !argument.starts_with('{')
            || !object_is_whole(input.as_bytes(), input.len() - argument.len(), inert)
        {
            return true;
        }
    }
    false
}

/// Does the object literal opening at `open` close, with no spread of its
/// own? Text and comments ([`inert_spans`]) are skipped, so a `...` or a brace
/// inside a string is neither a spread nor the object's end.
fn object_is_whole(bytes: &[u8], open: usize, inert: &[std::ops::Range<usize>]) -> bool {
    let mut next = inert.partition_point(|span| span.start < open);
    let mut depth = 0usize;
    let mut at = open;
    while at < bytes.len() {
        if let Some(span) = inert.get(next).filter(|span| span.start == at) {
            at = span.end;
            next += 1;
            continue;
        }
        match bytes[at] {
            b'{' | b'[' | b'(' => depth += 1,
            b'}' | b']' | b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return true;
                }
            }
            b'.' if depth == 1 && bytes[at..].starts_with(b"...") => return false,
            _ => {}
        }
        at += 1;
    }
    false
}

/// Byte ranges of `input` that JavaScript reads as text rather than code:
/// string and template literals from their opening quote, comments, and
/// regular-expression literals. The `${…}` of a template is code again.
///
/// A lexer's first pass, not a parser, and it only decides what a `workdir`
/// key is worth, never whether one is seen ([`workdirs_in_literal`]). A quote
/// it pairs wrongly — an apostrophe in prose outside any comment, a regular
/// expression it takes for a division — makes the code after it look like
/// text, which can only turn a readable path opaque. A `'` or `"` string ends
/// at the line break JavaScript forbids inside it, so a stray quote costs the
/// rest of its line and no more.
fn inert_spans(input: &str) -> Vec<std::ops::Range<usize>> {
    let bytes = input.as_bytes();
    let mut spans = Vec::new();
    // The brace depth inside each `${` still open, innermost last.
    let mut interpolations: Vec<usize> = Vec::new();
    // A `/` opens a regular expression where no value has just ended: at the
    // start, and after an operator or an opening bracket. After a name, a
    // number or a closing bracket it divides.
    let mut regex_may_open = true;
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        match bytes[at] {
            quote @ (b'"' | b'\'') => {
                at = quoted_end(bytes, at + 1, quote);
                regex_may_open = false;
            }
            b'`' => {
                at = template_end(bytes, at + 1, &mut interpolations);
                regex_may_open = false;
            }
            b'}' if interpolations.last() == Some(&0) => {
                interpolations.pop();
                at = template_end(bytes, at + 1, &mut interpolations);
                regex_may_open = false;
            }
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                at = bytes[at..]
                    .iter()
                    .position(|&byte| byte == b'\n')
                    .map_or(bytes.len(), |line| at + line);
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at = bytes[at + 2..]
                    .windows(2)
                    .position(|pair| pair == b"*/")
                    .map_or(bytes.len(), |close| at + 2 + close + 2);
            }
            b'/' if regex_may_open => {
                at = regex_end(bytes, at + 1);
                regex_may_open = false;
            }
            byte => {
                if let Some(depth) = interpolations.last_mut() {
                    match byte {
                        b'{' => *depth += 1,
                        b'}' => *depth -= 1,
                        _ => {}
                    }
                }
                if !byte.is_ascii_whitespace() {
                    regex_may_open = matches!(
                        byte,
                        b'(' | b'['
                            | b'{'
                            | b'}'
                            | b','
                            | b';'
                            | b':'
                            | b'='
                            | b'!'
                            | b'&'
                            | b'|'
                            | b'?'
                            | b'+'
                            | b'-'
                            | b'*'
                            | b'%'
                            | b'<'
                            | b'>'
                            | b'~'
                            | b'^'
                    );
                }
                at += 1;
                continue;
            }
        }
        spans.push(start..at);
    }
    spans
}

/// Just past the quote that closes a `'` or `"` string, or at the line break
/// that ends it unclosed. An escaped line break continues the string.
fn quoted_end(bytes: &[u8], mut at: usize, quote: u8) -> usize {
    while at < bytes.len() {
        match bytes[at] {
            b'\\' if bytes[at + 1..].starts_with(b"\r\n") => at += 3,
            b'\\' => at += 2,
            b'\n' | b'\r' => return at,
            byte if byte == quote => return at + 1,
            _ => at += 1,
        }
    }
    bytes.len()
}

/// Just past the backtick that closes a template, or past a `${` that opens
/// code inside it, whose depth is then pushed onto `interpolations`.
fn template_end(bytes: &[u8], mut at: usize, interpolations: &mut Vec<usize>) -> usize {
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => at += 2,
            b'`' => return at + 1,
            b'$' if bytes.get(at + 1) == Some(&b'{') => {
                interpolations.push(0);
                return at + 2;
            }
            _ => at += 1,
        }
    }
    bytes.len()
}

/// Just past the `/` that closes a regular expression, or at the line break
/// that shows it was none. A `/` inside a character class does not close it.
fn regex_end(bytes: &[u8], mut at: usize) -> usize {
    let mut in_class = false;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => at += 2,
            b'\n' | b'\r' => return at,
            b'[' => {
                in_class = true;
                at += 1;
            }
            b']' => {
                in_class = false;
                at += 1;
            }
            b'/' if !in_class => return at + 1,
            _ => at += 1,
        }
    }
    bytes.len()
}

/// Does one of the sorted, disjoint `spans` hold `at` past its first byte? A
/// string that OPENS at `at` is the key's own quote, not text around it.
fn within(spans: &[std::ops::Range<usize>], at: usize) -> bool {
    let before = spans.partition_point(|span| span.start < at);
    before > 0 && at < spans[before - 1].end
}

/// The text on either side of a quoted key that brackets compute —
/// `["workdir"]` — with the brackets taken off, or `None` when the key is not
/// computed. What precedes the `[` then decides, as for any key, whether a
/// property opens there: `args["workdir"]` reads a member and
/// `["workdir", other]` builds an array, and neither names a directory.
fn computed_key<'a>(before: &'a str, after: &'a str) -> Option<(&'a str, &'a str)> {
    let open = before.trim_end().strip_suffix('[')?;
    let close = skip_comments(after)?.strip_prefix(']')?;
    Some((open, close))
}

/// Does a key that follows `before` open an object property?
///
/// Only right after `{` or `,`, or at the very start of the input (an
/// `arguments` string may be the object's body alone), with whole comments
/// between them skipped: `{cmd, /* selected target */ workdir: targetDir}` is
/// a property, and so is a key on the line after `{cmd, // note`. A comment on
/// the key's OWN line
/// that is still open where the key starts contains the key — `// workdir: the
/// repo` is prose — so a line comment only ever ends a line above the key.
///
/// A line comment is found from the right, and each one stripped leaves the
/// code before it to decide: `{url: "http://host", // note` ends in `,` once
/// the comment goes, and the `//` inside the string is never reached.
fn opens_property(before: &str) -> bool {
    let mut rest = before;
    // No line break between here and the key yet: a `//` on this line would
    // have commented the key out.
    let mut key_line = true;
    loop {
        let trimmed = rest.trim_end();
        key_line &= !rest[trimmed.len()..].contains('\n');
        rest = trimmed;
        if rest.is_empty() || rest.ends_with(['{', ',']) {
            return true;
        }
        if let Some(body) = rest.strip_suffix("*/") {
            match body.rfind("/*") {
                Some(open) => {
                    rest = &body[..open];
                    continue;
                }
                None => return false,
            }
        }
        if key_line {
            return false;
        }
        let line_start = rest.rfind('\n').map_or(0, |newline| newline + 1);
        match rest[line_start..].rfind("//") {
            Some(slash) => rest = &rest[..line_start + slash],
            None => return false,
        }
    }
}

/// A string literal read up to its closing delimiter ([`literal_body`]).
struct Literal {
    body: String,
    /// Byte offset in the scanned text just past the closing delimiter.
    end: usize,
    /// An escape in it can spell path structure its body does not show.
    masked: bool,
}

/// The body of a string literal that `delimiter` opened, up to the matching
/// unescaped `delimiter`; `None` when it never closes. Only `\\` and an
/// escaped delimiter are unescaped (see [`workdirs_in_literal`]).
fn literal_body(rest: &str, delimiter: char) -> Option<Literal> {
    let mut body = String::new();
    let mut masked = false;
    let mut chars = rest.char_indices();
    while let Some((at, c)) = chars.next() {
        if c == delimiter {
            return Some(Literal {
                body,
                end: at + c.len_utf8(),
                masked,
            });
        }
        if c != '\\' {
            body.push(c);
            continue;
        }
        match chars.next() {
            Some((_, escaped)) if escaped == '\\' || escaped == delimiter => body.push(escaped),
            Some((at, other)) => {
                masked |= escape_masks_path(other, &rest[at + other.len_utf8()..]);
                body.push('\\');
                body.push(other);
            }
            None => return None,
        }
    }
    None
}

/// Can the escape `\` + `escaped`, with `after` following it, spell path
/// structure that its written form does not show?
///
/// A backslash is kept as written so that a Windows workdir survives
/// ([`workdirs_in_literal`]), and that reading is sound only while the escape
/// cannot stand for a dot, a separator or a drive colon. A hex, Unicode or
/// octal escape can spell any character — `\x2e`, `\u002f` and `\56` are a
/// dot, a slash and a dot — while `\.`, `\/` and `\:` are those characters,
/// and a backslash before a line break removes both, joining what surrounds
/// it. A `\x` or `\u` that is no valid escape is a syntax error, so nothing
/// ran with it, and `C:\xampp` or `C:\users` still read as written.
fn escape_masks_path(escaped: char, after: &str) -> bool {
    let hex = |len: usize| {
        after
            .get(..len)
            .is_some_and(|digits| digits.chars().all(|c| c.is_ascii_hexdigit()))
    };
    match escaped {
        'x' => hex(2),
        'u' => after.starts_with('{') || hex(4),
        '0'..='9' | '.' | '/' | ':' | '\n' | '\r' | '\u{2028}' | '\u{2029}' => true,
        _ => false,
    }
}

/// Does a property value end right after its closing quote?
///
/// `rest` is what follows the literal. Only the end of the property may come
/// next — `,` or `}`, the close of an enclosing call or array, a statement
/// end, or the end of the input. Anything else continues an expression:
/// `workdir: "/repos/vista" + "-private"` names `/repos/vista-private`, and
/// reading the first operand as the path would place the call in another
/// checkout. A property written without the comma JavaScript requires before
/// the next one is read as an expression too, and so is prose that quotes a
/// path after `workdir:` and keeps talking; either costs the window its
/// attribution, never its correctness.
///
/// A comment ends nothing by itself: JavaScript allows one between operands,
/// so `"/repos/vista" /* note */ + "-private"` is still the expression. Each
/// whole comment is skipped and what follows it decides. A block comment that
/// never closes hid whatever came after it, so the value is opaque, as for a
/// literal that never closes.
fn value_ends_at(rest: &str) -> bool {
    skip_comments(rest)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with([',', '}', ')', ']', ';']))
}

/// `rest` from its next token on, past whitespace and every whole comment.
///
/// A line comment runs to the end of its line, or of the input. `None` when a
/// block comment never closes: whatever followed it is unreadable.
fn skip_comments(rest: &str) -> Option<&str> {
    let mut rest = rest.trim_start();
    loop {
        if let Some(comment) = rest.strip_prefix("//") {
            match comment.find('\n') {
                Some(newline) => rest = comment[newline + 1..].trim_start(),
                None => return Some(""),
            }
        } else if let Some(comment) = rest.strip_prefix("/*") {
            rest = comment[comment.find("*/")? + 2..].trim_start();
        } else {
            return Some(rest);
        }
    }
}

/// A `workdir` value that is not a literal and still names no directory.
///
/// `null` and `undefined` ask for the default directory, a number is no path,
/// and a key with nothing before its separator has no value. Each counts only
/// when the value ends right after it: `undefined ?? otherDir` and
/// `0 || otherDir` are expressions like any other.
fn names_no_directory(value: &str) -> bool {
    let unsigned = value.strip_prefix(['-', '+']).unwrap_or(value);
    let end = unsigned
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '.')))
        .unwrap_or(unsigned.len());
    let token = &unsigned[..end];
    let number = token.starts_with(|c: char| c.is_ascii_digit())
        || token
            .strip_prefix('.')
            .is_some_and(|fraction| fraction.starts_with(|c: char| c.is_ascii_digit()));
    let no_directory = token.is_empty() || number || matches!(token, "null" | "undefined");
    no_directory && value_ends_at(&unsigned[end..])
}

/// Resolve one workdir string to an absolute spelling, lexically normalized.
///
/// A relative workdir (`.`, `packages/api`) belongs to the TURN's cwd, never to
/// wherever `aicx` happens to be running: resolving it against the process cwd
/// would let the caller's own checkout adopt a historical rollout's messages.
/// Without a baseline there is nothing to resolve it against, so it stays
/// unknown.
///
/// Absoluteness is a property of the RECORDED spelling, not of the host doing
/// the parsing: a Windows rollout replayed on macOS still has `C:\repo` as an
/// absolute baseline. Asking `Path::is_absolute` made every relative workdir
/// of such a rollout unresolvable, and the window unattributed.
fn resolve_candidate(path: &str, baseline: Option<&str>) -> Option<String> {
    let path = path.trim();
    if let Some(on_drive) = on_baseline_drive(path, baseline) {
        return Some(lexically_normalized(&on_drive));
    }
    if absolute_anywhere(path) {
        return Some(lexically_normalized(path));
    }
    let base = baseline?.trim();
    if !absolute_anywhere(base) {
        return None;
    }
    let path = relative_to_baseline_drive(path, base)?;
    let separator = separator_of(base);
    Some(lexically_normalized(&format!(
        "{}{separator}{path}",
        base.trim_end_matches(['/', '\\'])
    )))
}

/// The part of a relative `path` that joins onto `base`, if any does.
///
/// A drive-relative Windows path (`C:fleet`, bare `C:`) is relative to the
/// current directory OF ITS DRIVE. The only drive whose current directory a
/// rollout records is the baseline's, so `D:fleet` under `D:\vista` is
/// `D:\vista\fleet`, while `C:fleet` under it names a directory nobody wrote
/// down: joining it anyway fabricated `D:\vista\C:fleet`, which the lexical
/// reduction then placed inside the baseline. A baseline without a drive
/// letter (`\repo`, UNC) knows no drive's current directory at all. Under a
/// Unix baseline there are no drives, and `C:fleet` is an ordinary name.
fn relative_to_baseline_drive<'a>(path: &'a str, base: &str) -> Option<&'a str> {
    let bytes = path.as_bytes();
    let drive_relative = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if !drive_relative || !windows_shaped(base) {
        return Some(path);
    }
    let base = base.as_bytes();
    let same_drive = base.len() >= 2 && base[1] == b':' && base[0].eq_ignore_ascii_case(&bytes[0]);
    same_drive.then(|| &path[2..])
}

/// A Windows path rooted without a drive (`\repo`, or `/repo` in either
/// separator) names that directory on the CURRENT drive, which for a tool call
/// is the drive of the turn's cwd. Kept drive-less it matched neither
/// `C:\repo` nor anything else, and the window went unattributed. Only a
/// baseline with a drive letter lends one: that session ran on Windows, where
/// `/repo` is not a Unix path (a session inside WSL records `/mnt/c/repo`).
/// Under a Unix baseline `/repo` stays as written. UNC (`\\server\share`,
/// `//server/share`) names its own root and is left alone.
fn on_baseline_drive(path: &str, baseline: Option<&str>) -> Option<String> {
    let mut leading = path.bytes().take(2);
    let rooted = leading
        .next()
        .is_some_and(|byte| matches!(byte, b'\\' | b'/'))
        && !leading
            .next()
            .is_some_and(|byte| matches!(byte, b'\\' | b'/'));
    if !rooted {
        return None;
    }
    let base = baseline?.trim();
    let bytes = base.as_bytes();
    (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        .then(|| format!("{}{path}", &base[..2]))
}

/// Is `path` absolute on ANY platform a rollout can come from? `/unix`,
/// `C:\` or `C:/` drive roots, and `\`-rooted or UNC (`\\server\share`)
/// Windows paths. A drive-relative `C:repo` is not.
fn absolute_anywhere(path: &str) -> bool {
    path.starts_with(['/', '\\']) || windows_drive_root(path)
}

fn windows_drive_root(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

/// Is this a Windows spelling, where `\` is a separator rather than a legal
/// filename character? A drive, a `\`-rooted path, or a UNC root in either
/// separator.
fn windows_shaped(path: &str) -> bool {
    path.starts_with('\\')
        || forward_slash_unc(path)
        || (path.len() >= 2
            && path.as_bytes()[0].is_ascii_alphabetic()
            && path.as_bytes()[1] == b':')
}

/// `//server/share`: Windows reads it as `\\server\share`. Read as a Unix
/// path, `..` popped the share and `//server/share/../other` became
/// `/server/other`, another share. Exactly two leading `/` and a name: POSIX
/// leaves that prefix implementation-defined, which is room for this reading,
/// while three or more collapse to `/` everywhere.
fn forward_slash_unc(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() > 2 && bytes[0] == b'/' && bytes[1] == b'/' && !matches!(bytes[2], b'/' | b'\\')
}

/// The separator a path is spelled with, so a join or a rewrite keeps it.
fn separator_of(path: &str) -> char {
    if windows_shaped(path) && (path.contains('\\') || !path.contains('/')) {
        '\\'
    } else {
        '/'
    }
}

/// Only a path absolute on THIS host may be probed on its filesystem. Any
/// other spelling — a Windows path on Unix, a Unix path on Windows — would be
/// resolved against the process cwd, which is exactly the leak the relative
/// case refuses.
fn host_path(path: &str) -> Option<&Path> {
    let path = Path::new(path);
    path.is_absolute().then_some(path)
}

/// Is this Codex payload type an executable tool CALL, whose arguments can
/// carry a `workdir`?
///
/// The full adapter routes calls into `push_tool_turn` from two envelopes:
/// `response_item` (`function_call`, `custom_tool_call`, `web_search_call`)
/// and `event_msg` (`function_call`, `tool_call`, `mcp_tool_call`). The
/// bounded catalog reader and the over-cap check read their set from here,
/// so no reader can quietly skip a call type another one scopes. Results
/// (`function_call_output`, `mcp_tool_call_end`, …) are not calls.
pub fn is_tool_call_payload_type(payload_type: &str) -> bool {
    matches!(
        payload_type,
        "function_call" | "custom_tool_call" | "web_search_call" | "tool_call" | "mcp_tool_call"
    )
}

/// Does the readable head of an over-cap record look like a tool-call
/// envelope? The record is truncated (never valid JSON), so this reads the
/// visible prefix only: the envelope and payload discriminators are written
/// before the oversized `arguments`/`input` body, so they survive the cap.
pub fn truncated_record_is_tool_call(prefix: &str) -> bool {
    // Key order inside a record is not a contract, so this scans the whole
    // visible prefix rather than a fixed head: the discriminator can sit
    // after a megabyte of `arguments`. Over-cap records are rare and their
    // bytes are already in hand.
    let discriminators = record_type_discriminators(prefix);
    // `function_call_output` / `mcp_tool_call_end` are RESULTS, not calls:
    // they carry no workdir, so an oversized one is not lost evidence. Exact
    // equality against the parsed value keeps them out.
    if discriminators
        .iter()
        .any(|value| is_tool_call_payload_type(value))
    {
        return true;
    }
    // Otherwise the question is whether the PAYLOAD discriminator was
    // readable at all. Key order inside a record is not a contract: an
    // envelope whose own `type` is visible can still be truncated before its
    // payload type, hiding a tool call behind a megabyte of `arguments`.
    // Seeing one discriminator (the envelope's) or none is not evidence that
    // nothing was lost, so it fails closed.
    //
    // Only a discriminator whose VALUE was read counts toward that proof. A
    // `"type":"funct` cut by the cap is a key without an answer: counting it
    // let an envelope plus a truncated payload type pass as two readable
    // discriminators and keep the window attributed.
    discriminators
        .iter()
        .filter(|value| !value.is_empty())
        .count()
        < 2
}

/// Values of every `"type"` key that can be a RECORD discriminator in the
/// readable head of a truncated record, in order.
///
/// A truncated record is never valid JSON, so this is a byte scanner rather
/// than a parse. Two things make it structural rather than a substring count:
///
/// * it tracks JSON string state, so `"type":` written inside an argument
///   body is text, not a key; and
/// * it tracks which object each key sits in and keeps only the envelope's
///   own keys and those of its `payload` object, which is where a Codex
///   record's discriminators live. Depth alone is not that: a sibling such
///   as `metadata: {"type":"note"}` is as shallow as the payload and is
///   content all the same.
///
/// Both matter for the same reason: the threshold below decides whether an
/// unreadable record is allowed to keep its window attributed. A count that
/// payload CONTENT or a sibling object can raise — an `arguments` object
/// carrying its own `"type"` field, a `metadata` note before the payload —
/// hands that decision to bytes that say nothing about the payload.
fn record_type_discriminators(prefix: &str) -> Vec<String> {
    let bytes = prefix.as_bytes();
    // Reads the string token starting at `bytes[at] == b'"'`, returning its
    // contents and the index just past the closing quote. An unterminated
    // string (the truncation itself) ends the scan.
    let read_string = |at: usize| -> Option<(String, usize)> {
        let mut out = String::new();
        let mut cursor = at + 1;
        while cursor < bytes.len() {
            match bytes[cursor] {
                b'\\' => cursor += 2,
                b'"' => return Some((out, cursor + 1)),
                byte => {
                    out.push(byte as char);
                    cursor += 1;
                }
            }
        }
        None
    };

    let mut found = Vec::new();
    // One entry per open container, outermost first: is it the envelope's
    // `payload` object?
    let mut containers: Vec<bool> = Vec::new();
    // The envelope key whose value comes next, so the container it opens
    // knows whether it is the payload.
    let mut envelope_key: Option<String> = None;
    let mut idx = 0usize;
    while idx < bytes.len() {
        match bytes[idx] {
            open @ (b'{' | b'[') => {
                containers.push(
                    open == b'{'
                        && containers.len() == 1
                        && envelope_key.as_deref() == Some("payload"),
                );
                envelope_key = None;
                idx += 1;
                continue;
            }
            b'}' | b']' => {
                containers.pop();
                idx += 1;
                continue;
            }
            b'"' => {}
            _ => {
                idx += 1;
                continue;
            }
        }
        let Some((token, after)) = read_string(idx) else {
            break;
        };
        idx = after;
        // Only a string used as a KEY names anything.
        let mut cursor = idx;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] != b':' {
            continue;
        }
        let discriminator = token == "type"
            && match containers.len() {
                1 => true,
                2 => containers[1],
                _ => false,
            };
        if containers.len() == 1 {
            envelope_key = Some(token);
        }
        if !discriminator {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        // A discriminator truncated before or inside its value, or carrying a
        // value that is not a string, is recorded as empty: the key was
        // there, its answer was not read, and the threshold above must not
        // count it as proof of anything.
        match bytes.get(cursor) {
            Some(b'"') => match read_string(cursor) {
                Some((value, after)) => {
                    found.push(value);
                    idx = after;
                }
                None => {
                    found.push(String::new());
                    break;
                }
            },
            _ => found.push(String::new()),
        }
    }
    found
}

/// Normalize one explicit workdir to its repo-level identity.
///
/// Filesystem-only and bounded (ancestor walk to the first `.git`); no
/// subprocess and no remote lookup — the canonical root path is identity
/// enough. A path that does not exist here resolves to nothing: an ancestor's
/// `.git` is not evidence about a directory that was never there, and treating
/// `/repo-b/deleted-fleet` as `/repo-b` would stamp positive scope on a
/// workdir the contract calls unattributable.
pub fn normalize_workdir(path: &str, baseline: Option<&str>) -> WorkdirIdentity {
    let Some(candidate) = resolve_candidate(path, baseline) else {
        // Nothing to resolve a bare relative token against; it stays as-is.
        return WorkdirIdentity::Unresolved(trim_path(path).to_string());
    };
    if let Some(on_host) = host_path(&candidate)
        && on_host.exists()
        && let Some(root) = discover_git_root(on_host)
    {
        // Canonical form, so one checkout reached through a symlink or a
        // `/var` vs `/private/var` spelling is one identity.
        return WorkdirIdentity::Resolved(std::fs::canonicalize(&root).unwrap_or(root));
    }
    // Unresolvable, but not unknown: a replayed session whose checkout is gone
    // from this machine still told us `.` or `packages/api` RELATIVE TO its
    // baseline. Discarding that join and keeping the bare token would compare
    // `.` against `/old/repo` and throw away turns that are plainly in scope.
    WorkdirIdentity::Unresolved(candidate)
}

/// The workdir as the rollout RECORDED it, made comparable without asking the
/// filesystem anything: a relative workdir is joined onto the window's
/// baseline, `.`/`..` are collapsed, trailing separators dropped.
///
/// This is the host-independent form of the evidence. [`normalize_workdir`]
/// answers "which checkout is this ON THIS HOST", which changes when a
/// nested checkout appears; this answers "which path did the session name",
/// which never does. Privacy filters and cache identities need the latter:
/// both must see the raw evidence, not what survived one host's reduction.
pub fn recorded_workdir(path: &str, baseline: Option<&str>) -> String {
    match resolve_candidate(path, baseline) {
        Some(candidate) => trim_path(&candidate).to_string(),
        None => trim_path(path).to_string(),
    }
}

/// Collapse `.` and `..` without touching the filesystem, in the path's own
/// spelling.
///
/// The lexical comparison below is the last resort for paths that do not exist
/// here, so it must at least compare like with like: `/old/repo` joined with
/// `.` is `/old/repo`, not `/old/repo/.`. It works on the string rather than on
/// `Path` components because components are the HOST's grammar: on Unix a
/// Windows path is one opaque component, so `C:\repo\..\other` never
/// collapsed. `\` separates only in a Windows spelling; in a Unix path it is a
/// legal filename character and stays one.
///
/// A UNC root is the whole `\\server\share` (or `//server/share`): Windows
/// anchors `..` at the share, so `\\server\share\..\other` is
/// `\\server\share\other`. Popping the share as an ordinary component
/// fabricated `\\server\other`, another share, and a window whose baseline was
/// that share read the call as its own.
fn lexically_normalized(path: &str) -> String {
    let path = path.trim();
    let windows = windows_shaped(path);
    let separator = separator_of(path);
    let is_separator = |c: char| c == '/' || (windows && c == '\\');
    let unc = windows && (path.starts_with("\\\\") || path.starts_with("//"));
    let root_len = if windows_drive_root(path) {
        3
    } else if unc {
        unc_root_len(path, is_separator)
    } else if path.starts_with(is_separator) {
        1
    } else if windows {
        // Drive-relative `C:repo`: the drive is the only root there is.
        2
    } else {
        0
    };
    let (root, rest) = path.split_at(root_len);
    let root: String = root
        .chars()
        .map(|c| if is_separator(c) { separator } else { c })
        .collect();
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split(is_separator) {
        match part {
            "" | "." => {}
            ".." => match parts.last() {
                Some(last) if *last != ".." => {
                    parts.pop();
                }
                // Above a root there is nothing to climb to; a relative path
                // keeps the `..` it cannot resolve.
                _ if root.is_empty() => parts.push(".."),
                _ => {}
            },
            part => parts.push(part),
        }
    }
    let body = parts.join(separator.to_string().as_str());
    if root.is_empty() && body.is_empty() {
        ".".to_owned()
    } else if unc && !body.is_empty() {
        // The UNC root stops at the share name, before its separator.
        format!("{root}{separator}{body}")
    } else {
        format!("{root}{body}")
    }
}

/// The byte length of a UNC root, `\\server\share`, up to the separator after
/// the share name; a path with no share yet is its server alone.
fn unc_root_len(path: &str, is_separator: impl Fn(char) -> bool) -> usize {
    let mut names = 0;
    let mut in_name = false;
    for (at, c) in path.char_indices().skip(2) {
        if !is_separator(c) {
            in_name = true;
        } else if in_name {
            in_name = false;
            names += 1;
            if names == 2 {
                return at;
            }
        }
    }
    path.len()
}

fn trim_path(path: &str) -> &str {
    let trimmed = path.trim().trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        path.trim()
    } else {
        trimmed
    }
}

/// Lexical containment — the fallback used ONLY where repo identity cannot be
/// established on this machine (neither path resolves to a checkout).
///
/// Both sides are compared lexically normalized, so a scope written with a
/// trailing `.` or `..` still contains what it names, and in the form the
/// recorded platform resolves them: see [`comparable_spelling`].
fn lexically_within(candidate: &str, scope_root: &str) -> bool {
    let candidate = comparable_spelling(&lexically_normalized(candidate));
    let scope = comparable_spelling(&lexically_normalized(scope_root));
    let windows = windows_shaped(&scope);
    let candidate = trim_path(&candidate);
    let scope = trim_path(&scope);
    candidate == scope
        || candidate
            .strip_prefix(scope)
            .is_some_and(|rest| rest.starts_with('/') || (windows && rest.starts_with('\\')))
}

/// One path in the spelling its own platform treats as identical.
///
/// Lexical normalization keeps each path's separator, so a Windows baseline
/// `C:/Users/dev/repo` and a workdir `C:\Users\dev\repo\pkg` from the same
/// rollout never shared a prefix, and `c:\users\…` never matched `C:\Users\…`:
/// the window went unattributed and its intents were dropped. Windows accepts
/// either separator and, by default, ignores letter case, so a Windows
/// spelling is folded to `\` and ASCII lowercase before it is compared. ASCII
/// folding is a subset of what Windows itself folds, so it never makes two
/// paths equal that Windows would keep apart. A Unix spelling is left byte for
/// byte: there `\` is a filename character and case is significant.
fn comparable_spelling(path: &str) -> String {
    if !windows_shaped(path) {
        return path.to_owned();
    }
    path.chars()
        .map(|c| {
            if c == '/' {
                '\\'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect()
}

/// Does `candidate` belong to the same checkout as `scope_root`?
///
/// The single membership predicate for scope decisions. Repo identity wins
/// over path shape: a nested checkout or submodule sits lexically below its
/// parent and is still a different repository, so a resolved candidate must
/// match the scope's resolved repo ROOT, not merely live under its path.
/// Lexical containment survives only where identity is genuinely unknowable —
/// a workdir that does not exist on this machine (historical rollouts,
/// foreign-machine paths), where the declared scope is the only evidence
/// there is.
pub fn workdir_within_scope(candidate: &str, scope_root: &str) -> bool {
    match normalize_workdir(candidate, Some(scope_root)) {
        // The candidate is a real checkout here: only root identity counts.
        // A nested checkout resolves to ITSELF, so it is never absorbed by
        // the enclosing path.
        WorkdirIdentity::Resolved(candidate_root) => match normalize_workdir(scope_root, None) {
            WorkdirIdentity::Resolved(scope) => candidate_root == scope,
            WorkdirIdentity::Unresolved(scope) => {
                trim_path(&candidate_root.to_string_lossy()) == trim_path(&scope)
            }
        },
        // Unknowable path. The declared scope is the only evidence there is,
        // but it is evidence ONLY where the scope root is equally unknowable.
        // A scope root that resolves here cannot adopt a path it cannot prove:
        // a removed nested checkout or submodule sits lexically below its
        // parent and is still a different repository, so lexical containment
        // would re-open exactly the leak identity checking exists to close.
        WorkdirIdentity::Unresolved(candidate) => match normalize_workdir(scope_root, None) {
            WorkdirIdentity::Resolved(_) => false,
            WorkdirIdentity::Unresolved(scope) => lexically_within(&candidate, &scope),
        },
    }
}

/// Is `candidate` plausibly INSIDE `scope_root`, for the purpose of counting
/// how many repository identities a turn window touched?
///
/// Deliberately more tolerant than [`workdir_within_scope`], because the two
/// answer different questions. Membership asks "may this frame be served as
/// project P?", where an unprovable claim must fail closed. Reduction asks "is
/// this a SECOND repository?", where an unprovable claim must fail OPEN: a
/// window judged [`WindowScope::Unattributed`] has its frames dropped outright
/// by the project filter, so calling every vanished path a second identity
/// silently deletes ordinary operator evidence — a deleted `target/`, a
/// removed temp dir, a worktree that has been cleaned up.
///
/// The one case where a missing path IS provably its own repository is a
/// declared submodule: the parent's `.gitmodules` still names it after the
/// working tree is gone. That is read here, bounded and filesystem-only, so
/// the common case keeps its evidence and the provable case keeps its
/// identity. A bare nested checkout that was deleted leaves no such trace and
/// is genuinely indistinguishable from a deleted directory; it is absorbed,
/// and that trade is deliberate.
fn plausibly_within_scope(candidate: &str, scope_root: &str) -> bool {
    match normalize_workdir(candidate, Some(scope_root)) {
        // Provable on both sides: identity decides, exactly as for membership.
        WorkdirIdentity::Resolved(_) => workdir_within_scope(candidate, scope_root),
        WorkdirIdentity::Unresolved(path) => {
            if !lexically_within(&path, trim_path(scope_root)) {
                return false;
            }
            !declared_submodule(scope_root, &path)
        }
    }
}

/// How many repository identities a session's recorded workdirs name.
///
/// The session-level twin of the reduction in [`effective_window_scope`], with
/// the same asymmetry and for the same reason: a count above one makes the
/// whole session mixed, and a mixed session loses its cwd-less frames from
/// project results. So a proven root counts once, however many of its
/// directories the session stood in, and a nested checkout it contains counts
/// again; a path that proves nothing — a deleted `target/` dir, a removed
/// worktree, a historical path — counts only when neither a proven checkout
/// nor another such path plausibly contains it ([`plausibly_within_scope`]).
///
/// The verdict does not depend on the order the paths were recorded in:
/// proven roots are gathered first, and unproven paths are visited ancestors
/// first, so `/repo/a` and `/repo/b` recorded before `/repo` are still one.
pub fn repository_count<'a>(workdirs: impl IntoIterator<Item = &'a str>) -> usize {
    let mut roots: Vec<PathBuf> = Vec::new();
    // Every spelling a proven checkout may be recorded under: the recorded
    // path, its root as the ancestor walk found it, and its canonical root. An
    // unproven path cannot be canonicalized, so it only ever matches the
    // spelling it was written in.
    let mut anchors: Vec<String> = Vec::new();
    let mut unproven: Vec<String> = Vec::new();
    for raw in workdirs {
        let raw = trim_path(raw);
        if raw.is_empty() {
            continue;
        }
        match normalize_workdir(raw, None) {
            WorkdirIdentity::Resolved(root) => {
                anchors.push(raw.to_owned());
                if let Some(walked) = host_path(raw).and_then(discover_git_root) {
                    anchors.push(walked.to_string_lossy().into_owned());
                }
                anchors.push(root.to_string_lossy().into_owned());
                if !roots.contains(&root) {
                    roots.push(root);
                }
            }
            WorkdirIdentity::Unresolved(path) => unproven.push(path),
        }
    }

    unproven.sort_by_key(String::len);
    let mut unproven_roots: Vec<String> = Vec::new();
    for path in unproven {
        let absorbed = anchors
            .iter()
            .chain(&unproven_roots)
            .any(|anchor| plausibly_within_scope(&path, anchor));
        if !absorbed {
            unproven_roots.push(path);
        }
    }
    roots.len() + unproven_roots.len()
}

/// The directory whose `.gitmodules` declares the submodules around `scope`.
///
/// `.gitmodules` lives at the CHECKOUT root and spells its paths relative to
/// it, while a scope is only a working directory — frequently a subdirectory.
/// Reading it where the session happened to stand would miss every
/// declaration and silently absorb the submodule into its parent.
/// `discover_git_root` walks ancestors without canonicalizing, so the root it
/// returns stays comparable with a candidate spelled the same way.
fn submodule_declarations_root(scope: &Path) -> PathBuf {
    discover_git_root(scope).unwrap_or_else(|| scope.to_path_buf())
}

/// Everything the scope verdicts in this module read from THIS host's
/// filesystem about one recorded path, as bytes for a cache key.
///
/// A verdict is a pure function of the recorded evidence plus two reads: how
/// the path resolves ([`normalize_workdir`]: does it exist, which `.git`
/// ancestor is nearest) and which submodules its checkout declares
/// ([`declared_submodule`] reads `.gitmodules` at that checkout root). A cache
/// keyed on source bytes alone therefore serves stale verdicts after a nested
/// checkout appears or a `.gitmodules` is edited, although no source byte
/// moved. Hashing this for every path a session recorded makes the layout
/// part of the cache identity. It lives beside the reads it mirrors, so a
/// verdict that starts reading something new has one place to declare it.
pub fn scope_layout_evidence(path: &str) -> Vec<u8> {
    let mut evidence = Vec::new();
    match normalize_workdir(path, None) {
        WorkdirIdentity::Resolved(root) => {
            evidence.extend_from_slice(b"resolved\0");
            evidence.extend_from_slice(root.to_string_lossy().as_bytes());
        }
        WorkdirIdentity::Unresolved(candidate) => {
            evidence.extend_from_slice(b"unresolved\0");
            evidence.extend_from_slice(candidate.as_bytes());
        }
    }
    evidence.push(0);
    // Only a path spelled for this host has a `.gitmodules` to read, which is
    // exactly the gate `declared_submodule` applies.
    if let Some(scope) = host_path(trim_path(path)) {
        match std::fs::read(submodule_declarations_root(scope).join(".gitmodules")) {
            Ok(body) => {
                evidence.extend_from_slice(b"gitmodules\0");
                evidence.extend_from_slice(&(body.len() as u64).to_le_bytes());
                evidence.extend_from_slice(&body);
            }
            Err(_) => evidence.extend_from_slice(b"no-gitmodules\0"),
        }
    }
    evidence
}

/// Is `path` a submodule the checkout at `scope_root` declares?
///
/// Bounded and offline: one `.gitmodules` read, no subprocess, no network. A
/// missing or unreadable file simply means "not declared".
fn declared_submodule(scope_root: &str, path: &str) -> bool {
    // Both sides stay in the spelling the caller used: the candidate could not
    // be canonicalized (it does not exist), so canonicalizing only the root
    // would leave the two incomparable on any host where the scope path runs
    // through a symlink.
    let Some(scope) = host_path(trim_path(scope_root)) else {
        // A scope spelled for another platform has no `.gitmodules` on this
        // host; reading one relative to the process cwd would be a guess.
        return false;
    };
    let root = submodule_declarations_root(scope);
    let Ok(raw) = std::fs::read_to_string(root.join(".gitmodules")) else {
        return false;
    };
    declares_submodule(&raw, &root.to_string_lossy(), path)
}

/// Does the `.gitmodules` body `raw`, found at the checkout `root`, declare
/// `path` or a directory that contains it?
///
/// The paths are compared in the spelling [`lexically_within`] compares them
/// in ([`comparable_spelling`]), and a declaration under a Windows root is
/// folded the same way. `C:\Repo\vendor\fleet` and `path = Vendor/Fleet` in
/// `C:\repo` are one directory to Windows, and containment had already placed
/// the path inside the checkout on that reading; matching the declaration
/// byte for byte then missed it, and the parent absorbed a submodule it
/// declares.
fn declares_submodule(raw: &str, root: &str, path: &str) -> bool {
    let root = comparable_spelling(&lexically_normalized(root));
    let path = comparable_spelling(&lexically_normalized(path));
    let windows = windows_shaped(&root);
    let is_separator = |c: char| c == '/' || (windows && c == '\\');
    let root = trim_path(&root);
    let Some(rest) = trim_path(&path).strip_prefix(root) else {
        return false;
    };
    // The boundary is a path component: `/repo-old` is not inside `/repo`. A
    // bare root (`/`) already ends in its separator.
    let relative = if root.ends_with(is_separator) {
        Some(rest)
    } else {
        rest.strip_prefix(is_separator)
    };
    let Some(relative) = relative else {
        return false;
    };
    let fold = |spelling: &str| {
        if windows {
            spelling.replace('\\', "/").to_ascii_lowercase()
        } else {
            spelling.to_owned()
        }
    };
    let relative = fold(relative);
    let relative = trim_path(&relative);
    gitmodules_paths(raw)
        .iter()
        .map(|declared| fold(trim_path(declared)))
        .filter(|declared| !declared.is_empty())
        // A submodule's DESCENDANTS are the submodule's repository too. A
        // vanished `vendor/fleet-bus/src` is still inside the declared
        // `vendor/fleet-bus`, so exact equality alone would let the parent
        // checkout absorb it. The boundary is a path component, never a
        // character prefix: `vendor/fleet-bus-old` is a different directory.
        .any(|declared| {
            relative == declared
                || relative
                    .strip_prefix(declared.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        })
}

/// Every `submodule.<name>.path` value in a `.gitmodules` body, read with
/// git-config syntax: section and key names are case-insensitive (`Path =`),
/// values may be double-quoted (`"vendor/fleet bus"`), `;` and `#` start a
/// comment outside quotes, `\` escapes a quote, a backslash or a newline
/// (continuation), and surrounding whitespace is dropped while inner
/// whitespace is kept.
///
/// A declaration this reader misses lets the parent absorb a vanished
/// submodule — the fail-open direction — so malformed input is read
/// leniently, never discarded: an unterminated quote runs to the end of the
/// line and an unknown escape keeps its character.
fn gitmodules_paths(raw: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut in_submodule = false;
    let mut chars = raw.chars().peekable();
    loop {
        while chars.next_if(|c| *c != '\n' && c.is_whitespace()).is_some() {}
        match chars.peek().copied() {
            None => break,
            Some('\n') => {
                chars.next();
            }
            Some('#' | ';') => skip_config_line(&mut chars),
            Some('[') => {
                chars.next();
                let mut header = String::new();
                while let Some(c) = chars.next_if(|c| *c != ']' && *c != '\n') {
                    header.push(c);
                }
                if chars.next_if_eq(&']').is_none() {
                    in_submodule = false;
                    continue;
                }
                // `[submodule "name"]`, or the legacy `[submodule.name]`.
                let section = header
                    .trim_start()
                    .split(|c: char| c.is_whitespace() || c == '"' || c == '.')
                    .next()
                    .unwrap_or_default();
                in_submodule = section.eq_ignore_ascii_case("submodule");
                // A key may follow the header on the same line.
            }
            Some(_) => {
                let mut key = String::new();
                while let Some(c) = chars.next_if(|c| c.is_ascii_alphanumeric() || *c == '-') {
                    key.push(c);
                }
                while chars.next_if(|c| *c == ' ' || *c == '\t').is_some() {}
                if key.is_empty() || chars.next_if_eq(&'=').is_none() {
                    // A bare boolean key, or noise: nothing to read.
                    skip_config_line(&mut chars);
                    continue;
                }
                let value = config_value(&mut chars);
                if in_submodule && key.eq_ignore_ascii_case("path") {
                    paths.push(value);
                }
            }
        }
    }
    paths
}

fn skip_config_line(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for c in chars.by_ref() {
        if c == '\n' {
            break;
        }
    }
}

/// One git-config value, consuming through the end of its (possibly
/// continued) line. Mirrors git's `parse_value`: leading whitespace skipped,
/// trailing unquoted whitespace trimmed, inner whitespace verbatim.
fn config_value(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut value = String::new();
    let mut quoted = false;
    let mut comment = false;
    // Length of `value` before a run of unquoted whitespace, so a run that
    // turns out to be trailing can be cut off.
    let mut trim_len: Option<usize> = None;
    while let Some(c) = chars.next() {
        if c == '\n' {
            break;
        }
        if comment {
            continue;
        }
        if c.is_whitespace() && !quoted {
            if trim_len.is_none() {
                trim_len = Some(value.len());
            }
            if !value.is_empty() {
                value.push(c);
            }
            continue;
        }
        if !quoted && (c == ';' || c == '#') {
            comment = true;
            continue;
        }
        trim_len = None;
        match c {
            '\\' => match chars.next() {
                Some('\n') => {}
                Some('\r') if chars.next_if_eq(&'\n').is_some() => {}
                Some('n') => value.push('\n'),
                Some('t') => value.push('\t'),
                Some('b') => value.push('\u{8}'),
                Some(other) => value.push(other),
                None => break,
            },
            '"' => quoted = !quoted,
            other => value.push(other),
        }
    }
    if let Some(len) = trim_len {
        value.truncate(len);
    }
    value
}

/// Do these two paths resolve to two DIFFERENT checkouts on this machine?
///
/// Only true when both sides are proven, so callers can distinguish "provably
/// another repository" from "cannot tell". A downstream project filter needs
/// that distinction: its path-segment fallback would otherwise re-admit a
/// nested checkout whose path happens to spell the requested project.
pub fn distinct_repo_identity(candidate: &str, scope_root: &str) -> bool {
    match (
        normalize_workdir(candidate, Some(scope_root)),
        normalize_workdir(scope_root, None),
    ) {
        (WorkdirIdentity::Resolved(left), WorkdirIdentity::Resolved(right)) => left != right,
        _ => false,
    }
}

/// Reduce a window's explicit workdir evidence to one effective-scope verdict.
///
/// Evidence naming the baseline checkout counts as an observed identity, not
/// as noise to drop: a window that ran tools in BOTH the baseline and another
/// checkout has two proven identities and must not be re-scoped wholesale to
/// the foreign one. A proven root never absorbs another proven root — a nested
/// checkout is a repository of its own. It DOES absorb a path that merely no
/// longer exists inside it, unless `.gitmodules` still declares that path a
/// submodule: see [`plausibly_within_scope`] for why reduction fails open
/// where membership fails closed.
///
/// An unresolvable workdir pointing away from the baseline (historical or
/// foreign-machine path), and evidence that could not be read at all
/// ([`WorkdirEvidence::Opaque`]), are [`WindowScope::Unattributed`]: a durable
/// "evidence exists but says nothing" state — never a positive attribution,
/// never proof of divergence, and never eligible for bucket inheritance
/// downstream.
pub fn effective_window_scope(
    workdirs: &[WorkdirEvidence],
    baseline: Option<&str>,
) -> (WindowScope, Option<String>) {
    let baseline = baseline.map(str::trim).filter(|value| !value.is_empty());
    // Unreadable evidence can never be proven to agree with anything else in
    // the window, so it decides the verdict on its own.
    if workdirs.contains(&WorkdirEvidence::Opaque) {
        return (WindowScope::Unattributed, None);
    }

    let mut saw_baseline = false;
    let mut foreign: Vec<WorkdirIdentity> = Vec::new();
    for raw in workdirs.iter().filter_map(WorkdirEvidence::path) {
        if baseline.is_some_and(|base| plausibly_within_scope(raw, base)) {
            saw_baseline = true;
            continue;
        }
        let identity = normalize_workdir(raw, baseline);
        if !foreign.contains(&identity) {
            foreign.push(identity);
        }
    }

    // A proven foreign root absorbs unprovable paths that plausibly sit inside
    // it, for the same reason the baseline does: a vanished directory is not
    // evidence of a second repository. It never absorbs another proven root.
    let roots: Vec<String> = foreign
        .iter()
        .filter_map(|identity| match identity {
            WorkdirIdentity::Resolved(root) => Some(root.to_string_lossy().into_owned()),
            WorkdirIdentity::Unresolved(_) => None,
        })
        .collect();
    foreign.retain(|identity| match identity {
        WorkdirIdentity::Resolved(_) => true,
        WorkdirIdentity::Unresolved(path) => {
            !roots.iter().any(|root| plausibly_within_scope(path, root))
        }
    });

    let resolved: Vec<&WorkdirIdentity> = foreign
        .iter()
        .filter(|identity| identity.is_resolved())
        .collect();
    if resolved.len() >= 2 {
        return (WindowScope::Conflict, None);
    }
    if resolved.len() == 1 && saw_baseline {
        // The window ran in the baseline AND in another checkout. Proven
        // divergence when the baseline itself is a checkout here; otherwise
        // the second identity is unproven, which is "unknown", not a verdict.
        let baseline_resolved = baseline
            .map(|base| normalize_workdir(base, None).is_resolved())
            .unwrap_or(false);
        return if baseline_resolved {
            (WindowScope::Conflict, None)
        } else {
            (WindowScope::Unattributed, None)
        };
    }
    if foreign.iter().any(|identity| !identity.is_resolved()) {
        return (WindowScope::Unattributed, None);
    }
    match foreign.first() {
        None => (WindowScope::Baseline, None),
        Some(identity) => (WindowScope::Consistent, Some(identity.scope_path())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn explicit(paths: &[&str]) -> Vec<WorkdirEvidence> {
        paths
            .iter()
            .map(|path| WorkdirEvidence::Explicit((*path).to_string()))
            .collect()
    }

    /// Identities are canonical, so a scratch root under a symlinked temp dir
    /// compares equal to what `normalize_workdir` returns.
    fn canonical(path: &Path) -> String {
        std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned()
    }

    fn scratch(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "aicx-scope-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn first_workdir(payload: &Value) -> Option<String> {
        tool_call_workdirs(payload)
            .into_iter()
            .next()
            .and_then(|evidence| evidence.path().map(str::to_owned))
    }

    fn js_input(input: &str) -> Value {
        serde_json::json!({"type": "custom_tool_call", "name": "exec", "input": input})
    }

    #[test]
    fn workdir_from_json_arguments_string() {
        let payload: Value = serde_json::from_str(
            r#"{"type":"function_call","name":"shell","arguments":"{\"cmd\":\"cargo test\",\"workdir\":\"/repo/a\"}"}"#,
        )
        .expect("fixture payload");
        assert_eq!(first_workdir(&payload).as_deref(), Some("/repo/a"));
    }

    /// Finding: arguments that parsed as a JSON object with no top-level
    /// `workdir` were scanned again as JavaScript, so a nested
    /// `options.workdir` — data the call carried — became the directory it ran
    /// in and re-scoped a baseline call to that checkout. A parsed object is
    /// read structurally; only arguments that do not parse are scanned.
    #[test]
    fn only_the_top_level_workdir_of_json_arguments_is_read() {
        let arguments = |raw: &str| serde_json::json!({"type": "function_call", "name": "shell", "arguments": raw});
        for raw in [
            r#"{"cmd":"pwd","options":{"workdir":"/repo/foreign"}}"#,
            r#"{"cmd":"pwd","workdir":null,"env":[{"workdir":"/repo/foreign"}]}"#,
            // Parsed, but not an object: there is no top-level `workdir` at
            // all, and the JavaScript scan must not find one inside it.
            r#"[{"workdir":"/repo/foreign"}]"#,
            r#""{\"workdir\":\"/repo/foreign\"}""#,
        ] {
            assert!(tool_call_workdirs(&arguments(raw)).is_empty(), "{raw}");
        }
        // The same shape as an object value, not a string, is read the same.
        let object = serde_json::json!({
            "type": "function_call",
            "name": "shell",
            "arguments": {"cmd": "pwd", "options": {"workdir": "/repo/foreign"}},
        });
        assert!(tool_call_workdirs(&object).is_empty());
        // The top-level key still decides, and a value that is no path and
        // no "default" is a directory we cannot see.
        assert_eq!(
            tool_call_workdirs(&arguments(
                r#"{"options":{"workdir":"/repo/foreign"},"workdir":"/repo/a"}"#
            )),
            explicit(&["/repo/a"])
        );
        assert_eq!(
            tool_call_workdirs(&arguments(r#"{"cmd":"pwd","workdir":["/repo/a"]}"#)),
            vec![WorkdirEvidence::Opaque]
        );
        // Arguments that do not parse are still scanned as JavaScript.
        assert_eq!(
            tool_call_workdirs(&arguments(r#"{cmd: "pwd", workdir: '/repo/b'}"#)),
            explicit(&["/repo/b"])
        );
    }

    #[test]
    fn workdir_from_js_literal_input() {
        let payload: Value = serde_json::from_str(
            r#"{"type":"custom_tool_call","name":"exec","input":"const r = await tools.exec_command({cmd:\"npm test\",\"workdir\":\"/repo/b\",\"yield_time_ms\":30000});"}"#,
        )
        .expect("fixture payload");
        assert_eq!(first_workdir(&payload).as_deref(), Some("/repo/b"));
    }

    #[test]
    fn workdir_from_single_quoted_js_literal() {
        // Codex writes `custom_tool_call.input` as model-authored JavaScript:
        // the quote style is whatever the model emitted. A double-quote-only
        // reader silently dropped this evidence and kept the baseline project.
        let payload: Value = serde_json::from_str(
            r#"{"type":"custom_tool_call","name":"exec","input":"const r = await tools.exec_command({cmd: 'npm test', workdir: '/foreign/repo'});"}"#,
        )
        .expect("fixture payload");
        assert_eq!(first_workdir(&payload).as_deref(), Some("/foreign/repo"));

        let mixed: Value = serde_json::from_str(
            r#"{"type":"custom_tool_call","name":"exec","input":"tools.exec_command({'workdir': \"/foreign/other\"})"}"#,
        )
        .expect("fixture payload");
        assert_eq!(first_workdir(&mixed).as_deref(), Some("/foreign/other"));
    }

    /// A Windows workdir is `C:\repo\crate`. A capture that stops at the first
    /// backslash reduces it to `C:` — no evidence, and the window silently
    /// keeps a baseline it never verified.
    #[test]
    fn workdir_with_backslashes_survives_the_literal_scanner() {
        let payload: Value = serde_json::json!({
            "type": "custom_tool_call",
            "name": "exec",
            "input": r#"tools.exec_command({cmd:"cargo test",workdir:"C:\Users\runner\fleet-bus"})"#,
        });
        assert_eq!(
            first_workdir(&payload).as_deref(),
            Some(r"C:\Users\runner\fleet-bus")
        );
    }

    /// One `input` can orchestrate several `exec_command` calls. Reading only
    /// the first hides a later hop into another repository behind a
    /// baseline-looking opener.
    #[test]
    fn every_workdir_in_an_orchestrated_tool_call_is_collected() {
        let payload: Value = serde_json::json!({
            "type": "custom_tool_call",
            "name": "exec",
            "input": "await tools.exec_command({cmd:\"ls\",workdir:\"/repo/vista\"});\n\
                      await tools.exec_command({cmd:\"ls\",workdir:'/repo/fleet-bus'});",
        });
        assert_eq!(
            tool_call_workdirs(&payload),
            explicit(&["/repo/vista", "/repo/fleet-bus"]),
            "a later hop must not hide behind the first call"
        );
    }

    /// Finding: `tools.exec_command({cmd, ...opts})` gave no evidence, so a
    /// call whose runtime-built options carried another checkout's `workdir`
    /// left the window on its baseline. An argument not all written at the
    /// call names a directory only the runtime knew.
    #[test]
    fn an_exec_argument_not_written_at_the_call_is_opaque() {
        for input in [
            r#"await tools.exec_command({cmd: "make", ...opts})"#,
            r#"tools.exec_command({...opts, cmd: "make"})"#,
            "tools.exec_command(args)",
            "for (const a of plan) await tools.exec_command(a);",
            "tools.exec_command(...calls)",
            r#"tools.exec_command(Object.assign({cmd: "make"}, opts))"#,
            // The argument object was cut off.
            r#"tools.exec_command({cmd: "make""#,
        ] {
            assert_eq!(
                tool_call_workdirs(&js_input(input)),
                vec![WorkdirEvidence::Opaque],
                "{input}"
            );
        }
        // Everything the call takes is written at it.
        for input in [
            r#"tools.exec_command({cmd: "ls", env: {...process.env, CI: "1"}})"#,
            r#"tools.exec_command({cmd: "ls", args: [...files]})"#,
            r#"tools.exec_command({cmd: "echo ...done", note: "}"})"#,
            r#"tools.exec_command("ls")"#,
            "tools.exec_command()",
            // Other tools take variables as a matter of course.
            "tools.apply_patch(patch); tools.write_stdin(input)",
            r#"console.log("tools.exec_command(args)")"#,
            "// tools.exec_command(args)\ntools.exec_command({cmd: \"ls\"})",
        ] {
            assert!(tool_call_workdirs(&js_input(input)).is_empty(), "{input}");
        }
        // A spread after a written `workdir` may override it.
        assert_eq!(
            tool_call_workdirs(&js_input(
                r#"tools.exec_command({cmd: "ls", workdir: "/repo/a", ...opts})"#
            )),
            [explicit(&["/repo/a"]), vec![WorkdirEvidence::Opaque]].concat()
        );
        // So may whatever follows where the argument was cut off.
        assert_eq!(
            tool_call_workdirs(&js_input(
                "tools.exec_command({workdir: \"/repo/a\" // last line"
            )),
            [explicit(&["/repo/a"]), vec![WorkdirEvidence::Opaque]].concat()
        );
    }

    /// Finding: the value capture stopped at the first quote of EITHER kind,
    /// whatever opened it, so a legal apostrophe cut the path short and the
    /// fabricated prefix decided the window.
    #[test]
    fn a_workdir_value_runs_to_the_quote_that_opened_it() {
        assert_eq!(
            tool_call_workdirs(&js_input(
                r#"tools.exec_command({cmd:"ls",workdir:"/Users/O'Brien/repo"})"#
            )),
            explicit(&["/Users/O'Brien/repo"])
        );
        assert_eq!(
            tool_call_workdirs(&js_input(
                r#"tools.exec_command({workdir:'/tmp/say "hi"/x'})"#
            )),
            explicit(&[r#"/tmp/say "hi"/x"#])
        );
        // An escaped delimiter and an escaped backslash are part of the value;
        // any other backslash is a Windows separator and stays.
        assert_eq!(
            tool_call_workdirs(&js_input(
                r"tools.exec_command({workdir:'/Users/O\'Brien/repo'})"
            )),
            explicit(&["/Users/O'Brien/repo"])
        );
        assert_eq!(
            tool_call_workdirs(&js_input(
                r#"tools.exec_command({workdir:"C:\\Users\\dev\\repo"})"#
            )),
            explicit(&[r"C:\Users\dev\repo"])
        );
        // A literal that never closes was cut off: its path is unknowable.
        assert_eq!(
            tool_call_workdirs(&js_input(r#"tools.exec_command({workdir:"/foreign/re"#)),
            vec![WorkdirEvidence::Opaque]
        );
    }

    /// Finding: a template literal is a string literal in JavaScript, and the
    /// scanner did not recognise one, so `` workdir: `/foreign/repo` `` left a
    /// foreign call on its baseline project.
    #[test]
    fn a_template_literal_workdir_is_read_and_an_interpolated_one_is_opaque() {
        assert_eq!(
            tool_call_workdirs(&js_input(
                "tools.exec_command({cmd:'ls', workdir: `/foreign/repo`})"
            )),
            explicit(&["/foreign/repo"])
        );
        // Interpolation names a directory only the runtime knew. It is
        // evidence we cannot read, never the literal text `${root}/pkg`.
        assert_eq!(
            tool_call_workdirs(&js_input(
                "tools.exec_command({cmd:'ls', workdir: `${root}/pkg`})"
            )),
            vec![WorkdirEvidence::Opaque]
        );
        let (scope, _) = effective_window_scope(
            &tool_call_workdirs(&js_input("tools.exec_command({workdir: `${root}/pkg`})")),
            Some("/present/vista"),
        );
        assert_eq!(scope, WindowScope::Unattributed);
    }

    #[test]
    fn no_workdir_anywhere_is_none() {
        let payload: Value = serde_json::from_str(
            r#"{"type":"custom_tool_call","name":"exec","input":"const r = await tools.exec_command({cmd:\"pwd\"});"}"#,
        )
        .expect("fixture payload");
        assert!(tool_call_workdirs(&payload).is_empty());
    }

    #[test]
    fn same_repo_subdirs_are_one_scope() {
        let root = scratch("same-repo");
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git dir");
        std::fs::create_dir_all(repo.join("packages/a")).expect("pkg a");
        std::fs::create_dir_all(repo.join("packages/b")).expect("pkg b");
        let workdirs = explicit(&[
            repo.join("packages/a").to_string_lossy().as_ref(),
            repo.join("packages/b").to_string_lossy().as_ref(),
        ]);
        let (scope, path) = effective_window_scope(&workdirs, None);
        assert_eq!(scope, WindowScope::Consistent);
        assert_eq!(path.as_deref(), Some(canonical(&repo).as_str()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn two_real_repo_roots_are_a_conflict() {
        let root = scratch("two-repos");
        let repo_a = root.join("repo-a");
        let repo_b = root.join("repo-b");
        std::fs::create_dir_all(repo_a.join(".git")).expect("git dir a");
        std::fs::create_dir_all(repo_b.join(".git")).expect("git dir b");
        let workdirs = explicit(&[
            repo_a.to_string_lossy().as_ref(),
            repo_b.to_string_lossy().as_ref(),
        ]);
        let (scope, path) = effective_window_scope(&workdirs, None);
        assert_eq!(scope, WindowScope::Conflict);
        assert_eq!(path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn nested_checkout_under_the_baseline_is_not_baseline_scope() {
        // Submodule / vendored checkout: lexically below the parent, its own
        // repository. A path-prefix baseline test swallowed it and kept the
        // window in the parent's project bucket.
        let root = scratch("nested-checkout");
        let parent = root.join("vista");
        let nested = parent.join("vendor/fleet-bus");
        std::fs::create_dir_all(parent.join(".git")).expect("parent git dir");
        std::fs::create_dir_all(nested.join(".git")).expect("nested git dir");
        std::fs::create_dir_all(parent.join("crates/core")).expect("parent subdir");
        let baseline = parent.to_string_lossy().into_owned();

        assert!(!workdir_within_scope(
            nested.to_string_lossy().as_ref(),
            &baseline
        ));
        assert!(workdir_within_scope(
            parent.join("crates/core").to_string_lossy().as_ref(),
            &baseline
        ));
        assert!(distinct_repo_identity(
            nested.to_string_lossy().as_ref(),
            &baseline
        ));

        let (scope, path) = effective_window_scope(
            &explicit(&[nested.to_string_lossy().as_ref()]),
            Some(&baseline),
        );
        assert_eq!(
            scope,
            WindowScope::Consistent,
            "the nested checkout is its own repo identity, not the parent's"
        );
        assert_eq!(path.as_deref(), Some(canonical(&nested).as_str()));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Two proven roots in one window are a conflict even when one of them is
    /// the enclosing checkout: a resolved root never absorbs another resolved
    /// root by lexical containment.
    #[test]
    fn parent_and_nested_checkout_together_are_a_conflict() {
        let root = scratch("parent-plus-nested");
        let parent = root.join("vista");
        let nested = parent.join("vendor/fleet-bus");
        std::fs::create_dir_all(parent.join(".git")).expect("parent git dir");
        std::fs::create_dir_all(nested.join(".git")).expect("nested git dir");

        // No baseline: both are ordinary evidence.
        let (scope, path) = effective_window_scope(
            &explicit(&[
                parent.to_string_lossy().as_ref(),
                nested.to_string_lossy().as_ref(),
            ]),
            None,
        );
        assert_eq!(scope, WindowScope::Conflict, "two proven roots, one window");
        assert_eq!(path, None);

        // With the parent as the declared baseline, the window still ran in
        // two checkouts and must not be re-scoped wholesale to either.
        let (scoped, scoped_path) = effective_window_scope(
            &explicit(&[
                parent.to_string_lossy().as_ref(),
                nested.to_string_lossy().as_ref(),
            ]),
            Some(parent.to_string_lossy().as_ref()),
        );
        assert_eq!(scoped, WindowScope::Conflict);
        assert_eq!(scoped_path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An explicit baseline workdir is observed evidence, not noise: dropping
    /// it before reduction let one foreign root re-scope the whole window,
    /// including the turns that really did run in the baseline checkout.
    #[test]
    fn baseline_plus_foreign_checkout_is_a_conflict_not_a_rescope() {
        let root = scratch("baseline-plus-foreign");
        let vista = root.join("vista");
        let fleet = root.join("fleet-bus");
        std::fs::create_dir_all(vista.join(".git")).expect("vista git dir");
        std::fs::create_dir_all(fleet.join(".git")).expect("fleet git dir");

        let (scope, path) = effective_window_scope(
            &explicit(&[
                vista.to_string_lossy().as_ref(),
                fleet.to_string_lossy().as_ref(),
            ]),
            Some(vista.to_string_lossy().as_ref()),
        );
        assert_eq!(scope, WindowScope::Conflict);
        assert_eq!(path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A relative workdir belongs to the turn's cwd. Resolving it against the
    /// process cwd would let whichever checkout `aicx` runs from adopt a
    /// historical rollout's messages.
    #[test]
    fn relative_workdirs_resolve_against_the_baseline_not_the_process() {
        let root = scratch("relative-workdir");
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git dir");
        std::fs::create_dir_all(repo.join("packages/api")).expect("pkg");
        let baseline = repo.to_string_lossy().into_owned();

        assert_eq!(
            normalize_workdir("packages/api", Some(&baseline)),
            WorkdirIdentity::Resolved(
                std::fs::canonicalize(&repo).unwrap_or_else(|_| repo.clone())
            )
        );
        // Without a baseline there is nothing to resolve against — never the
        // process cwd.
        assert_eq!(
            normalize_workdir("packages/api", None),
            WorkdirIdentity::Unresolved("packages/api".to_string())
        );
        let (scope, _) = effective_window_scope(&explicit(&["packages/api"]), Some(&baseline));
        assert_eq!(scope, WindowScope::Baseline);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An ancestor's `.git` says nothing about a directory that is not there.
    /// Treating `/repo/deleted-subdir` as `/repo` stamps positive scope on a
    /// workdir the contract calls unattributable.
    #[test]
    fn missing_path_under_a_real_checkout_stays_unresolved() {
        let root = scratch("missing-under-repo");
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git dir");
        let missing = repo.join("deleted-fleet");

        assert_eq!(
            normalize_workdir(missing.to_string_lossy().as_ref(), None),
            WorkdirIdentity::Unresolved(missing.to_string_lossy().into_owned())
        );
        let (scope, path) =
            effective_window_scope(&explicit(&[missing.to_string_lossy().as_ref()]), None);
        assert_eq!(scope, WindowScope::Unattributed);
        assert_eq!(path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Membership and identity-counting are different questions, and this pins
    /// the split deliberately rather than letting it fall out.
    ///
    /// A path under a live checkout that no longer exists cannot be PROVEN to
    /// belong to it, so the project filter must not serve it (fail closed).
    /// It equally cannot be proven to be a second repository, and the window
    /// reduction must not call it one (fail open) — `retain_frames_for_project`
    /// drops `scope_unattributed` frames outright, so counting every deleted
    /// `target/` or cleaned-up worktree as a foreign repo would silently
    /// delete ordinary operator evidence.
    #[test]
    fn a_vanished_path_fails_closed_for_membership_and_open_for_identity() {
        let root = scratch("vanished-under-live");
        let parent = root.join("vista");
        std::fs::create_dir_all(parent.join(".git")).expect("parent git dir");
        let base = parent.to_string_lossy().into_owned();
        let gone = parent.join("target").join("tmp-build");
        let gone = gone.to_string_lossy().into_owned();

        assert!(
            !workdir_within_scope(&gone, &base),
            "membership: a proven checkout must not claim a path it cannot prove"
        );
        let (scope, path) = effective_window_scope(&explicit(&[&gone]), Some(&base));
        assert_eq!(
            scope,
            WindowScope::Baseline,
            "identity: a vanished directory is not a second repository"
        );
        assert_eq!(path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The one vanished path that IS provably its own repository: the parent
    /// still declares it in `.gitmodules` after the working tree is gone. A
    /// bare nested checkout leaves no such trace and is absorbed — that half
    /// of the trade is stated in `plausibly_within_scope`, not hidden.
    #[test]
    fn a_vanished_declared_submodule_is_still_its_own_repository() {
        let root = scratch("vanished-submodule");
        let parent = root.join("vista");
        std::fs::create_dir_all(parent.join(".git")).expect("parent git dir");
        std::fs::write(
            parent.join(".gitmodules"),
            "[submodule \"fleet-bus\"]\n\tpath = vendor/fleet-bus\n\turl = https://example.invalid/fleet-bus.git\n",
        )
        .expect("write .gitmodules");
        let base = parent.to_string_lossy().into_owned();
        let gone = parent.join("vendor").join("fleet-bus");
        let gone = gone.to_string_lossy().into_owned();

        assert!(!workdir_within_scope(&gone, &base));
        let (scope, path) = effective_window_scope(&explicit(&[&gone]), Some(&base));
        assert_eq!(
            scope,
            WindowScope::Unattributed,
            "a declared submodule keeps its identity once the tree is gone"
        );
        assert_eq!(path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Counting a session's repositories is identity reduction, not
    /// membership: it fails open on paths that prove nothing, keeps a proven
    /// nested checkout and a declared submodule apart, and does not depend on
    /// the order the paths were recorded in.
    #[test]
    fn a_session_counts_repositories_not_recorded_paths() {
        let root = scratch("repository-count");
        let parent = root.join("vista");
        let nested = parent.join("tools").join("nested");
        std::fs::create_dir_all(parent.join(".git")).expect("parent git dir");
        std::fs::create_dir_all(parent.join("pkg")).expect("subdirectory");
        std::fs::create_dir_all(nested.join(".git")).expect("nested git dir");
        std::fs::write(
            parent.join(".gitmodules"),
            "[submodule \"fleet-bus\"]\n\tpath = vendor/fleet-bus\n\turl = https://example.invalid/fleet-bus.git\n",
        )
        .expect("write .gitmodules");
        let path = |p: PathBuf| p.to_string_lossy().into_owned();
        let top = path(parent.clone());
        let pkg = path(parent.join("pkg"));
        let gone_a = path(parent.join("target").join("tmp-build"));
        let gone_b = path(parent.join("worktrees").join("removed"));
        let submodule = path(parent.join("vendor").join("fleet-bus"));
        let nested = path(nested);

        assert_eq!(repository_count([top.as_str(), pkg.as_str()]), 1);
        assert_eq!(repository_count([top.as_str(), gone_a.as_str()]), 1);
        assert_eq!(
            repository_count([gone_a.as_str(), gone_b.as_str(), top.as_str()]),
            1,
            "vanished paths recorded before their checkout are still inside it"
        );
        assert_eq!(
            repository_count([pkg.as_str(), gone_a.as_str()]),
            1,
            "a subdirectory's checkout root absorbs a vanished sibling"
        );
        assert_eq!(repository_count([top.as_str(), nested.as_str()]), 2);
        assert_eq!(
            repository_count([top.as_str(), submodule.as_str()]),
            2,
            "a declared submodule keeps its identity once the tree is gone"
        );
        assert_eq!(
            repository_count(["/aicx-scope-nowhere/repo/pkg", "/aicx-scope-nowhere/repo"]),
            1
        );
        assert_eq!(
            repository_count(["/aicx-scope-nowhere/repo/pkg", "/aicx-scope-nowhere/other"]),
            2
        );
        assert_eq!(repository_count(["", "  "]), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The baseline is a working directory, not the checkout root. Reading
    /// `.gitmodules` where the session happened to stand misses the
    /// declaration entirely and absorbs the vanished submodule into its
    /// parent — the exact leak the declaration exists to prevent.
    #[test]
    fn a_declared_submodule_is_found_from_a_subdirectory_baseline() {
        let root = scratch("submodule-subdir-baseline");
        let parent = root.join("vista");
        std::fs::create_dir_all(parent.join(".git")).expect("parent git dir");
        std::fs::create_dir_all(parent.join("packages").join("api")).expect("baseline dir");
        std::fs::write(
            parent.join(".gitmodules"),
            "[submodule \"fleet-bus\"]\n\tpath = packages/api/vendor/fleet-bus\n\turl = https://example.invalid/fleet-bus.git\n",
        )
        .expect("write .gitmodules");

        // The session stood in a subdirectory; the submodule tree is gone.
        let base = parent.join("packages").join("api");
        let base = base.to_string_lossy().into_owned();
        let gone = parent
            .join("packages")
            .join("api")
            .join("vendor")
            .join("fleet-bus");
        let gone = gone.to_string_lossy().into_owned();

        let (scope, path) = effective_window_scope(&explicit(&[&gone]), Some(&base));
        assert_eq!(
            scope,
            WindowScope::Unattributed,
            "a declared submodule keeps its identity even when the baseline is a subdirectory"
        );
        assert_eq!(path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Finding: a submodule's DESCENDANTS belong to the submodule, not to the
    /// parent that declares it. Exact `.gitmodules` matching absorbed a
    /// vanished `vendor/fleet-bus/src` into the parent checkout and let the
    /// window keep the parent's positive attribution.
    #[test]
    fn a_vanished_path_inside_a_declared_submodule_is_not_the_parent() {
        let root = scratch("submodule-descendant");
        let parent = root.join("vista");
        std::fs::create_dir_all(parent.join(".git")).expect("parent git dir");
        std::fs::write(
            parent.join(".gitmodules"),
            "[submodule \"fleet-bus\"]\n\tpath = vendor/fleet-bus\n\turl = https://example.invalid/fleet-bus.git\n",
        )
        .expect("write .gitmodules");
        let base = parent.to_string_lossy().into_owned();

        let inside = parent.join("vendor").join("fleet-bus").join("src");
        let (scope, path) =
            effective_window_scope(&explicit(&[&inside.to_string_lossy()]), Some(&base));
        assert_eq!(
            scope,
            WindowScope::Unattributed,
            "a path under a declared submodule must not inherit the parent checkout"
        );
        assert_eq!(path, None);

        // The boundary is a path COMPONENT: a sibling that merely starts with
        // the declared spelling is an ordinary vanished directory.
        let sibling = parent.join("vendor").join("fleet-bus-old");
        let (scope, _) =
            effective_window_scope(&explicit(&[&sibling.to_string_lossy()]), Some(&base));
        assert_eq!(
            scope,
            WindowScope::Baseline,
            "`fleet-bus-old` is not inside the declared `fleet-bus`"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Finding (P1-06): `.gitmodules` is git-config, not `path = x` lines. A
    /// reader that matched only a lowercase unquoted `path` let the parent
    /// absorb every submodule declared as `Path =` or with a quoted value.
    #[test]
    fn gitmodules_are_read_with_git_config_syntax() {
        let raw = concat!(
            "# vendored checkouts\n",
            "[Submodule \"fleet-bus\"]\n",
            "\tPath = \"vendor/fleet-bus\" ; moved in 2025\n",
            "\turl = https://example.invalid/fleet-bus.git\n",
            "[submodule \"spaced\"]\n",
            "\tpath=\"vendor/fleet bus\"\n",
            "[submodule \"commented\"]\n",
            "\tPATH = vendor/plain   # trailing comment\n",
            "[submodule \"hash\"]\n",
            "\tpath = \"vendor/with#hash\"\r\n",
            "[submodule \"continued\"]\n",
            "\tpath = vendor/cont\\\n",
            "inued\n",
            "[submodule \"escaped\"]\n",
            "\tpath = \"vendor/q\\\"uote\"\n",
            "[remote \"origin\"]\n",
            "\tpath = not/a/submodule\n",
            "[submodule.legacy] path = vendor/legacy\n",
            "[submodule \"broken\"]\n",
            "\tpath = \"vendor/unterminated\n",
        );
        assert_eq!(
            gitmodules_paths(raw),
            vec![
                "vendor/fleet-bus",
                "vendor/fleet bus",
                "vendor/plain",
                "vendor/with#hash",
                "vendor/continued",
                "vendor/q\"uote",
                "vendor/legacy",
                "vendor/unterminated",
            ]
        );
    }

    /// The same finding through the verdict: a submodule declared with a
    /// capitalised key and a quoted, spaced value keeps its identity once its
    /// tree is gone, and the component boundary still holds.
    #[test]
    fn a_quoted_submodule_declaration_is_not_absorbed_by_its_parent() {
        let root = scratch("quoted-submodule");
        let parent = root.join("vista");
        std::fs::create_dir_all(parent.join(".git")).expect("parent git dir");
        std::fs::write(
            parent.join(".gitmodules"),
            "[submodule \"fleet bus\"]\n\tPath = \"vendor/fleet bus\" ; moved\n",
        )
        .expect("write .gitmodules");
        let base = parent.to_string_lossy().into_owned();

        let gone = parent.join("vendor").join("fleet bus").join("src");
        let (scope, path) =
            effective_window_scope(&explicit(&[&gone.to_string_lossy()]), Some(&base));
        assert_eq!(scope, WindowScope::Unattributed);
        assert_eq!(path, None);

        let sibling = parent.join("vendor").join("fleet bus-old");
        let (scope, _) =
            effective_window_scope(&explicit(&[&sibling.to_string_lossy()]), Some(&base));
        assert_eq!(scope, WindowScope::Baseline);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Finding (P1-01/02): the index cache keyed verdicts on source bytes, so
    /// a nested checkout appearing or a `.gitmodules` edit left stale verdicts
    /// servable. The layout evidence of a recorded path must move whenever a
    /// verdict about it can — including when the path is only a subdirectory
    /// of the checkout that declares the submodule — and must hold still when
    /// nothing the verdicts read has changed.
    #[test]
    fn layout_evidence_moves_exactly_when_a_verdict_can() {
        let root = scratch("layout-evidence");
        let parent = root.join("vista");
        let app = parent.join("app");
        let nested = app.join("tools").join("fleet-bus");
        std::fs::create_dir_all(parent.join(".git")).expect("parent git dir");
        std::fs::create_dir_all(nested.join("src")).expect("nested dir");
        let base = app.to_string_lossy().into_owned();
        let inside = nested.join("src").to_string_lossy().into_owned();
        let vanished = app
            .join("vendor")
            .join("fleet")
            .to_string_lossy()
            .into_owned();
        let verdict = |workdir: &str| effective_window_scope(&explicit(&[workdir]), Some(&base)).0;

        let before = (scope_layout_evidence(&base), scope_layout_evidence(&inside));
        assert_eq!(
            (scope_layout_evidence(&base), scope_layout_evidence(&inside)),
            before,
            "nothing moved, so neither may the evidence"
        );
        assert_eq!(verdict(&inside), WindowScope::Baseline);
        assert_eq!(verdict(&vanished), WindowScope::Baseline);

        std::fs::create_dir_all(nested.join(".git")).expect("nested git dir");
        assert_eq!(verdict(&inside), WindowScope::Consistent);
        assert_ne!(scope_layout_evidence(&inside), before.1);
        assert_eq!(
            scope_layout_evidence(&base),
            before.0,
            "the baseline's own layout did not move"
        );

        std::fs::write(
            parent.join(".gitmodules"),
            "[submodule \"fleet\"]\n\tpath = app/vendor/fleet\n",
        )
        .expect("write .gitmodules");
        assert_eq!(verdict(&vanished), WindowScope::Unattributed);
        assert_ne!(scope_layout_evidence(&base), before.0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Finding: the declaration was matched byte for byte while containment
    /// folds a Windows spelling. `C:\Repo\vendor\fleet` against
    /// `path = Vendor/Fleet` in `C:\repo` is one directory to Windows and was
    /// already inside the checkout, so the missed declaration let the parent
    /// absorb its vanished submodule. The match is pure string work and is
    /// checked here on every host.
    #[test]
    fn a_windows_submodule_declaration_matches_in_any_case() {
        let raw = "[submodule \"fleet\"]\n\tpath = Vendor/Fleet\n";
        for path in [
            r"C:\Repo\vendor\fleet",
            r"c:\REPO\VENDOR\FLEET\src",
            r"C:\repo\Vendor\Fleet\",
            "C:/repo/vendor/fleet",
        ] {
            assert!(declares_submodule(raw, r"C:\repo", path), "{path}");
            assert!(declares_submodule(raw, "C:/Repo", path), "{path}");
        }
        for path in [
            r"C:\repo\vendor\fleet-old",
            r"C:\repository\vendor\fleet",
            r"C:\repo",
            r"D:\repo\vendor\fleet",
        ] {
            assert!(!declares_submodule(raw, r"C:\repo", path), "{path}");
        }
        // A Unix spelling keeps its case: there `Vendor/Fleet` and
        // `vendor/fleet` are two directories.
        assert!(declares_submodule(raw, "/repo", "/repo/Vendor/Fleet/src"));
        assert!(!declares_submodule(raw, "/repo", "/repo/vendor/fleet"));
        assert!(!declares_submodule(raw, "/repo", "/repo-old/Vendor/Fleet"));
    }

    /// Finding (P2-01): absoluteness was the HOST's question. A Windows
    /// rollout replayed on Unix (or the reverse) had its baseline called
    /// relative, so every relative workdir became unresolvable and the whole
    /// window unattributed. The recorded spelling decides now, on every host.
    #[test]
    fn a_foreign_platform_baseline_still_anchors_relative_workdirs() {
        let windows = r"C:\Users\dev\repo";
        assert_eq!(recorded_workdir(".", Some(windows)), windows);
        assert_eq!(
            recorded_workdir(r"packages\api", Some(windows)),
            r"C:\Users\dev\repo\packages\api"
        );
        assert_eq!(
            recorded_workdir("packages/api", Some(windows)),
            r"C:\Users\dev\repo\packages\api"
        );
        assert_eq!(
            recorded_workdir(r"..\other", Some(windows)),
            r"C:\Users\dev\other"
        );
        assert_eq!(
            recorded_workdir(r"\\server\share\repo", Some(windows)),
            r"\\server\share\repo"
        );
        let (scope, path) = effective_window_scope(
            &explicit(&[".", r"packages\api", r"C:\Users\dev\repo\src"]),
            Some(windows),
        );
        assert_eq!(scope, WindowScope::Baseline);
        assert_eq!(path, None);
        let (scope, _) = effective_window_scope(&explicit(&[r"..\other"]), Some(windows));
        assert_eq!(
            scope,
            WindowScope::Unattributed,
            "leaving the baseline is still foreign evidence"
        );

        // The Unix spelling reads the same on a Windows host.
        let unix = "/sessions/replayed";
        assert_eq!(recorded_workdir("./api/..", Some(unix)), unix);
        let (scope, _) = effective_window_scope(&explicit(&[".", "api"]), Some(unix));
        assert_eq!(scope, WindowScope::Baseline);

        // In a Unix spelling `\` is a filename character, not a separator.
        assert_eq!(
            recorded_workdir(r"odd\name", Some(unix)),
            r"/sessions/replayed/odd\name"
        );
        // A drive-relative token is not absolute anywhere.
        assert!(!absolute_anywhere("C:repo"));
    }

    /// Finding: a UNC path kept only its two leading separators as its root,
    /// so `..` popped the share name. `\\server\share\..\other` became
    /// `\\server\other`, another share, and a window whose baseline was that
    /// share read the call as its own. Windows anchors `..` at the share.
    #[test]
    fn a_unc_share_is_one_root() {
        for (path, normalized) in [
            (r"\\server\share\..\other", r"\\server\share\other"),
            (r"\\server\share\a\..\..\..\b", r"\\server\share\b"),
            (r"\\server\share\..", r"\\server\share"),
            (r"\\server/share\repo\.\pkg", r"\\server\share\repo\pkg"),
            (r"\\server\share\", r"\\server\share"),
            (r"\\server", r"\\server"),
        ] {
            assert_eq!(lexically_normalized(path), normalized, "{path}");
        }
        let share = r"\\server\share";
        assert_eq!(
            recorded_workdir(r"..\other", Some(share)),
            r"\\server\share\other"
        );
        // Containment is pure string work, so no host probes a share here.
        assert!(
            !lexically_within(r"\\server\share\..\other", r"\\server\other"),
            "the call stayed on `share`, not on the baseline share"
        );
        assert!(lexically_within(r"\\server\share\pkg\..\api", share));
    }

    /// Finding: `//server/share` is the same UNC root in the separator
    /// Windows also accepts, but it read as a Unix path, so `..` popped the
    /// share: `//server/share/../other` became `/server/other` and sat inside
    /// a `//server/other` baseline. Three or more leading `/` stay a Unix root.
    #[test]
    fn a_forward_slash_unc_share_is_one_root() {
        for (path, normalized) in [
            ("//server/share/../other", "//server/share/other"),
            ("//server/share/..", "//server/share"),
            ("//server/share/repo/./pkg", "//server/share/repo/pkg"),
            ("///server/share/../other", "/server/other"),
        ] {
            assert_eq!(lexically_normalized(path), normalized, "{path}");
        }
        assert_eq!(
            recorded_workdir("../other", Some("//server/share")),
            "//server/share/other"
        );
        // Containment is pure string work, so no host probes a share here.
        assert!(
            !lexically_within("//server/share/../other", "//server/other"),
            "the call stayed on `share`, not on the baseline share"
        );
        // One share in both separators and any case, as Windows resolves it.
        assert!(lexically_within(r"\\Server\Share\pkg", "//server/share"));
    }

    /// Finding: lexical containment kept each path's own separator and case,
    /// so one Windows checkout spelled `C:/…` by the baseline and `C:\…` or
    /// `c:\…` by a tool call was two strings with no common prefix. The window
    /// went unattributed and its intents were dropped, although Windows
    /// resolves every one of those spellings to the same directory.
    #[test]
    fn windows_spellings_of_one_checkout_are_one_scope() {
        let forward = "C:/aicx-scope-nowhere/dev/repo";
        let back = r"C:\aicx-scope-nowhere\dev\repo";
        for (candidate, scope) in [
            (r"C:\aicx-scope-nowhere\dev\repo\pkg", forward),
            ("C:/aicx-scope-nowhere/dev/repo/pkg", back),
            (r"c:\AICX-SCOPE-NOWHERE\Dev\Repo\pkg", back),
            (r"C:\aicx-scope-nowhere/dev\repo", forward),
        ] {
            assert!(
                workdir_within_scope(candidate, scope),
                "`{candidate}` is inside `{scope}` on Windows"
            );
        }
        let (scope, path) = effective_window_scope(
            &explicit(&[r"C:\aicx-scope-nowhere\dev\repo\pkg", "src"]),
            Some(forward),
        );
        assert_eq!(scope, WindowScope::Baseline);
        assert_eq!(path, None);

        // Folding never widens a scope past its own directory.
        assert!(!workdir_within_scope(
            r"C:\aicx-scope-nowhere\dev\repo-other",
            forward
        ));
        assert!(!workdir_within_scope(
            r"D:\aicx-scope-nowhere\dev\repo",
            back
        ));
        // A Unix spelling keeps its case and its `\` as a filename character.
        assert!(!workdir_within_scope(
            "/aicx-scope-nowhere/Dev/repo",
            "/aicx-scope-nowhere/dev/repo"
        ));
        assert!(!workdir_within_scope(
            r"/aicx-scope-nowhere/dev\repo",
            "/aicx-scope-nowhere/dev"
        ));
    }

    /// Finding: a Windows path rooted without a drive (`\repo`) lives on the
    /// CURRENT drive, the turn cwd's. Kept drive-less it matched nothing, so a
    /// call that never left `C:\repo` unplaced its window.
    #[test]
    fn a_drive_less_rooted_windows_workdir_takes_the_baseline_drive() {
        let baseline = r"C:\aicx-scope-nowhere\dev\repo";
        assert_eq!(
            resolve_candidate(r"\aicx-scope-nowhere\dev\repo\pkg", Some(baseline)).as_deref(),
            Some(r"C:\aicx-scope-nowhere\dev\repo\pkg")
        );
        let (scope, _) = effective_window_scope(
            &explicit(&[r"\aicx-scope-nowhere\dev\repo\pkg"]),
            Some(baseline),
        );
        assert_eq!(scope, WindowScope::Baseline);
        // Another directory on that drive stays another directory.
        let (scope, _) = effective_window_scope(
            &explicit(&[r"\aicx-scope-nowhere\dev\other"]),
            Some(baseline),
        );
        assert_ne!(scope, WindowScope::Baseline);
        // UNC names its own root in either separator, and without a
        // drive-shaped baseline there is no drive to borrow.
        for unc in [
            r"\\server\share\repo",
            "//server/share/repo",
            r"/\server\share\repo",
        ] {
            assert_eq!(on_baseline_drive(unc, Some(baseline)), None, "{unc}");
        }
        assert_eq!(
            on_baseline_drive(
                "/aicx-scope-nowhere/dev/repo",
                Some("/aicx-scope-nowhere/dev")
            ),
            None
        );
        assert_eq!(
            on_baseline_drive(
                r"\aicx-scope-nowhere\repo",
                Some("/aicx-scope-nowhere/repo")
            ),
            None
        );
        assert_eq!(on_baseline_drive(r"\aicx-scope-nowhere\repo", None), None);
    }

    /// Finding: Windows reads `/repo/pkg` as rooted on the current drive just
    /// like `\repo\pkg`, but only the backslash form borrowed the baseline's
    /// drive. Kept Unix-shaped, it matched no `C:\repo` baseline and a call
    /// that never left the checkout unplaced its window.
    #[test]
    fn a_slash_rooted_workdir_takes_a_windows_baseline_drive() {
        let baseline = r"C:\aicx-scope-nowhere\dev\repo";
        assert_eq!(
            resolve_candidate("/aicx-scope-nowhere/dev/repo/pkg", Some(baseline)).as_deref(),
            Some("C:/aicx-scope-nowhere/dev/repo/pkg")
        );
        let (scope, _) = effective_window_scope(
            &explicit(&["/aicx-scope-nowhere/dev/repo/pkg"]),
            Some(baseline),
        );
        assert_eq!(scope, WindowScope::Baseline);
        // `..` stays on that drive, and another directory stays another.
        let (scope, _) = effective_window_scope(
            &explicit(&["/aicx-scope-nowhere/dev/repo/../other"]),
            Some(baseline),
        );
        assert_ne!(scope, WindowScope::Baseline);
        // A Unix baseline lends no drive: `/repo` there is a Unix path.
        assert_eq!(
            resolve_candidate(
                "/aicx-scope-nowhere/dev/repo/pkg",
                Some("/aicx-scope-nowhere/dev/repo")
            )
            .as_deref(),
            Some("/aicx-scope-nowhere/dev/repo/pkg")
        );
    }

    /// Finding: a drive-relative Windows workdir (`C:fleet`) is relative to
    /// the current directory OF ITS DRIVE. Joined onto a `D:` baseline it
    /// became `D:\…\vista\C:fleet`, which the lexical reduction placed inside
    /// the baseline, so a call in a foreign `C:` checkout kept its window on
    /// the `D:` project.
    #[test]
    fn a_drive_relative_workdir_joins_only_its_own_drive() {
        let baseline = r"D:\aicx-scope-nowhere\vista";
        for foreign in ["C:fleet", "C:", r"c:..\fleet"] {
            assert_eq!(
                resolve_candidate(foreign, Some(baseline)),
                None,
                "{foreign}"
            );
            assert_eq!(
                effective_window_scope(&explicit(&[foreign]), Some(baseline)),
                (WindowScope::Unattributed, None),
                "{foreign}"
            );
        }
        // The baseline IS its own drive's current directory.
        for (same_drive, resolved) in [
            ("D:fleet", r"D:\aicx-scope-nowhere\vista\fleet"),
            ("d:fleet", r"D:\aicx-scope-nowhere\vista\fleet"),
            ("D:", r"D:\aicx-scope-nowhere\vista"),
        ] {
            assert_eq!(
                resolve_candidate(same_drive, Some(baseline)).as_deref(),
                Some(resolved),
                "{same_drive}"
            );
            assert_eq!(
                effective_window_scope(&explicit(&[same_drive]), Some(baseline)),
                (WindowScope::Baseline, None),
                "{same_drive}"
            );
        }
        // A drive-less or UNC baseline records no drive's current directory.
        for base in [r"\aicx-scope-nowhere\vista", r"\\server\share\vista"] {
            assert_eq!(resolve_candidate("C:fleet", Some(base)), None, "{base}");
        }
        // Under a Unix baseline there are no drives: `C:fleet` is a name.
        assert_eq!(
            resolve_candidate("C:fleet", Some("/aicx-scope-nowhere/vista")).as_deref(),
            Some("/aicx-scope-nowhere/vista/C:fleet")
        );
    }

    /// Finding: a `workdir` written as a variable or an expression was
    /// skipped as "no evidence", so `tools.exec_command({cmd, workdir:
    /// targetDir})` into another checkout left the window on its baseline.
    /// Only the runtime knew that directory: the evidence is opaque.
    ///
    /// Finding: an expression that opens with punctuation —
    /// `[root, repo].join('/')`, `!local ? foreign : base` — was skipped the
    /// same way, and so were `undefined ?? otherDir` and `0 || otherDir`,
    /// because only their first token was looked at.
    #[test]
    fn a_workdir_that_is_not_a_literal_is_opaque() {
        for input in [
            r#"tools.exec_command({cmd: "cargo test", workdir: targetDir})"#,
            r#"tools.exec_command({cmd: "cargo test", workdir: cfg.repo})"#,
            r#"tools.exec_command({cmd: "ls", workdir: path.join(root, "pkg")})"#,
            r#"tools.exec_command({cmd: "ls", workdir: nullish})"#,
            "tools.exec_command({ cmd, workdir })",
            "tools.exec_command({workdir, cmd})",
            r#"tools.exec_command({cmd: "ls", workdir: [root, repo].join('/')})"#,
            r#"tools.exec_command({cmd: "ls", workdir: !local ? foreign : base})"#,
            r#"tools.exec_command({cmd: "ls", workdir: undefined ?? otherDir})"#,
            r#"tools.exec_command({cmd: "ls", workdir: 0 || otherDir})"#,
            r#"tools.exec_command({cmd: "ls", workdir: /* pinned */ otherDir})"#,
            r#"tools.exec_command({cmd: "ls", workdir: /* never closes "/repo/a"})"#,
        ] {
            assert_eq!(
                tool_call_workdirs(&js_input(input)),
                vec![WorkdirEvidence::Opaque],
                "{input}"
            );
        }
        // Asking for the default directory is not naming another one, and a
        // `workdir:` that is not an object property is prose, not a value.
        for input in [
            r#"tools.exec_command({cmd: "ls", workdir: null})"#,
            r#"tools.exec_command({cmd: "ls", workdir: undefined})"#,
            r#"tools.exec_command({cmd: "ls", workdir: null /* default */})"#,
            r#"tools.exec_command({cmd: "ls", workdir: 0})"#,
            r#"tools.exec_command({cmd: "ls", workdir: -1, timeout: 5})"#,
            "// workdir: the repo root\ntools.exec_command({cmd: \"ls\"})",
            r#"tools.exec_command({cmd: "echo workdir: pkg"})"#,
            r#"tools.exec_command({cmd: "echo workdir: [x]"})"#,
        ] {
            assert!(tool_call_workdirs(&js_input(input)).is_empty(), "{input}");
        }
        // A comment before a literal hides nothing: the literal is the value.
        for input in [
            r#"tools.exec_command({cmd: "ls", workdir: /* pinned */ "/repo/a"})"#,
            "tools.exec_command({cmd: \"ls\", workdir: // pinned\n \"/repo/a\"})",
        ] {
            assert_eq!(
                tool_call_workdirs(&js_input(input)),
                vec![WorkdirEvidence::Explicit("/repo/a".to_string())],
                "{input}"
            );
        }
        // A readable literal beside an unreadable value keeps both.
        assert_eq!(
            tool_call_workdirs(&js_input(
                r#"tools.exec_command({cmd: "a", workdir: "/repo/a"}); tools.exec_command({cmd: "b", workdir: other})"#
            )),
            vec![
                WorkdirEvidence::Explicit("/repo/a".to_string()),
                WorkdirEvidence::Opaque
            ]
        );
    }

    /// Finding: the key pattern had no identifier boundary, so another
    /// property whose name merely ends in `workdir` was read as the call's.
    #[test]
    fn only_a_whole_workdir_key_is_read() {
        for input in [
            r#"tools.exec_command({cmd: "ls", networkdir: "/foreign/repo"})"#,
            r#"tools.exec_command({cmd: "ls", fallback_workdir: "/foreign/repo"})"#,
            r#"tools.exec_command({cmd: "ls", "fallback_workdir": "/foreign/repo"})"#,
            r#"tools.exec_command({cmd: "ls", "fallback-workdir": "/foreign/repo"})"#,
            r#"tools.exec_command({cmd: "ls", $workdir: "/foreign/repo"})"#,
            r#"tools.exec_command({cmd: "ls", workdirs: ["/foreign/repo"]})"#,
            r#"tools.exec_command({cmd: "ls", fallback_workdir: other})"#,
        ] {
            assert!(tool_call_workdirs(&js_input(input)).is_empty(), "{input}");
        }
        for input in [
            r#"{workdir: "/repo/a"}"#,
            r#"{"workdir": "/repo/a"}"#,
            r#"{cmd: "ls",workdir:'/repo/a'}"#,
            r#"workdir: "/repo/a""#,
        ] {
            assert_eq!(
                first_workdir(&js_input(input)).as_deref(),
                Some("/repo/a"),
                "{input}"
            );
        }
    }

    /// Finding: a quoted `workdir:` was read as evidence even where no object
    /// property began, so a string that merely quotes a path —
    /// `const example = 'workdir:"/repo/foreign"}'` — re-scoped the window of
    /// the real call beside it to that checkout.
    #[test]
    fn a_workdir_outside_an_object_property_is_no_evidence() {
        for input in [
            r#"const example = 'workdir:"/repo/foreign"}'; await tools.exec_command({cmd:"pwd"});"#,
            r#"console.log("workdir: '/repo/foreign'"); tools.exec_command({cmd: "ls"})"#,
            r#"const hint = `workdir: "/repo/foreign"`; tools.exec_command({cmd: "ls"})"#,
            r#"tools.exec_command({cmd: "echo workdir: '/repo/foreign'"})"#,
            r#"tools.exec_command({cmd: "ls"}) /* workdir: "/repo/foreign" */"#,
            "tools.exec_command({cmd: \"ls\", // workdir: \"/repo/foreign\"\n})",
            r#"const example = "workdir"; tools.exec_command({cmd: "ls"})"#,
        ] {
            assert!(tool_call_workdirs(&js_input(input)).is_empty(), "{input}");
        }
    }

    /// Finding: a `workdir` property written inside a string or a comment —
    /// `'{workdir: "/repo/foreign"}'`, a commented-out call — was read as a
    /// path. Beside a real call that names no directory it was the only
    /// evidence, and it moved the whole window to that checkout.
    #[test]
    fn a_workdir_written_inside_text_is_never_a_path() {
        for input in [
            r#"const example = '{workdir: "/repo/foreign"}'; tools.exec_command({cmd: "pwd"})"#,
            r#"tools.exec_command({cmd: "echo {workdir: '/repo/foreign'}"})"#,
            r#"const doc = `{cmd: "ls", workdir: "/repo/foreign"}`; tools.exec_command({cmd: "pwd"})"#,
            "// tools.exec_command({cmd: \"ls\", workdir: \"/repo/foreign\"})\ntools.exec_command({cmd: \"pwd\"})",
            r#"/* {workdir: "/repo/foreign"} */ tools.exec_command({cmd: "pwd"})"#,
            // A stray apostrophe in prose hides nothing: the key after it on
            // the same line is still seen, only no longer as a path.
            r#"It's here: tools.exec_command({cmd: "ls", workdir: "/repo/foreign"})"#,
        ] {
            assert_eq!(
                tool_call_workdirs(&js_input(input)),
                vec![WorkdirEvidence::Opaque],
                "{input}"
            );
        }
        // JSON arguments whose only `workdir` is inside the command string are
        // read structurally, and the string is data: no evidence at all.
        let payload = serde_json::json!({
            "type": "function_call",
            "name": "shell",
            "arguments": r#"{"cmd":"echo {workdir: '/repo/foreign'}"}"#,
        });
        assert!(tool_call_workdirs(&payload).is_empty());
        // Code beside text is still code: a string, a comment, a template's
        // interpolation, a regular expression and a division each end where
        // JavaScript ends them, and the real call after them is read.
        for input in [
            r#"const note = "it's fine"; tools.exec_command({cmd: "ls", workdir: "/repo/a"})"#,
            "// don't touch\ntools.exec_command({cmd: \"ls\", workdir: \"/repo/a\"})",
            "It's here\ntools.exec_command({cmd: \"ls\", workdir: \"/repo/a\"})",
            r#"const s = `${tools.exec_command({cmd: "ls", workdir: "/repo/a"})}`"#,
            r#"const quote = /'/; tools.exec_command({cmd: "ls", workdir: "/repo/a"})"#,
            r#"const half = total / 2; tools.exec_command({cmd: "ls", workdir: "/repo/a"})"#,
            r#"tools.exec_command({cmd: "curl http://host/x", workdir: "/repo/a"})"#,
            r#"tools.exec_command({"cmd": "ls", "workdir": "/repo/a"})"#,
        ] {
            assert_eq!(
                tool_call_workdirs(&js_input(input)),
                explicit(&["/repo/a"]),
                "{input}"
            );
        }
    }

    /// Finding: a computed key, `{cmd, ["workdir"]: targetDir}`, left the text
    /// before the key ending in `[`, so the occurrence was discarded without
    /// even opaque evidence and a call into another checkout kept its window
    /// on the baseline.
    #[test]
    fn a_computed_workdir_key_is_the_same_property() {
        for input in [
            r#"tools.exec_command({cmd, ["workdir"]: targetDir})"#,
            "tools.exec_command({cmd, ['workdir']: targetDir})",
            "tools.exec_command({cmd, [`workdir`]: targetDir})",
            r#"tools.exec_command({cmd, [ "workdir" /* pinned */ ] : targetDir})"#,
            r#"tools.exec_command({cmd, /* pinned */ ["workdir"]: targetDir})"#,
        ] {
            assert_eq!(
                tool_call_workdirs(&js_input(input)),
                vec![WorkdirEvidence::Opaque],
                "{input}"
            );
        }
        for input in [
            r#"tools.exec_command({cmd: "ls", ["workdir"]: "/repo/a"})"#,
            r#"tools.exec_command({["workdir"]: '/repo/a', cmd: "ls"})"#,
            r#"tools.exec_command({cmd: "ls", [`workdir`]: `/repo/a`})"#,
        ] {
            assert_eq!(
                tool_call_workdirs(&js_input(input)),
                explicit(&["/repo/a"]),
                "{input}"
            );
        }
        // Brackets that compute no property of an object: a member read, an
        // array, a variable's value as the key, and a template literal outside
        // brackets, which is no key at all.
        for input in [
            r#"const dir = options["workdir"]; tools.exec_command({cmd: "ls"})"#,
            r#"const keys = ["workdir", "cmd"]; tools.exec_command({cmd: "ls"})"#,
            r#"tools.exec_command({cmd: "ls", [workdir]: "/repo/foreign"})"#,
            "tools.exec_command({cmd: \"ls\", `workdir`: \"/repo/foreign\"})",
        ] {
            assert!(tool_call_workdirs(&js_input(input)).is_empty(), "{input}");
        }
    }

    /// Finding: the scanner keeps backslashes as written, so an escape that
    /// JavaScript decodes into path structure was read as its spelling.
    /// `"/repo/\x2e\x2e/foreign"` runs in `/foreign`, but read as written it
    /// sat beneath `/repo` and kept the call on the baseline.
    #[test]
    fn an_escape_that_can_spell_path_structure_is_opaque() {
        for input in [
            r#"tools.exec_command({cmd: "ls", workdir: "/repo/\x2e\x2e/foreign"})"#,
            r#"tools.exec_command({cmd: "ls", workdir: "/repo/\u002e\u002e/foreign"})"#,
            r#"tools.exec_command({cmd: "ls", workdir: "/repo/\u{2e}\u{2e}/foreign"})"#,
            r#"tools.exec_command({cmd: "ls", workdir: "/repo/\56\56/foreign"})"#,
            r#"tools.exec_command({cmd: "ls", workdir: '/repo/\.\./foreign'})"#,
            r#"tools.exec_command({cmd: "ls", workdir: "\x2fforeign"})"#,
            r#"tools.exec_command({cmd: "ls", workdir: "repo\/..\/..\/foreign"})"#,
            r#"tools.exec_command({cmd: "ls", workdir: "C\:/foreign"})"#,
            "tools.exec_command({cmd: \"ls\", workdir: `/repo/.\\\n./foreign`})",
        ] {
            assert_eq!(
                tool_call_workdirs(&js_input(input)),
                vec![WorkdirEvidence::Opaque],
                "{input}"
            );
        }
        // A Windows workdir written without escaping keeps its backslashes,
        // and so does a `\x` or `\u` that is no valid escape.
        for (input, workdir) in [
            (
                r#"tools.exec_command({workdir: "C:\repo\crate"})"#,
                r"C:\repo\crate",
            ),
            (
                r#"tools.exec_command({workdir: "C:\users\dev\xampp"})"#,
                r"C:\users\dev\xampp",
            ),
            (
                r#"tools.exec_command({workdir: "C:\\repo\\crate"})"#,
                r"C:\repo\crate",
            ),
        ] {
            assert_eq!(
                tool_call_workdirs(&js_input(input)),
                explicit(&[workdir]),
                "{input}"
            );
        }
    }

    /// Finding: a comment between the previous property and the key hid the
    /// key: `{cmd, /* selected target */ workdir: targetDir}` left the text
    /// before the key ending in `*/`, the value was never read, and a call
    /// into another checkout kept its window on the baseline.
    #[test]
    fn a_comment_before_the_key_hides_nothing() {
        for input in [
            "tools.exec_command({cmd, /* selected target */ workdir: targetDir})",
            "tools.exec_command({cmd, /* a */ /* b */ workdir: targetDir})",
            "tools.exec_command({cmd, // selected target\n  workdir: targetDir})",
            "tools.exec_command({cmd, /* a */ // b\n  workdir: targetDir})",
            "tools.exec_command({/* first */ workdir: targetDir, cmd})",
            "tools.exec_command({url: \"http://host\", // note\n  workdir: targetDir})",
        ] {
            assert_eq!(
                tool_call_workdirs(&js_input(input)),
                vec![WorkdirEvidence::Opaque],
                "{input}"
            );
        }
        for input in [
            r#"tools.exec_command({cmd: "ls", /* pinned */ workdir: "/repo/a"})"#,
            "tools.exec_command({cmd: \"ls\", // pinned\n  workdir: '/repo/a'})",
            "tools.exec_command({cmd: \"ls\",\n  // pinned\n  workdir: \"/repo/a\"})",
        ] {
            assert_eq!(
                tool_call_workdirs(&js_input(input)),
                explicit(&["/repo/a"]),
                "{input}"
            );
        }
    }

    /// Finding: a literal was read as the whole value whatever followed it,
    /// so `"/repos/vista" + "-private"` placed the call in `/repos/vista`.
    #[test]
    fn a_literal_is_the_value_only_when_the_value_ends_there() {
        for input in [
            r#"tools.exec_command({cmd: "ls", workdir: "/repos/vista" + "-private"})"#,
            r#"tools.exec_command({cmd: "ls", workdir: '/repos/vista'.concat(suffix)})"#,
            r#"tools.exec_command({cmd: "ls", workdir: `/repos/vista` + tail, yield_time_ms: 1})"#,
            "tools.exec_command({\n  cmd: \"ls\",\n  workdir: \"/repos/vista\"\n    + \"-private\",\n})",
            r#"tools.exec_command({cmd: "ls", workdir: "/repos/a" || "/repos/b"})"#,
            r#"tools.exec_command({cmd: "ls", workdir: "/repos/vista" /* suffix */ + "-private"})"#,
            "tools.exec_command({\n  workdir: \"/repos/vista\" // suffix\n    + \"-private\",\n})",
            "tools.exec_command({workdir: \"/repos/vista\" /* a */ /* b */ + tail})",
            "tools.exec_command({workdir: \"/repos/vista\" /* never closes",
        ] {
            assert_eq!(
                tool_call_workdirs(&js_input(input)),
                vec![WorkdirEvidence::Opaque],
                "{input}"
            );
        }
        for input in [
            r#"tools.exec_command({cmd: "ls", workdir: "/repos/vista"})"#,
            r#"tools.exec_command({workdir: "/repos/vista", cmd: "ls"})"#,
            "tools.exec_command({\n  workdir: '/repos/vista'  // the checkout\n})",
            "tools.exec_command({workdir: \"/repos/vista\" /* pinned */})",
            "tools.exec_command({workdir: \"/repos/vista\" /* a */ // b\n, cmd: \"ls\"})",
            // A line comment may run to the end of the input. An `exec_command`
            // argument cut off there is also opaque, see
            // `an_exec_argument_not_written_at_the_call_is_opaque`.
            "{workdir: \"/repos/vista\" // last line",
            r#"[{workdir: "/repos/vista"}]"#,
            r#"{"workdir": "/repos/vista"}"#,
        ] {
            assert_eq!(
                tool_call_workdirs(&js_input(input)),
                vec![WorkdirEvidence::Explicit("/repos/vista".to_string())],
                "{input}"
            );
        }
    }

    /// Finding: the bounded catalog reader kept its own copy of the call
    /// types and missed `web_search_call`, which the full adapter scopes.
    /// Every reader now asks this one predicate.
    #[test]
    fn every_call_type_the_adapter_scopes_is_a_tool_call() {
        // The `response_item` and `event_msg` call arms of the Codex adapter.
        for call in [
            "function_call",
            "custom_tool_call",
            "web_search_call",
            "tool_call",
            "mcp_tool_call",
        ] {
            assert!(is_tool_call_payload_type(call), "{call}");
        }
        for other in [
            "function_call_output",
            "custom_tool_call_output",
            "mcp_tool_call_end",
            "message",
            "",
        ] {
            assert!(!is_tool_call_payload_type(other), "{other}");
        }
    }

    /// Finding: the fail-closed threshold for an over-cap record counted raw
    /// `"type":` substrings anywhere in the visible prefix. An `arguments`
    /// object carrying its own `type` field therefore raised the count past
    /// the threshold and bought the record a not-a-tool-call verdict — the
    /// unreadable payload deciding whether it had to be treated as opaque.
    #[test]
    fn argument_content_cannot_talk_an_over_cap_record_out_of_being_a_tool_call() {
        // Envelope discriminator readable, payload discriminator truncated
        // away, and the visible argument body carries a nested `type` key.
        let prefix = concat!(
            r#"{"timestamp":"2026-09-22T00:00:00Z","type":"response_item","payload":{"#,
            r#""arguments":{"cmd":"deploy","env":{"type":"noise"},"workdir":"/other/checkout"#
        );
        assert!(
            truncated_record_is_tool_call(prefix),
            "payload content must never satisfy the discriminator threshold"
        );

        // The same text inside a STRING body is equally inert.
        let quoted = concat!(
            r#"{"timestamp":"2026-09-22T00:00:00Z","type":"response_item","payload":{"#,
            r#""arguments":"{\"cmd\":\"rg 'type': src\",\"workdir\":\"/other/checkout"#
        );
        assert!(
            truncated_record_is_tool_call(quoted),
            "argument text must never satisfy the discriminator threshold"
        );

        // A plainly readable non-call record still keeps its evidence: both
        // real discriminators survived the cap, so nothing was lost.
        let readable = concat!(
            r#"{"timestamp":"2026-09-22T00:00:00Z","type":"response_item","payload":{"#,
            r#""type":"message","role":"assistant","content":[{"#
        );
        assert!(
            !truncated_record_is_tool_call(readable),
            "two structural discriminators, neither a call"
        );
    }

    /// Finding: a payload `type` cut inside its value was recorded as an
    /// empty discriminator and still counted, so an envelope plus a truncated
    /// payload type read as two readable discriminators — neither a call — and
    /// the record kept its window attributed. A key without its answer proves
    /// nothing.
    #[test]
    fn a_truncated_payload_type_is_not_a_readable_discriminator() {
        let envelope = r#"{"timestamp":"2026-09-22T00:00:00Z","type":"response_item","payload":{"#;
        for cut in [r#""type":"funct"#, r#""type":"#, r#""type": "#] {
            let prefix = format!("{envelope}{cut}");
            assert!(
                truncated_record_is_tool_call(&prefix),
                "`{cut}` hides the payload type, so the record may be a call"
            );
        }
        // A discriminator whose value is not a string is not a readable answer
        // either.
        let odd = format!(r#"{envelope}"type":null,"arguments":"#);
        assert!(truncated_record_is_tool_call(&odd));
    }

    /// Finding: the discriminator scan kept every `type` key within two levels,
    /// so a sibling object as shallow as the payload — `metadata: {"type":…}`
    /// before it — supplied the second discriminator. An envelope, that note
    /// and a payload cut before its own late `type` read as two readable
    /// non-call discriminators and kept the window attributed.
    #[test]
    fn a_sibling_type_before_the_payload_is_not_its_discriminator() {
        let prefix = concat!(
            r#"{"timestamp":"2026-09-22T00:00:00Z","type":"response_item","#,
            r#""metadata":{"type":"note"},"payload":{"#,
            r#""arguments":"{\"cmd\":\"deploy\",\"workdir\":\"/other/checkout"#
        );
        assert!(
            truncated_record_is_tool_call(prefix),
            "only the payload's own `type` can prove the record is not a call"
        );

        // A sibling array of objects is content too.
        let listed = concat!(
            r#"{"type":"response_item","tags":[{"type":"note"}],"payload":{"#,
            r#""arguments":"{\"workdir\":\"/other/checkout"#
        );
        assert!(truncated_record_is_tool_call(listed));

        // The payload's own `type`, after the same sibling, still reads.
        let readable = concat!(
            r#"{"type":"response_item","metadata":{"type":"note"},"payload":{"#,
            r#""type":"message","content":[{"#
        );
        assert!(!truncated_record_is_tool_call(readable));
    }

    /// Finding: replaying a session whose checkout is gone from this machine.
    /// Nothing resolves, so lexical comparison is the only evidence there is —
    /// and it has to compare like with like: the baseline-joined `.`, not the
    /// bare token, or every turn of the session is discarded as foreign.
    #[test]
    fn a_relative_workdir_in_a_vanished_checkout_still_belongs_to_it() {
        let baseline = "/nonexistent-aicx-scope/old/repo";

        assert_eq!(
            normalize_workdir(".", Some(baseline)),
            WorkdirIdentity::Unresolved(baseline.to_string()),
            "a relative workdir keeps the baseline it was recorded against"
        );
        assert!(workdir_within_scope(".", baseline));
        assert!(workdir_within_scope("packages/api", baseline));
        assert!(!workdir_within_scope("../sibling", baseline));

        let (scope, path) =
            effective_window_scope(&explicit(&[".", "packages/api"]), Some(baseline));
        assert_eq!(scope, WindowScope::Baseline);
        assert_eq!(path, None);
    }

    #[test]
    fn unresolved_paths_are_unattributed_never_positive_never_conflict() {
        let missing_a = "/definitely/missing/aicx-scope-a";
        let missing_b = "/definitely/missing/aicx-scope-b";
        // One unresolved workdir: not a positive attribution (no scope path
        // stamped), and not proof of divergence either — just "unknown".
        let (single, path) = effective_window_scope(&explicit(&[missing_a]), None);
        assert_eq!(single, WindowScope::Unattributed);
        assert_eq!(path, None);
        // Two distinct unresolved paths: still unknown, not a proven conflict.
        let (scope, path) = effective_window_scope(&explicit(&[missing_a, missing_b]), None);
        assert_eq!(scope, WindowScope::Unattributed);
        assert_eq!(path, None);
    }

    #[test]
    fn resolved_plus_unresolved_is_unattributed_not_conflict() {
        let root = scratch("mixed");
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git dir");
        let workdirs = explicit(&[
            repo.to_string_lossy().as_ref(),
            "/definitely/missing/aicx-scope-elsewhere",
        ]);
        let (scope, path) = effective_window_scope(&workdirs, None);
        assert_eq!(scope, WindowScope::Unattributed);
        assert_eq!(path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_workdirs_keeps_the_baseline() {
        let (scope, path) = effective_window_scope(&[], None);
        assert_eq!(scope, WindowScope::Baseline);
        assert_eq!(path, None);
    }

    #[test]
    fn unreadable_tool_call_evidence_fails_closed_to_unattributed() {
        // The bounded reader drops records over its per-record cap. A tool
        // call whose payload it could not read is still evidence that the
        // window MIGHT have moved: it must never keep the baseline.
        let (scope, path) =
            effective_window_scope(&[WorkdirEvidence::Opaque], Some("/present/vista"));
        assert_eq!(scope, WindowScope::Unattributed);
        assert_eq!(path, None);

        let root = scratch("opaque-plus-resolved");
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git dir");
        let mut workdirs = explicit(&[repo.to_string_lossy().as_ref()]);
        workdirs.push(WorkdirEvidence::Opaque);
        // Even a window that otherwise looks consistent cannot be proven so
        // while one of its tool calls is unreadable.
        let (scope, path) = effective_window_scope(&workdirs, None);
        assert_eq!(scope, WindowScope::Unattributed);
        assert_eq!(path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unresolved_workdir_matching_baseline_is_baseline_evidence() {
        // The codescribe-golden shape: the explicit workdir IS the
        // turn_context cwd (or nests under it) but does not exist on this
        // machine. Nothing foreign happened — the window stays baseline.
        let (scope, path) = effective_window_scope(
            &explicit(&["/Volumes/vc-workspace/vetcoders/codescribe"]),
            Some("/Volumes/vc-workspace/vetcoders/codescribe"),
        );
        assert_eq!(scope, WindowScope::Baseline);
        assert_eq!(path, None);
        let (nested, _) = effective_window_scope(
            &explicit(&["/Volumes/vc-workspace/vetcoders/codescribe/site"]),
            Some("/Volumes/vc-workspace/vetcoders/codescribe"),
        );
        assert_eq!(nested, WindowScope::Baseline);
    }

    #[test]
    fn unresolved_workdir_foreign_to_baseline_is_unattributed() {
        // The 60b7 shape under a missing checkout: turn_context says vista,
        // the explicit workdir points at a fleet-bus path that does not
        // resolve here. Durable do-not-inherit, never a positive vista stamp.
        let (scope, path) =
            effective_window_scope(&explicit(&["/missing/fleet-bus"]), Some("/present/vista"));
        assert_eq!(scope, WindowScope::Unattributed);
        assert_eq!(path, None);
    }
}

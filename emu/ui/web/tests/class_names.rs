//! Guards against a common failure mode in a plain HTML/CSS/JS stack with no build step:
//! renaming or removing a class in `style.css` (or introducing a typo in HTML/JS) has
//! nothing to catch the mismatch - the element just silently renders unstyled at runtime.
//!
//! This test statically extracts every class name *defined* in `assets/style.css` and
//! every class name *used* by `assets/index.html` + `assets/js/*.js`, and fails if
//! anything used isn't defined anywhere. It intentionally does NOT check the reverse
//! direction (CSS classes that are never used) - dynamic class construction (see below)
//! makes that direction too prone to false positives to be worth enforcing.
//!
//! ## Coverage
//! Recognizes: `class="literal tokens"` (in HTML, and in JS template literals - including
//! `${...}`-interpolated tokens, which are skipped rather than guessed at),
//! `.classList.add/remove/toggle("literal", ...)`, and
//! `.className = "literal"` / `` .className = `literal ${x}` ``.
//!
//! ## Known limitation
//! Does NOT trace class names that pass through an intermediate string array/variable
//! before being interpolated - e.g. `emu/ui/web/assets/js/11-view-appinsights.js`'s
//! `SEVERITY_PILL_CLASS` array is indexed and interpolated into a template a few lines
//! away, so its entries aren't seen as "used" here. There are only a couple of such arrays
//! in the codebase, each clearly named and colocated with its one usage site, so a rename
//! there is easy enough to catch by eye. A fully general data-flow analysis is out of scope
//! for a lightweight text-based checker like this one.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

fn assets_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
}

fn is_class_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '-'
}

fn is_class_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn strip_css_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut chars = css.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            while let Some(c2) = chars.next() {
                if c2 == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Pulls dot-prefixed class tokens (`.foo`) out of a chunk of CSS selector text, e.g.
/// `.nav-item.active:hover` -> `nav-item`, `active`.
fn extract_dotted_classes(text: &str, out: &mut HashSet<String>) {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '.' && i + 1 < chars.len() && is_class_start(chars[i + 1]) {
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && is_class_char(chars[j]) {
                j += 1;
            }
            out.insert(chars[start..j].iter().collect());
            i = j;
        } else {
            i += 1;
        }
    }
}

/// Every class name defined anywhere in `style.css`, found by scanning only the text that
/// immediately precedes each `{` (i.e. selector text) - text *inside* declaration blocks
/// (property values like `url("...svg")` or decimal numbers like `.5`) is never scanned,
/// so it can never pollute this set with false "definitions".
fn defined_css_classes(css: &str) -> HashSet<String> {
    let cleaned = strip_css_comments(css);
    let mut classes = HashSet::new();
    let mut segment = String::new();
    for c in cleaned.chars() {
        match c {
            '{' => {
                extract_dotted_classes(&segment, &mut classes);
                segment.clear();
            }
            '}' => segment.clear(),
            _ => segment.push(c),
        }
    }
    classes
}

/// Finds the index of the closing `quote` for a string/template literal starting at
/// `start`, honoring `${...}` interpolation depth so an embedded quote inside an
/// interpolated expression (e.g. `` `class="${cls.join(" ")}"` ``) doesn't end the match
/// early.
fn find_literal_end(chars: &[char], start: usize, quote: char) -> Option<usize> {
    let mut i = start;
    let mut depth = 0usize;
    while i < chars.len() {
        if depth == 0 && chars[i] == quote {
            return Some(i);
        }
        if chars[i] == '$' && chars.get(i + 1) == Some(&'{') {
            depth += 1;
            i += 2;
            continue;
        }
        if depth > 0 {
            if chars[i] == '{' {
                depth += 1;
            } else if chars[i] == '}' {
                depth -= 1;
            }
        }
        i += 1;
    }
    None
}

/// Removes every balanced `${...}` interpolation region from `value` (replacing each with a
/// single space), so e.g. `dot ${anyRunning ? "dot-on" : "dot-off"}` becomes just `dot ` -
/// leaving only the statically-known literal text behind. Without this, splitting the raw
/// value on whitespace would leak pieces of the *expression itself* (`?`, `:`, `"dot-on"`,
/// `"dot-off"}`, ...) as if they were separate class-name tokens.
fn strip_interpolations(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && chars.get(i + 1) == Some(&'{') {
            let mut depth = 1;
            i += 2;
            while i < chars.len() && depth > 0 {
                match chars[i] {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
            out.push(' ');
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Splits an extracted attribute value into individual class tokens, after first removing
/// any `${...}` interpolation (can't be statically verified) rather than guessing.
fn tokens_from_value(value: &str, out: &mut HashSet<String>) {
    for tok in strip_interpolations(value).split_whitespace() {
        if !tok.is_empty() {
            out.insert(tok.to_string());
        }
    }
}

fn starts_with_at(chars: &[char], i: usize, pat: &str) -> bool {
    let pat_chars: Vec<char> = pat.chars().collect();
    i + pat_chars.len() <= chars.len() && chars[i..i + pat_chars.len()] == pat_chars[..]
}

/// Scans HTML/JS source text for every class name used via `class="..."` attributes,
/// `.className = "..."` / `` .className = `...` `` assignments, and
/// `.classList.add/remove/toggle("...")` calls.
fn used_classes(src: &str) -> HashSet<String> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = HashSet::new();
    let mut i = 0;
    while i < chars.len() {
        if starts_with_at(&chars, i, "class=\"") {
            let start = i + "class=\"".len();
            if let Some(end) = find_literal_end(&chars, start, '"') {
                let value: String = chars[start..end].iter().collect();
                tokens_from_value(&value, &mut out);
                i = end + 1;
                continue;
            }
        }

        if starts_with_at(&chars, i, ".className = ") {
            let after = i + ".className = ".len();
            if let Some(&quote) = chars.get(after) {
                if quote == '"' || quote == '`' {
                    let start = after + 1;
                    if let Some(end) = find_literal_end(&chars, start, quote) {
                        let value: String = chars[start..end].iter().collect();
                        tokens_from_value(&value, &mut out);
                        i = end + 1;
                        continue;
                    }
                }
            }
        }

        let classlist_method = [".classList.add(", ".classList.remove(", ".classList.toggle("]
            .into_iter()
            .find(|m| starts_with_at(&chars, i, m));
        if let Some(method) = classlist_method {
            let mut j = i + method.len();
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if let Some(&quote) = chars.get(j) {
                if quote == '"' || quote == '\'' || quote == '`' {
                    let start = j + 1;
                    if let Some(end) = find_literal_end(&chars, start, quote) {
                        let value: String = chars[start..end].iter().collect();
                        if !value.contains("${") {
                            out.insert(value);
                        }
                        i = end + 1;
                        continue;
                    }
                }
            }
        }

        i += 1;
    }
    out
}

#[test]
fn every_used_class_is_defined_in_css() {
    let dir = assets_dir();
    let css = fs::read_to_string(dir.join("style.css")).expect("read style.css");
    let defined = defined_css_classes(&css);

    let mut used: HashSet<String> = HashSet::new();
    let html = fs::read_to_string(dir.join("index.html")).expect("read index.html");
    used.extend(used_classes(&html));

    let js_dir = dir.join("js");
    let mut js_files: Vec<_> = fs::read_dir(&js_dir)
        .expect("read assets/js dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "js").unwrap_or(false))
        .collect();
    js_files.sort();
    assert!(!js_files.is_empty(), "expected split JS files under assets/js/");

    for path in &js_files {
        let src = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        used.extend(used_classes(&src));
    }

    let mut missing: Vec<&String> = used.iter().filter(|c| !defined.contains(*c)).collect();
    missing.sort();
    assert!(
        missing.is_empty(),
        "class name(s) used in HTML/JS but not defined anywhere in style.css - likely a \
         typo, or a stale reference left over from a CSS rename: {missing:?}\n(if this is a \
         false positive from a class name that only exists via an intermediate array/\
         variable - see this test file's module doc - double check by hand instead of just \
         adding it to style.css)"
    );
}

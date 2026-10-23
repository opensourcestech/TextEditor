//! Sublime Text snippets: `.sublime-snippet` files, and the template syntax
//! inside them: `$1` and `${2:placeholder}` fields (visited with Tab, with
//! repeated numbers typed into together), `$0` for where the caret ends up,
//! `$TM_FILENAME`-style variables, optionally `${VAR:with a default}`, and
//! `${1/regex/format/options}` transforms of a field or variable.

use std::collections::HashMap;
use std::str::FromStr;

use syntect::highlighting::ScopeSelectors;
use syntect::parsing::{Regex, Region, Scope};

use crate::buffer::Field;

pub struct Snippet {
    /// Typed before Tab to insert the snippet; empty for palette-only ones.
    pub trigger: String,
    pub description: String,
    pub content: String,
    /// Where the trigger works, e.g. `source.rust`; `None` means everywhere.
    scope: Option<ScopeSelectors>,
    /// `Package/path.sublime-snippet`, for `insert_snippet {"name": …}`.
    pub path: String,
}

impl Snippet {
    /// Reads a `.sublime-snippet` file (a small XML document).
    pub fn parse(xml: &str, path: String) -> Result<Self, String> {
        let content = tag(xml, "content").ok_or("no <content>")?;
        // The text usually starts on the line after `<![CDATA[`; that newline isn't part of it.
        let content = content.strip_prefix("\r\n").or_else(|| content.strip_prefix('\n')).unwrap_or(&content);
        let content = content.trim_end_matches([' ', '\t']).trim_end_matches('\n').trim_end_matches('\r');
        let scope = match tag(xml, "scope").map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()) {
            Some(s) => Some(ScopeSelectors::from_str(&s).map_err(|e| format!("scope {s:?}: {e}"))?),
            None => None,
        };
        Ok(Self {
            trigger: tag(xml, "tabTrigger").map(|t| t.trim().to_owned()).unwrap_or_default(),
            description: tag(xml, "description").map(|d| d.trim().to_owned()).unwrap_or_default(),
            content: content.to_owned(),
            scope,
            path,
        })
    }

    /// What the palette shows: the description, else the trigger, else the file name.
    pub fn title(&self) -> String {
        if !self.description.is_empty() {
            self.description.clone()
        } else if !self.trigger.is_empty() {
            self.trigger.clone()
        } else {
            crate::packages::base_name(&self.path).to_owned()
        }
    }

    /// Whether the snippet is meant for text with these scopes (outermost first).
    pub fn applies_to(&self, scopes: &[Scope]) -> bool {
        self.scope.as_ref().is_none_or(|selector| selector.does_match(scopes).is_some())
    }
}

/// The snippet whose trigger `before` (the text before the caret) ends
/// with, preferring the longest trigger. `scopes(n)` gives the scopes `n`
/// chars before the caret, where a trigger that long starts.
pub fn find_trigger<'a>(snippets: &'a [Snippet], before: &str, scopes: &dyn Fn(usize) -> Vec<Scope>) -> Option<&'a Snippet> {
    snippets
        .iter()
        .filter(|s| ends_with_trigger(before, &s.trigger) && s.applies_to(&scopes(s.trigger.chars().count())))
        // Later packages (and User) win over earlier ones with the same trigger.
        .max_by_key(|s| s.trigger.chars().count())
}

/// `before` ends with `trigger` as a whole word: `fn` matches `x = fn`, not `xfn`.
pub fn ends_with_trigger(before: &str, trigger: &str) -> bool {
    let Some(rest) = before.strip_suffix(trigger) else {
        return false;
    };
    let word = |c: char| c.is_alphanumeric() || c == '_';
    !trigger.is_empty() && !(trigger.starts_with(word) && rest.ends_with(word))
}

/// What expanding a template needs to know about where it goes.
pub struct Context<'a> {
    /// Values for `$NAME` variables; unknown ones expand to nothing.
    pub var: &'a dyn Fn(&str) -> Option<String>,
    /// Added after every newline, so the snippet keeps the line's indentation.
    pub indent: &'a str,
    /// Replaces each tab at the start of a snippet line, e.g. with spaces.
    pub tab: &'a str,
}

/// A template expanded where it goes.
pub struct Expanded {
    pub text: String,
    /// Char offsets into `text`. There is always a group-0 field: at `$0`,
    /// else at the end.
    pub fields: Vec<Field>,
    /// What `Field::transform` indexes.
    pub transforms: Vec<Transform>,
}

pub fn expand(template: &str, cx: &Context) -> Expanded {
    // The first pass finds each field's placeholder, so `$1` can repeat it
    // even when it comes before `${1:…}`.
    let mut first = Expander::new(template, cx, HashMap::new());
    first.sequence(false);
    let mut second = Expander::new(template, cx, first.placeholders);
    second.sequence(false);
    let mut fields = second.fields;
    if !fields.iter().any(|f| f.group == 0) {
        fields.push(Field { start: second.len, end: second.len, group: 0, transform: None });
    }
    Expanded { text: second.out, fields, transforms: second.transforms }
}

/// `${1/regex/format/options}`: a field's (or variable's) text with
/// `regex` replaced by `format`. The format can use `$1` or `${1}` for
/// groups, `\u`/`\l` to change the case of the next letter, `\U`/`\L`
/// … `\E` for a stretch, `\n`/`\t`, and `(?1:if matched:otherwise)`.
/// Options: `g` replaces every match, `i` ignores case.
#[derive(Clone, Debug)]
pub struct Transform {
    regex: Regex,
    format: Vec<char>,
    global: bool,
}

impl Transform {
    /// `None` if the regex is invalid.
    pub fn new(regex: &str, format: &str, options: &str) -> Option<Self> {
        let pattern = if options.contains('i') { format!("(?i){regex}") } else { regex.to_owned() };
        if Regex::try_compile(&pattern).is_some() {
            return None;
        }
        Some(Self { regex: Regex::new(pattern), format: format.chars().collect(), global: options.contains('g') })
    }

    pub fn apply(&self, text: &str) -> String {
        let mut out = String::new();
        let mut pos = 0;
        let mut region = Region::new();
        while pos <= text.len() && self.regex.search(text, pos, text.len(), Some(&mut region)) {
            let Some((start, end)) = region.pos(0) else {
                break;
            };
            out.push_str(&text[pos..start]);
            let mut writer = CaseWriter::default();
            writer.format(&self.format, &|n| region.pos(n).map(|(a, b)| &text[a..b]));
            out.push_str(&writer.out);
            pos = end;
            if start == end {
                // An empty match: keep one char and search on after it.
                match text[end..].chars().next() {
                    Some(c) => {
                        out.push(c);
                        pos += c.len_utf8();
                    }
                    None => pos = text.len() + 1,
                }
            }
            if !self.global {
                break;
            }
        }
        if let Some(rest) = text.get(pos..) {
            out.push_str(rest);
        }
        out
    }
}

/// Writes a format string's output, applying `\u`, `\U`… case changes.
#[derive(Default)]
struct CaseWriter {
    out: String,
    /// `\U` or `\L` in effect: upper (true) or lower case.
    stretch: Option<bool>,
    /// `\u` or `\l` waiting for the next letter.
    next: Option<bool>,
}

impl CaseWriter {
    fn push(&mut self, text: &str) {
        for c in text.chars() {
            match self.next.take().or(self.stretch) {
                Some(true) => self.out.extend(c.to_uppercase()),
                Some(false) => self.out.extend(c.to_lowercase()),
                None => self.out.push(c),
            }
        }
    }

    fn format<'t>(&mut self, f: &[char], group: &dyn Fn(usize) -> Option<&'t str>) {
        let mut i = 0;
        let number = |i: &mut usize| {
            let mut n = 0;
            while let Some(d) = f.get(*i).and_then(|c| c.to_digit(10)) {
                n = n * 10 + d as usize;
                *i += 1;
            }
            n
        };
        while i < f.len() {
            match (f[i], f.get(i + 1).copied()) {
                ('\\', Some(c)) => {
                    i += 2;
                    match c {
                        'u' => self.next = Some(true),
                        'l' => self.next = Some(false),
                        'U' => self.stretch = Some(true),
                        'L' => self.stretch = Some(false),
                        'E' => self.stretch = None,
                        'n' => self.push("\n"),
                        't' => self.push("\t"),
                        c => self.push(c.encode_utf8(&mut [0; 4])),
                    }
                }
                ('$', Some(d)) if d.is_ascii_digit() => {
                    i += 1;
                    let n = number(&mut i);
                    self.push(group(n).unwrap_or(""));
                }
                ('$', Some('{')) if f.get(i + 2).is_some_and(char::is_ascii_digit) => {
                    i += 2;
                    let n = number(&mut i);
                    if f.get(i) == Some(&'}') {
                        i += 1;
                    }
                    self.push(group(n).unwrap_or(""));
                }
                ('(', Some('?')) if f.get(i + 2).is_some_and(char::is_ascii_digit) => {
                    i += 2;
                    let n = number(&mut i);
                    if f.get(i) == Some(&':') {
                        i += 1;
                    }
                    let (then, otherwise, after) = split_conditional(f, i);
                    let branch = if group(n).is_some_and(|g| !g.is_empty()) { then } else { otherwise };
                    self.format(branch, group);
                    i = after;
                }
                (c, _) => {
                    i += 1;
                    self.push(c.encode_utf8(&mut [0; 4]));
                }
            }
        }
    }
}

/// From just inside `(?1:`, the "then" and "else" parts and the index after
/// the closing `)`.
fn split_conditional(f: &[char], start: usize) -> (&[char], &[char], usize) {
    let mut depth = 0;
    let mut colon = None;
    let mut i = start;
    while i < f.len() {
        match f[i] {
            '\\' => i += 1,
            '(' => depth += 1,
            ')' if depth == 0 => {
                let (then, otherwise) = match colon {
                    Some(c) => (&f[start..c], &f[c + 1..i]),
                    None => (&f[start..i], &f[i..i]),
                };
                return (then, otherwise, i + 1);
            }
            ')' => depth -= 1,
            ':' if depth == 0 && colon.is_none() => colon = Some(i),
            _ => {}
        }
        i += 1;
    }
    (&f[start..], &f[f.len()..], f.len())
}

struct Expander<'a> {
    chars: Vec<char>,
    i: usize,
    out: String,
    /// `out`'s length in chars.
    len: usize,
    fields: Vec<Field>,
    transforms: Vec<Transform>,
    placeholders: HashMap<u32, String>,
    at_line_start: bool,
    /// Parsing a default that isn't used: nothing is written.
    muted: bool,
    cx: &'a Context<'a>,
}

impl<'a> Expander<'a> {
    fn new(template: &str, cx: &'a Context<'a>, placeholders: HashMap<u32, String>) -> Self {
        Self {
            chars: template.chars().collect(),
            i: 0,
            out: String::new(),
            len: 0,
            fields: Vec::new(),
            transforms: Vec::new(),
            placeholders,
            at_line_start: true,
            muted: false,
            cx,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn emit(&mut self, c: char) {
        if self.muted {
            return;
        }
        let push = |s: &str, out: &mut String, len: &mut usize| {
            out.push_str(s);
            *len += s.chars().count();
        };
        if c == '\n' {
            push("\n", &mut self.out, &mut self.len);
            push(self.cx.indent, &mut self.out, &mut self.len);
            self.at_line_start = true;
        } else if c == '\t' && self.at_line_start {
            push(self.cx.tab, &mut self.out, &mut self.len);
        } else {
            push(c.encode_utf8(&mut [0; 4]), &mut self.out, &mut self.len);
            self.at_line_start = false;
        }
    }

    fn emit_str(&mut self, s: &str) {
        for c in s.chars() {
            self.emit(c);
        }
    }

    /// Text up to the end, or up to an unmatched `}` when `in_braces`.
    fn sequence(&mut self, in_braces: bool) {
        while let Some(c) = self.peek() {
            match c {
                '\\' if matches!(self.chars.get(self.i + 1), Some('$' | '\\' | '}')) => {
                    self.emit(self.chars[self.i + 1]);
                    self.i += 2;
                }
                '}' if in_braces => return,
                '$' if self.dollar() => {}
                _ => {
                    self.emit(c);
                    self.i += 1;
                }
            }
        }
    }

    /// Reads a `$…` construct; false (and nothing read) if it isn't one,
    /// in which case the `$` is ordinary text.
    fn dollar(&mut self) -> bool {
        let start = self.i;
        self.i += 1;
        match self.peek() {
            Some(c) if c.is_ascii_digit() => {
                let n = self.number();
                self.mirror(n);
                true
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let name = self.name();
                let value = (self.cx.var)(&name).unwrap_or_default();
                self.emit_str(&value);
                true
            }
            Some('{') => {
                self.i += 1;
                if self.braced() {
                    true
                } else {
                    self.i = start;
                    false
                }
            }
            _ => {
                self.i = start;
                false
            }
        }
    }

    /// After `${`: a field, a variable, or a transform (which is skipped).
    fn braced(&mut self) -> bool {
        match self.peek() {
            Some(c) if c.is_ascii_digit() => {
                let n = self.number();
                match self.peek() {
                    Some('}') => {
                        self.i += 1;
                        self.mirror(n);
                    }
                    Some(':') => {
                        self.i += 1;
                        let (start, at) = (self.len, self.out.len());
                        self.sequence(true);
                        self.i += 1;
                        if !self.muted {
                            self.fields.push(Field { start, end: self.len, group: n, transform: None });
                            let text = self.out[at..].to_owned();
                            self.placeholders.entry(n).or_insert(text);
                        }
                    }
                    // `${1/regex/format/}`: kept up to date as field 1 changes.
                    Some('/') => {
                        let transform = self.transform();
                        let start = self.len;
                        let source = self.placeholders.get(&n).cloned().unwrap_or_default();
                        // An invalid one is left out, rather than mirroring the field.
                        if let Some(t) = transform {
                            self.emit_raw(&t.apply(&source));
                            if !self.muted {
                                self.transforms.push(t);
                                let index = Some((self.transforms.len() - 1) as u16);
                                self.fields.push(Field { start, end: self.len, group: n, transform: index });
                            }
                        }
                    }
                    _ => return false,
                }
                true
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let name = self.name();
                let value = (self.cx.var)(&name).unwrap_or_default();
                match self.peek() {
                    Some('}') => {
                        self.i += 1;
                        self.emit_str(&value);
                    }
                    Some(':') => {
                        self.i += 1;
                        // The default is used only when the variable is empty.
                        let was_muted = self.muted;
                        if value.is_empty() {
                            self.sequence(true);
                        } else {
                            self.emit_str(&value);
                            self.muted = true;
                            self.sequence(true);
                            self.muted = was_muted;
                        }
                        self.i += 1;
                    }
                    Some('/') => {
                        let value = match self.transform() {
                            Some(t) => t.apply(&value),
                            None => value,
                        };
                        self.emit_str(&value);
                    }
                    _ => return false,
                }
                true
            }
            _ => false,
        }
    }

    /// `$1`: the field's placeholder again, typed into along with it.
    fn mirror(&mut self, n: u32) {
        let start = self.len;
        let text = self.placeholders.get(&n).cloned().unwrap_or_default();
        // Already indented when it was first written out.
        self.emit_raw(&text);
        if !self.muted {
            self.fields.push(Field { start, end: self.len, group: n, transform: None });
        }
    }

    /// Writes text as it is, without re-indenting it.
    fn emit_raw(&mut self, text: &str) {
        if !self.muted {
            self.out.push_str(text);
            self.len += text.chars().count();
        }
    }

    /// After `${1` or `${VAR`, at the first `/`: reads `/regex/format/options}`.
    /// `None` (with the text still skipped) if the regex is invalid.
    fn transform(&mut self) -> Option<Transform> {
        self.i += 1;
        let regex = self.until('/');
        let format = self.until('/');
        let options = self.until('}');
        Transform::new(&regex, &format, &options)
    }

    /// Text up to an unescaped `stop`, which is skipped. An escaped `stop`
    /// loses its backslash; other escapes are left for the regex or format.
    fn until(&mut self, stop: char) -> String {
        let mut text = String::new();
        while let Some(c) = self.peek() {
            self.i += 1;
            if c == stop {
                break;
            }
            if c == '\\'
                && let Some(next) = self.peek()
            {
                self.i += 1;
                if next != stop {
                    text.push('\\');
                }
                text.push(next);
                continue;
            }
            text.push(c);
        }
        text
    }

    fn number(&mut self) -> u32 {
        let mut n: u32 = 0;
        while let Some(d) = self.peek().and_then(|c| c.to_digit(10)) {
            n = n.saturating_mul(10).saturating_add(d);
            self.i += 1;
        }
        n
    }

    fn name(&mut self) -> String {
        let mut name = String::new();
        while let Some(c) = self.peek().filter(|c| c.is_ascii_alphanumeric() || *c == '_') {
            name.push(c);
            self.i += 1;
        }
        name
    }
}

/// The text of the first `<name>…</name>`, with CDATA sections kept as they
/// are and XML entities decoded elsewhere.
fn tag(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = xml.find(&open)? + open.len();
    let end = start + xml[start..].find(&close)?;
    let mut inner = &xml[start..end];
    let mut text = String::new();
    while !inner.is_empty() {
        if let Some(rest) = inner.strip_prefix("<![CDATA[") {
            let stop = rest.find("]]>").unwrap_or(rest.len());
            text.push_str(&rest[..stop]);
            inner = rest.get(stop + 3..).unwrap_or("");
        } else {
            let stop = inner.find("<![CDATA[").unwrap_or(inner.len());
            text.push_str(
                &inner[..stop]
                    .replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&quot;", "\"")
                    .replace("&apos;", "'")
                    .replace("&amp;", "&"),
            );
            inner = &inner[stop..];
        }
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expand_plain(template: &str) -> (String, Vec<(usize, usize, u32)>) {
        let var = |name: &str| match name {
            "TM_FILENAME" => Some("main.rs".to_owned()),
            "SELECTION" => Some("picked".to_owned()),
            _ => None,
        };
        let cx = Context { var: &var, indent: "    ", tab: "  " };
        let e = expand(template, &cx);
        (e.text, e.fields.iter().map(|f| (f.start, f.end, f.group)).collect())
    }

    #[test]
    fn expands_fields_mirrors_and_variables() {
        let (text, fields) = expand_plain("fn ${1:name}($2) -> $1 {\n\t$0\n}");
        assert_eq!(text, "fn name() -> name {\n      \n    }");
        assert_eq!(fields, [(3, 7, 1), (8, 8, 2), (13, 17, 1), (26, 26, 0)]);

        // A mirror before its placeholder still gets the placeholder's text.
        assert_eq!(expand_plain("$1 ${1:x}").0, "x x");
        // Nested fields, variables, defaults, escapes and plain dollars.
        let (text, fields) = expand_plain("${1:a ${2:b}} $TM_FILENAME ${NOPE:none} ${SELECTION:unused} \\$5 $ 5");
        assert_eq!(text, "a b main.rs none picked $5 $ 5");
        assert_eq!(fields[0..2], [(2, 3, 2), (0, 3, 1)]);
        assert_eq!(fields.last(), Some(&(text.chars().count(), text.chars().count(), 0)));
        // A transform starts from the placeholder; it gets its own field.
        let (text, fields) = expand_plain("${1:my_name} ${1/(^|_)(\\w)/\\u$2/g}!");
        assert_eq!(text, "my_name MyName!");
        assert_eq!(fields[1], (8, 14, 1));
        assert_eq!(expand_plain("${TM_FILENAME/(\\w+)\\.rs/$1/}").0, "main");
        // An invalid regex leaves an empty field.
        assert_eq!(expand_plain("${1:x}${1/(/y/}.").0, "x.");
    }

    #[test]
    fn transforms_change_case_and_pick_branches() {
        let t = |re: &str, format: &str, options: &str, text: &str| Transform::new(re, format, options).unwrap().apply(text);
        assert_eq!(t("a", "b", "", "aaa"), "baa");
        assert_eq!(t("a", "b", "g", "aaa"), "bbb");
        assert_eq!(t("A", "b", "gi", "aA"), "bb");
        assert_eq!(t("(\\w+) (\\w+)", "\\U$2\\E-${1}", "", "hello world"), "WORLD-hello");
        assert_eq!(t("^(\\w)", "\\u$1", "", "word"), "Word");
        assert_eq!(t("^(x)?(.*)", "(?1:had x:no x) $2", "", "xyz"), "had x yz");
        assert_eq!(t("^(x)?(.*)", "(?1:had x:no x) $2", "", "abc"), "no x abc");
        assert_eq!(t("$", "!", "g", "end"), "end!", "empty matches don't loop");
        assert!(Transform::new("(", "", "").is_none());
    }

    #[test]
    fn reads_snippet_files() {
        let xml = r#"<snippet>
    <content><![CDATA[
println!("${1:{}}", $2);
]]></content>
    <tabTrigger>pl</tabTrigger>
    <scope>source.rust</scope>
    <description>println &amp; friends</description>
</snippet>"#;
        let s = Snippet::parse(xml, "Rust/println.sublime-snippet".into()).unwrap();
        assert_eq!(s.content, "println!(\"${1:{}}\", $2);");
        assert_eq!(s.trigger, "pl");
        assert_eq!(s.title(), "println & friends");
        assert!(s.applies_to(&[Scope::new("source.rust").unwrap()]));
        assert!(!s.applies_to(&[Scope::new("source.python").unwrap()]));
        assert!(Snippet::parse("<snippet/>", "x".into()).is_err());

        let snippets = [s];
        let rust = [Scope::new("source.rust").unwrap()];
        let in_rust = |_| rust.to_vec();
        assert!(find_trigger(&snippets, "    pl", &in_rust).is_some());
        assert!(find_trigger(&snippets, "    apl", &in_rust).is_none(), "only whole words");
        assert!(find_trigger(&snippets, "pl", &|_| vec![Scope::new("text.plain").unwrap()]).is_none());
        // Selectors see every scope at the caret, e.g. not inside a string.
        let in_string = [rust[0], Scope::new("string.quoted").unwrap()];
        let code_only = Snippet::parse(&xml.replace("source.rust", "source.rust - string"), "x".into()).unwrap();
        assert!(code_only.applies_to(&rust) && !code_only.applies_to(&in_string));
    }
}

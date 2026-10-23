//! Find in Files: search every file in folders (or the
//! open files) for text or a regex, list the matches in a Find Results tab,
//! and replace them all.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use syntect::parsing::{Regex, Region};

/// Bigger files are skipped: they are rarely text anyone searches.
const MAX_FILE_BYTES: u64 = 16 << 20;
/// Stop listing files past this many, so a huge folder can't stall a search.
const MAX_FILES: usize = 100_000;
/// Long lines are cut in the results so minified files stay readable.
const MAX_SHOWN_LINE_CHARS: usize = 300;
/// Groups a replacement can refer to, `$0` to `$9`.
const MAX_GROUPS: usize = 10;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

impl Query {
    /// The query as a regex; an error if `pattern` isn't a valid one.
    pub fn compile(&self) -> Result<Regex, String> {
        if self.pattern.is_empty() {
            return Err("nothing to find".into());
        }
        let mut pattern = if self.regex { self.pattern.clone() } else { escape_regex(&self.pattern) };
        if self.whole_word {
            pattern = format!(r"\b(?:{pattern})\b");
        }
        if !self.case_sensitive {
            pattern = format!("(?i){pattern}");
        }
        if let Some(error) = Regex::try_compile(&pattern) {
            return Err(format!("bad pattern: {error}"));
        }
        Ok(Regex::new(pattern))
    }

    /// How the results header describes the search.
    pub fn describe(&self) -> String {
        let mut flags = Vec::new();
        if self.regex {
            flags.push("regex");
        }
        if self.case_sensitive {
            flags.push("case sensitive");
        }
        if self.whole_word {
            flags.push("whole word");
        }
        let flags = if flags.is_empty() { String::new() } else { format!(" ({})", flags.join(", ")) };
        format!("\"{}\"{flags}", self.pattern)
    }
}

pub fn escape_regex(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\.^$|?*+()[]{}/".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// One match: bytes `start..end` of the text, and each group's bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    pub start: usize,
    pub end: usize,
    pub groups: Vec<Option<(usize, usize)>>,
}

/// Every non-overlapping match of `regex` in `text`.
pub fn find_matches(regex: &Regex, text: &str) -> Vec<Match> {
    let mut found = Vec::new();
    let mut region = Region::new();
    let mut pos = 0;
    while pos <= text.len() && regex.search(text, pos, text.len(), Some(&mut region)) {
        let Some((start, end)) = region.pos(0) else {
            break;
        };
        // Region can't say how many groups there are; past the last, pos is None.
        let groups = (0..MAX_GROUPS).map(|i| region.pos(i)).collect();
        found.push(Match { start, end, groups });
        // An empty match moves on by a char, so it isn't found forever.
        pos = if end > start { end } else { end + text[end..].chars().next().map_or(1, char::len_utf8) };
    }
    found
}

/// The replacement for one match. With regexes, `$1`, `${1}` and `\1`
/// stand for groups, `$0` and `$&` for the whole match, and `\n` and `\t`
/// for a newline and a tab.
pub fn expand_replacement(replacement: &str, m: &Match, text: &str, regex: bool) -> String {
    if !regex {
        return replacement.to_owned();
    }
    let group = |n: usize| m.groups.get(n).copied().flatten().map_or("", |(s, e)| &text[s..e]);
    let mut out = String::new();
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek().copied()) {
            ('$', Some('&')) => {
                chars.next();
                out.push_str(group(0));
            }
            ('$', Some('{')) => {
                let rest: String = chars.clone().skip(1).take_while(|c| *c != '}').collect();
                match rest.parse::<usize>() {
                    Ok(n) => {
                        out.push_str(group(n));
                        for _ in 0..rest.len() + 2 {
                            chars.next();
                        }
                    }
                    Err(_) => out.push('$'),
                }
            }
            ('$' | '\\', Some(d)) if d.is_ascii_digit() => {
                let mut digits = String::new();
                while let Some(d) = chars.peek().copied().filter(char::is_ascii_digit) {
                    digits.push(d);
                    chars.next();
                }
                out.push_str(group(digits.parse().unwrap_or(0)));
            }
            ('\\', Some('n')) => {
                chars.next();
                out.push('\n');
            }
            ('\\', Some('t')) => {
                chars.next();
                out.push('\t');
            }
            ('\\', Some('\\')) => {
                chars.next();
                out.push('\\');
            }
            _ => out.push(c),
        }
    }
    out
}

/// Where to search, from the Where field: folders and files, `*.rs`-style
/// patterns files must match, `-pattern`s they must not, and `<open
/// files>` or `<open folders>`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scope {
    pub paths: Vec<PathBuf>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub open_files: bool,
}

impl Scope {
    /// Reads the Where field. With no folder or file in it, the search
    /// covers `folder` (the open one), or else the open files.
    pub fn parse(text: &str, folder: Option<&Path>) -> Self {
        let mut scope = Scope::default();
        let mut open_folders = false;
        for item in text.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if item == "<open files>" {
                scope.open_files = true;
            } else if item == "<open folders>" || item == "<project>" {
                open_folders = true;
            } else if let Some(pattern) = item.strip_prefix('-') {
                scope.exclude.push(pattern.trim().to_owned());
            } else if item.contains(['*', '?']) && !Path::new(item).is_absolute() {
                scope.include.push(item.to_owned());
            } else {
                scope.paths.push(expand_home(item));
            }
        }
        if (open_folders || (scope.paths.is_empty() && !scope.open_files)) && let Some(folder) = folder {
            scope.paths.push(folder.to_path_buf());
        }
        if scope.paths.is_empty() {
            scope.open_files = true;
        }
        scope
    }

    /// Whether `path` passes the patterns. A pattern with a `/` is matched
    /// against the path below `base`. Otherwise an include pattern is
    /// matched against the file name, and an exclude pattern against every
    /// part of the path, so `-node_modules` leaves out that whole folder.
    pub fn wants(&self, path: &Path, base: &Path) -> bool {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let rel = path.strip_prefix(base).unwrap_or(path).to_string_lossy().replace('\\', "/");
        let in_path = |pattern: &str| {
            let pattern = pattern.trim_start_matches('/');
            glob_match(pattern, &rel) || glob_match(&format!("*/{pattern}"), &rel)
        };
        let included = |p: &String| if p.contains('/') { in_path(p) } else { glob_match(p, &name) };
        let excluded = |p: &String| if p.contains('/') { in_path(p) } else { rel.split('/').any(|part| glob_match(p, part)) };
        (self.include.is_empty() || self.include.iter().any(included)) && !self.exclude.iter().any(excluded)
    }

    /// Every file to search, from the folders and files given (not the
    /// open files, which the caller adds).
    pub fn files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for path in &self.paths {
            if path.is_file() {
                out.push(path.clone());
            } else if path.is_dir() {
                walk(path, path, self, &mut out);
            }
            if out.len() >= MAX_FILES {
                break;
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

fn expand_home(item: &str) -> PathBuf {
    match item.strip_prefix("~/").zip(dirs::home_dir()) {
        Some((rest, home)) => home.join(rest),
        None => PathBuf::from(item),
    }
}

fn walk(dir: &Path, base: &Path, scope: &Scope, out: &mut Vec<PathBuf>) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = read.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if crate::workspace::skipped(&name) {
                continue;
            }
            // Symlinked folders are skipped so a link loop can't recurse forever.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                if !scope.exclude.iter().any(|p| !p.contains('/') && glob_match(p, &name)) {
                    stack.push(path);
                }
            } else if scope.wants(&path, base) {
                out.push(path);
                if out.len() >= MAX_FILES {
                    return;
                }
            }
        }
    }
}

/// `*` matches any run of characters, `?` any one.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// The text of a file to search, if it is text: not too big, UTF-8, no NULs.
pub fn read_text(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok().map(|t| t.replace("\r\n", "\n"))
}

/// A line with matches: its number (from 1), its text, and the matches'
/// char ranges within it.
#[derive(Clone, Debug, PartialEq)]
pub struct LineMatch {
    pub line: usize,
    pub text: String,
    pub ranges: Vec<(usize, usize)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileMatches {
    pub path: PathBuf,
    pub lines: Vec<LineMatch>,
    pub count: usize,
}

/// The matches in one file's text, by line.
pub fn search_text(regex: &Regex, text: &str) -> Vec<LineMatch> {
    let mut out: Vec<LineMatch> = Vec::new();
    let line_starts: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
    for m in find_matches(regex, text) {
        let line = line_starts.partition_point(|&s| s <= m.start) - 1;
        let start = line_starts[line];
        let end = line_starts.get(line + 1).map_or(text.len(), |e| e - 1);
        // A match running past the line is shown up to its end.
        let col = |byte: usize| text[start..byte.min(end).max(start)].chars().count();
        let range = (col(m.start), col(m.end).max(col(m.start)));
        match out.last_mut() {
            Some(last) if last.line == line + 1 => last.ranges.push(range),
            _ => out.push(LineMatch { line: line + 1, text: text[start..end].to_owned(), ranges: vec![range] }),
        }
    }
    out
}

/// What a search running in the background reports.
pub enum Update {
    /// Files searched so far, of the total.
    Progress(usize, usize),
    Done(Vec<FileMatches>, usize),
}

/// Searches `files` (with `texts` standing in for files open with unsaved
/// changes) and sends updates, until `cancel` is set.
pub fn run(query: Query, files: Vec<PathBuf>, texts: HashMap<PathBuf, String>, cancel: Arc<AtomicBool>, send: Sender<Update>, wake: impl Fn()) {
    let Ok(regex) = query.compile() else {
        let _ = send.send(Update::Done(Vec::new(), 0));
        wake();
        return;
    };
    let total = files.len();
    let mut results = Vec::new();
    for (i, path) in files.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        if i % 200 == 0 && i > 0 {
            let _ = send.send(Update::Progress(i, total));
            wake();
        }
        let Some(text) = texts.get(&path).cloned().or_else(|| read_text(&path)) else {
            continue;
        };
        let lines = search_text(&regex, &text);
        if !lines.is_empty() {
            let count = lines.iter().map(|l| l.ranges.len()).sum();
            results.push(FileMatches { path, lines, count });
        }
    }
    let _ = send.send(Update::Done(results, total));
    wake();
}

/// The Find Results tab: its text, the char ranges of every match in it
/// (to highlight), and where each result line leads.
pub struct Report {
    pub text: String,
    pub highlights: Vec<(usize, usize)>,
    /// Line of the report (from 0) → file, line (from 1) and column.
    pub targets: HashMap<usize, (PathBuf, usize, usize)>,
}

pub fn report(query: &Query, results: &[FileMatches], searched: usize) -> Report {
    let mut text = format!("Searching {searched} file{} for {}\n", plural(searched), query.describe());
    let mut highlights = Vec::new();
    let mut targets = HashMap::new();
    let mut line = 1;
    let mut chars = text.chars().count();
    let push = |text: &mut String, piece: &str, line: &mut usize, chars: &mut usize| {
        text.push_str(piece);
        *line += piece.matches('\n').count();
        *chars += piece.chars().count();
    };
    let width = results.iter().flat_map(|f| f.lines.iter()).map(|l| l.line.to_string().len()).max().unwrap_or(1).max(4);
    for file in results {
        push(&mut text, &format!("\n{}:\n", file.path.display()), &mut line, &mut chars);
        for m in &file.lines {
            let prefix = format!("{:>width$}: ", m.line);
            let shown: String = m.text.chars().take(MAX_SHOWN_LINE_CHARS).collect();
            let shown_len = shown.chars().count();
            let start = chars + prefix.chars().count();
            for &(a, z) in &m.ranges {
                if a < shown_len {
                    highlights.push((start + a, start + z.min(shown_len)));
                }
            }
            targets.insert(line, (file.path.clone(), m.line, m.ranges[0].0));
            push(&mut text, &format!("{prefix}{shown}\n"), &mut line, &mut chars);
        }
    }
    let count: usize = results.iter().map(|f| f.count).sum();
    let summary = if count == 0 {
        "\n0 matches\n".to_owned()
    } else {
        format!("\n{count} match{} across {} file{}\n", if count == 1 { "" } else { "es" }, results.len(), plural(results.len()))
    };
    push(&mut text, &summary, &mut line, &mut chars);
    Report { text, highlights, targets }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(pattern: &str) -> Query {
        Query { pattern: pattern.into(), ..Query::default() }
    }

    #[test]
    fn finds_by_line_with_case_word_and_regex_options() {
        let text = "let foo = Foo;\nfoofoo\nno match\nfoo";
        let lines = search_text(&query("foo").compile().unwrap(), text);
        assert_eq!(lines.iter().map(|l| (l.line, l.ranges.clone())).collect::<Vec<_>>(), vec![
            (1, vec![(4, 7), (10, 13)]),
            (2, vec![(0, 3), (3, 6)]),
            (4, vec![(0, 3)]),
        ]);
        let exact = Query { case_sensitive: true, whole_word: true, ..query("foo") };
        assert_eq!(search_text(&exact.compile().unwrap(), text).len(), 2);
        let regex = Query { regex: true, ..query(r"f\w+") };
        assert_eq!(search_text(&regex.compile().unwrap(), "x fab")[0].ranges, vec![(2, 5)]);
        assert!(Query { regex: true, ..query("(") }.compile().is_err());
        assert!(query("a.b").compile().is_ok_and(|r| find_matches(&r, "axb").is_empty()), "literal by default");
    }

    #[test]
    fn replacements_use_groups() {
        let q = Query { regex: true, ..query(r"(\w+)=(\w+)") };
        let text = "a=1, bb=22";
        let regex = q.compile().unwrap();
        let out: Vec<String> = find_matches(&regex, text).iter().map(|m| expand_replacement("$2=${1}\\t$&", m, text, true)).collect();
        assert_eq!(out, vec!["1=a\ta=1", "22=bb\tbb=22"]);
        let m = &find_matches(&regex, text)[0];
        assert_eq!(expand_replacement("$1", m, text, false), "$1", "plain text when not a regex");
    }

    #[test]
    fn where_field_and_patterns() {
        let root = Path::new("/proj");
        let scope = Scope::parse("*.rs, -target/*, -*.min.js", Some(root));
        assert_eq!(scope.paths, vec![root.to_path_buf()]);
        assert!(scope.wants(Path::new("/proj/src/main.rs"), root));
        assert!(!scope.wants(Path::new("/proj/README.md"), root));
        assert!(!scope.wants(Path::new("/proj/target/x.rs"), root));
        let open = Scope::parse("<open files>", Some(root));
        assert!(open.open_files && open.paths.is_empty());
        assert!(Scope::parse("", None).open_files, "nothing else to search");
        assert!(glob_match("*.min.js", "app.min.js") && !glob_match("*.min.js", "app.js"));
        assert!(glob_match("a?c*", "abcdef"));
    }

    #[test]
    fn searches_folders_and_reports_results() {
        let dir = std::env::temp_dir().join(format!("text_editor_search_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn needle() {}\n").unwrap();
        std::fs::write(dir.join("b.txt"), "no\nneedle and needle\n").unwrap();
        std::fs::write(dir.join("node_modules/c.js"), "needle").unwrap();
        std::fs::write(dir.join("bin.dat"), b"needle\0").unwrap();
        let scope = Scope::parse("", Some(&dir));
        let files = scope.files();
        assert_eq!(files.len(), 3, "node_modules is skipped: {files:?}");

        let (send, receive) = std::sync::mpsc::channel();
        let unsaved = HashMap::from([(dir.join("src/a.rs"), "nothing here".to_owned())]);
        run(query("needle"), files, unsaved, Arc::new(AtomicBool::new(false)), send, || {});
        let Ok(Update::Done(results, searched)) = receive.recv() else { panic!() };
        assert_eq!(searched, 3);
        assert_eq!(results.len(), 1, "unsaved text wins over the file; binary files are skipped");
        let r = report(&query("needle"), &results, searched);
        assert!(r.text.contains("b.txt:\n   2: needle and needle\n"), "{}", r.text);
        assert!(r.text.ends_with("\n2 matches across 1 file\n"));
        let (line, target) = r.targets.iter().next().unwrap();
        assert_eq!(r.text.lines().nth(*line).unwrap(), "   2: needle and needle");
        assert_eq!(target.1, 2);
        let chars: Vec<char> = r.text.chars().collect();
        let (a, z) = r.highlights[1];
        assert_eq!(chars[a..z].iter().collect::<String>(), "needle");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

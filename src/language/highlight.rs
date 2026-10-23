use std::collections::BTreeMap;
use std::io::Cursor;
use std::ops::Range;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::Color32;
use ropey::Rope;
use syntect::highlighting::{
    FontStyle, HighlightIterator, HighlightState, Highlighter as ThemeHighlighter, Theme, ThemeSet,
};
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxDefinition, SyntaxSet};

use crate::packages::{Loaded, Resource};

/// Parser state is saved every this many lines, so an edit only re-parses
/// from the checkpoint above it.
const CHECKPOINT_EVERY: usize = 32;
/// Longer lines are shown uncolored: parsing them is slow and they are
/// usually minified or generated.
const MAX_HIGHLIGHT_LINE_BYTES: usize = 10_000;
/// Finding the scope at a point parses from the nearest checkpoint; past
/// this many lines it gives up rather than stall a key press.
const MAX_SCOPE_PARSE_LINES: usize = 2_000;
/// Cached lines kept around the visible area; the rest are dropped.
const MAX_CACHED_LINES: usize = 4_000;
pub const DEFAULT_THEME: &str = "base16-ocean.dark";

/// A colored run within one line, as byte offsets into the line's text.
#[derive(Clone, Debug)]
pub struct Span {
    pub range: Range<usize>,
    pub color: Color32,
    pub italic: bool,
    pub underline: bool,
}

/// Colors for everything around the text, taken from the theme.
#[derive(Clone, Copy)]
pub struct EditorColors {
    pub background: Color32,
    pub foreground: Color32,
    pub caret: Color32,
    pub selection: Color32,
    pub line_highlight: Color32,
    pub gutter_foreground: Color32,
    pub gutter_active: Color32,
    pub guide: Color32,
    pub find_highlight: Color32,
    pub dark: bool,
}

/// Syntax definitions and color themes, shared by all tabs.
pub struct Highlighter {
    syntaxes: SyntaxSet,
    themes: BTreeMap<String, Theme>,
    theme_name: String,
    colors: EditorColors,
    /// Bumped when the theme or the languages change so each tab's cached
    /// colors are redone.
    generation: u64,
    /// Package theme files that could not be read.
    theme_errors: Vec<String>,
    /// Package language files that could not be read.
    syntax_errors: Vec<String>,
}

impl Highlighter {
    /// The bundled themes plus those from `packages`, and the bundled
    /// languages. Package languages take longer to prepare and are added
    /// with [`Highlighter::set_syntaxes`].
    pub fn new(theme: &str, packages: &Loaded) -> Self {
        let (themes, theme_errors) = load_themes(&packages.themes);
        let mut highlighter = Self {
            syntaxes: two_face::syntax::extra_newlines(),
            theme_errors,
            syntax_errors: Vec::new(),
            colors: colors_of(&themes[DEFAULT_THEME]),
            themes,
            theme_name: DEFAULT_THEME.to_owned(),
            generation: 0,
        };
        highlighter.set_theme(theme);
        highlighter
    }

    /// Swaps in the themes of a new set of packages, e.g. after one was
    /// installed. Keeps the current theme if it still exists.
    pub fn reload_themes(&mut self, packages: &Loaded) {
        let (themes, theme_errors) = load_themes(&packages.themes);
        self.themes = themes;
        self.theme_errors = theme_errors;
        if !self.themes.contains_key(&self.theme_name) {
            self.theme_name = DEFAULT_THEME.to_owned();
        }
        // The theme's file may have changed even if its name didn't.
        self.colors = colors_of(&self.themes[&self.theme_name]);
        self.generation += 1;
    }

    /// Swaps in a new set of languages; every tab is colored again.
    pub fn set_syntaxes(&mut self, syntaxes: Syntaxes) {
        self.syntaxes = syntaxes.set;
        self.syntax_errors = syntaxes.errors;
        self.generation += 1;
    }

    /// Package files that could not be read as a theme or language.
    #[cfg(test)]
    pub fn load_errors(&self) -> Vec<String> {
        self.theme_errors.iter().chain(&self.syntax_errors).cloned().collect()
    }

    pub fn theme_errors(&self) -> &[String] {
        &self.theme_errors
    }

    pub fn syntax_errors(&self) -> &[String] {
        &self.syntax_errors
    }

    /// The scope a language gives all of its text, e.g. `source.rust`.
    pub fn base_scope(&self, syntax: &str) -> Option<Scope> {
        self.syntaxes.find_syntax_by_name(syntax).map(|s| s.scope)
    }

    /// The scopes of the character at char `col` of `line`, outermost
    /// first, e.g. `source.rust string.quoted.double`. Parses from the
    /// nearest checkpoint in `cache`; `None` if that is too far to be quick.
    pub fn scopes_at(&self, cache: &LineCache, rope: &Rope, line: usize, col: usize) -> Option<Vec<Scope>> {
        let syntax = self.syntaxes.find_syntax_by_name(&cache.syntax)?;
        // Checkpoints only count if made with the current syntax set.
        let usable = cache.syntax_used == cache.syntax && cache.generation == self.generation;
        let (from, mut parse, mut stack) = match usable.then(|| cache.checkpoints.len().checked_sub(1)).flatten() {
            Some(last) => {
                let k = (line / CHECKPOINT_EVERY).min(last);
                let (parse, highlight) = &cache.checkpoints[k];
                (k * CHECKPOINT_EVERY, parse.clone(), highlight.path.clone())
            }
            None => (0, ParseState::new(syntax), ScopeStack::new()),
        };
        if line - from > MAX_SCOPE_PARSE_LINES {
            return None;
        }
        for l in from..=line {
            let text = line_string(rope, l);
            // Skipped when coloring too, so the checkpoints agree.
            if text.len() > MAX_HIGHLIGHT_LINE_BYTES {
                continue;
            }
            let ops = parse.parse_line(&text, &self.syntaxes).ok()?;
            let end = if l == line { text.char_indices().nth(col).map_or(text.len(), |(b, _)| b) } else { usize::MAX };
            for (at, op) in ops {
                if at > end {
                    break;
                }
                stack.apply(&op).ok()?;
            }
        }
        Some(stack.as_slice().to_vec())
    }

    pub fn theme_names(&self) -> Vec<String> {
        self.themes.keys().cloned().collect()
    }

    pub fn theme_name(&self) -> &str {
        &self.theme_name
    }

    /// Switches theme; returns false (and keeps the current one) if unknown.
    pub fn set_theme(&mut self, name: &str) -> bool {
        let Some(theme) = self.themes.get(name) else {
            return false;
        };
        if name != self.theme_name {
            self.colors = colors_of(theme);
            self.theme_name = name.to_owned();
            self.generation += 1;
        }
        true
    }

    pub fn colors(&self) -> &EditorColors {
        &self.colors
    }

    /// The color the theme gives a scope like `invalid` or `region.redish`,
    /// for plugins' regions and `style_for_scope`: its text color, else its
    /// background. `None` if the theme has nothing for it.
    pub fn scope_color(&self, scope: &str) -> Option<Color32> {
        let scopes: Vec<Scope> = scope.split_whitespace().filter_map(|s| Scope::new(s).ok()).collect();
        if scopes.is_empty() {
            return None;
        }
        let theme = &self.themes[&self.theme_name];
        let style = ThemeHighlighter::new(theme).style_mod_for_stack(&scopes);
        style.foreground.or(style.background).map(to_color32)
    }

    /// Picks a syntax from the file extension, falling back to the first line
    /// (e.g. a `#!/bin/bash` shebang), then plain text.
    pub fn detect_syntax(&self, path: Option<&Path>, first_line: &str) -> String {
        let by_ext = path.and_then(|p| {
            let name = p.file_name()?.to_string_lossy();
            let ext = p.extension().map(|e| e.to_string_lossy());
            // Some syntaxes match whole file names, e.g. "Makefile".
            ext.and_then(|e| self.syntaxes.find_syntax_by_extension(&e))
                .or_else(|| self.syntaxes.find_syntax_by_extension(&name))
        });
        by_ext
            .or_else(|| self.syntaxes.find_syntax_by_first_line(first_line))
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text())
            .name
            .clone()
    }

    /// Names for the "Set Syntax" commands, alphabetical.
    pub fn syntax_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .syntaxes
            .syntaxes()
            .iter()
            .filter(|s| !s.hidden)
            .map(|s| s.name.clone())
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        // A package language that replaces a bundled one is listed once.
        names.dedup();
        names
    }

    /// Makes sure lines `first..=last` have colors in `cache`, spending at most
    /// `budget`. Returns false if it ran out of time; call again next frame.
    pub fn prepare(&self, cache: &mut LineCache, rope: &Rope, first: usize, last: usize, budget: Duration) -> bool {
        if cache.syntax_used != cache.syntax || cache.generation != self.generation {
            cache.checkpoints.clear();
            cache.lines.clear();
            cache.syntax_used = cache.syntax.clone();
            cache.generation = self.generation;
        }
        let Some(syntax) = self.syntaxes.find_syntax_by_name(&cache.syntax) else {
            return true;
        };
        if syntax.name == "Plain Text" {
            return true;
        }
        let line_count = rope.len_lines();
        let last = last.min(line_count.saturating_sub(1));
        if (first..=last).all(|l| cache.lines.contains_key(&l)) {
            return true;
        }

        let theme = ThemeHighlighter::new(&self.themes[&self.theme_name]);
        if cache.checkpoints.is_empty() {
            cache
                .checkpoints
                .push((ParseState::new(syntax), HighlightState::new(&theme, ScopeStack::new())));
        }

        // Parse forward to the checkpoint just above the visible lines.
        let started = Instant::now();
        let target = first / CHECKPOINT_EVERY;
        while cache.checkpoints.len() <= target {
            let k = cache.checkpoints.len() - 1;
            let mut state = cache.checkpoints[k].clone();
            let end = ((k + 1) * CHECKPOINT_EVERY).min(line_count);
            for line in k * CHECKPOINT_EVERY..end {
                self.highlight_line(&theme, &mut state, &line_string(rope, line));
            }
            cache.checkpoints.push(state);
            if started.elapsed() > budget {
                return false;
            }
        }

        // Color the visible lines, saving checkpoints passed on the way.
        let mut state = cache.checkpoints[target].clone();
        for line in target * CHECKPOINT_EVERY..=last {
            let spans = self.highlight_line(&theme, &mut state, &line_string(rope, line));
            if line >= first {
                cache.lines.insert(line, spans.into());
            }
            let next = line + 1;
            if next % CHECKPOINT_EVERY == 0 && next / CHECKPOINT_EVERY == cache.checkpoints.len() {
                cache.checkpoints.push(state.clone());
            }
        }
        if cache.lines.len() > MAX_CACHED_LINES {
            let keep = first.saturating_sub(500)..last + 500;
            cache.lines.retain(|l, _| keep.contains(l));
        }
        true
    }

    fn highlight_line(
        &self,
        theme: &ThemeHighlighter<'_>,
        state: &mut (ParseState, HighlightState),
        line: &str,
    ) -> Vec<Span> {
        let text_len = line.trim_end_matches('\n').len();
        let plain = || {
            vec![Span { range: 0..text_len, color: self.colors.foreground, italic: false, underline: false }]
        };
        if line.len() > MAX_HIGHLIGHT_LINE_BYTES {
            return plain();
        }
        let Ok(ops) = state.0.parse_line(line, &self.syntaxes) else {
            return plain();
        };
        let mut spans = Vec::new();
        let mut at = 0;
        for (style, piece) in HighlightIterator::new(&mut state.1, &ops, line, theme) {
            let end = (at + piece.len()).min(text_len);
            if end > at {
                spans.push(Span {
                    range: at..end,
                    color: to_color32(style.foreground),
                    italic: style.font_style.contains(FontStyle::ITALIC),
                    underline: style.font_style.contains(FontStyle::UNDERLINE),
                });
            }
            at += piece.len();
        }
        spans
    }
}

/// One tab's highlighting progress: parser checkpoints and colored lines.
pub struct LineCache {
    /// The syntax this tab should use; change it to re-highlight.
    pub syntax: String,
    syntax_used: String,
    generation: u64,
    checkpoints: Vec<(ParseState, HighlightState)>,
    lines: BTreeMap<usize, Arc<[Span]>>,
}

impl LineCache {
    pub fn new(syntax: String) -> Self {
        Self {
            syntax,
            syntax_used: String::new(),
            generation: 0,
            checkpoints: Vec::new(),
            lines: BTreeMap::new(),
        }
    }

    /// Forgets everything from `line` down, after an edit there.
    pub fn invalidate_from(&mut self, line: usize) {
        self.checkpoints.truncate(line / CHECKPOINT_EVERY + 1);
        self.lines.split_off(&line);
    }

    pub fn spans(&self, line: usize) -> Option<Arc<[Span]>> {
        self.lines.get(&line).cloned()
    }
}

/// The bundled themes, then those from packages. A package theme with the
/// same name as an earlier one replaces it. Also returns the files that
/// could not be read.
fn load_themes(files: &[Resource]) -> (BTreeMap<String, Theme>, Vec<String>) {
    let mut errors = Vec::new();
    let mut themes: BTreeMap<String, Theme> = ThemeSet::load_defaults().themes.into_iter().collect();
    for file in files {
        let theme = if crate::packages::is_kind(&file.path, "color-scheme") {
            file.text().and_then(crate::language::color_scheme::parse)
        } else {
            ThemeSet::load_from_reader(&mut Cursor::new(&file.data)).map_err(|e| e.to_string())
        };
        match theme {
            Ok(theme) => {
                themes.insert(file.stem().to_owned(), theme);
            }
            Err(e) => errors.push(format!("{}: {e}", file.label())),
        }
    }
    (themes, errors)
}

/// Languages: the bundled ones (`two-face`'s, collected by `bat`)
/// (TypeScript, TOML, Swift, Kotlin, Dockerfile…), then those from packages,
/// with the package files that could not be read.
pub struct Syntaxes {
    set: SyntaxSet,
    pub errors: Vec<String>,
}

impl Syntaxes {
    /// Builds the set. With package languages this is slow (every bundled
    /// language is linked again), so callers use [`Syntaxes::cached`] first
    /// and do this off the main thread.
    pub fn build(files: &[Resource]) -> Self {
        let bundled = two_face::syntax::extra_newlines();
        if files.is_empty() {
            return Self { set: bundled, errors: Vec::new() };
        }
        let mut errors = Vec::new();
        let mut builder = bundled.into_builder();
        for file in files {
            let parsed = file.text().and_then(|text| {
                SyntaxDefinition::load_from_str(text, true, Some(file.stem())).map_err(|e| e.to_string())
            });
            match parsed {
                // Lookups search newest first, so this wins over a bundled namesake.
                Ok(syntax) => builder.add(syntax),
                Err(e) => errors.push(format!("{}: {e}", file.label())),
            }
        }
        Self { set: builder.build(), errors }
    }

    /// A set built earlier from exactly these files, if one was saved in `dir`.
    pub fn cached(files: &[Resource], dir: &Path) -> Option<Self> {
        let (set, errors) = syntect::dumps::from_dump_file(cache_path(files, dir)).ok()?;
        Some(Self { set, errors })
    }

    /// [`Syntaxes::build`], saving the result in `dir` for next time and
    /// removing sets saved for other files.
    pub fn build_and_cache(files: &[Resource], dir: &Path) -> Self {
        let built = Self::build(files);
        let path = cache_path(files, dir);
        if let Ok(old) = std::fs::read_dir(dir) {
            for entry in old.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with(CACHE_PREFIX) && entry.path() != path {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        let tmp = path.with_extension("tmp");
        let saved = std::fs::create_dir_all(dir)
            .map_err(|e| e.to_string())
            .and_then(|_| syntect::dumps::dump_to_file(&(&built.set, &built.errors), &tmp).map_err(|e| e.to_string()))
            .and_then(|_| std::fs::rename(&tmp, &path).map_err(|e| e.to_string()));
        if saved.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        built
    }
}

const CACHE_PREFIX: &str = "syntaxes-";

impl Syntaxes {
    /// Identifies a set of package languages: equal keys build equal sets.
    /// Includes the app version, which fixes the bundled languages.
    pub fn key(files: &[Resource]) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        env!("CARGO_PKG_VERSION").hash(&mut h);
        for file in files {
            (&file.package, &file.path, &file.data).hash(&mut h);
        }
        h.finish()
    }
}

/// Named after [`Syntaxes::key`], so any change makes a new file.
fn cache_path(files: &[Resource], dir: &Path) -> std::path::PathBuf {
    dir.join(format!("{CACHE_PREFIX}{:016x}.packdump", Syntaxes::key(files)))
}

fn line_string(rope: &Rope, line: usize) -> String {
    rope.line(line).to_string()
}

fn colors_of(theme: &Theme) -> EditorColors {
    let s = &theme.settings;
    let background = s.background.map(to_color32).unwrap_or(Color32::from_gray(30));
    let foreground = s.foreground.map(to_color32).unwrap_or(Color32::from_gray(210));
    let mix = |t: f32| blend(background, foreground, t);
    EditorColors {
        background,
        foreground,
        caret: s.caret.map(to_color32).unwrap_or(foreground),
        selection: s.selection.map(to_color32).unwrap_or(mix(0.25)),
        line_highlight: s.line_highlight.map(to_color32).unwrap_or(mix(0.06)),
        gutter_foreground: s.gutter_foreground.map(to_color32).unwrap_or(mix(0.35)),
        gutter_active: mix(0.8),
        guide: s.guide.map(to_color32).unwrap_or(mix(0.12)),
        find_highlight: Color32::from_rgba_unmultiplied(255, 200, 0, 70),
        dark: luminance(background) < 0.5,
    }
}

pub fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()))
}

fn luminance(c: Color32) -> f32 {
    (0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32) / 255.0
}

fn to_color32(c: syntect::highlighting::Color) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONG: Duration = Duration::from_secs(30);

    #[test]
    fn spans_cover_each_line_without_the_newline() {
        let h = Highlighter::new(DEFAULT_THEME, &Loaded::default());
        let rope = Rope::from_str("fn main() {\n    let x = 1;\n}\n");
        let mut cache = LineCache::new("Rust".into());
        assert!(h.prepare(&mut cache, &rope, 0, 3, LONG));
        let spans = cache.spans(1).unwrap();
        assert_eq!(spans.first().unwrap().range.start, 0);
        assert_eq!(spans.last().unwrap().range.end, "    let x = 1;".len());
        // Keywords and numbers get different colors.
        let colors: std::collections::HashSet<_> = spans.iter().map(|s| s.color).collect();
        assert!(colors.len() > 1);
    }

    #[test]
    fn far_lines_are_reached_through_checkpoints() {
        let h = Highlighter::new(DEFAULT_THEME, &Loaded::default());
        let text = "/* start\n".to_owned() + &"inside comment\n".repeat(200) + "*/ fn x() {}\n";
        let rope = Rope::from_str(&text);
        let mut cache = LineCache::new("Rust".into());
        // Only ask for line 150: it must still know it is inside the comment.
        assert!(h.prepare(&mut cache, &rope, 150, 150, LONG));
        let comment_color = cache.spans(150).unwrap()[0].color;
        assert!(h.prepare(&mut cache, &rope, 201, 201, LONG));
        assert!(cache.spans(201).unwrap().iter().any(|s| s.color != comment_color));

        // Turning the block comment into a line comment recolors line 150.
        cache.invalidate_from(0);
        let edited = Rope::from_str(&text.replacen("/* start", "// start", 1));
        assert!(h.prepare(&mut cache, &edited, 150, 150, LONG));
        assert_ne!(cache.spans(150).unwrap()[0].color, comment_color);
    }

    #[test]
    fn extra_languages_are_detected_by_extension() {
        let h = Highlighter::new(DEFAULT_THEME, &Loaded::default());
        let detect = |name: &str| h.detect_syntax(Some(Path::new(name)), "");
        assert_eq!(detect("app.ts"), "TypeScript");
        assert_eq!(detect("view.tsx"), "TypeScriptReact");
        assert_eq!(detect("Cargo.toml"), "TOML");
        assert_eq!(detect("main.swift"), "Swift");
        assert_eq!(detect("Main.kt"), "Kotlin");
        assert_eq!(detect("Dockerfile"), "Dockerfile");
        assert_eq!(detect("style.scss"), "SCSS");
        assert_eq!(detect("App.vue"), "Vue Component");
        assert_eq!(detect("main.zig"), "Zig");
        assert_eq!(detect("schema.graphql"), "GraphQL");
        assert_eq!(detect("main.tf"), "Terraform");
        assert_eq!(detect("flake.nix"), "Nix");
        assert_eq!(detect("main.rs"), "Rust");
        assert_eq!(detect("notes.unknownext"), "Plain Text");
    }

    fn package_file(path: &str, text: &str) -> Resource {
        Resource { package: "Test".into(), path: path.into(), data: text.into() }
    }

    #[test]
    fn package_themes_and_languages_load_and_reload() {
        use crate::packages::tests::{SYNTAX, THEME};
        let file = package_file;
        let packages = Loaded {
            themes: vec![file("Night.tmTheme", THEME), file("Broken.tmTheme", "<plist")],
            // Same name as a bundled language, so it replaces it.
            syntaxes: vec![file("Fancy.sublime-syntax", SYNTAX), file("Rust.sublime-syntax", &SYNTAX.replace("Fancy", "Rust").replace("fancy", "rs"))],
            ..Loaded::default()
        };
        let mut h = Highlighter::new("Night", &packages);
        h.set_syntaxes(Syntaxes::build(&packages.syntaxes));
        assert_eq!(h.theme_name(), "Night");
        assert_eq!(h.colors().background, Color32::from_rgb(0x10, 0x20, 0x30));
        assert_eq!(h.load_errors().len(), 1);
        assert!(h.load_errors()[0].starts_with("Test/Broken.tmTheme"), "{}", h.load_errors()[0]);
        assert_eq!(h.detect_syntax(Some(Path::new("a.fancy")), ""), "Fancy");
        assert_eq!(h.syntax_names().iter().filter(|n| *n == "Rust").count(), 1);
        assert_eq!(h.base_scope("Fancy").map(|s| s.build_string()).as_deref(), Some("source.fancy"));
        let mut cache = LineCache::new("Rust".into());
        h.prepare(&mut cache, &Rope::from_str("fn fancy\n"), 0, 0, LONG);
        let generation = cache.generation;

        // Without the package, its theme and language are gone and tabs re-color.
        h.reload_themes(&Loaded::default());
        h.set_syntaxes(Syntaxes::build(&[]));
        assert_eq!(h.theme_name(), DEFAULT_THEME);
        assert_eq!(h.detect_syntax(Some(Path::new("a.fancy")), ""), "Plain Text");
        assert!(h.load_errors().is_empty());
        h.prepare(&mut cache, &Rope::from_str("fn fancy\n"), 0, 0, LONG);
        assert_ne!(cache.generation, generation);
    }

    #[test]
    fn scopes_at_a_point_include_the_innermost() {
        let h = Highlighter::new(DEFAULT_THEME, &Loaded::default());
        let text = "fn main() {\n".to_owned() + &"    let a = 1;\n".repeat(100) + "    let s = \"hi\";\n}\n";
        let rope = Rope::from_str(&text);
        let mut cache = LineCache::new("Rust".into());
        let names = |scopes: Option<Vec<Scope>>| scopes.unwrap().iter().map(|s| s.build_string()).collect::<Vec<_>>().join(" ");
        // Without checkpoints it parses from the top; with them, from nearby.
        let in_string = names(h.scopes_at(&cache, &rope, 101, 13));
        assert!(in_string.starts_with("source.rust") && in_string.contains("string.quoted"), "{in_string}");
        h.prepare(&mut cache, &rope, 0, 102, LONG);
        assert_eq!(names(h.scopes_at(&cache, &rope, 101, 13)), in_string);
        assert!(!names(h.scopes_at(&cache, &rope, 101, 4)).contains("string"));
    }

    #[test]
    fn color_schemes_are_themes() {
        let scheme = r##"{ "globals": { "background": "#204060", "foreground": "white" }, "rules": [] }"##;
        let packages = Loaded { themes: vec![package_file("schemes/Sea.sublime-color-scheme", scheme)], ..Loaded::default() };
        let h = Highlighter::new("Sea", &packages);
        assert_eq!(h.theme_name(), "Sea");
        assert_eq!(h.colors().background, Color32::from_rgb(0x20, 0x40, 0x60));
    }

    #[test]
    fn package_languages_are_cached_by_content() {
        use crate::packages::tests::{SYNTAX, temp_root};
        let dir = temp_root("syntax_cache");
        let files = vec![package_file("Fancy.sublime-syntax", SYNTAX), package_file("Bad.sublime-syntax", "nope: [")];
        assert!(Syntaxes::cached(&files, &dir).is_none());
        let built = Syntaxes::build_and_cache(&files, &dir);
        assert_eq!(built.errors.len(), 1);

        let cached = Syntaxes::cached(&files, &dir).expect("saved");
        assert_eq!(cached.errors, built.errors);
        assert!(cached.set.find_syntax_by_name("Fancy").is_some());

        // Different files miss the cache, and building for them replaces the old file.
        let changed = vec![package_file("Fancy.sublime-syntax", &SYNTAX.replace("fancy", "fcy"))];
        assert!(Syntaxes::cached(&changed, &dir).is_none());
        Syntaxes::build_and_cache(&changed, &dir);
        assert!(Syntaxes::cached(&files, &dir).is_none());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unknown_theme_is_rejected_and_bundled_ones_listed() {
        let mut h = Highlighter::new("no such theme", &Loaded::default());
        assert_eq!(h.theme_name(), DEFAULT_THEME);
        assert!(h.theme_names().iter().any(|n| n == "Solarized (light)"));
        assert!(h.set_theme("Solarized (light)"));
        assert!(!h.colors().dark);
    }
}

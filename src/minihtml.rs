//! minihtml, as plugins use for popups and phantoms: enough HTML
//! to show text with bold, italics, code, colors, links and line breaks.
//! Anything else is shown as its text.

use eframe::egui::Color32;

/// A piece of text in one style.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub color: Option<Color32>,
    /// Where clicking it goes; plugins get this in `on_navigate`.
    pub link: Option<String>,
}

/// The text as lines of runs.
pub type Lines = Vec<Vec<Run>>;

/// Lays out `html` as lines of styled runs.
pub fn parse(html: &str) -> Lines {
    let mut lines: Lines = vec![Vec::new()];
    // The style in effect, one entry per open tag.
    let mut stack: Vec<(String, Run)> = Vec::new();
    let mut pre = 0usize;
    let mut rest = html;
    let new_line = |lines: &mut Lines| {
        if lines.last().is_some_and(|l| !l.is_empty()) {
            lines.push(Vec::new());
        }
    };
    while !rest.is_empty() {
        let Some(open) = rest.find('<') else {
            push_text(&mut lines, &stack, rest, pre > 0);
            break;
        };
        push_text(&mut lines, &stack, &rest[..open], pre > 0);
        rest = &rest[open..];
        // A comment, or a style or script block, shows nothing.
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |i| &after[i + 3..]);
            continue;
        }
        let Some(close) = rest.find('>') else {
            push_text(&mut lines, &stack, rest, pre > 0);
            break;
        };
        let tag = &rest[1..close];
        rest = &rest[close + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
        if !closing && (name == "style" || name == "script") {
            let end = format!("</{name}");
            rest = rest.to_ascii_lowercase().find(&end).map_or("", |i| &rest[i..]);
            continue;
        }
        let block = matches!(name.as_str(), "p" | "div" | "br" | "li" | "ul" | "ol" | "pre" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "tr" | "blockquote");
        if block {
            new_line(&mut lines);
        }
        if closing {
            if let Some(at) = stack.iter().rposition(|(n, _)| *n == name) {
                stack.truncate(at);
            }
            if name == "pre" {
                pre = pre.saturating_sub(1);
            }
            continue;
        }
        if name == "br" || tag.ends_with('/') {
            continue;
        }
        let mut style = stack.last().map(|(_, s)| s.clone()).unwrap_or_default();
        match name.as_str() {
            "b" | "strong" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => style.bold = true,
            "i" | "em" => style.italic = true,
            "code" | "tt" | "pre" => style.code = true,
            "a" => style.link = Some(attribute(tag, "href").unwrap_or_default()),
            "li" => push_text(&mut lines, &stack, "• ", true),
            _ => {}
        }
        if name == "pre" {
            pre += 1;
        }
        if let Some(color) = attribute(tag, "style").and_then(|s| style_color(&s)).or_else(|| attribute(tag, "color").and_then(|c| hex_color(&c))) {
            style.color = Some(color);
        }
        stack.push((name, style));
    }
    while lines.len() > 1 && lines.last().is_some_and(Vec::is_empty) {
        lines.pop();
    }
    lines
}

/// Adds text in the current style. Outside `<pre>`, runs of white space
/// become one space, and newlines don't break lines.
fn push_text(lines: &mut Lines, stack: &[(String, Run)], text: &str, pre: bool) {
    let text = decode_entities(text);
    let style = stack.last().map(|(_, s)| s.clone()).unwrap_or_default();
    let pieces: Vec<String> = if pre {
        text.split('\n').map(str::to_owned).collect()
    } else {
        let mut collapsed = String::new();
        for c in text.chars() {
            if c.is_whitespace() {
                if !collapsed.ends_with(' ') {
                    collapsed.push(' ');
                }
            } else {
                collapsed.push(c);
            }
        }
        vec![collapsed]
    };
    for (i, piece) in pieces.into_iter().enumerate() {
        if i > 0 {
            lines.push(Vec::new());
        }
        let line = lines.last_mut().unwrap();
        // No space at the start of a line.
        let piece = if !pre && line.is_empty() { piece.trim_start().to_owned() } else { piece };
        if piece.is_empty() {
            continue;
        }
        match line.last_mut() {
            Some(last) if Run { text: String::new(), ..last.clone() } == Run { text: String::new(), ..style.clone() } => last.text.push_str(&piece),
            _ => line.push(Run { text: piece, ..style.clone() }),
        }
    }
}

/// The value of `name="value"` (or single-quoted, or bare) in a tag.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        let before_ok = at > 0 && lower.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = lower[from..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let value_at = tag.len() - rest.len() + 1;
        let value = tag[value_at..].trim_start();
        return Some(match value.chars().next() {
            Some(q @ ('"' | '\'')) => value[1..].split(q).next().unwrap_or_default().to_owned(),
            _ => value.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or_default().to_owned(),
        });
    }
    None
}

/// `color: #rrggbb` in a style attribute.
fn style_color(style: &str) -> Option<Color32> {
    style.split(';').find_map(|decl| {
        let (key, value) = decl.split_once(':')?;
        (key.trim().eq_ignore_ascii_case("color")).then(|| hex_color(value.trim())).flatten()
    })
}

fn hex_color(text: &str) -> Option<Color32> {
    let hex = text.strip_prefix('#')?;
    let digit = |i: usize, n: usize| u8::from_str_radix(hex.get(i..i + n)?, 16).ok();
    match hex.len() {
        3 => Some(Color32::from_rgb(digit(0, 1)? * 17, digit(1, 1)? * 17, digit(2, 1)? * 17)),
        6 | 8 => Some(Color32::from_rgb(digit(0, 2)?, digit(2, 2)?, digit(4, 2)?)),
        _ => None,
    }
}

fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let entity = rest.find(';').filter(|&end| end <= 10).map(|end| (&rest[1..end], end));
        let decoded = entity.and_then(|(name, end)| {
            let c = match name {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                "nbsp" => '\u{a0}',
                _ => {
                    let number = name.strip_prefix('#')?;
                    let code = match number.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => number.parse().ok()?,
                    };
                    char::from_u32(code)?
                }
            };
            Some((c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// All of the text on one line.
#[cfg(test)]
pub fn plain_text(lines: &Lines) -> String {
    lines.iter().map(|l| l.iter().map(|r| r.text.as_str()).collect::<String>()).collect::<Vec<_>>().join("  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &Lines) -> Vec<String> {
        lines.iter().map(|l| l.iter().map(|r| r.text.as_str()).collect()).collect()
    }

    #[test]
    fn styles_links_and_lines() {
        let lines = parse(
            "<body id=x><style>a { color: red }</style><b>Error:</b>  missing   <code>;</code><br>\
             <a href='subl:go 3'>Go &amp; fix</a> <span style=\"color: #ff0000\">now</span></body>",
        );
        assert_eq!(texts(&lines), vec!["Error: missing ;", "Go & fix now"]);
        assert!(lines[0][0].bold && lines[0][0].text == "Error:");
        assert!(lines[0][2].code);
        assert_eq!(lines[1][0].link.as_deref(), Some("subl:go 3"));
        assert_eq!(lines[1][2].color, Some(Color32::from_rgb(255, 0, 0)));
        assert_eq!(plain_text(&lines), "Error: missing ;  Go & fix now");
    }

    #[test]
    fn keeps_preformatted_text_and_lists() {
        let lines = parse("<pre>a  b\nc</pre><ul><li>one</li><li>two &#x263A;</li></ul>");
        assert_eq!(texts(&lines), vec!["a  b", "c", "• one", "• two ☺"]);
        assert_eq!(texts(&parse("plain &lt;text&gt; &bogus")), vec!["plain <text> &bogus"]);
    }
}

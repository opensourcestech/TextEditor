//! Sublime Text's `.sublime-color-scheme` format: JSON with variables,
//! global colors and scope rules, and CSS-like colors (`#hex`, `rgb()`,
//! `hsl()`, named colors, and `color(var(x) alpha(0.5))` adjustments).
//! Converted into the same theme type that `.tmTheme` files load into.

use std::collections::HashMap;
use std::str::FromStr;

use serde_json::Value;
use syntect::highlighting::{Color, FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings};

/// Variables may refer to other variables; this deep is surely a loop.
const MAX_VAR_DEPTH: usize = 16;

/// Reads a color scheme. Rules whose scope or color can't be understood are
/// skipped rather than failing the whole scheme.
pub fn parse(text: &str) -> Result<Theme, String> {
    let json = crate::packages::parse_json(text)?;
    let vars: HashMap<String, String> = json
        .get("variables")
        .and_then(Value::as_object)
        .map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect())
        .unwrap_or_default();
    let color = |v: &Value| v.as_str().and_then(|s| parse_color(s, &vars, 0));

    let mut settings = ThemeSettings::default();
    if let Some(globals) = json.get("globals").and_then(Value::as_object) {
        for (key, value) in globals {
            if let Some(slot) = global_slot(&mut settings, key) {
                *slot = color(value);
            }
        }
    }
    if settings.background.is_none() || settings.foreground.is_none() {
        return Err("needs \"background\" and \"foreground\" colors in \"globals\"".into());
    }

    let mut scopes = Vec::new();
    for rule in json.get("rules").and_then(Value::as_array).into_iter().flatten() {
        let Some(scope) = rule.get("scope").and_then(Value::as_str) else {
            continue;
        };
        let Ok(scope) = ScopeSelectors::from_str(scope) else {
            continue;
        };
        // An array of colors is a gradient for "hashed" highlighting; use the first.
        let first = |v: &Value| v.as_array().and_then(|a| a.first()).cloned().unwrap_or_else(|| v.clone());
        let foreground = rule.get("foreground").map(first).as_ref().and_then(color);
        let background = rule.get("background").map(first).as_ref().and_then(color);
        let font_style = rule.get("font_style").and_then(Value::as_str).map(|s| {
            let mut style = FontStyle::empty();
            for word in s.split_whitespace() {
                match word {
                    "bold" => style |= FontStyle::BOLD,
                    "italic" => style |= FontStyle::ITALIC,
                    "underline" => style |= FontStyle::UNDERLINE,
                    _ => {}
                }
            }
            style
        });
        scopes.push(ThemeItem { scope, style: StyleModifier { foreground, background, font_style } });
    }

    Ok(Theme {
        name: json.get("name").and_then(Value::as_str).map(str::to_owned),
        author: json.get("author").and_then(Value::as_str).map(str::to_owned),
        settings,
        scopes,
    })
}

/// The theme setting a color scheme's global sets, by its name in the file.
fn global_slot<'a>(s: &'a mut ThemeSettings, key: &str) -> Option<&'a mut Option<Color>> {
    Some(match key {
        "foreground" => &mut s.foreground,
        "background" => &mut s.background,
        "caret" => &mut s.caret,
        "line_highlight" => &mut s.line_highlight,
        "misspelling" => &mut s.misspelling,
        "accent" => &mut s.accent,
        "bracket_contents_foreground" => &mut s.bracket_contents_foreground,
        "brackets_foreground" => &mut s.brackets_foreground,
        "brackets_background" => &mut s.brackets_background,
        "tags_foreground" => &mut s.tags_foreground,
        "highlight" => &mut s.highlight,
        "find_highlight" => &mut s.find_highlight,
        "find_highlight_foreground" => &mut s.find_highlight_foreground,
        "gutter" => &mut s.gutter,
        "gutter_foreground" => &mut s.gutter_foreground,
        "selection" => &mut s.selection,
        "selection_foreground" => &mut s.selection_foreground,
        "selection_border" => &mut s.selection_border,
        "inactive_selection" => &mut s.inactive_selection,
        "inactive_selection_foreground" => &mut s.inactive_selection_foreground,
        "guide" => &mut s.guide,
        "active_guide" => &mut s.active_guide,
        "stack_guide" => &mut s.stack_guide,
        "shadow" => &mut s.shadow,
        _ => return None,
    })
}

/// A CSS-like color, resolving `var(name)` through `vars`.
pub fn parse_color(text: &str, vars: &HashMap<String, String>, depth: usize) -> Option<Color> {
    let text = text.trim();
    if depth > MAX_VAR_DEPTH {
        return None;
    }
    if let Some(hex) = text.strip_prefix('#') {
        return parse_hex(hex);
    }
    let Some((func, args)) = call(text) else {
        return named(text);
    };
    match func {
        "var" => parse_color(vars.get(args.trim())?, vars, depth + 1),
        "rgb" | "rgba" => {
            let n = numbers(args)?;
            let channel = |i: usize| -> Option<u8> {
                let (v, percent) = n.get(i).copied()?;
                Some(if percent { v * 2.55 } else { v }.round().clamp(0.0, 255.0) as u8)
            };
            Some(Color { r: channel(0)?, g: channel(1)?, b: channel(2)?, a: alpha(n.get(3).copied()) })
        }
        "hsl" | "hsla" => {
            let n = numbers(args)?;
            let (h, s, l) = (n.first()?.0, n.get(1)?.0 / 100.0, n.get(2)?.0 / 100.0);
            let (r, g, b) = hsl_to_rgb(h, s, l);
            Some(Color { r, g, b, a: alpha(n.get(3).copied()) })
        }
        "color" => {
            let (base, rest) = split_first(args);
            let mut c = parse_color(base, vars, depth + 1)?;
            let mut rest = rest;
            while let Some((adjuster, after)) = next_call(rest) {
                c = adjust(c, adjuster, vars, depth)?;
                rest = after;
            }
            Some(c)
        }
        _ => None,
    }
}

/// One `color()` adjuster, e.g. `alpha(0.5)` or `blend(#fff 30%)`.
fn adjust(c: Color, (func, args): (&str, &str), vars: &HashMap<String, String>, depth: usize) -> Option<Color> {
    let amount = |s: &str| -> Option<f32> {
        let s = s.trim();
        Some(match s.strip_suffix('%') {
            Some(p) => p.trim().parse::<f32>().ok()? / 100.0,
            None => s.parse().ok()?,
        })
    };
    match func {
        "alpha" | "a" => Some(Color { a: (amount(args)?.clamp(0.0, 1.0) * 255.0).round() as u8, ..c }),
        "blend" | "blenda" => {
            // Keeps `percent` of the base color and mixes in the rest of the other.
            let (other, percent) = args.trim().rsplit_once(char::is_whitespace)?;
            let other = parse_color(other, vars, depth + 1)?;
            let p = amount(percent)?.clamp(0.0, 1.0);
            let mix = |a: u8, b: u8| (a as f32 * p + b as f32 * (1.0 - p)).round() as u8;
            let a = if func == "blenda" { mix(c.a, other.a) } else { c.a };
            Some(Color { r: mix(c.r, other.r), g: mix(c.g, other.g), b: mix(c.b, other.b), a })
        }
        "lightness" | "l" | "saturation" | "s" => {
            let (h, mut s, mut l) = rgb_to_hsl(c);
            let arg = args.trim();
            let value = amount(arg.trim_start_matches(['+', '-']))?;
            let target = if func.starts_with('l') { &mut l } else { &mut s };
            *target = match arg.chars().next() {
                Some('+') => *target + value,
                Some('-') => *target - value,
                _ => value,
            }
            .clamp(0.0, 1.0);
            let (r, g, b) = hsl_to_rgb(h, s, l);
            Some(Color { r, g, b, a: c.a })
        }
        // Accessibility tweaks like min-contrast() don't change the color here.
        _ => Some(c),
    }
}

/// `name(args)` → `("name", "args")`, when `text` is exactly one call.
fn call(text: &str) -> Option<(&str, &str)> {
    let (func, rest) = text.split_once('(')?;
    let args = rest.strip_suffix(')')?;
    func.chars().all(|c| c.is_ascii_alphabetic() || c == '-').then_some((func.trim(), args))
}

/// The next `name(args)` in `text` and what follows it, with nested parentheses.
fn next_call(text: &str) -> Option<((&str, &str), &str)> {
    let text = text.trim_start();
    let open = text.find('(')?;
    let mut depth = 0;
    for (i, ch) in text.char_indices().skip(open) {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(((text[..open].trim(), &text[open + 1..i]), &text[i + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits off the first space-separated item, keeping parentheses together.
fn split_first(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    let mut depth = 0;
    for (i, ch) in text.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c.is_whitespace() && depth == 0 => return (&text[..i], &text[i..]),
            _ => {}
        }
    }
    (text, "")
}

/// Numbers separated by commas, spaces or `/`, each flagged if it was a percentage.
fn numbers(args: &str) -> Option<Vec<(f32, bool)>> {
    args.split([',', ' ', '/'])
        .filter(|s| !s.trim().is_empty())
        .map(|s| {
            let s = s.trim().trim_end_matches("deg");
            match s.strip_suffix('%') {
                Some(p) => p.parse().ok().map(|v| (v, true)),
                None => s.parse().ok().map(|v| (v, false)),
            }
        })
        .collect()
}

fn alpha(value: Option<(f32, bool)>) -> u8 {
    match value {
        Some((v, true)) => (v * 2.55).round().clamp(0.0, 255.0) as u8,
        Some((v, false)) => (v * 255.0).round().clamp(0.0, 255.0) as u8,
        None => 255,
    }
}

fn parse_hex(hex: &str) -> Option<Color> {
    let digit = |i: usize| u8::from_str_radix(hex.get(i..i + 1)?, 16).ok();
    let pair = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    match hex.len() {
        3 | 4 => {
            let d = |i: usize| digit(i).map(|v| v * 17);
            Some(Color { r: d(0)?, g: d(1)?, b: d(2)?, a: if hex.len() == 4 { d(3)? } else { 255 } })
        }
        6 | 8 => Some(Color { r: pair(0)?, g: pair(2)?, b: pair(4)?, a: if hex.len() == 8 { pair(6)? } else { 255 } }),
        _ => None,
    }
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let h = h.rem_euclid(360.0) / 360.0;
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let channel = |t: f32| {
        let t = t.rem_euclid(1.0);
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    };
    (channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0))
}

fn rgb_to_hsl(c: Color) -> (f32, f32, f32) {
    let (r, g, b) = (c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    if max == min {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

/// CSS named colors.
fn named(name: &str) -> Option<Color> {
    const NAMES: &str = "aliceblue f0f8ff antiquewhite faebd7 aqua 00ffff aquamarine 7fffd4 azure f0ffff \
        beige f5f5dc bisque ffe4c4 black 000000 blanchedalmond ffebcd blue 0000ff blueviolet 8a2be2 \
        brown a52a2a burlywood deb887 cadetblue 5f9ea0 chartreuse 7fff00 chocolate d2691e coral ff7f50 \
        cornflowerblue 6495ed cornsilk fff8dc crimson dc143c cyan 00ffff darkblue 00008b darkcyan 008b8b \
        darkgoldenrod b8860b darkgray a9a9a9 darkgreen 006400 darkgrey a9a9a9 darkkhaki bdb76b \
        darkmagenta 8b008b darkolivegreen 556b2f darkorange ff8c00 darkorchid 9932cc darkred 8b0000 \
        darksalmon e9967a darkseagreen 8fbc8f darkslateblue 483d8b darkslategray 2f4f4f \
        darkslategrey 2f4f4f darkturquoise 00ced1 darkviolet 9400d3 deeppink ff1493 deepskyblue 00bfff \
        dimgray 696969 dimgrey 696969 dodgerblue 1e90ff firebrick b22222 floralwhite fffaf0 \
        forestgreen 228b22 fuchsia ff00ff gainsboro dcdcdc ghostwhite f8f8ff gold ffd700 \
        goldenrod daa520 gray 808080 green 008000 greenyellow adff2f grey 808080 honeydew f0fff0 \
        hotpink ff69b4 indianred cd5c5c indigo 4b0082 ivory fffff0 khaki f0e68c lavender e6e6fa \
        lavenderblush fff0f5 lawngreen 7cfc00 lemonchiffon fffacd lightblue add8e6 lightcoral f08080 \
        lightcyan e0ffff lightgoldenrodyellow fafad2 lightgray d3d3d3 lightgreen 90ee90 lightgrey d3d3d3 \
        lightpink ffb6c1 lightsalmon ffa07a lightseagreen 20b2aa lightskyblue 87cefa \
        lightslategray 778899 lightslategrey 778899 lightsteelblue b0c4de lightyellow ffffe0 lime 00ff00 \
        limegreen 32cd32 linen faf0e6 magenta ff00ff maroon 800000 mediumaquamarine 66cdaa \
        mediumblue 0000cd mediumorchid ba55d3 mediumpurple 9370db mediumseagreen 3cb371 \
        mediumslateblue 7b68ee mediumspringgreen 00fa9a mediumturquoise 48d1cc mediumvioletred c71585 \
        midnightblue 191970 mintcream f5fffa mistyrose ffe4e1 moccasin ffe4b5 navajowhite ffdead \
        navy 000080 oldlace fdf5e6 olive 808000 olivedrab 6b8e23 orange ffa500 orangered ff4500 \
        orchid da70d6 palegoldenrod eee8aa palegreen 98fb98 paleturquoise afeeee palevioletred db7093 \
        papayawhip ffefd5 peachpuff ffdab9 peru cd853f pink ffc0cb plum dda0dd powderblue b0e0e6 \
        purple 800080 rebeccapurple 663399 red ff0000 rosybrown bc8f8f royalblue 4169e1 \
        saddlebrown 8b4513 salmon fa8072 sandybrown f4a460 seagreen 2e8b57 seashell fff5ee \
        sienna a0522d silver c0c0c0 skyblue 87ceeb slateblue 6a5acd slategray 708090 slategrey 708090 \
        snow fffafa springgreen 00ff7f steelblue 4682b4 tan d2b48c teal 008080 thistle d8bfd8 \
        tomato ff6347 turquoise 40e0d0 violet ee82ee wheat f5deb3 white ffffff whitesmoke f5f5f5 \
        yellow ffff00 yellowgreen 9acd32 transparent 00000000";
    let name = name.to_ascii_lowercase();
    let mut words = NAMES.split_whitespace();
    while let (Some(n), Some(hex)) = (words.next(), words.next()) {
        if n == name {
            return parse_hex(hex);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color(text: &str) -> Option<Color> {
        let vars = HashMap::from([
            ("white".to_owned(), "hsl(0, 0%, 100%)".to_owned()),
            ("bg".to_owned(), "var(white)".to_owned()),
        ]);
        parse_color(text, &vars, 0)
    }

    fn rgba(r: u8, g: u8, b: u8, a: u8) -> Option<Color> {
        Some(Color { r, g, b, a })
    }

    #[test]
    fn reads_css_colors() {
        assert_eq!(color("#fa0"), rgba(255, 170, 0, 255));
        assert_eq!(color("#ff000080"), rgba(255, 0, 0, 128));
        assert_eq!(color("rgb(10, 20, 30)"), rgba(10, 20, 30, 255));
        assert_eq!(color("rgba(10, 20, 30, 0.5)"), rgba(10, 20, 30, 128));
        assert_eq!(color("hsl(210, 50%, 60%)"), rgba(102, 153, 204, 255));
        assert_eq!(color("hsla(0, 100%, 50%, 0.25)"), rgba(255, 0, 0, 64));
        assert_eq!(color("rebeccapurple"), rgba(0x66, 0x33, 0x99, 255));
        assert_eq!(color("var(bg)"), rgba(255, 255, 255, 255));
        assert_eq!(color("color(var(white) alpha(0.5))"), rgba(255, 255, 255, 128));
        assert_eq!(color("color(var(white) a(0.25))"), rgba(255, 255, 255, 64));
        assert_eq!(color("color(#000 blend(#fff 25%))"), rgba(191, 191, 191, 255));
        let lighter = color("color(hsl(0, 0%, 50%) lightness(+10%))").unwrap();
        assert!((153..=154).contains(&lighter.r) && lighter.r == lighter.b, "{lighter:?}");
        assert_eq!(color("var(missing)"), None);
        assert_eq!(color("nonsense"), None);
    }

    #[test]
    fn reads_a_color_scheme() {
        let theme = parse(
            r##"{
                // Comments and trailing commas are allowed.
                "name": "Test",
                "variables": { "blue": "hsl(210, 50%, 60%)", },
                "globals": {
                    "background": "#102030",
                    "foreground": "var(blue)",
                    "selection": "color(var(blue) alpha(0.5))",
                },
                "rules": [
                    { "scope": "comment", "foreground": "#808080", "font_style": "italic" },
                    { "scope": "string", "foreground": ["#ff0000", "#00ff00"] },
                    { "foreground": "#fff" },
                ],
            }"##,
        )
        .unwrap();
        assert_eq!(theme.name.as_deref(), Some("Test"));
        assert_eq!(theme.settings.background, rgba(0x10, 0x20, 0x30, 255));
        assert_eq!(theme.settings.foreground, rgba(102, 153, 204, 255));
        assert_eq!(theme.settings.selection, rgba(102, 153, 204, 128));
        assert_eq!(theme.scopes.len(), 2);
        assert_eq!(theme.scopes[0].style.font_style, Some(FontStyle::ITALIC));
        assert_eq!(theme.scopes[1].style.foreground, rgba(255, 0, 0, 255));
        assert!(parse(r#"{ "globals": {} }"#).is_err());
    }
}

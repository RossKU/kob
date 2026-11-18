//! Which characters a displayed name, ticker or text may hold (security finding L7).
//!
//! One definition, `data/display-chars.json`, read here and by the web (`web/src/kob/display.ts`).
//! The same file holds test cases that both sides run, so the two cannot drift.
//!
//! * **Refused** ([`has_bad_char`]): characters that draw nothing or reorder what is drawn, so a
//!   name reads differently from what it is: control characters, format characters (Unicode
//!   category Cf) including the bidi controls, soft hyphen, Arabic letter mark and zero-width
//!   characters, the line and paragraph separators, the Hangul, braille and Khmer fillers,
//!   combining grapheme joiner, variation selectors and tag characters.
//! * **Kept for emoji**: a zero-width joiner between two visible characters, an emoji or text
//!   variation selector right after a visible character, and a flag tag sequence (black flag, tags,
//!   cancel tag). Anything else of those is refused.
//! * **Flagged** ([`has_lookalike`]): a character that is drawn like a Latin letter (Cyrillic and
//!   Greek twins, full-width forms, mathematical and enclosed letters). A name with one is shown
//!   and marked, never refused: a rule against look-alikes would be KOB choosing which token may
//!   carry a name.

use std::sync::OnceLock;

struct Table {
    refuse: Vec<(u32, u32)>,
    zwj: u32,
    variation: (u32, u32),
    tag_base: u32,
    tag_range: (u32, u32),
    tag_end: u32,
    look_ranges: Vec<(u32, u32)>,
    look_chars: Vec<char>,
}

fn hex(v: &serde_json::Value) -> u32 {
    u32::from_str_radix(v.as_str().expect("display-chars.json: hex string"), 16).expect("display-chars.json: hex")
}

fn pair(v: &serde_json::Value) -> (u32, u32) {
    (hex(&v[0]), hex(&v[1]))
}

pub(crate) const TABLE_JSON: &str = include_str!("../data/display-chars.json");

fn table() -> &'static Table {
    static T: OnceLock<Table> = OnceLock::new();
    T.get_or_init(|| {
        let d: serde_json::Value = serde_json::from_str(TABLE_JSON).expect("data/display-chars.json");
        let ranges = |k: &str| d[k].as_array().expect("display-chars.json: ranges").iter().map(pair).collect::<Vec<_>>();
        Table {
            refuse: ranges("refuse"),
            zwj: hex(&d["zwj"]),
            variation: pair(&d["variation"]),
            tag_base: hex(&d["tag_base"]),
            tag_range: pair(&d["tag_range"]),
            tag_end: hex(&d["tag_end"]),
            look_ranges: ranges("lookalike_ranges"),
            look_chars: d["lookalike_chars"].as_str().expect("display-chars.json: lookalike_chars").chars().collect(),
        }
    })
}

fn in_ranges(r: &[(u32, u32)], c: u32) -> bool {
    r.iter().any(|&(lo, hi)| (lo..=hi).contains(&c))
}

/// A character that is drawn and is neither a joiner, a selector, a tag nor white space.
fn visible(t: &Table, c: char) -> bool {
    let u = c as u32;
    !c.is_whitespace()
        && u != t.zwj
        && !(t.variation.0..=t.variation.1).contains(&u)
        && !(t.tag_range.0..=t.tag_range.1).contains(&u)
        && !in_ranges(&t.refuse, u)
}

/// For each character of `s`: whether it is refused.
fn flags(s: &str) -> Vec<bool> {
    let t = table();
    let cs: Vec<char> = s.chars().collect();
    let mut bad = vec![false; cs.len()];
    let mut i = 0;
    while i < cs.len() {
        let u = cs[i] as u32;
        let prev_visible = i > 0 && (visible(t, cs[i - 1]) || ((cs[i - 1] as u32) == t.variation.1 && !bad[i - 1]));
        if in_ranges(&t.refuse, u) {
            bad[i] = true;
        } else if u == t.zwj {
            // between two visible characters; a selector may sit before it (heart + selector + joiner)
            bad[i] = !(prev_visible && cs.get(i + 1).is_some_and(|&n| visible(t, n)));
        } else if (t.variation.0..=t.variation.1).contains(&u) {
            bad[i] = !(i > 0 && visible(t, cs[i - 1]));
        } else if (t.tag_range.0..=t.tag_range.1).contains(&u) {
            // a flag tag sequence: black flag, tags, cancel tag
            let mut j = i;
            while j < cs.len() && (t.tag_range.0..=t.tag_range.1).contains(&(cs[j] as u32)) {
                j += 1;
            }
            let ok = i > 0 && cs[i - 1] as u32 == t.tag_base && cs[j - 1] as u32 == t.tag_end && j - i >= 2;
            for b in &mut bad[i..j] {
                *b = !ok;
            }
            i = j;
            continue;
        }
        i += 1;
    }
    bad
}

/// Whether `s` holds a refused character (see the module documentation).
pub fn has_bad_char(s: &str) -> bool {
    flags(s).into_iter().any(|b| b)
}

/// `s` without its refused characters.
pub fn strip_bad(s: &str) -> String {
    let bad = flags(s);
    s.chars().zip(bad).filter(|(_, b)| !b).map(|(c, _)| c).collect()
}

/// Whether `s` holds a character drawn like a Latin letter (a warning for display, never a refusal).
pub fn has_lookalike(s: &str) -> bool {
    let t = table();
    s.chars().any(|c| t.look_chars.contains(&c) || in_ranges(&t.look_ranges, c as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shared_cases() {
        let d: serde_json::Value = serde_json::from_str(TABLE_JSON).unwrap();
        let cases = d["cases"].as_array().unwrap();
        assert!(cases.len() > 40);
        for c in cases {
            let s = c["s"].as_str().unwrap();
            assert_eq!(has_bad_char(s), c["bad"].as_bool().unwrap(), "bad: {s:?}");
            assert_eq!(has_lookalike(s), c["lookalike"].as_bool().unwrap(), "lookalike: {s:?}");
        }
    }

    #[test]
    fn the_listed_code_points_are_refused() {
        for cp in [0x61C, 0x2028, 0x2029, 0xAD, 0x115F, 0x1160, 0x3164, 0xFFA0, 0x2800, 0x200B, 0xE0001, 0xE0100, 0xFE00, 0x85, 0x7F] {
            let s = format!("A{}B", char::from_u32(cp).unwrap());
            assert!(has_bad_char(&s), "{cp:X}");
            assert_eq!(strip_bad(&s), "AB", "{cp:X}");
        }
    }

    #[test]
    fn emoji_sequences_survive() {
        for s in ["\u{1F469}\u{200D}\u{1F4BB}", "\u{1F469}\u{200D}\u{2764}\u{FE0F}\u{200D}\u{1F468}", "\u{2764}\u{FE0F}", "\u{1F3F4}\u{E0067}\u{E0062}\u{E007F}"] {
            assert!(!has_bad_char(s), "{s:?}");
            assert_eq!(strip_bad(s), s);
        }
        // a joiner with nothing to join is dropped, the rest stays
        assert_eq!(strip_bad("\u{200D}A\u{200D}"), "A");
        assert_eq!(strip_bad("Ab\u{E0041}"), "Ab");
    }

    #[test]
    fn lookalikes_are_flagged_not_refused() {
        assert!(has_lookalike("p\u{0430}ypal") && !has_bad_char("p\u{0430}ypal"));
        assert!(!has_lookalike("Kaspa 123 \u{1F600}"));
    }
}

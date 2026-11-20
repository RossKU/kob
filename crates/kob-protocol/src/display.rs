//! Which characters a displayed name, ticker or text may hold.
//!
//! One definition, `data/display-chars.json`, read here and by the web (`web/src/kob/display.ts`).
//! The same file holds test cases that both sides run, so the two cannot drift. Its Unicode-derived
//! parts (`emoji`, `nfc_no`, `nfc_compose`) are written by `web/scripts/gen-display-chars.mjs`.
//!
//! * **Refused** ([`has_bad_char`]): characters that draw nothing or reorder what is drawn, so a
//!   name reads differently from what it is: control characters, format characters (Unicode
//!   category Cf) including the bidi controls, soft hyphen, Arabic letter mark and zero-width
//!   characters, the line and paragraph separators, the Hangul, braille and Khmer fillers,
//!   combining grapheme joiner, variation selectors and tag characters.
//! * **Kept for emoji**: a zero-width joiner between two emoji (the first may end in a skin tone
//!   or a kept selector), an emoji or text variation selector right after an emoji or inside a
//!   keycap (`#`, `*` or a digit, the selector, U+20E3), and a flag tag sequence (black flag, tags,
//!   cancel tag). Anything else of those is refused: a joiner or a selector next to a letter draws
//!   nothing, so `US<ZWJ>DT` would read as `USDT`.
//! * **Flagged** ([`has_lookalike`]): a character that is drawn like a Latin letter (Cyrillic,
//!   Greek, Cherokee and Lisu twins, Latin small capitals, full-width forms, mathematical and
//!   enclosed letters), and a string that NFC normalization would change (a decomposed accent, a
//!   singleton such as the Greek question mark), which is drawn like its normalized twin. A name
//!   with one is shown and marked, never refused: a rule against look-alikes would be KOB choosing
//!   which token may carry a name.

use std::sync::OnceLock;

struct Table {
    refuse: Vec<(u32, u32)>,
    zwj: u32,
    variation: (u32, u32),
    tag_base: u32,
    tag_range: (u32, u32),
    tag_end: u32,
    emoji: Vec<(u32, u32)>,
    emoji_modifier: (u32, u32),
    keycap: u32,
    keycap_bases: Vec<char>,
    look_ranges: Vec<(u32, u32)>,
    look_chars: Vec<char>,
    nfc_no: Vec<(u32, u32)>,
    /// (combining character, a character it composes with), sorted
    nfc_compose: Vec<(u32, u32)>,
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
        let chars = |k: &str| d[k].as_str().expect("display-chars.json: characters").chars().collect::<Vec<_>>();
        let mut nfc_compose: Vec<(u32, u32)> = d["nfc_compose"]
            .as_array()
            .expect("display-chars.json: nfc_compose")
            .iter()
            .flat_map(|e| {
                let mark = hex(&e[0]);
                e[1].as_str().expect("display-chars.json: nfc_compose bases").chars().map(move |b| (mark, b as u32))
            })
            .collect();
        nfc_compose.sort_unstable();
        Table {
            refuse: ranges("refuse"),
            zwj: hex(&d["zwj"]),
            variation: pair(&d["variation"]),
            tag_base: hex(&d["tag_base"]),
            tag_range: pair(&d["tag_range"]),
            tag_end: hex(&d["tag_end"]),
            emoji: ranges("emoji"),
            emoji_modifier: pair(&d["emoji_modifier"]),
            keycap: hex(&d["keycap"]),
            keycap_bases: chars("keycap_bases"),
            look_ranges: ranges("lookalike_ranges"),
            look_chars: chars("lookalike_chars"),
            nfc_no: ranges("nfc_no"),
            nfc_compose,
        }
    })
}

fn in_ranges(r: &[(u32, u32)], c: u32) -> bool {
    r.iter().any(|&(lo, hi)| (lo..=hi).contains(&c))
}

/// For each character of `s`: whether it is refused.
fn flags(s: &str) -> Vec<bool> {
    let t = table();
    let cs: Vec<u32> = s.chars().map(|c| c as u32).collect();
    let mut bad = vec![false; cs.len()];
    let emoji = |c: u32| in_ranges(&t.emoji, c);
    let selector = |c: u32| (t.variation.0..=t.variation.1).contains(&c);
    let tag = |c: u32| (t.tag_range.0..=t.tag_range.1).contains(&c);
    let mut i = 0;
    while i < cs.len() {
        let u = cs[i];
        let prev = i.checked_sub(1).map(|k| cs[k]);
        let next = cs.get(i + 1).copied();
        if in_ranges(&t.refuse, u) {
            bad[i] = true;
        } else if u == t.zwj {
            // between two emoji; the first may end in a skin tone or a kept selector (heart + selector + joiner)
            let after = prev
                .is_some_and(|p| emoji(p) || (t.emoji_modifier.0..=t.emoji_modifier.1).contains(&p) || (selector(p) && !bad[i - 1]));
            bad[i] = !(after && next.is_some_and(emoji));
        } else if selector(u) {
            // right after an emoji, or inside a keycap
            let keycap =
                prev.is_some_and(|p| char::from_u32(p).is_some_and(|c| t.keycap_bases.contains(&c))) && next == Some(t.keycap);
            bad[i] = !(prev.is_some_and(emoji) || keycap);
        } else if tag(u) {
            // a flag tag sequence: black flag, tags, cancel tag
            let mut j = i;
            while j < cs.len() && tag(cs[j]) {
                j += 1;
            }
            let ok = prev == Some(t.tag_base) && cs[j - 1] == t.tag_end && j - i >= 2;
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

/// Whether NFC normalization would change `s`: it holds a character NFC never keeps, or two
/// neighbours that compose (Hangul jamo by rule, the rest from `nfc_compose`). Marks that are only
/// out of their canonical order, with nothing to compose, are not seen.
fn changes_under_nfc(t: &Table, s: &str) -> bool {
    const L: (u32, u32) = (0x1100, 0x1112);
    const V: (u32, u32) = (0x1161, 0x1175);
    const T: (u32, u32) = (0x11A8, 0x11C2);
    const SYLLABLES: (u32, u32) = (0xAC00, 0xD7A3);
    let within = |r: (u32, u32), c: u32| (r.0..=r.1).contains(&c);
    let mut prev: Option<u32> = None;
    for c in s.chars().map(|c| c as u32) {
        if in_ranges(&t.nfc_no, c) {
            return true;
        }
        if let Some(p) = prev {
            let hangul =
                (within(L, p) && within(V, c)) || (within(SYLLABLES, p) && (p - SYLLABLES.0).is_multiple_of(28) && within(T, c));
            if hangul || t.nfc_compose.binary_search(&(c, p)).is_ok() {
                return true;
            }
        }
        prev = Some(c);
    }
    false
}

/// Whether `s` holds a character drawn like a Latin letter, or would change under NFC (a warning
/// for display, never a refusal).
pub fn has_lookalike(s: &str) -> bool {
    let t = table();
    s.chars().any(|c| t.look_chars.contains(&c) || in_ranges(&t.look_ranges, c as u32)) || changes_under_nfc(t, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shared_cases() {
        let d: serde_json::Value = serde_json::from_str(TABLE_JSON).unwrap();
        let cases = d["cases"].as_array().unwrap();
        assert!(cases.len() > 70);
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
        for s in [
            "\u{1F469}\u{200D}\u{1F4BB}",
            "\u{1F469}\u{200D}\u{2764}\u{FE0F}\u{200D}\u{1F468}",
            "\u{1F469}\u{1F3FD}\u{200D}\u{1F4BB}",
            "\u{2764}\u{FE0F}",
            "\u{2764}\u{FE0E}",
            "#\u{FE0F}\u{20E3}",
            "\u{1F3F4}\u{E0067}\u{E0062}\u{E007F}",
        ] {
            assert!(!has_bad_char(s), "{s:?}");
            assert_eq!(strip_bad(s), s);
        }
        // a joiner with nothing to join is dropped, the rest stays
        assert_eq!(strip_bad("\u{200D}A\u{200D}"), "A");
        assert_eq!(strip_bad("Ab\u{E0041}"), "Ab");
    }

    #[test]
    fn a_joiner_or_selector_next_to_a_letter_is_refused() {
        for (s, kept) in [
            ("US\u{200D}DT", "USDT"),
            ("KAS\u{FE0F}", "KAS"),
            ("KAS\u{FE0E}", "KAS"),
            ("A\u{200D}\u{1F600}", "A\u{1F600}"),
            ("\u{1F600}\u{200D}A", "\u{1F600}A"),
            ("1\u{FE0F}", "1"),
        ] {
            assert!(has_bad_char(s), "{s:?}");
            assert_eq!(strip_bad(s), kept, "{s:?}");
        }
    }

    #[test]
    fn lookalikes_are_flagged_not_refused() {
        assert!(has_lookalike("p\u{0430}ypal") && !has_bad_char("p\u{0430}ypal"));
        assert!(!has_lookalike("Kaspa 123 \u{1F600}"));
        // Lisu and Cherokee letters, Latin small capitals
        for s in ["\u{A4D7}\u{A4EE}\u{A4E2}", "\u{13AA}", "\u{1D0B}\u{1D00}\u{A731}", "Kas\u{01C0}"] {
            assert!(has_lookalike(s) && !has_bad_char(s), "{s:?}");
        }
    }

    #[test]
    fn a_string_nfc_would_change_is_flagged() {
        // the decomposed twin of an accented name; the composed one is not flagged
        assert!(has_lookalike("Cafe\u{0301}") && !has_lookalike("Caf\u{00E9}"));
        assert!(has_lookalike("\u{1100}\u{1161}") && !has_lookalike("\u{AC00}"));
        assert!(has_lookalike("\u{AC00}\u{11A8}") && !has_lookalike("\u{AC01}"));
        assert!(has_lookalike("\u{037E}"));
        // a mark that composes with nothing before it is the normal form
        assert!(!has_lookalike("\u{0915}\u{093C}") && !has_lookalike("\u{0995}\u{09BE}"));
    }
}

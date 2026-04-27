//! libretro cheat-database (`.cht`) fetch support: URL construction, INI-ish
//! parsing, and HTML-entity decoding.
//!
//! The libretro-database ships one `.cht` per game under
//! `cht/Nintendo - Game Boy/<NAME>.cht` (or `… Game Boy Color/`), where `<NAME>`
//! is the canonical No-Intro name ([`no_intro::identify`](crate::no_intro)). This
//! module is pure (no I/O, no threads): it builds the candidate URLs, and parses
//! a downloaded `.cht` body into a list of [`FetchedCheat`]s. The session emits a
//! fetch request; each platform performs the HTTP GET and feeds the body back to
//! [`Session::finish_fetched_cheats`](crate::session::Session::finish_fetched_cheats),
//! which calls [`parse_cht`] here.

use serde::{Deserialize, Serialize};

/// One entry parsed from a `.cht` file: a human description plus the raw code
/// string(s). A single entry's `codes` are `+`-joined GameShark/Game Genie codes
/// that must all be applied together for the cheat to work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchedCheat {
    pub description: String,
    pub codes: Vec<String>,
}

/// The base of the raw libretro-database `.cht` tree. The game name (URL-encoded)
/// and `.cht` suffix are appended per candidate.
const CHT_BASE: &str =
    "https://raw.githubusercontent.com/libretro/libretro-database/master/cht";

/// The two libretro system folders a Game Boy / Game Boy Color `.cht` can live
/// under. Some entries are misfiled across the two, so callers try both.
const GB_FOLDER: &str = "Nintendo - Game Boy";
const GBC_FOLDER: &str = "Nintendo - Game Boy Color";

/// Whether a ROM's cartridge header marks it as Game Boy Color. The CGB flag is
/// header byte `0x0143`: `0x80` (CGB-enhanced) or `0xC0` (CGB-only) ⇒ Color.
pub fn is_cgb(rom: &[u8]) -> bool {
    matches!(rom.get(0x0143), Some(0x80) | Some(0xC0))
}

/// The candidate `.cht` URLs for `name`, in preference order: the folder chosen
/// by the cartridge's CGB flag first, then the other folder (entries are
/// occasionally misfiled). `name` is the canonical No-Intro name; it is
/// percent-encoded here.
pub fn candidate_urls(name: &str, cgb: bool) -> Vec<String> {
    let folders = if cgb {
        [GBC_FOLDER, GB_FOLDER]
    } else {
        [GB_FOLDER, GBC_FOLDER]
    };
    let variants = name_variants(name);
    let mut urls = Vec::with_capacity(folders.len() * variants.len());
    for folder in folders {
        for v in &variants {
            urls.push(format!(
                "{CHT_BASE}/{}/{}.cht",
                percent_encode(folder),
                percent_encode(v)
            ));
        }
    }
    urls
}

/// Filename candidates for a No-Intro `name`, most-specific first: the exact name,
/// then progressively dropping trailing " (...)" qualifiers. libretro's cht files
/// are frequently named from an older No-Intro DAT that omits the "(Rev N)" (and
/// sometimes the region) suffixes, so the exact name often 404s while a shortened
/// form matches — e.g. "Pokemon - Crystal Version (USA, Europe) (Rev 1)" has no
/// cht, but "Pokemon - Crystal Version (USA, Europe)" does.
fn name_variants(name: &str) -> Vec<String> {
    let mut out = vec![name.to_string()];
    let mut cur = name.trim();
    while cur.ends_with(')') {
        let Some(idx) = cur.rfind(" (") else { break };
        cur = cur[..idx].trim_end();
        if !cur.is_empty() && !out.iter().any(|s| s == cur) {
            out.push(cur.to_string());
        }
    }
    out
}

/// Percent-encode a path segment for a raw.githubusercontent URL. Encodes bytes
/// that are unsafe in a URL path segment (notably space → `%20`, comma → `%2C`)
/// while leaving the URL-path-safe punctuation that pervades No-Intro names —
/// parentheses, `!`, `'`, `*` — literal, matching how curl/browsers request these
/// files (GitHub serves either form, but keeping the common shape is lowest-risk).
pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        let safe = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'~' | b'(' | b')' | b'!' | b'\'' | b'*');
        if safe {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(hex_digit(b >> 4));
            out.push(hex_digit(b & 0x0F));
        }
    }
    out
}

fn hex_digit(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'A' + (n - 10)) as char,
    }
}

/// Parse a libretro `.cht` body into its cheat entries.
///
/// The format is INI-ish: a `cheats = N` header then `cheatN_desc`, `cheatN_code`
/// (and ignored `cheatN_enable`) keys. `desc` values are quoted and may contain
/// HTML entities (decoded here); `code` values are one or more `+`-joined raw
/// GameShark/Game Genie codes. Entries with an empty code list are dropped.
/// Robust to blank lines, stray whitespace, and out-of-order keys; unknown keys
/// are ignored.
pub fn parse_cht(body: &str) -> Vec<FetchedCheat> {
    // Collect desc/code per index, then emit in ascending index order.
    let mut descs: std::collections::BTreeMap<u32, String> = Default::default();
    let mut codes: std::collections::BTreeMap<u32, Vec<String>> = Default::default();

    for line in body.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = unquote(value.trim());

        let Some(rest) = key.strip_prefix("cheat") else {
            continue;
        };
        // `rest` is like "0_desc" / "12_code" / "0_enable".
        let Some((idx_str, field)) = rest.split_once('_') else {
            continue;
        };
        let Ok(idx) = idx_str.parse::<u32>() else {
            continue;
        };

        match field {
            "desc" => {
                descs.insert(idx, decode_entities(&value));
            }
            "code" => {
                let split: Vec<String> = value
                    .split('+')
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                    .map(str::to_string)
                    .collect();
                codes.insert(idx, split);
            }
            _ => {}
        }
    }

    let mut out = Vec::new();
    // libretro's cht files concatenate several cheat dumps and repeat each entry
    // many times (Pokémon Crystal: 8035 lines, only 1264 distinct). Drop exact
    // (description, codes) duplicates, keeping first-seen order.
    let mut seen = std::collections::HashSet::new();
    for (idx, code_list) in codes {
        if code_list.is_empty() {
            continue;
        }
        let description = descs
            .get(&idx)
            .cloned()
            .unwrap_or_else(|| format!("Cheat {idx}"));
        if seen.insert((description.clone(), code_list.clone())) {
            out.push(FetchedCheat { description, codes: code_list });
        }
    }
    out
}

/// Strip one pair of surrounding double quotes from a `.cht` value, if present.
fn unquote(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Decode the small set of HTML entities that appear in libretro cheat
/// descriptions: the named XML/HTML basics, a couple of accented letters seen in
/// the DB, and numeric (`&#NN;` / `&#xHH;`) references. Unknown entities are left
/// verbatim so nothing is silently dropped.
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        match after.find(';') {
            // Cap the entity name length so a lone `&` in prose (e.g. "Fire & Ice"
            // with a far-off `;`) isn't swallowed.
            Some(semi) if semi <= 10 => {
                let entity = &after[1..semi];
                if let Some(ch) = decode_one_entity(entity) {
                    out.push(ch);
                } else {
                    out.push('&');
                    out.push_str(entity);
                    out.push(';');
                }
                rest = &after[semi + 1..];
            }
            _ => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Decode one entity body (the text between `&` and `;`), or `None` if unknown.
fn decode_one_entity(entity: &str) -> Option<char> {
    let named = match entity {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" | "#39" => '\'',
        "eacute" => 'é',
        "egrave" => 'è',
        "agrave" => 'à',
        "ccedil" => 'ç',
        "nbsp" => ' ',
        _ => return decode_numeric_entity(entity),
    };
    Some(named)
}

/// Decode a numeric character reference: `#NN` (decimal) or `#xHH` / `#XHH` (hex).
fn decode_numeric_entity(entity: &str) -> Option<char> {
    let digits = entity.strip_prefix('#')?;
    let code = if let Some(hex) = digits.strip_prefix(['x', 'X']) {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        digits.parse::<u32>().ok()?
    };
    char::from_u32(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cgb_flag_selects_color() {
        let mut rom = vec![0u8; 0x150];
        assert!(!is_cgb(&rom));
        rom[0x0143] = 0x80;
        assert!(is_cgb(&rom));
        rom[0x0143] = 0xC0;
        assert!(is_cgb(&rom));
        rom[0x0143] = 0x00;
        assert!(!is_cgb(&rom));
    }

    #[test]
    fn urls_order_by_cgb_flag_and_encode_spaces() {
        let gb = candidate_urls("Pokemon - Red Version (USA, Europe) (SGB Enhanced)", false);
        // Exact name first, in the primary (GB) folder.
        assert!(gb[0].contains("/Nintendo%20-%20Game%20Boy/"));
        assert!(gb[0].contains("Pokemon%20-%20Red%20Version"));
        assert!(gb[0].contains("(USA%2C%20Europe)")); // comma encoded, parens kept
        assert!(gb[0].contains("(SGB%20Enhanced)"));
        assert!(gb[0].ends_with(".cht"));
        // The GBC folder appears as a fallback for a non-CGB ROM.
        assert!(gb.iter().any(|u| u.contains("/Nintendo%20-%20Game%20Boy%20Color/")));

        let gbc = candidate_urls("Some Color Game", true);
        assert!(gbc[0].contains("/Nintendo%20-%20Game%20Boy%20Color/"));
        assert!(gbc.iter().any(|u| u.contains("/Nintendo%20-%20Game%20Boy/")));
    }

    #[test]
    fn name_variants_strip_trailing_qualifiers() {
        let v = name_variants("Pokemon - Crystal Version (USA, Europe) (Rev 1)");
        assert_eq!(v[0], "Pokemon - Crystal Version (USA, Europe) (Rev 1)");
        assert_eq!(v[1], "Pokemon - Crystal Version (USA, Europe)"); // the one that exists
        assert_eq!(v[2], "Pokemon - Crystal Version");
        // A name with no qualifiers yields just itself.
        assert_eq!(name_variants("Tetris"), vec!["Tetris".to_string()]);
    }

    #[test]
    fn parses_multi_entry_cht() {
        let body = "cheats = 2\n\
                    \n\
                    cheat0_desc = \"Infinite Health\"\n\
                    cheat0_code = \"010AF4C6\"\n\
                    cheat0_enable = false\n\
                    \n\
                    cheat1_desc = \"Have All Badges\"\n\
                    cheat1_code = \"01FF56D3+01FF57D3\"\n\
                    cheat1_enable = false\n";
        let cheats = parse_cht(body);
        assert_eq!(cheats.len(), 2);
        assert_eq!(cheats[0].description, "Infinite Health");
        assert_eq!(cheats[0].codes, vec!["010AF4C6"]);
        assert_eq!(cheats[1].description, "Have All Badges");
        assert_eq!(cheats[1].codes, vec!["01FF56D3", "01FF57D3"]);
    }

    #[test]
    fn parse_is_order_and_gap_robust() {
        // Keys out of order, an entry with only a code (desc missing), blank lines.
        let body = "cheat2_code = \"01FFC0DE\"\n\
                    cheat0_code = \"010102D0\"\n\
                    cheat0_desc = \"Zero\"\n";
        let cheats = parse_cht(body);
        assert_eq!(cheats.len(), 2);
        // Ascending index order regardless of file order.
        assert_eq!(cheats[0].description, "Zero");
        assert_eq!(cheats[1].description, "Cheat 2"); // synthesized when desc absent
    }

    #[test]
    fn drops_empty_code_entries() {
        let body = "cheat0_desc = \"Broken\"\ncheat0_code = \"\"\n";
        assert!(parse_cht(body).is_empty());
    }

    #[test]
    fn decodes_html_entities() {
        assert_eq!(decode_entities("Sword &amp; Shield"), "Sword & Shield");
        assert_eq!(decode_entities("&lt;tag&gt;"), "<tag>");
        assert_eq!(decode_entities("say &quot;hi&quot;"), "say \"hi\"");
        assert_eq!(decode_entities("it&#39;s / it&apos;s"), "it's / it's");
        assert_eq!(decode_entities("Pok&eacute;mon"), "Pokémon");
        assert_eq!(decode_entities("caf&egrave;"), "cafè");
        assert_eq!(decode_entities("A&#66;C"), "ABC"); // numeric decimal
        assert_eq!(decode_entities("A&#x42;C"), "ABC"); // numeric hex
    }

    #[test]
    fn leaves_unknown_or_lone_ampersand_verbatim() {
        assert_eq!(decode_entities("Fire & Ice"), "Fire & Ice");
        assert_eq!(decode_entities("100% &unknownentity; done"), "100% &unknownentity; done");
        assert_eq!(decode_entities("plain text"), "plain text");
    }

    #[test]
    fn entity_decode_applies_in_full_parse() {
        let body = "cheat0_desc = \"Mario &amp; Luigi\"\ncheat0_code = \"010AF4C6\"\n";
        let cheats = parse_cht(body);
        assert_eq!(cheats[0].description, "Mario & Luigi");
    }
}

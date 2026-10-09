//! Targeted edits to Mylar's `config.ini` text.
//!
//! Mylar writes the file with Python's `configparser`: `[Section]` headers and
//! `key = value` lines, keys lowercased. An edit replaces only the value of the
//! named key in the named section and leaves every other byte as it was; a key
//! missing from its section is appended to that section, and a missing section
//! is appended to the file.

/// Set `key` to `value` in `[section]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub section: String,
    pub key: String,
    pub value: String,
}

/// `text` with every edit applied.
pub fn rewrite(text: &str, edits: &[Edit]) -> String {
    let mut out = text.to_string();
    for e in edits {
        out = apply(&out, e);
    }
    out
}

/// The value of `key` in `[section]`, trimmed.
pub fn get<'a>(text: &'a str, section: &str, key: &str) -> Option<&'a str> {
    let mut current: Option<&str> = None;
    for line in text.lines() {
        if let Some(name) = header(line) {
            current = Some(name);
            continue;
        }
        if current != Some(section) || !is_entry(line) {
            continue;
        }
        if let Some((k, v)) = line.split_once(['=', ':']) {
            if k.trim().eq_ignore_ascii_case(key) {
                return Some(v.trim());
            }
        }
    }
    None
}

fn header(line: &str) -> Option<&str> {
    let t = line.trim();
    t.strip_prefix('[')?.strip_suffix(']').map(str::trim)
}

/// A `key = value` line: not blank, not a comment, not a continuation line.
fn is_entry(line: &str) -> bool {
    let t = line.trim_start();
    !t.is_empty() && !t.starts_with('#') && !t.starts_with(';') && !line.starts_with([' ', '\t'])
}

fn apply(text: &str, e: &Edit) -> String {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    // Lines keep their own terminators so untouched lines round-trip exactly.
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut in_section = false;
    // Index just past the section's last entry, where a missing key goes.
    let mut section_end: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        let body = line.trim_end_matches(['\r', '\n']);
        if let Some(name) = header(body) {
            in_section = name == e.section;
            if in_section {
                section_end = Some(i + 1);
            }
            continue;
        }
        if !in_section {
            continue;
        }
        if !body.trim().is_empty() {
            section_end = Some(i + 1);
        }
        let Some(pos) = body.find(['=', ':']).filter(|_| is_entry(body)) else {
            continue;
        };
        if body[..pos].trim().eq_ignore_ascii_case(&e.key) {
            let after = &body[pos + 1..];
            let gap = &after[..after.len() - after.trim_start().len()];
            let ending = &line[body.len()..];
            let replaced = format!("{}{}{}{}", &body[..=pos], gap, e.value, ending);
            // Indented lines after the entry continue its old value; configparser
            // would join them onto the new one.
            let continued = lines[i + 1..]
                .iter()
                .take_while(|l| l.starts_with([' ', '\t']) && !l.trim().is_empty())
                .count();
            let mut out = String::with_capacity(text.len());
            for (j, l) in lines.iter().enumerate() {
                if j == i {
                    out.push_str(&replaced);
                } else if j <= i || j > i + continued {
                    out.push_str(l);
                }
            }
            return out;
        }
    }
    // configparser lowercases keys on write.
    let entry = format!("{} = {}", e.key.to_ascii_lowercase(), e.value);
    match section_end {
        Some(at) => {
            let mut out = String::with_capacity(text.len() + entry.len() + 2);
            for (j, l) in lines.iter().enumerate() {
                out.push_str(l);
                if j + 1 == at {
                    if !l.ends_with('\n') {
                        out.push_str(newline);
                    }
                    out.push_str(&entry);
                    out.push_str(newline);
                }
            }
            out
        }
        None => {
            let mut out = text.to_string();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push_str(newline);
            }
            if !out.is_empty() {
                out.push_str(newline);
            }
            out.push_str(&format!("[{}]{newline}{entry}{newline}", e.section));
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(section: &str, key: &str, value: &str) -> Edit {
        Edit {
            section: section.into(),
            key: key.into(),
            value: value.into(),
        }
    }

    const SAMPLE: &str = "[General]\nconfig_version = 14\nsearch_delay = 5\n\n[Providers]\nusenet_retention = 3500\nextra = a, b\n\n[Client]\nnzb_downloader = 3\n";

    #[test]
    fn replaces_only_the_target_value() {
        let out = rewrite(SAMPLE, &[edit("Providers", "usenet_retention", "6000")]);
        assert_eq!(
            out,
            SAMPLE.replace("usenet_retention = 3500", "usenet_retention = 6000")
        );
    }

    #[test]
    fn the_same_key_in_another_section_is_left_alone() {
        let text = "[A]\nk = 1\n[B]\nk = 2\n";
        assert_eq!(
            rewrite(text, &[edit("B", "k", "9")]),
            "[A]\nk = 1\n[B]\nk = 9\n"
        );
    }

    #[test]
    fn keeps_the_lines_own_spacing_and_line_endings() {
        let text = "[General]\r\nsearch_delay=5\r\nx = y\r\n";
        assert_eq!(
            rewrite(text, &[edit("General", "search_delay", "1")]),
            "[General]\r\nsearch_delay=1\r\nx = y\r\n"
        );
    }

    #[test]
    fn a_missing_key_joins_the_end_of_its_section() {
        let out = rewrite(SAMPLE, &[edit("General", "dynamic_update", "0")]);
        assert_eq!(
            out,
            SAMPLE.replace(
                "search_delay = 5\n",
                "search_delay = 5\ndynamic_update = 0\n"
            )
        );
        let out = rewrite(
            "[Client]\nnzb_downloader = 3",
            &[edit("Client", "sab_host", "h")],
        );
        assert_eq!(out, "[Client]\nnzb_downloader = 3\nsab_host = h\n");
    }

    #[test]
    fn a_missing_section_is_appended() {
        let out = rewrite(
            "[General]\na = 1\n",
            &[edit("Providers", "usenet_retention", "6000")],
        );
        assert_eq!(
            out,
            "[General]\na = 1\n\n[Providers]\nusenet_retention = 6000\n"
        );
        assert_eq!(rewrite("", &[edit("S", "k", "v")]), "[S]\nk = v\n");
    }

    #[test]
    fn comments_and_continuations_are_not_keys() {
        let text = "[General]\n# search_delay = 9\nnotes = a\n  search_delay = 7\n";
        assert_eq!(get(text, "General", "search_delay"), None);
        let out = rewrite(text, &[edit("General", "search_delay", "1")]);
        assert_eq!(out, format!("{text}search_delay = 1\n"));
    }

    #[test]
    fn get_reads_back_what_rewrite_wrote() {
        let out = rewrite(
            SAMPLE,
            &[
                edit("Providers", "usenet_retention", "6000"),
                edit("General", "search_delay", "1"),
                edit("Client", "nzb_downloader", "0"),
            ],
        );
        assert_eq!(get(&out, "Providers", "usenet_retention"), Some("6000"));
        assert_eq!(get(&out, "General", "search_delay"), Some("1"));
        assert_eq!(get(&out, "Client", "nzb_downloader"), Some("0"));
        assert_eq!(get(&out, "Providers", "extra"), Some("a, b"));
        assert_eq!(get(&out, "Client", "missing"), None);
    }

    #[test]
    fn an_edit_that_changes_nothing_is_byte_identical() {
        assert_eq!(
            rewrite(SAMPLE, &[edit("General", "search_delay", "5")]),
            SAMPLE
        );
    }

    #[test]
    fn keys_match_case_insensitively_and_sections_do_not() {
        let text = "[General]\nsearch_delay = 5\n";
        assert_eq!(get(text, "General", "SEARCH_DELAY"), Some("5"));
        assert_eq!(get(text, "general", "search_delay"), None);
        assert_eq!(
            rewrite(text, &[edit("General", "Search_Delay", "1")]),
            "[General]\nsearch_delay = 1\n"
        );
        assert_eq!(
            rewrite(text, &[edit("General", "Dynamic_Update", "0")]),
            "[General]\nsearch_delay = 5\ndynamic_update = 0\n"
        );
    }

    #[test]
    fn empty_values_as_configparser_writes_them() {
        // configparser writes an empty value as `key = ` with the trailing space.
        let text = "[Client]\nsab_host = \nsab_port =\n";
        assert_eq!(get(text, "Client", "sab_host"), Some(""));
        assert_eq!(get(text, "Client", "sab_port"), Some(""));
        assert_eq!(
            rewrite(
                text,
                &[
                    edit("Client", "sab_host", "h"),
                    edit("Client", "sab_port", "8080")
                ]
            ),
            "[Client]\nsab_host = h\nsab_port =8080\n"
        );
        assert_eq!(
            rewrite(
                "[Client]\nsab_host = h\n",
                &[edit("Client", "sab_host", "")]
            ),
            "[Client]\nsab_host = \n"
        );
    }

    #[test]
    fn the_first_delimiter_splits_and_later_ones_belong_to_the_value() {
        let text = "[General]\nhttp_root = http://h:8090/?a=b\n";
        assert_eq!(
            get(text, "General", "http_root"),
            Some("http://h:8090/?a=b")
        );
        assert_eq!(
            rewrite(text, &[edit("General", "http_root", "/m")]),
            "[General]\nhttp_root = /m\n"
        );
    }

    #[test]
    fn replacing_a_multiline_value_drops_its_continuation_lines() {
        let text = "[General]\nnotes = a\n\tb\n  c\nnext = 1\n";
        assert_eq!(
            rewrite(text, &[edit("General", "notes", "z")]),
            "[General]\nnotes = z\nnext = 1\n"
        );
    }
}

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

/// An edit that configparser could not read back as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    Section(String),
    Key(String),
    Value(String),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Section(s) => write!(f, "invalid config.ini section name {s:?}"),
            Self::Key(k) => write!(f, "invalid config.ini key {k:?}"),
            Self::Value(v) => write!(f, "invalid config.ini value {v:?}"),
        }
    }
}

impl std::error::Error for EditError {}

/// `text` with every edit applied; nothing is applied if any edit is invalid.
pub fn rewrite(text: &str, edits: &[Edit]) -> Result<String, EditError> {
    for e in edits {
        validate(e)?;
    }
    let mut out = text.to_string();
    for e in edits {
        out = apply(&out, e);
    }
    Ok(out)
}

fn validate(e: &Edit) -> Result<(), EditError> {
    let breaks = |s: &str| s.contains(['\r', '\n']);
    if e.section.is_empty() || breaks(&e.section) || e.section.contains(']') {
        return Err(EditError::Section(e.section.clone()));
    }
    if e.key.trim().is_empty()
        || e.key.trim() != e.key
        || breaks(&e.key)
        || e.key.contains(['=', ':'])
        || e.key.starts_with(['#', ';', '['])
    {
        return Err(EditError::Key(e.key.clone()));
    }
    // configparser strips values on read, so edge whitespace would not survive.
    if breaks(&e.value) || e.value.trim() != e.value {
        return Err(EditError::Value(e.value.clone()));
    }
    Ok(())
}

/// The value of `key` in `[section]` as configparser reads it: trimmed,
/// continuation lines joined with `\n`, and `%%` unescaped to `%`. `None` if
/// the value holds any other `%`, which BasicInterpolation rejects or expands.
pub fn get(text: &str, section: &str, key: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let entry = entries(&lines)
        .into_iter()
        .find(|en| en.section == Some(section) && en.key.eq_ignore_ascii_case(key))?;
    let first = lines[entry.line][entry.delim + 1..].trim();
    let mut value = vec![first];
    value.extend(
        lines[entry.line + 1..entry.end]
            .iter()
            .filter(|l| !is_comment(l))
            .map(|l| l.trim()),
    );
    unescape(&value.join("\n"))
}

fn unescape(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '%' && chars.next() != Some('%') {
            return None;
        }
        out.push(c);
    }
    Some(out)
}

/// A `key = value` line and the lines its value spans.
struct Entry<'a> {
    section: Option<&'a str>,
    key: &'a str,
    line: usize,
    /// Byte offset of the `=` or `:` in the key line.
    delim: usize,
    /// One past the value's last non-blank continuation line.
    end: usize,
}

/// A `[name]` header; configparser keeps the name's inner whitespace, runs the
/// name to the last `]`, and ignores text after it.
fn header(line: &str) -> Option<&str> {
    let rest = line.trim().strip_prefix('[')?;
    let name = &rest[..rest.rfind(']')?];
    (!name.is_empty()).then_some(name)
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with(['#', ';'])
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Every entry in `lines`, read with configparser's defaults. A non-blank line
/// indented deeper than the key line continues the value, even across blank
/// and comment lines (`empty_lines_in_values`); comments are skipped, not part
/// of the value.
fn entries<'a>(lines: &[&'a str]) -> Vec<Entry<'a>> {
    let mut out: Vec<Entry<'a>> = Vec::new();
    let mut section = None;
    let mut open: Option<usize> = None;
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() {
            continue;
        }
        if is_comment(line) {
            continue;
        }
        if let Some(at) = open {
            if indent(line) > at {
                if let Some(last) = out.last_mut() {
                    last.end = i + 1;
                }
                continue;
            }
        }
        open = None;
        if let Some(name) = header(line) {
            section = Some(name);
            continue;
        }
        if let Some(delim) = line.find(['=', ':']) {
            open = Some(indent(line));
            out.push(Entry {
                section,
                key: line[..delim].trim(),
                line: i,
                delim,
                end: i + 1,
            });
        }
    }
    out
}

fn apply(text: &str, e: &Edit) -> String {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    // Lines keep their own terminators so untouched lines round-trip exactly.
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    // Mylar writes through BasicInterpolation, which reads `%%` as `%`.
    let value = e.value.replace('%', "%%");
    let found = entries(&lines);
    let in_section: Vec<&Entry> = found
        .iter()
        .filter(|en| en.section == Some(e.section.as_str()))
        .collect();
    if let Some(en) = in_section
        .iter()
        .find(|en| en.key.eq_ignore_ascii_case(&e.key))
    {
        let line = lines[en.line];
        let body = line.trim_end_matches(['\r', '\n']);
        let pos = en.delim;
        let after = &body[pos + 1..];
        let mut gap = &after[..after.len() - after.trim_start().len()];
        if gap.is_empty() && !value.is_empty() && body[..pos].ends_with([' ', '\t']) {
            gap = " ";
        }
        let ending = &line[body.len()..];
        let replaced = format!("{}{gap}{value}{ending}", &body[..=pos]);
        let mut out = String::with_capacity(text.len());
        for (j, l) in lines.iter().enumerate() {
            if j == en.line {
                out.push_str(&replaced);
            } else if j < en.line || j >= en.end {
                out.push_str(l);
            }
        }
        return out;
    }
    // configparser lowercases keys on write.
    let entry = format!("{} = {value}", e.key.to_ascii_lowercase());
    let header_line = lines
        .iter()
        .rposition(|l| header(l.trim_end_matches(['\r', '\n'])) == Some(e.section.as_str()));
    let at = in_section
        .last()
        .map(|en| en.end)
        .or(header_line.map(|h| h + 1));
    match at {
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

    fn rw(text: &str, edits: &[Edit]) -> String {
        rewrite(text, edits).unwrap()
    }

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
        let out = rw(SAMPLE, &[edit("Providers", "usenet_retention", "6000")]);
        assert_eq!(
            out,
            SAMPLE.replace("usenet_retention = 3500", "usenet_retention = 6000")
        );
    }

    #[test]
    fn the_same_key_in_another_section_is_left_alone() {
        let text = "[A]\nk = 1\n[B]\nk = 2\n";
        assert_eq!(rw(text, &[edit("B", "k", "9")]), "[A]\nk = 1\n[B]\nk = 9\n");
    }

    #[test]
    fn keeps_the_lines_own_spacing_and_line_endings() {
        let text = "[General]\r\nsearch_delay=5\r\nx = y\r\n";
        assert_eq!(
            rw(text, &[edit("General", "search_delay", "1")]),
            "[General]\r\nsearch_delay=1\r\nx = y\r\n"
        );
    }

    #[test]
    fn a_missing_key_joins_the_end_of_its_section() {
        let out = rw(SAMPLE, &[edit("General", "dynamic_update", "0")]);
        assert_eq!(
            out,
            SAMPLE.replace(
                "search_delay = 5\n",
                "search_delay = 5\ndynamic_update = 0\n"
            )
        );
        let out = rw(
            "[Client]\nnzb_downloader = 3",
            &[edit("Client", "sab_host", "h")],
        );
        assert_eq!(out, "[Client]\nnzb_downloader = 3\nsab_host = h\n");
    }

    #[test]
    fn a_missing_section_is_appended() {
        let out = rw(
            "[General]\na = 1\n",
            &[edit("Providers", "usenet_retention", "6000")],
        );
        assert_eq!(
            out,
            "[General]\na = 1\n\n[Providers]\nusenet_retention = 6000\n"
        );
        assert_eq!(rw("", &[edit("S", "k", "v")]), "[S]\nk = v\n");
    }

    #[test]
    fn comments_and_continuations_are_not_keys() {
        let text = "[General]\n# search_delay = 9\nnotes = a\n  search_delay = 7\n";
        assert_eq!(get(text, "General", "search_delay").as_deref(), None);
        let out = rw(text, &[edit("General", "search_delay", "1")]);
        assert_eq!(out, format!("{text}search_delay = 1\n"));
    }

    #[test]
    fn get_reads_back_what_rewrite_wrote() {
        let out = rw(
            SAMPLE,
            &[
                edit("Providers", "usenet_retention", "6000"),
                edit("General", "search_delay", "1"),
                edit("Client", "nzb_downloader", "0"),
            ],
        );
        assert_eq!(
            get(&out, "Providers", "usenet_retention").as_deref(),
            Some("6000")
        );
        assert_eq!(get(&out, "General", "search_delay").as_deref(), Some("1"));
        assert_eq!(get(&out, "Client", "nzb_downloader").as_deref(), Some("0"));
        assert_eq!(get(&out, "Providers", "extra").as_deref(), Some("a, b"));
        assert_eq!(get(&out, "Client", "missing").as_deref(), None);
    }

    #[test]
    fn an_edit_that_changes_nothing_is_byte_identical() {
        assert_eq!(rw(SAMPLE, &[edit("General", "search_delay", "5")]), SAMPLE);
    }

    #[test]
    fn keys_match_case_insensitively_and_sections_do_not() {
        let text = "[General]\nsearch_delay = 5\n";
        assert_eq!(get(text, "General", "SEARCH_DELAY").as_deref(), Some("5"));
        assert_eq!(get(text, "general", "search_delay").as_deref(), None);
        assert_eq!(
            rw(text, &[edit("General", "Search_Delay", "1")]),
            "[General]\nsearch_delay = 1\n"
        );
        assert_eq!(
            rw(text, &[edit("General", "Dynamic_Update", "0")]),
            "[General]\nsearch_delay = 5\ndynamic_update = 0\n"
        );
    }

    #[test]
    fn empty_values_as_configparser_writes_them() {
        // configparser writes an empty value as `key = ` with the trailing space.
        let text = "[Client]\nsab_host = \nsab_port =\n";
        assert_eq!(get(text, "Client", "sab_host").as_deref(), Some(""));
        assert_eq!(get(text, "Client", "sab_port").as_deref(), Some(""));
        assert_eq!(
            rw(
                text,
                &[
                    edit("Client", "sab_host", "h"),
                    edit("Client", "sab_port", "8080")
                ]
            ),
            "[Client]\nsab_host = h\nsab_port = 8080\n"
        );
        assert_eq!(
            rw(
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
            get(text, "General", "http_root").as_deref(),
            Some("http://h:8090/?a=b")
        );
        assert_eq!(
            rw(text, &[edit("General", "http_root", "/m")]),
            "[General]\nhttp_root = /m\n"
        );
    }

    #[test]
    fn replacing_a_multiline_value_drops_its_continuation_lines() {
        let text = "[General]\nnotes = a\n\tb\n  c\nnext = 1\n";
        assert_eq!(
            rw(text, &[edit("General", "notes", "z")]),
            "[General]\nnotes = z\nnext = 1\n"
        );
    }

    #[test]
    fn invalid_edits_are_rejected_and_nothing_is_applied() {
        let bad = [
            (
                edit("Gen\neral", "k", "v"),
                EditError::Section("Gen\neral".into()),
            ),
            (edit("Gen]", "k", "v"), EditError::Section("Gen]".into())),
            (edit("General", "", "v"), EditError::Key("".into())),
            (edit("General", "a=b", "v"), EditError::Key("a=b".into())),
            (edit("General", "a:b", "v"), EditError::Key("a:b".into())),
            (edit("General", "k\r", "v"), EditError::Key("k\r".into())),
            (edit("General", " k", "v"), EditError::Key(" k".into())),
            (edit("General", "k\t", "v"), EditError::Key("k\t".into())),
            (edit("General", "#k", "v"), EditError::Key("#k".into())),
            (edit("General", ";k", "v"), EditError::Key(";k".into())),
            (edit("General", "[k", "v"), EditError::Key("[k".into())),
            (
                edit("General", "k", "a\nb"),
                EditError::Value("a\nb".into()),
            ),
            (edit("General", "k", " v"), EditError::Value(" v".into())),
            (edit("General", "k", "v\t"), EditError::Value("v\t".into())),
        ];
        for (e, err) in bad {
            let edits = [edit("General", "search_delay", "1"), e];
            assert_eq!(rewrite(SAMPLE, &edits), Err(err));
        }
    }

    #[test]
    fn percent_is_escaped_on_write_and_unescaped_on_read() {
        let out = rw(SAMPLE, &[edit("General", "search_delay", "50%")]);
        assert!(out.contains("search_delay = 50%%\n"));
        assert_eq!(get(&out, "General", "search_delay").as_deref(), Some("50%"));
        let out = rw(SAMPLE, &[edit("General", "fmt", "%s%%")]);
        assert!(out.contains("fmt = %%s%%%%\n"));
        assert_eq!(get(&out, "General", "fmt").as_deref(), Some("%s%%"));
    }

    #[test]
    fn a_blank_line_inside_a_multiline_value_does_not_end_it() {
        let text = "[General]\nnotes = a\n\n  b\n\nnext = 1\n";
        assert_eq!(get(text, "General", "notes").as_deref(), Some("a\n\nb"));
        assert_eq!(
            rw(text, &[edit("General", "notes", "z")]),
            "[General]\nnotes = z\n\nnext = 1\n"
        );
    }

    #[test]
    fn a_continuation_is_relative_to_the_key_lines_indent() {
        let text = "[General]\n  notes = a\n  other = b\n    c\n";
        assert_eq!(get(text, "General", "notes").as_deref(), Some("a"));
        assert_eq!(get(text, "General", "other").as_deref(), Some("b\nc"));
        assert_eq!(
            rw(text, &[edit("General", "other", "z")]),
            "[General]\n  notes = a\n  other = z\n"
        );
    }

    #[test]
    fn a_comment_does_not_end_a_multiline_value() {
        let text = "[General]\nnotes = a\n  # c\n  b = 2\n";
        assert_eq!(get(text, "General", "notes").as_deref(), Some("a\nb = 2"));
        assert_eq!(get(text, "General", "b"), None);
        assert_eq!(
            rw(text, &[edit("General", "notes", "z")]),
            "[General]\nnotes = z\n"
        );
        let text = "[General]\nnotes = a\n# c\n  b\n# trailing\nk = 1\n";
        assert_eq!(get(text, "General", "notes").as_deref(), Some("a\nb"));
        assert_eq!(
            rw(text, &[edit("General", "notes", "z")]),
            "[General]\nnotes = z\n# trailing\nk = 1\n"
        );
    }

    #[test]
    fn get_rejects_percent_other_than_escaped() {
        let at = |v: &str| get(&format!("[G]\nk = {v}\n"), "G", "k");
        assert_eq!(at("50%%").as_deref(), Some("50%"));
        assert_eq!(at("%%%%").as_deref(), Some("%%"));
        assert_eq!(at("50%"), None);
        assert_eq!(at("%(x)s"), None);
        assert_eq!(at("%%%"), None);
    }

    #[test]
    fn headers_run_to_the_last_bracket_and_ignore_trailing_text() {
        assert_eq!(get("[A] junk\nx = 1\n", "A", "x").as_deref(), Some("1"));
        assert_eq!(get("[A]x]\nx = 1\n", "A]x", "x").as_deref(), Some("1"));
    }

    #[test]
    fn section_names_keep_inner_whitespace() {
        let text = "[ General ]\nk = 1\n";
        assert_eq!(get(text, " General ", "k").as_deref(), Some("1"));
        assert_eq!(get(text, "General", "k"), None);
        assert_eq!(
            rw(text, &[edit("General", "k", "2")]),
            "[ General ]\nk = 1\n\n[General]\nk = 2\n"
        );
    }

    #[test]
    fn a_missing_key_goes_after_the_last_entry_not_trailing_comments() {
        let text = "[A]\nk = 1\n  more\n\n# about B\n[B]\nj = 2\n";
        assert_eq!(
            rw(text, &[edit("A", "n", "3")]),
            "[A]\nk = 1\n  more\nn = 3\n\n# about B\n[B]\nj = 2\n"
        );
        assert_eq!(
            rw("[A]\n# only a comment\n[B]\n", &[edit("A", "n", "3")]),
            "[A]\nn = 3\n# only a comment\n[B]\n"
        );
    }

    #[test]
    fn filling_an_empty_value_adds_one_space() {
        assert_eq!(
            rw(
                "[C]\nk =\nj=\n",
                &[edit("C", "k", "v"), edit("C", "j", "w")]
            ),
            "[C]\nk = v\nj=w\n"
        );
        assert_eq!(rw("[C]\nk = \n", &[edit("C", "k", "v")]), "[C]\nk = v\n");
    }
}

use plugin_toolkit::prelude::*;
use plugin_toolkit::{scrub, serde_json};

use crate::api::{LogPage, Mylar};

pub const DEFAULT_LINES: usize = 200;
pub const MAX_LINES: usize = 2500;
pub const MAX_BYTES: usize = 7 * 1024 * 1024;

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogsReport {
    pub name: String,
    pub grep: Option<String>,
    /// Newest lines skipped before this page.
    pub offset: usize,
    /// Lines in Mylar's in-memory log matching `grep`.
    pub matched: usize,
    pub returned: usize,
    /// Lines were dropped to hold `lines` or the response size cap.
    pub truncated: bool,
    /// `<time> <level> <message>`, oldest first.
    pub lines: Vec<String>,
}

pub async fn logs(
    name: &str,
    m: &Mylar,
    grep: Option<&str>,
    offset: usize,
    lines: usize,
) -> Result<LogsReport> {
    let lines = lines.clamp(1, MAX_LINES);
    let ini = m
        .config()
        .await
        .context("read Mylar's settings to scrub the secrets it logs")?;
    let page = m.log(grep.unwrap_or_default(), offset, lines).await?;
    Ok(build(name, grep, page, offset, lines, &ini.secrets()))
}

fn scrub_secrets(msg: &str, secrets: &[&str]) -> String {
    secrets.iter().fold(msg.to_string(), |out, s| {
        out.replace(s, scrub::REDACTED_TEXT)
            .replace(&plugin_toolkit::url::encode(s), scrub::REDACTED_TEXT)
    })
}

pub fn build(
    name: &str,
    grep: Option<&str>,
    page: LogPage,
    offset: usize,
    lines: usize,
    secrets: &[&str],
) -> LogsReport {
    let mut out = Vec::new();
    let mut bytes = 0;
    for (time, level, msg) in page.rows.into_iter().take(lines) {
        let line = scrub_secrets(&format!("{time} {level} {msg}"), secrets);
        bytes += serde_json::to_string(&line).map_or(line.len() * 6, |j| j.len()) + 1;
        if bytes > MAX_BYTES {
            break;
        }
        out.push(line);
    }
    out.reverse();
    LogsReport {
        name: name.to_string(),
        grep: grep.map(str::to_string),
        offset,
        matched: page.matched,
        returned: out.len(),
        truncated: offset + out.len() < page.matched,
        lines: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::matchers::{method, path, query_param};
    use crate::mock::{Mock, MockServer, ResponseTemplate};
    use plugin_toolkit::serde_json;

    fn row(i: usize, msg: &str) -> (String, String, String) {
        (
            format!("2026-10-19 00:00:{i:02}"),
            "INFO".into(),
            msg.into(),
        )
    }

    async fn config(server: &MockServer, rows: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path("/getConfig"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "aaData": rows })),
            )
            .mount(server)
            .await;
    }

    #[test]
    fn config_secrets_are_scrubbed_in_any_form() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            config(
                &server,
                serde_json::json!([["sab_apikey", "SAB/KEY+1"], ["sab_host", "h"]]),
            )
            .await;
            Mock::given(method("GET"))
                .and(path("/getLog"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "iTotalDisplayRecords": 4,
                    "aaData": [
                        ["t", "INFO", "raw SAB/KEY+1 here"],
                        ["t", "INFO", "url http://sab/api?mode=queue&apikey=OTHER&x=1"],
                        ["t", "INFO", "encoded q=SAB%2FKEY%2B1"],
                        ["t", "INFO", "nested next=%2Fapi%3Fapikey%3DNESTED"]
                    ]
                })))
                .mount(&server)
                .await;
            let m = Mylar::new(&server.uri(), "KEY", None);
            let r = logs("m", &m, None, 0, 10).await.unwrap();
            let all = r.lines.join("\n");
            for leak in ["SAB/KEY+1", "SAB%2FKEY%2B1", "OTHER", "NESTED"] {
                assert!(!all.contains(leak), "{leak}: {all}");
            }
        })
    }

    #[test]
    fn offset_pages_back() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            config(&server, serde_json::json!([])).await;
            Mock::given(method("GET"))
                .and(path("/getLog"))
                .and(query_param("iDisplayStart", "2"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "iTotalDisplayRecords": 3, "aaData": [["t0", "INFO", "oldest"]]
                })))
                .mount(&server)
                .await;
            let m = Mylar::new(&server.uri(), "KEY", None);
            let r = logs("m", &m, None, 2, 2).await.unwrap();
            assert_eq!((r.offset, r.returned, r.truncated), (2, 1, false));
        })
    }

    #[test]
    fn tail_is_oldest_first_and_filtered_server_side() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            config(&server, serde_json::json!([])).await;
            Mock::given(method("GET"))
                .and(path("/getLog"))
                .and(query_param("sSearch", "sab"))
                .and(query_param("iDisplayLength", "2"))
                .and(query_param("sSortDir_0", "desc"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "iTotalDisplayRecords": 5, "iTotalRecords": 90,
                    "aaData": [["t2", "WARNING", "sab newest"], ["t1", "INFO", "sab older"]]
                })))
                .mount(&server)
                .await;
            let m = Mylar::new(&server.uri(), "KEY", None);
            let r = logs("m", &m, Some("sab"), 0, 2).await.unwrap();
            assert_eq!(r.lines, ["t1 INFO sab older", "t2 WARNING sab newest"]);
            assert_eq!((r.matched, r.returned, r.truncated), (5, 2, true));
        })
    }

    #[test]
    fn line_cap_is_clamped() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            config(&server, serde_json::json!([])).await;
            Mock::given(method("GET"))
                .and(path("/getLog"))
                .and(query_param("iDisplayLength", &MAX_LINES.to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "iTotalDisplayRecords": 0, "aaData": []
                })))
                .mount(&server)
                .await;
            let m = Mylar::new(&server.uri(), "KEY", None);
            let r = logs("m", &m, None, 0, 1_000_000).await.unwrap();
            assert_eq!(r.returned, 0);
            assert!(!r.truncated);
        })
    }

    #[test]
    fn response_stays_under_eight_mib() {
        let big = "x".repeat(1024 * 1024);
        let rows: Vec<_> = (0..20).map(|i| row(i, &big)).collect();
        let r = build("m", None, LogPage { matched: 20, rows }, 0, MAX_LINES, &[]);
        let size = serde_json::to_vec(&r).unwrap().len();
        assert!(size < 8 * 1024 * 1024, "{size}");
        assert!(r.truncated && r.returned == 6);
        assert!(r.lines.last().unwrap().starts_with("2026-10-19 00:00:00"));
    }

    #[test]
    fn json_escaping_counts_toward_the_cap() {
        let quotes = "\"".repeat(1024 * 1024);
        let rows: Vec<_> = (0..8).map(|i| row(i, &quotes)).collect();
        let r = build("m", None, LogPage { matched: 8, rows }, 0, MAX_LINES, &[]);
        let size = serde_json::to_vec(&r).unwrap().len();
        assert!(size < 8 * 1024 * 1024, "{size}");
        assert!(r.truncated);
    }
}

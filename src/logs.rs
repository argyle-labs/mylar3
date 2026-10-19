use plugin_toolkit::prelude::*;

use crate::api::{LogPage, Mylar};

pub const DEFAULT_LINES: usize = 200;
pub const MAX_LINES: usize = 2500;
pub const MAX_BYTES: usize = 7 * 1024 * 1024;

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogsReport {
    pub name: String,
    pub grep: Option<String>,
    /// Lines in Mylar's in-memory log matching `grep`.
    pub matched: usize,
    pub returned: usize,
    /// Lines were dropped to hold `lines` or the response size cap.
    pub truncated: bool,
    /// `<time> <level> <message>`, oldest first.
    pub lines: Vec<String>,
}

pub async fn logs(name: &str, m: &Mylar, grep: Option<&str>, lines: usize) -> Result<LogsReport> {
    let lines = lines.clamp(1, MAX_LINES);
    let page = m.log(grep.unwrap_or_default(), lines).await?;
    Ok(build(name, grep, page, lines))
}

pub fn build(name: &str, grep: Option<&str>, page: LogPage, lines: usize) -> LogsReport {
    let mut out = Vec::new();
    let mut bytes = 0;
    for (time, level, msg) in page.rows.into_iter().take(lines) {
        let line = format!("{time} {level} {msg}");
        bytes += line.len() + 8;
        if bytes > MAX_BYTES {
            break;
        }
        out.push(line);
    }
    out.reverse();
    LogsReport {
        name: name.to_string(),
        grep: grep.map(str::to_string),
        matched: page.matched,
        returned: out.len(),
        truncated: out.len() < page.matched,
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

    #[test]
    fn tail_is_oldest_first_and_filtered_server_side() {
        crate::mock::run(async {
            let server = MockServer::start().await;
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
            let r = logs("m", &m, Some("sab"), 2).await.unwrap();
            assert_eq!(r.lines, ["t1 INFO sab older", "t2 WARNING sab newest"]);
            assert_eq!((r.matched, r.returned, r.truncated), (5, 2, true));
        })
    }

    #[test]
    fn line_cap_is_clamped() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/getLog"))
                .and(query_param("iDisplayLength", &MAX_LINES.to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "iTotalDisplayRecords": 0, "aaData": []
                })))
                .mount(&server)
                .await;
            let m = Mylar::new(&server.uri(), "KEY", None);
            let r = logs("m", &m, None, 1_000_000).await.unwrap();
            assert_eq!(r.returned, 0);
            assert!(!r.truncated);
        })
    }

    #[test]
    fn response_stays_under_eight_mib() {
        let big = "x".repeat(1024 * 1024);
        let rows: Vec<_> = (0..20).map(|i| row(i, &big)).collect();
        let r = build("m", None, LogPage { matched: 20, rows }, MAX_LINES);
        let size = serde_json::to_vec(&r).unwrap().len();
        assert!(size < 8 * 1024 * 1024, "{size}");
        assert!(r.truncated && r.returned == 6);
        assert!(r.lines.last().unwrap().starts_with("2026-10-19 00:00:00"));
    }
}

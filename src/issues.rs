use plugin_toolkit::prelude::*;

use crate::api::{IssueState, Mylar, Series, Snatch, Wanted};

pub const DEFAULT_LIMIT: usize = 200;
pub const MAX_LIMIT: usize = 1000;

const MAYBE_APPLIED: &str = "the write may already have been applied; check status before retrying";

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueReport {
    pub name: String,
    pub issue_id: String,
    pub comic_id: Option<String>,
    pub issue_number: Option<String>,
    pub annual: bool,
    /// Status before queueing; known only when resolved by series.
    pub previous_status: Option<String>,
    /// Mylar marked the issue Wanted and a search was requested.
    pub search_requested: bool,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailReport {
    pub name: String,
    pub issue_id: String,
    pub comic_id: String,
    /// Provider of the newest Snatched grab, now on Mylar's failed list.
    pub provider: Option<String>,
    /// Issue status re-read after the fail.
    pub status_after: Option<String>,
    /// `failed_auto` is on, so Mylar marked the issue Wanted and requested a search skipping the failed release.
    pub search_requested: bool,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WantedEntry {
    pub comic_id: Option<String>,
    pub issue_id: Option<String>,
    pub series: Option<String>,
    pub issue_number: Option<String>,
    pub release_date: Option<String>,
    pub annual: bool,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WantedReport {
    pub name: String,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub issues: Vec<WantedEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub issue_id: String,
    pub comic_id: Option<String>,
    pub series_name: Option<String>,
    pub number: Option<String>,
    pub status: Option<String>,
    pub annual: bool,
}

pub async fn resolve(
    m: &Mylar,
    issue_id: Option<&str>,
    series: Option<&str>,
    number: Option<&str>,
    annual: bool,
) -> Result<Resolved> {
    match (issue_id.filter(|s| !s.is_empty()), series, number) {
        (Some(_), None, None) if annual => {
            bail!("an annual is addressed by series and issue_number; Mylar queues annuals by ComicID")
        }
        (Some(id), None, None) => Ok(Resolved {
            issue_id: id.to_string(),
            comic_id: None,
            series_name: None,
            number: None,
            status: None,
            annual: false,
        }),
        (None, Some(series), Some(number)) => {
            let comic_id = match_series(&m.index().await?, series)?;
            let comic = m.comic(&comic_id).await?;
            let (list, kind) = match annual {
                true => (&comic.annuals, "annual"),
                false => (&comic.issues, "issue"),
            };
            let issue = match_issue(list, number)
                .ok_or_else(|| anyhow!("series {comic_id} has no {kind} numbered '{number}'"))?;
            Ok(Resolved {
                issue_id: issue
                    .id
                    .clone()
                    .ok_or_else(|| anyhow!("mylar listed {kind} '{number}' without an id"))?,
                series_name: comic.comic.first().and_then(|c| c.name.clone()),
                comic_id: Some(comic_id),
                number: issue.number.clone(),
                status: issue.status.clone(),
                annual,
            })
        }
        _ => bail!("give either issue_id, or series and issue_number"),
    }
}

pub fn match_series(index: &[Series], series: &str) -> Result<String> {
    let want = series.trim();
    if let Some(id) = index
        .iter()
        .filter_map(|s| s.id.as_deref())
        .find(|id| *id == want)
    {
        return Ok(id.to_string());
    }
    let hits: Vec<&Series> = index
        .iter()
        .filter(|s| {
            s.name
                .as_deref()
                .is_some_and(|n| n.trim().eq_ignore_ascii_case(want))
        })
        .collect();
    match hits.as_slice() {
        [one] => one
            .id
            .clone()
            .ok_or_else(|| anyhow!("mylar listed series '{want}' without an id")),
        [] => bail!("no series named '{want}' on the watchlist"),
        many => bail!(
            "series name '{want}' matches {} watchlist entries ({}); pass the comic id",
            many.len(),
            many.iter()
                .filter_map(|s| s.id.as_deref())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn normalize(n: &str) -> String {
    let n = n.trim().to_ascii_lowercase();
    let stripped = n.trim_start_matches('0');
    match stripped.chars().next() {
        Some(c) if c.is_ascii_digit() => stripped.to_string(),
        Some('.') => format!("0{stripped}"),
        None if !n.is_empty() => "0".to_string(),
        _ => n,
    }
}

pub fn match_issue<'a>(
    issues: impl IntoIterator<Item = &'a IssueState>,
    number: &str,
) -> Option<&'a IssueState> {
    let want = normalize(number);
    issues
        .into_iter()
        .find(|i| i.number.as_deref().map(normalize) == Some(want.clone()))
}

pub async fn queue(name: &str, m: &Mylar, target: Resolved) -> Result<QueueReport> {
    let sent = match (&target.annual, &target.comic_id, &target.number) {
        (true, Some(comic_id), Some(number)) => {
            m.queue_annual(
                comic_id,
                &target.issue_id,
                number,
                target.series_name.as_deref(),
            )
            .await
        }
        (true, _, _) => bail!("an annual needs its ComicID and number to queue"),
        (false, _, _) => m.queue_issue(&target.issue_id).await,
    };
    sent.map_err(|e| anyhow!("{e:#}; {MAYBE_APPLIED}"))?;
    Ok(QueueReport {
        name: name.to_string(),
        issue_id: target.issue_id,
        comic_id: target.comic_id,
        issue_number: target.number,
        annual: target.annual,
        previous_status: target.status,
        search_requested: true,
    })
}

pub fn newest_snatch<'a>(history: &'a [Snatch], issue_id: &str) -> Option<&'a Snatch> {
    history
        .iter()
        .filter(|s| s.issue_id.as_deref() == Some(issue_id))
        .filter(|s| s.status.as_deref() == Some("Snatched"))
        .max_by(|a, b| a.date_added.cmp(&b.date_added))
}

pub async fn fail(name: &str, m: &Mylar, target: Resolved) -> Result<FailReport> {
    if target.annual {
        bail!("failing an annual's release is not supported; Mylar's failed handling resolves issues only");
    }
    let ini = m
        .config()
        .await
        .context("read Mylar's settings to check failed download handling")?;
    if ini.flag("failed_download_handling") != Some(true) {
        bail!(
            "mylar has failed_download_handling off, so its search ignores the failed list; \
             enable it (mylar3.configure --failed-download-handling true) before failing a release"
        );
    }
    let history = m.history().await?;
    let snatch = newest_snatch(&history, &target.issue_id).ok_or_else(|| {
        anyhow!(
            "issue {} has no Snatched grab in Mylar's history, so there is no release to fail; \
             use mylar3.issue.queue to search again",
            target.issue_id
        )
    })?;
    let comic_id = target
        .comic_id
        .clone()
        .or_else(|| snatch.comic_id.clone())
        .ok_or_else(|| {
            anyhow!(
                "mylar's history for issue {} has no ComicID",
                target.issue_id
            )
        })?;
    m.failed_handling(&comic_id, &target.issue_id)
        .await
        .map_err(|e| anyhow!("{e:#}; {MAYBE_APPLIED}"))?;
    let comic = m.comic(&comic_id).await?;
    let status_after = comic
        .issues
        .iter()
        .find(|i| i.id.as_deref() == Some(target.issue_id.as_str()))
        .and_then(|i| i.status.clone());
    let after = m.history().await?;
    let same_grab =
        newest_snatch(&after, &target.issue_id).map(|s| &s.date_added) == Some(&snatch.date_added);
    if status_after.as_deref() == Some("Snatched") && same_grab {
        bail!(
            "mylar accepted /failed_handling but issue {} is still Snatched by the same grab; \
             Mylar did not fail it (see mylar3.logs)",
            target.issue_id
        );
    }
    Ok(FailReport {
        name: name.to_string(),
        issue_id: target.issue_id,
        comic_id,
        provider: snatch.provider.clone(),
        status_after,
        search_requested: ini.flag("failed_auto") == Some(true),
    })
}

pub async fn wanted(name: &str, m: &Mylar, offset: usize, limit: usize) -> Result<WantedReport> {
    Ok(build_wanted(name, m.wanted().await?, offset, limit))
}

pub fn build_wanted(name: &str, w: Wanted, offset: usize, limit: usize) -> WantedReport {
    let limit = limit.clamp(1, MAX_LIMIT);
    let all: Vec<WantedEntry> = w
        .issues
        .into_iter()
        .map(|i| (i, false))
        .chain(w.annuals.into_iter().map(|i| (i, true)))
        .map(|(i, annual)| WantedEntry {
            comic_id: i.comic_id,
            issue_id: i.issue_id,
            series: i.comic_name,
            issue_number: i.issue_number,
            release_date: i.release_date,
            annual,
        })
        .collect();
    WantedReport {
        name: name.to_string(),
        total: all.len(),
        offset,
        limit,
        issues: all.into_iter().skip(offset).take(limit).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::matchers::{method, path, query_param};
    use crate::mock::{Mock, MockServer, ResponseTemplate};
    use plugin_toolkit::serde_json::{self, json};

    fn client(server: &MockServer) -> Mylar {
        Mylar::new(&server.uri(), "KEY", None)
    }

    async fn api(server: &MockServer, cmd: &str, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("cmd", cmd))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    async fn watchlist(server: &MockServer) {
        api(
            server,
            "getIndex",
            json!({"success": true, "data": [
                {"id": "100", "name": "Saga"},
                {"id": "200", "name": "Paper Girls"},
                {"id": "300", "name": "Paper Girls"}
            ]}),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("cmd", "getComic"))
            .and(query_param("id", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true, "data": {
                    "comic": [{"id": "100", "name": "Saga"}],
                    "issues": [
                        {"id": "9001", "number": "1", "status": "Skipped"},
                        {"id": "9012", "number": "12", "status": "Snatched"}
                    ],
                    "annuals": [{"id": "9100", "number": "1", "status": "Skipped"}]
                }
            })))
            .mount(server)
            .await;
    }

    fn series(id: &str, name: &str) -> Series {
        Series {
            id: Some(id.into()),
            name: Some(name.into()),
        }
    }

    #[test]
    fn series_matches_id_then_unique_name() {
        let index = [series("1", "Saga"), series("2", "X"), series("3", "X")];
        assert_eq!(match_series(&index, "2").unwrap(), "2");
        assert_eq!(match_series(&index, " saga ").unwrap(), "1");
        let err = match_series(&index, "x").unwrap_err().to_string();
        assert!(err.contains("2 watchlist entries (2, 3)"), "{err}");
        assert!(match_series(&index, "nope").is_err());
    }

    #[test]
    fn issue_numbers_ignore_leading_zeros() {
        let issues = [
            IssueState {
                id: Some("a".into()),
                number: Some("001".into()),
                status: None,
            },
            IssueState {
                id: Some("b".into()),
                number: Some("0.5".into()),
                status: None,
            },
            IssueState {
                id: Some("c".into()),
                number: Some("0".into()),
                status: None,
            },
        ];
        let id = |n| match_issue(&issues, n).and_then(|i| i.id.as_deref());
        assert_eq!(id("1"), Some("a"));
        assert_eq!(id(".5"), Some("b"));
        assert_eq!(id("0.5"), Some("b"));
        assert_eq!(id("00"), Some("c"));
        assert_eq!(id("2"), None);
    }

    #[test]
    fn resolve_needs_one_addressing_mode() {
        crate::mock::run(async {
            let m = Mylar::new("http://127.0.0.1:1", "KEY", None);
            for (id, s, n) in [
                (Some("1"), Some("Saga"), Some("1")),
                (None, Some("Saga"), None),
                (None, None, None),
            ] {
                let err = resolve(&m, id, s, n, false).await.unwrap_err().to_string();
                assert!(err.contains("either issue_id"), "{err}");
            }
            let r = resolve(&m, Some("42"), None, None, false).await.unwrap();
            assert_eq!(r.issue_id, "42");
            assert!(!r.annual);
            let err = resolve(&m, Some("42"), None, None, true)
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("series and issue_number"), "{err}");
        })
    }

    #[test]
    fn resolve_by_series_and_number() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            watchlist(&server).await;
            let m = client(&server);
            let r = resolve(&m, None, Some("Saga"), Some("012"), false)
                .await
                .unwrap();
            assert_eq!(r.issue_id, "9012");
            assert_eq!(r.comic_id.as_deref(), Some("100"));
            assert_eq!(r.status.as_deref(), Some("Snatched"));
            let r = resolve(&m, None, Some("Saga"), Some("1"), false)
                .await
                .unwrap();
            assert_eq!(r.issue_id, "9001");
            let r = resolve(&m, None, Some("100"), Some("1"), true)
                .await
                .unwrap();
            assert_eq!(r.issue_id, "9100");
            assert!(r.annual);
            assert_eq!(r.series_name.as_deref(), Some("Saga"));
            let err = resolve(&m, None, Some("Saga"), Some("12"), true)
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("no annual numbered '12'"), "{err}");
            let err = resolve(&m, None, Some("Saga"), Some("99"), false)
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("no issue numbered '99'"), "{err}");
            let err = resolve(&m, None, Some("Paper Girls"), Some("1"), false)
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("pass the comic id"), "{err}");
        })
    }

    #[test]
    fn queue_sends_queue_issue_and_checks_ok() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api"))
                .and(query_param("apikey", "KEY"))
                .and(query_param("cmd", "queueIssue"))
                .and(query_param("id", "9001"))
                .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
                .mount(&server)
                .await;
            let m = client(&server);
            let target = Resolved {
                issue_id: "9001".into(),
                comic_id: Some("100".into()),
                series_name: Some("Saga".into()),
                number: Some("1".into()),
                status: Some("Skipped".into()),
                annual: false,
            };
            let r = queue("m", &m, target).await.unwrap();
            assert!(r.search_requested);
            assert_eq!(r.previous_status.as_deref(), Some("Skipped"));
        })
    }

    #[test]
    fn queue_annual_uses_the_web_want_ann_route() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/queueit"))
                .and(query_param("mode", "want_ann"))
                .and(query_param("ComicID", "100"))
                .and(query_param("IssueID", "9100"))
                .and(query_param("ComicIssue", "1"))
                .and(query_param("ComicName", "Saga"))
                .respond_with(ResponseTemplate::new(200))
                .mount(&server)
                .await;
            let target = Resolved {
                issue_id: "9100".into(),
                comic_id: Some("100".into()),
                series_name: Some("Saga".into()),
                number: Some("1".into()),
                status: Some("Skipped".into()),
                annual: true,
            };
            let r = queue("m", &client(&server), target).await.unwrap();
            assert!(r.annual && r.search_requested);
            let reqs = server.received_requests().await.unwrap();
            assert!(reqs.iter().all(|r| r.url.path() != "/api"));
        })
    }

    #[test]
    fn queue_transport_error_warns_the_write_may_have_applied() {
        crate::mock::run(async {
            let m = Mylar::new("http://127.0.0.1:1", "KEY", None);
            let err = queue("m", &m, target("1")).await.unwrap_err().to_string();
            assert!(err.contains("may already have been applied"), "{err}");
        })
    }

    #[test]
    fn queue_surfaces_a_refusal() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            api(
                &server,
                "queueIssue",
                json!({"success": false, "error": {"code": 460, "message": "Missing parameter: id"}}),
            )
            .await;
            let err = client(&server)
                .queue_issue("x")
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("Missing parameter: id"), "{err}");
        })
    }

    async fn config(server: &MockServer, handling: &str, auto: &str) {
        Mock::given(method("GET"))
            .and(path("/getConfig"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "aaData": [["FAILED_DOWNLOAD_HANDLING", handling], ["FAILED_AUTO", auto]]
            })))
            .mount(server)
            .await;
    }

    fn target(id: &str) -> Resolved {
        Resolved {
            issue_id: id.into(),
            comic_id: None,
            series_name: None,
            number: None,
            status: None,
            annual: false,
        }
    }

    async fn snatched_twice(server: &MockServer) {
        api(
            server,
            "getHistory",
            json!({"success": true, "data": [
                {"IssueID": "9012", "ComicID": "100", "Status": "Snatched",
                 "Provider": "old", "DateAdded": "2026-10-01 10:00:00"},
                {"IssueID": "9012", "ComicID": "100", "Status": "Snatched",
                 "Provider": "nzbgeek", "DateAdded": "2026-10-02 10:00:00"}
            ]}),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/failed_handling"))
            .and(query_param("ComicID", "100"))
            .and(query_param("IssueID", "9012"))
            .respond_with(ResponseTemplate::new(200))
            .mount(server)
            .await;
    }

    async fn issue_status(server: &MockServer, status: &str) {
        api(
            server,
            "getComic",
            json!({"success": true, "data": {
                "comic": [{"id": "100", "name": "Saga"}],
                "issues": [{"id": "9012", "number": "12", "status": status}],
                "annuals": []
            }}),
        )
        .await;
    }

    #[test]
    fn fail_errors_when_the_issue_is_unchanged() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            config(&server, "True", "True").await;
            snatched_twice(&server).await;
            issue_status(&server, "Snatched").await;
            let err = fail("m", &client(&server), target("9012"))
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("still Snatched by the same grab"), "{err}");
        })
    }

    #[test]
    fn fail_refuses_annuals() {
        crate::mock::run(async {
            let m = Mylar::new("http://127.0.0.1:1", "KEY", None);
            let mut t = target("9100");
            t.annual = true;
            let err = fail("m", &m, t).await.unwrap_err().to_string();
            assert!(err.contains("annual"), "{err}");
        })
    }

    #[test]
    fn fail_blocklists_the_snatched_release() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            config(&server, "True", "True").await;
            snatched_twice(&server).await;
            issue_status(&server, "Failed").await;
            let r = fail("m", &client(&server), target("9012")).await.unwrap();
            assert_eq!(r.comic_id, "100");
            assert_eq!(r.provider.as_deref(), Some("nzbgeek"));
            assert_eq!(r.status_after.as_deref(), Some("Failed"));
            assert!(r.search_requested);
        })
    }

    #[test]
    fn fail_refuses_without_failed_download_handling() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            config(&server, "False", "True").await;
            let err = fail("m", &client(&server), target("9012"))
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("failed_download_handling off"), "{err}");
        })
    }

    #[test]
    fn fail_refuses_an_issue_without_a_snatch() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            config(&server, "True", "False").await;
            api(
                &server,
                "getHistory",
                json!({"success": true, "data": [
                    {"IssueID": "9012", "ComicID": "100", "Status": "Post-Processed"}
                ]}),
            )
            .await;
            let err = fail("m", &client(&server), target("9012"))
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("no Snatched grab"), "{err}");
            let reqs = server.received_requests().await.unwrap();
            assert!(reqs.iter().all(|r| r.url.path() != "/failed_handling"));
        })
    }

    #[test]
    fn failed_handling_names_forms_login() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/failed_handling"))
                .respond_with(ResponseTemplate::new(200).set_body_string("<html>login</html>"))
                .mount(&server)
                .await;
            let err = client(&server)
                .failed_handling("1", "2")
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("forms login"), "{err}");
        })
    }

    #[test]
    fn wanted_lists_issues_then_annuals_paged() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            api(
                &server,
                "getWanted",
                json!({
                    "issues": [
                        {"ComicName": "Saga", "Issue_Number": "1", "ComicID": "100", "IssueID": "9001"},
                        {"ComicName": "Saga", "Issue_Number": "2", "ComicID": "100", "IssueID": "9002"}
                    ],
                    "annuals": [{"ComicName": "Saga Annual", "Issue_Number": "1", "IssueID": "9100"}]
                }),
            )
            .await;
            let r = wanted("m", &client(&server), 1, 5).await.unwrap();
            assert_eq!(r.total, 3);
            assert_eq!(r.issues.len(), 2);
            assert_eq!(r.issues[0].issue_id.as_deref(), Some("9002"));
            assert!(r.issues[1].annual);
        })
    }
}

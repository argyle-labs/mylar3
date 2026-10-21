use plugin_toolkit::prelude::*;

use crate::api::{ArcIssue, IssueState, Mylar, Series, Snatch, Wanted};

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
    /// Story arc the issue was queued through, for a reading-list issue off the watchlist.
    pub story_arc: Option<String>,
    /// Status before queueing; known only when resolved by series.
    pub previous_status: Option<String>,
    /// Mylar marked the issue Wanted and a search was requested.
    pub search_requested: bool,
    /// Story-arc issue was listed Wanted before queueing.
    pub was_wanted: bool,
    /// Story-arc issue is listed Wanted after queueing.
    pub wanted_after: bool,
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
    /// `failed_auto` is on, so Mylar requested a search skipping the failed release.
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
    pub story_arc: Option<ArcTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArcTarget {
    pub name: String,
    pub year: String,
}

pub fn story_arc_id(issue_id: &str) -> Option<&str> {
    match issue_id.split_once('_') {
        Some((arc, rest)) if !arc.is_empty() && !rest.is_empty() => Some(arc),
        _ => None,
    }
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
            story_arc: None,
        }),
        (Some(id), Some(series), Some(number)) if !annual && story_arc_id(id).is_some() => {
            let arc_id = story_arc_id(id).unwrap_or_default();
            let rows = m.story_arc(arc_id).await?;
            let row = match match_arc_issue(&rows, series, number).as_slice() {
                [] => bail!("story arc {arc_id} has no issue '{series}' #{number}"),
                [row] => *row,
                many => bail!(
                    "story arc {arc_id} lists '{series}' #{number} {} times and Mylar's arc listing \
                     carries no IssueArcID to tell them apart; series and issue_number must match \
                     exactly one row of the arc {id} belongs to",
                    many.len()
                ),
            };
            let year = row
                .issue_date
                .as_deref()
                .and_then(|d| d.get(..4))
                .filter(|y| y.bytes().all(|b| b.is_ascii_digit()) && *y != "0000")
                .ok_or_else(|| {
                    anyhow!(
                        "story arc {arc_id} lists '{series}' #{number} without an issue date, and \
                         Mylar's story-arc queue needs the year; refresh the arc in Mylar first"
                    )
                })?
                .to_string();
            Ok(Resolved {
                issue_id: id.to_string(),
                comic_id: None,
                series_name: row.comic_name.clone(),
                number: row.issue_number.clone(),
                status: None,
                annual: false,
                story_arc: Some(ArcTarget {
                    name: row
                        .story_arc
                        .clone()
                        .ok_or_else(|| anyhow!("mylar listed story arc {arc_id} without a name"))?,
                    year,
                }),
            })
        }
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
                story_arc: None,
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

pub fn match_arc_issue<'a>(rows: &'a [ArcIssue], series: &str, number: &str) -> Vec<&'a ArcIssue> {
    let want = normalize(number);
    rows.iter()
        .filter(|r| {
            r.comic_name
                .as_deref()
                .is_some_and(|n| n.trim().eq_ignore_ascii_case(series.trim()))
                && r.issue_number.as_deref().map(normalize) == Some(want.clone())
        })
        .collect()
}

async fn arc_wanted(m: &Mylar, issue_arc_id: &str) -> Result<bool> {
    let arcs = m.wanted_arcs().await?.story_arcs.ok_or_else(|| {
        anyhow!(
            "mylar's getWanted lists no story arcs, so the queued arc issue cannot be read back; \
             enable upcoming_storyarcs in Mylar"
        )
    })?;
    Ok(arcs.iter().any(|a| {
        a.issue_arc_id.as_deref() == Some(issue_arc_id) && a.status.as_deref() == Some("Wanted")
    }))
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
    if let Some(arc) = &target.story_arc {
        let was_wanted = arc_wanted(m, &target.issue_id).await?;
        m.queue_arc_issue(
            &target.issue_id,
            &arc.name,
            target.series_name.as_deref().unwrap_or_default(),
            target.number.as_deref().unwrap_or_default(),
            &arc.year,
        )
        .await
        .map_err(|e| anyhow!("{e:#}; {MAYBE_APPLIED}"))?;
        let mut wanted_after = false;
        for _ in 0..10 {
            wanted_after = arc_wanted(m, &target.issue_id)
                .await
                .map_err(|e| anyhow!("{e:#}; {MAYBE_APPLIED}"))?;
            if wanted_after {
                break;
            }
            plugin_toolkit::time::sleep(std::time::Duration::from_millis(300)).await;
        }
        return Ok(QueueReport {
            name: name.to_string(),
            issue_id: target.issue_id,
            comic_id: None,
            issue_number: target.number,
            annual: false,
            story_arc: Some(arc.name.clone()),
            previous_status: None,
            search_requested: wanted_after,
            was_wanted,
            wanted_after,
        });
    }
    if let Some(arc_id) = story_arc_id(&target.issue_id) {
        bail!(
            "{} is a story-arc issue id (arc {arc_id}); Mylar's queueIssue would write it into the \
             watchlist issues table, so pass series and issue_number as listed in the arc to queue \
             it through the story-arc route",
            target.issue_id
        );
    }
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
        story_arc: None,
        previous_status: target.status,
        search_requested: true,
        was_wanted: false,
        wanted_after: false,
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
    if story_arc_id(&target.issue_id).is_some() {
        bail!(
            "{} is a story-arc issue id; Mylar's failed handling resolves watchlist issues only",
            target.issue_id
        );
    }
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
                story_arc: None,
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
                story_arc: None,
            };
            let r = queue("m", &client(&server), target).await.unwrap();
            assert!(r.annual && r.search_requested);
            let reqs = server.received_requests().await.unwrap();
            assert!(reqs.iter().all(|r| r.url.path() != "/api"));
        })
    }

    async fn arc(server: &MockServer) {
        api(
            server,
            "getStoryArc",
            json!([
                {"StoryArc": "Secret Wars", "ComicName": "Defenders", "IssueNumber": "152",
                 "IssueDate": "1986-02-01", "IssueID": "777"},
                {"StoryArc": "Secret Wars", "ComicName": "Saga", "IssueNumber": "1",
                 "IssueDate": "2012-03-01", "IssueID": "9001"}
            ]),
        )
        .await;
    }

    #[test]
    fn story_arc_ids_split_on_underscore() {
        assert_eq!(story_arc_id("55688_2753"), Some("55688"));
        assert_eq!(story_arc_id("9001"), None);
        assert_eq!(story_arc_id("_1"), None);
        assert_eq!(story_arc_id("1_"), None);
    }

    async fn arc_wanted(server: &MockServer, statuses: serde_json::Value, times: usize) {
        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("cmd", "getWanted"))
            .and(query_param("story_arcs", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(statuses))
            .up_to_n_times(times)
            .mount(server)
            .await;
    }

    async fn arc_rows(server: &MockServer, rows: serde_json::Value) -> Mylar {
        api(server, "getStoryArc", rows).await;
        client(server)
    }

    #[test]
    fn arc_issue_matching_several_rows_is_refused() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            let row = json!({"StoryArc": "Secret Wars", "ComicName": "Defenders",
                             "IssueNumber": "152", "IssueDate": "1986-02-01"});
            let m = arc_rows(&server, json!([row, row])).await;
            let err = resolve(
                &m,
                Some("55688_2753"),
                Some("Defenders"),
                Some("152"),
                false,
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(err.contains("2 times"), "{err}");
        })
    }

    #[test]
    fn arc_issue_without_a_usable_date_is_refused() {
        crate::mock::run(async {
            for date in [json!(null), json!(""), json!("0000-00-00")] {
                let server = MockServer::start().await;
                let m = arc_rows(
                    &server,
                    json!([{"StoryArc": "Secret Wars", "ComicName": "Defenders",
                            "IssueNumber": "152", "IssueDate": date}]),
                )
                .await;
                let err = resolve(
                    &m,
                    Some("55688_2753"),
                    Some("Defenders"),
                    Some("152"),
                    false,
                )
                .await
                .unwrap_err()
                .to_string();
                assert!(err.contains("without an issue date"), "{err}");
            }
        })
    }

    #[test]
    fn arc_queue_without_a_wanted_readback_reports_no_search() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            arc(&server).await;
            arc_wanted(&server, json!({"issues": [], "story_arcs": []}), 100).await;
            Mock::given(method("GET"))
                .and(path("/queueit"))
                .respond_with(ResponseTemplate::new(200))
                .mount(&server)
                .await;
            let m = client(&server);
            let t = resolve(
                &m,
                Some("55688_2753"),
                Some("Defenders"),
                Some("152"),
                false,
            )
            .await
            .unwrap();
            let r = queue("m", &m, t).await.unwrap();
            assert!(!r.search_requested && !r.was_wanted && !r.wanted_after);
        })
    }

    #[test]
    fn arc_readback_failure_after_queueing_warns_the_write_may_have_applied() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            arc(&server).await;
            arc_wanted(&server, json!({"issues": [], "story_arcs": []}), 1).await;
            arc_wanted(&server, json!({"issues": []}), 100).await;
            Mock::given(method("GET"))
                .and(path("/queueit"))
                .respond_with(ResponseTemplate::new(200))
                .mount(&server)
                .await;
            let m = client(&server);
            let t = resolve(
                &m,
                Some("55688_2753"),
                Some("Defenders"),
                Some("152"),
                false,
            )
            .await
            .unwrap();
            let err = queue("m", &m, t).await.unwrap_err().to_string();
            assert!(err.contains("may already have been applied"), "{err}");
        })
    }

    #[test]
    fn arc_queue_without_story_arcs_in_get_wanted_is_refused_before_any_write() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            arc(&server).await;
            arc_wanted(&server, json!({"issues": []}), 100).await;
            let m = client(&server);
            let t = resolve(
                &m,
                Some("55688_2753"),
                Some("Defenders"),
                Some("152"),
                false,
            )
            .await
            .unwrap();
            let err = queue("m", &m, t).await.unwrap_err().to_string();
            assert!(err.contains("upcoming_storyarcs"), "{err}");
            let reqs = server.received_requests().await.unwrap();
            assert!(reqs.iter().all(|r| r.url.path() != "/queueit"));
        })
    }

    #[test]
    fn arc_issue_queues_through_the_readlist_route() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            arc(&server).await;
            arc_wanted(&server, json!({"issues": [], "story_arcs": []}), 1).await;
            arc_wanted(
                &server,
                json!({"issues": [], "story_arcs": [{"IssueArcID": "55688_2753", "Status": "Wanted"}]}),
                100,
            )
            .await;
            Mock::given(method("GET"))
                .and(path("/queueit"))
                .and(query_param("mode", "readlist"))
                .and(query_param("IssueArcID", "55688_2753"))
                .and(query_param("SARC", "Secret Wars"))
                .and(query_param("ComicName", "Defenders"))
                .and(query_param("ComicIssue", "152"))
                .and(query_param("ComicYear", "1986"))
                .respond_with(ResponseTemplate::new(200))
                .mount(&server)
                .await;
            let m = client(&server);
            let t = resolve(
                &m,
                Some("55688_2753"),
                Some("defenders"),
                Some("0152"),
                false,
            )
            .await
            .unwrap();
            assert_eq!(
                t.story_arc,
                Some(ArcTarget {
                    name: "Secret Wars".into(),
                    year: "1986".into()
                })
            );
            let r = queue("m", &m, t).await.unwrap();
            assert_eq!(r.story_arc.as_deref(), Some("Secret Wars"));
            assert!(r.search_requested && r.comic_id.is_none());
            assert!(!r.was_wanted && r.wanted_after);
            let reqs = server.received_requests().await.unwrap();
            assert!(reqs
                .iter()
                .all(|r| r.url.query().is_none_or(|q| !q.contains("queueIssue"))));
            let err = resolve(&m, Some("55688_2753"), Some("Defenders"), Some("9"), false)
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("no issue 'Defenders' #9"), "{err}");
        })
    }

    #[test]
    fn arc_issue_id_alone_is_refused_before_any_write() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            let m = client(&server);
            let t = resolve(&m, Some("55688_2753"), None, None, false)
                .await
                .unwrap();
            let err = queue("m", &m, t.clone()).await.unwrap_err().to_string();
            assert!(err.contains("story-arc issue id (arc 55688)"), "{err}");
            let err = fail("m", &m, t).await.unwrap_err().to_string();
            assert!(err.contains("watchlist issues only"), "{err}");
            assert!(server.received_requests().await.unwrap().is_empty());
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
            story_arc: None,
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

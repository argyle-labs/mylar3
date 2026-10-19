use plugin_toolkit::prelude::*;
use plugin_toolkit::time::Timestamp;

use crate::api::{ConfigIni, Mylar, Snatch};
use crate::sab::{HistorySlot, Sab};
use crate::status;

pub const DEFAULT_CATEGORY: &str = "comics";
const CANDIDATES: usize = 3;

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SabEntry {
    pub nzo_id: Option<String>,
    pub name: Option<String>,
    pub status: Option<String>,
    pub fail_message: Option<String>,
    pub storage: Option<String>,
    /// Unix seconds.
    pub completed: Option<i64>,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciledSnatch {
    pub issue_id: String,
    pub comic: String,
    pub issue: String,
    /// Mylar's local time.
    pub snatched_at: String,
    pub provider: Option<String>,
    /// Mylar's `Hash` column; the SAB nzo_id when Mylar stored one.
    pub nzo_id: Option<String>,
    /// Snatched rows Mylar holds for this issue.
    pub grabs: usize,
    /// `sab_completed`, `sab_failed`, `sab_pending`, `sab_missing` or `name_mismatch`.
    pub class: String,
    /// `nzo_id` or `name`.
    pub matched_by: Option<String>,
    /// Newest matching SAB history entry.
    pub sab: Option<SabEntry>,
    /// SAB history entries matching this issue.
    pub sab_matches: usize,
    /// For `name_mismatch`: newest SAB job names for the series that carry no matching issue number.
    pub candidates: Vec<String>,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReconcileSummary {
    pub total: usize,
    pub sab_completed: usize,
    pub sab_failed: usize,
    pub sab_pending: usize,
    pub sab_missing: usize,
    pub name_mismatch: usize,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    pub name: String,
    pub threshold_hours: u32,
    /// SAB base URL as Mylar holds it.
    pub sab_host: String,
    pub sab_category: String,
    /// SAB history entries read for the category.
    pub sab_history_entries: usize,
    pub summary: ReconcileSummary,
    pub rows: Vec<ReconciledSnatch>,
}

pub async fn reconcile(name: &str, m: &Mylar, stuck_hours: u32) -> Result<ReconcileReport> {
    let ini = m.config().await?;
    let (host, key, category) = sab_settings(&ini)?;
    let history = m.history().await?;
    let sab = Sab::new(host, key);
    let slots = sab.history(&category).await?;
    Ok(build(
        name,
        sab.base(),
        &category,
        &history,
        &slots,
        stuck_hours,
        Timestamp::now(),
    ))
}

fn sab_settings(ini: &ConfigIni) -> Result<(&str, &str, String)> {
    let host = ini
        .get("sab_host")
        .ok_or_else(|| anyhow!("mylar has no sab_host set"))?;
    let key = ini
        .get("sab_apikey")
        .ok_or_else(|| anyhow!("mylar has no sab_apikey set"))?;
    let category = ini
        .get("sab_category")
        .unwrap_or(DEFAULT_CATEGORY)
        .to_string();
    Ok((host, key, category))
}

pub fn build(
    name: &str,
    sab_host: &str,
    category: &str,
    history: &[Snatch],
    slots: &[HistorySlot],
    stuck_hours: u32,
    now: Timestamp,
) -> ReconcileReport {
    let (stuck, grabs) = status::stuck_snatches(history, stuck_hours, now);
    let named: Vec<(Vec<String>, &HistorySlot)> =
        slots.iter().map(|s| (tokens(slot_name(s)), s)).collect();
    let mut summary = ReconcileSummary::default();
    let rows: Vec<ReconciledSnatch> = stuck
        .into_iter()
        .map(|s| {
            let id = s.issue_id.as_deref().unwrap_or_default();
            let row = classify(s, grabs.get(id).copied().unwrap_or_default(), &named);
            summary.total += 1;
            match row.class.as_str() {
                "sab_completed" => summary.sab_completed += 1,
                "sab_failed" => summary.sab_failed += 1,
                "sab_pending" => summary.sab_pending += 1,
                "name_mismatch" => summary.name_mismatch += 1,
                _ => summary.sab_missing += 1,
            }
            row
        })
        .collect();
    ReconcileReport {
        name: name.to_string(),
        threshold_hours: stuck_hours,
        sab_host: sab_host.to_string(),
        sab_category: category.to_string(),
        sab_history_entries: slots.len(),
        summary,
        rows,
    }
}

fn slot_name(s: &HistorySlot) -> &str {
    s.name
        .as_deref()
        .or(s.nzb_name.as_deref())
        .unwrap_or_default()
}

fn classify(s: &Snatch, grabs: usize, named: &[(Vec<String>, &HistorySlot)]) -> ReconciledSnatch {
    let nzo_id = s
        .hash
        .clone()
        .filter(|h| !h.trim().is_empty() && h != "None");
    let comic = s.comic_name.clone().unwrap_or_default();
    let issue = s.issue_number.clone().unwrap_or_default();
    let comic_tokens = series_tokens(&comic);
    let by_id: Vec<&HistorySlot> = nzo_id
        .as_deref()
        .map(|id| {
            named
                .iter()
                .filter(|(_, slot)| slot.nzo_id.as_deref() == Some(id))
                .map(|(_, slot)| *slot)
                .collect()
        })
        .unwrap_or_default();
    let series: Vec<&(Vec<String>, &HistorySlot)> = named
        .iter()
        .filter(|(t, _)| !comic_tokens.is_empty() && comic_tokens.iter().all(|c| t.contains(c)))
        .collect();
    let by_name: Vec<&HistorySlot> = series
        .iter()
        .filter(|(t, _)| issue_matches(&issue, t))
        .map(|(_, slot)| *slot)
        .collect();
    let (matched_by, matches) = if !by_id.is_empty() {
        (Some("nzo_id".to_string()), by_id)
    } else if !by_name.is_empty() {
        (Some("name".to_string()), by_name)
    } else {
        (None, Vec::new())
    };
    let newest = matches.iter().max_by_key(|slot| slot.completed).copied();
    let class = match newest.map(|slot| slot.status.as_deref().unwrap_or_default()) {
        Some("Completed") => "sab_completed",
        Some("Failed") => "sab_failed",
        Some(_) => "sab_pending",
        None if series.is_empty() => "sab_missing",
        None => "name_mismatch",
    };
    let mut candidates: Vec<&HistorySlot> = if newest.is_none() {
        series.iter().map(|(_, slot)| *slot).collect()
    } else {
        Vec::new()
    };
    candidates.sort_by_key(|slot| std::cmp::Reverse(slot.completed));
    ReconciledSnatch {
        issue_id: s.issue_id.clone().unwrap_or_default(),
        comic,
        issue,
        snatched_at: s.date_added.clone().unwrap_or_default(),
        provider: s.provider.clone(),
        nzo_id,
        grabs,
        class: class.to_string(),
        matched_by,
        sab: newest.map(|slot| SabEntry {
            nzo_id: slot.nzo_id.clone(),
            name: Some(slot_name(slot).to_string()),
            status: slot.status.clone(),
            fail_message: slot.fail_message.clone().filter(|m| !m.is_empty()),
            storage: slot.storage.clone().filter(|p| !p.is_empty()),
            completed: slot.completed,
        }),
        sab_matches: matches.len(),
        candidates: candidates
            .into_iter()
            .take(CANDIDATES)
            .map(|slot| slot_name(slot).to_string())
            .collect(),
    }
}

pub fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

fn series_tokens(comic: &str) -> Vec<String> {
    tokens(comic)
        .into_iter()
        .filter(|t| !matches!(t.as_str(), "the" | "a" | "an" | "and" | "of"))
        .collect()
}

fn issue_number(t: &str) -> &str {
    match t.trim_start_matches('0') {
        "" => "0",
        n => n,
    }
}

fn issue_matches(issue: &str, name: &[String]) -> bool {
    let mut words: Vec<&str> = issue.split_whitespace().collect();
    let Some(number) = words.pop() else {
        return false;
    };
    let parts = tokens(number);
    let Some(whole) = parts.first() else {
        return false;
    };
    words
        .iter()
        .flat_map(|w| tokens(w))
        .all(|w| name.contains(&w))
        && name.windows(parts.len()).any(|w| {
            w[0].starts_with(|c: char| c.is_ascii_digit())
                && issue_number(&w[0]) == issue_number(whole)
                && w[1..] == parts[1..]
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::matchers::{method, path, query_param};
    use crate::mock::{Mock, MockServer, ResponseTemplate};
    use plugin_toolkit::serde_json;
    use std::time::Duration;

    fn snatch(id: &str, comic: &str, issue: &str, at: &str, hash: Option<&str>) -> Snatch {
        Snatch {
            issue_id: Some(id.into()),
            comic_name: Some(comic.into()),
            issue_number: Some(issue.into()),
            date_added: Some(at.into()),
            status: Some("Snatched".into()),
            provider: Some("NZBgeek".into()),
            hash: hash.map(str::to_string),
        }
    }

    fn slot(id: &str, name: &str, status: &str, completed: i64) -> HistorySlot {
        HistorySlot {
            nzo_id: Some(id.into()),
            name: Some(name.into()),
            status: Some(status.into()),
            fail_message: (status == "Failed").then(|| "Download failed".into()),
            storage: (status == "Completed").then(|| format!("/downloads/completed/comics/{name}")),
            completed: Some(completed),
            ..Default::default()
        }
    }

    fn now() -> Timestamp {
        Timestamp::now()
    }

    fn old() -> String {
        now()
            .minus(Duration::from_secs(72 * 3600))
            .to_rfc3339()
            .replacen('T', " ", 1)[..19]
            .to_string()
    }

    #[test]
    fn tokens_split_on_punctuation() {
        assert_eq!(
            tokens("The.Defenders.v1.152.[Digital]"),
            ["the", "defenders", "v1", "152", "digital"]
        );
    }

    #[test]
    fn issue_numbers_ignore_zero_padding() {
        assert!(issue_matches("1", &tokens("Saga 001 (2012)")));
        assert!(!issue_matches("10", &tokens("Saga 001 (2012)")));
        assert!(issue_matches("Annual 2", &tokens("Saga Annual 02 (2014)")));
        assert!(!issue_matches("Annual 2", &tokens("Saga 02 (2014)")));
        assert!(issue_matches("0", &tokens("Saga 000")));
        assert!(issue_matches(
            "152",
            &tokens("The.Defenders.v1.152.1986.Digital")
        ));
        assert!(issue_matches("1.5", &tokens("Saga.001.5.2012")));
        assert!(!issue_matches("1.5", &tokens("Saga.001.2012")));
    }

    #[test]
    fn classifies_each_row() {
        let at = old();
        let history = vec![
            snatch("1", "Saga", "1", &at, None),
            snatch("2", "Saga", "2", &at, None),
            snatch("2", "Saga", "2", &at, None),
            snatch(
                "3",
                "The Wicked + The Divine",
                "4",
                &at,
                Some("SABnzbd_nzo_x"),
            ),
            snatch("4", "Saga", "3", &at, None),
            snatch("5", "Monstress", "7", &at, None),
            snatch("6", "Saga", "5", &at, None),
        ];
        let slots = vec![
            slot("a", "Saga 001 (2012) (Digital)", "Completed", 10),
            slot("b", "Saga 002 (2012)", "Failed", 11),
            slot("c", "Saga 002 (2012) (Digital)", "Failed", 12),
            slot("SABnzbd_nzo_x", "WicDiv.04.nzb", "Completed", 13),
            slot("d", "Saga 005", "Extracting", 14),
        ];
        let r = build("m", "http://sab", "comics", &history, &slots, 24, now());
        let by_id = |id: &str| r.rows.iter().find(|r| r.issue_id == id).unwrap();
        assert_eq!(by_id("1").class, "sab_completed");
        assert_eq!(
            by_id("1").sab.as_ref().unwrap().storage.as_deref(),
            Some("/downloads/completed/comics/Saga 001 (2012) (Digital)")
        );
        let two = by_id("2");
        assert_eq!(two.class, "sab_failed");
        assert_eq!((two.grabs, two.sab_matches), (2, 2));
        assert_eq!(two.sab.as_ref().unwrap().nzo_id.as_deref(), Some("c"));
        assert_eq!(
            two.sab.as_ref().unwrap().fail_message.as_deref(),
            Some("Download failed")
        );
        assert_eq!(by_id("3").matched_by.as_deref(), Some("nzo_id"));
        assert_eq!(by_id("3").class, "sab_completed");
        let four = by_id("4");
        assert_eq!(four.class, "name_mismatch");
        assert_eq!(four.candidates[0], "Saga 005");
        assert_eq!(by_id("5").class, "sab_missing");
        assert_eq!(by_id("6").class, "sab_pending");
        assert_eq!(
            r.summary,
            ReconcileSummary {
                total: 6,
                sab_completed: 2,
                sab_failed: 1,
                sab_pending: 1,
                sab_missing: 1,
                name_mismatch: 1,
            }
        );
    }

    #[test]
    fn recent_snatches_are_skipped() {
        let fresh = now().to_rfc3339().replacen('T', " ", 1)[..19].to_string();
        let r = build(
            "m",
            "h",
            "comics",
            &[snatch("1", "Saga", "1", &fresh, None)],
            &[],
            24,
            now(),
        );
        assert!(r.rows.is_empty());
    }

    #[test]
    fn reconcile_reads_sab_through_mylars_config() {
        crate::mock::run(async {
            let mylar = MockServer::start().await;
            let sab = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/getConfig"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "aaData": [["sab_host", sab.uri()], ["sab_apikey", "SABKEY"], ["sab_category", "comics"]]
                })))
                .mount(&mylar)
                .await;
            Mock::given(method("GET"))
                .and(path("/api"))
                .and(query_param("cmd", "getHistory"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "success": true,
                    "data": [{"IssueID": "1", "ComicName": "Saga", "Issue_Number": "1",
                              "DateAdded": old(), "Status": "Snatched", "Provider": "NZBgeek",
                              "Hash": null}]
                })))
                .mount(&mylar)
                .await;
            Mock::given(method("GET"))
                .and(path("/api"))
                .and(query_param("mode", "history"))
                .and(query_param("cat", "comics"))
                .and(query_param("apikey", "SABKEY"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "history": {"noofslots": 1, "slots": [
                        {"nzo_id": "a", "name": "Saga 001", "status": "Failed",
                         "fail_message": "Unpacking failed", "storage": "", "completed": 5}
                    ]}
                })))
                .mount(&sab)
                .await;
            let m = Mylar::new(&mylar.uri(), "KEY", None);
            let r = reconcile("m", &m, 24).await.unwrap();
            assert_eq!(r.summary.sab_failed, 1);
            assert_eq!(
                r.rows[0].sab.as_ref().unwrap().fail_message.as_deref(),
                Some("Unpacking failed")
            );
            assert!(r.rows[0].sab.as_ref().unwrap().storage.is_none());
            let out = serde_json::to_string(&r).unwrap();
            assert!(!out.contains("SABKEY"), "{out}");
        })
    }

    #[test]
    fn missing_sab_settings_are_named() {
        let ini = ConfigIni(
            [("sab_host", "h")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        );
        let err = sab_settings(&ini).unwrap_err().to_string();
        assert!(err.contains("sab_apikey"), "{err}");
    }
}

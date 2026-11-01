use plugin_toolkit::prelude::*;
use plugin_toolkit::time::Timestamp;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::api::{ConfigIni, Mylar, Snatch};
use crate::issues::{self, Resolved};
use crate::sab::{HistorySlot, Sab};
use crate::status;

pub const DEFAULT_CATEGORY: &str = "comics";
pub const DEFAULT_LIMIT: usize = 200;
pub const MAX_LIMIT: usize = 1000;
/// Stuck grabs `resolve` will act on in one run; more is an error, never a truncation.
pub const RESOLVE_CAP: usize = 50_000;
pub const DEFAULT_MISSING_AFTER_DAYS: u32 = 14;
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
    pub comic_id: Option<String>,
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
    /// Matching SAB queue job, else newest matching history entry.
    pub sab: Option<SabEntry>,
    /// SAB queue and history entries matching this issue.
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
    /// SAB history entries read, across every category.
    pub sab_history_entries: usize,
    pub sab_queue_entries: usize,
    /// Counts over every stuck grab, not just this page.
    pub summary: ReconcileSummary,
    pub offset: usize,
    pub limit: usize,
    /// Stuck grabs remain past this page.
    pub truncated: bool,
    pub rows: Vec<ReconciledSnatch>,
}

struct Inputs {
    sab_host: String,
    category: String,
    history: Vec<Snatch>,
    queue: Vec<HistorySlot>,
    slots: Vec<HistorySlot>,
}

async fn fetch(m: &Mylar, stuck_hours: u32) -> Result<Inputs> {
    let ini = m.config().await?;
    let (host, key, category) = sab_settings(&ini)?;
    let history = status::current(m, m.history().await?, stuck_hours, Timestamp::now()).await?;
    let sab = Sab::new(host, key);
    let queue = sab.queue().await?;
    let slots = sab.history().await?;
    Ok(Inputs {
        sab_host: sab.base().to_string(),
        category,
        history,
        queue,
        slots,
    })
}

impl Inputs {
    fn classify(&self, name: &str, stuck_hours: u32, now: Timestamp) -> Classified {
        classify_all(
            name,
            &self.sab_host,
            &self.category,
            &self.history,
            &self.queue,
            &self.slots,
            stuck_hours,
            now,
        )
    }
}

pub async fn reconcile(
    name: &str,
    m: &Mylar,
    stuck_hours: u32,
    offset: usize,
    limit: usize,
) -> Result<ReconcileReport> {
    let inputs = fetch(m, stuck_hours).await?;
    Ok(inputs
        .classify(name, stuck_hours, Timestamp::now())
        .page(Page { offset, limit }))
}

/// Pages `page_fn` until every stuck grab is read; errors when the total exceeds `cap`.
fn every_stuck(
    cap: usize,
    mut page_fn: impl FnMut(Page) -> ReconcileReport,
) -> Result<(ReconcileSummary, Vec<ReconciledSnatch>)> {
    let first = page_fn(Page {
        offset: 0,
        limit: MAX_LIMIT,
    });
    let summary = first.summary;
    if summary.total > cap {
        bail!(
            "{} stuck grabs exceed the resolve cap of {cap}; resolve refuses to act on a partial set",
            summary.total
        );
    }
    let mut rows = first.rows;
    while rows.len() < summary.total {
        let page = page_fn(Page {
            offset: rows.len(),
            limit: MAX_LIMIT,
        });
        if page.rows.is_empty() {
            bail!(
                "stuck grabs ended at {} of {} reported",
                rows.len(),
                summary.total
            );
        }
        rows.extend(page.rows);
    }
    Ok((summary, rows))
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedGrab {
    pub issue_id: String,
    pub comic: String,
    pub issue: String,
    pub class: String,
    /// `fail` (release goes on Mylar's failed list), `post_process` (SAB finished it; run `mylar3.backlog.process` on its folder), `review` (SAB has no record but the grab is not provably dead) or `none`.
    pub action: String,
    /// SAB's storage path for a completed download.
    pub storage: Option<String>,
    /// Why a `sab_missing` grab is `review` rather than `fail`.
    pub reason: Option<String>,
    /// The fail went through and Mylar re-read the issue as no longer Snatched by that grab.
    pub failed: bool,
    pub error: Option<String>,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveReport {
    pub name: String,
    pub threshold_hours: u32,
    /// `sab_missing` grabs younger than this stay `review`.
    pub missing_after_days: u32,
    pub summary: ReconcileSummary,
    /// Grabs whose release was failed.
    pub failed: usize,
    /// Grabs that needed a fail which did not go through.
    pub errors: usize,
    pub rows: Vec<ResolvedGrab>,
    pub dry_run: bool,
}

/// Guards a `sab_missing` grab must clear before `--include-missing` fails it.
pub struct Missing {
    /// Mylar-local time; grabs snatched at or after it are too young to call dead.
    pub before: String,
    /// Issues Mylar's issue table still shows as `Snatched`.
    pub still_snatched: HashSet<String>,
}

fn missing_reason(r: &ReconciledSnatch, m: &Missing) -> Option<String> {
    if r.snatched_at.is_empty() || r.snatched_at >= m.before {
        return Some(format!(
            "snatched {} is newer than the missing cutoff {}",
            r.snatched_at, m.before
        ));
    }
    if !m.still_snatched.contains(&r.issue_id) {
        return Some("mylar does not show the issue as still Snatched".to_string());
    }
    None
}

pub fn actions(rows: &[ReconciledSnatch], missing: Option<&Missing>) -> Vec<ResolvedGrab> {
    rows.iter()
        .map(|r| {
            let mut reason = None;
            let action = match (r.class.as_str(), missing) {
                ("sab_failed", _) => "fail",
                ("sab_missing", Some(m)) => {
                    reason = missing_reason(r, m);
                    if reason.is_some() {
                        "review"
                    } else {
                        "fail"
                    }
                }
                ("sab_completed", _) => "post_process",
                _ => "none",
            };
            ResolvedGrab {
                issue_id: r.issue_id.clone(),
                comic: r.comic.clone(),
                issue: r.issue.clone(),
                class: r.class.clone(),
                action: action.to_string(),
                storage: r.sab.as_ref().and_then(|s| s.storage.clone()),
                reason,
                failed: false,
                error: None,
            }
        })
        .collect()
}

/// Fail every stuck grab SAB reports failed (and, with `include_missing`, every grab SAB has no queue
/// or history record of that is older than `missing_after_days` and Mylar still shows Snatched).
pub async fn resolve(
    name: &str,
    m: &Mylar,
    stuck_hours: u32,
    include_missing: bool,
    missing_after_days: u32,
    execute: bool,
) -> Result<ResolveReport> {
    let inputs = fetch(m, stuck_hours).await?;
    let now = Timestamp::now();
    let classified = inputs.classify(name, stuck_hours, now);
    let (summary, stuck) = every_stuck(RESOLVE_CAP, |page| classified.page(page))?;
    let before = status::utc_as_mylar_local_time(
        Timestamp::now().minus(Duration::from_secs(u64::from(missing_after_days) * 86_400)),
    );
    let guard = Missing {
        before,
        still_snatched: if include_missing {
            still_snatched(m, &stuck).await
        } else {
            HashSet::new()
        },
    };
    let mut rows = actions(&stuck, include_missing.then_some(&guard));
    if execute {
        for row in rows.iter_mut().filter(|r| r.action == "fail") {
            let target = Resolved {
                issue_id: row.issue_id.clone(),
                comic_id: None,
                series_name: None,
                number: None,
                status: None,
                annual: false,
                story_arc: None,
            };
            match issues::fail(name, m, target).await {
                Ok(_) => row.failed = true,
                Err(e) => row.error = Some(format!("{e:#}")),
            }
        }
    }
    Ok(ResolveReport {
        name: name.to_string(),
        threshold_hours: stuck_hours,
        missing_after_days,
        summary,
        failed: rows.iter().filter(|r| r.failed).count(),
        errors: rows.iter().filter(|r| r.error.is_some()).count(),
        rows,
        dry_run: !execute,
    })
}

/// A series that cannot be read leaves its issues out, so they stay `review`.
async fn still_snatched(m: &Mylar, rows: &[ReconciledSnatch]) -> HashSet<String> {
    let comics: HashSet<&str> = rows
        .iter()
        .filter(|r| r.class == "sab_missing")
        .filter_map(|r| r.comic_id.as_deref())
        .collect();
    let mut out = HashSet::new();
    for id in comics {
        let Ok(c) = m.comic(id).await else {
            continue;
        };
        out.extend(
            c.issues
                .into_iter()
                .chain(c.annuals)
                .filter(|i| i.status.as_deref() == Some("Snatched"))
                .filter_map(|i| i.id),
        );
    }
    out
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

#[derive(Debug, Clone, Copy)]
pub struct Page {
    pub offset: usize,
    pub limit: usize,
}

impl Default for Page {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: DEFAULT_LIMIT,
        }
    }
}

/// Every stuck grab classified once; pages slice it.
pub struct Classified {
    name: String,
    threshold_hours: u32,
    sab_host: String,
    sab_category: String,
    sab_history_entries: usize,
    sab_queue_entries: usize,
    summary: ReconcileSummary,
    rows: Vec<ReconciledSnatch>,
}

impl Classified {
    pub fn page(&self, page: Page) -> ReconcileReport {
        let limit = page.limit.clamp(1, MAX_LIMIT);
        let rows: Vec<ReconciledSnatch> = self
            .rows
            .iter()
            .skip(page.offset)
            .take(limit)
            .cloned()
            .collect();
        let truncated = page.offset + rows.len() < self.summary.total;
        ReconcileReport {
            name: self.name.clone(),
            threshold_hours: self.threshold_hours,
            sab_host: self.sab_host.clone(),
            sab_category: self.sab_category.clone(),
            sab_history_entries: self.sab_history_entries,
            sab_queue_entries: self.sab_queue_entries,
            summary: self.summary.clone(),
            offset: page.offset,
            limit,
            truncated,
            rows,
        }
    }
}

/// SAB queue then history slots, tokenized once, with token and nzo_id postings in slot order.
struct SlotIndex<'a> {
    named: Vec<(Vec<String>, &'a HistorySlot)>,
    by_token: HashMap<String, Vec<usize>>,
    by_nzo: HashMap<&'a str, Vec<usize>>,
}

impl<'a> SlotIndex<'a> {
    fn new(queue: &'a [HistorySlot], slots: &'a [HistorySlot]) -> Self {
        let named: Vec<(Vec<String>, &HistorySlot)> = queue
            .iter()
            .chain(slots)
            .map(|s| (tokens(slot_name(s)), s))
            .collect();
        let mut by_token: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_nzo: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, (t, slot)) in named.iter().enumerate() {
            if let Some(id) = slot.nzo_id.as_deref() {
                by_nzo.entry(id).or_default().push(i);
            }
            for tok in t {
                let list = by_token.entry(tok.clone()).or_default();
                if list.last() != Some(&i) {
                    list.push(i);
                }
            }
        }
        SlotIndex {
            named,
            by_token,
            by_nzo,
        }
    }

    fn by_nzo(&self, id: &str) -> Vec<&'a HistorySlot> {
        self.by_nzo
            .get(id)
            .map(|ids| ids.iter().map(|&i| self.named[i].1).collect())
            .unwrap_or_default()
    }

    /// Slots whose name carries every comic token, in slot order.
    fn series(&self, comic: &[String]) -> Vec<&(Vec<String>, &'a HistorySlot)> {
        let Some(rarest) = comic
            .iter()
            .map(|c| self.by_token.get(c).map_or(&[][..], Vec::as_slice))
            .min_by_key(|ids| ids.len())
        else {
            return Vec::new();
        };
        rarest
            .iter()
            .map(|&i| &self.named[i])
            .filter(|(t, _)| comic.iter().all(|c| t.contains(c)))
            .collect()
    }
}

#[allow(clippy::too_many_arguments)]
pub fn classify_all(
    name: &str,
    sab_host: &str,
    category: &str,
    history: &[Snatch],
    queue: &[HistorySlot],
    slots: &[HistorySlot],
    stuck_hours: u32,
    now: Timestamp,
) -> Classified {
    let (stuck, grabs) = status::stuck_snatches(history, stuck_hours, now);
    let index = SlotIndex::new(queue, slots);
    let mut summary = ReconcileSummary::default();
    let rows: Vec<ReconciledSnatch> = stuck
        .into_iter()
        .map(|s| {
            let id = s.issue_id.as_deref().unwrap_or_default();
            let row = classify(s, grabs.get(id).copied().unwrap_or_default(), &index);
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
    Classified {
        name: name.to_string(),
        threshold_hours: stuck_hours,
        sab_host: sab_host.to_string(),
        sab_category: category.to_string(),
        sab_history_entries: slots.len(),
        sab_queue_entries: queue.len(),
        summary,
        rows,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build(
    name: &str,
    sab_host: &str,
    category: &str,
    history: &[Snatch],
    queue: &[HistorySlot],
    slots: &[HistorySlot],
    stuck_hours: u32,
    now: Timestamp,
    page: Page,
) -> ReconcileReport {
    classify_all(
        name,
        sab_host,
        category,
        history,
        queue,
        slots,
        stuck_hours,
        now,
    )
    .page(page)
}

fn slot_name(s: &HistorySlot) -> &str {
    s.name
        .as_deref()
        .or(s.nzb_name.as_deref())
        .unwrap_or_default()
}

fn classify(s: &Snatch, grabs: usize, index: &SlotIndex) -> ReconciledSnatch {
    let nzo_id = s
        .hash
        .clone()
        .filter(|h| !h.trim().is_empty() && h != "None");
    let comic = s.comic_name.clone().unwrap_or_default();
    let issue = s.issue_number.clone().unwrap_or_default();
    let comic_tokens = series_tokens(&comic);
    let by_id: Vec<&HistorySlot> = nzo_id
        .as_deref()
        .map(|id| index.by_nzo(id))
        .unwrap_or_default();
    let series = index.series(&comic_tokens);
    let by_name: Vec<&HistorySlot> = series
        .iter()
        .filter(|(t, _)| issue_matches(&issue, t) && same_series(&comic_tokens, &issue, t))
        .map(|(_, slot)| *slot)
        .collect();
    let (matched_by, matches) = if !by_id.is_empty() {
        (Some("nzo_id".to_string()), by_id)
    } else if !by_name.is_empty() {
        (Some("name".to_string()), by_name)
    } else {
        (None, Vec::new())
    };
    // Queue jobs carry no `completed`; a live job outranks any older history outcome.
    let newest = matches
        .iter()
        .max_by_key(|slot| (slot.completed.is_none(), slot.completed))
        .copied();
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
        comic_id: s.comic_id.clone(),
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

fn same_series(comic: &[String], issue: &str, name: &[String]) -> bool {
    let issue_words: Vec<String> = issue.split_whitespace().flat_map(tokens).collect();
    let is_volume =
        |t: &str| t.len() > 1 && t.starts_with('v') && t[1..].bytes().all(|b| b.is_ascii_digit());
    let job: HashSet<&str> = name
        .iter()
        .take_while(|t| !t.starts_with(|c: char| c.is_ascii_digit()))
        .map(String::as_str)
        .filter(|t| {
            !matches!(*t, "the" | "a" | "an" | "and" | "of")
                && !is_volume(t)
                && !issue_words.iter().any(|w| w == t)
        })
        .collect();
    job == comic.iter().map(String::as_str).collect()
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
            comic_id: None,
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

    fn guard() -> Missing {
        Missing {
            before: now().to_rfc3339().replacen('T', " ", 1)[..19].to_string(),
            still_snatched: ["5".to_string()].into(),
        }
    }

    fn missing_row() -> ReconciledSnatch {
        let r = build(
            "m",
            "h",
            "comics",
            &[snatch("5", "Monstress", "7", &old(), None)],
            &[],
            &[],
            24,
            now(),
            Page::default(),
        );
        r.rows[0].clone()
    }

    #[test]
    fn queued_job_is_pending_not_missing() {
        let at = old();
        let history = vec![snatch("5", "Monstress", "7", &at, None)];
        let queue = vec![HistorySlot {
            nzo_id: Some("q".into()),
            nzb_name: Some("Monstress 007 (2016)".into()),
            status: Some("Downloading".into()),
            ..Default::default()
        }];
        let slots = vec![slot("a", "Monstress 007 (2016)", "Failed", 10)];
        let r = build(
            "m",
            "h",
            "comics",
            &history,
            &queue,
            &slots,
            24,
            now(),
            Page::default(),
        );
        assert_eq!(r.rows[0].class, "sab_pending");
        assert_eq!(r.sab_queue_entries, 1);
        assert_eq!(actions(&r.rows, Some(&guard()))[0].action, "none");
    }

    #[test]
    fn young_missing_grab_is_review() {
        let row = missing_row();
        let g = Missing {
            before: old(),
            ..guard()
        };
        let a = &actions(&[row], Some(&g))[0];
        assert_eq!(a.action, "review");
        assert!(a.reason.as_deref().unwrap().contains("newer"), "{a:?}");
    }

    #[test]
    fn missing_grab_mylar_moved_on_is_review() {
        let row = missing_row();
        let g = Missing {
            still_snatched: HashSet::new(),
            ..guard()
        };
        let a = &actions(std::slice::from_ref(&row), Some(&g))[0];
        assert_eq!(a.action, "review");
        assert!(a.reason.as_deref().unwrap().contains("Snatched"), "{a:?}");
        let a = &actions(&[row], Some(&guard()))[0];
        assert_eq!((a.action.as_str(), a.reason.as_deref()), ("fail", None));
    }

    #[test]
    fn resolve_checks_mylar_issue_status_before_failing_missing() {
        crate::mock::run(async {
            let mylar = MockServer::start().await;
            let sab = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/getConfig"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "aaData": [["sab_host", sab.uri()], ["sab_apikey", "SABKEY"]]
                })))
                .mount(&mylar)
                .await;
            // The fillers and "c" fill status::current's series-lookup cap, so only the guard reads "late".
            let fillers = status::MAX_SERIES_LOOKUPS - 1;
            let snatch = |issue: &str, comic: &str, date: &str| {
                serde_json::json!({"IssueID": issue, "ComicID": comic, "ComicName": "Saga",
                                   "Issue_Number": issue, "DateAdded": date, "Status": "Snatched"})
            };
            let mut history: Vec<_> = (0..fillers)
                .map(|i| snatch(&format!("f{i}"), &format!("f{i}"), "2019-01-01 00:00:00"))
                .collect();
            history.push(snatch("1", "c", "2019-01-01 00:00:00"));
            history.push(snatch("2", "late", "2020-01-01 00:00:00"));
            history.push(snatch("3", "late", "2020-01-01 00:00:00"));
            Mock::given(method("GET"))
                .and(path("/api"))
                .and(query_param("cmd", "getHistory"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({"success": true, "data": history})),
                )
                .mount(&mylar)
                .await;
            let comics = (0..fillers)
                .map(|i| (format!("f{i}"), serde_json::json!([{"id": format!("f{i}"), "status": "Snatched"}])))
                .chain([
                    ("c".to_string(), serde_json::json!([{"id": "1", "status": "Snatched"}])),
                    (
                        "late".to_string(),
                        serde_json::json!([{"id": "2", "status": "Downloaded"}, {"id": "3", "status": "Snatched"}]),
                    ),
                ]);
            for (id, issues) in comics {
                Mock::given(method("GET"))
                    .and(path("/api"))
                    .and(query_param("cmd", "getComic"))
                    .and(query_param("id", id.as_str()))
                    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "success": true,
                        "data": {"comic": [], "issues": issues, "annuals": []}
                    })))
                    .mount(&mylar)
                    .await;
            }
            for mode in ["queue", "history"] {
                Mock::given(method("GET"))
                    .and(path("/api"))
                    .and(query_param("mode", mode))
                    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        mode: {"noofslots": 0, "slots": []}
                    })))
                    .mount(&sab)
                    .await;
            }
            let m = Mylar::new(&mylar.uri(), "KEY", None);
            let r = resolve("m", &m, 24, true, DEFAULT_MISSING_AFTER_DAYS, false)
                .await
                .unwrap();
            assert_eq!(r.summary.sab_missing, fillers + 3);
            let row = |id: &str| r.rows.iter().find(|g| g.issue_id == id).unwrap();
            assert_eq!(row("1").action, "fail");
            assert_eq!(row("3").action, "fail");
            assert_eq!(row("2").action, "review");
            let r = resolve("m", &m, 24, false, DEFAULT_MISSING_AFTER_DAYS, false)
                .await
                .unwrap();
            assert!(r.rows.iter().all(|g| g.action == "none"));
        })
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
        let r = build(
            "m",
            "http://sab",
            "comics",
            &history,
            &[],
            &slots,
            24,
            now(),
            Page::default(),
        );
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
            &[],
            24,
            now(),
            Page::default(),
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
                .and(query_param("mode", "queue"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "queue": {"noofslots": 0, "slots": []}
                })))
                .mount(&sab)
                .await;
            Mock::given(method("GET"))
                .and(path("/api"))
                .and(query_param("mode", "history"))
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
            let r = reconcile("m", &m, 24, 0, DEFAULT_LIMIT).await.unwrap();
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
    fn looser_series_name_is_review_not_fail() {
        let at = old();
        let history = vec![
            snatch("1", "Batman", "1", &at, None),
            snatch("2", "Defenders", "152", &at, None),
        ];
        let slots = vec![
            slot("a", "Batman Beyond 001 (2016)", "Failed", 10),
            slot("b", "The.Defenders.v1.152.1986.Digital", "Failed", 11),
        ];
        let r = build(
            "m",
            "h",
            "comics",
            &history,
            &[],
            &slots,
            24,
            now(),
            Page::default(),
        );
        let row = |id: &str| r.rows.iter().find(|r| r.issue_id == id).unwrap();
        assert_eq!(row("1").class, "name_mismatch");
        assert!(row("1").matched_by.is_none());
        assert_eq!(row("2").class, "sab_failed");
        let rows = actions(&r.rows, Some(&guard()));
        let batman = rows.iter().find(|r| r.issue_id == "1").unwrap();
        assert_eq!(batman.action, "none");
    }

    #[test]
    fn actions_fail_only_dead_grabs() {
        let at = old();
        let history = vec![
            snatch("1", "Saga", "1", &at, None),
            snatch("2", "Saga", "2", &at, None),
            snatch("5", "Monstress", "7", &at, None),
            snatch("6", "Saga", "5", &at, None),
        ];
        let slots = vec![
            slot("a", "Saga 001", "Completed", 10),
            slot("b", "Saga 002", "Failed", 11),
            slot("d", "Saga 005", "Extracting", 14),
        ];
        let r = build(
            "m",
            "h",
            "comics",
            &history,
            &[],
            &slots,
            24,
            now(),
            Page::default(),
        );
        let of = |rows: &[ResolvedGrab], id: &str| {
            rows.iter()
                .find(|r| r.issue_id == id)
                .unwrap()
                .action
                .clone()
        };
        let rows = actions(&r.rows, None);
        assert_eq!(of(&rows, "1"), "post_process");
        assert_eq!(of(&rows, "2"), "fail");
        assert_eq!(of(&rows, "5"), "none");
        assert_eq!(of(&rows, "6"), "none");
        assert_eq!(
            rows.iter()
                .find(|r| r.issue_id == "1")
                .unwrap()
                .storage
                .as_deref(),
            Some("/downloads/completed/comics/Saga 001")
        );
        assert_eq!(of(&actions(&r.rows, Some(&guard())), "5"), "fail");
    }

    #[test]
    fn resolve_dry_run_writes_nothing_and_execute_reports_errors() {
        crate::mock::run(async {
            let mylar = MockServer::start().await;
            let sab = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/getConfig"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "aaData": [["sab_host", sab.uri()], ["sab_apikey", "SABKEY"],
                               ["sab_category", "comics"], ["failed_download_handling", "False"]]
                })))
                .mount(&mylar)
                .await;
            Mock::given(method("GET"))
                .and(path("/api"))
                .and(query_param("cmd", "getHistory"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "success": true,
                    "data": [{"IssueID": "1", "ComicName": "Saga", "Issue_Number": "1",
                              "DateAdded": old(), "Status": "Snatched"}]
                })))
                .mount(&mylar)
                .await;
            Mock::given(method("GET"))
                .and(path("/api"))
                .and(query_param("mode", "queue"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "queue": {"noofslots": 0, "slots": []}
                })))
                .mount(&sab)
                .await;
            Mock::given(method("GET"))
                .and(path("/api"))
                .and(query_param("mode", "history"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "history": {"noofslots": 1, "slots": [
                        {"nzo_id": "a", "name": "Saga 001", "status": "Failed", "completed": 5}
                    ]}
                })))
                .mount(&sab)
                .await;
            let m = Mylar::new(&mylar.uri(), "KEY", None);
            let r = resolve("m", &m, 24, false, DEFAULT_MISSING_AFTER_DAYS, false)
                .await
                .unwrap();
            assert!(r.dry_run);
            assert_eq!(
                (r.rows[0].action.as_str(), r.failed, r.errors),
                ("fail", 0, 0)
            );
            let r = resolve("m", &m, 24, false, DEFAULT_MISSING_AFTER_DAYS, true)
                .await
                .unwrap();
            assert_eq!((r.failed, r.errors), (0, 1));
            let err = r.rows[0].error.as_deref().unwrap();
            assert!(err.contains("failed_download_handling off"), "{err}");
            let reqs = mylar.received_requests().await.unwrap();
            assert!(reqs.iter().all(|q| q.url.path() != "/failed_handling"));
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

    #[test]
    fn rows_page_while_summary_covers_all() {
        let at = old();
        let history: Vec<Snatch> = (0..5)
            .map(|i| snatch(&i.to_string(), "Saga", &i.to_string(), &at, None))
            .collect();
        let page = Page {
            offset: 3,
            limit: 10,
        };
        let r = build("m", "h", "comics", &history, &[], &[], 24, now(), page);
        assert_eq!((r.summary.total, r.rows.len(), r.truncated), (5, 2, false));
        let r = build(
            "m",
            "h",
            "comics",
            &history,
            &[],
            &[],
            24,
            now(),
            Page {
                offset: 0,
                limit: 0,
            },
        );
        assert_eq!((r.limit, r.rows.len(), r.truncated), (1, 1, true));
    }

    #[test]
    fn every_stuck_pages_past_max_limit() {
        let at = old();
        let history: Vec<Snatch> = (0..2_345)
            .map(|i| snatch(&i.to_string(), "Saga", &i.to_string(), &at, None))
            .collect();
        let mut pages = 0;
        let (summary, rows) = every_stuck(RESOLVE_CAP, |page| {
            pages += 1;
            build("m", "h", "comics", &history, &[], &[], 24, now(), page)
        })
        .unwrap();
        assert_eq!((summary.total, rows.len(), pages), (2_345, 2_345, 3));
        let ids: HashSet<&str> = rows.iter().map(|r| r.issue_id.as_str()).collect();
        assert_eq!(ids.len(), 2_345);
    }

    #[test]
    fn classification_scales_to_thousands_of_grabs_and_slots() {
        let at = old();
        let history: Vec<Snatch> = (0..5_000)
            .map(|i| snatch(&i.to_string(), &format!("Series{i}"), "1", &at, None))
            .collect();
        let slots: Vec<HistorySlot> = (0..20_000)
            .map(|i| {
                slot(
                    &format!("n{i}"),
                    &format!("Series{i} 001 (2020)"),
                    "Failed",
                    i,
                )
            })
            .collect();
        let started = std::time::Instant::now();
        let classified = classify_all("m", "h", "comics", &history, &[], &slots, 24, now());
        let (summary, rows) = every_stuck(RESOLVE_CAP, |page| classified.page(page)).unwrap();
        assert_eq!(
            (summary.total, summary.sab_failed, rows.len()),
            (5_000, 5_000, 5_000)
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn every_stuck_errors_past_the_cap() {
        let at = old();
        let history: Vec<Snatch> = (0..1_001)
            .map(|i| snatch(&i.to_string(), "Saga", &i.to_string(), &at, None))
            .collect();
        let err = every_stuck(1_000, |page| {
            build("m", "h", "comics", &history, &[], &[], 24, now(), page)
        })
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("1001 stuck grabs exceed the resolve cap of 1000"),
            "{err}"
        );
    }
}

//! `mylar3.status`: search reach, completed-download handling, stuck grabs, and
//! torrent wiring, read from Mylar's API and its settings table.
//!
//! Every config-derived field is `None` when the setting could not be read;
//! `unknown` names those settings and `config_error` says why.

use std::collections::BTreeMap;
use std::time::Duration;

use plugin_toolkit::prelude::*;
use plugin_toolkit::time::Timestamp;

use crate::api::{ConfigIni, Mylar, Snatch, Wanted, WantedIssue};

/// `config.ini` keys the report reads.
const KEYS: &[&str] = &[
    "usenet_retention",
    "search_delay",
    "newznab",
    "nzb_downloader",
    "sab_host",
    "sab_apikey",
    "sab_category",
    "sab_client_post_processing",
    "sab_to_mylar",
    "sab_directory",
    "post_processing",
    "enable_check_folder",
    "check_folder",
    "enable_torrents",
    "enable_torrent_search",
    "enable_torznab",
    "torrent_downloader",
];

/// Sample size for the stuck-grab list.
const STUCK_SAMPLE: usize = 10;

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionCheck {
    /// Days Mylar sends as newznab `maxage`; indexers drop older posts.
    pub usenet_retention_days: Option<i64>,
    /// Posts older than this date are filtered out by the indexer.
    pub cutoff_date: Option<String>,
    /// Oldest release (else cover) date among Wanted issues and annuals.
    pub oldest_wanted_date: Option<String>,
    /// Wanted issues with a usable date.
    pub wanted_dated: usize,
    /// Wanted issues released before `cutoff_date`.
    pub wanted_before_cutoff: Option<usize>,
    /// The cutoff hides at least one Wanted issue.
    pub drift: Option<bool>,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostProcessingCheck {
    /// `sabnzbd`, `nzbget`, `blackhole`, `none`, or Mylar's raw value.
    pub nzb_downloader: Option<String>,
    pub sab_host: Option<String>,
    pub sab_api_key_set: Option<bool>,
    pub sab_category: Option<String>,
    /// `sab_client_post_processing`: Mylar polls SAB and imports finished jobs.
    pub completed_download_handling: Option<bool>,
    /// `sab_to_mylar`: map SAB's storage path onto `sab_directory`.
    pub sab_to_mylar: Option<bool>,
    pub sab_directory: Option<String>,
    pub post_processing: Option<bool>,
    pub check_folder_enabled: Option<bool>,
    pub check_folder: Option<String>,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StuckIssue {
    pub comic: String,
    pub issue: String,
    /// Mylar's local time.
    pub snatched_at: String,
    pub provider: Option<String>,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StuckSnatched {
    pub threshold_hours: u32,
    /// Issues whose latest history row is `Snatched` and older than the threshold.
    pub count: usize,
    /// Issues grabbed more than once.
    pub repeat_grabbed: usize,
    pub oldest: Option<String>,
    pub sample: Vec<StuckIssue>,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchCheck {
    /// Minutes between provider hits.
    pub search_delay_minutes: Option<i64>,
    /// A whole number of at least 1, the only form Mylar honours.
    pub search_delay_valid: Option<bool>,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentCheck {
    pub torrents_enabled: Option<bool>,
    pub torrent_search_enabled: Option<bool>,
    pub torznab_enabled: Option<bool>,
    /// `watchfolder`, `utorrent`, `rtorrent`, `transmission`, `deluge`,
    /// `qbittorrent`, or Mylar's raw value.
    pub downloader: Option<String>,
    /// The selected downloader has its host (or watch dir) set.
    pub client_configured: Option<bool>,
    /// Torrent results can be grabbed but have nowhere to go.
    pub drift: Option<bool>,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReport {
    pub name: String,
    pub series: usize,
    pub wanted: usize,
    pub retention: RetentionCheck,
    pub post_processing: PostProcessingCheck,
    pub stuck_snatched: StuckSnatched,
    pub search: SearchCheck,
    pub torrents: TorrentCheck,
    /// Why the settings table could not be read.
    pub config_error: Option<String>,
    /// Settings that could not be read (absent from `config.ini` or table
    /// unreadable).
    pub unknown: Vec<String>,
    /// Detected drift, one line each.
    pub findings: Vec<String>,
    /// No findings and the settings table was readable.
    pub healthy: bool,
}

pub async fn status(name: &str, m: &Mylar, stuck_hours: u32) -> Result<StatusReport> {
    let series = m.index().await?.len();
    let wanted = m.wanted().await?;
    let history = m.history().await?;
    let config = m.config().await;
    Ok(build(
        name,
        series,
        &wanted,
        &history,
        config,
        stuck_hours,
        Timestamp::now(),
    ))
}

pub fn build(
    name: &str,
    series: usize,
    wanted: &Wanted,
    history: &[Snatch],
    config: Result<ConfigIni>,
    stuck_hours: u32,
    now: Timestamp,
) -> StatusReport {
    let (ini, config_error) = match config {
        Ok(ini) => (ini, None),
        Err(e) => (ConfigIni::default(), Some(e.to_string())),
    };
    let unknown: Vec<String> = KEYS
        .iter()
        .filter(|k| !ini.has(k))
        .map(|k| k.to_string())
        .collect();

    let issues: Vec<&WantedIssue> = wanted.issues.iter().chain(&wanted.annuals).collect();
    let retention = retention(&ini, &issues, now);
    let post_processing = post_processing(&ini);
    let stuck_snatched = stuck(history, stuck_hours, now);
    let search = search(&ini);
    let torrents = torrents(&ini);

    let mut findings = Vec::new();
    if retention.drift == Some(true) {
        findings.push(format!(
            "usenet_retention={} sends maxage={} so indexers drop posts before {}; {} of {} \
             dated Wanted issues were released before that",
            retention.usenet_retention_days.unwrap_or_default(),
            retention.usenet_retention_days.unwrap_or_default(),
            retention.cutoff_date.as_deref().unwrap_or("?"),
            retention.wanted_before_cutoff.unwrap_or_default(),
            retention.wanted_dated,
        ));
    }
    findings.extend(post_processing_findings(&ini, &post_processing));
    if stuck_snatched.count > 0 {
        findings.push(format!(
            "{} issues stuck at Snatched for more than {}h (oldest {}); {} issues grabbed \
             more than once",
            stuck_snatched.count,
            stuck_hours,
            stuck_snatched.oldest.as_deref().unwrap_or("?"),
            stuck_snatched.repeat_grabbed,
        ));
    }
    if search.search_delay_valid == Some(false) {
        findings.push(format!(
            "search_delay={} is not a whole number of minutes >= 1",
            ini.get("search_delay").unwrap_or("")
        ));
    }
    if torrents.drift == Some(true) {
        findings.push(format!(
            "torrent search is on but torrent downloader {} is not configured (enable_torrents={}); \
             sends fail with 'TorrentClient' object has no attribute 'client'",
            torrents.downloader.as_deref().unwrap_or("?"),
            fmt_opt(torrents.torrents_enabled),
        ));
    }

    StatusReport {
        name: name.to_string(),
        series,
        wanted: issues.len(),
        healthy: findings.is_empty() && config_error.is_none(),
        retention,
        post_processing,
        stuck_snatched,
        search,
        torrents,
        config_error,
        unknown,
        findings,
    }
}

/// Release date when known, else cover date; `YYYY-MM-DD`, or `None` for
/// Mylar's `0000-00-00` placeholder.
fn issue_date(i: &WantedIssue) -> Option<String> {
    [&i.release_date, &i.issue_date]
        .into_iter()
        .flatten()
        .map(|d| d.trim())
        .find(|d| {
            d.len() >= 10
                && d.as_bytes()[..4].iter().all(u8::is_ascii_digit)
                && !d.starts_with("0000")
        })
        .map(|d| d[..10].to_string())
}

fn retention(ini: &ConfigIni, issues: &[&WantedIssue], now: Timestamp) -> RetentionCheck {
    let days = ini.int("usenet_retention");
    let dates: Vec<String> = issues.iter().filter_map(|i| issue_date(i)).collect();
    let oldest_wanted_date = dates.iter().min().cloned();
    // Mylar appends maxage only for a set retention; 0 is still sent.
    let cutoff_date = days
        .filter(|d| *d >= 0)
        .map(|d| now.minus(Duration::from_secs(d as u64 * 86_400)).date());
    // `YYYY-MM-DD` sorts as text.
    let wanted_before_cutoff = cutoff_date
        .as_ref()
        .map(|c| dates.iter().filter(|d| d.as_str() < c.as_str()).count());
    RetentionCheck {
        usenet_retention_days: days,
        drift: wanted_before_cutoff.map(|n| n > 0),
        cutoff_date,
        oldest_wanted_date,
        wanted_dated: dates.len(),
        wanted_before_cutoff,
    }
}

fn nzb_downloader_name(v: i64) -> String {
    match v {
        0 => "sabnzbd".into(),
        1 => "nzbget".into(),
        2 => "blackhole".into(),
        3 => "none".into(),
        other => other.to_string(),
    }
}

fn post_processing(ini: &ConfigIni) -> PostProcessingCheck {
    PostProcessingCheck {
        nzb_downloader: ini.int("nzb_downloader").map(nzb_downloader_name),
        sab_host: ini.get("sab_host").map(str::to_string),
        sab_api_key_set: ini
            .has("sab_apikey")
            .then(|| ini.get("sab_apikey").is_some()),
        sab_category: ini.get("sab_category").map(str::to_string),
        completed_download_handling: ini.flag("sab_client_post_processing"),
        sab_to_mylar: ini.flag("sab_to_mylar"),
        sab_directory: ini.get("sab_directory").map(str::to_string),
        post_processing: ini.flag("post_processing"),
        check_folder_enabled: ini.flag("enable_check_folder"),
        check_folder: ini.get("check_folder").map(str::to_string),
    }
}

fn post_processing_findings(ini: &ConfigIni, pp: &PostProcessingCheck) -> Vec<String> {
    let mut out = Vec::new();
    // search.py gates every NZB search on a usable downloader.
    if ini.flag("newznab") == Some(true) && pp.nzb_downloader.as_deref() == Some("none") {
        out.push(
            "newznab indexers are on but nzb_downloader=3 (none): Mylar aborts every search \
             with 'There are no search providers enabled'"
                .into(),
        );
    }
    if pp.nzb_downloader.as_deref() != Some("sabnzbd") {
        return out;
    }
    if ini.has("sab_host") && pp.sab_host.is_none() {
        out.push("nzb_downloader=sabnzbd but sab_host is empty".into());
    }
    if pp.sab_api_key_set == Some(false) {
        out.push("nzb_downloader=sabnzbd but sab_apikey is empty".into());
    }
    let cdh = pp.completed_download_handling;
    let folder = pp.check_folder_enabled;
    if cdh == Some(false) && folder == Some(false) {
        out.push(
            "completed downloads are never imported: completed download handling \
             (sab_client_post_processing) and the folder monitor (enable_check_folder) are both off"
                .into(),
        );
    }
    if cdh == Some(true) && pp.sab_category.is_none() && ini.has("sab_category") {
        out.push(
            "completed download handling is on with no sab_category: Mylar's SAB history \
             lookup is unfiltered and its CDH path mapping assumes no category folder"
                .into(),
        );
    }
    if cdh == Some(true) && pp.sab_to_mylar == Some(true) && pp.sab_directory.is_none() {
        out.push(
            "sab_to_mylar is on but sab_directory is empty, so SAB paths cannot be mapped".into(),
        );
    }
    if folder == Some(true) && pp.check_folder.is_none() && ini.has("check_folder") {
        out.push("enable_check_folder is on but check_folder is empty".into());
    }
    if pp.post_processing == Some(false) {
        out.push("post_processing is off: nothing is renamed or moved into the library".into());
    }
    out
}

/// `YYYY-MM-DD HH:MM:SS`, the shape of Mylar's `DateAdded`.
fn mylar_datetime(t: Timestamp) -> String {
    t.to_rfc3339()
        .replacen('T', " ", 1)
        .trim_end_matches('Z')
        .to_string()
}

pub fn stuck(history: &[Snatch], threshold_hours: u32, now: Timestamp) -> StuckSnatched {
    // Mylar stamps local time; against UTC the threshold is off by the host's
    // UTC offset, which is noise at the hours-to-days scale this is read at.
    let before = mylar_datetime(now.minus(Duration::from_secs(u64::from(threshold_hours) * 3600)));
    let mut latest: BTreeMap<&str, &Snatch> = BTreeMap::new();
    let mut grabs: BTreeMap<&str, usize> = BTreeMap::new();
    for s in history {
        let Some(id) = s.issue_id.as_deref() else {
            continue;
        };
        if s.status.as_deref() == Some("Snatched") {
            *grabs.entry(id).or_default() += 1;
        }
        let newer = latest
            .get(id)
            .is_none_or(|cur| s.date_added.as_deref() >= cur.date_added.as_deref());
        if newer {
            latest.insert(id, s);
        }
    }
    let mut stuck: Vec<&Snatch> = latest
        .into_values()
        .filter(|s| s.status.as_deref() == Some("Snatched"))
        .filter(|s| s.date_added.as_deref().is_some_and(|d| d < before.as_str()))
        .collect();
    stuck.sort_by(|a, b| a.date_added.cmp(&b.date_added));
    StuckSnatched {
        threshold_hours,
        count: stuck.len(),
        repeat_grabbed: grabs.values().filter(|n| **n > 1).count(),
        oldest: stuck.first().and_then(|s| s.date_added.clone()),
        sample: stuck
            .iter()
            .take(STUCK_SAMPLE)
            .map(|s| StuckIssue {
                comic: s.comic_name.clone().unwrap_or_default(),
                issue: s.issue_number.clone().unwrap_or_default(),
                snatched_at: s.date_added.clone().unwrap_or_default(),
                provider: s.provider.clone(),
            })
            .collect(),
    }
}

fn search(ini: &ConfigIni) -> SearchCheck {
    let raw = ini.get("search_delay");
    let minutes = raw.and_then(|v| v.parse::<i64>().ok());
    SearchCheck {
        search_delay_minutes: minutes,
        search_delay_valid: ini.has("search_delay").then(|| {
            raw.is_some_and(|v| v.bytes().all(|b| b.is_ascii_digit())) && minutes >= Some(1)
        }),
    }
}

fn torrent_downloader_name(v: i64) -> String {
    match v {
        0 => "watchfolder".into(),
        1 => "utorrent".into(),
        2 => "rtorrent".into(),
        3 => "transmission".into(),
        4 => "deluge".into(),
        5 => "qbittorrent".into(),
        other => other.to_string(),
    }
}

/// Whether the selected torrent client has the setting its `connect` needs; a
/// missing host leaves Mylar's client object without `.client`.
fn client_configured(ini: &ConfigIni, downloader: i64) -> Option<bool> {
    let host = |k: &str| ini.has(k).then(|| ini.get(k).is_some());
    match downloader {
        0 => {
            let local =
                ini.flag("torrent_local") == Some(true) && ini.get("local_watchdir").is_some();
            let seedbox =
                ini.flag("torrent_seedbox") == Some(true) && ini.get("seedbox_watchdir").is_some();
            let known = ini.has("torrent_local") || ini.has("torrent_seedbox");
            known.then_some(local || seedbox)
        }
        1 => host("utorrent_host"),
        2 => host("rtorrent_host"),
        3 => host("transmission_host"),
        4 => host("deluge_host"),
        5 => host("qbittorrent_host"),
        _ => None,
    }
}

fn torrents(ini: &ConfigIni) -> TorrentCheck {
    let torrents_enabled = ini.flag("enable_torrents");
    let torrent_search_enabled = ini.flag("enable_torrent_search");
    let torznab_enabled = ini.flag("enable_torznab");
    let downloader = ini.int("torrent_downloader");
    let client_configured = downloader.and_then(|d| client_configured(ini, d));
    let searching = match (torrent_search_enabled, torznab_enabled) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    };
    let drift = match searching {
        Some(false) => Some(false),
        Some(true) => match (torrents_enabled, client_configured) {
            (Some(false), _) | (_, Some(false)) => Some(true),
            (Some(true), Some(true)) => Some(false),
            _ => None,
        },
        None => None,
    };
    TorrentCheck {
        torrents_enabled,
        torrent_search_enabled,
        torznab_enabled,
        downloader: downloader.map(torrent_downloader_name),
        client_configured,
        drift,
    }
}

fn fmt_opt(v: Option<bool>) -> String {
    v.map_or_else(|| "unknown".into(), |b| b.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Timestamp {
        Timestamp::parse_rfc3339("2026-10-05T12:00:00Z").unwrap()
    }

    fn ini(pairs: &[(&str, &str)]) -> ConfigIni {
        ConfigIni(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    fn issue(release: &str, cover: &str) -> WantedIssue {
        WantedIssue {
            comic_name: Some("X".into()),
            issue_number: Some("1".into()),
            release_date: Some(release.into()),
            issue_date: Some(cover.into()),
        }
    }

    fn snatch(id: &str, at: &str, status: &str) -> Snatch {
        Snatch {
            issue_id: Some(id.into()),
            comic_name: Some(format!("C{id}")),
            issue_number: Some("1".into()),
            date_added: Some(at.into()),
            status: Some(status.into()),
            provider: Some("NZBGeek".into()),
        }
    }

    #[test]
    fn retention_counts_wanted_hidden_by_the_cutoff() {
        let wanted = Wanted {
            issues: vec![
                issue("2010-01-06", "2010-03-01"),
                issue("0000-00-00", "2016-12-01"),
                issue("2020-05-05", "2020-07-01"),
            ],
            annuals: vec![issue("", "1999-01-00")],
        };
        let s = build(
            "m",
            3,
            &wanted,
            &[],
            Ok(ini(&[("usenet_retention", "3500")])),
            24,
            now(),
        );
        // 2026-10-05 minus 3500 days.
        assert_eq!(s.retention.cutoff_date.as_deref(), Some("2017-03-06"));
        assert_eq!(
            s.retention.oldest_wanted_date.as_deref(),
            Some("1999-01-00")
        );
        assert_eq!(s.retention.wanted_dated, 4);
        assert_eq!(s.retention.wanted_before_cutoff, Some(3));
        assert_eq!(s.retention.drift, Some(true));
        assert_eq!(s.wanted, 4);
        assert!(s.findings.iter().any(|f| f.contains("maxage=3500")));
        assert!(!s.healthy);

        let s = build(
            "m",
            3,
            &wanted,
            &[],
            Ok(ini(&[("usenet_retention", "12000")])),
            24,
            now(),
        );
        assert_eq!(s.retention.drift, Some(false));
    }

    #[test]
    fn unreadable_config_reports_unknown_not_guesses() {
        let wanted = Wanted {
            issues: vec![issue("2010-01-06", "")],
            annuals: vec![],
        };
        let s = build("m", 1, &wanted, &[], Err(anyhow!("forms login")), 24, now());
        assert_eq!(s.config_error.as_deref(), Some("forms login"));
        assert_eq!(s.unknown.len(), KEYS.len());
        assert_eq!(s.retention.usenet_retention_days, None);
        assert_eq!(s.retention.drift, None);
        assert_eq!(
            s.retention.oldest_wanted_date.as_deref(),
            Some("2010-01-06")
        );
        assert_eq!(s.post_processing.completed_download_handling, None);
        assert_eq!(s.torrents.drift, None);
        assert_eq!(s.search.search_delay_valid, None);
        assert!(s.findings.is_empty(), "{:?}", s.findings);
        assert!(!s.healthy);
    }

    #[test]
    fn stuck_uses_each_issues_latest_row() {
        let history = vec![
            snatch("1", "2026-09-01 10:00:00", "Snatched"),
            snatch("1", "2026-09-02 10:00:00", "Snatched"),
            snatch("2", "2026-09-03 10:00:00", "Snatched"),
            snatch("2", "2026-09-03 11:00:00", "Post-Processed"),
            snatch("3", "2026-10-05 09:00:00", "Snatched"),
        ];
        let s = build(
            "m",
            0,
            &Wanted::default(),
            &history,
            Ok(ini(&[])),
            24,
            now(),
        );
        assert_eq!(s.stuck_snatched.count, 1);
        assert_eq!(s.stuck_snatched.repeat_grabbed, 1);
        assert_eq!(
            s.stuck_snatched.oldest.as_deref(),
            Some("2026-09-02 10:00:00")
        );
        assert_eq!(s.stuck_snatched.sample[0].comic, "C1");
        let s = build("m", 0, &Wanted::default(), &history, Ok(ini(&[])), 1, now());
        assert_eq!(s.stuck_snatched.count, 2);
    }

    #[test]
    fn cdh_findings() {
        let off = ini(&[
            ("newznab", "True"),
            ("nzb_downloader", "0"),
            ("sab_host", "http://10.0.0.15:8080"),
            ("sab_apikey", "k"),
            ("sab_category", "comics"),
            ("sab_client_post_processing", "False"),
            ("enable_check_folder", "False"),
            ("post_processing", "True"),
        ]);
        let s = build("m", 0, &Wanted::default(), &[], Ok(off), 24, now());
        assert_eq!(s.post_processing.nzb_downloader.as_deref(), Some("sabnzbd"));
        assert_eq!(s.post_processing.sab_api_key_set, Some(true));
        assert!(
            s.findings.iter().any(|f| f.contains("never imported")),
            "{:?}",
            s.findings
        );

        let none = ini(&[("newznab", "True"), ("nzb_downloader", "3")]);
        let s = build("m", 0, &Wanted::default(), &[], Ok(none), 24, now());
        assert!(s.findings.iter().any(|f| f.contains("no search providers")));
    }

    #[test]
    fn search_delay_validity() {
        let ok = build(
            "m",
            0,
            &Wanted::default(),
            &[],
            Ok(ini(&[("search_delay", "5")])),
            24,
            now(),
        );
        assert_eq!(ok.search.search_delay_minutes, Some(5));
        assert_eq!(ok.search.search_delay_valid, Some(true));
        let bad = build(
            "m",
            0,
            &Wanted::default(),
            &[],
            Ok(ini(&[("search_delay", "5.0")])),
            24,
            now(),
        );
        assert_eq!(bad.search.search_delay_valid, Some(false));
    }

    #[test]
    fn torrent_search_without_a_client_is_drift() {
        let cfg = ini(&[
            ("enable_torrents", "True"),
            ("enable_torrent_search", "True"),
            ("enable_torznab", "False"),
            ("torrent_downloader", "5"),
            ("qbittorrent_host", "None"),
        ]);
        let s = build("m", 0, &Wanted::default(), &[], Ok(cfg), 24, now());
        assert_eq!(s.torrents.downloader.as_deref(), Some("qbittorrent"));
        assert_eq!(s.torrents.client_configured, Some(false));
        assert_eq!(s.torrents.drift, Some(true));

        let cfg = ini(&[
            ("enable_torrents", "True"),
            ("enable_torrent_search", "False"),
            ("enable_torznab", "True"),
            ("torrent_downloader", "5"),
            ("qbittorrent_host", "http://10.0.0.16:8080"),
        ]);
        let s = build("m", 0, &Wanted::default(), &[], Ok(cfg), 24, now());
        assert_eq!(s.torrents.drift, Some(false));
    }
}

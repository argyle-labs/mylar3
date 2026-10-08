//! Remediation: `mylar3.configure` (settings drift) and `mylar3.backlog.process`
//! (import finished downloads Mylar never picked up).

use plugin_toolkit::prelude::*;
use plugin_toolkit::time::Timestamp;

use crate::api::{ConfigIni, Mylar};
use crate::status;

/// `checked_configs` in Mylar's `configUpdate` (`mylar/webserve.py`): the form's
/// checkboxes, which it sets False whenever they are not posted.
const FORM_CHECKBOXES: &[&str] = &[
    "enable_https",
    "launch_browser",
    "backup_on_start",
    "syno_fix",
    "auto_update",
    "annuals_on",
    "api_enabled",
    "nzb_startup_search",
    "enforce_perms",
    "sab_to_mylar",
    "torrent_local",
    "torrent_seedbox",
    "rtorrent_ssl",
    "rtorrent_verify",
    "rtorrent_startonload",
    "enable_torrents",
    "enable_rss",
    "experimental",
    "enable_torrent_search",
    "enable_32p",
    "enable_torznab",
    "newznab",
    "use_minsize",
    "use_maxsize",
    "ddump",
    "failed_download_handling",
    "sab_client_post_processing",
    "nzbget_client_post_processing",
    "failed_auto",
    "post_processing",
    "enable_check_folder",
    "enable_pre_scripts",
    "enable_snatch_script",
    "enable_extra_scripts",
    "enable_meta",
    "cbr2cbz_only",
    "ct_tag_cr",
    "ct_tag_cbl",
    "ct_cbz_overwrite",
    "cmtag_start_year_as_volume",
    "cmtag_volume",
    "setdefaultvolume",
    "rename_files",
    "replace_spaces",
    "zero_level",
    "sab_remove_completed",
    "sab_remove_failed",
    "lowercase_filenames",
    "autowant_upcoming",
    "autowant_all",
    "comic_cover_local",
    "cover_folder_local",
    "series_metadata_local",
    "alternate_latest_series_covers",
    "cvinfo",
    "snatchedtorrent_notify",
    "prowl_enabled",
    "prowl_onsnatch",
    "pushover_enabled",
    "pushover_onsnatch",
    "pushover_image",
    "mattermost_enabled",
    "mattermost_onsnatch",
    "boxcar_enabled",
    "boxcar_onsnatch",
    "pushbullet_enabled",
    "pushbullet_onsnatch",
    "telegram_enabled",
    "telegram_onsnatch",
    "telegram_image",
    "discord_enabled",
    "discord_onsnatch",
    "slack_enabled",
    "slack_onsnatch",
    "email_enabled",
    "email_enc",
    "email_ongrab",
    "email_onpost",
    "gotify_enabled",
    "gotify_server_url",
    "gotify_token",
    "gotify_onsnatch",
    "opds_enable",
    "opds_authentication",
    "opds_metainfo",
    "opds_pagesize",
    "enable_ddl",
    "enable_getcomics",
    "enable_external_server",
    "ddl_prefer_upscaled",
    "deluge_pause",
];

/// Provider lists `configUpdate` rebuilds from posted `<prefix>_<field><id>` rows
/// and empties when none are posted. Field order is Mylar's stored tuple order.
const PROVIDER_LISTS: &[(&str, &str, [&str; 6])] = &[
    (
        "extra_newznabs",
        "newznab",
        ["name", "host", "verify", "apikey", "uid", "enabled"],
    ),
    (
        "extra_torznabs",
        "torznab",
        ["name", "host", "verify", "apikey", "category", "enabled"],
    ),
];

pub const DEFAULT_USENET_RETENTION: u32 = 6000;

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingChange {
    /// `config.ini` key.
    pub key: String,
    /// `None` when the key is absent from `config.ini`.
    pub current: Option<String>,
    pub target: String,
    pub reason: String,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigureReport {
    pub name: String,
    pub changes: Vec<SettingChange>,
    pub in_sync: bool,
    /// The settings form was submitted.
    pub applied: bool,
    /// Re-read after the write: every change landed and nothing else moved.
    pub verified: bool,
    /// Settings that changed on write without being planned, `key: before -> after`.
    pub side_effects: Vec<String>,
    pub dry_run: bool,
}

pub fn plan(ini: &ConfigIni, usenet_retention: u32) -> Vec<SettingChange> {
    let mut out = Vec::new();
    if ini.int("usenet_retention") != Some(i64::from(usenet_retention)) {
        out.push(SettingChange {
            key: "usenet_retention".into(),
            current: ini.get("usenet_retention").map(str::to_string),
            target: usenet_retention.to_string(),
            reason: "sent to indexers as newznab maxage; older posts are never returned".into(),
        });
    }
    if ini.flag("newznab") == Some(true)
        && ini.int("nzb_downloader") == Some(3)
        && ini.get("sab_host").is_some()
    {
        out.push(SettingChange {
            key: "nzb_downloader".into(),
            current: Some("3".into()),
            target: "0".into(),
            reason: "with no downloader Mylar aborts every NZB search; SABnzbd is configured"
                .into(),
        });
    }
    out
}

/// The `/configUpdate` body that applies `changes` and leaves the rest of the
/// form as `ini` has it. The form is all-or-nothing, so every checkbox and
/// provider is re-posted at its current value; refuses when that value cannot
/// be known.
pub fn form(ini: &ConfigIni, changes: &[SettingChange]) -> Result<Vec<(String, String)>> {
    // A minimal ini omits defaulted keys, so an absent checkbox's value is unknown.
    if ini.flag("minimal_ini") == Some(true) {
        bail!("minimal_ini is on: config.ini omits default values, so the settings form cannot be re-posted unchanged");
    }
    let mut fields: Vec<(String, String)> = changes
        .iter()
        .map(|c| (c.key.clone(), c.target.clone()))
        .collect();
    for key in FORM_CHECKBOXES {
        if changes.iter().any(|c| c.key == *key) {
            continue;
        }
        if let Some(v) = ini.0.get(*key) {
            fields.push((key.to_string(), v.clone()));
        }
    }
    for (ini_key, prefix, columns) in PROVIDER_LISTS {
        let Some(raw) = ini.get(ini_key) else {
            continue;
        };
        let parts: Vec<&str> = raw.split(", ").collect();
        if !parts.len().is_multiple_of(7) {
            bail!("{ini_key} does not split into 7-field provider rows; refusing to re-post it");
        }
        for row in parts.chunks(7) {
            let id = row[6];
            if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
                bail!("{ini_key} has a provider row with non-numeric id '{id}'; refusing to re-post it");
            }
            for (column, value) in columns.iter().zip(row) {
                fields.push((format!("{prefix}_{column}{id}"), value.to_string()));
            }
        }
    }
    Ok(fields)
}

/// Every key whose value differs between `before` and `after`, other than
/// `planned`. Provider lists embed API keys, so secret-bearing values are elided.
fn side_effects(before: &ConfigIni, after: &ConfigIni, planned: &[SettingChange]) -> Vec<String> {
    const SECRET: &[&str] = &[
        "apikey",
        "api_key",
        "password",
        "token",
        "extra_newznabs",
        "extra_torznabs",
    ];
    let keys: std::collections::BTreeSet<&String> = before.0.keys().chain(after.0.keys()).collect();
    keys.into_iter()
        .filter(|k| !planned.iter().any(|c| &c.key == *k))
        .filter(|k| before.0.get(*k) != after.0.get(*k))
        .map(|k| {
            let secret = SECRET.iter().any(|s| k.contains(s));
            let show = |v: Option<&String>| match v {
                None => "unset".to_string(),
                Some(_) if secret => "<redacted>".to_string(),
                Some(v) => v.clone(),
            };
            format!("{k}: {} -> {}", show(before.0.get(k)), show(after.0.get(k)))
        })
        .collect()
}

/// Plan the drift against `usenet_retention`; with `execute`, submit the
/// settings form and re-read to confirm.
pub async fn configure(
    name: &str,
    m: &Mylar,
    usenet_retention: u32,
    execute: bool,
) -> Result<ConfigureReport> {
    let before = m
        .config()
        .await
        .context("read Mylar's settings to plan configure")?;
    let changes = plan(&before, usenet_retention);
    let mut report = ConfigureReport {
        name: name.to_string(),
        in_sync: changes.is_empty(),
        changes,
        applied: false,
        verified: false,
        side_effects: Vec::new(),
        dry_run: !execute,
    };
    if !execute || report.in_sync {
        return Ok(report);
    }
    let fields = form(&before, &report.changes)?;
    m.config_update(fields).await?;
    report.applied = true;
    let after = m
        .config()
        .await
        .context("settings were submitted; re-read to verify failed")?;
    let missed: Vec<&str> = plan(&after, usenet_retention)
        .iter()
        .filter_map(|c| report.changes.iter().find(|p| p.key == c.key))
        .map(|c| c.key.as_str())
        .collect();
    report.side_effects = side_effects(&before, &after, &report.changes);
    report.verified = missed.is_empty() && report.side_effects.is_empty();
    if !missed.is_empty() {
        bail!(
            "mylar3.configure submitted the settings form but [{}] still drift; other changes: [{}]",
            missed.join(", "),
            report.side_effects.join(", ")
        );
    }
    Ok(report)
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklogProcess {
    pub name: String,
    pub folder: String,
    /// Issues stuck at Snatched past the threshold before this call.
    pub stuck_snatched: usize,
    pub stuck_hours: u32,
    /// Mylar accepted the post-processing request. It runs in Mylar's queue;
    /// re-run `mylar3.status` to watch `stuck_snatched` fall.
    pub queued: bool,
    pub mylar_reply: Option<String>,
    pub dry_run: bool,
}

pub async fn process_backlog(
    name: &str,
    m: &Mylar,
    folder: &str,
    stuck_hours: u32,
    execute: bool,
) -> Result<BacklogProcess> {
    if !folder.starts_with('/') {
        bail!("folder must be an absolute path as Mylar's container sees it, got '{folder}'");
    }
    let history = m.history().await?;
    let stuck = status::stuck(&history, stuck_hours, Timestamp::now()).count;
    let mylar_reply = if execute {
        Some(m.force_process(folder).await?)
    } else {
        None
    };
    Ok(BacklogProcess {
        name: name.to_string(),
        folder: folder.to_string(),
        stuck_snatched: stuck,
        stuck_hours,
        queued: mylar_reply.is_some(),
        mylar_reply,
        dry_run: !execute,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin_toolkit::serde_json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn server(rows: serde_json::Value) -> MockServer {
        let server = MockServer::start().await;
        mount_config(&server, rows, None).await;
        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("cmd", "getHistory"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "data": [{"IssueID": "1", "DateAdded": "2020-01-01 00:00:00", "Status": "Snatched"}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("cmd", "forceProcess"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "Successfully submitted request for post-processing for Manual Run",
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/configUpdate"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        server
    }

    /// `times` limits how many reads this table answers, so a later mount can
    /// serve the post-write state.
    async fn mount_config(server: &MockServer, rows: serde_json::Value, times: Option<u64>) {
        let mock = Mock::given(method("GET"))
            .and(path("/getConfig"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "aaData": rows })),
            );
        match times {
            Some(n) => mock.up_to_n_times(n).mount(server).await,
            None => mock.mount(server).await,
        }
    }

    async fn posted_form(server: &MockServer) -> Vec<(String, String)> {
        let req = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.url.path() == "/configUpdate")
            .expect("configUpdate posted");
        String::from_utf8(req.body)
            .unwrap()
            .split('&')
            .map(|pair| {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                let dec = |s: &str| plugin_toolkit::url::decode(&s.replace('+', " ")).unwrap();
                (dec(k), dec(v))
            })
            .collect()
    }

    async fn writes(server: &MockServer) -> usize {
        server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| {
                r.url.path() == "/configUpdate"
                    || r.url
                        .query()
                        .is_some_and(|q| q.contains("cmd=forceProcess"))
            })
            .count()
    }

    #[tokio::test]
    async fn configure_dry_run_plans_retention_and_downloader() {
        let server = server(serde_json::json!([
            ["usenet_retention", "3500"],
            ["newznab", "True"],
            ["nzb_downloader", "3"],
            ["sab_host", "http://10.0.0.15:8080"]
        ]))
        .await;
        let m = Mylar::new(&server.uri(), "KEY", None);
        let r = configure("m", &m, DEFAULT_USENET_RETENTION, false)
            .await
            .unwrap();
        assert!(r.dry_run && !r.applied && !r.in_sync);
        assert_eq!(r.changes.len(), 2);
        assert_eq!(r.changes[0].key, "usenet_retention");
        assert_eq!(r.changes[0].current.as_deref(), Some("3500"));
        assert_eq!(r.changes[0].target, "6000");
        assert_eq!(r.changes[1].key, "nzb_downloader");
        assert_eq!(writes(&server).await, 0);
    }

    fn live_rows(retention: &str) -> serde_json::Value {
        serde_json::json!([
            ["usenet_retention", retention],
            ["newznab", "True"],
            ["post_processing", "True"],
            ["api_enabled", "True"],
            ["enable_check_folder", "False"],
            ["nzb_downloader", "0"],
            ["sab_host", "http://10.0.0.15:8080"],
            ["extra_newznabs", "NZBGeek, https://api.nzbgeek.info, 0, KEY1, , 1, 1, DOGnzb, https://api.dognzb.cr, 1, KEY2, 7030#7020, 0, 4"],
            ["extra_torznabs", "None"]
        ])
    }

    #[tokio::test]
    async fn configure_execute_fixes_retention_and_reposts_the_rest() {
        let server = MockServer::start().await;
        mount_config(&server, live_rows("3500"), Some(1)).await;
        mount_config(&server, live_rows("6000"), None).await;
        Mock::given(method("POST"))
            .and(path("/configUpdate"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let m = Mylar::new(&server.uri(), "KEY", None);
        let r = configure("m", &m, DEFAULT_USENET_RETENTION, true)
            .await
            .unwrap();
        assert!(r.applied && r.verified && !r.dry_run, "{r:?}");
        assert_eq!(r.changes.len(), 1);
        assert_eq!(r.changes[0].current.as_deref(), Some("3500"));
        assert!(r.side_effects.is_empty(), "{:?}", r.side_effects);

        let form = posted_form(&server).await;
        let get = |k: &str| form.iter().find(|(f, _)| f == k).map(|(_, v)| v.as_str());
        assert_eq!(get("usenet_retention"), Some("6000"));
        assert_eq!(get("newznab"), Some("True"));
        assert_eq!(get("post_processing"), Some("True"));
        assert_eq!(get("api_enabled"), Some("True"));
        assert_eq!(get("enable_check_folder"), Some("False"));
        assert_eq!(get("newznab_name1"), Some("NZBGeek"));
        assert_eq!(get("newznab_uid1"), Some(""));
        assert_eq!(get("newznab_apikey4"), Some("KEY2"));
        assert_eq!(get("newznab_uid4"), Some("7030#7020"));
        assert_eq!(get("newznab_enabled4"), Some("0"));
        assert!(form.iter().all(|(k, _)| !k.starts_with("torznab_")));
        // Keys outside the form's checkboxes are left to Mylar's current value.
        assert_eq!(get("sab_host"), None);
    }

    #[tokio::test]
    async fn configure_execute_reports_side_effects() {
        let server = MockServer::start().await;
        mount_config(&server, live_rows("3500"), Some(1)).await;
        let mut after = live_rows("6000");
        after[3] = serde_json::json!(["api_enabled", "False"]);
        mount_config(&server, after, None).await;
        Mock::given(method("POST"))
            .and(path("/configUpdate"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let m = Mylar::new(&server.uri(), "KEY", None);
        let r = configure("m", &m, 6000, true).await.unwrap();
        assert!(r.applied && !r.verified);
        assert_eq!(
            r.side_effects,
            vec!["api_enabled: True -> False".to_string()]
        );

        let mut changed = live_rows("6000");
        changed[7] = serde_json::json!(["extra_newznabs", ""]);
        let after = ConfigIni(
            changed
                .as_array()
                .unwrap()
                .iter()
                .map(|r| (r[0].as_str().unwrap().into(), r[1].as_str().unwrap().into()))
                .collect(),
        );
        let before = m.config().await.unwrap();
        let fx = side_effects(&before, &after, &[]);
        assert!(
            fx.contains(&"extra_newznabs: <redacted> -> <redacted>".to_string()),
            "{fx:?}"
        );
    }

    #[tokio::test]
    async fn configure_execute_errors_when_the_change_does_not_land() {
        let server = server(live_rows("3500")).await;
        let m = Mylar::new(&server.uri(), "KEY", None);
        let err = configure("m", &m, 6000, true)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("[usenet_retention] still drift"), "{err}");
    }

    #[test]
    fn form_refuses_what_it_cannot_repost() {
        let change = |k: &str| SettingChange {
            key: k.into(),
            current: None,
            target: "6000".into(),
            reason: String::new(),
        };
        let ini = |pairs: &[(&str, &str)]| {
            ConfigIni(
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            )
        };
        let err = form(
            &ini(&[("minimal_ini", "True")]),
            &[change("usenet_retention")],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("minimal_ini"), "{err}");
        let err = form(
            &ini(&[("extra_newznabs", "a, b, c")]),
            &[change("usenet_retention")],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("7-field"), "{err}");
        let err = form(
            &ini(&[("extra_torznabs", "a, b, 0, k, 8000, 1, x")]),
            &[change("usenet_retention")],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("non-numeric id"), "{err}");
    }

    #[tokio::test]
    async fn configure_in_sync_is_ok_on_execute() {
        let server = server(serde_json::json!([["usenet_retention", "6000"]])).await;
        let m = Mylar::new(&server.uri(), "KEY", None);
        let r = configure("m", &m, 6000, true).await.unwrap();
        assert!(r.in_sync && !r.dry_run && !r.applied);
        assert_eq!(writes(&server).await, 0);
    }

    #[tokio::test]
    async fn backlog_dry_run_queues_nothing() {
        let server = server(serde_json::json!([])).await;
        let m = Mylar::new(&server.uri(), "KEY", None);
        let r = process_backlog("m", &m, "/downloads/completed/comics", 24, false)
            .await
            .unwrap();
        assert!(r.dry_run && !r.queued);
        assert_eq!(r.stuck_snatched, 1);
        assert_eq!(writes(&server).await, 0);
    }

    #[tokio::test]
    async fn backlog_execute_queues_manual_run() {
        let server = server(serde_json::json!([])).await;
        let m = Mylar::new(&server.uri(), "KEY", None);
        let r = process_backlog("m", &m, "/downloads/completed/comics", 24, true)
            .await
            .unwrap();
        assert!(r.queued && !r.dry_run);
        assert_eq!(writes(&server).await, 1);
    }

    #[tokio::test]
    async fn backlog_rejects_relative_folder() {
        let server = server(serde_json::json!([])).await;
        let m = Mylar::new(&server.uri(), "KEY", None);
        assert!(process_backlog("m", &m, "downloads", 24, true)
            .await
            .is_err());
        assert_eq!(writes(&server).await, 0);
    }
}

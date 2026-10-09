//! Remediation: `mylar3.configure` (settings drift) and `mylar3.backlog.process`
//! (import finished downloads Mylar never picked up).

use plugin_toolkit::prelude::*;
use plugin_toolkit::time::Timestamp;

use crate::api::{ConfigIni, Mylar};
use crate::status;
use crate::write::{self, Providers, SettingChange, Write};

pub const DEFAULT_USENET_RETENTION: u32 = 6000;

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigureReport {
    pub name: String,
    /// Secret keys' values are redacted.
    pub changes: Vec<SettingChange>,
    /// Drift this tool will not write, with what to set by hand.
    pub manual: Vec<String>,
    /// No changes and nothing manual.
    pub in_sync: bool,
    /// Mylar accepted the settings form.
    pub applied: bool,
    /// A re-read after the write shows every change and nothing else moved.
    /// `/getConfig` serves Mylar's in-memory settings, so this does not prove
    /// the `config.ini` write reached disk.
    pub verified: bool,
    pub dry_run: bool,
}

/// `usenet_retention`: an explicit target is matched exactly; without one, only
/// values below [`DEFAULT_USENET_RETENTION`] are raised, so a longer retention
/// an operator chose is never cut.
pub fn plan(ini: &ConfigIni, usenet_retention: Option<u32>) -> (Vec<SettingChange>, Vec<String>) {
    let mut changes = Vec::new();
    let mut manual = Vec::new();
    let current = ini.int("usenet_retention");
    let target = match usenet_retention {
        Some(t) => (current != Some(i64::from(t))).then_some(t),
        None => current
            .is_none_or(|c| c < i64::from(DEFAULT_USENET_RETENTION))
            .then_some(DEFAULT_USENET_RETENTION),
    };
    if let Some(target) = target {
        changes.push(SettingChange {
            key: "usenet_retention".into(),
            current: ini.get("usenet_retention").map(str::to_string),
            target: target.to_string(),
            reason: "sent to indexers as newznab maxage; older posts are never returned".into(),
        });
    }
    if ini.flag("newznab") == Some(true)
        && ini.int("nzb_downloader") == Some(3)
        && ini.get("sab_host").is_some()
    {
        // With SABnzbd selected, Mylar's configure() fills a missing SAB path
        // mapping in memory only, so the switch is safe only once it is set.
        let mapped = ini.get("sab_directory").is_some() || ini.flag("sab_to_mylar") == Some(true);
        if ini.get("sab_apikey").is_some() && mapped {
            changes.push(SettingChange {
                key: "nzb_downloader".into(),
                current: Some("3".into()),
                target: "0".into(),
                reason: "with no downloader Mylar aborts every NZB search; SABnzbd is configured"
                    .into(),
            });
        } else {
            manual.push(
                "nzb_downloader=3 (none) aborts every NZB search; set sab_apikey and \
                 sab_directory (or sab_to_mylar) for the SABnzbd at sab_host, then select \
                 SABnzbd"
                    .into(),
            );
        }
    }
    (changes, manual)
}

/// Plan the drift; with `execute`, write it through [`write::apply`].
pub async fn configure(
    name: &str,
    m: &Mylar,
    usenet_retention: Option<u32>,
    execute: bool,
) -> Result<ConfigureReport> {
    if usenet_retention == Some(0) {
        bail!("usenet_retention must be at least 1 day");
    }
    let before = m
        .config()
        .await
        .context("read Mylar's settings to plan configure")?;
    let (changes, manual) = plan(&before, usenet_retention);
    let mut report = ConfigureReport {
        name: name.to_string(),
        in_sync: changes.is_empty() && manual.is_empty(),
        changes: changes.iter().map(SettingChange::redacted).collect(),
        manual,
        applied: false,
        verified: false,
        dry_run: !execute,
    };
    if !execute || changes.is_empty() {
        return Ok(report);
    }
    let w = Write {
        changes,
        providers: Providers::parse(&before)?,
    };
    write::apply(m, &before, &w).await?;
    report.applied = true;
    report.verified = true;
    Ok(report)
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklogProcess {
    pub name: String,
    /// The requested folder with `.` and repeated or trailing `/` removed.
    pub folder: String,
    /// Which Mylar download folder contains `folder`, e.g. `sab_directory /downloads`.
    pub allowed_by: String,
    /// Issues stuck at Snatched past the threshold before this call.
    pub stuck_snatched: usize,
    pub stuck_hours: u32,
    /// Mylar accepted the post-processing request. It runs in Mylar's queue;
    /// re-run `mylar3.status` to watch `stuck_snatched` fall.
    pub queued: bool,
    pub mylar_reply: Option<String>,
    pub dry_run: bool,
}

/// Path components of an absolute path, with `.` and empty segments dropped;
/// `None` for a relative path or one with `..`.
fn components(path: &str) -> Option<Vec<&str>> {
    if !path.starts_with('/') {
        return None;
    }
    let parts: Vec<&str> = path
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    (!parts.contains(&"..")).then_some(parts)
}

/// Checks `folder` against Mylar's settings: never the library
/// (`destination_dir`) or anything containing or inside it, and always inside a
/// download folder Mylar post-processes from.
pub fn check_folder(ini: &ConfigIni, folder: &str) -> Result<(String, String)> {
    let parts = components(folder).ok_or_else(|| {
        anyhow!("folder must be an absolute path without `..`, as Mylar's container sees it")
    })?;
    if parts.is_empty() {
        bail!("folder must not be `/`");
    }
    let resolved = format!("/{}", parts.join("/"));
    let dest = ini
        .get("destination_dir")
        .and_then(|d| components(d).map(|c| (d, c)))
        .ok_or_else(|| {
            anyhow!("destination_dir is unset or not an absolute path; cannot rule out the library")
        })?;
    if parts.starts_with(&dest.1) || dest.1.starts_with(&parts) {
        bail!(
            "{resolved} overlaps the library destination_dir {}; post-processing it would move library files",
            dest.0
        );
    }
    let roots: Vec<(&str, &str)> = ["sab_directory", "check_folder"]
        .into_iter()
        .filter_map(|k| ini.get(k).map(|v| (k, v)))
        .collect();
    for (key, root) in &roots {
        // A root of `/` has no components and would contain every path.
        if components(root).is_some_and(|r| !r.is_empty() && parts.starts_with(&r)) {
            return Ok((resolved, format!("{key} {root}")));
        }
    }
    let known: Vec<String> = roots.iter().map(|(k, v)| format!("{k}={v}")).collect();
    bail!(
        "{resolved} is not inside sab_directory or check_folder [{}]",
        known.join(", ")
    )
}

pub async fn process_backlog(
    name: &str,
    m: &Mylar,
    folder: &str,
    stuck_hours: u32,
    execute: bool,
) -> Result<BacklogProcess> {
    let ini = m
        .config()
        .await
        .context("read Mylar's settings to check the folder")?;
    let (folder, allowed_by) = check_folder(&ini, folder)?;
    let history = m.history().await?;
    let stuck = status::stuck(&history, stuck_hours, Timestamp::now()).count;
    let mylar_reply = if execute {
        Some(m.force_process(&folder).await?)
    } else {
        None
    };
    Ok(BacklogProcess {
        name: name.to_string(),
        folder,
        allowed_by,
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
    use crate::testkit::*;
    use crate::write::FORM_CHECKBOXES;
    use wiremock::ResponseTemplate;

    #[tokio::test]
    async fn dry_run_plans_retention_and_writes_nothing() {
        let server = server(&[table(&[])], ok()).await;
        let r = configure("m", &mylar(&server), None, false).await.unwrap();
        assert!(r.dry_run && !r.applied && !r.in_sync);
        assert_eq!(r.changes.len(), 1);
        assert_eq!(r.changes[0].key, "usenet_retention");
        assert_eq!(r.changes[0].current.as_deref(), Some("3500"));
        assert_eq!(r.changes[0].target, "6000");
        assert_eq!(writes(&server).await, 0);
    }

    #[tokio::test]
    async fn execute_fixes_retention_and_reposts_the_whole_form() {
        let before = table(&[]);
        let after = table(&[("usenet_retention", "6000")]);
        let server = server(&[before.clone(), before.clone(), after], ok()).await;
        let r = configure("m", &mylar(&server), None, true).await.unwrap();
        assert!(r.applied && r.verified && !r.dry_run, "{r:?}");

        let form = posted_form(&server).await;
        let get = |k: &str| form.iter().find(|(f, _)| f == k).map(|(_, v)| v.as_str());
        assert_eq!(get("usenet_retention"), Some("6000"));
        let current = ini(&before);
        for key in FORM_CHECKBOXES {
            assert_eq!(get(key), current.0.get(*key).map(String::as_str), "{key}");
        }
        assert_eq!(get("newznab_name1"), Some("NZBGeek"));
        assert_eq!(get("newznab_uid1"), Some(""));
        assert_eq!(get("newznab_apikey4"), Some("KEY2"));
        assert_eq!(get("newznab_uid4"), Some("7030#7020"));
        assert_eq!(get("newznab_enabled4"), Some("0"));
        assert_eq!(get("torznab_name6"), Some("Jackett"));
        assert_eq!(get("torznab_host6"), Some("http://10.0.0.16:9117/api"));
        assert_eq!(get("torznab_verify6"), Some("0"));
        assert_eq!(get("torznab_apikey6"), Some("TKEY"));
        assert_eq!(get("torznab_category6"), Some("7030#8000"));
        assert_eq!(get("torznab_enabled6"), Some("1"));
        assert_eq!(get("sab_host"), None);
        assert_eq!(get("comicvine_api"), None);
    }

    #[tokio::test]
    async fn execute_in_sync_writes_nothing() {
        let server = server(&[table(&[("usenet_retention", "6000")])], ok()).await;
        let r = configure("m", &mylar(&server), None, true).await.unwrap();
        assert!(r.in_sync && !r.dry_run && !r.applied);
        assert_eq!(writes(&server).await, 0);
    }

    #[tokio::test]
    async fn execute_fails_on_side_effects_with_redacted_diff_and_prior_values() {
        let before = table(&[]);
        let after = table(&[
            ("usenet_retention", "6000"),
            ("api_enabled", "False"),
            ("sab_apikey", "LEAKED"),
            ("gotify_token", "GLEAK"),
        ]);
        let server = server(&[before.clone(), before, after], ok()).await;
        let err = configure("m", &mylar(&server), None, true)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("api_enabled: True -> False"), "{err}");
        assert!(err.contains("api_enabled=True"), "{err}");
        assert!(
            err.contains("sab_apikey: <redacted> -> <redacted>"),
            "{err}"
        );
        assert!(err.contains("gotify_token: <redacted>"), "{err}");
        for secret in ["SABKEY", "LEAKED", "GTOKEN", "GLEAK"] {
            assert!(!err.contains(secret), "{secret} in {err}");
        }
    }

    #[tokio::test]
    async fn execute_errors_when_the_change_does_not_land() {
        let server = server(&[table(&[])], ok()).await;
        let err = configure("m", &mylar(&server), None, true)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("usenet_retention: wanted 6000, Mylar stored 3500"),
            "{err}"
        );
        assert!(
            !err.contains("[]") && !err.contains("other settings"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn execute_refuses_when_settings_move_after_the_plan() {
        let before = table(&[]);
        let moved = table(&[("post_processing", "False")]);
        let server = server(&[before, moved], ok()).await;
        let err = configure("m", &mylar(&server), None, true)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("changed since the plan"), "{err}");
        assert!(err.contains("post_processing: True -> False"), "{err}");
        assert_eq!(writes(&server).await, 0);
    }

    #[tokio::test]
    async fn rejected_write_reports_what_moved() {
        let before = table(&[]);
        let after = table(&[("enable_rss", "True")]);
        let server = server(
            &[before.clone(), before, after],
            ResponseTemplate::new(200).set_body_string("<html>Traceback</html>"),
        )
        .await;
        let err = configure("m", &mylar(&server), None, true)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("unexpected reply"), "{err}");
        assert!(err.contains("enable_rss: False -> True"), "{err}");
    }

    #[test]
    fn retention_only_raises_unless_explicit() {
        let at = |v: &str| ini(&table(&[("usenet_retention", v)]));
        assert!(plan(&at("12000"), None).0.is_empty());
        assert_eq!(plan(&at("3500"), None).0[0].target, "6000");
        assert_eq!(plan(&at("12000"), Some(4000)).0[0].target, "4000");
        assert!(plan(&at("4000"), Some(4000)).0.is_empty());
        let mut rows = table(&[]);
        unset(&mut rows, "usenet_retention");
        assert_eq!(plan(&ini(&rows), None).0[0].target, "6000");
    }

    #[tokio::test]
    async fn zero_retention_is_rejected() {
        let server = server(&[table(&[])], ok()).await;
        assert!(configure("m", &mylar(&server), Some(0), false)
            .await
            .is_err());
        assert_eq!(writes(&server).await, 0);
    }

    #[test]
    fn downloader_switch_needs_sab_fully_set() {
        let base = [("nzb_downloader", "3"), ("usenet_retention", "6000")];
        let with = |extra: &[(&str, &str)]| {
            let mut rows = table(&base);
            for (k, v) in extra {
                set(&mut rows, k, v);
            }
            plan(&ini(&rows), None)
        };
        let (changes, manual) = with(&[]);
        assert_eq!(changes[0].key, "nzb_downloader");
        assert!(manual.is_empty());

        let (changes, manual) = with(&[("sab_apikey", "None")]);
        assert!(changes.is_empty());
        assert!(manual[0].contains("sab_apikey"), "{manual:?}");

        let (changes, manual) = with(&[("sab_directory", "None"), ("sab_to_mylar", "False")]);
        assert!(changes.is_empty() && manual.len() == 1);

        let (changes, _) = with(&[("sab_directory", "None"), ("sab_to_mylar", "True")]);
        assert_eq!(changes[0].key, "nzb_downloader");

        let (changes, manual) = with(&[("sab_host", "None")]);
        assert!(changes.is_empty() && manual.is_empty());
    }

    #[tokio::test]
    async fn backlog_dry_run_shows_the_checked_folder() {
        let server = server(&[table(&[])], ok()).await;
        let r = process_backlog(
            "m",
            &mylar(&server),
            "/downloads/complete//./comics/",
            24,
            false,
        )
        .await
        .unwrap();
        assert!(r.dry_run && !r.queued);
        assert_eq!(r.folder, "/downloads/complete/comics");
        assert_eq!(r.allowed_by, "sab_directory /downloads/complete");
        assert_eq!(r.stuck_snatched, 1);
        assert_eq!(writes(&server).await, 0);
    }

    #[tokio::test]
    async fn backlog_execute_queues_manual_run() {
        let server = server(&[table(&[])], ok()).await;
        let r = process_backlog("m", &mylar(&server), "/downloads/complete", 24, true)
            .await
            .unwrap();
        assert!(r.queued && !r.dry_run);
        assert_eq!(writes(&server).await, 1);
    }

    #[test]
    fn backlog_folder_checks() {
        let rows = table(&[("check_folder", "/watch")]);
        let ok = |f: &str| check_folder(&ini(&rows), f);
        assert_eq!(ok("/watch/x").unwrap().1, "check_folder /watch");
        for bad in [
            "downloads",
            "/",
            "//",
            "/downloads/complete/../../comics",
            "/elsewhere",
        ] {
            assert!(ok(bad).is_err(), "{bad}");
        }
        let lib = table(&[
            ("sab_directory", "/data"),
            ("destination_dir", "/data/comics"),
        ]);
        let err = check_folder(&ini(&lib), "/data/comics/new")
            .unwrap_err()
            .to_string();
        assert!(err.contains("destination_dir"), "{err}");
        assert!(check_folder(&ini(&lib), "/data/comics").is_err());
        assert!(check_folder(&ini(&lib), "/data").is_err());
        assert!(check_folder(&ini(&lib), "/data/complete").is_ok());

        // Relative paths resolve against Mylar's unknown working directory.
        assert_eq!(components("downloads/complete"), None);
        let relative = table(&[("sab_directory", "downloads"), ("check_folder", "/watch")]);
        assert!(check_folder(&ini(&relative), "downloads/complete").is_err());
        assert!(check_folder(&ini(&relative), "/downloads/complete").is_err());
        let relative_lib = table(&[("destination_dir", "comics")]);
        let err = check_folder(&ini(&relative_lib), "/downloads/complete")
            .unwrap_err()
            .to_string();
        assert!(err.contains("destination_dir"), "{err}");

        let slash = table(&[("sab_directory", "/"), ("check_folder", "None")]);
        assert!(check_folder(&ini(&slash), "/downloads/complete").is_err());

        let mut no_dest = table(&[]);
        unset(&mut no_dest, "destination_dir");
        let err = check_folder(&ini(&no_dest), "/downloads/complete")
            .unwrap_err()
            .to_string();
        assert!(err.contains("destination_dir"), "{err}");
    }

    #[tokio::test]
    async fn backlog_rejects_before_any_write() {
        let server = server(&[table(&[])], ok()).await;
        assert!(process_backlog("m", &mylar(&server), "/comics", 24, true)
            .await
            .is_err());
        assert_eq!(writes(&server).await, 0);
    }
}

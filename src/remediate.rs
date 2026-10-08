//! Remediation: `mylar3.configure` (settings drift) and `mylar3.backlog.process`
//! (import finished downloads Mylar never picked up).

use plugin_toolkit::prelude::*;
use plugin_toolkit::scrub;
use plugin_toolkit::time::Timestamp;

use crate::api::{ConfigIni, Mylar};
use crate::definitions::CONFIG_KEYS;
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

/// Secret-bearing keys `scrub::is_sensitive_key` does not recognise.
const MYLAR_SECRETS: &[&str] = &[
    "extra_newznabs",
    "extra_torznabs",
    "comicvine_api",
    "prowl_keys",
    "pushover_userkey",
    "passkey_32p",
    "username_32p",
    "seedbox_pass",
    "tab_pass",
    "pp_sshpasswd",
];

/// Prefix of a value Mylar stored with `encrypt_passwords`:
/// `^~$z$` + base64(secret + 8-byte salt), re-salted on every save.
const ENCRYPTED_PREFIX: &str = "^~$z$";

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

/// The `/configUpdate` body that applies `changes`. Mylar sets every
/// [`FORM_CHECKBOXES`] key not posted to False and replaces both provider lists
/// with the rows posted, while other keys keep their value unless posted. So
/// this posts the changes, every checkbox at its current value, and every
/// provider row; it refuses when that state cannot be read exactly.
pub fn form(ini: &ConfigIni, changes: &[SettingChange]) -> Result<Vec<(String, String)>> {
    // A minimal ini omits defaulted keys, so an absent checkbox's value is unknown.
    if ini.flag("minimal_ini") != Some(false) {
        bail!("minimal_ini is not False: config.ini may omit default values, so the settings form cannot be re-posted unchanged");
    }
    let missing: Vec<&str> = FORM_CHECKBOXES
        .iter()
        .copied()
        .filter(|k| !ini.has(k))
        .collect();
    if !missing.is_empty() {
        bail!(
            "this Mylar's settings lack form checkboxes [{}]; its version does not match the form this tool re-posts",
            missing.join(", ")
        );
    }
    // A key outside v0.8.3's definitions may be a checkbox another version's
    // form has, which this re-post would turn off; a stale legacy key also trips this.
    let unknown: Vec<&str> = ini
        .0
        .keys()
        .map(String::as_str)
        .filter(|k| CONFIG_KEYS.binary_search(k).is_err())
        .collect();
    if !unknown.is_empty() {
        bail!(
            "this Mylar's settings hold keys mylar3 v0.8.3 does not define [{}]; its form may differ from the one this tool re-posts",
            unknown.join(", ")
        );
    }
    let mut fields: Vec<(String, String)> = changes
        .iter()
        .map(|c| (c.key.clone(), c.target.clone()))
        .collect();
    for key in FORM_CHECKBOXES {
        if !changes.iter().any(|c| c.key == *key) {
            fields.push((key.to_string(), ini.0[*key].clone()));
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
        let mut ids = std::collections::BTreeSet::new();
        for row in parts.chunks(7) {
            let id = row[6];
            // A misparsed row can shift an API key into the id column; never echo it.
            if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
                bail!("{ini_key} has a provider row with a non-numeric id; refusing to re-post it");
            }
            if !ids.insert(id) {
                bail!("{ini_key} has two provider rows with id {id}; refusing to re-post it");
            }
            // configUpdate renames a nameless row after its host, or drops it.
            if row[0].is_empty() {
                bail!("{ini_key} provider {id} has no name; re-posting would rename or delete it");
            }
            for (column, value) in columns.iter().zip(row) {
                fields.push((format!("{prefix}_{column}{id}"), value.to_string()));
            }
        }
    }
    if fields.iter().any(|(_, v)| v.starts_with(ENCRYPTED_PREFIX)) {
        bail!("a re-posted setting holds an encrypted value, which Mylar would encrypt again");
    }
    Ok(fields)
}

/// Values compare by plaintext: an encrypted value is re-salted on every save.
fn normalized(v: &str) -> std::borrow::Cow<'_, [u8]> {
    let Some(b64) = v.strip_prefix(ENCRYPTED_PREFIX) else {
        return v.as_bytes().into();
    };
    match base64_decode(b64) {
        Some(mut bytes) if bytes.len() >= 8 => {
            bytes.truncate(bytes.len() - 8);
            bytes.into()
        }
        _ => v.as_bytes().into(),
    }
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.trim_end_matches('=').bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Values shown in diffs: form checkboxes (gotify's hold a URL and token) and
/// the planned keys. Everything else may be a reversible secret.
fn shown(key: &str, planned: &[SettingChange]) -> bool {
    let visible = (FORM_CHECKBOXES.contains(&key) && !key.starts_with("gotify_"))
        || planned.iter().any(|c| c.key == key);
    visible && !scrub::is_sensitive_key(key) && !MYLAR_SECRETS.contains(&key)
}

/// Keys whose value moved between `before` and `after`, other than `planned`.
fn changed<'a>(
    before: &'a ConfigIni,
    after: &'a ConfigIni,
    planned: &[SettingChange],
) -> Vec<&'a str> {
    let keys: std::collections::BTreeSet<&String> = before.0.keys().chain(after.0.keys()).collect();
    keys.into_iter()
        .filter(|k| !planned.iter().any(|c| &c.key == *k))
        .filter(|k| {
            before.0.get(*k).map(|v| normalized(v)) != after.0.get(*k).map(|v| normalized(v))
        })
        .map(String::as_str)
        .collect()
}

/// `key: before -> after` for each [`changed`] key, values outside [`shown`]
/// redacted.
fn diff(before: &ConfigIni, after: &ConfigIni, planned: &[SettingChange]) -> Vec<String> {
    changed(before, after, planned)
        .into_iter()
        .map(|k| {
            let show = |v: Option<&String>| match v {
                None => "unset".to_string(),
                Some(_) if !shown(k, planned) => scrub::REDACTED.to_string(),
                Some(v) => v.clone(),
            };
            format!("{k}: {} -> {}", show(before.0.get(k)), show(after.0.get(k)))
        })
        .collect()
}

/// Plan the drift; with `execute`, submit the settings form and re-read to
/// confirm the changes landed and nothing else moved.
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
        changes,
        manual,
        applied: false,
        verified: false,
        dry_run: !execute,
    };
    if !execute || report.changes.is_empty() {
        return Ok(report);
    }
    let fields = form(&before, &report.changes)?;
    let fresh = m
        .config()
        .await
        .context("re-read Mylar's settings before writing")?;
    let moved = diff(&before, &fresh, &[]);
    if !moved.is_empty() {
        bail!(
            "Mylar's settings changed since the plan was read: [{}]; nothing written, re-run",
            moved.join(", ")
        );
    }
    if let Err(e) = m.config_update(fields).await {
        match m.config().await {
            Ok(after) => {
                let moved = diff(&before, &after, &[]);
                bail!(
                    "{e:#}; settings now differ from before by: [{}]",
                    moved.join(", ")
                );
            }
            Err(re) => bail!("{e:#}; re-reading settings to see what changed failed: {re:#}"),
        }
    }
    report.applied = true;
    let after = m
        .config()
        .await
        .context("settings were submitted; re-read to verify failed")?;
    let missed: Vec<&str> = plan(&after, usenet_retention)
        .0
        .iter()
        .filter_map(|c| report.changes.iter().find(|p| p.key == c.key))
        .map(|c| c.key.as_str())
        .collect();
    let side_effects = diff(&before, &after, &report.changes);
    if !missed.is_empty() || !side_effects.is_empty() {
        let prior: Vec<String> = changed(&before, &after, &report.changes)
            .into_iter()
            .filter(|k| shown(k, &report.changes))
            .map(|k| format!("{k}={}", before.0.get(k).map_or("unset", String::as_str)))
            .collect();
        let mut parts = Vec::new();
        if !missed.is_empty() {
            parts.push(format!("[{}] still drift", missed.join(", ")));
        }
        if !side_effects.is_empty() {
            parts.push(format!(
                "other settings moved: [{}]",
                side_effects.join(", ")
            ));
        }
        if !prior.is_empty() {
            parts.push(format!(
                "prior values to restore by hand: [{}]",
                prior.join(", ")
            ));
        }
        bail!(
            "mylar3.configure submitted the settings form, but {}",
            parts.join("; ")
        );
    }
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
    use plugin_toolkit::serde_json::{json, Value};
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// `checked_configs` from mylar3 v0.8.3 `mylar/webserve.py`, verbatim.
    const UPSTREAM_V083: &str = r#"['enable_https', 'launch_browser', 'backup_on_start', 'syno_fix', 'auto_update', 'annuals_on', 'api_enabled', 'nzb_startup_search',
'enforce_perms', 'sab_to_mylar', 'torrent_local', 'torrent_seedbox', 'rtorrent_ssl', 'rtorrent_verify', 'rtorrent_startonload',
'enable_torrents', 'enable_rss', 'experimental', 'enable_torrent_search', 'enable_32p', 'enable_torznab',
'newznab', 'use_minsize', 'use_maxsize', 'ddump', 'failed_download_handling', 'sab_client_post_processing', 'nzbget_client_post_processing',
'failed_auto', 'post_processing', 'enable_check_folder', 'enable_pre_scripts', 'enable_snatch_script', 'enable_extra_scripts',
'enable_meta', 'cbr2cbz_only', 'ct_tag_cr', 'ct_tag_cbl', 'ct_cbz_overwrite', 'cmtag_start_year_as_volume', 'cmtag_volume', 'setdefaultvolume',
'rename_files', 'replace_spaces', 'zero_level', 'sab_remove_completed', 'sab_remove_failed',
'lowercase_filenames', 'autowant_upcoming', 'autowant_all', 'comic_cover_local', 'cover_folder_local', 'series_metadata_local', 'alternate_latest_series_covers', 'cvinfo', 'snatchedtorrent_notify',
'prowl_enabled', 'prowl_onsnatch', 'pushover_enabled', 'pushover_onsnatch', 'pushover_image', 'mattermost_enabled', 'mattermost_onsnatch', 'boxcar_enabled',
'boxcar_onsnatch', 'pushbullet_enabled', 'pushbullet_onsnatch', 'telegram_enabled', 'telegram_onsnatch', 'telegram_image', 'discord_enabled', 'discord_onsnatch', 'slack_enabled', 'slack_onsnatch',
'email_enabled', 'email_enc', 'email_ongrab', 'email_onpost', 'gotify_enabled', 'gotify_server_url', 'gotify_token', 'gotify_onsnatch', 'opds_enable', 'opds_authentication', 'opds_metainfo', 'opds_pagesize', 'enable_ddl',
'enable_getcomics', 'enable_external_server', 'ddl_prefer_upscaled', 'deluge_pause']"#;

    /// A full, non-minimal settings table: every checkbox, two newznabs, one
    /// torznab, a library and download folders.
    fn table(overrides: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut rows: Vec<(String, String)> = FORM_CHECKBOXES
            .iter()
            .map(|k| (k.to_string(), "False".to_string()))
            .collect();
        for (k, v) in [
            ("minimal_ini", "False"),
            ("usenet_retention", "3500"),
            ("newznab", "True"),
            ("post_processing", "True"),
            ("api_enabled", "True"),
            ("gotify_server_url", "https://gotify.example"),
            ("gotify_token", "GTOKEN"),
            ("opds_pagesize", "30"),
            ("nzb_downloader", "0"),
            ("sab_host", "http://10.0.0.15:8080"),
            ("sab_apikey", "SABKEY"),
            ("sab_directory", "/downloads/complete"),
            ("check_folder", "None"),
            ("destination_dir", "/comics"),
            ("comicvine_api", "CVKEY"),
            (
                "extra_newznabs",
                "NZBGeek, https://api.nzbgeek.info, 0, KEY1, , 1, 1, DOGnzb, https://api.dognzb.cr, 1, KEY2, 7030#7020, 0, 4",
            ),
            ("extra_torznabs", "Jackett, http://10.0.0.16:9117/api, 0, TKEY, 7030#8000, 1, 6"),
        ] {
            set(&mut rows, k, v);
        }
        for (k, v) in overrides {
            set(&mut rows, k, v);
        }
        rows
    }

    fn set(rows: &mut Vec<(String, String)>, k: &str, v: &str) {
        match rows.iter_mut().find(|(key, _)| key == k) {
            Some(row) => row.1 = v.to_string(),
            None => rows.push((k.to_string(), v.to_string())),
        }
    }

    fn unset(rows: &mut Vec<(String, String)>, k: &str) {
        rows.retain(|(key, _)| key != k);
    }

    fn ini(rows: &[(String, String)]) -> ConfigIni {
        ConfigIni(rows.iter().cloned().collect())
    }

    fn json_rows(rows: &[(String, String)]) -> Value {
        json!({ "aaData": rows.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>() })
    }

    /// Serve `reads` as successive `/getConfig` replies; the last repeats.
    async fn server(reads: &[Vec<(String, String)>], update: ResponseTemplate) -> MockServer {
        let server = MockServer::start().await;
        for (i, rows) in reads.iter().enumerate() {
            let mock = Mock::given(method("GET"))
                .and(path("/getConfig"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json_rows(rows)));
            if i + 1 < reads.len() {
                mock.up_to_n_times(1).mount(&server).await;
            } else {
                mock.mount(&server).await;
            }
        }
        Mock::given(method("POST"))
            .and(path("/configUpdate"))
            .respond_with(update)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("cmd", "getHistory"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
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
        server
    }

    fn ok() -> ResponseTemplate {
        ResponseTemplate::new(200)
    }

    fn mylar(server: &MockServer) -> Mylar {
        Mylar::new(&server.uri(), "KEY", None)
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

    fn change(key: &str, target: &str) -> SettingChange {
        SettingChange {
            key: key.into(),
            current: None,
            target: target.into(),
            reason: String::new(),
        }
    }

    #[test]
    fn checkbox_list_matches_upstream_v083() {
        let mut upstream: Vec<&str> = UPSTREAM_V083.split('\'').skip(1).step_by(2).collect();
        assert_eq!(upstream.len(), 91);
        let mut ours = FORM_CHECKBOXES.to_vec();
        upstream.sort_unstable();
        ours.sort_unstable();
        assert_eq!(ours, upstream);
    }

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
        assert!(err.contains("[usenet_retention] still drift"), "{err}");
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

    #[test]
    fn form_refuses_version_skew() {
        let c = [change("usenet_retention", "6000")];
        let mut rows = table(&[]);
        unset(&mut rows, "deluge_pause");
        let err = form(&ini(&rows), &c).unwrap_err().to_string();
        assert!(err.contains("deluge_pause"), "{err}");

        let rows = table(&[("jd2_enable", "False"), ("nzbsu_apikey", "x")]);
        let err = form(&ini(&rows), &c).unwrap_err().to_string();
        assert!(err.contains("[jd2_enable, nzbsu_apikey]"), "{err}");
        assert!(FORM_CHECKBOXES
            .iter()
            .all(|k| CONFIG_KEYS.binary_search(k).is_ok()));

        let mut rows = table(&[]);
        unset(&mut rows, "minimal_ini");
        assert!(form(&ini(&rows), &c)
            .unwrap_err()
            .to_string()
            .contains("minimal_ini"));
        let rows = table(&[("minimal_ini", "True")]);
        assert!(form(&ini(&rows), &c)
            .unwrap_err()
            .to_string()
            .contains("minimal_ini"));
    }

    #[test]
    fn form_refuses_providers_it_cannot_repost() {
        let c = [change("usenet_retention", "6000")];
        let refuse = |list: &str, v: &str| {
            form(&ini(&table(&[(list, v)])), &c)
                .unwrap_err()
                .to_string()
        };
        assert!(refuse("extra_newznabs", "a, b, c").contains("7-field"));
        let err = refuse("extra_torznabs", "a, b, 0, k, 8000, 1, SECRETID");
        assert!(
            err.contains("non-numeric id") && !err.contains("SECRETID"),
            "{err}"
        );
        let err = refuse(
            "extra_newznabs",
            "a, http://a, 0, k, , 1, 3, b, http://b, 0, k, , 1, 3",
        );
        assert!(err.contains("two provider rows with id 3"), "{err}");
        assert!(refuse("extra_newznabs", ", , 0, k, , 1, 3").contains("no name"));
        assert!(refuse("extra_newznabs", ", http://a, 0, k, , 1, 3").contains("no name"));
        let err = refuse(
            "extra_newznabs",
            "a, http://a, 0, ^~$z$S0VZMXNhbHRzYWx0, , 1, 3",
        );
        assert!(err.contains("encrypted"), "{err}");
    }

    #[test]
    fn diff_redacts_everything_but_checkboxes_and_planned_keys() {
        let planned = [change("usenet_retention", "6000")];
        let before = ini(&table(&[]));
        let after = ini(&table(&[
            ("usenet_retention", "6000"),
            ("enable_rss", "True"),
            ("sab_apikey", "NEWKEY"),
            ("comicvine_api", "CV2"),
            ("gotify_server_url", "https://other"),
            ("extra_newznabs", "x, http://x, 0, K9, , 1, 1"),
            ("sab_host", "http://10.0.0.99:8080"),
        ]));
        let d = diff(&before, &after, &planned).join("\n");
        assert!(d.contains("enable_rss: False -> True"), "{d}");
        assert!(!d.contains("usenet_retention"), "{d}");
        for key in [
            "sab_apikey",
            "comicvine_api",
            "gotify_server_url",
            "extra_newznabs",
            "sab_host",
        ] {
            assert!(
                d.contains(&format!("{key}: <redacted> -> <redacted>")),
                "{key}: {d}"
            );
        }
        for leak in [
            "SABKEY",
            "NEWKEY",
            "CVKEY",
            "CV2",
            "gotify.example",
            "other",
            "K9",
            "KEY1",
        ] {
            assert!(!d.contains(leak), "{leak}: {d}");
        }
        assert!(shown("usenet_retention", &planned));
    }

    #[test]
    fn re_encryption_is_not_a_change() {
        let before = ini(&table(&[("sab_apikey", "^~$z$S0VZMXNhbHRzYWx0")]));
        let resalted = ini(&table(&[("sab_apikey", "^~$z$S0VZMXBlcHBlcjEy")]));
        assert!(diff(&before, &resalted, &[]).is_empty());
        // Non-UTF-8 plaintexts that a lossy decode would render identically.
        let a = ini(&table(&[("sab_apikey", "^~$z$/3NhbHRzYWx0")]));
        let b = ini(&table(&[("sab_apikey", "^~$z$/nNhbHRzYWx0")]));
        assert_eq!(diff(&a, &b, &[]).len(), 1);
        let rekeyed = ini(&table(&[("sab_apikey", "^~$z$S0VZMnNhbHRzYWx0")]));
        assert_eq!(
            diff(&before, &rekeyed, &[]),
            vec!["sab_apikey: <redacted> -> <redacted>".to_string()]
        );
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

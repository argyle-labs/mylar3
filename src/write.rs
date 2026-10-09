//! The settings write. Mylar's only settings writer is the all-or-nothing
//! `/configUpdate` form, so every write re-posts the whole form: the planned
//! key changes, every checkbox at its current value, and every indexer row.
//! [`apply`] re-reads before posting, posts, reads back, and fails unless
//! exactly the planned values landed.

use std::collections::BTreeSet;

use plugin_toolkit::prelude::*;
use plugin_toolkit::scrub;

use crate::api::{ConfigIni, Mylar};
use crate::definitions::{BOOL_KEYS, CONFIG_KEYS, INT_KEYS};

/// `checked_configs` in Mylar's `configUpdate` (`mylar/webserve.py`): the form's
/// checkboxes, which it sets False whenever they are not posted.
pub(crate) const FORM_CHECKBOXES: &[&str] = &[
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

pub fn is_secret(key: &str) -> bool {
    scrub::is_sensitive_key(key) || MYLAR_SECRETS.contains(&key)
}

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

impl SettingChange {
    /// For output: a secret key's values replaced with the redaction marker.
    pub fn redacted(&self) -> SettingChange {
        if !is_secret(&self.key) {
            return self.clone();
        }
        SettingChange {
            key: self.key.clone(),
            current: self.current.as_ref().map(|_| scrub::REDACTED.to_string()),
            target: scrub::REDACTED.to_string(),
            reason: self.reason.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Newznab,
    Torznab,
}

impl ProviderKind {
    const ALL: [ProviderKind; 2] = [ProviderKind::Newznab, ProviderKind::Torznab];

    fn ini_key(self) -> &'static str {
        match self {
            ProviderKind::Newznab => "extra_newznabs",
            ProviderKind::Torznab => "extra_torznabs",
        }
    }

    /// Form field prefix; `configUpdate` reads `<prefix>_<column><suffix>`.
    fn prefix(self) -> &'static str {
        match self {
            ProviderKind::Newznab => "newznab",
            ProviderKind::Torznab => "torznab",
        }
    }

    /// The fifth column: newznab's uid, torznab's categories.
    fn extra_column(self) -> &'static str {
        match self {
            ProviderKind::Newznab => "uid",
            ProviderKind::Torznab => "category",
        }
    }
}

/// One indexer row, in Mylar's stored tuple order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRow {
    pub name: String,
    pub host: String,
    /// `0`/`1`, as posted.
    pub verify: String,
    pub apikey: String,
    /// Newznab uid or torznab categories, `#`-separated as stored.
    pub extra: String,
    /// `0`/`1`, as posted.
    pub enabled: String,
    /// `None` for a new row; Mylar numbers it on save.
    pub id: Option<u32>,
}

impl ProviderRow {
    fn columns(&self) -> [&str; 6] {
        [
            &self.name,
            &self.host,
            &self.verify,
            &self.apikey,
            &self.extra,
            &self.enabled,
        ]
    }

    /// As `configUpdate` stores it: spaces trimmed off the host, commas in the
    /// uid/categories turned into `#`.
    fn stored(&self) -> ProviderRow {
        ProviderRow {
            host: self.host.trim_matches(' ').to_string(),
            extra: self.extra.replace(',', "#"),
            ..self.clone()
        }
    }
}

/// Both indexer lists, as Mylar holds them or as a write will post them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Providers {
    pub newznab: Vec<ProviderRow>,
    pub torznab: Vec<ProviderRow>,
}

impl Providers {
    /// Parse `extra_newznabs`/`extra_torznabs`, refusing what could not be
    /// re-posted unchanged. Errors never echo a field: a misparsed row can shift
    /// an API key into any column.
    pub fn parse(ini: &ConfigIni) -> Result<Self> {
        let mut out = Providers::default();
        for kind in ProviderKind::ALL {
            let ini_key = kind.ini_key();
            let Some(raw) = ini.get(ini_key) else {
                continue;
            };
            let parts: Vec<&str> = raw.split(", ").collect();
            if !parts.len().is_multiple_of(7) {
                bail!(
                    "{ini_key} does not split into 7-field provider rows; refusing to re-post it"
                );
            }
            let mut ids = BTreeSet::new();
            for row in parts.chunks(7) {
                let id: u32 = match row[6].parse() {
                    Ok(id) if row[6].bytes().all(|b| b.is_ascii_digit()) => id,
                    _ => bail!(
                        "{ini_key} has a provider row with a non-numeric id; refusing to re-post it"
                    ),
                };
                if !ids.insert(id) {
                    bail!("{ini_key} has two provider rows with id {id}; refusing to re-post it");
                }
                // configUpdate renames a nameless row after its host, or drops it.
                if row[0].is_empty() {
                    bail!(
                        "{ini_key} provider {id} has no name; re-posting would rename or delete it"
                    );
                }
                out.list_mut(kind).push(ProviderRow {
                    name: row[0].to_string(),
                    host: row[1].to_string(),
                    verify: row[2].to_string(),
                    apikey: row[3].to_string(),
                    extra: row[4].to_string(),
                    enabled: row[5].to_string(),
                    id: Some(id),
                });
            }
        }
        Ok(out)
    }

    pub fn list(&self, kind: ProviderKind) -> &[ProviderRow] {
        match kind {
            ProviderKind::Newznab => &self.newznab,
            ProviderKind::Torznab => &self.torznab,
        }
    }

    pub fn list_mut(&mut self, kind: ProviderKind) -> &mut Vec<ProviderRow> {
        match kind {
            ProviderKind::Newznab => &mut self.newznab,
            ProviderKind::Torznab => &mut self.torznab,
        }
    }
}

/// What one settings write posts: key changes plus the full indexer lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Write {
    pub changes: Vec<SettingChange>,
    pub providers: Providers,
}

/// The `/configUpdate` body for `changes` and `providers`. Mylar sets every
/// [`FORM_CHECKBOXES`] key not posted to False and replaces both indexer lists
/// with the rows posted, while other keys keep their value unless posted. So
/// this posts the changes, every checkbox at its current value, and every row;
/// it refuses when the current state cannot be read exactly or a value would
/// not store as posted.
pub fn form(
    ini: &ConfigIni,
    changes: &[SettingChange],
    providers: &Providers,
) -> Result<Vec<(String, String)>> {
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
    for c in changes {
        check_target(c)?;
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
    for kind in ProviderKind::ALL {
        let mut ids = BTreeSet::new();
        let mut new_rows = 0;
        for row in providers.list(kind) {
            let suffix = match row.id {
                Some(id) => {
                    if !ids.insert(id) {
                        bail!("two {} rows with id {id}", kind.prefix());
                    }
                    id.to_string()
                }
                // A suffix starting `_` makes configUpdate number the row
                // from its own counter.
                None => {
                    new_rows += 1;
                    format!("_{new_rows}")
                }
            };
            if row.name.is_empty() {
                bail!(
                    "a {} row has no name; configUpdate would rename or drop it",
                    kind.prefix()
                );
            }
            let columns = [
                "name",
                "host",
                "verify",
                "apikey",
                kind.extra_column(),
                "enabled",
            ];
            for (column, value) in columns.iter().zip(row.columns()) {
                // `, ` is the stored list's separator.
                if value.contains(", ") {
                    bail!(
                        "{} row '{}': {column} contains \", \", which would corrupt {}",
                        kind.prefix(),
                        row.name,
                        kind.ini_key()
                    );
                }
                fields.push((
                    format!("{}_{column}{suffix}", kind.prefix()),
                    value.to_string(),
                ));
            }
        }
    }
    if fields.iter().any(|(_, v)| v.starts_with(ENCRYPTED_PREFIX)) {
        bail!("a posted setting holds an encrypted value, which Mylar would encrypt again");
    }
    Ok(fields)
}

/// Refuse a change `process_kwargs` would not store as given.
fn check_target(c: &SettingChange) -> Result<()> {
    let key = c.key.as_str();
    if ProviderKind::ALL.iter().any(|k| k.ini_key() == key) {
        bail!("{key} is written through the indexer rows, not as a setting");
    }
    if CONFIG_KEYS.binary_search(&key).is_err() {
        bail!("{key} is not a mylar3 v0.8.3 setting");
    }
    let t = c.target.as_str();
    // process_kwargs swaps an empty or `None` value for the default.
    if t.is_empty() || t == "None" {
        bail!("{key}: clearing a setting is not supported");
    }
    if BOOL_KEYS.binary_search(&key).is_ok() && bool_word(t).is_none() {
        bail!("{key} is a boolean; give true/false, 1/0 or on/off");
    }
    if INT_KEYS.binary_search(&key).is_ok() && !t.bytes().all(|b| b.is_ascii_digit()) {
        bail!("{key} is a whole number");
    }
    Ok(())
}

/// `argToBool`, rendered as `str(bool)`.
fn bool_word(v: &str) -> Option<&'static str> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "on" | "true" => Some("True"),
        "0" | "off" | "false" => Some("False"),
        _ => None,
    }
}

/// `key`'s value as Mylar uses it: `process_kwargs`' type coercion, then the
/// rewrites `config.configure()` applies on every save. Most rewrites stay in
/// memory while `config.ini` (and `/getConfig`) keep the posted text, so saved
/// and target values compare through this on both sides.
pub fn stored_form(key: &str, target: &str) -> String {
    let mut v = target.to_string();
    if BOOL_KEYS.binary_search(&key).is_ok() {
        if let Some(word) = bool_word(&v) {
            v = word.to_string();
        }
    } else if INT_KEYS.binary_search(&key).is_ok()
        && !v.is_empty()
        && v.bytes().all(|b| b.is_ascii_digit())
    {
        let digits = v.trim_start_matches('0');
        v = if digits.is_empty() {
            "0".into()
        } else {
            digits.to_string()
        };
    }
    match key {
        "sab_host" => {
            if !v.starts_with("http://") && !v.starts_with("https://") {
                v = format!("http://{v}");
            }
            if v.ends_with('/') {
                v.pop();
            }
        }
        "sab_priority" if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => {
            v = match v.as_str() {
                "1" => "Low",
                "2" => "Normal",
                "3" => "High",
                "4" => "Paused",
                _ => "Default",
            }
            .to_string();
        }
        "torrent_downloader" if !matches!(v.as_str(), "0" | "1" | "2" | "3" | "4" | "5") => {
            v = "0".into();
        }
        "gotify_server_url" if !v.ends_with('/') => v.push('/'),
        _ => {}
    }
    v
}

/// Whether `ini` already holds a value for `key` equivalent to `target`.
pub fn is_stored(ini: &ConfigIni, key: &str, target: &str) -> bool {
    ini.0
        .get(key)
        .is_some_and(|v| canonical(key, v) == canonical(key, target))
}

fn canonical(key: &str, v: &str) -> Vec<u8> {
    normalized(&stored_form(key, v)).into_owned()
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
pub(crate) fn shown(key: &str, planned: &[&str]) -> bool {
    let visible =
        (FORM_CHECKBOXES.contains(&key) && !key.starts_with("gotify_")) || planned.contains(&key);
    visible && !is_secret(key)
}

/// Keys whose value moved between `before` and `after`, other than `planned`.
fn changed<'a>(before: &'a ConfigIni, after: &'a ConfigIni, planned: &[&str]) -> Vec<&'a str> {
    let keys: BTreeSet<&String> = before.0.keys().chain(after.0.keys()).collect();
    keys.into_iter()
        .map(String::as_str)
        .filter(|k| !planned.contains(k))
        .filter(|k| {
            before.0.get(*k).map(|v| canonical(k, v)) != after.0.get(*k).map(|v| canonical(k, v))
        })
        .collect()
}

/// `key: before -> after` for each [`changed`] key, values outside [`shown`]
/// redacted.
pub(crate) fn diff(before: &ConfigIni, after: &ConfigIni, planned: &[&str]) -> Vec<String> {
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

/// Planned keys whose saved value is not equivalent to the target.
fn unlanded(after: &ConfigIni, changes: &[SettingChange]) -> Vec<String> {
    changes
        .iter()
        .filter_map(|c| {
            if is_stored(after, &c.key, &c.target) {
                return None;
            }
            let want = stored_form(&c.key, &c.target);
            let got = after.0.get(&c.key);
            Some(if is_secret(&c.key) {
                format!("{} (value withheld)", c.key)
            } else {
                format!(
                    "{}: wanted {want}, Mylar stored {}",
                    c.key,
                    got.map_or("nothing", String::as_str)
                )
            })
        })
        .collect()
}

/// Indexer rows in `after` that differ from `posted`: rows with an id must
/// come back unchanged under it; a new row must come back exactly once, under
/// an id no posted row holds, matched by name.
fn rows_unlanded(posted: &Providers, after: &Providers) -> Vec<String> {
    let mut out = Vec::new();
    for kind in ProviderKind::ALL {
        let prefix = kind.prefix();
        let got = after.list(kind);
        let kept: BTreeSet<u32> = posted.list(kind).iter().filter_map(|r| r.id).collect();
        let mut matched = BTreeSet::new();
        for row in posted.list(kind) {
            let want = row.stored();
            let found: Vec<&ProviderRow> = match row.id {
                Some(id) => got.iter().filter(|g| g.id == Some(id)).collect(),
                None => got
                    .iter()
                    .filter(|g| g.name == row.name && g.id.is_some_and(|id| !kept.contains(&id)))
                    .collect(),
            };
            matched.extend(found.iter().filter_map(|g| g.id));
            match found.as_slice() {
                [g] if ProviderRow {
                    id: g.id,
                    ..want.clone()
                } == **g => {}
                [] => out.push(format!("{prefix} row '{}' is missing", row.name)),
                [_] => out.push(format!(
                    "{prefix} row '{}' was stored differently",
                    row.name
                )),
                _ => out.push(format!(
                    "{prefix} row '{}' came back more than once",
                    row.name
                )),
            }
        }
        for g in got {
            if g.id.is_some_and(|id| !matched.contains(&id)) {
                out.push(format!("{prefix} row '{}' appeared unplanned", g.name));
            }
        }
    }
    out
}

/// Post `write`, planned from `before`, and confirm it. Refuses if the settings
/// moved since `before` was read. Fails if the form is rejected, a planned
/// value did not land, or anything else moved; the error lists what, with
/// secret values withheld. Confirmation reads `/getConfig`, which serves
/// Mylar's in-memory settings, so it does not prove the `config.ini` write
/// reached disk.
pub async fn apply(m: &Mylar, before: &ConfigIni, write: &Write) -> Result<()> {
    let fields = form(before, &write.changes, &write.providers)?;
    let fresh = m
        .config()
        .await
        .context("re-read Mylar's settings before writing")?;
    let moved = diff(before, &fresh, &[]);
    if !moved.is_empty() {
        bail!(
            "Mylar's settings changed since the plan was read: [{}]; nothing written, re-run",
            moved.join(", ")
        );
    }
    if let Err(e) = m.config_update(fields).await {
        match m.config().await {
            Ok(after) => {
                let moved = diff(before, &after, &[]);
                bail!(
                    "{e:#}; settings now differ from before by: [{}]",
                    moved.join(", ")
                );
            }
            Err(re) => bail!("{e:#}; re-reading settings to see what changed failed: {re:#}"),
        }
    }
    let after = m
        .config()
        .await
        .context("settings were submitted; re-read to verify failed")?;
    let missed = unlanded(&after, &write.changes);
    let rows = match Providers::parse(&after) {
        Ok(got) => rows_unlanded(&write.providers, &got),
        Err(e) => vec![format!("indexer rows unreadable after the write: {e}")],
    };
    let planned: Vec<&str> = write
        .changes
        .iter()
        .map(|c| c.key.as_str())
        .chain(ProviderKind::ALL.iter().map(|k| k.ini_key()))
        .collect();
    let side_effects = diff(before, &after, &planned);
    if missed.is_empty() && rows.is_empty() && side_effects.is_empty() {
        return Ok(());
    }
    let prior: Vec<String> = changed(before, &after, &planned)
        .into_iter()
        .filter(|k| shown(k, &planned))
        .map(|k| format!("{k}={}", before.0.get(k).map_or("unset", String::as_str)))
        .collect();
    let mut parts = Vec::new();
    for (label, items) in [
        ("planned values did not land", &missed),
        ("indexer rows did not land", &rows),
        ("other settings moved", &side_effects),
        ("prior values to restore by hand", &prior),
    ] {
        if !items.is_empty() {
            parts.push(format!("{label}: [{}]", items.join(", ")));
        }
    }
    bail!("Mylar accepted the settings form, but {}", parts.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::*;

    /// `form` against the rows the settings already hold.
    fn form_of(ini: &ConfigIni, changes: &[SettingChange]) -> Result<Vec<(String, String)>> {
        form(ini, changes, &Providers::parse(ini)?)
    }

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

    #[test]
    fn checkbox_list_matches_upstream_v083() {
        let mut upstream: Vec<&str> = UPSTREAM_V083.split('\'').skip(1).step_by(2).collect();
        assert_eq!(upstream.len(), 91);
        let mut ours = FORM_CHECKBOXES.to_vec();
        upstream.sort_unstable();
        ours.sort_unstable();
        assert_eq!(ours, upstream);
    }

    #[test]
    fn form_refuses_version_skew() {
        let c = [change("usenet_retention", "6000")];
        let mut rows = table(&[]);
        unset(&mut rows, "deluge_pause");
        let err = form_of(&ini(&rows), &c).unwrap_err().to_string();
        assert!(err.contains("deluge_pause"), "{err}");

        let rows = table(&[("jd2_enable", "False"), ("nzbsu_apikey", "x")]);
        let err = form_of(&ini(&rows), &c).unwrap_err().to_string();
        assert!(err.contains("[jd2_enable, nzbsu_apikey]"), "{err}");
        assert!(FORM_CHECKBOXES
            .iter()
            .all(|k| CONFIG_KEYS.binary_search(k).is_ok()));

        let mut rows = table(&[]);
        unset(&mut rows, "minimal_ini");
        assert!(form_of(&ini(&rows), &c)
            .unwrap_err()
            .to_string()
            .contains("minimal_ini"));
        let rows = table(&[("minimal_ini", "True")]);
        assert!(form_of(&ini(&rows), &c)
            .unwrap_err()
            .to_string()
            .contains("minimal_ini"));
    }

    #[test]
    fn form_refuses_providers_it_cannot_repost() {
        let c = [change("usenet_retention", "6000")];
        let refuse = |list: &str, v: &str| {
            form_of(&ini(&table(&[(list, v)])), &c)
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
        let planned = ["usenet_retention"];
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

    fn row(name: &str, apikey: &str, id: Option<u32>) -> ProviderRow {
        ProviderRow {
            name: name.into(),
            host: format!("https://{}.example", name.to_lowercase()),
            verify: "1".into(),
            apikey: apikey.into(),
            extra: "7030".into(),
            enabled: "1".into(),
            id,
        }
    }

    #[test]
    fn stored_form_applies_mylars_coercions_and_rewrites() {
        assert_eq!(
            stored_form("sab_host", "10.0.0.5:8080/"),
            "http://10.0.0.5:8080"
        );
        assert_eq!(
            stored_form("sab_host", "https://sab.example/"),
            "https://sab.example"
        );
        assert_eq!(
            stored_form("sab_host", "http://sab.example"),
            "http://sab.example"
        );
        assert_eq!(stored_form("sab_to_mylar", "1"), "True");
        assert_eq!(stored_form("enable_torrents", "off"), "False");
        assert_eq!(stored_form("usenet_retention", "06000"), "6000");
        assert_eq!(stored_form("usenet_retention", "0"), "0");
        assert_eq!(stored_form("sab_priority", "3"), "High");
        assert_eq!(stored_form("sab_priority", "9"), "Default");
        assert_eq!(stored_form("sab_priority", "Low"), "Low");
        assert_eq!(stored_form("torrent_downloader", "5"), "5");
        assert_eq!(stored_form("torrent_downloader", "6"), "0");
        assert_eq!(stored_form("gotify_server_url", "https://g"), "https://g/");
        assert_eq!(stored_form("nzbget_port", "06789"), "06789");
        assert_eq!(
            stored_form("qbittorrent_host", "http://q:8080/"),
            "http://q:8080/"
        );
    }

    #[test]
    fn secret_changes_are_redacted_for_output() {
        let secret = SettingChange {
            key: "sab_apikey".into(),
            current: Some("OLD".into()),
            target: "NEW".into(),
            reason: "r".into(),
        };
        let shown = secret.redacted();
        assert_eq!(shown.current.as_deref(), Some(scrub::REDACTED));
        assert_eq!(shown.target, scrub::REDACTED);
        assert_eq!(shown.reason, "r");
        let unset = SettingChange {
            current: None,
            ..secret.clone()
        }
        .redacted();
        assert_eq!(unset.current, None);
        for key in ["nzbget_password", "seedbox_pass", "qbittorrent_password"] {
            let c = SettingChange {
                key: key.into(),
                ..secret.clone()
            };
            assert_eq!(c.redacted().target, scrub::REDACTED, "{key}");
        }
        let plain = change("sab_host", "http://h");
        assert_eq!(plain.redacted(), plain);
    }

    #[test]
    fn form_refuses_targets_mylar_would_not_store() {
        let i = ini(&table(&[]));
        let p = Providers::parse(&i).unwrap();
        let refuse = |key: &str, target: &str| {
            form(&i, &[change(key, target)], &p)
                .unwrap_err()
                .to_string()
        };
        assert!(refuse("extra_newznabs", "x").contains("indexer rows"));
        assert!(refuse("nzbsu_apikey", "x").contains("not a mylar3 v0.8.3 setting"));
        assert!(refuse("sab_host", "").contains("clearing"));
        assert!(refuse("sab_host", "None").contains("clearing"));
        assert!(refuse("sab_to_mylar", "yes").contains("boolean"));
        assert!(refuse("torrent_downloader", "qbit").contains("whole number"));
        assert!(form(&i, &[change("sab_to_mylar", "on")], &p).is_ok());
    }

    #[test]
    fn form_posts_edited_and_new_rows() {
        let i = ini(&table(&[]));
        let mut p = Providers::parse(&i).unwrap();
        p.newznab[1].apikey = "NEWKEY".into();
        p.torznab.push(row("Prowlarr", "PKEY", None));
        p.torznab.push(row("Other", "OKEY", None));
        let fields = form(&i, &[], &p).unwrap();
        let rows: Vec<&(String, String)> = fields
            .iter()
            .filter(|(k, _)| k.starts_with("newznab_") || k.starts_with("torznab_"))
            .collect();
        let pairs = |prefix: &str, suffix: &str, vals: [&str; 6]| -> Vec<(String, String)> {
            let extra = if prefix == "newznab" {
                "uid"
            } else {
                "category"
            };
            ["name", "host", "verify", "apikey", extra, "enabled"]
                .iter()
                .zip(vals)
                .map(|(c, v)| (format!("{prefix}_{c}{suffix}"), v.to_string()))
                .collect()
        };
        let mut want = Vec::new();
        want.extend(pairs(
            "newznab",
            "1",
            ["NZBGeek", "https://api.nzbgeek.info", "0", "KEY1", "", "1"],
        ));
        want.extend(pairs(
            "newznab",
            "4",
            [
                "DOGnzb",
                "https://api.dognzb.cr",
                "1",
                "NEWKEY",
                "7030#7020",
                "0",
            ],
        ));
        want.extend(pairs(
            "torznab",
            "6",
            [
                "Jackett",
                "http://10.0.0.16:9117/api",
                "0",
                "TKEY",
                "7030#8000",
                "1",
            ],
        ));
        want.extend(pairs(
            "torznab",
            "_1",
            [
                "Prowlarr",
                "https://prowlarr.example",
                "1",
                "PKEY",
                "7030",
                "1",
            ],
        ));
        want.extend(pairs(
            "torznab",
            "_2",
            ["Other", "https://other.example", "1", "OKEY", "7030", "1"],
        ));
        assert_eq!(rows.into_iter().cloned().collect::<Vec<_>>(), want);
    }

    #[test]
    fn form_refuses_rows_that_would_not_store_as_posted() {
        let i = ini(&table(&[]));
        let base = Providers::parse(&i).unwrap();
        let refuse = |edit: &dyn Fn(&mut Providers)| {
            let mut p = base.clone();
            edit(&mut p);
            form(&i, &[], &p).unwrap_err().to_string()
        };
        let err = refuse(&|p| p.newznab[0].apikey = "SEC, RET".into());
        assert!(
            err.contains("apikey contains") && !err.contains("SEC"),
            "{err}"
        );
        assert!(refuse(&|p| p.torznab[0].extra = "7030, 8000".into()).contains("category contains"));
        assert!(refuse(&|p| p.newznab[1].id = Some(1)).contains("two newznab rows with id 1"));
        assert!(refuse(&|p| p.torznab.push(row("", "k", None))).contains("no name"));
        let err = refuse(&|p| p.torznab.push(row("Enc", "^~$z$S0VZMXNhbHRzYWx0", None)));
        assert!(err.contains("encrypted"), "{err}");
    }

    #[test]
    fn landed_values_compare_by_equivalence() {
        // `config.ini` keeps sab_host as posted; the rewrite is in memory only.
        let after = ini(&table(&[
            ("sab_host", "http://10.0.0.5:8080/"),
            ("sab_apikey", "^~$z$S0VZMXNhbHRzYWx0"),
            ("sab_to_mylar", "True"),
            ("sab_priority", "3"),
        ]));
        let ok = [
            change("sab_host", "http://10.0.0.5:8080/"),
            change("sab_apikey", "KEY1"),
            change("sab_to_mylar", "1"),
            change("sab_priority", "3"),
        ];
        assert!(unlanded(&after, &ok).is_empty());
        let equivalent = [
            change("sab_host", "10.0.0.5:8080"),
            change("sab_priority", "High"),
        ];
        assert!(unlanded(&after, &equivalent).is_empty());
        let bad = [change("sab_apikey", "OTHER"), change("sab_directory", "/x")];
        let missed = unlanded(&after, &bad);
        assert_eq!(
            missed,
            vec![
                "sab_apikey (value withheld)".to_string(),
                "sab_directory: wanted /x, Mylar stored /downloads/complete".to_string(),
            ]
        );
    }

    #[test]
    fn rows_land_by_id_or_by_name_under_a_fresh_id() {
        let before = Providers::parse(&ini(&table(&[]))).unwrap();
        let mut posted = before.clone();
        posted.torznab.push(row("Prowlarr", "PKEY", None));
        let mut got = before.clone();
        got.torznab.push(row("Prowlarr", "PKEY", Some(7)));
        assert!(rows_unlanded(&posted, &got).is_empty());

        // A new row "found" under an id a posted row keeps is not the new row.
        let mut reused = before.clone();
        reused.torznab[0] = row("Prowlarr", "PKEY", Some(6));
        let out = rows_unlanded(&posted, &reused);
        assert!(
            out.contains(&"torznab row 'Jackett' was stored differently".to_string()),
            "{out:?}"
        );
        assert!(
            out.contains(&"torznab row 'Prowlarr' is missing".to_string()),
            "{out:?}"
        );

        let mut changed = got.clone();
        changed.torznab[1].apikey = "LEAK".into();
        let out = rows_unlanded(&posted, &changed);
        assert_eq!(
            out,
            vec!["torznab row 'Prowlarr' was stored differently".to_string()]
        );

        let mut extra = got.clone();
        extra.newznab.push(row("Ghost", "G", Some(9)));
        assert_eq!(
            rows_unlanded(&posted, &extra),
            vec!["newznab row 'Ghost' appeared unplanned".to_string()]
        );

        let mut twice = got.clone();
        twice.torznab.push(row("Prowlarr", "PKEY", Some(8)));
        let out = rows_unlanded(&posted, &twice);
        assert!(
            out.contains(&"torznab row 'Prowlarr' came back more than once".to_string()),
            "{out:?}"
        );

        // Stored form: commas in categories come back as `#`, host spaces trimmed.
        let mut spaced = before.clone();
        let mut r = row("Spaced", "S", None);
        r.host = " https://s.example ".into();
        r.extra = "7030,8000".into();
        spaced.torznab.push(r);
        let mut stored = before.clone();
        let mut r = row("Spaced", "S", Some(7));
        r.host = "https://s.example".into();
        r.extra = "7030#8000".into();
        stored.torznab.push(r);
        assert!(rows_unlanded(&spaced, &stored).is_empty());
    }

    #[test]
    fn equivalent_rewrites_are_not_moves() {
        let before = ini(&table(&[("gotify_server_url", "https://g")]));
        let after = ini(&table(&[("gotify_server_url", "https://g/")]));
        assert!(diff(&before, &after, &[]).is_empty());
    }

    #[tokio::test]
    async fn apply_verifies_a_value_saved_as_posted() {
        let before = table(&[]);
        let after = table(&[("sab_host", "http://10.0.0.99:8080/")]);
        let server = server(&[before.clone(), after], ok()).await;
        let b = ini(&before);
        let w = Write {
            changes: vec![change("sab_host", "http://10.0.0.99:8080/")],
            providers: Providers::parse(&b).unwrap(),
        };
        apply(&mylar(&server), &b, &w).await.unwrap();
    }

    #[tokio::test]
    async fn apply_adds_a_row_and_confirms_it_by_name() {
        let before = table(&[]);
        let after = table(&[(
            "extra_torznabs",
            "Jackett, http://10.0.0.16:9117/api, 0, TKEY, 7030#8000, 1, 6, Prowlarr, https://prowlarr.example, 1, PKEY, 7030, 1, 7",
        )]);
        let server = server(&[before.clone(), after], ok()).await;
        let b = ini(&before);
        let mut providers = Providers::parse(&b).unwrap();
        providers.torznab.push(row("Prowlarr", "PKEY", None));
        let w = Write {
            changes: vec![],
            providers,
        };
        apply(&mylar(&server), &b, &w).await.unwrap();
        let form = posted_form(&server).await;
        assert!(form.contains(&("torznab_name_1".to_string(), "Prowlarr".to_string())));
        assert!(form.contains(&("torznab_name6".to_string(), "Jackett".to_string())));
    }

    #[tokio::test]
    async fn apply_fails_when_a_row_is_lost_and_withholds_its_key() {
        let before = table(&[]);
        let after = table(&[(
            "extra_newznabs",
            "NZBGeek, https://api.nzbgeek.info, 0, KEY1, , 1, 1",
        )]);
        let server = server(&[before.clone(), after], ok()).await;
        let b = ini(&before);
        let w = Write {
            changes: vec![],
            providers: Providers::parse(&b).unwrap(),
        };
        let err = apply(&mylar(&server), &b, &w)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("indexer rows did not land: [newznab row 'DOGnzb' is missing]"),
            "{err}"
        );
        assert!(
            !err.contains("KEY2") && !err.contains("other settings"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn apply_writes_a_secret_without_echoing_it() {
        let before = table(&[]);
        let server = server(std::slice::from_ref(&before), ok()).await;
        let b = ini(&before);
        let w = Write {
            changes: vec![change("sab_apikey", "NEWSECRET")],
            providers: Providers::parse(&b).unwrap(),
        };
        let err = apply(&mylar(&server), &b, &w)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("sab_apikey (value withheld)"), "{err}");
        assert!(
            !err.contains("NEWSECRET") && !err.contains("SABKEY"),
            "{err}"
        );
        let form = posted_form(&server).await;
        assert!(form.contains(&("sab_apikey".to_string(), "NEWSECRET".to_string())));
    }
}

//! A download client orca resolved (orca#796) mapped to the `config.ini` values
//! Mylar v0.11.0 reads. A direct ini write skips Mylar's save-time fixups, so
//! each value is the one Mylar itself would leave after a save. Indexers reach
//! Mylar through Prowlarr, not here.

use plugin_toolkit::prelude::*;
use plugin_toolkit::reqwest::Url;
use plugin_toolkit::scrub;

use crate::api::ConfigIni;
use crate::remediate::SettingChange;
use crate::{ini, secret};

/// The client category mylar3 files its downloads under, per media type.
pub fn category_map(media_type: &str) -> Option<&'static str> {
    (media_type == "comics").then_some("comics")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Usenet,
    Torrent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientProvider {
    Sabnzbd,
    Nzbget,
    Blackhole,
    Qbittorrent,
    Deluge,
    Transmission,
    Rtorrent,
    Utorrent,
    Watchdir,
}

impl ClientProvider {
    pub fn protocol(self) -> Protocol {
        match self {
            ClientProvider::Sabnzbd | ClientProvider::Nzbget | ClientProvider::Blackhole => {
                Protocol::Usenet
            }
            _ => Protocol::Torrent,
        }
    }

    /// The `config.ini` section holding this client's settings.
    fn section(self) -> &'static str {
        match self {
            ClientProvider::Sabnzbd => "SABnzbd",
            ClientProvider::Nzbget => "NZBGet",
            ClientProvider::Blackhole => "Blackhole",
            ClientProvider::Qbittorrent => "qBittorrent",
            ClientProvider::Deluge => "Deluge",
            ClientProvider::Transmission => "Transmission",
            ClientProvider::Rtorrent => "Rtorrent",
            ClientProvider::Utorrent => "uTorrent",
            ClientProvider::Watchdir => "Watchdir",
        }
    }

    /// The `[Client]` key and value selecting this client.
    fn selector(self) -> (&'static str, &'static str) {
        match self {
            ClientProvider::Sabnzbd => ("nzb_downloader", "0"),
            ClientProvider::Nzbget => ("nzb_downloader", "1"),
            ClientProvider::Blackhole => ("nzb_downloader", "2"),
            ClientProvider::Watchdir => ("torrent_downloader", "0"),
            ClientProvider::Utorrent => ("torrent_downloader", "1"),
            ClientProvider::Rtorrent => ("torrent_downloader", "2"),
            ClientProvider::Transmission => ("torrent_downloader", "3"),
            ClientProvider::Deluge => ("torrent_downloader", "4"),
            ClientProvider::Qbittorrent => ("torrent_downloader", "5"),
        }
    }
}

/// A download client as core hands it over: every reference resolved, secrets
/// in plain text. Blank fields count as absent, except a secret: an empty one
/// or one with leading or trailing whitespace is refused.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ResolvedDownloadClient {
    pub provider: Option<ClientProvider>,
    pub name: String,
    /// Base URL, e.g. `http://10.0.0.15:8080`.
    pub url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub api_key: Option<String>,
    /// Client category or label; defaults to [`category_map`]`("comics")`.
    pub category: Option<String>,
    /// A folder as Mylar's container sees it: where SABnzbd/NZBGet complete
    /// into (`sab_directory`, `nzbget_directory`), or the blackhole/watch
    /// folder (`blackhole_dir`, `local_watchdir`).
    pub directory: Option<String>,
    /// The folder a torrent client saves into, as the client sees it:
    /// `qbittorrent_folder`, `deluge_download_directory`,
    /// `transmission_directory`, `rtorrent_directory`.
    pub client_directory: Option<String>,
    pub priority: Option<String>,
}

impl std::fmt::Debug for ResolvedDownloadClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedDownloadClient")
            .field("provider", &self.provider)
            .field("name", &self.name)
            .field(
                "url",
                &self.url.as_deref().map(|u| {
                    if holds_credentials(u) {
                        scrub::REDACTED
                    } else {
                        u
                    }
                }),
            )
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| scrub::REDACTED))
            .field("api_key", &self.api_key.as_ref().map(|_| scrub::REDACTED))
            .field("category", &self.category)
            .field("directory", &self.directory)
            .field("client_directory", &self.client_directory)
            .field("priority", &self.priority)
            .finish()
    }
}

/// Client keys Mylar stores encrypted when `encrypt_passwords` is on
/// (`encrypt_items` in `mylar/config.py`).
const ENCRYPTED: &[&str] = &[
    "sab_password",
    "sab_apikey",
    "nzbget_password",
    "utorrent_password",
    "transmission_password",
    "deluge_password",
    "qbittorrent_password",
    "rtorrent_password",
];

pub fn is_secret(key: &str) -> bool {
    ENCRYPTED.contains(&key) || scrub::is_sensitive_key(key)
}

/// What configparser writes for a `str` setting whose default is `None`, and
/// what Mylar reads back as unset.
const UNSET: &str = "None";

/// Whether Mylar reads `v` as unset.
fn is_unset(v: &str) -> bool {
    matches!(v.trim(), "" | UNSET)
}

/// `value` for output: withheld for a secret key unless it is unset, and for an
/// `nzbget_sub` that may hold NZBGet's `user:pass` segment.
fn shown<'a>(key: &str, value: &'a str) -> &'a str {
    if (is_secret(key) && !is_unset(value)) || (key == "nzbget_sub" && path_holds_colon(value)) {
        scrub::REDACTED
    } else {
        value
    }
}

/// One `config.ini` value, as configparser reads it back.
#[derive(Clone, PartialEq, Eq)]
pub struct Setting {
    pub section: &'static str,
    pub key: &'static str,
    pub value: String,
}

impl Setting {
    pub fn edit(&self) -> ini::Edit {
        ini::Edit {
            section: self.section.to_string(),
            key: self.key.to_string(),
            value: self.value.clone(),
        }
    }
}

impl std::fmt::Debug for Setting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Setting")
            .field("section", &self.section)
            .field("key", &self.key)
            .field("value", &shown(self.key, &self.value))
            .finish()
    }
}

/// Every setting a client owns, absent fields written as unset, plus the
/// supplied fields mylar3 has no setting for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapping {
    pub settings: Vec<Setting>,
    pub ignored: Vec<String>,
}

/// Control characters break the ini line, and the URL parser drops some
/// silently, so the value checked would not be the value stored. Errors never
/// echo the value.
fn refuse_control(name: &str, v: &str) -> Result<()> {
    if v.chars().any(char::is_control) {
        bail!("{name} must not contain control characters");
    }
    Ok(())
}

fn refuse_unset(name: &str, v: &str) -> Result<()> {
    if v == UNSET {
        bail!("{name} must not be \"None\"; Mylar reads that as unset");
    }
    Ok(())
}

/// A supplied field, trimmed; `None` when absent or blank.
fn present<'a>(name: &str, v: &'a Option<String>) -> Result<Option<&'a str>> {
    if let Some(v) = v.as_deref() {
        refuse_control(name, v)?;
    }
    let Some(v) = v.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    refuse_unset(name, v)?;
    Ok(Some(v))
}

/// A supplied secret, untrimmed: an empty one would clear the stored secret
/// and edge whitespace is most likely a paste error.
fn present_secret<'a>(name: &str, v: &'a Option<String>) -> Result<Option<&'a str>> {
    let Some(v) = v.as_deref() else {
        return Ok(None);
    };
    if v.is_empty() {
        bail!("{name} is empty; leave it out to store none");
    }
    refuse_control(name, v)?;
    if v.trim() != v {
        bail!("{name} must not have leading or trailing whitespace");
    }
    refuse_unset(name, v)?;
    Ok(Some(v))
}

/// The words Mylar's senders map (`search.py`, `nzbget.py`); any other value
/// sends no priority or fails the send.
const SAB_PRIORITIES: &[&str] = &["Default", "Low", "Normal", "High", "Paused"];
const NZBGET_PRIORITIES: &[&str] = &[
    "Default",
    "Normal",
    "Low",
    "High",
    "Very High",
    "Force",
    "Paused",
];

/// The priority word to store. Matching ignores case; for SABnzbd a digit is
/// taken as `configure` reads one, 0-4 in [`SAB_PRIORITIES`] order, since it
/// only becomes a word in Mylar's memory.
fn priority(
    value: Option<&str>,
    words: &[&'static str],
    digits: bool,
) -> Result<Option<&'static str>> {
    let Some(p) = value else {
        return Ok(None);
    };
    let word = match p.parse::<usize>() {
        Ok(i) if digits => words.get(i),
        _ => words.iter().find(|w| w.eq_ignore_ascii_case(p)),
    };
    match word {
        Some(w) => Ok(Some(w)),
        None => bail!("priority must be one of {}", words.join(", ")),
    }
}

/// Credentials belong in the username/password settings; in the url they
/// would be stored under a key no redaction covers.
fn refuse_userinfo(url: &str) -> Result<()> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        bail!("url must not carry credentials (user:pass@); give username and password");
    }
    Ok(())
}

/// The path of `url`, scheme optional, without its query or fragment.
fn raw_path(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    rest.find('/').map_or("", |i| &rest[i..])
}

/// NZBGet takes credentials as a `/user:pass/` path segment, so a `:` in a
/// path is treated as one.
fn path_holds_colon(path: &str) -> bool {
    path.contains(':') || path.to_ascii_lowercase().contains("%3a")
}

fn holds_credentials(url: &str) -> bool {
    url.contains('@') || path_holds_colon(raw_path(url))
}

fn http_url(url: &str) -> Result<Url> {
    let u = Url::parse(url).map_err(|_| anyhow!("url is not a valid URL"))?;
    if !matches!(u.scheme(), "http" | "https") {
        bail!("url must be http:// or https://");
    }
    if u.host_str().is_none() {
        bail!("url has no host");
    }
    Ok(u)
}

/// `sab_host` as Mylar's `configure` leaves it: `http://` added to a bare
/// `host:port`, no trailing `/`. Mylar appends `/api?...`, so no query.
fn sab_host(url: &str) -> Result<String> {
    let full = if url.contains("://") {
        url.to_string()
    } else {
        format!("http://{url}")
    };
    let u = http_url(&full)?;
    if u.query().is_some() || u.fragment().is_some() {
        bail!("SABnzbd url must not carry a query or fragment");
    }
    Ok(u.as_str().trim_end_matches('/').to_string())
}

/// `(nzbget_host, nzbget_port, nzbget_sub)`: Mylar builds NZBGet's URL as
/// `host:port` + the sub path + `/xmlrpc`.
fn nzbget_url(url: &str) -> Result<(String, String, Option<String>)> {
    let u = http_url(url)?;
    if u.query().is_some() || u.fragment().is_some() {
        bail!("NZBGet url must not carry a query or fragment");
    }
    if path_holds_colon(u.path()) {
        bail!("NZBGet url path must not hold ':' (user:pass); give username and password");
    }
    let host = u.host_str().ok_or_else(|| anyhow!("url has no host"))?;
    let port = u
        .port_or_known_default()
        .ok_or_else(|| anyhow!("url has no port"))?;
    Ok((
        format!("{}://{host}", u.scheme()),
        port.to_string(),
        nzbget_sub(u.path()),
    ))
}

/// `nzbget_sub` as `nzbget.py` uses it, one leading `/` and no trailing one,
/// so `nzbget` and `/nzbget/` store alike. A final `xmlrpc` segment is dropped:
/// Mylar appends its own.
fn nzbget_sub(path: &str) -> Option<String> {
    let p = path.trim_matches('/');
    sub_form(match p.rsplit_once('/') {
        Some((head, "xmlrpc")) => head,
        None if p == "xmlrpc" => "",
        _ => p,
    })
}

fn sub_form(path: &str) -> Option<String> {
    let p = path.trim_matches('/');
    (!p.is_empty()).then(|| format!("/{p}"))
}

/// transmissionrpc uses a URL with a scheme verbatim, adding
/// `/transmission/rpc` only to a bare `host:port`.
fn transmission_url(url: &str) -> Result<String> {
    let u = http_url(url)?;
    if u.query().is_some() || u.fragment().is_some() {
        bail!("Transmission url must not carry a query or fragment");
    }
    Ok(if matches!(u.path(), "" | "/") {
        format!("{}/transmission/rpc", url.trim_end_matches('/'))
    } else {
        url.to_string()
    })
}

/// `(rtorrent_host, rtorrent_rpc_url)`: Mylar cleans the host to
/// `scheme://host:port/` and appends the rpc url. It prefixes `http://` to any
/// host not starting `https:`/`http://`, so other schemes cannot be stored.
fn rtorrent_url(url: &str) -> Result<(String, Option<String>)> {
    if !url.contains("://") {
        bail!("rTorrent url needs an http:// or https:// scheme");
    }
    let u = http_url(url)?;
    if u.query().is_some() || u.fragment().is_some() {
        bail!("rTorrent url must not carry a query or fragment");
    }
    let host = u.host_str().ok_or_else(|| anyhow!("url has no host"))?;
    let base = match u.port() {
        Some(port) => format!("{}://{host}:{port}", u.scheme()),
        None => format!("{}://{host}", u.scheme()),
    };
    let rpc = u.path().trim_start_matches('/');
    Ok((base, (!rpc.is_empty()).then(|| rpc.to_string())))
}

/// `utorrent_host` with its path kept as given: utorrent.py strips one `/`
/// and one `/gui` before appending `/gui/`, so rewriting the path could change
/// the URL Mylar builds. It prefixes `http://` only to a host not starting
/// `http`, so a bare host gets it here: `httpbox` would otherwise go unprefixed.
fn utorrent_host(url: &str) -> Result<String> {
    let (scheme, rest) = url.split_once("://").unwrap_or(("http", url));
    let u = http_url(&format!("{scheme}://{rest}"))?;
    if u.query().is_some() || u.fragment().is_some() {
        bail!("uTorrent url must not carry a query or fragment");
    }
    Ok(format!("{}://{rest}", u.scheme()))
}

/// Deluge's `host:port`: Mylar splits the setting on `:`, so no scheme and no
/// IPv6 literal.
fn deluge_host(url: &str) -> Result<String> {
    let with_scheme = if url.contains("://") {
        url.to_string()
    } else {
        format!("http://{url}")
    };
    let u = Url::parse(&with_scheme).map_err(|_| anyhow!("url is not host:port or a URL"))?;
    let host = u.host_str().ok_or_else(|| anyhow!("url has no host"))?;
    if host.starts_with('[') {
        bail!("Deluge's host cannot be an IPv6 literal; Mylar splits deluge_host on ':'");
    }
    if !matches!(u.path(), "" | "/") || u.query().is_some() || u.fragment().is_some() {
        bail!("Deluge url must be host:port, without a path, query or fragment");
    }
    // `Url` drops a scheme's default port, so read it from the text.
    let authority = with_scheme
        .split_once("://")
        .map_or("", |(_, rest)| rest)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let port = authority
        .rsplit_once(':')
        .map(|(_, port)| port)
        .filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|p| p.parse::<u16>().ok())
        .ok_or_else(|| anyhow!("url has no port"))?;
    Ok(format!("{host}:{port}"))
}

/// Mylar decrypts when `ENCRYPT_PASSWORDS is True`, which config load sets
/// with configparser's `getboolean`.
fn encrypts(ini: &ConfigIni) -> bool {
    ini.flag("encrypt_passwords") == Some(true)
}

fn encrypt(key: &str, value: &str) -> Result<String> {
    secret::encode(value).map_err(|e| anyhow!("encrypting {key}: {e}"))
}

/// The settings `client` maps to. `ini` says whether Mylar encrypts secrets.
pub fn map(ini: &ConfigIni, client: &ResolvedDownloadClient) -> Result<Mapping> {
    use ClientProvider::*;
    let provider = client
        .provider
        .ok_or_else(|| anyhow!("download client '{}' has no provider", client.name))?;
    let url = present("url", &client.url)?;
    let username = present("username", &client.username)?;
    let password = present_secret("password", &client.password)?;
    let api_key = present_secret("api_key", &client.api_key)?;
    let category_given = present("category", &client.category)?;
    let directory = present("directory", &client.directory)?;
    let client_directory = present("client_directory", &client.client_directory)?;
    let priority_given = present("priority", &client.priority)?;
    let need_url = || -> Result<&str> {
        let url = url.ok_or_else(|| anyhow!("{provider:?} needs a url"))?;
        refuse_userinfo(url)?;
        Ok(url)
    };
    let need_dir = || directory.ok_or_else(|| anyhow!("{provider:?} needs a directory"));
    let category = category_given.or(category_map("comics"));
    let section = provider.section();
    let mut settings = Vec::new();
    let mut used = vec!["provider", "name"];
    let mut put = |key: &'static str, value: Option<&str>, field: &'static str| {
        settings.push(Setting {
            section,
            key,
            value: value.unwrap_or(UNSET).to_string(),
        });
        used.push(field);
    };
    match provider {
        Sabnzbd => {
            put("sab_host", Some(&sab_host(need_url()?)?), "url");
            let key = api_key.ok_or_else(|| anyhow!("Sabnzbd needs an api_key"))?;
            put("sab_apikey", Some(key), "api_key");
            put("sab_username", username, "username");
            put("sab_password", password, "password");
            put("sab_category", category, "category");
            let p = priority(priority_given, SAB_PRIORITIES, true)?;
            put("sab_priority", Some(p.unwrap_or("Default")), "priority");
            put("sab_directory", directory, "directory");
            put(
                "sab_to_mylar",
                Some(if directory.is_some() { "True" } else { "False" }),
                "directory",
            );
        }
        Nzbget => {
            let (host, port, sub) = nzbget_url(need_url()?)?;
            put("nzbget_host", Some(&host), "url");
            put("nzbget_port", Some(&port), "url");
            put("nzbget_sub", sub.as_deref(), "url");
            put("nzbget_username", username, "username");
            put("nzbget_password", password, "password");
            put("nzbget_category", category, "category");
            put(
                "nzbget_priority",
                priority(priority_given, NZBGET_PRIORITIES, false)?,
                "priority",
            );
            put("nzbget_directory", directory, "directory");
        }
        Blackhole => put("blackhole_dir", Some(need_dir()?), "directory"),
        Qbittorrent => {
            let url = need_url()?;
            let u = http_url(url)?;
            if u.query().is_some() || u.fragment().is_some() {
                bail!("qBittorrent url must not carry a query or fragment");
            }
            put("qbittorrent_host", Some(url), "url");
            put("qbittorrent_username", username, "username");
            put("qbittorrent_password", password, "password");
            put("qbittorrent_label", category, "category");
            put("qbittorrent_folder", client_directory, "client_directory");
        }
        Deluge => {
            let host = deluge_host(need_url()?)?;
            // deluge.py refuses to connect without both.
            if username.is_none() || password.is_none() {
                bail!("Deluge needs a username and password");
            }
            put("deluge_host", Some(&host), "url");
            put("deluge_username", username, "username");
            put("deluge_password", password, "password");
            put("deluge_label", category, "category");
            // Its default is "", not None.
            put(
                "deluge_download_directory",
                Some(client_directory.unwrap_or("")),
                "client_directory",
            );
        }
        Transmission => {
            put(
                "transmission_host",
                Some(&transmission_url(need_url()?)?),
                "url",
            );
            put("transmission_username", username, "username");
            put("transmission_password", password, "password");
            put(
                "transmission_directory",
                client_directory,
                "client_directory",
            );
        }
        Rtorrent => {
            let (host, rpc) = rtorrent_url(need_url()?)?;
            put("rtorrent_host", Some(&host), "url");
            put("rtorrent_rpc_url", rpc.as_deref(), "url");
            put("rtorrent_username", username, "username");
            put("rtorrent_password", password, "password");
            put("rtorrent_label", category, "category");
            put("rtorrent_directory", client_directory, "client_directory");
        }
        Utorrent => {
            put("utorrent_host", Some(&utorrent_host(need_url()?)?), "url");
            put("utorrent_username", username, "username");
            put("utorrent_password", password, "password");
            put("utorrent_label", category, "category");
        }
        Watchdir => {
            put("local_watchdir", Some(need_dir()?), "directory");
            put("torrent_local", Some("True"), "directory");
        }
    }
    let (selector, value) = provider.selector();
    settings.push(Setting {
        section: "Client",
        key: selector,
        value: value.to_string(),
    });
    if provider.protocol() == Protocol::Torrent {
        settings.push(Setting {
            section: "Torrents",
            key: "enable_torrents",
            value: "True".to_string(),
        });
    }
    if encrypts(ini) {
        for s in &mut settings {
            if ENCRYPTED.contains(&s.key) && s.value != UNSET {
                s.value = encrypt(s.key, &s.value)?;
            }
        }
    }
    let supplied = [
        ("url", url.is_some()),
        ("username", username.is_some()),
        ("password", password.is_some()),
        ("api_key", api_key.is_some()),
        ("category", category_given.is_some()),
        ("directory", directory.is_some()),
        ("client_directory", client_directory.is_some()),
        ("priority", priority_given.is_some()),
    ];
    let ignored = supplied
        .into_iter()
        .filter(|(field, given)| *given && !used.contains(field))
        .map(|(field, _)| field.to_string())
        .collect();
    Ok(Mapping { settings, ignored })
}

/// A mapped setting whose stored value Mylar reads differently.
#[derive(Clone, PartialEq, Eq)]
pub struct Change {
    pub setting: Setting,
    /// The value `config.ini` holds now; `None` when the key is absent.
    pub current: Option<String>,
    /// The change unsets a value Mylar reads as set now.
    pub clears: bool,
}

impl std::fmt::Debug for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Change")
            .field("setting", &self.setting)
            .field(
                "current",
                &self.current.as_deref().map(|v| shown(self.setting.key, v)),
            )
            .field("clears", &self.clears)
            .finish()
    }
}

/// [`plan`]'s result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientDiff {
    pub name: String,
    pub changes: Vec<Change>,
    /// Fields the client supplied that mylar3 has no setting for.
    pub ignored: Vec<String>,
}

/// [`ClientDiff::report`]'s output, secret values withheld.
#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientReport {
    /// Changes that set a value.
    pub changes: Vec<SettingChange>,
    /// Changes that unset a value Mylar reads as set now.
    pub clears: Vec<SettingChange>,
}

impl ClientDiff {
    pub fn in_sync(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn edits(&self) -> Vec<ini::Edit> {
        self.changes.iter().map(|c| c.setting.edit()).collect()
    }

    pub fn report(&self) -> ClientReport {
        let (clears, changes): (Vec<&Change>, Vec<&Change>) =
            self.changes.iter().partition(|c| c.clears);
        let out = |changes: Vec<&Change>| {
            changes
                .into_iter()
                .map(|c| {
                    let key = c.setting.key;
                    SettingChange {
                        key: key.to_string(),
                        current: c.current.as_deref().map(|v| shown(key, v).to_string()),
                        target: shown(key, &c.setting.value).to_string(),
                        reason: format!("download client '{}'", self.name),
                    }
                })
                .collect()
        };
        ClientReport {
            changes: out(changes),
            clears: out(clears),
        }
    }
}

/// Mylar's default for an owned key whose `_CONFIG_DEFINITIONS` default is not
/// `None`; a missing or unset key reads as it (`minimal_ini` omits defaults).
fn default(key: &str) -> Option<&'static str> {
    match key {
        "sab_priority" => Some("Default"),
        "sab_to_mylar" | "enable_torrents" | "torrent_local" => Some("False"),
        "nzb_downloader" => Some("3"),
        "torrent_downloader" => Some("0"),
        _ => None,
    }
}

/// `sab_host` as Mylar's config load fixes it up, character for character.
fn sab_host_read(v: &str) -> String {
    let mut v = if v.starts_with("http://") || v.starts_with("https://") {
        v.to_string()
    } else {
        format!("http://{v}")
    };
    if v.ends_with('/') {
        v.pop();
    }
    v
}

/// `sab_priority` as Mylar's config load reads it: a digit string becomes a
/// word, any other value is compared exactly, as the senders do.
fn sab_priority_read(v: &str) -> String {
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return v.to_string();
    }
    let word = match v {
        "1" => "Low",
        "2" => "Normal",
        "3" => "High",
        "4" => "Paused",
        _ => "Default",
    };
    word.to_string()
}

/// `utorrent_host` as utorrent.py builds its base URL from it.
fn utorrent_host_read(v: &str) -> String {
    let v = if v.starts_with("http") {
        v.to_string()
    } else {
        format!("http://{v}")
    };
    let v = v.strip_suffix('/').unwrap_or(&v);
    v.strip_suffix("/gui").unwrap_or(v).to_string()
}

/// `v` as Mylar acts on it, `None` when unset: a missing or unset key as its
/// default, secrets decrypted only when Mylar decrypts them, and the forms
/// Mylar's readers treat alike made equal.
fn canonical(key: &str, v: Option<&str>, encrypted: bool) -> Option<String> {
    let v = match v.map(str::trim).filter(|v| !is_unset(v)) {
        Some(v) => v,
        None => default(key)?,
    };
    let v = if encrypted && ENCRYPTED.contains(&key) {
        secret::decode(v).unwrap_or_else(|| v.to_string())
    } else {
        v.to_string()
    };
    match key {
        "nzbget_sub" => sub_form(&v),
        "sab_host" => Some(sab_host_read(&v)),
        "sab_priority" => Some(sab_priority_read(&v)),
        "utorrent_host" => Some(utorrent_host_read(&v)),
        "nzb_downloader" | "torrent_downloader" => {
            Some(v.parse::<u8>().map_or(v, |n| n.to_string()))
        }
        "sab_to_mylar" | "enable_torrents" | "torrent_local" => {
            Some(match v.to_ascii_lowercase().as_str() {
                "1" | "yes" | "true" | "on" => "True".to_string(),
                "0" | "no" | "false" | "off" => "False".to_string(),
                _ => v,
            })
        }
        _ => Some(v),
    }
}

/// The settings `client` changes, against `ini`'s configparser-read values:
/// `%` unescaped, as `/getConfig` serves them and [`ini::get`] returns them.
pub fn plan(ini: &ConfigIni, client: &ResolvedDownloadClient) -> Result<ClientDiff> {
    let mapping = map(ini, client)?;
    let encrypted = encrypts(ini);
    let stored = |key| canonical(key, ini.0.get(key).map(String::as_str), encrypted);
    // In Docker, Mylar fills an unset SAB directory with these in memory
    // (config.py, DOCKER-AWARE) and a web save persists them.
    let docker_sab = mapping
        .settings
        .iter()
        .any(|s| s.key == "sab_directory" && s.value == UNSET)
        && stored("sab_to_mylar").as_deref() == Some("True")
        && stored("sab_directory").as_deref() == Some("/downloads");
    let changes = mapping
        .settings
        .into_iter()
        .filter(|s| !(docker_sab && matches!(s.key, "sab_directory" | "sab_to_mylar")))
        .filter_map(|setting| {
            let current = ini.0.get(setting.key).cloned();
            let stored = canonical(setting.key, current.as_deref(), encrypted);
            let target = canonical(setting.key, Some(&setting.value), encrypted);
            (stored != target).then(|| Change {
                clears: target.is_none(),
                setting,
                current,
            })
        })
        .collect();
    Ok(ClientDiff {
        name: client.name.clone(),
        changes,
        ignored: mapping.ignored,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(provider: ClientProvider) -> ResolvedDownloadClient {
        ResolvedDownloadClient {
            provider: Some(provider),
            name: "dl".into(),
            ..Default::default()
        }
    }

    fn sab(url: &str, key: &str) -> ResolvedDownloadClient {
        ResolvedDownloadClient {
            url: Some(url.into()),
            api_key: Some(key.into()),
            directory: Some("/downloads/complete".into()),
            ..client(ClientProvider::Sabnzbd)
        }
    }

    fn ini(pairs: &[(&str, &str)]) -> ConfigIni {
        ConfigIni(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    fn try_map(c: &ResolvedDownloadClient) -> Result<Mapping> {
        map(&ini(&[]), c)
    }

    fn values(c: &ResolvedDownloadClient) -> Vec<(&'static str, &'static str, String)> {
        try_map(c)
            .unwrap()
            .settings
            .into_iter()
            .map(|s| (s.section, s.key, s.value))
            .collect()
    }

    fn value(c: &ResolvedDownloadClient, key: &str) -> String {
        values(c)
            .into_iter()
            .find(|(_, k, _)| *k == key)
            .unwrap_or_else(|| panic!("{key} not mapped"))
            .2
    }

    fn err(c: &ResolvedDownloadClient) -> String {
        try_map(c).unwrap_err().to_string()
    }

    #[test]
    fn category_defaults_for_comics_only() {
        assert_eq!(category_map("comics"), Some("comics"));
        assert_eq!(category_map("movies"), None);
    }

    #[test]
    fn sab_maps_every_key_it_owns_with_unset_ones_as_none() {
        let s = |section, key, value: &str| (section, key, value.to_string());
        assert_eq!(
            values(&sab("http://10.0.0.15:8080/", "SABKEY")),
            vec![
                s("SABnzbd", "sab_host", "http://10.0.0.15:8080"),
                s("SABnzbd", "sab_apikey", "SABKEY"),
                s("SABnzbd", "sab_username", "None"),
                s("SABnzbd", "sab_password", "None"),
                s("SABnzbd", "sab_category", "comics"),
                s("SABnzbd", "sab_priority", "Default"),
                s("SABnzbd", "sab_directory", "/downloads/complete"),
                s("SABnzbd", "sab_to_mylar", "True"),
                s("Client", "nzb_downloader", "0"),
            ]
        );
        let c = ResolvedDownloadClient {
            directory: None,
            ..sab("http://10.0.0.15:8080", "K")
        };
        assert_eq!(value(&c, "sab_directory"), "None");
        assert_eq!(value(&c, "sab_to_mylar"), "False");
    }

    #[test]
    fn sab_host_is_stored_as_configure_leaves_it() {
        let host = |url: &str| value(&sab(url, "K"), "sab_host");
        assert_eq!(host("10.0.0.15:8080"), "http://10.0.0.15:8080");
        assert_eq!(host("http://10.0.0.15:8080//"), "http://10.0.0.15:8080");
        assert_eq!(
            host("https://s.example/sabnzbd/"),
            "https://s.example/sabnzbd"
        );
        assert_eq!(host("HTTP://S.example:8080"), "http://s.example:8080");
        for url in ["http://s:8080/?a=1", "http://s:8080#f"] {
            assert!(err(&sab(url, "K")).contains("query or fragment"), "{url}");
        }
        assert!(err(&sab("ftp://s:21", "K")).contains("http"));
    }

    fn nzbget(url: &str) -> ResolvedDownloadClient {
        ResolvedDownloadClient {
            url: Some(url.into()),
            ..client(ClientProvider::Nzbget)
        }
    }

    #[test]
    fn nzbget_splits_the_port_and_maps_the_path_to_its_sub() {
        let c = ResolvedDownloadClient {
            username: Some("nzb".into()),
            password: Some("PW".into()),
            ..nzbget("https://nzb.example:6789")
        };
        let s = |key, value: &str| ("NZBGet", key, value.to_string());
        assert_eq!(
            values(&c),
            vec![
                s("nzbget_host", "https://nzb.example"),
                s("nzbget_port", "6789"),
                s("nzbget_sub", "None"),
                s("nzbget_username", "nzb"),
                s("nzbget_password", "PW"),
                s("nzbget_category", "comics"),
                s("nzbget_priority", "None"),
                s("nzbget_directory", "None"),
                ("Client", "nzb_downloader", "1".to_string()),
            ]
        );
        assert_eq!(value(&nzbget("http://10.0.0.15"), "nzbget_port"), "80");
        assert!(err(&nzbget("https://nzb.example:6789/?x=1")).contains("query"));
        assert!(err(&nzbget("ftp://nzb.example:6789")).contains("http"));
    }

    #[test]
    fn nzbget_sub_drops_xmlrpc_and_normalizes_slashes() {
        let sub = |url: &str| value(&nzbget(url), "nzbget_sub");
        for path in [
            "/nzbget",
            "/nzbget/",
            "//nzbget//",
            "/nzbget/xmlrpc",
            "/nzbget/xmlrpc/",
        ] {
            assert_eq!(
                sub(&format!("http://10.0.0.15:6789{path}")),
                "/nzbget",
                "{path}"
            );
        }
        for path in ["", "/", "/xmlrpc", "/xmlrpc/"] {
            assert_eq!(
                sub(&format!("http://10.0.0.15:6789{path}")),
                "None",
                "{path}"
            );
        }
        assert_eq!(sub("http://10.0.0.15:6789/a/b"), "/a/b");
        assert_eq!(sub("http://10.0.0.15:6789/myxmlrpc"), "/myxmlrpc");
        assert_eq!(nzbget_sub("nzbget"), nzbget_sub("/nzbget/"));
    }

    #[test]
    fn priorities_are_stored_as_the_words_mylar_maps() {
        let with = |provider: ClientProvider, p: &str| {
            let c = ResolvedDownloadClient {
                provider: Some(provider),
                url: Some("http://10.0.0.15:1".into()),
                priority: Some(p.into()),
                ..sab("http://unused", "K")
            };
            let key = match provider {
                ClientProvider::Sabnzbd => "sab_priority",
                _ => "nzbget_priority",
            };
            try_map(&c).map(|m| m.settings.into_iter().find(|s| s.key == key).unwrap().value)
        };
        assert_eq!(with(ClientProvider::Sabnzbd, "high").unwrap(), "High");
        for (digit, word) in SAB_PRIORITIES.iter().enumerate() {
            assert_eq!(
                with(ClientProvider::Sabnzbd, &digit.to_string()).unwrap(),
                *word
            );
        }
        assert!(with(ClientProvider::Sabnzbd, "5").is_err());
        assert!(with(ClientProvider::Sabnzbd, "Force").is_err());
        assert_eq!(
            with(ClientProvider::Nzbget, "very high").unwrap(),
            "Very High"
        );
        assert_eq!(with(ClientProvider::Nzbget, "Force").unwrap(), "Force");
        let e = with(ClientProvider::Nzbget, "2").unwrap_err().to_string();
        assert!(e.contains("Very High"), "{e}");
    }

    #[test]
    fn urls_refuse_credentials() {
        for provider in [
            ClientProvider::Sabnzbd,
            ClientProvider::Nzbget,
            ClientProvider::Qbittorrent,
            ClientProvider::Deluge,
            ClientProvider::Transmission,
            ClientProvider::Rtorrent,
            ClientProvider::Utorrent,
        ] {
            for url in ["http://user:PW@h:1", "user:PW@h:1"] {
                let c = ResolvedDownloadClient {
                    provider: Some(provider),
                    ..sab(url, "K")
                };
                let e = err(&c);
                assert!(
                    e.contains("credentials") && !e.contains("PW"),
                    "{provider:?}: {e}"
                );
            }
        }
    }

    #[test]
    fn secrets_are_encrypted_when_mylar_encrypts_them() {
        let c = ResolvedDownloadClient {
            username: Some("me".into()),
            password: Some("PW".into()),
            ..sab("http://10.0.0.15:8080", "SABKEY")
        };
        let on = map(&ini(&[("encrypt_passwords", "True")]), &c).unwrap();
        let get = |m: &Mapping, key: &str| {
            m.settings
                .iter()
                .find(|s| s.key == key)
                .unwrap()
                .value
                .clone()
        };
        for (key, plain) in [("sab_apikey", "SABKEY"), ("sab_password", "PW")] {
            let stored = get(&on, key);
            assert!(stored.starts_with("^~$z$"), "{key}: {stored}");
            assert_eq!(secret::decode(&stored).as_deref(), Some(plain), "{key}");
        }
        assert_eq!(get(&on, "sab_username"), "me");
        assert_eq!(get(&on, "sab_host"), "http://10.0.0.15:8080");

        let n = ResolvedDownloadClient {
            password: Some("NPW".into()),
            ..nzbget("http://10.0.0.15:6789")
        };
        let on = map(&ini(&[("encrypt_passwords", "True")]), &n).unwrap();
        assert_eq!(
            secret::decode(&get(&on, "nzbget_password")).as_deref(),
            Some("NPW")
        );

        let unset = map(
            &ini(&[("encrypt_passwords", "True")]),
            &sab("http://h:1", "K"),
        )
        .unwrap();
        assert_eq!(get(&unset, "sab_password"), "None");
        let off = map(&ini(&[("encrypt_passwords", "False")]), &c).unwrap();
        assert_eq!(get(&off, "sab_apikey"), "SABKEY");
    }

    #[test]
    fn edits_carry_raw_values_that_read_back_unchanged() {
        let s = Setting {
            section: "SABnzbd",
            key: "sab_password",
            value: "100%sure".into(),
        };
        assert_eq!(
            s.edit(),
            ini::Edit {
                section: "SABnzbd".into(),
                key: "sab_password".into(),
                value: "100%sure".into(),
            }
        );
        let text = ini::rewrite("[SABnzbd]\nsab_password = old\n", &[s.edit()]).unwrap();
        assert_eq!(
            ini::get(&text, "SABnzbd", "sab_password").as_deref(),
            Some("100%sure")
        );
    }

    #[test]
    fn encrypt_passwords_reads_as_getboolean() {
        let c = sab("http://10.0.0.15:8080", "SABKEY");
        let apikey = |flag: &str| {
            map(&ini(&[("encrypt_passwords", flag)]), &c)
                .unwrap()
                .settings
                .into_iter()
                .find(|s| s.key == "sab_apikey")
                .unwrap()
                .value
        };
        for on in ["True", " true ", "1", "ON", "yes"] {
            assert!(apikey(on).starts_with("^~$z$"), "{on}");
        }
        for off in ["False", "0", "no", "None", "maybe"] {
            assert_eq!(apikey(off), "SABKEY", "{off}");
        }
    }

    #[test]
    fn control_characters_are_refused_without_echoing_the_value() {
        type Set = fn(&mut ResolvedDownloadClient, Option<String>);
        let fields: [(&str, Set); 7] = [
            ("url", |c, v| c.url = v),
            ("username", |c, v| c.username = v),
            ("password", |c, v| c.password = v),
            ("api_key", |c, v| c.api_key = v),
            ("category", |c, v| c.category = v),
            ("directory", |c, v| c.directory = v),
            ("priority", |c, v| c.priority = v),
        ];
        for (name, set) in fields {
            for bad in [
                "pa\nSECRETPW",
                "pa\tSECRETPW",
                "SECRETPW\n",
                "pa\u{7f}SECRETPW",
            ] {
                let mut c = sab("http://10.0.0.15:8080", "K");
                set(&mut c, Some(bad.into()));
                let e = err(&c);
                assert!(
                    e.contains(name) && e.contains("control") && !e.contains("SECRETPW"),
                    "{name} {bad:?}: {e}"
                );
            }
        }
    }

    #[test]
    fn nzbget_url_paths_refuse_credentials() {
        for url in [
            "http://10.0.0.15:6789/nzbget:S3CRET/xmlrpc",
            "http://10.0.0.15:6789/nzbget:S3CRET",
            "http://10.0.0.15:6789/nzbget%3AS3CRET/xmlrpc",
            "http://10.0.0.15:6789/nzbget%3aS3CRET/xmlrpc",
        ] {
            let e = err(&nzbget(url));
            assert!(
                e.contains("user:pass") && !e.contains("S3CRET"),
                "{url}: {e}"
            );
            let dbg = format!("{:?}", nzbget(url));
            assert!(!dbg.contains("S3CRET"), "{dbg}");
        }
        let dbg = format!("{:?}", nzbget("10.0.0.15:6789/a:S3CRET?x=1"));
        assert!(!dbg.contains("S3CRET"), "{dbg}");
        let plain = format!("{:?}", nzbget("http://10.0.0.15:6789/nzbget"));
        assert!(plain.contains("http://10.0.0.15:6789/nzbget"), "{plain}");
    }

    #[test]
    fn a_literal_none_is_refused_in_any_field() {
        type Set = fn(&mut ResolvedDownloadClient, Option<String>);
        let fields: [(&str, Set); 7] = [
            ("url", |c, v| c.url = v),
            ("username", |c, v| c.username = v),
            ("password", |c, v| c.password = v),
            ("api_key", |c, v| c.api_key = v),
            ("category", |c, v| c.category = v),
            ("directory", |c, v| c.directory = v),
            ("priority", |c, v| c.priority = v),
        ];
        for (name, set) in fields {
            for provider in [ClientProvider::Sabnzbd, ClientProvider::Blackhole] {
                let mut c = ResolvedDownloadClient {
                    provider: Some(provider),
                    ..sab("http://10.0.0.15:8080", "K")
                };
                let none = if name == "password" || name == "api_key" {
                    "None"
                } else {
                    " None "
                };
                set(&mut c, Some(none.into()));
                let e = err(&c);
                assert!(e.contains(name) && e.contains("None"), "{name}: {e}");
            }
        }
        let c = ResolvedDownloadClient {
            username: Some("none".into()),
            category: Some("NONE".into()),
            ..sab("http://10.0.0.15:8080", "K")
        };
        assert_eq!(value(&c, "sab_username"), "none");
        assert_eq!(value(&c, "sab_category"), "NONE");
    }

    #[test]
    fn secrets_are_taken_verbatim_or_refused() {
        for (name, c) in [
            (
                "password",
                ResolvedDownloadClient {
                    password: Some(" PW".into()),
                    ..sab("http://10.0.0.15:8080", "K")
                },
            ),
            ("api_key", sab("http://10.0.0.15:8080", "K\t")),
            (
                "password",
                ResolvedDownloadClient {
                    password: Some(String::new()),
                    ..sab("http://10.0.0.15:8080", "K")
                },
            ),
            ("api_key", sab("http://10.0.0.15:8080", "")),
        ] {
            let e = err(&c);
            assert!(e.contains(name) && !e.contains("PW"), "{name}: {e}");
        }
        let c = ResolvedDownloadClient {
            password: Some("p w".into()),
            username: Some("  me  ".into()),
            ..sab("http://10.0.0.15:8080", "K")
        };
        assert_eq!(value(&c, "sab_password"), "p w");
        assert_eq!(value(&c, "sab_username"), "me");
    }

    #[test]
    fn debug_withholds_secrets_and_shows_the_rest() {
        let c = ResolvedDownloadClient {
            username: Some("someuser".into()),
            password: Some("PW123".into()),
            ..sab("http://10.0.0.15:8080", "KEY123")
        };
        let m = try_map(&c).unwrap();
        for dbg in [format!("{c:?}"), format!("{m:?}")] {
            assert!(!dbg.contains("PW123") && !dbg.contains("KEY123"), "{dbg}");
            assert!(dbg.contains(scrub::REDACTED), "{dbg}");
            assert!(
                dbg.contains("someuser") && dbg.contains("10.0.0.15:8080"),
                "{dbg}"
            );
        }
        let url = ResolvedDownloadClient {
            url: Some("http://user:PW9@h:1".into()),
            ..c
        };
        assert!(!format!("{url:?}").contains("PW9"));

        for key in ENCRYPTED {
            let s = Setting {
                section: "S",
                key,
                value: "SECRETVALUE".into(),
            };
            assert!(!format!("{s:?}").contains("SECRETVALUE"), "{key}");
            let unset = Setting {
                value: UNSET.into(),
                ..s
            };
            assert!(format!("{unset:?}").contains(UNSET), "{key}");
        }
        let plain = Setting {
            section: "SABnzbd",
            key: "sab_host",
            value: "http://h".into(),
        };
        assert!(format!("{plain:?}").contains("http://h"));
    }

    #[test]
    fn reports_supplied_fields_with_no_setting() {
        let c = ResolvedDownloadClient {
            url: Some("http://unused:1".into()),
            directory: Some("/blackhole".into()),
            priority: Some("High".into()),
            ..client(ClientProvider::Blackhole)
        };
        let m = try_map(&c).unwrap();
        assert_eq!(
            m.settings,
            vec![
                Setting {
                    section: "Blackhole",
                    key: "blackhole_dir",
                    value: "/blackhole".into(),
                },
                Setting {
                    section: "Client",
                    key: "nzb_downloader",
                    value: "2".into(),
                },
            ]
        );
        assert_eq!(m.ignored, vec!["url".to_string(), "priority".to_string()]);
    }

    #[test]
    fn refuses_incomplete_clients() {
        let no_provider = ResolvedDownloadClient {
            name: "x".into(),
            ..Default::default()
        };
        assert!(err(&no_provider).contains("no provider"));
        let no_key = ResolvedDownloadClient {
            url: Some("http://s:8080".into()),
            ..client(ClientProvider::Sabnzbd)
        };
        assert!(err(&no_key).contains("api_key"));
        let blank_key = ResolvedDownloadClient {
            api_key: Some("  ".into()),
            ..no_key
        };
        assert!(err(&blank_key).contains("api_key"));
        assert!(err(&client(ClientProvider::Nzbget)).contains("url"));
        assert!(err(&client(ClientProvider::Blackhole)).contains("directory"));
    }

    fn deluge(url: &str) -> ResolvedDownloadClient {
        ResolvedDownloadClient {
            url: Some(url.into()),
            username: Some("localclient".into()),
            password: Some("PW".into()),
            ..client(ClientProvider::Deluge)
        }
    }

    #[test]
    fn deluge_stores_bare_host_port_and_needs_credentials() {
        for url in ["http://10.0.0.20:58846", "10.0.0.20:58846"] {
            assert_eq!(
                value(&deluge(url), "deluge_host"),
                "10.0.0.20:58846",
                "{url}"
            );
        }
        for url in [
            "10.0.0.20",
            "10.0.0.20:",
            "http://10.0.0.20:",
            "http://10.0.0.20",
        ] {
            let e = err(&deluge(url));
            assert!(e.contains("port"), "{url}: {e}");
        }
        for url in [
            "http://10.0.0.20:58846/x",
            "10.0.0.20:58846?a=1",
            "10.0.0.20:58846#f",
        ] {
            let e = err(&deluge(url));
            assert!(e.contains("without a path"), "{url}: {e}");
        }
        assert_eq!(
            value(&deluge("http://10.0.0.20:80"), "deluge_host"),
            "10.0.0.20:80"
        );
        assert_eq!(
            value(&deluge("10.0.0.20:80"), "deluge_host"),
            "10.0.0.20:80"
        );
        for url in ["http://[fd00::20]:58846", "[fd00::20]:58846"] {
            let e = err(&deluge(url));
            assert!(e.contains("IPv6"), "{url}: {e}");
        }
        for c in [
            ResolvedDownloadClient {
                username: None,
                ..deluge("10.0.0.20:58846")
            },
            ResolvedDownloadClient {
                password: None,
                ..deluge("10.0.0.20:58846")
            },
        ] {
            assert!(err(&c).contains("username and password"));
        }
        assert_eq!(
            value(&deluge("10.0.0.20:58846"), "deluge_download_directory"),
            ""
        );
    }

    #[test]
    fn torrent_clients_select_themselves_and_enable_torrents() {
        let c = ResolvedDownloadClient {
            url: Some("http://10.0.0.16:8080".into()),
            username: Some("admin".into()),
            password: Some("PW".into()),
            client_directory: Some("/downloads/torrents".into()),
            ..client(ClientProvider::Qbittorrent)
        };
        let s = |key, value: &str| ("qBittorrent", key, value.to_string());
        assert_eq!(
            values(&c),
            vec![
                s("qbittorrent_host", "http://10.0.0.16:8080"),
                s("qbittorrent_username", "admin"),
                s("qbittorrent_password", "PW"),
                s("qbittorrent_label", "comics"),
                s("qbittorrent_folder", "/downloads/torrents"),
                ("Client", "torrent_downloader", "5".to_string()),
                ("Torrents", "enable_torrents", "True".to_string()),
            ]
        );
        for (provider, selected) in [
            (ClientProvider::Utorrent, "1"),
            (ClientProvider::Rtorrent, "2"),
            (ClientProvider::Transmission, "3"),
            (ClientProvider::Deluge, "4"),
        ] {
            let c = ResolvedDownloadClient {
                provider: Some(provider),
                ..deluge("http://10.0.0.16:1")
            };
            assert_eq!(value(&c, "torrent_downloader"), selected, "{provider:?}");
            assert_eq!(value(&c, "enable_torrents"), "True", "{provider:?}");
        }
        assert!(!values(&sab("http://10.0.0.15:8080", "K"))
            .iter()
            .any(|(_, k, _)| *k == "enable_torrents"));
    }

    #[test]
    fn utorrent_host_keeps_its_path_and_gets_a_lowercase_scheme() {
        let host = |url: &str| {
            value(
                &ResolvedDownloadClient {
                    url: Some(url.into()),
                    ..client(ClientProvider::Utorrent)
                },
                "utorrent_host",
            )
        };
        for (url, stored) in [
            ("10.0.0.18:8080", "http://10.0.0.18:8080"),
            ("10.0.0.18:8080/gui/", "http://10.0.0.18:8080/gui/"),
            ("HTTP://10.0.0.18:8080/", "http://10.0.0.18:8080/"),
            ("httpbox:8080", "http://httpbox:8080"),
            (
                "http://10.0.0.18:8080/gui/gui",
                "http://10.0.0.18:8080/gui/gui",
            ),
            (
                "https://u.example/utorrent/gui//",
                "https://u.example/utorrent/gui//",
            ),
        ] {
            assert_eq!(host(url), stored, "{url}");
        }
        let refused = |url: &str| {
            err(&ResolvedDownloadClient {
                url: Some(url.into()),
                ..client(ClientProvider::Utorrent)
            })
        };
        for url in ["http://10.0.0.18:8080/gui/?token=x", "10.0.0.18:8080#f"] {
            assert!(refused(url).contains("query or fragment"), "{url}");
        }
        assert!(refused("ftp://10.0.0.18:21").contains("http://"));
    }

    #[test]
    fn torrent_fields_refuse_control_characters_the_url_parser_drops() {
        let qbit = ResolvedDownloadClient {
            url: Some("http://10.0.0.16:8080/q\tb".into()),
            ..client(ClientProvider::Qbittorrent)
        };
        let transmission = ResolvedDownloadClient {
            url: Some("http://10.0.0.15:9091/custom\n/rpc".into()),
            ..client(ClientProvider::Transmission)
        };
        let dir = ResolvedDownloadClient {
            url: Some("http://10.0.0.16:8080".into()),
            client_directory: Some("/data/\ntorrents".into()),
            ..client(ClientProvider::Qbittorrent)
        };
        let watch = ResolvedDownloadClient {
            directory: Some("/watch\r".into()),
            ..client(ClientProvider::Watchdir)
        };
        for (field, c) in [
            ("url", qbit),
            ("url", transmission),
            ("client_directory", dir),
            ("directory", watch),
        ] {
            let e = err(&c);
            assert!(e.contains(field) && e.contains("control"), "{field}: {e}");
        }
    }

    #[test]
    fn qbittorrent_url_refuses_a_query_or_fragment() {
        for url in ["http://10.0.0.16:8080/?x=1", "http://10.0.0.16:8080#f"] {
            let c = ResolvedDownloadClient {
                url: Some(url.into()),
                ..client(ClientProvider::Qbittorrent)
            };
            assert!(err(&c).contains("query or fragment"), "{url}");
        }
    }

    fn transmission(url: &str) -> ResolvedDownloadClient {
        ResolvedDownloadClient {
            url: Some(url.into()),
            ..client(ClientProvider::Transmission)
        }
    }

    #[test]
    fn transmission_gets_its_rpc_path_unless_one_is_given() {
        let host = |url: &str| value(&transmission(url), "transmission_host");
        assert_eq!(
            host("http://10.0.0.15:9091"),
            "http://10.0.0.15:9091/transmission/rpc"
        );
        assert_eq!(
            host("http://10.0.0.15:9091/"),
            "http://10.0.0.15:9091/transmission/rpc"
        );
        assert_eq!(
            host("https://t.example/custom/rpc"),
            "https://t.example/custom/rpc"
        );
        for url in ["http://t:9091?x=1", "http://t:9091/rpc#f"] {
            let e = err(&transmission(url));
            assert!(e.contains("query or fragment"), "{url}: {e}");
        }
        let c = transmission("http://10.0.0.15:9091");
        assert_eq!(value(&c, "transmission_username"), "None");
        assert_eq!(value(&c, "transmission_directory"), "None");
    }

    #[test]
    fn rtorrent_splits_the_rpc_path_out_of_the_host() {
        let c = |url: &str| ResolvedDownloadClient {
            url: Some(url.into()),
            ..client(ClientProvider::Rtorrent)
        };
        let rt = c("https://rt.example:8443/user/RPC2");
        assert_eq!(value(&rt, "rtorrent_host"), "https://rt.example:8443");
        assert_eq!(value(&rt, "rtorrent_rpc_url"), "user/RPC2");
        let rt = c("http://10.0.0.17:5000/");
        assert_eq!(value(&rt, "rtorrent_host"), "http://10.0.0.17:5000");
        assert_eq!(value(&rt, "rtorrent_rpc_url"), "None");
        assert!(err(&c("scgi://10.0.0.17:5000")).contains("http"));
        assert!(err(&c("10.0.0.17:5000")).contains("scheme"));
        assert!(err(&c("http://10.0.0.17:5000/RPC2?x=1")).contains("query"));
    }

    #[test]
    fn torrent_urls_need_http_where_mylar_does() {
        for provider in [ClientProvider::Qbittorrent, ClientProvider::Transmission] {
            for url in ["ftp://h:1", "h.example:1"] {
                let c = ResolvedDownloadClient {
                    provider: Some(provider),
                    url: Some(url.into()),
                    ..Default::default()
                };
                assert!(err(&c).contains("http"), "{provider:?} {url}");
            }
        }
    }

    #[test]
    fn directories_split_by_whose_view_they_are() {
        let c = ResolvedDownloadClient {
            url: Some("http://10.0.0.16:8080".into()),
            directory: Some("/mylar/downloads".into()),
            client_directory: Some("/data/torrents".into()),
            ..client(ClientProvider::Qbittorrent)
        };
        assert_eq!(value(&c, "qbittorrent_folder"), "/data/torrents");
        assert_eq!(try_map(&c).unwrap().ignored, vec!["directory".to_string()]);

        let c = ResolvedDownloadClient {
            client_directory: Some("/data/complete".into()),
            ..sab("http://10.0.0.15:8080", "K")
        };
        assert_eq!(value(&c, "sab_directory"), "/downloads/complete");
        assert_eq!(
            try_map(&c).unwrap().ignored,
            vec!["client_directory".to_string()]
        );
    }

    #[test]
    fn watchdir_needs_a_directory_and_reports_ignored_fields() {
        let c = ResolvedDownloadClient {
            url: Some("http://unused:1".into()),
            directory: Some("/watch".into()),
            ..client(ClientProvider::Watchdir)
        };
        let m = try_map(&c).unwrap();
        assert_eq!(
            values(&c),
            vec![
                ("Watchdir", "local_watchdir", "/watch".to_string()),
                ("Watchdir", "torrent_local", "True".to_string()),
                ("Client", "torrent_downloader", "0".to_string()),
                ("Torrents", "enable_torrents", "True".to_string()),
            ]
        );
        assert_eq!(m.ignored, vec!["url".to_string()]);
        assert!(err(&client(ClientProvider::Watchdir)).contains("directory"));

        let c = ResolvedDownloadClient {
            category: Some("comics".into()),
            priority: Some("High".into()),
            ..transmission("http://10.0.0.15:9091")
        };
        assert_eq!(
            try_map(&c).unwrap().ignored,
            vec!["category".to_string(), "priority".to_string()]
        );
    }

    #[test]
    fn torrent_passwords_are_encrypted_when_mylar_encrypts_them() {
        let on = ini(&[("encrypt_passwords", "True")]);
        for provider in [
            ClientProvider::Qbittorrent,
            ClientProvider::Deluge,
            ClientProvider::Transmission,
            ClientProvider::Rtorrent,
            ClientProvider::Utorrent,
        ] {
            let c = ResolvedDownloadClient {
                provider: Some(provider),
                ..deluge("http://10.0.0.16:1")
            };
            let m = map(&on, &c).unwrap();
            let pw = m
                .settings
                .iter()
                .find(|s| s.key.ends_with("_password"))
                .unwrap();
            assert_eq!(
                secret::decode(&pw.value).as_deref(),
                Some("PW"),
                "{provider:?}"
            );
            assert!(!format!("{m:?}").contains(&pw.value), "{provider:?}");
        }
    }

    fn changed(saved: &[(&str, &str)], c: &ResolvedDownloadClient) -> Vec<(String, String)> {
        plan(&ini(saved), c)
            .unwrap()
            .changes
            .into_iter()
            .map(|c| (c.setting.key.to_string(), c.setting.value))
            .collect()
    }

    const SAB_SAVED: &[(&str, &str)] = &[
        ("sab_host", "http://10.0.0.15:8080"),
        ("sab_apikey", "SABKEY"),
        ("sab_username", "None"),
        ("sab_password", ""),
        ("sab_category", "comics"),
        ("sab_priority", "Default"),
        ("sab_directory", "/downloads/complete"),
        ("sab_to_mylar", "True"),
        ("nzb_downloader", "0"),
    ];

    #[test]
    fn a_client_already_stored_plans_nothing() {
        let diff = plan(&ini(SAB_SAVED), &sab("http://10.0.0.15:8080", "SABKEY")).unwrap();
        assert!(diff.in_sync(), "{diff:?}");
        assert!(diff.edits().is_empty());

        let mut saved = SAB_SAVED.to_vec();
        saved.retain(|(k, _)| *k != "sab_username");
        assert!(changed(&saved, &sab("http://10.0.0.15:8080", "SABKEY")).is_empty());
    }

    #[test]
    fn forms_mylar_reads_alike_are_not_rewritten() {
        let c = sab("http://10.0.0.15:8080", "SABKEY");
        let with = |key: &str, v: &str| {
            let mut saved = SAB_SAVED.to_vec();
            saved.retain(|(k, _)| *k != key);
            saved.push((key, v));
            changed(&saved, &c)
        };
        for (key, saved) in [
            ("sab_host", "10.0.0.15:8080/"),
            ("sab_host", "http://10.0.0.15:8080/"),
            ("sab_priority", "0"),
            ("sab_to_mylar", "true"),
            ("sab_to_mylar", "yes"),
            ("nzb_downloader", "00"),
        ] {
            assert!(with(key, saved).is_empty(), "{key}={saved}");
        }
        assert_eq!(
            with("sab_priority", "3"),
            vec![("sab_priority".to_string(), "Default".to_string())]
        );
        assert_eq!(
            with("sab_host", "http://10.0.0.16:8080"),
            vec![("sab_host".to_string(), "http://10.0.0.15:8080".to_string())]
        );

        let n = nzbget("http://10.0.0.15:6789/nzbget");
        let base = [
            ("nzbget_host", "http://10.0.0.15"),
            ("nzbget_port", "6789"),
            ("nzbget_category", "comics"),
            ("nzb_downloader", "1"),
        ];
        for saved in ["nzbget", "/nzbget/", "/nzbget"] {
            let mut s = base.to_vec();
            s.push(("nzbget_sub", saved));
            assert!(changed(&s, &n).is_empty(), "{saved}");
        }
        let mut s = base.to_vec();
        s.push(("nzbget_sub", "/nzbget/xmlrpc"));
        assert_eq!(
            changed(&s, &n),
            vec![("nzbget_sub".to_string(), "/nzbget".to_string())]
        );
    }

    #[test]
    fn a_stale_value_is_cleared_to_none() {
        let n = nzbget("http://10.0.0.15:6789");
        let diff = plan(&ini(&[("nzbget_sub", "/nzbget")]), &n).unwrap();
        let sub = diff
            .changes
            .iter()
            .find(|c| c.setting.key == "nzbget_sub")
            .unwrap();
        assert_eq!(sub.setting.value, "None");
        assert_eq!(sub.current.as_deref(), Some("/nzbget"));
        assert_eq!(sub.setting.edit().value, "None");
        assert!(sub.clears);
        let host = diff
            .changes
            .iter()
            .find(|c| c.setting.key == "nzbget_host")
            .unwrap();
        assert!(!host.clears);
        let report = diff.report();
        assert_eq!(
            report.clears.iter().map(|c| &c.key).collect::<Vec<_>>(),
            ["nzbget_sub"]
        );
        assert!(!report.changes.iter().any(|c| c.key == "nzbget_sub"));
        assert!(report.changes.iter().any(|c| c.key == "nzbget_host"));
        for saved in ["None", "", " "] {
            let diff = plan(&ini(&[("nzbget_sub", saved)]), &n).unwrap();
            assert!(
                !diff.changes.iter().any(|c| c.setting.key == "nzbget_sub"),
                "{saved:?}"
            );
        }

        let rt = ResolvedDownloadClient {
            url: Some("http://10.0.0.17:5000".into()),
            ..client(ClientProvider::Rtorrent)
        };
        assert!(changed(&[("rtorrent_rpc_url", "RPC2")], &rt)
            .contains(&("rtorrent_rpc_url".to_string(), "None".to_string())));
        let d = deluge("10.0.0.20:58846");
        assert!(changed(&[("deluge_download_directory", "/old")], &d)
            .contains(&("deluge_download_directory".to_string(), String::new())));
        assert!(!changed(&[("deluge_download_directory", "")], &d)
            .iter()
            .any(|(k, _)| k == "deluge_download_directory"));
    }

    #[test]
    fn encrypted_values_compare_by_plaintext_only_when_mylar_decrypts() {
        let c = ResolvedDownloadClient {
            username: Some("me".into()),
            ..sab("http://10.0.0.15:8080", "SABKEY")
        };
        let stored = secret::encode("SABKEY").unwrap();
        let mut saved = SAB_SAVED.to_vec();
        saved.retain(|(k, _)| *k != "sab_apikey" && *k != "sab_username");
        saved.push(("sab_apikey", &stored));
        saved.push(("sab_username", "me"));
        let off = changed(&saved, &c);
        assert_eq!(off, vec![("sab_apikey".to_string(), "SABKEY".to_string())]);
        let mut yes = saved.clone();
        yes.push(("encrypt_passwords", "yes"));
        assert!(changed(&yes, &c).is_empty());
        let mut unparsed = saved.clone();
        unparsed.push(("encrypt_passwords", "maybe"));
        assert_eq!(changed(&unparsed, &c).len(), 1);

        saved.push(("encrypt_passwords", "True"));
        assert!(changed(&saved, &c).is_empty());

        let username = secret::encode("me").unwrap();
        let mut encoded_name = saved.clone();
        encoded_name.retain(|(k, _)| *k != "sab_username");
        encoded_name.push(("sab_username", &username));
        assert_eq!(
            changed(&encoded_name, &c),
            vec![("sab_username".to_string(), "me".to_string())]
        );

        let other = ResolvedDownloadClient {
            api_key: Some("NEWKEY".into()),
            ..c
        };
        let diff = changed(&saved, &other);
        assert_eq!(diff.len(), 1, "{diff:?}");
        assert_eq!(diff[0].0, "sab_apikey");
        assert_eq!(secret::decode(&diff[0].1).as_deref(), Some("NEWKEY"));
    }

    #[test]
    fn sab_priority_words_compare_exactly_and_digits_as_mylar_reads_them() {
        let with = |saved: &str, p: Option<&str>| {
            let mut s = SAB_SAVED.to_vec();
            s.retain(|(k, _)| *k != "sab_priority");
            s.push(("sab_priority", saved));
            let c = ResolvedDownloadClient {
                priority: p.map(Into::into),
                ..sab("http://10.0.0.15:8080", "SABKEY")
            };
            changed(&s, &c)
        };
        assert_eq!(
            with("high", Some("High")),
            vec![("sab_priority".to_string(), "High".to_string())]
        );
        assert_eq!(
            with("default", None),
            vec![("sab_priority".to_string(), "Default".to_string())]
        );
        assert!(with("High", Some("high")).is_empty());
        for saved in ["0", "5", "9", "00", "03", "None", ""] {
            assert!(with(saved, None).is_empty(), "{saved:?}");
        }
        assert!(with("3", Some("High")).is_empty());
    }

    #[test]
    fn a_missing_key_reads_as_its_mylar_default() {
        let c = ResolvedDownloadClient {
            directory: None,
            ..sab("http://10.0.0.15:8080", "SABKEY")
        };
        let mut saved = SAB_SAVED.to_vec();
        saved.retain(|(k, _)| !matches!(*k, "sab_priority" | "sab_to_mylar" | "sab_directory"));
        assert!(changed(&saved, &c).is_empty());
        saved.retain(|(k, _)| *k != "nzb_downloader");
        assert_eq!(
            changed(&saved, &c),
            vec![("nzb_downloader".to_string(), "0".to_string())]
        );
        let unset: Vec<_> = SAB_SAVED
            .iter()
            .map(|&(k, v)| match k {
                "sab_priority" | "sab_to_mylar" | "sab_directory" => (k, "None"),
                _ => (k, v),
            })
            .collect();
        assert!(changed(&unset, &c).is_empty());

        let watch = ResolvedDownloadClient {
            directory: Some("/watch".into()),
            ..client(ClientProvider::Watchdir)
        };
        assert_eq!(
            changed(&[("local_watchdir", "/watch")], &watch),
            vec![
                ("torrent_local".to_string(), "True".to_string()),
                ("enable_torrents".to_string(), "True".to_string()),
            ]
        );
        let diff = plan(&ini(&[("local_watchdir", "/watch")]), &watch).unwrap();
        assert!(diff
            .changes
            .iter()
            .all(|c| !c.clears && c.current.is_none()));
    }

    #[test]
    fn sab_host_compares_by_mylars_literal_fix_up() {
        let with = |saved: &str| {
            let mut s = SAB_SAVED.to_vec();
            s.retain(|(k, _)| *k != "sab_host");
            s.push(("sab_host", saved));
            changed(&s, &sab("http://10.0.0.15:8080", "SABKEY"))
        };
        for saved in [
            "10.0.0.15:8080",
            "10.0.0.15:8080/",
            "http://10.0.0.15:8080/",
        ] {
            assert!(with(saved).is_empty(), "{saved}");
        }
        for saved in [
            "HTTP://10.0.0.15:8080",
            "http://10.0.0.15:8080//",
            "http://10.0.0.15:80800",
        ] {
            assert_eq!(with(saved).len(), 1, "{saved}");
        }
        let https = sab("https://s.example/sabnzbd/", "SABKEY");
        let mut s = SAB_SAVED.to_vec();
        s.retain(|(k, _)| *k != "sab_host");
        s.push(("sab_host", "https://s.example/sabnzbd/"));
        assert!(changed(&s, &https).is_empty());
    }

    #[test]
    fn utorrent_host_compares_as_utorrent_py_builds_it() {
        let c = |url: &str| ResolvedDownloadClient {
            url: Some(url.into()),
            ..client(ClientProvider::Utorrent)
        };
        let host = |saved: &str, url: &str| {
            changed(
                &[
                    ("utorrent_host", saved),
                    ("utorrent_label", "comics"),
                    ("torrent_downloader", "1"),
                    ("enable_torrents", "True"),
                ],
                &c(url),
            )
        };
        for saved in [
            "10.0.0.18:8080",
            "http://10.0.0.18:8080/",
            "http://10.0.0.18:8080/gui",
            "http://10.0.0.18:8080/gui/",
        ] {
            assert!(host(saved, "10.0.0.18:8080").is_empty(), "{saved}");
        }
        assert!(host("http://10.0.0.18:8080/gui", "http://10.0.0.18:8080/").is_empty());
        assert_eq!(
            host("http://10.0.0.18:8080/gui/gui", "http://10.0.0.18:8080/gui"),
            vec![(
                "utorrent_host".to_string(),
                "http://10.0.0.18:8080/gui".to_string()
            )]
        );
    }

    #[test]
    fn docker_sab_defaults_are_in_sync_when_orca_gives_no_directory() {
        let c = ResolvedDownloadClient {
            directory: None,
            ..sab("http://10.0.0.15:8080", "SABKEY")
        };
        let with = |dir: &str, to_mylar: &str| {
            let mut s = SAB_SAVED.to_vec();
            s.retain(|(k, _)| !matches!(*k, "sab_directory" | "sab_to_mylar"));
            s.push(("sab_directory", dir));
            s.push(("sab_to_mylar", to_mylar));
            changed(&s, &c)
        };
        assert!(with("/downloads", "True").is_empty());
        assert!(with("/downloads", "1").is_empty());
        assert_eq!(with("/downloads", "False").len(), 1);
        assert_eq!(with("/other", "True").len(), 2);
        let given = ResolvedDownloadClient {
            directory: Some("/complete".into()),
            ..c
        };
        let mut s = SAB_SAVED.to_vec();
        s.retain(|(k, _)| *k != "sab_directory");
        s.push(("sab_directory", "/downloads"));
        assert_eq!(
            changed(&s, &given),
            vec![("sab_directory".to_string(), "/complete".to_string())]
        );
    }

    #[test]
    fn a_stored_nzbget_sub_with_credentials_is_withheld() {
        let n = nzbget("http://10.0.0.15:6789/nzbget");
        let diff = plan(&ini(&[("nzbget_sub", "/nzbget:OLDPW")]), &n).unwrap();
        let report = diff.report();
        let sub = report
            .changes
            .iter()
            .find(|c| c.key == "nzbget_sub")
            .unwrap();
        assert_eq!(sub.current.as_deref(), Some(scrub::REDACTED));
        assert_eq!(sub.target, "/nzbget");
        assert!(!format!("{diff:?}").contains("OLDPW"));
    }

    #[test]
    fn report_and_debug_withhold_secrets_and_show_the_rest() {
        let saved = [
            ("sab_host", "http://10.0.0.16:8080"),
            ("sab_apikey", "OLDSECRET"),
            ("sab_password", "OLDPW"),
        ];
        let diff = plan(&ini(&saved), &sab("http://10.0.0.15:8080", "NEWSECRET")).unwrap();
        let report = diff.report();
        let json = plugin_toolkit::serde_json::to_string(&report).unwrap();
        for out in [format!("{diff:?}"), format!("{report:?}"), json.clone()] {
            for secret in ["OLDSECRET", "NEWSECRET", "OLDPW"] {
                assert!(!out.contains(secret), "{secret} in {out}");
            }
            assert!(out.contains(scrub::REDACTED), "{out}");
            assert!(
                out.contains("10.0.0.16:8080") && out.contains("10.0.0.15:8080"),
                "{out}"
            );
        }
        let get = |key: &str| report.changes.iter().find(|c| c.key == key).unwrap();
        let key = get("sab_apikey");
        assert_eq!(key.target, scrub::REDACTED);
        assert_eq!(key.current.as_deref(), Some(scrub::REDACTED));
        let pw = report
            .clears
            .iter()
            .find(|c| c.key == "sab_password")
            .unwrap();
        assert_eq!(pw.target, "None");
        assert_eq!(pw.current.as_deref(), Some(scrub::REDACTED));
        assert!(!report.changes.iter().any(|c| c.key == "sab_username"));
        assert_eq!(
            get("sab_host").current.as_deref(),
            Some("http://10.0.0.16:8080")
        );
        assert_eq!(key.reason, "download client 'dl'");
    }

    #[test]
    fn diff_edits_are_what_a_direct_write_needs() {
        let c = ResolvedDownloadClient {
            password: Some("50%off".into()),
            ..sab("http://10.0.0.15:8080", "SABKEY")
        };
        let diff = plan(&ini(SAB_SAVED), &c).unwrap();
        assert_eq!(
            diff.edits(),
            vec![ini::Edit {
                section: "SABnzbd".into(),
                key: "sab_password".into(),
                value: "50%off".into(),
            }]
        );
        let text = ini::rewrite("[SABnzbd]\nsab_password = \n", &diff.edits()).unwrap();
        let read = ini::get(&text, "SABnzbd", "sab_password").unwrap();
        assert_eq!(read, "50%off");
        let mut saved = SAB_SAVED.to_vec();
        saved.retain(|(k, _)| *k != "sab_password");
        saved.push(("sab_password", &read));
        assert!(plan(&ini(&saved), &c).unwrap().in_sync());
    }
}

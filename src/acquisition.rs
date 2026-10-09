//! A download client orca resolved (orca#796) mapped to the `config.ini` values
//! Mylar v0.8.3 reads. A direct ini write skips Mylar's save-time fixups, so
//! each value is the one Mylar itself would leave after a save. Indexers reach
//! Mylar through Prowlarr, not here.

use plugin_toolkit::prelude::*;
use plugin_toolkit::reqwest::Url;
use plugin_toolkit::scrub;

use crate::api::ConfigIni;
use crate::{ini, secret};

/// The client category mylar3 files its downloads under, per media type.
pub fn category_map(media_type: &str) -> Option<&'static str> {
    (media_type == "comics").then_some("comics")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientProvider {
    Sabnzbd,
    Nzbget,
    Blackhole,
}

impl ClientProvider {
    /// The `config.ini` section holding this client's settings.
    fn section(self) -> &'static str {
        match self {
            ClientProvider::Sabnzbd => "SABnzbd",
            ClientProvider::Nzbget => "NZBGet",
            ClientProvider::Blackhole => "Blackhole",
        }
    }

    /// The `[Client]` key and value selecting this client.
    fn selector(self) -> (&'static str, &'static str) {
        match self {
            ClientProvider::Sabnzbd => ("nzb_downloader", "0"),
            ClientProvider::Nzbget => ("nzb_downloader", "1"),
            ClientProvider::Blackhole => ("nzb_downloader", "2"),
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
    /// into (`sab_directory`, `nzbget_directory`), or the blackhole folder.
    pub directory: Option<String>,
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
            .field("priority", &self.priority)
            .finish()
    }
}

/// Client keys Mylar stores encrypted when `encrypt_passwords` is on
/// (`encrypt_items` in `mylar/config.py`).
const ENCRYPTED: &[&str] = &["sab_password", "sab_apikey", "nzbget_password"];

pub fn is_secret(key: &str) -> bool {
    ENCRYPTED.contains(&key) || scrub::is_sensitive_key(key)
}

/// What configparser writes for a `str` setting whose default is `None`, and
/// what Mylar reads back as unset.
const UNSET: &str = "None";

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
        let value = if is_secret(self.key) && self.value != UNSET {
            scrub::REDACTED
        } else {
            &self.value
        };
        f.debug_struct("Setting")
            .field("section", &self.section)
            .field("key", &self.key)
            .field("value", &value)
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
pub(crate) fn nzbget_sub(path: &str) -> Option<String> {
    let p = path.trim_matches('/');
    let p = match p.rsplit_once('/') {
        Some((head, "xmlrpc")) => head,
        None if p == "xmlrpc" => "",
        _ => p,
    };
    let p = p.trim_end_matches('/');
    (!p.is_empty()).then(|| format!("/{p}"))
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
    }
    let (selector, value) = provider.selector();
    settings.push(Setting {
        section: "Client",
        key: selector,
        value: value.to_string(),
    });
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
        ("priority", priority_given.is_some()),
    ];
    let ignored = supplied
        .into_iter()
        .filter(|(field, given)| *given && !used.contains(field))
        .map(|(field, _)| field.to_string())
        .collect();
    Ok(Mapping { settings, ignored })
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
        for provider in [ClientProvider::Sabnzbd, ClientProvider::Nzbget] {
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
}

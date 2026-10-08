//! Mylar3 HTTP surface: the `/api?apikey=&cmd=` API plus the web routes that
//! read (`/getConfig`) and write (`/configUpdate`) configuration.
//!
//! Mylar's API has no config read or write command (`cmd_list` in
//! `mylar/api.py`). `/getConfig` is the web UI's settings table: it dumps every
//! `config.ini` option as `[key, value]` rows. `/configUpdate` is the settings
//! form's submit target. Both sit behind the web login when one is configured.

use std::collections::BTreeMap;

use plugin_toolkit::http::{Client as HttpClient, HttpError, ResponseBody};
use plugin_toolkit::prelude::*;
use plugin_toolkit::serde_json;

pub struct Mylar {
    http: HttpClient,
    base: String,
    api_key: String,
    web_auth: Option<(String, String)>,
}

/// One series row from `getIndex`.
#[derive(Debug, Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
pub struct Series {
    #[serde(default)]
    pub id: Option<String>,
}

/// One issue row from `getWanted`. Dates are Mylar's `YYYY-MM-DD` strings;
/// `0000-00-00` or null when ComicVine has none.
#[derive(Debug, Clone, Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
pub struct WantedIssue {
    #[serde(rename = "ComicName", default)]
    pub comic_name: Option<String>,
    #[serde(rename = "Issue_Number", default)]
    pub issue_number: Option<String>,
    #[serde(rename = "ReleaseDate", default)]
    pub release_date: Option<String>,
    #[serde(rename = "IssueDate", default)]
    pub issue_date: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
pub struct Wanted {
    #[serde(default)]
    pub issues: Vec<WantedIssue>,
    /// Present only when Mylar has annuals enabled.
    #[serde(default)]
    pub annuals: Vec<WantedIssue>,
}

/// One row of Mylar's `snatched` table (`getHistory`). An issue gains a row per
/// grab and per post-process, so its latest row is its current state.
/// `DateAdded` is `YYYY-MM-DD HH:MM:SS` in Mylar's local time.
#[derive(Debug, Clone, Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
pub struct Snatch {
    #[serde(rename = "IssueID", default)]
    pub issue_id: Option<String>,
    #[serde(rename = "ComicName", default)]
    pub comic_name: Option<String>,
    #[serde(rename = "Issue_Number", default)]
    pub issue_number: Option<String>,
    #[serde(rename = "DateAdded", default)]
    pub date_added: Option<String>,
    #[serde(rename = "Status", default)]
    pub status: Option<String>,
    #[serde(rename = "Provider", default)]
    pub provider: Option<String>,
}

#[derive(Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
struct Envelope<T> {
    success: Option<bool>,
    data: Option<T>,
    error: Option<ApiError>,
}

#[derive(Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
struct ApiError {
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
struct ConfigTable {
    #[serde(rename = "aaData")]
    rows: Vec<(String, String)>,
}

/// `config.ini` options keyed by their lowercase ini name, as Mylar last wrote
/// them. Secrets are in here too; callers read only the keys they report.
#[derive(Debug, Default, Clone)]
pub struct ConfigIni(pub BTreeMap<String, String>);

impl ConfigIni {
    /// The value, with Mylar's `None`/empty placeholders as `None`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .get(key)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty() && *v != "None")
    }

    pub fn has(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    /// `config.getboolean` semantics.
    pub fn flag(&self, key: &str) -> Option<bool> {
        match self.get(key)?.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        }
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        self.get(key)?.parse().ok()
    }
}

impl Mylar {
    /// `web_auth` is the web UI login, sent as HTTP basic auth on `/getConfig`.
    pub fn new(base_url: &str, api_key: &str, web_auth: Option<(String, String)>) -> Self {
        Self {
            http: HttpClient::new(),
            base: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            web_auth,
        }
    }

    async fn api_raw(&self, cmd: &str, params: &[(&str, &str)]) -> Result<ResponseBody> {
        let mut req = self
            .http
            .get(format!("{}/api", self.base))
            .query("apikey", self.api_key.clone())
            .query("cmd", cmd);
        for (k, v) in params {
            req = req.query(*k, *v);
        }
        let resp = req.send().await.map_err(|e| anyhow!("mylar {cmd}: {e}"))?;
        Ok(resp.body)
    }

    /// Commands answer either `{success, data}` or their bare payload; a refusal
    /// (bad key, API disabled) is always `{success: false, error}`.
    async fn api<T: for<'de> Deserialize<'de>>(
        &self,
        cmd: &str,
        params: &[(&str, &str)],
    ) -> Result<T> {
        let json = match self.api_raw(cmd, params).await? {
            ResponseBody::Json { json } => json,
            ResponseBody::Text { text } => bail!("mylar {cmd}: non-JSON reply: {}", clip(&text)),
        };
        if json.get("success").is_some() {
            let env: Envelope<T> =
                serde_json::from_value(json).map_err(|e| anyhow!("decode mylar {cmd}: {e}"))?;
            if env.success != Some(true) {
                let msg = env.error.map(|e| e.message).unwrap_or_default();
                bail!("mylar {cmd} refused: {msg}");
            }
            return env
                .data
                .ok_or_else(|| anyhow!("mylar {cmd}: success without data"));
        }
        serde_json::from_value(json).map_err(|e| anyhow!("decode mylar {cmd}: {e}"))
    }

    pub async fn index(&self) -> Result<Vec<Series>> {
        self.api("getIndex", &[]).await
    }

    pub async fn wanted(&self) -> Result<Wanted> {
        self.api("getWanted", &[]).await
    }

    pub async fn history(&self) -> Result<Vec<Snatch>> {
        self.api("getHistory", &[]).await
    }

    /// Queue post-processing of `folder` (a path inside Mylar's container).
    /// `nzb_name=Manual Run` makes Mylar scan the whole folder against its
    /// watchlist; a real job name is joined onto the folder and misses.
    /// Mylar answers with a plain-text acknowledgement, never JSON.
    pub async fn force_process(&self, folder: &str) -> Result<String> {
        let body = self
            .api_raw(
                "forceProcess",
                &[("nzb_name", "Manual Run"), ("nzb_folder", folder)],
            )
            .await?;
        let text = match body {
            ResponseBody::Text { text } => text,
            ResponseBody::Json { json } => {
                if let Some(msg) = json
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                {
                    bail!("mylar forceProcess refused: {msg}");
                }
                json.as_str()
                    .map(str::to_string)
                    .unwrap_or(json.to_string())
            }
        };
        if !text.starts_with("Successfully submitted") {
            bail!("mylar forceProcess did not queue: {}", clip(&text));
        }
        Ok(text)
    }

    /// Read `config.ini` through the web UI's settings table. Errors name the
    /// cause: a 401 is a missing/wrong basic login; an HTML reply is the
    /// forms-login redirect, which needs a session cookie set during a
    /// redirect, and the toolkit HTTP client follows redirects without
    /// exposing intermediate `Set-Cookie` headers.
    pub async fn config(&self) -> Result<ConfigIni> {
        let resp = self
            .http
            .get(self.web_url("getConfig"))
            .query("iDisplayStart", "0")
            .query("iDisplayLength", "100000")
            .send()
            .await
            .map_err(|e| match e {
                HttpError::Status { status: 401, .. } => anyhow!(
                    "mylar /getConfig: 401 — the web UI uses basic auth; set the endpoint's \
                     web_username/web_password"
                ),
                e => anyhow!("mylar /getConfig: {e}"),
            })?;
        match resp.body {
            ResponseBody::Json { json } => {
                let table: ConfigTable = serde_json::from_value(json)
                    .map_err(|e| anyhow!("decode mylar /getConfig: {e}"))?;
                Ok(ConfigIni(
                    table
                        .rows
                        .into_iter()
                        .map(|(k, v)| (k.to_ascii_lowercase(), v))
                        .collect(),
                ))
            }
            ResponseBody::Text { .. } => bail!(
                "mylar /getConfig returned HTML (the forms login page): the web UI uses \
                 forms login (authentication=2), which needs a cookie session the toolkit \
                 HTTP client cannot establish"
            ),
        }
    }

    /// Submit the web settings form (`/configUpdate`). Mylar replies with an
    /// empty page either way; callers confirm by re-reading [`Self::config`].
    pub async fn config_update(&self, fields: Vec<(String, String)>) -> Result<()> {
        self.http
            .post(self.web_url("configUpdate"))
            .form(fields)
            .send()
            .await
            .map_err(|e| anyhow!("mylar /configUpdate: {e}"))?;
        Ok(())
    }

    fn web_url(&self, route: &str) -> String {
        let base = match &self.web_auth {
            Some((user, pass)) => with_userinfo(&self.base, user, pass),
            None => self.base.clone(),
        };
        format!("{base}/{route}")
    }
}

/// reqwest turns URL userinfo into an `Authorization: Basic` header.
fn with_userinfo(base: &str, user: &str, pass: &str) -> String {
    let creds = format!(
        "{}:{}@",
        plugin_toolkit::url::encode(user),
        plugin_toolkit::url::encode(pass)
    );
    match base.split_once("://") {
        Some((scheme, rest)) => format!("{scheme}://{creds}{rest}"),
        None => format!("{creds}{base}"),
    }
}

fn clip(s: &str) -> String {
    let s = s.trim();
    match s.char_indices().nth(200) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> Mylar {
        Mylar::new(&format!("{}/", server.uri()), "KEY", None)
    }

    #[tokio::test]
    async fn index_unwraps_success_envelope() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("apikey", "KEY"))
            .and(query_param("cmd", "getIndex"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true, "data": [{"id": "1"}, {"id": "2"}]
            })))
            .mount(&server)
            .await;
        assert_eq!(client(&server).index().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn wanted_reads_bare_payload() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("cmd", "getWanted"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issues": [{"ComicName": "Saga", "Issue_Number": "1",
                            "ReleaseDate": "2012-03-14", "IssueDate": "2012-05-01"}]
            })))
            .mount(&server)
            .await;
        let w = client(&server).wanted().await.unwrap();
        assert_eq!(w.issues.len(), 1);
        assert_eq!(w.issues[0].release_date.as_deref(), Some("2012-03-14"));
        assert!(w.annuals.is_empty());
    }

    #[tokio::test]
    async fn refusal_surfaces_mylar_message() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": false, "error": {"code": 460, "message": "Incorrect API key"}
            })))
            .mount(&server)
            .await;
        let err = client(&server).history().await.unwrap_err().to_string();
        assert!(err.contains("Incorrect API key"), "{err}");
    }

    #[tokio::test]
    async fn config_reads_settings_table_with_basic_auth() {
        let server = MockServer::start().await;
        // base64("admin:p@ss")
        Mock::given(method("GET"))
            .and(path("/getConfig"))
            .and(header("authorization", "Basic YWRtaW46cEBzcw=="))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "iTotalDisplayRecords": 2, "iTotalRecords": 2,
                "aaData": [["usenet_retention", "3500"], ["SAB_HOST", "None"]]
            })))
            .mount(&server)
            .await;
        let m = Mylar::new(&server.uri(), "KEY", Some(("admin".into(), "p@ss".into())));
        let ini = m.config().await.unwrap();
        assert_eq!(ini.int("usenet_retention"), Some(3500));
        assert!(ini.has("sab_host"));
        assert_eq!(ini.get("sab_host"), None);
    }

    #[tokio::test]
    async fn config_names_forms_login() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/getConfig"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>login</html>"))
            .mount(&server)
            .await;
        let err = client(&server).config().await.unwrap_err().to_string();
        assert!(err.contains("forms login"), "{err}");
    }

    #[tokio::test]
    async fn config_names_basic_auth_on_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/getConfig"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let err = client(&server).config().await.unwrap_err().to_string();
        assert!(err.contains("web_username"), "{err}");
    }

    #[tokio::test]
    async fn force_process_sends_manual_run_and_checks_ack() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("cmd", "forceProcess"))
            .and(query_param("nzb_name", "Manual Run"))
            .and(query_param("nzb_folder", "/downloads/completed/comics"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "Successfully submitted request for post-processing for Manual Run",
            ))
            .mount(&server)
            .await;
        let ack = client(&server)
            .force_process("/downloads/completed/comics")
            .await
            .unwrap();
        assert!(ack.starts_with("Successfully submitted"));
    }

    #[test]
    fn ini_flags_follow_getboolean() {
        let ini = ConfigIni(
            [("a", "True"), ("b", "0"), ("c", "None"), ("d", "maybe")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        );
        assert_eq!(ini.flag("a"), Some(true));
        assert_eq!(ini.flag("b"), Some(false));
        assert_eq!(ini.flag("c"), None);
        assert_eq!(ini.flag("d"), None);
        assert_eq!(ini.flag("missing"), None);
    }
}

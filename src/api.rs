use std::collections::BTreeMap;

use plugin_toolkit::prelude::*;
use plugin_toolkit::reqwest::header::HeaderValue;
use plugin_toolkit::reqwest::{Client as HttpClient, RequestBuilder};
use plugin_toolkit::scrub;
use plugin_toolkit::serde_json;

pub struct Mylar {
    http: HttpClient,
    base: String,
    api_key: String,
    web_auth: Option<(String, String)>,
}

#[derive(Debug, Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
pub struct Series {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

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
    #[serde(rename = "ComicID", default)]
    pub comic_id: Option<String>,
    #[serde(rename = "IssueID", default)]
    pub issue_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
pub struct Wanted {
    #[serde(default)]
    pub issues: Vec<WantedIssue>,
    #[serde(default)]
    pub annuals: Vec<WantedIssue>,
}

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
    #[serde(rename = "Hash", default)]
    pub hash: Option<String>,
    #[serde(rename = "ComicID", default)]
    pub comic_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
pub struct IssueState {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub number: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
pub struct ComicIssues {
    #[serde(default)]
    pub comic: Vec<Series>,
    #[serde(default)]
    pub issues: Vec<IssueState>,
    #[serde(default)]
    pub annuals: Vec<IssueState>,
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

#[derive(Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
struct LogTable {
    #[serde(rename = "iTotalDisplayRecords", default)]
    matched: usize,
    #[serde(rename = "aaData")]
    rows: Vec<(String, String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogPage {
    pub matched: usize,
    pub rows: Vec<(String, String, String)>,
}

#[derive(Debug, Default, Clone)]
pub struct ConfigIni(pub BTreeMap<String, String>);

impl ConfigIni {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .get(key)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty() && *v != "None")
    }

    pub fn secrets(&self) -> Vec<&str> {
        self.0
            .keys()
            .filter(|k| {
                let k = k.to_ascii_lowercase();
                [
                    "apikey", "api_key", "password", "passkey", "token", "secret",
                ]
                .iter()
                .any(|m| k.contains(m))
            })
            .filter_map(|k| self.get(k))
            .filter(|v| v.len() >= 4)
            .collect()
    }

    pub fn has(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

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
    pub fn new(base_url: &str, api_key: &str, web_auth: Option<(String, String)>) -> Self {
        Self {
            http: HttpClient::new(),
            base: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            web_auth,
        }
    }

    async fn api_raw(&self, cmd: &str, params: &[(&str, &str)]) -> Result<ResponseBody> {
        let mut query = vec![("apikey", self.api_key.as_str()), ("cmd", cmd)];
        query.extend_from_slice(params);
        let url = format!("{}/api?{}", self.base, form_encode(&query));
        let what = format!("mylar {cmd}");
        let (status, body) = self.send(self.http.get(url), &what).await?;
        self.success(&what, status, body)
    }

    async fn send(&self, req: RequestBuilder, what: &str) -> Result<(u16, ResponseBody)> {
        let resp = req
            .header("accept", HeaderValue::from_static("application/json"))
            .send()
            .await
            .map_err(|e| self.transport(what, &e.to_string()))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| self.transport(what, &e.to_string()))?;
        Ok((status, ResponseBody::from_bytes(&bytes)))
    }

    fn success(&self, what: &str, status: u16, body: ResponseBody) -> Result<ResponseBody> {
        if !(200..300).contains(&status) {
            bail!(
                "{what}: HTTP {status}: {}",
                clip(&self.redact(&body.text()))
            );
        }
        Ok(body)
    }

    fn web(&self, req: RequestBuilder) -> Result<RequestBuilder> {
        let Some((user, pass)) = &self.web_auth else {
            return Ok(req);
        };
        let token = crate::secret::base64_encode(format!("{user}:{pass}").as_bytes());
        let value = HeaderValue::from_str(&format!("Basic {token}")).map_err(|_| {
            anyhow!("web_username/web_password hold a character a header cannot carry")
        })?;
        Ok(req.header("authorization", value))
    }

    async fn api<T: for<'de> Deserialize<'de>>(
        &self,
        cmd: &str,
        params: &[(&str, &str)],
    ) -> Result<T> {
        let json = match self.api_raw(cmd, params).await? {
            ResponseBody::Json { json } => json,
            ResponseBody::Text { text } => {
                bail!("mylar {cmd}: non-JSON reply: {}", clip(&self.redact(&text)))
            }
        };
        if json.get("success").is_some() {
            let env: Envelope<T> =
                serde_json::from_value(json).map_err(|e| anyhow!("decode mylar {cmd}: {e}"))?;
            if env.success != Some(true) {
                let msg = env.error.map(|e| e.message).unwrap_or_default();
                bail!("mylar {cmd} refused: {}", self.redact(&msg));
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

    pub async fn comic(&self, comic_id: &str) -> Result<ComicIssues> {
        self.api("getComic", &[("id", comic_id)]).await
    }

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
                    bail!("mylar forceProcess refused: {}", self.redact(msg));
                }
                json.as_str()
                    .map(str::to_string)
                    .unwrap_or(json.to_string())
            }
        };
        if !text.starts_with("Successfully submitted") {
            bail!(
                "mylar forceProcess did not queue: {}",
                clip(&self.redact(&text))
            );
        }
        Ok(text)
    }

    pub async fn queue_issue(&self, issue_id: &str) -> Result<()> {
        let body = self.api_raw("queueIssue", &[("id", issue_id)]).await?;
        match body {
            ResponseBody::Text { text } if text.trim() == "OK" => Ok(()),
            ResponseBody::Json { json } => {
                if let Some(msg) = json
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                {
                    bail!("mylar queueIssue refused: {}", self.redact(msg));
                }
                bail!(
                    "mylar queueIssue: unexpected reply: {}",
                    clip(&self.redact(&json.to_string()))
                )
            }
            ResponseBody::Text { text } => bail!(
                "mylar queueIssue: unexpected reply: {}",
                clip(&self.redact(&text))
            ),
        }
    }

    pub async fn queue_annual(
        &self,
        comic_id: &str,
        issue_id: &str,
        number: &str,
        comic_name: Option<&str>,
    ) -> Result<()> {
        let mut query = vec![
            ("mode", "want_ann"),
            ("ComicID", comic_id),
            ("IssueID", issue_id),
            ("ComicIssue", number),
        ];
        if let Some(n) = comic_name {
            query.push(("ComicName", n));
        }
        self.web_get_empty("/queueit", &query).await
    }

    pub async fn failed_handling(&self, comic_id: &str, issue_id: &str) -> Result<()> {
        self.web_get_empty(
            "/failed_handling",
            &[("ComicID", comic_id), ("IssueID", issue_id)],
        )
        .await
    }

    async fn web_get_empty(&self, route: &str, query: &[(&str, &str)]) -> Result<()> {
        let what = format!("mylar {route}");
        let url = format!("{}{route}?{}", self.base, form_encode(query));
        let (status, body) = self.send(self.web(self.http.get(url))?, &what).await?;
        if status == 401 {
            bail!(
                "{what}: 401 — the web UI uses basic auth; set the endpoint's \
                 web_username/web_password"
            );
        }
        match self.success(&what, status, body)? {
            ResponseBody::Text { text } if text.trim().is_empty() => Ok(()),
            ResponseBody::Text { .. } => bail!(
                "{what} returned HTML (the forms login page): the web UI uses forms login \
                 (authentication=2), which needs a cookie session the toolkit HTTP client \
                 cannot establish"
            ),
            ResponseBody::Json { json } => bail!(
                "{what}: unexpected reply: {}",
                clip(&self.redact(&json.to_string()))
            ),
        }
    }

    pub async fn config(&self) -> Result<ConfigIni> {
        let what = "mylar /getConfig";
        let url = format!(
            "{}/getConfig?{}",
            self.base,
            form_encode(&[("iDisplayStart", "0"), ("iDisplayLength", "100000")])
        );
        let (status, body) = self.send(self.web(self.http.get(url))?, what).await?;
        if status == 401 {
            bail!(
                "mylar /getConfig: 401 — the web UI uses basic auth; set the endpoint's \
                 web_username/web_password"
            );
        }
        let body = self.success(what, status, body)?;
        match body {
            ResponseBody::Json { json } => {
                let table: ConfigTable = serde_json::from_value(json)
                    .map_err(|e| anyhow!("decode mylar /getConfig: {e}"))?;
                let mut ini = BTreeMap::new();
                for (k, v) in table.rows {
                    let key = k.to_ascii_lowercase();
                    if ini.insert(key.clone(), v).is_some() {
                        bail!(
                            "mylar /getConfig lists '{key}' twice; refusing to guess which applies"
                        );
                    }
                }
                Ok(ConfigIni(ini))
            }
            ResponseBody::Text { .. } => bail!(
                "mylar /getConfig returned HTML (the forms login page): the web UI uses \
                 forms login (authentication=2), which needs a cookie session the toolkit \
                 HTTP client cannot establish"
            ),
        }
    }

    pub async fn log(&self, search: &str, offset: usize, lines: usize) -> Result<LogPage> {
        let what = "mylar /getLog";
        let offset = offset.to_string();
        let lines = lines.to_string();
        let url = format!(
            "{}/getLog?{}",
            self.base,
            form_encode(&[
                ("iDisplayStart", offset.as_str()),
                ("iDisplayLength", lines.as_str()),
                ("iSortCol_0", "0"),
                ("sSortDir_0", "desc"),
                ("sSearch", search),
            ])
        );
        let (status, body) = self.send(self.web(self.http.get(url))?, what).await?;
        if status == 401 {
            bail!(
                "mylar /getLog: 401 — the web UI uses basic auth; set the endpoint's \
                 web_username/web_password"
            );
        }
        match self.success(what, status, body)? {
            ResponseBody::Json { json } => {
                let table: LogTable = serde_json::from_value(json)
                    .map_err(|e| anyhow!("decode mylar /getLog: {e}"))?;
                Ok(LogPage {
                    matched: table.matched,
                    rows: table
                        .rows
                        .into_iter()
                        .map(|(t, l, m)| (t, l, self.redact_log(&m)))
                        .collect(),
                })
            }
            ResponseBody::Text { .. } => bail!(
                "mylar /getLog returned HTML (the forms login page): the web UI uses forms \
                 login (authentication=2), which needs a cookie session the toolkit HTTP \
                 client cannot establish"
            ),
        }
    }

    pub async fn config_update(&self, fields: Vec<(String, String)>) -> Result<()> {
        let what = "mylar /configUpdate";
        let pairs: Vec<(&str, &str)> = fields
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let req = self
            .http
            .post(format!("{}/configUpdate", self.base))
            .header(
                "content-type",
                HeaderValue::from_static("application/x-www-form-urlencoded"),
            )
            .body(form_encode(&pairs));
        let (status, body) = self.send(self.web(req)?, what).await?;
        match self.success(what, status, body)? {
            ResponseBody::Text { text } if text.trim().is_empty() => Ok(()),
            ResponseBody::Text { text } => bail!(
                "mylar /configUpdate: unexpected reply: {}",
                clip(&self.redact(&text))
            ),
            ResponseBody::Json { json } => bail!(
                "mylar /configUpdate: unexpected reply: {}",
                clip(&self.redact(&json.to_string()))
            ),
        }
    }

    fn transport(&self, what: &str, e: &str) -> plugin_toolkit::anyhow::Error {
        anyhow!("{what}: {}", self.redact(e))
    }

    fn redact_log(&self, msg: &str) -> String {
        LOG_SECRET_MARKERS
            .iter()
            .fold(self.redact(msg), |out, marker| redact_query(&out, marker))
    }

    fn redact(&self, msg: &str) -> String {
        let mut secrets = vec![self.api_key.clone()];
        if let Some((user, pass)) = &self.web_auth {
            secrets.push(format!(
                "{}:{}@",
                plugin_toolkit::url::encode(user),
                plugin_toolkit::url::encode(pass)
            ));
            secrets.push(pass.clone());
        }
        let mut out = msg.to_string();
        for s in secrets.iter().filter(|s| !s.is_empty()) {
            out = out.replace(s.as_str(), scrub::REDACTED_TEXT);
            out = out.replace(&plugin_toolkit::url::encode(s), scrub::REDACTED_TEXT);
        }
        redact_query(&out, "apikey=")
    }
}

enum ResponseBody {
    Json { json: serde_json::Value },
    Text { text: String },
}

impl ResponseBody {
    fn from_bytes(b: &[u8]) -> Self {
        match serde_json::from_slice(b) {
            Ok(json) if !b.is_empty() => ResponseBody::Json { json },
            _ => ResponseBody::Text {
                text: String::from_utf8_lossy(b).into_owned(),
            },
        }
    }

    fn text(&self) -> String {
        match self {
            ResponseBody::Json { json } => json.to_string(),
            ResponseBody::Text { text } => text.clone(),
        }
    }
}

fn form_encode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                plugin_toolkit::url::encode(k),
                plugin_toolkit::url::encode(v)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

const LOG_SECRET_MARKERS: &[&str] = &[
    "apikey': '",
    "api_key': '",
    "apikey\": \"",
    "api_key\": \"",
    "apikey\":\"",
    "api_key\":\"",
    "apikey':'",
    "api_key':'",
    "api_key=",
    "apikey%3D",
    "apikey%3d",
    "api_key%3D",
    "api_key%3d",
    "password': '",
    "password\": \"",
    "password=",
    "token': '",
    "token=",
];

pub(crate) fn redact_query(msg: &str, marker: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut rest = msg;
    while let Some(i) = rest.find(marker) {
        let (head, tail) = rest.split_at(i + marker.len());
        out.push_str(head);
        let end = tail
            .find(|c: char| matches!(c, '&' | ')' | '"' | '\'') || c.is_whitespace())
            .unwrap_or(tail.len());
        if end > 0 {
            out.push_str(scrub::REDACTED_TEXT);
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

pub(crate) fn clip(s: &str) -> String {
    let s = s.trim();
    match s.char_indices().nth(200) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::matchers::{header, method, path, query_param};
    use crate::mock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> Mylar {
        Mylar::new(&format!("{}/", server.uri()), "KEY", None)
    }

    #[test]
    fn index_unwraps_success_envelope() {
        crate::mock::run(async {
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
        })
    }

    #[test]
    fn wanted_reads_bare_payload() {
        crate::mock::run(async {
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
        })
    }

    #[test]
    fn refusal_surfaces_mylar_message() {
        crate::mock::run(async {
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
        })
    }

    #[test]
    fn config_reads_settings_table_with_basic_auth() {
        crate::mock::run(async {
            let server = MockServer::start().await;
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
        })
    }

    #[test]
    fn config_names_forms_login() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/getConfig"))
                .respond_with(ResponseTemplate::new(200).set_body_string("<html>login</html>"))
                .mount(&server)
                .await;
            let err = client(&server).config().await.unwrap_err().to_string();
            assert!(err.contains("forms login"), "{err}");
        })
    }

    #[test]
    fn config_names_basic_auth_on_401() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/getConfig"))
                .respond_with(ResponseTemplate::new(401))
                .mount(&server)
                .await;
            let err = client(&server).config().await.unwrap_err().to_string();
            assert!(err.contains("web_username"), "{err}");
        })
    }

    #[test]
    fn force_process_sends_manual_run_and_checks_ack() {
        crate::mock::run(async {
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
        })
    }

    #[test]
    fn transport_errors_hide_the_api_key_and_web_login() {
        crate::mock::run(async {
            let m = Mylar::new(
                "http://127.0.0.1:1",
                "SECRETKEY123",
                Some(("admin".into(), "hunter2!".into())),
            );
            let err = format!("{:#}", m.index().await.unwrap_err());
            assert!(!err.contains("SECRETKEY123"), "{err}");
            let err = format!("{:#}", m.config().await.unwrap_err());
            assert!(!err.contains("hunter2"), "{err}");
            let err = format!("{:#}", m.config_update(vec![]).await.unwrap_err());
            assert!(!err.contains("hunter2"), "{err}");
        })
    }

    #[test]
    fn reply_text_is_redacted() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api"))
                .and(query_param("cmd", "getIndex"))
                .respond_with(ResponseTemplate::new(200).set_body_string("bad key KEY"))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api"))
                .and(query_param("cmd", "forceProcess"))
                .respond_with(ResponseTemplate::new(200).set_body_string("?apikey=KEY failed"))
                .mount(&server)
                .await;
            let m = client(&server);
            let err = m.index().await.unwrap_err().to_string();
            assert!(err.contains("bad key ***") && !err.contains("KEY"), "{err}");
            let err = m.force_process("/d").await.unwrap_err().to_string();
            assert!(!err.contains("KEY"), "{err}");
        })
    }

    #[test]
    fn log_lines_hide_keys_mylar_echoes() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/getLog"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "iTotalDisplayRecords": 1,
                    "aaData": [["t", "INFO", "sendtosab: {'output': 'json', 'apikey': 'SABSECRET', 'password': 'pw1'} url?apikey=KEY&x=1"]]
                })))
                .mount(&server)
                .await;
            let page = client(&server).log("", 0, 10).await.unwrap();
            let msg = &page.rows[0].2;
            assert!(
                !msg.contains("SABSECRET") && !msg.contains("pw1") && !msg.contains("KEY"),
                "{msg}"
            );
            assert!(msg.contains("'apikey': '***'"), "{msg}");
        })
    }

    #[test]
    fn redact_query_blanks_every_apikey() {
        assert_eq!(
            redact_query("GET /api?apikey=abc&cmd=x (apikey=def)", "apikey="),
            "GET /api?apikey=***&cmd=x (apikey=***)"
        );
    }

    #[test]
    fn config_refuses_a_key_listed_twice() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/getConfig"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "aaData": [["SAB_HOST", "a"], ["sab_host", "b"]]
                })))
                .mount(&server)
                .await;
            let err = client(&server).config().await.unwrap_err().to_string();
            assert!(err.contains("'sab_host' twice"), "{err}");
        })
    }

    #[test]
    fn config_update_rejects_a_non_empty_reply() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/configUpdate"))
                .respond_with(ResponseTemplate::new(200).set_body_string("<html>login</html>"))
                .mount(&server)
                .await;
            let err = client(&server)
                .config_update(vec![("a".into(), "b".into())])
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("unexpected reply"), "{err}");
        })
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

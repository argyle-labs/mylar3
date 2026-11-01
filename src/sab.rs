use plugin_toolkit::prelude::*;
use plugin_toolkit::reqwest::Client as HttpClient;
use plugin_toolkit::scrub;
use plugin_toolkit::serde_json;

use crate::api::{clip, redact_query};

pub const PAGE: usize = 500;
pub const MAX_PAGES: usize = 40;

pub struct Sab {
    http: HttpClient,
    base: String,
    api_key: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
pub struct HistorySlot {
    #[serde(default)]
    pub nzo_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, alias = "filename")]
    pub nzb_name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub fail_message: Option<String>,
    #[serde(default)]
    pub storage: Option<String>,
    #[serde(default, alias = "cat")]
    pub category: Option<String>,
    #[serde(default)]
    pub completed: Option<i64>,
}

#[derive(Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
struct Reply {
    history: Option<History>,
    queue: Option<History>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
struct History {
    #[serde(default)]
    noofslots: usize,
    #[serde(default)]
    slots: Vec<HistorySlot>,
}

impl Sab {
    pub fn new(host: &str, api_key: &str) -> Self {
        let host = host.trim().trim_end_matches('/');
        let base = if host.contains("://") {
            host.to_string()
        } else {
            format!("http://{host}")
        };
        Self {
            http: HttpClient::new(),
            base,
            api_key: api_key.to_string(),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    async fn page(&self, mode: &str, start: usize) -> Result<History> {
        let start = start.to_string();
        let limit = PAGE.to_string();
        let query = [
            ("mode", mode),
            ("output", "json"),
            ("start", start.as_str()),
            ("limit", limit.as_str()),
            ("apikey", self.api_key.as_str()),
        ]
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                plugin_toolkit::url::encode(k),
                plugin_toolkit::url::encode(v)
            )
        })
        .collect::<Vec<_>>()
        .join("&");
        let resp = self
            .http
            .get(format!("{}/api?{query}", self.base))
            .send()
            .await
            .map_err(|e| anyhow!("sab {mode}: {}", self.redact(&e.to_string())))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| anyhow!("sab {mode}: {}", self.redact(&e.to_string())))?;
        if !(200..300).contains(&status) {
            bail!(
                "sab {mode}: HTTP {status}: {}",
                clip(&self.redact(&String::from_utf8_lossy(&bytes)))
            );
        }
        let reply: Reply = serde_json::from_slice(&bytes).map_err(|_| {
            anyhow!(
                "sab {mode}: non-JSON reply: {}",
                clip(&self.redact(&String::from_utf8_lossy(&bytes)))
            )
        })?;
        let body = if mode == "queue" {
            reply.queue
        } else {
            reply.history
        };
        match (body, reply.error) {
            (_, Some(e)) => bail!("sab {mode} refused: {}", self.redact(&e)),
            (Some(h), None) => Ok(h),
            (None, None) => bail!("sab {mode}: reply has no {mode}"),
        }
    }

    /// History across every category, so jobs filed under another category still match.
    pub async fn history(&self) -> Result<Vec<HistorySlot>> {
        self.all("history").await
    }

    pub async fn queue(&self) -> Result<Vec<HistorySlot>> {
        self.all("queue").await
    }

    async fn all(&self, mode: &str) -> Result<Vec<HistorySlot>> {
        let mut out = Vec::new();
        for n in 0..MAX_PAGES {
            let page = self.page(mode, n * PAGE).await?;
            let got = page.slots.len();
            let total = page.noofslots;
            out.extend(page.slots);
            if out.len() >= total {
                return Ok(out);
            }
            if got < PAGE {
                bail!(
                    "sab {mode}: read {} of {total} slots, SAB ended early",
                    out.len()
                );
            }
        }
        bail!(
            "sab {mode}: more than {} slots exceeds the {MAX_PAGES}-page cap; refusing a partial {mode}",
            MAX_PAGES * PAGE
        )
    }

    fn redact(&self, msg: &str) -> String {
        let mut out = msg.to_string();
        if !self.api_key.is_empty() {
            out = out.replace(self.api_key.as_str(), scrub::REDACTED_TEXT);
            out = out.replace(
                &plugin_toolkit::url::encode(&self.api_key),
                scrub::REDACTED_TEXT,
            );
        }
        redact_query(&out, "apikey=")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::matchers::{method, path, query_param};
    use crate::mock::{Mock, MockServer, ResponseTemplate};

    fn slots(from: usize, n: usize) -> Vec<serde_json::Value> {
        (from..from + n)
            .map(|i| serde_json::json!({"nzo_id": format!("SABnzbd_nzo_{i}"), "name": format!("Job {i}")}))
            .collect()
    }

    #[test]
    fn history_pages_until_noofslots() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            for start in [0, PAGE] {
                let n = if start == 0 { PAGE } else { 3 };
                Mock::given(method("GET"))
                    .and(path("/api"))
                    .and(query_param("mode", "history"))
                    .and(query_param("apikey", "SABKEY"))
                    .and(query_param("start", &start.to_string()))
                    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "history": {"noofslots": PAGE + 3, "slots": slots(start, n)}
                    })))
                    .mount(&server)
                    .await;
            }
            let sab = Sab::new(&server.uri(), "SABKEY");
            let h = sab.history().await.unwrap();
            assert_eq!(h.len(), PAGE + 3);
            assert_eq!(server.received_requests().await.unwrap().len(), 2);
        })
    }

    #[test]
    fn history_over_the_cap_errors() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api"))
                .and(query_param("mode", "history"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "history": {"noofslots": MAX_PAGES * PAGE + 1, "slots": slots(0, PAGE)}
                })))
                .mount(&server)
                .await;
            let err = Sab::new(&server.uri(), "SABKEY")
                .history()
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("cap"), "{err}");
            assert_eq!(server.received_requests().await.unwrap().len(), MAX_PAGES);
        })
    }

    #[test]
    fn queue_short_of_noofslots_errors() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api"))
                .and(query_param("mode", "queue"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "queue": {"noofslots": 10, "slots": slots(0, 3)}
                })))
                .mount(&server)
                .await;
            let err = Sab::new(&server.uri(), "SABKEY")
                .queue()
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("3 of 10"), "{err}");
        })
    }

    #[test]
    fn history_spans_categories_and_queue_reads_filenames() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api"))
                .and(query_param("mode", "queue"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "queue": {"noofslots": 1, "slots": [
                        {"nzo_id": "q", "filename": "Saga 001", "status": "Downloading", "cat": "comics"}
                    ]}
                })))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api"))
                .and(query_param("mode", "history"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "history": {"noofslots": 0, "slots": []}
                })))
                .mount(&server)
                .await;
            let sab = Sab::new(&server.uri(), "SABKEY");
            let q = sab.queue().await.unwrap();
            assert_eq!(q[0].nzb_name.as_deref(), Some("Saga 001"));
            sab.history().await.unwrap();
            let reqs = server.received_requests().await.unwrap();
            assert!(reqs
                .iter()
                .all(|r| !r.url.query().unwrap_or_default().contains("cat=")));
        })
    }

    #[test]
    fn host_without_scheme_gets_http() {
        assert_eq!(
            Sab::new("10.0.0.5:8080/", "k").base(),
            "http://10.0.0.5:8080"
        );
        assert_eq!(Sab::new("https://sab.lan", "k").base(), "https://sab.lan");
    }

    #[test]
    fn errors_never_carry_the_key() {
        crate::mock::run(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({"error": "API Key Incorrect SABKEY"})),
                )
                .mount(&server)
                .await;
            let err = Sab::new(&server.uri(), "SABKEY")
                .history()
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("refused") && !err.contains("SABKEY"), "{err}");
            let err = format!(
                "{:#}",
                Sab::new("http://127.0.0.1:1", "SABKEY")
                    .history()
                    .await
                    .unwrap_err()
            );
            assert!(!err.contains("SABKEY"), "{err}");
        })
    }
}

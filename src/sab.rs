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
    #[serde(default)]
    pub nzb_name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub fail_message: Option<String>,
    #[serde(default)]
    pub storage: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub completed: Option<i64>,
}

#[derive(Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
struct HistoryReply {
    history: Option<History>,
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

    async fn page(&self, category: &str, start: usize) -> Result<History> {
        let start = start.to_string();
        let limit = PAGE.to_string();
        let query = [
            ("mode", "history"),
            ("output", "json"),
            ("cat", category),
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
            .map_err(|e| anyhow!("sab history: {}", self.redact(&e.to_string())))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| anyhow!("sab history: {}", self.redact(&e.to_string())))?;
        if !(200..300).contains(&status) {
            bail!(
                "sab history: HTTP {status}: {}",
                clip(&self.redact(&String::from_utf8_lossy(&bytes)))
            );
        }
        let reply: HistoryReply = serde_json::from_slice(&bytes).map_err(|_| {
            anyhow!(
                "sab history: non-JSON reply: {}",
                clip(&self.redact(&String::from_utf8_lossy(&bytes)))
            )
        })?;
        match (reply.history, reply.error) {
            (_, Some(e)) => bail!("sab history refused: {}", self.redact(&e)),
            (Some(h), None) => Ok(h),
            (None, None) => bail!("sab history: reply has no history"),
        }
    }

    pub async fn history(&self, category: &str) -> Result<Vec<HistorySlot>> {
        let mut out = Vec::new();
        for n in 0..MAX_PAGES {
            let page = self.page(category, n * PAGE).await?;
            let got = page.slots.len();
            out.extend(page.slots);
            if got < PAGE || out.len() >= page.noofslots {
                break;
            }
        }
        Ok(out)
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
                    .and(query_param("cat", "comics"))
                    .and(query_param("apikey", "SABKEY"))
                    .and(query_param("start", &start.to_string()))
                    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "history": {"noofslots": PAGE + 3, "slots": slots(start, n)}
                    })))
                    .mount(&server)
                    .await;
            }
            let sab = Sab::new(&server.uri(), "SABKEY");
            let h = sab.history("comics").await.unwrap();
            assert_eq!(h.len(), PAGE + 3);
            assert_eq!(server.received_requests().await.unwrap().len(), 2);
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
                .history("comics")
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("refused") && !err.contains("SABKEY"), "{err}");
            let err = format!(
                "{:#}",
                Sab::new("http://127.0.0.1:1", "SABKEY")
                    .history("comics")
                    .await
                    .unwrap_err()
            );
            assert!(!err.contains("SABKEY"), "{err}");
        })
    }
}

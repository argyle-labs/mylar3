//! Shared test fixtures: a full v0.8.3 settings table and a mock Mylar.

use plugin_toolkit::serde_json::{json, Value};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::api::{ConfigIni, Mylar};
use crate::write::{SettingChange, FORM_CHECKBOXES};

/// A full, non-minimal settings table: every checkbox, two newznabs, one
/// torznab, a library and download folders.
pub(crate) fn table(overrides: &[(&str, &str)]) -> Vec<(String, String)> {
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
        ("provider_order", "0, NZBGeek"),
    ] {
        set(&mut rows, k, v);
    }
    for (k, v) in overrides {
        set(&mut rows, k, v);
    }
    rows
}

pub(crate) fn set(rows: &mut Vec<(String, String)>, k: &str, v: &str) {
    match rows.iter_mut().find(|(key, _)| key == k) {
        Some(row) => row.1 = v.to_string(),
        None => rows.push((k.to_string(), v.to_string())),
    }
}

pub(crate) fn unset(rows: &mut Vec<(String, String)>, k: &str) {
    rows.retain(|(key, _)| key != k);
}

pub(crate) fn ini(rows: &[(String, String)]) -> ConfigIni {
    ConfigIni(rows.iter().cloned().collect())
}

pub(crate) fn json_rows(rows: &[(String, String)]) -> Value {
    json!({ "aaData": rows.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>() })
}

/// Serve `reads` as successive `/getConfig` replies; the last repeats.
pub(crate) async fn server(
    reads: &[Vec<(String, String)>],
    update: ResponseTemplate,
) -> MockServer {
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
        .respond_with(
            ResponseTemplate::new(200).set_body_string(
                "Successfully submitted request for post-processing for Manual Run",
            ),
        )
        .mount(&server)
        .await;
    server
}

pub(crate) fn ok() -> ResponseTemplate {
    ResponseTemplate::new(200)
}

pub(crate) fn mylar(server: &MockServer) -> Mylar {
    Mylar::new(&server.uri(), "KEY", None)
}

pub(crate) async fn writes(server: &MockServer) -> usize {
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

pub(crate) async fn posted_form(server: &MockServer) -> Vec<(String, String)> {
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

pub(crate) fn change(key: &str, target: &str) -> SettingChange {
    SettingChange {
        key: key.into(),
        current: None,
        target: target.into(),
        reason: String::new(),
    }
}

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;

use plugin_toolkit::abi::{HttpRequest, HttpResponse};
use plugin_toolkit::serde_json::{self, Value};
use url::Url;

thread_local! {
    static SERVERS: RefCell<Vec<Rc<RefCell<State>>>> = const { RefCell::new(Vec::new()) };
}

pub fn run<T>(fut: impl Future<Output = T>) -> T {
    plugin_toolkit::capsink::with_cap_sink(
        Box::new(|cap: &str, req: &str| {
            assert_eq!(cap, "http.request");
            let req: HttpRequest = serde_json::from_str(req).map_err(|e| e.to_string())?;
            let resp = dispatch(req)?;
            serde_json::to_string(&resp).map_err(|e| e.to_string())
        }),
        || {
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap()
                .block_on(fut)
        },
    )
}

fn dispatch(req: HttpRequest) -> Result<HttpResponse, String> {
    let url = Url::parse(&req.url).map_err(|e| e.to_string())?;
    let server = SERVERS.with(|s| {
        s.borrow()
            .iter()
            .find(|srv| Some(srv.borrow().host.as_str()) == url.host_str())
            .cloned()
    });
    let Some(server) = server else {
        return Err(format!("error sending request for url ({})", req.url));
    };
    let request = Request {
        method: req.method,
        url,
        headers: req.headers,
        body: req.body,
    };
    let mut state = server.borrow_mut();
    let hit = state
        .mocks
        .iter_mut()
        .find(|m| m.remaining != Some(0) && m.matchers.iter().all(|x| x.matches(&request)));
    let resp = match hit {
        Some(m) => {
            if let Some(n) = &mut m.remaining {
                *n -= 1;
            }
            HttpResponse {
                status: m.response.status,
                headers: Vec::new(),
                body: m.response.body.clone(),
            }
        }
        None => HttpResponse {
            status: 404,
            ..Default::default()
        },
    };
    state.received.push(request);
    Ok(resp)
}

struct State {
    host: String,
    mocks: Vec<Mock>,
    received: Vec<Request>,
}

#[derive(Clone)]
pub struct Request {
    pub method: String,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub struct MockServer(Rc<RefCell<State>>);

impl MockServer {
    pub async fn start() -> Self {
        let state = SERVERS.with(|s| {
            let mut s = s.borrow_mut();
            let state = Rc::new(RefCell::new(State {
                host: format!("mock{}.test", s.len()),
                mocks: Vec::new(),
                received: Vec::new(),
            }));
            s.push(state.clone());
            state
        });
        MockServer(state)
    }

    pub fn uri(&self) -> String {
        format!("http://{}", self.0.borrow().host)
    }

    pub async fn received_requests(&self) -> Option<Vec<Request>> {
        Some(self.0.borrow().received.clone())
    }
}

pub enum Matcher {
    Method(String),
    Path(String),
    Query(String, String),
    Header(String, String),
}

impl Matcher {
    fn matches(&self, r: &Request) -> bool {
        match self {
            Matcher::Method(m) => r.method.eq_ignore_ascii_case(m),
            Matcher::Path(p) => r.url.path() == p,
            Matcher::Query(k, v) => r.url.query_pairs().any(|(a, b)| a == *k && b == *v),
            Matcher::Header(k, v) => r
                .headers
                .iter()
                .any(|(a, b)| a.eq_ignore_ascii_case(k) && b == v),
        }
    }
}

pub mod matchers {
    use super::Matcher;

    pub fn method(m: &str) -> Matcher {
        Matcher::Method(m.into())
    }

    pub fn path(p: &str) -> Matcher {
        Matcher::Path(p.into())
    }

    pub fn query_param(k: &str, v: &str) -> Matcher {
        Matcher::Query(k.into(), v.into())
    }

    pub fn header(k: &str, v: &str) -> Matcher {
        Matcher::Header(k.into(), v.into())
    }
}

#[derive(Clone)]
pub struct ResponseTemplate {
    status: u16,
    body: Vec<u8>,
}

impl ResponseTemplate {
    pub fn new(status: u16) -> Self {
        Self {
            status,
            body: Vec::new(),
        }
    }

    pub fn set_body_json(mut self, json: Value) -> Self {
        self.body = json.to_string().into_bytes();
        self
    }

    pub fn set_body_string(mut self, s: impl Into<String>) -> Self {
        self.body = s.into().into_bytes();
        self
    }
}

pub struct Mock {
    matchers: Vec<Matcher>,
    response: ResponseTemplate,
    remaining: Option<usize>,
}

impl Mock {
    pub fn given(m: Matcher) -> MockBuilder {
        MockBuilder(vec![m])
    }

    pub fn up_to_n_times(mut self, n: usize) -> Self {
        self.remaining = Some(n);
        self
    }

    pub async fn mount(self, server: &MockServer) {
        server.0.borrow_mut().mocks.push(self);
    }
}

pub struct MockBuilder(Vec<Matcher>);

impl MockBuilder {
    pub fn and(mut self, m: Matcher) -> Self {
        self.0.push(m);
        self
    }

    pub fn respond_with(self, response: ResponseTemplate) -> Mock {
        Mock {
            matchers: self.0,
            response,
            remaining: None,
        }
    }
}

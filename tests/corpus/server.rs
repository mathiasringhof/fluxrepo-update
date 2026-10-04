use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use reqwest::blocking::{Client, ClientBuilder};
use serde_json::Value;
use url::Url;

pub struct FixtureServer {
    origin: String,
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<Vec<String>>>,
}

impl FixtureServer {
    pub fn new(responses: &Value) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").context("bind corpus HTTP fixture")?;
        listener.set_nonblocking(true)?;
        let origin = format!("http://{}", listener.local_addr()?);
        let responses = responses
            .as_object()
            .context("fixture responses must be an object")?
            .iter()
            .map(|(path, spec)| {
                if !path.starts_with('/') {
                    bail!("fixture response route must start with '/': {path}");
                }
                Ok((path.clone(), prepare_response(spec, &origin)?))
            })
            .collect::<Result<HashMap<_, _>>>()?;
        let stopped = Arc::new(AtomicBool::new(false));
        let fixture_stop = Arc::clone(&stopped);
        let fixture_origin = origin.clone();
        let thread = thread::spawn(move || {
            serve(
                listener,
                Arc::new(responses),
                &fixture_origin,
                &fixture_stop,
            )
        });
        Ok(Self {
            origin,
            stopped,
            thread: Some(thread),
        })
    }

    pub fn expand(&self, text: &str) -> String {
        replace_placeholders(text, &self.origin)
    }

    pub fn client(&self) -> Result<Client> {
        Ok(self.client_builder()?.build()?)
    }

    pub fn client_builder(&self) -> Result<ClientBuilder> {
        Ok(Client::builder()
            .no_proxy()
            .proxy(reqwest::Proxy::all(&self.origin)?)
            .timeout(Duration::from_secs(5)))
    }

    pub fn finish(mut self) -> Result<()> {
        self.stop_and_join()
    }

    fn stop_and_join(&mut self) -> Result<()> {
        self.stopped.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let errors = thread
                .join()
                .map_err(|_| anyhow::anyhow!("corpus HTTP fixture thread panicked"))?;
            if !errors.is_empty() {
                bail!("corpus HTTP fixture errors:\n{}", errors.join("\n"));
            }
        }
        Ok(())
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

fn replace_placeholders(text: &str, origin: &str) -> String {
    text.replace("{{server}}", origin)
        .replace("{{registry}}", origin.trim_start_matches("http://"))
}

fn prepare_response(spec: &Value, origin: &str) -> Result<Vec<u8>> {
    let spec = spec
        .as_object()
        .context("fixture response must be an object")?;
    let status = spec
        .get("status")
        .map_or(Some(200), Value::as_u64)
        .filter(|status| (100..=599).contains(status))
        .context("fixture status must be an integer from 100 to 599")?;
    if spec.contains_key("json") && spec.contains_key("text") {
        bail!("fixture response must choose json or text");
    }
    let body = if let Some(json) = spec.get("json") {
        replace_placeholders(&json.to_string(), origin)
    } else {
        replace_placeholders(
            spec.get("text")
                .map_or(Some(""), Value::as_str)
                .context("fixture text must be a string")?,
            origin,
        )
    };
    let mut response = format!(
        "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(headers) = spec.get("headers") {
        for (name, value) in headers
            .as_object()
            .context("fixture headers must be an object")?
        {
            reqwest::header::HeaderName::from_bytes(name.as_bytes())?;
            let value = replace_placeholders(
                value
                    .as_str()
                    .context("fixture header values must be strings")?,
                origin,
            );
            reqwest::header::HeaderValue::from_str(&value)?;
            writeln!(response, "{name}: {value}\r")?;
        }
    }
    response.push_str("\r\n");
    response.push_str(&body);
    Ok(response.into_bytes())
}

fn serve(
    listener: TcpListener,
    responses: Arc<HashMap<String, Vec<u8>>>,
    origin: &str,
    stopped: &Arc<AtomicBool>,
) -> Vec<String> {
    let mut workers = Vec::new();
    let mut errors = Vec::new();
    while !stopped.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let responses = Arc::clone(&responses);
                let stopped = Arc::clone(stopped);
                let origin = origin.to_owned();
                workers.push(thread::spawn(move || {
                    serve_connection(stream, &responses, &origin, &stopped)
                }));
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => {
                errors.push(format!("accept corpus HTTP request: {error}"));
                break;
            }
        }
    }
    drop(listener);
    for worker in workers {
        match worker.join() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => errors.push(format!("{error:#}")),
            Err(_) => errors.push("corpus HTTP request handler panicked".to_owned()),
        }
    }
    errors
}

fn serve_connection(
    mut stream: TcpStream,
    responses: &HashMap<String, Vec<u8>>,
    origin: &str,
    stopped: &AtomicBool,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(100)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    let result = (|| {
        let Some(headers) = read_headers(&mut stream, stopped)? else {
            return Ok(());
        };
        let response = response_for(&headers, responses, origin)?;
        stream
            .write_all(response)
            .context("write corpus HTTP response")
    })();
    if result.is_err() {
        let _ = stream.write_all(
            b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
    }
    result
}

fn read_headers(stream: &mut TcpStream, stopped: &AtomicBool) -> Result<Option<String>> {
    const HEADER_LIMIT: usize = 32 * 1024;
    let started = Instant::now();
    let mut headers = Vec::new();
    let mut buffer = [0; 1024];
    while !stopped.load(Ordering::Relaxed) {
        if started.elapsed() >= Duration::from_secs(2) {
            bail!("corpus HTTP request headers timed out");
        }
        match stream.read(&mut buffer) {
            Ok(0) => bail!("incomplete corpus HTTP request headers"),
            Ok(size) => {
                headers.extend_from_slice(&buffer[..size]);
                if let Some(end) = headers.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    if end + 4 > HEADER_LIMIT {
                        bail!("corpus HTTP request headers exceeded {HEADER_LIMIT} bytes");
                    }
                    headers.truncate(end);
                    return Ok(Some(String::from_utf8(headers)?));
                }
                if headers.len() > HEADER_LIMIT {
                    bail!("corpus HTTP request headers exceeded {HEADER_LIMIT} bytes");
                }
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(error) => return Err(error).context("read corpus HTTP request headers"),
        }
    }
    Ok(None)
}

fn response_for<'a>(
    headers: &str,
    responses: &'a HashMap<String, Vec<u8>>,
    origin: &str,
) -> Result<&'a [u8]> {
    let mut lines = headers.split("\r\n");
    let request = lines.next().context("missing corpus HTTP request line")?;
    let parts: Vec<_> = request.split_whitespace().collect();
    let [method, target, version] = parts.as_slice() else {
        bail!("invalid corpus HTTP request: {request}");
    };
    if *method == "CONNECT" {
        bail!("external HTTPS request: {target}");
    }
    if *method != "GET" {
        bail!("unexpected HTTP method: {method} {target}");
    }
    if !matches!(*version, "HTTP/1.0" | "HTTP/1.1") {
        bail!("invalid corpus HTTP version: {version}");
    }
    let path = if target.starts_with('/') {
        (*target).to_owned()
    } else {
        let url = Url::parse(target).context("invalid corpus HTTP request URL")?;
        if url.origin().ascii_serialization() != origin
            || !url.username().is_empty()
            || url.password().is_some()
        {
            bail!("external HTTP request: {target}");
        }
        url.query().map_or_else(
            || url.path().to_owned(),
            |query| format!("{}?{query}", url.path()),
        )
    };
    let hosts: Vec<_> = lines
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.eq_ignore_ascii_case("host"))
        .map(|(_, value)| value.trim())
        .collect();
    if hosts != [origin.trim_start_matches("http://")] {
        bail!("external HTTP request Host: {hosts:?} {target}");
    }
    let pathname = path.split_once('?').map_or(path.as_str(), |(path, _)| path);
    responses
        .get(&path)
        .or_else(|| responses.get(pathname))
        .map(Vec::as_slice)
        .with_context(|| format!("unexpected HTTP request: {path}"))
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::TcpStream;
    use std::thread;
    use std::time::{Duration, Instant};

    use reqwest::StatusCode;
    use serde_json::json;

    use super::FixtureServer;

    #[test]
    fn repeated_concurrent_requests_use_paths_and_expand_responses() {
        let server = FixtureServer::new(&json!({
            "/first": {"json": {"origin": "{{server}}", "registry": "{{registry}}"}},
            "/second": {"text": "second", "headers": {"X-Fixture": "{{registry}}"}},
            "/second?exact=1": {"status": 201, "text": "exact"}
        }))
        .expect("start fixture");
        let client = server.client().expect("fixture client");
        let origin = server.expand("{{server}}");
        let registry = server.expand("{{registry}}");

        thread::scope(|scope| {
            let requests: Vec<_> = ["/second?fallback=1", "/first", "/second", "/first"]
                .into_iter()
                .map(|path| {
                    let client = &client;
                    let origin = &origin;
                    let registry = &registry;
                    scope.spawn(move || {
                        let response = client.get(format!("{origin}{path}")).send().expect("GET");
                        assert_eq!(response.status(), StatusCode::OK);
                        if path == "/first" {
                            assert_eq!(
                                response.json::<serde_json::Value>().expect("JSON body"),
                                json!({"origin": origin, "registry": registry})
                            );
                        } else {
                            assert_eq!(response.headers()["X-Fixture"], registry);
                            assert_eq!(response.text().expect("text body"), "second");
                        }
                    })
                })
                .collect();
            for request in requests {
                request.join().expect("request completed");
            }
        });

        let response = client
            .get(server.expand("{{server}}/second?exact=1"))
            .send()
            .expect("exact query route");
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(response.text().expect("exact body"), "exact");
        server.finish().expect("clean fixture shutdown");
    }

    #[test]
    fn unknown_route_is_reported_when_finishing() {
        let server = FixtureServer::new(&json!({})).expect("start fixture");
        let response = server
            .client()
            .expect("fixture client")
            .get(server.expand("{{server}}/unknown"))
            .send()
            .expect("fixture rejection response");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let error = server
            .finish()
            .expect_err("unknown route must fail fixture");
        assert!(error.to_string().contains("/unknown"), "{error:#}");
    }

    #[test]
    fn custom_redirect_policy_preserves_the_fixture_proxy() {
        let server = FixtureServer::new(&json!({
            "/redirect": {"status": 302, "headers": {"Location": "{{server}}/target"}},
            "/target": {"text": "followed"}
        }))
        .expect("start fixture");
        let client = server
            .client_builder()
            .expect("fixture client builder")
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("custom fixture client");
        let response = client
            .get(server.expand("{{server}}/redirect"))
            .send()
            .expect("redirect response");
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(response.url().path(), "/redirect");

        let response = server
            .client()
            .expect("default fixture client")
            .get(server.expand("{{server}}/redirect"))
            .send()
            .expect("follow local redirect");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.url().path(), "/target");
        assert_eq!(response.text().expect("redirect target body"), "followed");

        let response = client
            .get("http://external-custom-client.invalid/target")
            .send()
            .expect("custom client proxy rejection");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let error = server
            .finish()
            .expect_err("external request must fail fixture");
        assert!(
            error.to_string().contains("external-custom-client.invalid"),
            "{error:#}"
        );
    }

    #[test]
    fn external_http_and_https_are_trapped_by_the_fixture_proxy() {
        let server =
            FixtureServer::new(&json!({"/allowed": {"text": "local"}})).expect("start fixture");
        let client = server.client().expect("fixture client");
        let response = client
            .get("http://external-http.invalid/allowed")
            .send()
            .expect("HTTP proxy rejection");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(
            client
                .get("https://external-https.invalid/allowed")
                .send()
                .is_err()
        );
        let error = server
            .finish()
            .expect_err("external requests must fail fixture");
        let message = error.to_string();
        assert!(message.contains("external-http.invalid"), "{message}");
        assert!(message.contains("external-https.invalid"), "{message}");
    }

    #[test]
    fn non_get_requests_are_reported() {
        let server =
            FixtureServer::new(&json!({"/allowed": {"text": "local"}})).expect("start fixture");
        let response = server
            .client()
            .expect("fixture client")
            .post(server.expand("{{server}}/allowed"))
            .send()
            .expect("fixture rejection response");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let error = server.finish().expect_err("POST must fail fixture");
        assert!(error.to_string().contains("POST"), "{error:#}");
    }

    #[test]
    fn finish_closes_the_listener_and_drop_joins_during_unwind() {
        let server = FixtureServer::new(&json!({})).expect("start fixture");
        let address = server.expand("{{registry}}");
        server.finish().expect("idle fixture finishes");
        assert!(TcpStream::connect(&address).is_err());

        let server = FixtureServer::new(&json!({})).expect("start fixture");
        let address = server.expand("{{registry}}");
        let mut pending = TcpStream::connect(&address).expect("pending connection");
        pending
            .write_all(b"GET / HTTP/1.1\r\n")
            .expect("incomplete headers");
        let started = Instant::now();
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _fixture = server;
            panic!("synthetic test unwind");
        }));
        assert!(unwind.is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(TcpStream::connect(&address).is_err());
    }
}

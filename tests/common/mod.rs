#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tempfile::TempDir;

use fluxrepo_update::cli::ResolverFactory;
use fluxrepo_update::resolvers::{
    ChartVersionResolver, ImageVersionResolver, StaticImageVersionResolver, StaticVersionResolver,
};

#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

pub struct ResponseSpec {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl ResponseSpec {
    pub fn new(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

pub struct TestHttpServer {
    pub base_url: String,
    pub requests: Arc<Mutex<Vec<RecordedRequest>>>,
    handle: Option<JoinHandle<()>>,
}

impl TestHttpServer {
    pub fn new(responses: Vec<ResponseSpec>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("server address");
        let base_url = format!("http://{address}");
        let host = address.to_string();
        let responses = responses
            .into_iter()
            .map(|mut response| {
                response.body = response
                    .body
                    .replace("{base_url}", &base_url)
                    .replace("{host}", &host);
                response.headers = response
                    .headers
                    .into_iter()
                    .map(|(name, value)| {
                        (
                            name,
                            value
                                .replace("{base_url}", &base_url)
                                .replace("{host}", &host),
                        )
                    })
                    .collect();
                response
            })
            .collect::<Vec<_>>();
        listener
            .set_nonblocking(true)
            .expect("set test server nonblocking");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let thread_requests = Arc::clone(&requests);
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            for response in responses {
                let Some((mut stream, _)) = accept_until(&listener, deadline) else {
                    break;
                };
                let request = read_request(&mut stream);
                thread_requests.lock().expect("request lock").push(request);
                write_response(&mut stream, response);
            }
        });

        Self {
            base_url,
            requests,
            handle: Some(handle),
        }
    }

    pub fn finish(mut self) -> Vec<RecordedRequest> {
        if let Some(handle) = self.handle.take() {
            handle.join().expect("test server thread");
        }
        self.requests.lock().expect("request lock").clone()
    }
}

pub fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("kubeflux")
}

pub fn copy_fixture() -> (TempDir, PathBuf) {
    let temp = tempfile::tempdir().expect("temp dir");
    let dest = temp.path().join("kubeflux");
    copy_dir(&fixture_root(), &dest);
    let dest = dest.canonicalize().expect("canonical fixture copy");
    (temp, dest)
}

pub fn copy_dir(source: &Path, dest: &Path) {
    fs::create_dir_all(dest).expect("create dest");
    for entry in fs::read_dir(source).expect("read source") {
        let entry = entry.expect("dir entry");
        let file_type = entry.file_type().expect("file type");
        let child_dest = dest.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir(&entry.path(), &child_dest);
        } else {
            fs::copy(entry.path(), child_dest).expect("copy file");
        }
    }
}

pub fn write_file(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, text).expect("write file");
}

#[derive(Debug, Clone, Default)]
pub struct StaticResolverFactory {
    pub chart_versions: HashMap<(String, String), String>,
    pub image_versions: HashMap<String, String>,
}

impl StaticResolverFactory {
    pub fn new(
        chart_versions: HashMap<(String, String), String>,
        image_versions: HashMap<String, String>,
    ) -> Self {
        Self {
            chart_versions,
            image_versions,
        }
    }
}

impl ResolverFactory for StaticResolverFactory {
    fn chart_resolver(&self) -> Box<dyn ChartVersionResolver + Sync> {
        Box::new(StaticVersionResolver::new(self.chart_versions.clone()))
    }

    fn image_resolver(&self) -> Box<dyn ImageVersionResolver + Sync> {
        Box::new(StaticImageVersionResolver::new(self.image_versions.clone()))
    }
}

fn accept_until(
    listener: &TcpListener,
    deadline: Instant,
) -> Option<(std::net::TcpStream, std::net::SocketAddr)> {
    loop {
        match listener.accept() {
            Ok(connection) => return Some(connection),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return None;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("test server accept failed: {error}"),
        }
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> RecordedRequest {
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("set read timeout");
    let mut bytes = Vec::new();
    let mut buffer = [0; 1024];
    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(error) => panic!("test server read failed: {error}"),
        }
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.split("\r\n");
    let first_line = lines.next().unwrap_or_default();
    let mut request_parts = first_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default().to_string();
    let path = request_parts.next().unwrap_or_default().to_string();
    let headers = lines
        .take_while(|line| !line.is_empty())
        .filter_map(|line| {
            line.split_once(':')
                .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        })
        .collect();

    RecordedRequest {
        method,
        path,
        headers,
    }
}

fn write_response(stream: &mut std::net::TcpStream, response: ResponseSpec) {
    let reason = match response.status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Test Response",
    };
    let mut text = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        reason,
        response.body.len()
    );
    for (name, value) in response.headers {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    text.push_str("\r\n");
    text.push_str(&response.body);
    stream
        .write_all(text.as_bytes())
        .expect("write test response");
}

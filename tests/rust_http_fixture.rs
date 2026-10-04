mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

use common::{ResponseSpec, TestHttpServer};

#[test]
fn fixture_records_a_request_sent_after_the_connection_is_accepted() {
    let server = TestHttpServer::new(vec![ResponseSpec::new(200, "ready")]);
    let mut client = TcpStream::connect(server.base_url.trim_start_matches("http://"))
        .expect("connect to fixture");
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set client timeout");

    thread::sleep(Duration::from_millis(100));
    let sent = client.write_all(b"GET /delayed HTTP/1.1\r\nHost: localhost\r\n\r\n");
    let mut response = String::new();
    let received = client.read_to_string(&mut response);

    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/delayed");
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].header("Host"), Some("localhost"));
    sent.expect("send delayed request");
    received.expect("receive fixture response");
    assert!(response.ends_with("\r\n\r\nready"));
}

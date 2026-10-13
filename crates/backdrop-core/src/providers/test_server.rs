//! A local HTTP server that answers with canned responses, so provider tests
//! never reach the real service.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

/// A request the server received.
#[derive(Clone, Debug)]
pub struct Request {
    /// The path and query, e.g. `/search?q=sky`.
    pub target: String,
    /// Header names in lower case.
    pub headers: Vec<(String, String)>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

pub struct TestServer {
    url: String,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl TestServer {
    /// Answers each request in turn with the next `(status, body)`, and closes
    /// the connection after each one.
    pub fn start(responses: Vec<(u16, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let received = requests.clone();
        thread::spawn(move || {
            for (status, body) in responses {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let target = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                let mut headers = Vec::new();
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    let Some((name, value)) = line.trim_end().split_once(':') else {
                        break;
                    };
                    headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
                }
                received.lock().unwrap().push(Request { target, headers });
                let mut stream = reader.into_inner();
                let head = format!(
                    "HTTP/1.1 {status} Canned\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        Self { url, requests }
    }

    /// The server's address, e.g. `http://127.0.0.1:1234`.
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

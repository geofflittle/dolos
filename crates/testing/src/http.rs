//! A loopback HTTP server that serves one body with byte range support.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

/// A server answering HEAD and GET for one body on a loopback port until it is
/// dropped.
pub struct RangeServer {
    address: SocketAddr,
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl RangeServer {
    /// Starts serving the body on a free loopback port.
    pub fn start(body: Vec<u8>) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let stopped = Arc::new(AtomicBool::new(false));
        let body = Arc::new(body);

        let thread = {
            let stopped = stopped.clone();

            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stopped.load(Ordering::SeqCst) {
                        break;
                    }

                    if let Ok(stream) = stream {
                        let body = body.clone();
                        std::thread::spawn(move || {
                            let _ = answer(stream, &body);
                        });
                    }
                }
            })
        };

        Ok(Self {
            address,
            stopped,
            thread: Some(thread),
        })
    }

    /// The URL the body is served at.
    pub fn url(&self) -> String {
        format!("http://{}/body", self.address)
    }
}

impl Drop for RangeServer {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);

        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn answer(stream: TcpStream, body: &[u8]) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;

    let mut range = None;

    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;

        if line.trim().is_empty() {
            break;
        }

        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("range") {
                range = parse_range(value.trim(), body.len() as u64);
            }
        }
    }

    let total = body.len() as u64;
    let head = request.starts_with("HEAD ");

    let (status, first, last) = match range {
        Some((first, last)) => ("206 Partial Content", first, last),
        None => ("200 OK", 0, total.saturating_sub(1)),
    };

    let length = if total == 0 { 0 } else { last - first + 1 };

    let mut response = format!(
        "HTTP/1.1 {status}\r\nAccept-Ranges: bytes\r\nContent-Length: {length}\r\nConnection: close\r\n"
    );

    if range.is_some() {
        response.push_str(&format!("Content-Range: bytes {first}-{last}/{total}\r\n"));
    }

    response.push_str("\r\n");

    let mut stream = stream;
    stream.write_all(response.as_bytes())?;

    if !head && length > 0 {
        stream.write_all(&body[first as usize..=last as usize])?;
    }

    stream.flush()
}

fn parse_range(value: &str, total: u64) -> Option<(u64, u64)> {
    let (first, last) = value.strip_prefix("bytes=")?.split_once('-')?;
    let first: u64 = first.parse().ok()?;
    let last: u64 = last.parse().unwrap_or(u64::MAX).min(total.checked_sub(1)?);

    (first <= last).then_some((first, last))
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    fn get(server: &RangeServer, range: &str) -> String {
        let mut stream = TcpStream::connect(server.address).expect("server accepts");
        write!(stream, "GET /body HTTP/1.1\r\nHost: test\r\n{range}\r\n").expect("request sent");

        let mut response = String::new();
        stream.read_to_string(&mut response).expect("response read");

        response
    }

    #[test]
    fn a_range_returns_only_its_bytes() {
        let server = RangeServer::start(b"abcdef".to_vec()).expect("server starts");

        let response = get(&server, "Range: bytes=1-3\r\n");

        assert!(response.starts_with("HTTP/1.1 206 Partial Content\r\n"));
        assert!(response.contains("\r\nContent-Range: bytes 1-3/6\r\n"));
        assert!(response.ends_with("\r\n\r\nbcd"));
    }

    #[test]
    fn a_get_without_a_range_returns_the_whole_body() {
        let server = RangeServer::start(b"abcdef".to_vec()).expect("server starts");

        let response = get(&server, "");

        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(!response.contains("Content-Range"));
        assert!(response.ends_with("\r\n\r\nabcdef"));
    }
}

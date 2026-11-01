//! The local server behind the voice page: the page itself, and one
//! WebSocket the page relays events over. It listens on 127.0.0.1 only, and a
//! WebSocket is accepted only from a page this server served.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serde_json::Value;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::StatusCode;
use tungstenite::Message;

const PAGE: &str = include_str!("page.html");

/// Who is told about the page. The voice service implements it.
pub trait PageSink: Send + Sync + 'static {
    /// A page connected; text sent on `outgoing` goes to it. Returns its id.
    fn connected(&self, outgoing: Sender<String>) -> u64;
    fn message(&self, page: u64, message: Value);
    fn closed(&self, page: u64);
}

/// Starts serving on `port` and returns the port it got (0 asks for any).
pub fn serve(port: u16, sink: Arc<dyn PageSink>) -> io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let port = listener.local_addr()?.port();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let sink = sink.clone();
            thread::spawn(move || {
                let _ = handle(stream, port, sink);
            });
        }
    });
    Ok(port)
}

fn local_hosts(port: u16) -> [String; 2] {
    [format!("127.0.0.1:{port}"), format!("localhost:{port}")]
}

fn handle(mut stream: TcpStream, port: u16, sink: Arc<dyn PageSink>) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut head = [0u8; 16];
    let peeked = stream.peek(&mut head)?;
    if head[..peeked].starts_with(b"GET /ws ") || head[..peeked].starts_with(b"GET /ws?") {
        return websocket(stream, port, sink);
    }
    let request = read_head(&mut stream)?;
    let mut lines = request.lines();
    let first = lines.next().unwrap_or_default();
    let host_ok = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("host"))
        .is_some_and(|(_, value)| local_hosts(port).iter().any(|host| host == value.trim()));
    let (status, kind, body): (&str, &str, &[u8]) = if !host_ok {
        ("403 Forbidden", "text/plain", b"forbidden")
    } else if first.starts_with("GET / ")
        || first.starts_with("GET /?")
        || first.starts_with("GET /index.html ")
    {
        ("200 OK", "text/html; charset=utf-8", PAGE.as_bytes())
    } else if first.starts_with("GET /favicon.ico ") {
        ("204 No Content", "text/plain", b"")
    } else {
        ("404 Not Found", "text/plain", b"not found")
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

fn read_head(stream: &mut TcpStream) -> io::Result<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut chunk)?;
        if read == 0 || buffer.len() > 16 * 1024 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

fn reject(status: StatusCode) -> ErrorResponse {
    let mut response = ErrorResponse::new(Some("forbidden".into()));
    *response.status_mut() = status;
    response
}

fn websocket(stream: TcpStream, port: u16, sink: Arc<dyn PageSink>) -> io::Result<()> {
    let hosts = local_hosts(port);
    let check = move |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
        let header = |name: &str| {
            request
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
        };
        let host_ok = hosts.iter().any(|host| host == header("host"));
        // A page on any other origin, or none, is not ours.
        let origin_ok = hosts
            .iter()
            .any(|host| format!("http://{host}") == header("origin"));
        if host_ok && origin_ok {
            Ok(response)
        } else {
            Err(reject(StatusCode::FORBIDDEN))
        }
    };
    let mut socket = match tungstenite::accept_hdr(stream, check) {
        Ok(socket) => socket,
        Err(_) => return Ok(()),
    };
    socket
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(40)))?;
    let (outgoing, queue) = mpsc::channel::<String>();
    let page = sink.connected(outgoing);
    'connection: loop {
        loop {
            match queue.try_recv() {
                Ok(text) => {
                    if socket.send(Message::Text(text)).is_err() {
                        break 'connection;
                    }
                }
                Err(TryRecvError::Empty) => break,
                // Another page took over: this one is closed.
                Err(TryRecvError::Disconnected) => break 'connection,
            }
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                if let Ok(message) = serde_json::from_str::<Value>(&text) {
                    sink.message(page, message);
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
    let _ = socket.close(None);
    sink.closed(page);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder {
        seen: Mutex<Vec<Value>>,
        senders: Mutex<Vec<Sender<String>>>,
        closed: Mutex<Vec<u64>>,
    }

    impl PageSink for Recorder {
        fn connected(&self, outgoing: Sender<String>) -> u64 {
            let mut senders = self.senders.lock().unwrap();
            senders.push(outgoing);
            senders.len() as u64
        }
        fn message(&self, _page: u64, message: Value) {
            self.seen.lock().unwrap().push(message);
        }
        fn closed(&self, page: u64) {
            self.closed.lock().unwrap().push(page);
        }
    }

    fn get(port: u16, path: &str, host: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut out = String::new();
        stream.read_to_string(&mut out).unwrap();
        out
    }

    fn connect(
        port: u16,
        origin: &str,
    ) -> Result<tungstenite::WebSocket<TcpStream>, tungstenite::Error> {
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let request = tungstenite::http::Request::builder()
            .uri(format!("ws://127.0.0.1:{port}/ws"))
            .header("Host", format!("127.0.0.1:{port}"))
            .header("Origin", origin)
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .body(())
            .unwrap();
        tungstenite::client(request, stream)
            .map(|(socket, _)| socket)
            .map_err(|error| match error {
                tungstenite::HandshakeError::Failure(error) => error,
                tungstenite::HandshakeError::Interrupted(_) => tungstenite::Error::ConnectionClosed,
            })
    }

    fn wait_for<T>(mut probe: impl FnMut() -> Option<T>) -> T {
        for _ in 0..100 {
            if let Some(found) = probe() {
                return found;
            }
            thread::sleep(Duration::from_millis(30));
        }
        panic!("timed out");
    }

    #[test]
    fn serves_the_page_to_its_own_host_only() {
        let port = serve(0, Arc::new(Recorder::default())).unwrap();
        let page = get(port, "/", &format!("127.0.0.1:{port}"));
        assert!(page.starts_with("HTTP/1.1 200") && page.contains("Agent View voice"));
        assert!(get(port, "/", "evil.example:80").starts_with("HTTP/1.1 403"));
        assert!(get(port, "/nope", &format!("localhost:{port}")).starts_with("HTTP/1.1 404"));
    }

    #[test]
    fn relays_both_ways_and_reports_the_close() {
        let sink = Arc::new(Recorder::default());
        let port = serve(0, sink.clone()).unwrap();
        let mut socket = connect(port, &format!("http://127.0.0.1:{port}")).unwrap();
        socket
            .send(Message::Text(r#"{"type":"hello","ready":true}"#.into()))
            .unwrap();
        let seen = wait_for(|| sink.seen.lock().unwrap().first().cloned());
        assert_eq!(seen["type"], "hello");
        let sender = wait_for(|| sink.senders.lock().unwrap().first().cloned());
        sender.send(r#"{"type":"start"}"#.into()).unwrap();
        socket
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        assert_eq!(
            socket.read().unwrap(),
            Message::Text(r#"{"type":"start"}"#.into())
        );
        socket.close(None).unwrap();
        let _ = socket.read();
        assert_eq!(wait_for(|| sink.closed.lock().unwrap().first().copied()), 1);
    }

    #[test]
    fn a_page_from_another_origin_cannot_connect() {
        let sink = Arc::new(Recorder::default());
        let port = serve(0, sink.clone()).unwrap();
        assert!(connect(port, "https://evil.example").is_err());
        assert!(connect(port, "").is_err());
        assert!(sink.senders.lock().unwrap().is_empty());
    }

    #[test]
    fn a_replaced_page_is_disconnected() {
        let sink = Arc::new(Recorder::default());
        let port = serve(0, sink.clone()).unwrap();
        let mut socket = connect(port, &format!("http://localhost:{port}")).unwrap_or_else(|_| {
            // Host is 127.0.0.1 in the helper; the origin may name localhost.
            connect(port, &format!("http://127.0.0.1:{port}")).unwrap()
        });
        let first = wait_for(|| sink.senders.lock().unwrap().first().cloned());
        drop(first);
        sink.senders.lock().unwrap().clear();
        socket
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        assert!(matches!(socket.read(), Ok(Message::Close(_)) | Err(_)));
    }
}

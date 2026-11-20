use crate::config::ProviderConfig;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::timeout;
use tokio_rustls::TlsConnector;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const CMD_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Error)]
pub enum NntpError {
    #[error("Article not found (430)")]
    ArticleNotFound,
    #[error("Authentication failed: {0}")]
    AuthFailed(String),
    #[error("Connection limit reached / rejected: {0}")]
    TooManyConnections(String),
    #[error("Protocol error: {0}")]
    Protocol(String),
    #[error("I/O or TLS error: {0}")]
    Io(String),
    #[error("Operation timed out")]
    Timeout,
}

trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

pub struct NntpConnection {
    reader: BufReader<Box<dyn AsyncReadWrite>>,
    greeting: String,
}

impl NntpConnection {
    pub async fn connect(cfg: &ProviderConfig) -> Result<Self, NntpError> {
        let addr = format!("{}:{}", cfg.host.trim(), cfg.port);
        let tcp = timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr))
            .await
            .map_err(|_| NntpError::Timeout)?
            .map_err(|e| NntpError::Io(format!("TCP connect to {} failed: {}", addr, e)))?;
        let _ = tcp.set_nodelay(true);

        let stream: Box<dyn AsyncReadWrite> = if cfg.tls {
            let connector = build_tls_connector();
            let server_name = ServerName::try_from(cfg.host.trim().to_string())
                .map_err(|e| NntpError::Io(format!("Invalid TLS hostname '{}': {}", cfg.host, e)))?;
            let tls_stream = timeout(CONNECT_TIMEOUT, connector.connect(server_name, tcp))
                .await
                .map_err(|_| NntpError::Timeout)?
                .map_err(|e| NntpError::Io(format!("TLS handshake with {} failed: {}", addr, e)))?;
            Box::new(tls_stream)
        } else {
            Box::new(tcp)
        };

        let mut conn = Self {
            reader: BufReader::with_capacity(64 * 1024, stream),
            greeting: String::new(),
        };

        let (code, line) = conn.read_status_line().await?;
        conn.greeting = line.clone();
        match code {
            200 | 201 => {}
            400 | 502 => return Err(NntpError::TooManyConnections(line)),
            _ => return Err(NntpError::Protocol(format!("Unexpected greeting: {}", line))),
        }

        if !cfg.username.is_empty() {
            conn.authenticate(&cfg.username, &cfg.password).await?;
        }

        Ok(conn)
    }

    pub fn greeting(&self) -> &str {
        &self.greeting
    }

    async fn authenticate(&mut self, username: &str, password: &str) -> Result<(), NntpError> {
        self.write_cmd(&format!("AUTHINFO USER {}", username)).await?;
        let (code, line) = self.read_status_line().await?;
        match code {
            281 => return Ok(()),
            381 => {}
            481 | 482 | 502 => return Err(NntpError::AuthFailed(line)),
            _ => return Err(NntpError::Protocol(format!("AUTHINFO USER failed: {}", line))),
        }

        self.write_cmd(&format!("AUTHINFO PASS {}", password)).await?;
        let (code, line) = self.read_status_line().await?;
        match code {
            281 => Ok(()),
            481 | 482 => Err(NntpError::AuthFailed(line)),
            502 => Err(NntpError::TooManyConnections(line)),
            _ => Err(NntpError::Protocol(format!("AUTHINFO PASS failed: {}", line))),
        }
    }

    /// Fetch the raw (dot-unstuffed) body bytes of an article by Message-ID.
    pub async fn fetch_body(&mut self, message_id: &str) -> Result<Vec<u8>, NntpError> {
        let clean_id = message_id.trim().trim_matches(|c| c == '<' || c == '>');
        self.write_cmd(&format!("BODY <{}>", clean_id)).await?;
        let (code, line) = self.read_status_line().await?;

        match code {
            222 => self.read_multiline_body().await,
            430 | 423 => Err(NntpError::ArticleNotFound),
            480 | 481 | 482 => Err(NntpError::AuthFailed(line)),
            400 | 502 => Err(NntpError::TooManyConnections(line)),
            _ => Err(NntpError::Protocol(format!("BODY failed ({}): {}", code, line))),
        }
    }

    pub async fn quit(mut self) {
        let _ = self.write_cmd("QUIT").await;
    }

    async fn write_cmd(&mut self, cmd: &str) -> Result<(), NntpError> {
        let full = format!("{}\r\n", cmd);
        timeout(CMD_TIMEOUT, async {
            self.reader.get_mut().write_all(full.as_bytes()).await?;
            self.reader.get_mut().flush().await
        })
        .await
        .map_err(|_| NntpError::Timeout)?
        .map_err(|e| NntpError::Io(e.to_string()))
    }

    async fn read_status_line(&mut self) -> Result<(u16, String), NntpError> {
        let mut buf = Vec::with_capacity(256);
        let n = timeout(CMD_TIMEOUT, self.reader.read_until(b'\n', &mut buf))
            .await
            .map_err(|_| NntpError::Timeout)?
            .map_err(|e| NntpError::Io(e.to_string()))?;
        if n == 0 {
            return Err(NntpError::Io("Connection closed by server".to_string()));
        }
        let line = String::from_utf8_lossy(&buf)
            .trim_end_matches(['\r', '\n'])
            .to_string();
        let code = line
            .get(0..3)
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| NntpError::Protocol(format!("Malformed status line: {}", line)))?;
        Ok((code, line))
    }

    async fn read_multiline_body(&mut self) -> Result<Vec<u8>, NntpError> {
        let mut body = Vec::with_capacity(512 * 1024);
        let mut line = Vec::with_capacity(256);

        loop {
            line.clear();
            let n = timeout(CMD_TIMEOUT, self.reader.read_until(b'\n', &mut line))
                .await
                .map_err(|_| NntpError::Timeout)?
                .map_err(|e| NntpError::Io(e.to_string()))?;
            if n == 0 {
                return Err(NntpError::Io(
                    "Connection closed unexpectedly during article body".to_string(),
                ));
            }

            // Check termination line ".\r\n" or ".\n"
            if line == b".\r\n" || line == b".\n" || line == b"." {
                break;
            }

            // Dot-unstuffing (RFC 3977 section 3.1.1): leading ".." becomes "."
            if line.starts_with(b"..") {
                body.extend_from_slice(&line[1..]);
            } else {
                body.extend_from_slice(&line);
            }
        }

        Ok(body)
    }
}

fn build_tls_connector() -> TlsConnector {
    let mut root_store = RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

/// Reusable connection pool for a single Usenet provider.
pub struct ProviderPool {
    pub config: ProviderConfig,
    semaphore: Arc<Semaphore>,
    idle: Mutex<VecDeque<NntpConnection>>,
    active_counter: Arc<AtomicUsize>,
}

impl ProviderPool {
    pub fn new(config: ProviderConfig, active_counter: Arc<AtomicUsize>) -> Self {
        let max_conns = config.connections.clamp(1, 100);
        Self {
            config,
            semaphore: Arc::new(Semaphore::new(max_conns)),
            idle: Mutex::new(VecDeque::new()),
            active_counter,
        }
    }

    /// Fetch an article body using a pooled (or newly opened) NNTP connection.
    pub async fn fetch_body(&self, message_id: &str) -> Result<Vec<u8>, NntpError> {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| NntpError::Io("Provider pool closed".to_string()))?;

        self.active_counter.fetch_add(1, Ordering::Relaxed);
        let result = self.fetch_body_inner(message_id).await;
        self.active_counter.fetch_sub(1, Ordering::Relaxed);
        result
    }

    async fn fetch_body_inner(&self, message_id: &str) -> Result<Vec<u8>, NntpError> {
        // Try reusing an idle connection first
        let mut reused = false;
        let mut conn = {
            let mut guard = self.idle.lock().await;
            if let Some(c) = guard.pop_back() {
                reused = true;
                c
            } else {
                drop(guard);
                NntpConnection::connect(&self.config).await?
            }
        };

        match conn.fetch_body(message_id).await {
            Ok(data) => {
                self.idle.lock().await.push_back(conn);
                Ok(data)
            }
            Err(NntpError::ArticleNotFound) => {
                // Connection is still healthy! Return it to pool.
                self.idle.lock().await.push_back(conn);
                Err(NntpError::ArticleNotFound)
            }
            Err(_err) if reused => {
                // Stale idle connection — reconnect once and retry
                drop(conn);
                let mut fresh = NntpConnection::connect(&self.config).await?;
                match fresh.fetch_body(message_id).await {
                    Ok(data) => {
                        self.idle.lock().await.push_back(fresh);
                        Ok(data)
                    }
                    Err(NntpError::ArticleNotFound) => {
                        self.idle.lock().await.push_back(fresh);
                        Err(NntpError::ArticleNotFound)
                    }
                    Err(e2) => Err(e2),
                }
            }
            Err(err) => Err(err),
        }
    }

    pub async fn close_all(&self) {
        let conns: Vec<_> = {
            let mut guard = self.idle.lock().await;
            guard.drain(..).collect()
        };
        for c in conns {
            c.quit().await;
        }
    }
}

/// Live connection test helper for the Settings UI.
pub async fn test_provider(cfg: &ProviderConfig) -> Result<String, String> {
    if cfg.host.trim().is_empty() {
        return Err("Host is empty".to_string());
    }
    let start = Instant::now();
    match NntpConnection::connect(cfg).await {
        Ok(conn) => {
            let ms = start.elapsed().as_millis();
            let greeting = conn.greeting().to_string();
            conn.quit().await;
            Ok(format!("Connected in {}ms — {}", ms, greeting))
        }
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn test_nntp_auth_and_dot_unstuffing() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            sock.write_all(b"200 Mock NNTP Ready\r\n").await.unwrap();
            let mut reader = BufReader::new(sock);
            let mut line = String::new();

            line.clear();
            reader.read_line(&mut line).await.unwrap();
            assert_eq!(line.trim(), "AUTHINFO USER alice");
            reader.get_mut().write_all(b"381 Pass required\r\n").await.unwrap();

            line.clear();
            reader.read_line(&mut line).await.unwrap();
            assert_eq!(line.trim(), "AUTHINFO PASS secret");
            reader.get_mut().write_all(b"281 Auth accepted\r\n").await.unwrap();

            line.clear();
            reader.read_line(&mut line).await.unwrap();
            assert_eq!(line.trim(), "BODY <part1@test>");
            reader
                .get_mut()
                .write_all(b"222 Body follows\r\nhello\r\n..dotline\r\n.\r\n")
                .await
                .unwrap();

            line.clear();
            reader.read_line(&mut line).await.unwrap();
            assert_eq!(line.trim(), "BODY <missing@test>");
            reader.get_mut().write_all(b"430 No such article\r\n").await.unwrap();

            line.clear();
            let _ = reader.read_line(&mut line).await;
        });

        let cfg = ProviderConfig {
            name: "Mock".into(),
            host: "127.0.0.1".into(),
            port: addr.port(),
            tls: false,
            username: "alice".into(),
            password: "secret".into(),
            connections: 2,
            priority: 0,
            enabled: true,
        };

        let mut conn = NntpConnection::connect(&cfg).await.unwrap();
        assert!(conn.greeting().contains("Mock NNTP Ready"));

        let body = conn.fetch_body("part1@test").await.unwrap();
        assert_eq!(body, b"hello\r\n.dotline\r\n");

        let err = conn.fetch_body("missing@test").await.unwrap_err();
        assert!(matches!(err, NntpError::ArticleNotFound));

        conn.quit().await;
        server.await.unwrap();
    }
}

use anyhow::{Context, Result, bail};
use envconfig::Envconfig;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

#[derive(Envconfig, Clone)]
pub struct HealthCheckEnvConfig {
    #[envconfig(from = "HOST", default = "0.0.0.0")]
    pub http_host: String,

    #[envconfig(from = "PORT", default = "3000")]
    pub http_port: u16,

    #[envconfig(from = "TIMEOUT", default = "5")]
    pub http_timeout: u8,

    /// Path to probe. Defaults to the service's own health route
    /// (`src/modules/router/router.rs`, `/health`), rather than merely
    /// completing a TCP handshake (#44/#48): a wedged process that still
    /// holds the listening socket - accepting connections but stuck before
    /// it can ever write a response - previously still reported healthy.
    #[envconfig(from = "HEALTHCHECK_PATH", default = "/health")]
    pub http_path: String,
}

fn main() -> Result<()> {
    let config = HealthCheckEnvConfig::init_from_env()?;
    // `0.0.0.0` is a valid *bind* address (any interface) but not a valid
    // *connect* destination on every platform; the health check always
    // dials the loopback interface the server is actually reachable on.
    let host = if config.http_host == "0.0.0.0" {
        "127.0.0.1"
    } else {
        config.http_host.as_str()
    };
    let address = format!("{host}:{}", config.http_port);
    let socket_addr: SocketAddr = address
        .parse()
        .with_context(|| format!("invalid health check address {address:?}"))?;
    let timeout = Duration::from_secs(config.http_timeout as u64);

    match probe(socket_addr, &config.http_host, &config.http_path, timeout) {
        Ok(status) if (200..300).contains(&status) => {
            println!("Health check is successful ({status})");
            Ok(())
        }
        Ok(status) => {
            eprintln!("Health check failed: unexpected status {status}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Health check failed: {e:#}");
            std::process::exit(1);
        }
    }
}

/// Issues a raw HTTP/1.1 GET for `path` over a fresh TCP connection and
/// returns the response status code.
///
/// Deliberately a bare TCP connection with hand-rolled request/response
/// handling rather than pulling in `reqwest` (already a dependency of this
/// package, but its async client would require spinning up a Tokio runtime
/// on every single probe invocation - wasteful for a command a container
/// orchestrator may run every few seconds). Crucially, this sets both a
/// connect timeout *and* read/write timeouts on the socket: a peer that
/// completes the TCP handshake but then never writes a response (a wedged
/// process still holding the listening socket, the exact gap #44/#48 flag)
/// must fail the probe via the read timeout, not hang indefinitely or - as
/// the previous connect-only implementation did - report healthy just
/// because `connect()` succeeded.
fn probe(addr: SocketAddr, host_header: &str, path: &str, timeout: Duration) -> Result<u16> {
    let mut stream = TcpStream::connect_timeout(&addr, timeout).context("failed to connect")?;
    stream
        .set_read_timeout(Some(timeout))
        .context("failed to set read timeout")?;
    stream
        .set_write_timeout(Some(timeout))
        .context("failed to set write timeout")?;

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\nUser-Agent: emgr-healthcheck\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .context("failed to send request")?;

    let mut response = Vec::new();
    stream.read_to_end(&mut response).context(
        "failed to read response (peer accepted the connection but never responded, or reset it)",
    )?;

    if response.is_empty() {
        bail!("connection closed with no response (peer accepted but never wrote anything)");
    }

    parse_status_code(&response)
}

/// Parses the status code out of an HTTP/1.1 response's status line
/// (`HTTP/1.1 200 OK`) without pulling in a full HTTP parser.
fn parse_status_code(response: &[u8]) -> Result<u16> {
    let text = String::from_utf8_lossy(response);
    let status_line = text.lines().next().unwrap_or_default();
    let code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("malformed or empty HTTP response line: {status_line:?}"))?;
    code.parse::<u16>()
        .with_context(|| format!("non-numeric status code {code:?} in {status_line:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn probe_succeeds_against_responsive_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf);
                let _ = socket.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                let _ = socket.shutdown(std::net::Shutdown::Both);
            }
        });

        let status = probe(addr, "127.0.0.1", "/health", Duration::from_secs(2))
            .expect("probe should succeed against a responsive server");
        assert_eq!(status, 200);
    }

    /// The scenario #44/#48 call out directly: the peer accepts the TCP
    /// connection (so a bare `connect()`-only check would already report
    /// healthy) but then wedges and never writes anything back.
    #[test]
    fn probe_fails_against_socket_that_accepts_but_never_responds() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((socket, _)) = listener.accept() {
                thread::sleep(Duration::from_secs(10));
                drop(socket);
            }
        });

        let result = probe(addr, "127.0.0.1", "/health", Duration::from_millis(300));
        assert!(
            result.is_err(),
            "expected probe to fail (read timeout) against an unresponsive peer, got {result:?}"
        );
    }

    #[test]
    fn probe_fails_when_connection_closes_with_no_bytes_written() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf);
                let _ = socket.shutdown(std::net::Shutdown::Both);
            }
        });

        let result = probe(addr, "127.0.0.1", "/health", Duration::from_secs(2));
        assert!(
            result.is_err(),
            "expected probe to fail on an empty response, got {result:?}"
        );
    }

    #[test]
    fn probe_reports_non_2xx_status_to_the_caller() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf);
                let _ = socket.write_all(
                    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                let _ = socket.shutdown(std::net::Shutdown::Both);
            }
        });

        let status = probe(addr, "127.0.0.1", "/health", Duration::from_secs(2))
            .expect("probe should still parse a non-2xx status");
        assert_eq!(status, 503);
    }

    #[test]
    fn probe_fails_when_nothing_is_listening() {
        // Bind then immediately drop, freeing the port with (almost
        // certainly) nothing else grabbing it in the meantime - a
        // reasonably reliable way to get a "connection refused" target
        // without depending on any specific unused port number.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let result = probe(addr, "127.0.0.1", "/health", Duration::from_secs(1));
        assert!(result.is_err());
    }

    #[test]
    fn parse_status_code_reads_the_status_line() {
        assert_eq!(
            parse_status_code(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").unwrap(),
            200
        );
        assert_eq!(
            parse_status_code(b"HTTP/1.1 404 Not Found\r\n\r\n").unwrap(),
            404
        );
    }

    #[test]
    fn parse_status_code_rejects_malformed_input() {
        assert!(parse_status_code(b"").is_err());
        assert!(parse_status_code(b"not an http response").is_err());
    }
}

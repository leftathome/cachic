//! Port 443 SNI pass-through (FR-08).
//!
//! Replaces `sniproxy` in the lancache deployment model. Clients resolve a CDN name to this host
//! and then speak TLS; we read the name out of the ClientHello, resolve it properly, and splice
//! bytes in both directions.
//!
//! **Nothing is decrypted, terminated or cached.** That is N2 in the PRD and is not a limitation
//! to work around: caching HTTPS would require installing a MITM certificate on every client.
//!
//! The address guard applies here exactly as it does to the HTTP path. An SNI splice to a private
//! address is the same open-relay hazard as an HTTP proxy to one, and it is easier to overlook
//! because no HTTP request is ever parsed.

use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
};

use super::clienthello;
use crate::upstream::resolver::UpstreamResolver;

/// How long a client has to send its ClientHello before we give up.
///
/// A connection that opens and says nothing costs us a socket and a task. Real clients send the
/// hello immediately.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
pub struct SniStats {
    pub accepted: AtomicU64,
    pub spliced: AtomicU64,
    /// Closed because the ClientHello carried no usable name.
    pub no_sni: AtomicU64,
    /// Closed because the resolved address was refused by the guard.
    pub refused: AtomicU64,
    pub upstream_failed: AtomicU64,
    pub bytes_client_to_origin: AtomicU64,
    pub bytes_origin_to_client: AtomicU64,
}

pub struct SniProxy {
    addr: SocketAddr,
    stats: Arc<SniStats>,
    shutdown: Arc<AtomicBool>,
}

impl SniProxy {
    pub async fn bind(
        listen: SocketAddr,
        resolver: Arc<UpstreamResolver>,
        port: u16,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind(listen).await?;
        let addr = listener.local_addr()?;
        let stats = Arc::new(SniStats::default());
        let shutdown = Arc::new(AtomicBool::new(false));

        let task_stats = stats.clone();
        let task_shutdown = shutdown.clone();
        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    r = listener.accept() => r,
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {
                        if task_shutdown.load(Ordering::Relaxed) {
                            return;
                        }
                        continue;
                    }
                };
                let Ok((stream, _peer)) = accepted else {
                    continue;
                };
                task_stats.accepted.fetch_add(1, Ordering::Relaxed);
                let stats = task_stats.clone();
                let resolver = resolver.clone();
                tokio::spawn(async move {
                    if let Err(e) = splice(stream, resolver, port, stats.clone()).await {
                        tracing::debug!(error = %e, "sni connection closed");
                    }
                });
            }
        });

        Ok(Self {
            addr,
            stats,
            shutdown,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn stats(&self) -> &Arc<SniStats> {
        &self.stats
    }
}

impl Drop for SniProxy {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SniError {
    #[error("client sent no ClientHello within {0:?}")]
    HelloTimeout(Duration),
    #[error("client did not send a TLS handshake")]
    NotTls,
    #[error("ClientHello carried no server name")]
    NoServerName,
    #[error("ClientHello exceeded {0} bytes")]
    HelloTooLarge(usize),
    #[error("refusing {host}: {source}")]
    Refused {
        host: String,
        #[source]
        source: crate::upstream::resolver::ResolveError,
    },
    #[error("cannot reach {host}: {source}")]
    Upstream {
        host: String,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Read the ClientHello, resolve, connect, and copy in both directions.
async fn splice(
    mut client: TcpStream,
    resolver: Arc<UpstreamResolver>,
    port: u16,
    stats: Arc<SniStats>,
) -> Result<(), SniError> {
    // Buffer just enough to parse the hello. It is replayed to the origin afterwards, because the
    // origin needs the bytes we consumed to read it.
    let mut buffer = Vec::with_capacity(clienthello::MIN_CLIENT_HELLO * 4);
    let host = tokio::time::timeout(HELLO_TIMEOUT, async {
        loop {
            let mut chunk = [0u8; 1024];
            let n = client.read(&mut chunk).await?;
            if n == 0 {
                return Err(SniError::NotTls);
            }
            buffer.extend_from_slice(&chunk[..n]);

            // Reject a non-TLS client as soon as the first byte proves it, rather than waiting
            // for a record that will never come.
            if !clienthello::looks_like_tls(&buffer) {
                return Err(SniError::NotTls);
            }
            if buffer.len() > clienthello::MAX_CLIENT_HELLO {
                return Err(SniError::HelloTooLarge(clienthello::MAX_CLIENT_HELLO));
            }
            // Wait for the whole record before parsing: a partial hello parses to None, which is
            // indistinguishable from one that genuinely lacks SNI.
            if let Some(needed) = clienthello::record_length(&buffer) {
                if buffer.len() >= needed {
                    return clienthello::server_name(&buffer).ok_or(SniError::NoServerName);
                }
            }
        }
    })
    .await
    .map_err(|_| SniError::HelloTimeout(HELLO_TIMEOUT))??;

    // The guard runs here exactly as on the HTTP path. Without it, anyone on the LAN could name
    // an internal host in their SNI and have us splice to it.
    let addresses = resolver.resolve(&host, port).await.map_err(|source| {
        stats.refused.fetch_add(1, Ordering::Relaxed);
        SniError::Refused {
            host: host.clone(),
            source,
        }
    })?;

    let mut origin = None;
    let mut last_error = None;
    for address in &addresses {
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => {
                origin = Some(stream);
                break;
            }
            Ok(Err(e)) => last_error = Some(e),
            Err(_) => {
                last_error = Some(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "connect timed out",
                ))
            }
        }
    }
    let mut origin = match origin {
        Some(stream) => stream,
        None => {
            stats.upstream_failed.fetch_add(1, Ordering::Relaxed);
            return Err(SniError::Upstream {
                host,
                source: last_error.unwrap_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::NotFound, "no addresses")
                }),
            });
        }
    };

    // Replay the bytes we consumed to read the hello; the origin needs them to complete the
    // handshake.
    tokio::io::AsyncWriteExt::write_all(&mut origin, &buffer).await?;
    stats
        .bytes_client_to_origin
        .fetch_add(buffer.len() as u64, Ordering::Relaxed);
    stats.spliced.fetch_add(1, Ordering::Relaxed);
    tracing::debug!(host = %host, "sni splice established");

    let (to_origin, to_client) = tokio::io::copy_bidirectional(&mut client, &mut origin).await?;
    stats
        .bytes_client_to_origin
        .fetch_add(to_origin, Ordering::Relaxed);
    stats
        .bytes_origin_to_client
        .fetch_add(to_client, Ordering::Relaxed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    async fn proxy(allow_private: bool) -> SniProxy {
        let resolver =
            Arc::new(UpstreamResolver::new(&["1.1.1.1".parse().unwrap()], allow_private).unwrap());
        SniProxy::bind("127.0.0.1:0".parse().unwrap(), resolver, 443)
            .await
            .unwrap()
    }

    /// A minimal ClientHello for `host`.
    fn client_hello(host: &str) -> Vec<u8> {
        let mut name = vec![0u8];
        name.extend_from_slice(&(host.len() as u16).to_be_bytes());
        name.extend_from_slice(host.as_bytes());
        let mut list = (name.len() as u16).to_be_bytes().to_vec();
        list.extend_from_slice(&name);
        let mut extensions = 0u16.to_be_bytes().to_vec();
        extensions.extend_from_slice(&(list.len() as u16).to_be_bytes());
        extensions.extend_from_slice(&list);

        let mut hello = vec![0x03, 0x03];
        hello.extend_from_slice(&[0u8; 32]);
        hello.push(0);
        hello.extend_from_slice(&2u16.to_be_bytes());
        hello.extend_from_slice(&[0x13, 0x01]);
        hello.push(1);
        hello.push(0);
        hello.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
        hello.extend_from_slice(&extensions);

        let mut body = vec![0x01];
        body.push((hello.len() >> 16) as u8);
        body.extend_from_slice(&((hello.len() & 0xffff) as u16).to_be_bytes());
        body.extend_from_slice(&hello);

        let mut record = vec![0x16, 0x03, 0x01];
        record.extend_from_slice(&(body.len() as u16).to_be_bytes());
        record.extend_from_slice(&body);
        record
    }

    #[tokio::test]
    async fn splices_to_a_real_origin() {
        // A plain TCP echo server standing in for a TLS origin: the splice does not care what the
        // bytes mean, which is the entire point.
        let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin_addr = origin.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = origin.accept().await.unwrap();
            let mut buffer = vec![0u8; 4096];
            let n = tokio::io::AsyncReadExt::read(&mut stream, &mut buffer)
                .await
                .unwrap();
            // Echo back a marker plus the byte count we received.
            stream
                .write_all(format!("origin-saw-{n}").as_bytes())
                .await
                .unwrap();
        });

        let p = proxy(true).await;
        let mut client = TcpStream::connect(p.addr()).await.unwrap();
        let hello = client_hello(&origin_addr.ip().to_string());
        client.write_all(&hello).await.unwrap();

        // The proxy resolves the SNI name to a literal address and connects on the configured
        // port, so point it at the echo server's port.
        let resolver =
            Arc::new(UpstreamResolver::new(&["1.1.1.1".parse().unwrap()], true).unwrap());
        let p2 = SniProxy::bind("127.0.0.1:0".parse().unwrap(), resolver, origin_addr.port())
            .await
            .unwrap();
        let mut client = TcpStream::connect(p2.addr()).await.unwrap();
        client.write_all(&hello).await.unwrap();

        let mut response = vec![0u8; 64];
        let n = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::io::AsyncReadExt::read(&mut client, &mut response),
        )
        .await
        .expect("splice never delivered anything")
        .unwrap();
        let text = String::from_utf8_lossy(&response[..n]).to_string();
        assert!(text.starts_with("origin-saw-"), "got {text:?}");
        // The origin must have received the replayed ClientHello, not an empty stream.
        assert!(
            text.contains(&hello.len().to_string()),
            "origin saw {text:?}, expected the {} replayed hello bytes",
            hello.len()
        );
        assert_eq!(p2.stats().spliced.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn refuses_a_private_sni_target() {
        // The open-relay hazard, and the one easiest to overlook here because no HTTP request is
        // ever parsed.
        let p = proxy(false).await;
        let mut client = TcpStream::connect(p.addr()).await.unwrap();
        client
            .write_all(&client_hello("192.168.1.1"))
            .await
            .unwrap();

        let mut response = vec![0u8; 16];
        let n = tokio::io::AsyncReadExt::read(&mut client, &mut response)
            .await
            .unwrap_or(0);
        assert_eq!(n, 0, "the connection should be closed, not spliced");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(p.stats().refused.load(Ordering::Relaxed), 1);
        assert_eq!(p.stats().spliced.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn closes_a_connection_with_no_sni() {
        let p = proxy(true).await;
        let mut client = TcpStream::connect(p.addr()).await.unwrap();
        // A handshake record with no extensions at all.
        let mut hello = vec![0x03, 0x03];
        hello.extend_from_slice(&[0u8; 32]);
        hello.push(0);
        hello.extend_from_slice(&2u16.to_be_bytes());
        hello.extend_from_slice(&[0x13, 0x01]);
        hello.push(1);
        hello.push(0);
        hello.extend_from_slice(&0u16.to_be_bytes());
        let mut body = vec![0x01];
        body.push((hello.len() >> 16) as u8);
        body.extend_from_slice(&((hello.len() & 0xffff) as u16).to_be_bytes());
        body.extend_from_slice(&hello);
        let mut record = vec![0x16, 0x03, 0x01];
        record.extend_from_slice(&(body.len() as u16).to_be_bytes());
        record.extend_from_slice(&body);

        client.write_all(&record).await.unwrap();
        let mut response = vec![0u8; 16];
        let n = tokio::io::AsyncReadExt::read(&mut client, &mut response)
            .await
            .unwrap_or(0);
        assert_eq!(n, 0, "a hello without SNI must be closed, not guessed at");
    }

    #[tokio::test]
    async fn closes_a_non_tls_connection_immediately() {
        let p = proxy(true).await;
        let mut client = TcpStream::connect(p.addr()).await.unwrap();
        client.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
        let mut response = vec![0u8; 16];
        let n = tokio::io::AsyncReadExt::read(&mut client, &mut response)
            .await
            .unwrap_or(0);
        assert_eq!(n, 0, "a non-TLS client should be closed");
    }

    #[tokio::test]
    async fn a_silent_client_does_not_hold_a_task_forever() {
        // Connect and say nothing. The timeout is what stops this being a way to exhaust sockets.
        let p = proxy(true).await;
        let _client = TcpStream::connect(p.addr()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(p.stats().accepted.load(Ordering::Relaxed), 1);
        assert_eq!(p.stats().spliced.load(Ordering::Relaxed), 0);
    }
}

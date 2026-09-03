//! A minimal DNS responder.
//!
//! Exists for one assertion: that upstream fetches connect to the address `UPSTREAM_DNS`
//! returned, and not to whatever the system resolver says. Proving that needs two resolvers whose
//! answers *differ*, and the only reliable way to get a name the system resolver will not resolve
//! is to invent one and serve it ourselves.
//!
//! It answers `A` queries for any name with one configured address, and answers everything else
//! with an empty NOERROR. That is enough for a resolver to accept it and not enough for anything
//! else.

use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};

use tokio::net::UdpSocket;

pub struct MockDns {
    addr: SocketAddr,
    queries: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
}

impl MockDns {
    /// Answer every `A` query with `answer`.
    pub async fn start(answer: Ipv4Addr) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(("127.0.0.1", 0)).await?;
        let addr = socket.local_addr()?;
        let queries = Arc::new(AtomicU64::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));

        let task_queries = queries.clone();
        let task_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let mut buffer = [0u8; 512];
            loop {
                if task_shutdown.load(Ordering::Relaxed) {
                    return;
                }
                let received = tokio::select! {
                    r = socket.recv_from(&mut buffer) => r,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => continue,
                };
                let Ok((len, from)) = received else { continue };
                task_queries.fetch_add(1, Ordering::Relaxed);
                if let Some(response) = respond(&buffer[..len], answer) {
                    let _ = socket.send_to(&response, from).await;
                }
            }
        });

        Ok(Self {
            addr,
            queries,
            shutdown,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn queries(&self) -> u64 {
        self.queries.load(Ordering::Relaxed)
    }
}

impl Drop for MockDns {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

/// Build a response to a query.
///
/// Returns `None` for anything too malformed to answer, which a real resolver would also drop.
fn respond(query: &[u8], answer: Ipv4Addr) -> Option<Vec<u8>> {
    if query.len() < 12 {
        return None;
    }
    let id = [query[0], query[1]];
    let qdcount = u16::from_be_bytes([query[4], query[5]]);
    if qdcount != 1 {
        return None;
    }

    // Walk the question's labels to find where it ends.
    let mut at = 12;
    loop {
        let len = *query.get(at)? as usize;
        at += 1;
        if len == 0 {
            break;
        }
        // Compression pointers are not valid in a question.
        if len & 0xc0 != 0 {
            return None;
        }
        at += len;
    }
    let qtype = u16::from_be_bytes([*query.get(at)?, *query.get(at + 1)?]);
    at += 4; // qtype + qclass
    let question = &query[12..at];

    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&id);
    // Standard response, recursion desired and available, NOERROR.
    out.extend_from_slice(&[0x81, 0x80]);
    out.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT

    // Only A records are answered. An AAAA query gets an empty NOERROR, which is what a resolver
    // expects for a name with no IPv6 address, and it stops the resolver waiting.
    let answer_count: u16 = if qtype == 1 { 1 } else { 0 };
    out.extend_from_slice(&answer_count.to_be_bytes()); // ANCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
    out.extend_from_slice(question);

    if answer_count == 1 {
        out.extend_from_slice(&[0xc0, 0x0c]); // pointer to the question's name
        out.extend_from_slice(&1u16.to_be_bytes()); // TYPE A
        out.extend_from_slice(&1u16.to_be_bytes()); // CLASS IN
        out.extend_from_slice(&30u32.to_be_bytes()); // TTL
        out.extend_from_slice(&4u16.to_be_bytes()); // RDLENGTH
        out.extend_from_slice(&answer.octets());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A query for `example.invalid`, type A.
    fn query() -> Vec<u8> {
        let mut q = vec![0x12, 0x34, 0x01, 0x00];
        q.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
        q.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        for label in ["example", "invalid"] {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&1u16.to_be_bytes()); // A
        q.extend_from_slice(&1u16.to_be_bytes()); // IN
        q
    }

    #[test]
    fn answers_an_a_query_with_the_configured_address() {
        let response = respond(&query(), Ipv4Addr::new(10, 1, 2, 3)).unwrap();
        assert_eq!(&response[0..2], &[0x12, 0x34], "id not echoed");
        assert_eq!(
            u16::from_be_bytes([response[6], response[7]]),
            1,
            "no answer"
        );
        assert_eq!(&response[response.len() - 4..], &[10, 1, 2, 3]);
    }

    #[test]
    fn answers_a_non_a_query_with_an_empty_noerror() {
        let mut q = query();
        let len = q.len();
        q[len - 4..len - 2].copy_from_slice(&28u16.to_be_bytes()); // AAAA
        let response = respond(&q, Ipv4Addr::new(10, 1, 2, 3)).unwrap();
        assert_eq!(u16::from_be_bytes([response[6], response[7]]), 0);
    }

    #[test]
    fn refuses_to_answer_malformed_queries() {
        assert!(respond(&[], Ipv4Addr::LOCALHOST).is_none());
        assert!(respond(&[0u8; 8], Ipv4Addr::LOCALHOST).is_none());
        // A compression pointer in the question is invalid.
        let mut q = query();
        q[12] = 0xc0;
        assert!(respond(&q, Ipv4Addr::LOCALHOST).is_none());
    }

    #[test]
    fn never_panics_on_arbitrary_input() {
        let mut seed = 0xdead_beef_1234_5678u64;
        for _ in 0..20_000 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let len = (seed % 80) as usize;
            let bytes: Vec<u8> = (0..len)
                .map(|i| ((seed >> (i % 8 * 8)) & 0xff) as u8)
                .collect();
            let _ = respond(&bytes, Ipv4Addr::LOCALHOST);
        }
    }
}

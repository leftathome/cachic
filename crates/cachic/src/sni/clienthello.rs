//! Minimal TLS ClientHello parser: enough to read the SNI host and nothing more.
//!
//! This is the first thing an untrusted client sends on port 443, so it is parsed defensively and
//! nothing else is touched. Deliberately not a TLS library - we never decrypt, never terminate,
//! and never negotiate (N2). All we need is the name, so we can resolve it and splice bytes.
//!
//! Every length in the wire format is attacker-controlled, so every read is bounds-checked and
//! the whole thing is total: it returns `None` rather than panicking on anything malformed.

/// TLS record type for a handshake.
const RECORD_HANDSHAKE: u8 = 0x16;
/// Handshake message type for ClientHello.
const HANDSHAKE_CLIENT_HELLO: u8 = 0x01;
/// The `server_name` extension.
const EXT_SERVER_NAME: u16 = 0x0000;
/// `host_name` name type within it.
const NAME_TYPE_HOST: u8 = 0x00;

/// A TLS record header is 5 bytes; a ClientHello cannot be smaller than this in total.
pub const MIN_CLIENT_HELLO: usize = 43;

/// The largest ClientHello we will buffer while looking for SNI.
///
/// A TLS record is capped at 16 KiB by the protocol. Refusing to buffer more means a client
/// cannot make us hold memory by promising a record it never sends.
pub const MAX_CLIENT_HELLO: usize = 16 * 1024 + 5;

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }
    /// Skip a field whose length is given by a one-byte prefix.
    fn skip_u8_vec(&mut self) -> Option<()> {
        let len = self.u8()? as usize;
        self.take(len).map(|_| ())
    }
    /// Skip a field whose length is given by a two-byte prefix.
    fn skip_u16_vec(&mut self) -> Option<()> {
        let len = self.u16()? as usize;
        self.take(len).map(|_| ())
    }
}

/// Whether a buffer looks like the start of a TLS handshake.
///
/// Cheap enough to run before buffering more, so a non-TLS client is rejected immediately rather
/// than after we have waited for a full record.
pub fn looks_like_tls(bytes: &[u8]) -> bool {
    matches!(bytes.first(), Some(&RECORD_HANDSHAKE))
}

/// The total record length declared by a TLS record header, if the header is complete.
pub fn record_length(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 5 {
        return None;
    }
    let declared = u16::from_be_bytes([bytes[3], bytes[4]]) as usize;
    Some(declared + 5)
}

/// Extract the SNI host name from a ClientHello record.
///
/// Returns `None` for anything that is not a ClientHello carrying a `host_name`, including
/// perfectly valid TLS that simply omits SNI. The caller closes the connection in that case: we
/// have no other way to know where to send the bytes.
pub fn server_name(record: &[u8]) -> Option<String> {
    let mut r = Reader::new(record);

    // Record header: type, version, length.
    if r.u8()? != RECORD_HANDSHAKE {
        return None;
    }
    let _version = r.u16()?;
    let record_len = r.u16()? as usize;
    // Trust the declared length only as far as the buffer actually goes.
    let body = r.take(record_len.min(record.len().saturating_sub(5)))?;

    let mut h = Reader::new(body);
    if h.u8()? != HANDSHAKE_CLIENT_HELLO {
        return None;
    }
    // Handshake length is 24 bits.
    let hi = h.u8()? as usize;
    let lo = h.u16()? as usize;
    let _handshake_len = (hi << 16) | lo;

    let _client_version = h.u16()?;
    h.take(32)?; // random
    h.skip_u8_vec()?; // session id
    h.skip_u16_vec()?; // cipher suites
    h.skip_u8_vec()?; // compression methods

    // Extensions are optional in the format, though every real client sends them.
    let extensions_len = h.u16()? as usize;
    let extensions = h.take(extensions_len)?;

    let mut e = Reader::new(extensions);
    while let (Some(kind), Some(len)) = (e.u16(), e.u16()) {
        let data = e.take(len as usize)?;
        if kind != EXT_SERVER_NAME {
            continue;
        }
        let mut names = Reader::new(data);
        let list_len = names.u16()? as usize;
        let list = names.take(list_len)?;
        let mut n = Reader::new(list);
        while let (Some(name_type), Some(name_len)) = (n.u8(), n.u16()) {
            let name = n.take(name_len as usize)?;
            if name_type == NAME_TYPE_HOST {
                // A host name must be ASCII; anything else is not something we can resolve.
                let host = std::str::from_utf8(name).ok()?;
                if host.is_empty() || !host.is_ascii() {
                    return None;
                }
                return Some(host.to_ascii_lowercase());
            }
        }
        return None;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a ClientHello carrying the given SNI host.
    fn client_hello(host: Option<&str>) -> Vec<u8> {
        let mut extensions = Vec::new();
        if let Some(host) = host {
            let mut name = vec![NAME_TYPE_HOST];
            name.extend_from_slice(&(host.len() as u16).to_be_bytes());
            name.extend_from_slice(host.as_bytes());

            let mut list = (name.len() as u16).to_be_bytes().to_vec();
            list.extend_from_slice(&name);

            extensions.extend_from_slice(&EXT_SERVER_NAME.to_be_bytes());
            extensions.extend_from_slice(&(list.len() as u16).to_be_bytes());
            extensions.extend_from_slice(&list);
        }

        let mut body = vec![HANDSHAKE_CLIENT_HELLO];
        let mut hello = Vec::new();
        hello.extend_from_slice(&[0x03, 0x03]); // client version
        hello.extend_from_slice(&[0u8; 32]); // random
        hello.push(0); // session id length
        hello.extend_from_slice(&2u16.to_be_bytes()); // cipher suites length
        hello.extend_from_slice(&[0x13, 0x01]);
        hello.push(1); // compression methods length
        hello.push(0);
        hello.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
        hello.extend_from_slice(&extensions);

        let len = hello.len();
        body.push((len >> 16) as u8);
        body.extend_from_slice(&((len & 0xffff) as u16).to_be_bytes());
        body.extend_from_slice(&hello);

        let mut record = vec![RECORD_HANDSHAKE, 0x03, 0x01];
        record.extend_from_slice(&(body.len() as u16).to_be_bytes());
        record.extend_from_slice(&body);
        record
    }

    #[test]
    fn extracts_the_server_name() {
        let record = client_hello(Some("lancache.steamcontent.com"));
        assert_eq!(
            server_name(&record).as_deref(),
            Some("lancache.steamcontent.com")
        );
    }

    #[test]
    fn lowercases_the_name() {
        let record = client_hello(Some("CDN.Example.COM"));
        assert_eq!(server_name(&record).as_deref(), Some("cdn.example.com"));
    }

    #[test]
    fn a_hello_without_sni_yields_nothing() {
        // Valid TLS, just no SNI. We have no way to know where to send the bytes, so the caller
        // closes rather than guessing.
        let record = client_hello(None);
        assert_eq!(server_name(&record), None);
    }

    #[test]
    fn rejects_non_handshake_records() {
        let mut record = client_hello(Some("example.com"));
        record[0] = 0x17; // application data
        assert_eq!(server_name(&record), None);
        assert!(!looks_like_tls(&record));
    }

    #[test]
    fn truncation_at_every_offset_yields_none_rather_than_panicking() {
        // Every length in this format is attacker-controlled. Truncating at each byte exercises
        // every bounds check in turn.
        let record = client_hello(Some("cdn.example.com"));
        for cut in 0..record.len() {
            let _ = server_name(&record[..cut]);
        }
    }

    #[test]
    fn a_lying_length_prefix_does_not_read_past_the_buffer() {
        let mut record = client_hello(Some("cdn.example.com"));
        // Claim the record is far longer than it is.
        record[3] = 0xff;
        record[4] = 0xff;
        let _ = server_name(&record);
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut seed = 0x5eed_1234_abcd_ef01u64;
        for _ in 0..20_000 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let len = (seed % 300) as usize;
            let bytes: Vec<u8> = (0..len)
                .map(|i| ((seed >> (i % 8 * 8)) & 0xff) as u8)
                .collect();
            let _ = server_name(&bytes);
            let _ = record_length(&bytes);
            let _ = looks_like_tls(&bytes);
        }
    }

    #[test]
    fn reads_the_declared_record_length() {
        let record = client_hello(Some("cdn.example.com"));
        assert_eq!(record_length(&record), Some(record.len()));
        assert_eq!(record_length(&record[..4]), None);
    }

    #[test]
    fn rejects_a_non_ascii_name() {
        // Not resolvable, and not something to pass to a resolver.
        let mut record = client_hello(Some("aaaaaaaa"));
        let position = record.len() - 8;
        record[position] = 0xff;
        assert_eq!(server_name(&record), None);
    }
}

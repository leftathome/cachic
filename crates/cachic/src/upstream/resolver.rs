//! The dedicated upstream resolver.
//!
//! This is the single most important non-obvious behaviour in cachic. The proxy exists because a
//! DNS server on the LAN is answering CDN hostnames with *this cache's* address. If the cache
//! resolved upstream hostnames through that same server, every fetch would loop straight back into
//! itself (FR-03). In Kubernetes the pod's resolver forwards to cluster DNS, which may well
//! forward to exactly that server, so "just use the system resolver" is wrong there too.
//!
//! The guarantee is structural rather than a matter of discipline: `hickory-resolver` is built
//! with `default-features = false`, which excludes its `system-config` feature. The functions that
//! read `/etc/resolv.conf` - `Resolver::builder` and `TokioResolver::builder_tokio` - are then not
//! compiled at all, so reaching for the system resolver is a compile error rather than a code
//! review catch.

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use hickory_resolver::{
    config::{ConnectionConfig, NameServerConfig, ResolveHosts, ResolverConfig},
    net::runtime::TokioRuntimeProvider,
    TokioResolver,
};

use super::guard::{self, Refusal};

/// Standard DNS port. `UPSTREAM_DNS` takes addresses, not host:port, matching monolithic.
const DNS_PORT: u16 = 53;

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("no upstream resolvers configured; UPSTREAM_DNS must list at least one")]
    NoResolvers,
    #[error("cannot build the upstream resolver: {source}")]
    Build {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("resolving {host:?} failed: {source}")]
    Lookup {
        host: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("{host:?} did not resolve to any address")]
    NoAddresses { host: String },
    #[error(
        "{host:?} resolved only to addresses cachic will not fetch from ({refusal}). \
         Refusing: without this check the cache is an open proxy on the LAN. \
         Set the allow-private option only if you are deliberately caching from an internal mirror."
    )]
    Refused { host: String, refusal: Refusal },
}

/// Resolves upstream hostnames, and only through the configured servers.
#[derive(Clone)]
pub struct UpstreamResolver {
    inner: TokioResolver,
    allow_private: bool,
}

impl std::fmt::Debug for UpstreamResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpstreamResolver")
            .field("allow_private", &self.allow_private)
            .finish_non_exhaustive()
    }
}

impl UpstreamResolver {
    /// Build a resolver over exactly these servers, on the standard DNS port.
    pub fn new(servers: &[IpAddr], allow_private: bool) -> Result<Self, ResolveError> {
        let servers: Vec<SocketAddr> = servers
            .iter()
            .map(|ip| SocketAddr::new(*ip, DNS_PORT))
            .collect();
        Self::with_servers(&servers, allow_private)
    }

    /// Build a resolver over exactly these servers, each with an explicit port.
    ///
    /// `UPSTREAM_DNS` takes bare addresses and always means port 53, so production goes through
    /// [`new`](Self::new). This exists so a test can stand up a DNS server on an ephemeral port -
    /// which is the only way to give this resolver an answer that differs from the system
    /// resolver's, and therefore the only way to prove the two are not being confused.
    pub fn with_servers(servers: &[SocketAddr], allow_private: bool) -> Result<Self, ResolveError> {
        if servers.is_empty() {
            return Err(ResolveError::NoResolvers);
        }
        let mut config = ResolverConfig::default();
        for server in servers {
            // UDP with TCP fallback: some CDN answers exceed 512 bytes.
            let connections = [ConnectionConfig::udp(), ConnectionConfig::tcp()]
                .into_iter()
                .map(|mut connection| {
                    connection.port = server.port();
                    connection
                })
                .collect();
            config.add_name_server(NameServerConfig::new(server.ip(), true, connections));
        }

        let mut builder =
            TokioResolver::builder_with_config(config, TokioRuntimeProvider::default());

        // Never consult /etc/hosts. hickory's default is `Auto`, which does read it, and that
        // quietly breaks the guarantee this module is named for: a hosts entry for a CDN
        // hostname - trivially present in a container image, or added by a sidecar - would be
        // honoured ahead of UPSTREAM_DNS and could point the cache straight back at itself.
        // Only the configured servers get a say.
        builder.options_mut().use_hosts_file = ResolveHosts::Never;

        let inner = builder.build().map_err(|source| ResolveError::Build {
            source: Box::new(source),
        })?;
        Ok(Self {
            inner,
            allow_private,
        })
    }

    /// Resolve a hostname to socket addresses that are safe to fetch from.
    ///
    /// Every returned address has passed the guard. Addresses that fail are dropped rather than
    /// failing the whole lookup, so a hostname with both a public and a private answer still
    /// works - but if *none* survive, that is an error rather than an empty list, because an
    /// empty list at the call site is easy to mistake for "no result".
    pub async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, ResolveError> {
        // A literal address needs no lookup, but still needs the guard.
        if let Ok(ip) = host.parse::<IpAddr>() {
            return match guard::check(ip, self.allow_private) {
                Ok(()) => Ok(vec![SocketAddr::new(ip, port)]),
                Err(refusal) => Err(ResolveError::Refused {
                    host: host.to_owned(),
                    refusal,
                }),
            };
        }

        let lookup = self
            .inner
            .lookup_ip(host)
            .await
            .map_err(|source| ResolveError::Lookup {
                host: host.to_owned(),
                source: Box::new(source),
            })?;

        let mut allowed = Vec::new();
        let mut first_refusal = None;
        let mut any = false;
        for ip in lookup.iter() {
            any = true;
            match guard::check(ip, self.allow_private) {
                Ok(()) => allowed.push(SocketAddr::new(ip, port)),
                Err(refusal) => {
                    first_refusal.get_or_insert(refusal);
                }
            }
        }

        if !any {
            return Err(ResolveError::NoAddresses {
                host: host.to_owned(),
            });
        }
        if allowed.is_empty() {
            return Err(ResolveError::Refused {
                host: host.to_owned(),
                refusal: first_refusal.unwrap_or(Refusal::Private),
            });
        }
        Ok(allowed)
    }
}

/// Adapter making [`UpstreamResolver`] the resolver reqwest itself dials through.
///
/// This exists because of a defect found by the first real deployment, and the shape of that
/// defect is worth stating plainly so it is not reintroduced.
///
/// The client used to resolve a URL through [`UpstreamResolver`] for the address guard, discard
/// the addresses, and then hand the *hostname* to reqwest — which resolved it again through the
/// system resolver and connected there. Two consequences, both serious:
///
/// 1. FR-03's loop prevention did not work. In a lancache deployment the system resolver is the
///    one answering CDN hostnames with this cache's own address, so upstream fetches looped back
///    into our own listener. The setting was consulted and then ignored.
/// 2. **The address guard was bypassable.** Checking one set of addresses and connecting to
///    another is a time-of-check/time-of-use hole: a DNS server answering a public address to
///    `UPSTREAM_DNS` and a private one to the system resolver defeats FR-64 entirely.
///
/// Wiring the resolver into reqwest closes both, because now the addresses that were guarded are
/// the addresses that get dialled. There is no separate lookup left to disagree.
#[derive(Clone)]
pub struct GuardedResolver(Arc<UpstreamResolver>);

impl GuardedResolver {
    pub fn new(resolver: Arc<UpstreamResolver>) -> Self {
        Self(resolver)
    }
}

impl std::fmt::Debug for GuardedResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuardedResolver").finish_non_exhaustive()
    }
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let resolver = self.0.clone();
        Box::pin(async move {
            // Port 0: reqwest substitutes the scheme's port, or the one named in the URL.
            let addresses = resolver.resolve(name.as_str(), 0).await?;
            Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_build_without_resolvers() {
        // There is no fallback to the system resolver, so no servers means no way to resolve
        // anything. Failing at construction makes that a startup error.
        let err = UpstreamResolver::new(&[], false).unwrap_err();
        assert!(matches!(err, ResolveError::NoResolvers));
    }

    #[tokio::test]
    async fn applies_the_guard_to_literal_addresses() {
        let r = UpstreamResolver::new(&["1.1.1.1".parse().unwrap()], false).unwrap();
        // A literal needs no DNS, but must still be guarded.
        assert!(r.resolve("192.168.1.1", 80).await.is_err());
        assert!(r.resolve("127.0.0.1", 80).await.is_err());
        let ok = r.resolve("93.184.216.34", 80).await.unwrap();
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].port(), 80);
    }

    #[tokio::test]
    async fn allow_private_permits_literals_deliberately() {
        let r = UpstreamResolver::new(&["1.1.1.1".parse().unwrap()], true).unwrap();
        assert!(r.resolve("192.168.1.1", 80).await.is_ok());
    }

    #[tokio::test]
    async fn a_refusal_explains_why_and_what_to_do() {
        let r = UpstreamResolver::new(&["1.1.1.1".parse().unwrap()], false).unwrap();
        let err = r.resolve("10.0.0.1", 80).await.unwrap_err();
        let text = err.to_string();
        assert!(text.contains("open proxy"), "{text}");
        assert!(text.contains("allow-private"), "{text}");
    }

    /// The system resolver is unreachable by construction.
    ///
    /// `hickory-resolver` is built without its `system-config` feature, so the constructors that
    /// read `/etc/resolv.conf` do not exist. This test documents that; the real enforcement is
    /// that uncommenting the line below would not compile.
    #[test]
    fn system_resolver_constructors_are_not_compiled_in() {
        // let _ = hickory_resolver::TokioResolver::builder_tokio();
        //         ^ does not exist without the `system-config` feature.
        //
        // Kept as a comment deliberately: a test that merely asserts a runtime property could be
        // satisfied by a resolver that silently falls back. Absence at compile time cannot.
        //
        // Verified by uncommenting it, which fails with:
        //   error[E0599]: no associated function or constant named `builder_tokio` found for
        //   struct `Resolver<P>` in the current scope
        //
        // If a future dependency re-enables hickory's `system-config` feature transitively, this
        // stops being true. That is what the feature line in the workspace manifest guards.
    }
}

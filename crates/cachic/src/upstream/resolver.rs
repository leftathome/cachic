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

use std::net::{IpAddr, SocketAddr};

use hickory_resolver::{
    config::{NameServerConfig, ResolverConfig},
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
    /// Build a resolver over exactly these servers.
    pub fn new(servers: &[IpAddr], allow_private: bool) -> Result<Self, ResolveError> {
        if servers.is_empty() {
            return Err(ResolveError::NoResolvers);
        }
        let mut config = ResolverConfig::default();
        for ip in servers {
            // UDP with TCP fallback: some CDN answers exceed 512 bytes.
            config.add_name_server(NameServerConfig::udp_and_tcp(*ip));
        }
        let _ = DNS_PORT; // documented above; hickory uses 53 for these constructors
        let inner = TokioResolver::builder_with_config(config, TokioRuntimeProvider::default())
            .build()
            .map_err(|source| ResolveError::Build {
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

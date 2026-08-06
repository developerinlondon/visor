//! Embedded DNS server for VM name resolution.
//!
//! Resolves VM names from the [`DnsRegistry`] and forwards unknown queries
//! to configurable upstream DNS servers. Uses hickory-server 0.25 with the
//! `RequestHandler` trait for custom query handling.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use anyhow::Context as _;
use hickory_server::ServerFuture;
use hickory_server::authority::{LookupObject, MessageResponseBuilder};
use hickory_server::proto::op::{Header, MessageType, OpCode, ResponseCode};
use hickory_server::proto::rr::{RData, Record, rdata};
use hickory_server::resolver::TokioResolver;
use hickory_server::resolver::config::{NameServerConfigGroup, ResolverConfig, ResolverOpts};
use hickory_server::resolver::name_server::TokioConnectionProvider;
use hickory_server::server::{Request, RequestHandler, ResponseHandler, ResponseInfo};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use crate::net::dns::{DnsRegistry, DnsResolverConfig};

/// DNS domain suffix used for VM name resolution.
const VM_DOMAIN_SUFFIX: &str = ".visor.";

/// TTL in seconds for registry-resolved A records.
const RECORD_TTL: u32 = 60;

// ── DnsHandler (private) ─────────────────────────────────────────────

/// Handles DNS requests by resolving from the registry or forwarding upstream.
struct DnsHandler {
    registry: Arc<RwLock<DnsRegistry>>,
    resolver: TokioResolver,
}

#[async_trait::async_trait]
impl RequestHandler for DnsHandler {
    async fn handle_request<R: ResponseHandler>(
        &self,
        request: &Request,
        mut response_handle: R,
    ) -> ResponseInfo {
        // Only handle standard queries.
        if request.message_type() != MessageType::Query || request.op_code() != OpCode::Query {
            let response = MessageResponseBuilder::from_message_request(request)
                .error_msg(request.header(), ResponseCode::Refused);
            return send_or_fail(request, response_handle.send_response(response).await);
        }

        let Some(query) = request.queries().first() else {
            let response = MessageResponseBuilder::from_message_request(request)
                .error_msg(request.header(), ResponseCode::FormErr);
            return send_or_fail(request, response_handle.send_response(response).await);
        };

        let name = query.name().to_string();
        debug!(name = %name, "DNS query received");

        // Try local registry if the name ends with our domain suffix.
        if let Some(vm_name) = name.strip_suffix(VM_DOMAIN_SUFFIX) {
            let registry = self.registry.read().await;
            if let Some(ip) = registry.resolve(vm_name) {
                let record = Record::from_rdata(
                    query.name().to_owned().into(),
                    RECORD_TTL,
                    RData::A(rdata::A(ip)),
                );
                let mut header = Header::response_from_request(request.header());
                header.set_authoritative(true);
                let response = MessageResponseBuilder::from_message_request(request).build(
                    header,
                    [&record],
                    None.iter(),
                    None.iter(),
                    None.iter(),
                );
                return send_or_fail(request, response_handle.send_response(response).await);
            }
        }

        // Forward to upstream resolver.
        match self.resolver.lookup(query.name(), query.query_type()).await {
            Ok(lookup) => {
                let forward = ForwardLookup(lookup);
                let header = Header::response_from_request(request.header());
                let response = MessageResponseBuilder::from_message_request(request).build(
                    header,
                    forward.iter(),
                    None.iter(),
                    None.iter(),
                    None.iter(),
                );
                send_or_fail(request, response_handle.send_response(response).await)
            }
            Err(e) => {
                debug!(error = %e, "upstream lookup failed, returning NXDomain");
                let response = MessageResponseBuilder::from_message_request(request)
                    .error_msg(request.header(), ResponseCode::NXDomain);
                send_or_fail(request, response_handle.send_response(response).await)
            }
        }
    }
}

/// Extract a `ResponseInfo` from a send result, falling back on error.
fn send_or_fail(request: &Request, result: Result<ResponseInfo, std::io::Error>) -> ResponseInfo {
    match result {
        Ok(info) => info,
        Err(e) => {
            error!(error = %e, "failed to send DNS response");
            ResponseInfo::from(*request.header())
        }
    }
}

// ── ForwardLookup (private) ──────────────────────────────────────────

/// Wraps an upstream [`hickory_server::resolver::lookup::Lookup`] to satisfy
/// the [`LookupObject`] trait required by `MessageResponseBuilder`.
struct ForwardLookup(hickory_server::resolver::lookup::Lookup);

impl LookupObject for ForwardLookup {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn iter<'a>(&'a self) -> Box<dyn Iterator<Item = &'a Record> + Send + 'a> {
        Box::new(self.0.record_iter())
    }

    fn take_additionals(&mut self) -> Option<Box<dyn LookupObject>> {
        None
    }
}

// ── DnsServer (public) ───────────────────────────────────────────────

/// DNS server that resolves VM names from the registry and forwards
/// unknown queries to upstream DNS servers.
pub struct DnsServer {
    shutdown: CancellationToken,
    addr: SocketAddr,
}

impl DnsServer {
    /// Start the embedded DNS server.
    ///
    /// Binds a UDP socket to the address specified in `config`, creates a
    /// handler that resolves names from `registry` and forwards unknown
    /// queries to the configured upstream DNS servers.
    ///
    /// # Errors
    ///
    /// Returns an error if the UDP socket cannot be bound or the upstream
    /// resolver fails to initialise.
    pub async fn start(
        config: &DnsResolverConfig,
        registry: Arc<RwLock<DnsRegistry>>,
    ) -> anyhow::Result<Self> {
        let listen_addr = SocketAddr::from((config.listen_ip(), config.listen_port()));
        let socket = tokio::net::UdpSocket::bind(listen_addr)
            .await
            .with_context(|| format!("failed to bind DNS UDP socket to {listen_addr}"))?;
        let local_addr = socket
            .local_addr()
            .context("failed to get local address of DNS socket")?;

        // Build upstream resolver from config.
        let upstream_ips: Vec<IpAddr> = config
            .upstream_servers()
            .iter()
            .map(|ip| IpAddr::V4(*ip))
            .collect();
        let ns_group = NameServerConfigGroup::from_ips_clear(&upstream_ips, 53, true);
        let resolver_config = ResolverConfig::from_parts(None, vec![], ns_group);
        let resolver =
            TokioResolver::builder_with_config(resolver_config, TokioConnectionProvider::default())
                .with_options(ResolverOpts::default())
                .build();

        let handler = DnsHandler { registry, resolver };
        let mut server = ServerFuture::new(handler);
        server.register_socket(socket);

        let shutdown = server.shutdown_token().clone();

        // Run the server in the background until cancelled.
        tokio::spawn(async move {
            if let Err(e) = server.block_until_done().await {
                warn!(error = %e, "DNS server exited with error");
            }
        });

        debug!(addr = %local_addr, "DNS server started");

        Ok(Self {
            shutdown,
            addr: local_addr,
        })
    }

    /// Returns the local address the server is listening on.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Gracefully shut down the DNS server.
    pub fn stop(&self) {
        self.shutdown.cancel();
    }
}

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;

//! HTTP CONNECT proxy that enforces connector destination policies.
//!
//! The proxy resolves destinations itself and connects to the resolved socket
//! address, preventing connector-backed MCP tools from reaching loopback,
//! private, link-local, documentation, benchmark, multicast, or unspecified
//! networks through redirects, subresources, or page JavaScript.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

const MAX_HEADER_BYTES: usize = 64 * 1024;
const BLOCKED_RESPONSE: &[u8] =
    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const BAD_GATEWAY_RESPONSE: &[u8] =
    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DestinationPolicy {
    PublicOnly,
}

/// A loopback-only proxy whose lifetime is tied to the owning MCP connection.
pub(crate) struct ConnectorEgressProxy {
    address: SocketAddr,
    cancellation: CancellationToken,
}

impl ConnectorEgressProxy {
    pub(crate) async fn start(policy: DestinationPolicy) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .context("failed to bind connector egress proxy")?;
        let address = listener.local_addr()?;
        let cancellation = CancellationToken::new();
        let accept_cancellation = cancellation.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = accept_cancellation.cancelled() => break,
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((stream, peer)) => {
                                tokio::spawn(async move {
                                    if let Err(error) = handle_connection(stream, policy).await {
                                        debug!(%peer, %error, "Connector proxy connection ended with an error");
                                    }
                                });
                            }
                            Err(error) => warn!(%error, "Connector proxy failed to accept a connection"),
                        }
                    }
                }
            }
        });

        Ok(Self {
            address,
            cancellation,
        })
    }

    pub(crate) fn url(&self) -> String {
        format!("http://{}", self.address)
    }
}

impl Drop for ConnectorEgressProxy {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

async fn handle_connection(mut client: TcpStream, policy: DestinationPolicy) -> Result<()> {
    let request = match read_request_head(&mut client).await {
        Ok(request) => request,
        Err(error) => {
            let _ = client.write_all(BAD_GATEWAY_RESPONSE).await;
            return Err(error);
        }
    };

    let parsed = match ParsedProxyRequest::parse(&request.head) {
        Ok(parsed) => parsed,
        Err(error) => {
            let _ = client.write_all(BAD_GATEWAY_RESPONSE).await;
            return Err(error);
        }
    };

    let upstream_addresses = match resolve_addresses(policy, &parsed.host, parsed.port).await {
        Ok(addresses) => addresses,
        Err(error) => {
            let _ = client.write_all(BLOCKED_RESPONSE).await;
            return Err(error);
        }
    };

    let mut upstream = match connect_first(&upstream_addresses).await {
        Ok(upstream) => upstream,
        Err(error) => {
            let _ = client.write_all(BAD_GATEWAY_RESPONSE).await;
            return Err(error);
        }
    };

    match parsed.kind {
        ProxyRequestKind::Connect => {
            client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
        }
        ProxyRequestKind::Forward { origin_form } => {
            upstream.write_all(origin_form.as_bytes()).await?;
            upstream.write_all(parsed.headers.as_bytes()).await?;
            upstream.write_all(b"\r\n\r\n").await?;
        }
    }

    if !request.remainder.is_empty() {
        upstream.write_all(&request.remainder).await?;
    }

    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

struct RequestHead {
    head: Vec<u8>,
    remainder: Vec<u8>,
}

async fn read_request_head(stream: &mut TcpStream) -> Result<RequestHead> {
    let mut buffer = Vec::with_capacity(4096);
    loop {
        if let Some(end) = find_header_end(&buffer) {
            return Ok(RequestHead {
                head: buffer[..end].to_vec(),
                remainder: buffer[end + 4..].to_vec(),
            });
        }
        if buffer.len() >= MAX_HEADER_BYTES {
            bail!("connector proxy request headers exceeded {MAX_HEADER_BYTES} bytes");
        }

        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            bail!("connector proxy client closed before sending complete headers");
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

enum ProxyRequestKind {
    Connect,
    Forward { origin_form: String },
}

struct ParsedProxyRequest {
    kind: ProxyRequestKind,
    host: String,
    port: u16,
    headers: String,
}

impl ParsedProxyRequest {
    fn parse(head: &[u8]) -> Result<Self> {
        let head = std::str::from_utf8(head).context("connector proxy headers were not UTF-8")?;
        let (request_line, headers) = head
            .split_once("\r\n")
            .ok_or_else(|| anyhow::anyhow!("connector proxy request had no headers"))?;
        let mut parts = request_line.split_whitespace();
        let method = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("connector proxy request had no method"))?;
        let target = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("connector proxy request had no target"))?;
        let version = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("connector proxy request had no HTTP version"))?;
        if parts.next().is_some() || !version.starts_with("HTTP/") {
            bail!("invalid connector proxy request line");
        }

        if method.eq_ignore_ascii_case("CONNECT") {
            let (host, port) = parse_authority(target, 443)?;
            return Ok(Self {
                kind: ProxyRequestKind::Connect,
                host,
                port,
                headers: headers.to_string(),
            });
        }

        let url = reqwest::Url::parse(target)
            .context("connector proxy expected an absolute HTTP request target")?;
        if url.scheme() != "http" {
            bail!("connector proxy only forwards plain HTTP absolute targets");
        }
        if !url.username().is_empty() || url.password().is_some() {
            bail!("connector proxy URL userinfo is not allowed");
        }
        let host = url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("connector proxy target had no host"))?
            .to_string();
        let port = url.port_or_known_default().unwrap_or(80);
        let mut path = url.path().to_string();
        if path.is_empty() {
            path.push('/');
        }
        if let Some(query) = url.query() {
            path.push('?');
            path.push_str(query);
        }

        Ok(Self {
            kind: ProxyRequestKind::Forward {
                origin_form: format!("{method} {path} {version}\r\n"),
            },
            host,
            port,
            headers: headers.to_string(),
        })
    }
}

fn parse_authority(authority: &str, default_port: u16) -> Result<(String, u16)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, port) = rest
            .split_once("]:")
            .ok_or_else(|| anyhow::anyhow!("IPv6 proxy target must include a port"))?;
        return Ok((host.to_string(), port.parse()?));
    }

    match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => Ok((host.to_string(), port.parse()?)),
        Some(_) => bail!("connector proxy target had an empty host"),
        None if !authority.is_empty() => Ok((authority.to_string(), default_port)),
        None => bail!("connector proxy target had an empty host"),
    }
}

async fn resolve_addresses(
    policy: DestinationPolicy,
    host: &str,
    port: u16,
) -> Result<Vec<SocketAddr>> {
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("failed to resolve connector destination '{host}'"))?
        .collect::<Vec<_>>();

    if addresses.is_empty() {
        bail!("connector destination '{host}' did not resolve");
    }
    if let Some(blocked) = addresses
        .iter()
        .find(|address| !policy.allows(address.ip()))
    {
        bail!(
            "blocked connector destination '{host}' resolved to a disallowed address {}",
            blocked.ip()
        );
    }

    Ok(addresses)
}

async fn connect_first(addresses: &[SocketAddr]) -> Result<TcpStream> {
    let mut last_error = None;
    for address in addresses {
        match TcpStream::connect(address).await {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some((*address, error)),
        }
    }

    match last_error {
        Some((address, error)) => Err(error)
            .with_context(|| format!("failed to connect connector proxy upstream {address}")),
        None => bail!("connector proxy had no upstream addresses to connect"),
    }
}

impl DestinationPolicy {
    fn allows(self, ip: IpAddr) -> bool {
        match self {
            Self::PublicOnly => is_global_ip(ip),
        }
    }
}

fn is_global_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_global_ipv4(ip),
        IpAddr::V6(ip) => is_global_ipv6(ip),
    }
}

fn is_global_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..=127).contains(&b))
        || a >= 240
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && b == 51)
        || (a == 203 && b == 0)
        || (a == 198 && (18..=19).contains(&b)))
}

fn is_global_ipv6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || segments[..6].iter().all(|segment| *segment == 0)
        || (segments[0] == 0x0064 && segments[1] == 0xff9b && segments[2] == 0x0001)
        || (segments[0] == 0x0100 && segments[1] == 0 && segments[2] == 0 && segments[3] == 0)
        || (segments[0] == 0x2001 && segments[1] <= 0x01ff)
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] & 0xfff0) == 0x3ff0
        || segments[0] == 0x5f00
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xffc0) == 0xfec0
        || ip.to_ipv4_mapped().is_some_and(|ip| !is_global_ipv4(ip)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_public_and_non_global_addresses() {
        for public in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(is_global_ip(public.parse().unwrap()), "{public}");
        }
        for blocked in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "192.168.1.1",
            "100.64.0.1",
            "198.18.0.1",
            "192.0.2.1",
            "192.88.99.1",
            "::1",
            "::8.8.8.8",
            "64:ff9b:1::1",
            "100::1",
            "2001:2::1",
            "fe80::1",
            "fec0::1",
            "fd00::1",
            "2001:db8::1",
            "3fff::1",
            "5f00::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!is_global_ip(blocked.parse().unwrap()), "{blocked}");
        }
    }

    #[test]
    fn parses_connect_and_absolute_http_requests() {
        let connect =
            ParsedProxyRequest::parse(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443")
                .unwrap();
        assert_eq!(connect.host, "example.com");
        assert_eq!(connect.port, 443);
        assert!(matches!(connect.kind, ProxyRequestKind::Connect));

        let forward = ParsedProxyRequest::parse(
            b"GET http://example.com/docs?q=1 HTTP/1.1\r\nHost: example.com",
        )
        .unwrap();
        assert_eq!(forward.host, "example.com");
        assert_eq!(forward.port, 80);
        assert!(matches!(
            forward.kind,
            ProxyRequestKind::Forward { ref origin_form }
                if origin_form == "GET /docs?q=1 HTTP/1.1\r\n"
        ));
    }

    #[tokio::test]
    async fn proxy_rejects_loopback_connect_targets() {
        let proxy = ConnectorEgressProxy::start(DestinationPolicy::PublicOnly)
            .await
            .unwrap();
        let mut client = TcpStream::connect(proxy.address).await.unwrap();
        client
            .write_all(b"CONNECT 127.0.0.1:80 HTTP/1.1\r\nHost: 127.0.0.1:80\r\n\r\n")
            .await
            .unwrap();

        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();

        assert!(response.starts_with(b"HTTP/1.1 403 Forbidden"));
    }
}

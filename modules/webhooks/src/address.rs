//! Where a webhook may point. Without this, whoever may create a webhook
//! could make the server call addresses only the server can reach: its own
//! loopback, the private network, a cloud's metadata service.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use erpcraft_kernel::{Error, Result};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use url::Url;

/// Set to `1` to let webhooks point at private and local addresses, for
/// development and tests.
pub const ALLOW_PRIVATE: &str = "ERPCRAFT_WEBHOOKS_ALLOW_PRIVATE";

/// Inside the IPv4 unicast space (1.0.0.0 to 223.255.255.255), the blocks
/// that are not globally reachable: the special-purpose address registry.
/// Outside that space nothing is public: "this network" (0/8), multicast
/// (224/4) and the reserved top (240/4, broadcast with it).
const NOT_GLOBAL_V4: &[(Ipv4Addr, u32)] = &[
    (Ipv4Addr::new(10, 0, 0, 0), 8),      // private
    (Ipv4Addr::new(100, 64, 0, 0), 10),   // shared by carriers
    (Ipv4Addr::new(127, 0, 0, 0), 8),     // loopback
    (Ipv4Addr::new(169, 254, 0, 0), 16),  // link-local, cloud metadata
    (Ipv4Addr::new(172, 16, 0, 0), 12),   // private
    (Ipv4Addr::new(192, 0, 0, 0), 24),    // protocol assignments
    (Ipv4Addr::new(192, 0, 2, 0), 24),    // documentation
    (Ipv4Addr::new(192, 88, 99, 0), 24),  // 6to4 relays, retired
    (Ipv4Addr::new(192, 168, 0, 0), 16),  // private
    (Ipv4Addr::new(198, 18, 0, 0), 15),   // benchmarking
    (Ipv4Addr::new(198, 51, 100, 0), 24), // documentation
    (Ipv4Addr::new(203, 0, 113, 0), 24),  // documentation
];

/// Inside the IPv6 global unicast block, `2000::/3`, the parts that are not
/// globally reachable, as `(first 32 bits, prefix length)`. 6to4,
/// `2002::/16`, is judged by the IPv4 address it carries before this.
const NOT_GLOBAL_V6: &[(u32, u32)] = &[
    (0x2001_0000, 23), // protocol assignments: Teredo, benchmarking, ORCHID
    (0x2001_0db8, 32), // documentation
    (0x3fff_0000, 20), // documentation
];

/// Whether `ip` is an address on the public internet.
///
/// Public is decided by what is allowed, not by what is known to be
/// private: an address must lie in the global unicast space of its family
/// (IPv4 1.0.0.0 to 223.255.255.255, IPv6 `2000::/3`) and outside the
/// blocks of that space the special-purpose registries mark as not
/// globally reachable. Anything else is refused, known or not. An IPv6
/// address that carries an IPv4 address a router or translator will use
/// is judged by that IPv4 address. Teredo (`2001::/32`) carries one too,
/// in a form only a Teredo relay reads; it is refused whole, whatever it
/// carries.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => match embedded_v4(ip) {
            Some(inside) => is_public_v4(inside),
            None => is_public_v6(ip),
        },
    }
}

/// Whether `bits` lies in the block `network/length`.
fn within(bits: u32, network: u32, length: u32) -> bool {
    (bits ^ network) >> (32 - length) == 0
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let bits = ip.to_bits();
    matches!(ip.octets()[0], 1..=223)
        && !NOT_GLOBAL_V4
            .iter()
            .any(|(network, length)| within(bits, network.to_bits(), *length))
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let top = (ip.to_bits() >> 96) as u32;
    within(top, 0x2000_0000, 3)
        && !NOT_GLOBAL_V6
            .iter()
            .any(|(network, length)| within(top, *network, *length))
}

/// The IPv4 address an IPv6 address stands for, in the forms that carry
/// one where a router or a translator will use it: IPv4-mapped
/// (`::ffff:a.b.c.d`), IPv4-compatible (`::a.b.c.d`), IPv4-translated
/// (`::ffff:0:a.b.c.d`), NAT64 (`64:ff9b::a.b.c.d`) and 6to4
/// (`2002:AABB:CCDD::/48`). Teredo is left out: [`is_public`] refuses it
/// whole.
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let bits = ip.to_bits();
    let low = Ipv4Addr::from_bits(bits as u32);
    match bits >> 32 {
        // ::/96 holds the unspecified address and loopback as well; as
        // IPv4 they are 0.0.0.0 and 0.0.0.1, refused either way.
        0 | 0xffff | 0xffff_0000 | 0x0064_ff9b_0000_0000_0000_0000 => Some(low),
        _ if bits >> 112 == 0x2002 => Some(Ipv4Addr::from_bits((bits >> 80) as u32)),
        _ => None,
    }
}

/// The one answer to a URL that may not be called. It is the same whether
/// the address is private, the name leads to a private address, or the
/// name does not exist: the difference would tell whoever may save a
/// webhook which names exist inside the network and where they lead. The
/// reason goes to the server's log.
fn refuse(host: &str, reason: &str) -> Error {
    tracing::warn!(host, reason, "a webhook URL was refused");
    Error::validation(
        "the URL does not lead to a public address; a webhook must point to a host on the \
         public internet",
    )
}

/// Check what can be told from the URL alone: https (http only to this
/// machine), no credentials, and no literal private address. Returns the
/// parsed URL.
pub fn check_url(url: &str, allow_private: bool) -> Result<Url> {
    let url = Url::parse(url.trim())
        .map_err(|e| Error::validation(format!("the URL is not valid: {e}")))?;
    let host = url
        .host()
        .ok_or_else(|| Error::validation("the URL needs a host"))?;
    let local = match &host {
        url::Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
    };
    match url.scheme() {
        "https" => {}
        "http" if local => {}
        "http" => {
            return Err(Error::validation(
                "the URL must use https; plain http is accepted only for localhost",
            ));
        }
        _ => return Err(Error::validation("the URL must use https")),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::validation(
            "the URL cannot carry a user name or password; the receiver checks the signature instead",
        ));
    }
    let literal = match host {
        url::Host::Ipv4(ip) => Some(IpAddr::V4(ip)),
        url::Host::Ipv6(ip) => Some(IpAddr::V6(ip)),
        url::Host::Domain(_) => None,
    };
    match literal {
        Some(ip) if !allow_private && !is_public(ip) => {
            Err(refuse(&ip.to_string(), "the address is not public"))
        }
        _ => Ok(url),
    }
}

/// Look `host` up and refuse it if any of its addresses is not public.
async fn lookup(host: &str, allow_private: bool) -> Result<Vec<SocketAddr>> {
    let addrs: Vec<SocketAddr> = match tokio::net::lookup_host((host, 0)).await {
        Ok(addrs) => addrs.collect(),
        Err(error) => return Err(refuse(host, &format!("the name was not found: {error}"))),
    };
    if addrs.is_empty() {
        return Err(refuse(host, "the name has no address"));
    }
    match addrs.iter().find(|addr| !is_public(addr.ip())) {
        Some(addr) if !allow_private => Err(refuse(
            host,
            &format!("the name leads to {}, which is not public", addr.ip()),
        )),
        _ => Ok(addrs),
    }
}

/// Everything [`check_url`] checks, and that the host's name leads to
/// public addresses now. The name is looked up again for every request.
pub async fn check_target(url: &str, allow_private: bool) -> Result<Url> {
    let url = check_url(url, allow_private)?;
    if let Some(url::Host::Domain(name)) = url.host() {
        lookup(name, allow_private).await?;
    }
    Ok(url)
}

/// The name lookup of the HTTP client. The addresses it returns are the
/// ones the client connects to, so a name cannot pass the check with one
/// address and be reached at another.
pub(crate) struct Guarded {
    pub allow_private: bool,
}

impl Resolve for Guarded {
    fn resolve(&self, name: Name) -> Resolving {
        let allow_private = self.allow_private;
        Box::pin(async move {
            let addrs = lookup(name.as_str(), allow_private)
                .await
                .map_err(|e| e.to_string())?;
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_addresses_are_public() {
        for private in [
            "127.0.0.1",
            "127.255.255.254",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.255",
            "0.0.0.0",
            "0.1.2.3",
            "192.0.0.8",
            "192.0.2.1",
            "192.88.99.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.7",
            "203.0.113.7",
            "224.0.0.1",
            "239.255.255.255",
            "240.0.0.1",
            "255.255.255.255",
            // IPv6 of its own.
            "::1",
            "::",
            "fc00::1",
            "fd00:ec2::254",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "100::1",
            "2001:db8::1",
            "3fff::1",
            "2001:2::1",
            "2001:10::1",
            "4000::1",
            "e000::1",
            // IPv6 that wraps a private IPv4 address.
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
            "::10.0.0.1",
            "::127.0.0.1",
            "::ffff:0:192.168.1.1",
            "64:ff9b::10.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "2002:0a00:0001::1",
            "2002:7f00:0001::1",
            "2002:c0a8:0101:1::1",
            // Teredo, whatever it carries, and local-use translation.
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
            "2001:0:808:808::f7f7:f7f7",
            "64:ff9b:1::10.0.0.1",
            // The edges of the IPv4 unicast space.
            "0.255.255.255",
            "224.0.0.0",
            // Written in hex, still the metadata address.
            "::ffff:a9fe:a9fe",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
        for public in [
            "93.184.216.34",
            "8.8.8.8",
            "1.1.1.1",
            "172.15.255.255",
            "172.32.0.1",
            "100.63.255.255",
            "100.128.0.1",
            "192.0.1.1",
            "198.17.255.255",
            "198.20.0.1",
            "223.255.255.254",
            "2606:4700::1111",
            "2001:4860:4860::8888",
            "2001:200::1",
            "2a00:1450::1",
            "3ffe::1",
            // IPv6 that wraps a public IPv4 address.
            "::ffff:8.8.8.8",
            "::8.8.8.8",
            "::ffff:0:8.8.8.8",
            "64:ff9b::8.8.8.8",
            "1.0.0.1",
            "223.255.255.255",
            "2002:0808:0808::1",
        ] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn urls_are_checked() {
        let refused = |url: &str, allow| check_url(url, allow).unwrap_err().to_string();
        let inward = "the URL does not lead to a public address";
        assert!(check_url("https://example.com/hook?x=1", false).is_ok());
        assert!(refused("http://example.com/hook", true).contains("must use https"));
        assert_eq!(
            refused("ftp://example.com/", true),
            "the URL must use https"
        );
        assert!(refused("example.com/hook", true).contains("not valid"));
        assert!(refused("https://user:pw@example.com/", true).contains("user name or password"));
        // http is for this machine only, and this machine is private.
        assert!(check_url("http://localhost:8080/hook", true).is_ok());
        assert!(check_url("http://127.0.0.1:8080/hook", true).is_ok());
        assert!(refused("http://192.168.1.1/", true).contains("only for localhost"));
        // One answer for every address that is not public, without the
        // address in it.
        for url in [
            "http://127.0.0.1:8080/hook",
            "https://10.0.0.5/hook",
            "https://[::1]/hook",
            "https://169.254.169.254/latest",
            "https://[::10.0.0.5]/hook",
            "https://[2002:a00:5::1]/hook",
            "https://[64:ff9b::10.0.0.5]/hook",
            "https://[::ffff:a9fe:a9fe]/latest",
            "https://[2001:0:4136:e378:8000:63bf:3fff:fdd2]/hook",
        ] {
            let said = refused(url, false);
            assert!(said.starts_with(inward), "{url}: {said}");
            assert!(!said.contains("10.0.0") && !said.contains("127."), "{said}");
        }
    }

    #[tokio::test]
    async fn a_name_is_refused_the_same_way_whatever_it_leads_to() {
        let leads_inward = lookup("localhost", false).await.unwrap_err().to_string();
        let no_such_name = lookup("no-such-host.invalid", false)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(leads_inward, no_such_name);
        assert!(!leads_inward.contains("127.") && !leads_inward.contains("::1"));
        assert!(!no_such_name.contains("found"));
        assert!(lookup("localhost", true).await.is_ok());
    }
}

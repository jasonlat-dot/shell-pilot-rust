use super::{invalid, Error};
use std::net::IpAddr;

struct Rule {
    host: String,
    port: Option<u16>,
    subdomains: bool,
}
fn host(raw: &str) -> Result<String, Error> {
    let raw = raw.trim_end_matches('.');
    if let Ok(ip) = raw.parse::<IpAddr>() {
        return Ok(ip.to_string());
    }
    match url::Host::parse(raw).map_err(|_| invalid("白名单地址格式无效"))? {
        url::Host::Domain(value)
            if !value.is_empty()
                && value.len() <= 253
                && value.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && label
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
                }) =>
        {
            Ok(value.to_ascii_lowercase())
        }
        url::Host::Ipv4(value) => Ok(value.to_string()),
        url::Host::Ipv6(value) => Ok(value.to_string()),
        _ => Err(invalid("白名单地址格式无效")),
    }
}
impl Rule {
    fn parse(raw: &str) -> Result<Self, Error> {
        if raw.is_empty()
            || raw.len() > 300
            || raw
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || "/\\@?#".contains(c))
        {
            return Err(invalid(
                "白名单请填写 IP、域名或地址:端口，不包含协议或路径",
            ));
        }
        let (address, port) = if raw.starts_with('[') {
            let end = raw
                .find(']')
                .ok_or_else(|| invalid("IPv6 白名单格式应为 [地址]:端口"))?;
            let address = &raw[..=end];
            let tail = &raw[end + 1..];
            if tail.is_empty() {
                (address, None)
            } else {
                (
                    address,
                    Some(
                        tail.strip_prefix(':')
                            .ok_or_else(|| invalid("IPv6 白名单格式应为 [地址]:端口"))?,
                    ),
                )
            }
        } else if raw.matches(':').count() == 1 {
            let (address, port) = raw.split_once(':').unwrap();
            (address, Some(port))
        } else if raw.contains(':') {
            let ip = raw
                .parse::<IpAddr>()
                .map_err(|_| invalid("带端口的 IPv6 白名单请使用 [地址]:端口"))?;
            return Ok(Self {
                host: ip.to_string(),
                port: None,
                subdomains: false,
            });
        } else {
            (raw, None)
        };
        let port = port
            .map(|value| {
                value
                    .parse::<u16>()
                    .ok()
                    .filter(|port| *port > 0)
                    .ok_or_else(|| invalid("白名单端口必须是 1–65535"))
            })
            .transpose()?;
        let subdomains = address.starts_with("*.");
        let host = host(address.strip_prefix("*.").unwrap_or(address))?;
        if subdomains && host.parse::<IpAddr>().is_ok() {
            return Err(invalid("通配符仅用于域名，如 *.example.com"));
        }
        Ok(Self {
            host,
            port,
            subdomains,
        })
    }
    fn text(&self) -> String {
        let address = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            format!("{}{}", if self.subdomains { "*." } else { "" }, self.host)
        };
        self.port
            .map(|port| format!("{address}:{port}"))
            .unwrap_or(address)
    }
    fn matches(&self, target: &str, port: u16) -> bool {
        self.port.is_none_or(|value| value == port)
            && if self.subdomains {
                target
                    .strip_suffix(&self.host)
                    .is_some_and(|prefix| prefix.len() > 1 && prefix.ends_with('.'))
            } else {
                target == self.host
            }
    }
}
pub(super) fn normalize(values: Vec<String>) -> Result<Vec<String>, Error> {
    if values.len() > 256 {
        return Err(invalid("每组直连白名单最多 256 条"));
    }
    let mut rules = Vec::new();
    for (index, value) in values.iter().enumerate() {
        if value.trim().is_empty() {
            continue;
        }
        let rule=Rule::parse(value.trim()).map_err(|_|invalid(&format!("直连白名单第 {} 条格式无效，请使用 IP、域名或地址:端口；IPv6 带端口写为 [地址]:端口",index+1)))?.text();
        if !rules.contains(&rule) {
            rules.push(rule);
        }
    }
    Ok(rules)
}
pub(super) fn matches(values: &[String], target: &str, port: u16) -> Result<bool, Error> {
    if values.is_empty() {
        return Ok(false);
    }
    let target = host(target.trim_matches(['[', ']']))?;
    for value in values {
        if Rule::parse(value)?.matches(&target, port) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rules(values: &[&str]) -> Vec<String> {
        normalize(values.iter().map(|value| value.to_string()).collect()).unwrap()
    }
    #[test]
    fn exact_hosts_ports_ipv6_and_subdomains_have_explicit_boundaries() {
        let values = rules(&[
            "192.168.1.10:22",
            "API.Example.com:443",
            "*.internal.example",
            "[2001:db8::1]:2222",
            "all-ports.example",
        ]);
        for (target, port) in [
            ("192.168.1.10", 22),
            ("api.example.com", 443),
            ("API.EXAMPLE.COM.", 443),
            ("a.internal.example", 80),
            ("a.b.internal.example", 443),
            ("2001:db8:0:0::1", 2222),
            ("all-ports.example", 12345),
        ] {
            assert!(matches(&values, target, port).unwrap(), "{target}:{port}");
        }
        for (target, port) in [
            ("192.168.1.10", 2222),
            ("api.example.com", 80),
            ("internal.example", 80),
            ("fakeinternal.example", 80),
            ("internal.example.evil", 80),
            ("a.api.example.com", 443),
            ("2001:db8::1", 22),
        ] {
            assert!(!matches(&values, target, port).unwrap(), "{target}:{port}");
        }
    }
    #[test]
    fn normalize_deduplicates_domains_and_ipv6_without_resolving_dns() {
        assert_eq!(
            rules(&[
                " API.Example.com. ",
                "api.example.com",
                "2001:db8:0:0::1",
                "[2001:db8::1]",
                "bücher.example"
            ]),
            ["api.example.com", "[2001:db8::1]", "xn--bcher-kva.example"]
        );
        assert!(!matches(&rules(&["192.168.1.1"]), "server.example", 22).unwrap());
    }
    #[test]
    fn rejects_urls_paths_invalid_ports_partial_wildcards_and_malformed_ipv6() {
        for input in [
            "https://example.com",
            "example.com/path",
            "user@host",
            "host:0",
            "host:65536",
            "host:",
            "host:abc",
            "host:443:invalid",
            "[2001:db8::1]:22junk",
            "[2001:db8::1",
            "*",
            "foo*.example.com",
            "*.192.168.1.1",
            "bad host",
            "host\r\nother",
        ] {
            assert!(normalize(vec![input.into()]).is_err(), "{input}");
        }
        assert!(normalize(vec!["host.example".into(); 257]).is_err());
    }
}

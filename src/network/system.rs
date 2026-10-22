use super::*;
fn parse(raw: &str, socks: bool) -> Result<Endpoint, Error> {
    let address = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("{}://{raw}", if socks { "socks5h" } else { "http" })
    };
    let url =
        url::Url::parse(&address).map_err(|_| invalid("本机代理地址无法解析，请使用自定义代理"))?;
    let protocol = match url.scheme() {
        "http" => Protocol::Http,
        "socks5" | "socks5h" => Protocol::Socks5,
        _ => {
            return Err(invalid(
                "本机代理协议不受支持，请使用 HTTP 或 SOCKS5 自定义代理",
            ))
        }
    };
    let endpoint = Endpoint {
        protocol,
        host: url
            .host_str()
            .unwrap_or_default()
            .trim_matches(['[', ']'])
            .into(),
        port: url
            .port_or_known_default()
            .unwrap_or(if protocol == Protocol::Socks5 {
                1080
            } else {
                80
            }),
        username: percent_encoding::percent_decode_str(url.username())
            .decode_utf8_lossy()
            .into_owned(),
        password: percent_encoding::percent_decode_str(url.password().unwrap_or_default())
            .decode_utf8_lossy()
            .into_owned(),
    };
    endpoint.validate()?;
    Ok(endpoint)
}
fn bypass(host: &str, list: &str) -> bool {
    list.split([';', ','])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .any(|item| {
            item == "*"
                || (item == "<local>" && !host.contains('.'))
                || host.eq_ignore_ascii_case(item)
                || item
                    .strip_prefix("*.")
                    .or_else(|| item.strip_prefix('.'))
                    .is_some_and(|suffix| {
                        host.eq_ignore_ascii_case(suffix)
                            || host
                                .to_ascii_lowercase()
                                .ends_with(&format!(".{}", suffix.to_ascii_lowercase()))
                    })
        })
}
fn configured(raw: &str, scheme: &str) -> Result<Route, Error> {
    if !raw.contains('=')
        || raw
            .find("://")
            .is_some_and(|index| !raw[..index].contains('='))
    {
        return parse(raw, false).map(Some);
    }
    let items: Vec<_> = raw
        .split(';')
        .filter_map(|item| item.trim().split_once('='))
        .collect();
    let selected = items
        .iter()
        .find(|(key, _)| *key == scheme)
        .or_else(|| items.iter().find(|(key, _)| *key == "http"))
        .or_else(|| items.iter().find(|(key, _)| *key == "socks"));
    selected
        .map(|(key, value)| parse(value.trim(), *key == "socks"))
        .transpose()
}
pub(super) fn resolve(host: &str, scheme: &str) -> Result<Route, Error> {
    if std::env::var("NO_PROXY")
        .or_else(|_| std::env::var("no_proxy"))
        .is_ok_and(|list| bypass(host, &list))
    {
        return Ok(None);
    }
    let keys: &[&str] = if scheme == "http" {
        &["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"]
    } else if scheme == "ssh" {
        &[
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
            "HTTP_PROXY",
            "http_proxy",
        ]
    } else {
        &["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
    };
    for key in keys {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                return parse(value.trim(), false).map(Some);
            }
        }
    }
    let (proxy, exclusions, pac) = windows_proxy();
    if bypass(host, &exclusions) {
        return Ok(None);
    }
    if !proxy.is_empty() {
        return configured(&proxy, if scheme == "ssh" { "https" } else { scheme });
    }
    if pac {
        return Err(invalid("系统使用 PAC 自动配置，当前请填写自定义代理地址"));
    }
    Ok(None)
}
pub(super) fn description() -> String {
    match resolve("shellpilot-proxy-detection.invalid", "https") {
        Ok(Some(endpoint)) => format!(
            "{} · {}:{}",
            if endpoint.protocol == Protocol::Http {
                "HTTP"
            } else {
                "SOCKS5"
            },
            endpoint.host,
            endpoint.port
        ),
        Ok(None) => "未检测到静态代理，将直接连接".into(),
        Err(_) => "系统代理无法直接使用，请填写自定义代理（支持 HTTP / SOCKS5）".into(),
    }
}
#[cfg(windows)]
fn windows_proxy() -> (String, String, bool) {
    use windows_sys::Win32::{
        Foundation::GlobalFree,
        Networking::WinHttp::{
            WinHttpGetIEProxyConfigForCurrentUser, WINHTTP_CURRENT_USER_IE_PROXY_CONFIG,
        },
    };
    unsafe fn take(value: *mut u16) -> String {
        if value.is_null() {
            return String::new();
        }
        let mut length = 0;
        while length < 65536 && unsafe { *value.add(length) } != 0 {
            length += 1;
        }
        let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(value, length) });
        unsafe { GlobalFree(value.cast()) };
        text
    }
    let mut value = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG::default();
    if unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut value) } == 0 {
        return Default::default();
    }
    unsafe {
        let pac = take(value.lpszAutoConfigUrl);
        (
            take(value.lpszProxy),
            take(value.lpszProxyBypass),
            !pac.is_empty(),
        )
    }
}
#[cfg(not(windows))]
fn windows_proxy() -> (String, String, bool) {
    Default::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn static_protocol_selection_bypass_and_credentials_are_parsed_without_environment_changes() {
        let route = configured(
            "http=127.0.0.1:7890;https=127.0.0.1:7891;socks=127.0.0.1:1080",
            "https",
        )
        .unwrap()
        .unwrap();
        assert_eq!(route.port, 7891);
        let route = configured("socks=127.0.0.1:1080", "https")
            .unwrap()
            .unwrap();
        assert!(route.protocol == Protocol::Socks5);
        let route = parse("socks5h://user:pass%40word@127.0.0.1:1080", false).unwrap();
        assert_eq!(route.password, "pass@word");
        for host in ["printer", "api.example.com", "example.com", "127.0.0.1"] {
            assert!(bypass(host, "<local>;*.example.com;127.0.0.1"));
        }
        assert!(!bypass("fakeexample.com", "*.example.com"));
        assert!(parse("https://127.0.0.1:7890", false).is_err());
    }
}

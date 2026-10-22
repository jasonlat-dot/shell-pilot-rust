//! Workspace-local outbound policy. Credentials never appear in views or logs.
mod bypass;
mod system;
#[cfg(test)]
mod tests;
mod tunnel;
use crate::{error::Error, secrets, store::Store};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    #[default]
    Direct,
    System,
    Custom,
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Protocol {
    #[default]
    Http,
    Socks5,
}
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProxyConfig {
    pub mode: Mode,
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub bypass: Vec<String>,
    pub(crate) password: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    pub ai: ProxyConfig,
    pub ssh: ProxyConfig,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            ai: ProxyConfig {
                mode: Mode::System,
                ..Default::default()
            },
            ssh: ProxyConfig::default(),
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProxyUpdate {
    mode: Mode,
    protocol: Protocol,
    host: String,
    port: u16,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    clear_password: bool,
    #[serde(default)]
    bypass: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    ai: ProxyUpdate,
    ssh: ProxyUpdate,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyView {
    mode: Mode,
    protocol: Protocol,
    host: String,
    port: u16,
    username: String,
    password_configured: bool,
    bypass: Vec<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    ai: ProxyView,
    ssh: ProxyView,
    system_proxy: String,
}

// No Debug: route equality includes authentication, used only for pool reuse.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Endpoint {
    protocol: Protocol,
    host: String,
    port: u16,
    username: String,
    password: String,
}
pub(crate) type Route = Option<Endpoint>;
fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
pub(crate) fn loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}
impl ProxyConfig {
    fn view(&self) -> ProxyView {
        ProxyView {
            mode: self.mode,
            protocol: self.protocol,
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            password_configured: !self.password.is_empty(),
            bypass: self.bypass.clone(),
        }
    }
    fn update(&self, update: ProxyUpdate) -> Result<Self, Error> {
        let next = Self {
            mode: update.mode,
            protocol: update.protocol,
            host: update.host.trim().trim_matches(['[', ']']).into(),
            port: update.port,
            username: update.username,
            bypass: bypass::normalize(update.bypass)?,
            password: if update.clear_password {
                String::new()
            } else {
                update.password.unwrap_or_else(|| self.password.clone())
            },
        };
        if next.mode == Mode::Custom {
            next.endpoint()?;
        }
        Ok(next)
    }
    fn endpoint(&self) -> Result<Endpoint, Error> {
        let endpoint = Endpoint {
            protocol: self.protocol,
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            password: self.password.clone(),
        };
        endpoint.validate()?;
        Ok(endpoint)
    }
    pub(crate) fn resolve(&self, host: &str, port: u16, scheme: &str) -> Result<Route, Error> {
        if loopback(host) || bypass::matches(&self.bypass, host, port)? {
            return Ok(None);
        }
        match self.mode {
            Mode::Direct => Ok(None),
            Mode::Custom => self.endpoint().map(Some),
            Mode::System => system::resolve(host, scheme),
        }
    }
    pub(crate) fn http_client(
        &self,
        builder: reqwest::ClientBuilder,
        target: &url::Url,
    ) -> Result<reqwest::ClientBuilder, Error> {
        let builder = builder.no_proxy();
        let Some(endpoint) = self.resolve(
            target.host_str().unwrap_or_default(),
            target.port_or_known_default().unwrap_or(0),
            target.scheme(),
        )?
        else {
            return Ok(builder);
        };
        let mut address = url::Url::parse(if endpoint.protocol == Protocol::Http {
            "http://proxy.invalid"
        } else {
            "socks5h://proxy.invalid"
        })
        .unwrap();
        if let Ok(ip) = endpoint.host.parse::<IpAddr>() {
            address
                .set_ip_host(ip)
                .map_err(|_| invalid("代理服务器地址无效"))?;
        } else {
            address
                .set_host(Some(&endpoint.host))
                .map_err(|_| invalid("代理服务器地址无效"))?;
        }
        address
            .set_port(Some(endpoint.port))
            .map_err(|_| invalid("代理端口无效"))?;
        address
            .set_username(&endpoint.username)
            .map_err(|_| invalid("代理用户名无效"))?;
        if !endpoint.password.is_empty() {
            address
                .set_password(Some(&endpoint.password))
                .map_err(|_| invalid("代理密码无效"))?;
        }
        let proxy = reqwest::Proxy::all(address).map_err(|_| invalid("无法创建代理连接配置"))?;
        Ok(builder.proxy(proxy))
    }
}
impl Endpoint {
    fn validate(&self) -> Result<(), Error> {
        if self.host.is_empty()
            || self.host.len() > 253
            || self.port == 0
            || self
                .host
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || "/\\@?#".contains(c))
        {
            return Err(invalid(
                "请输入有效的代理主机和 1–65535 端口，主机不包含协议或路径",
            ));
        }
        if self.host.parse::<IpAddr>().is_err() && url::Host::parse(&self.host).is_err() {
            return Err(invalid("代理主机格式无效，请将端口填在端口栏"));
        }
        if self.username.len() > 255
            || self.password.len() > 255
            || self.username.contains(':')
            || self
                .username
                .chars()
                .chain(self.password.chars())
                .any(char::is_control)
        {
            return Err(invalid("代理认证信息无效或超过 255 字节"));
        }
        Ok(())
    }
}
const ENTROPY: &str = "shellpilot-network-settings-v1";
pub fn load(store: &Store) -> Result<Settings, Error> {
    let encrypted: Option<Vec<u8>> = store
        .database
        .lock()
        .map_err(|_| Error::Worker)?
        .query_row(
            "SELECT protected_config FROM client_network_settings WHERE id=1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    match encrypted {
        None => Ok(Settings::default()),
        Some(bytes) => {
            let value = secrets::unprotect(&bytes, ENTROPY)
                .map_err(|_| invalid("无法解密代理设置，请重新保存"))?;
            serde_json::from_str(&value).map_err(|_| invalid("代理设置无法读取，请重新保存"))
        }
    }
}
pub fn view(store: &Store) -> Result<View, Error> {
    Ok(to_view(load(store)?))
}
fn to_view(settings: Settings) -> View {
    View {
        ai: settings.ai.view(),
        ssh: settings.ssh.view(),
        system_proxy: system::description(),
    }
}
pub fn save(store: &Store, update: Update) -> Result<View, Error> {
    let previous = load(store)?;
    let settings = Settings {
        ai: previous.ai.update(update.ai)?,
        ssh: previous.ssh.update(update.ssh)?,
    };
    let encrypted = secrets::protect(&serde_json::to_string(&settings)?, ENTROPY)
        .map_err(|_| invalid("无法安全保存代理设置"))?;
    store.database.lock().map_err(|_| Error::Worker)?.execute("INSERT INTO client_network_settings(id,protected_config) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET protected_config=excluded.protected_config", [encrypted])?;
    Ok(to_view(settings))
}
pub(crate) async fn connect(
    host: &str,
    port: u16,
    route: &Route,
) -> Result<tokio::net::TcpStream, Error> {
    tunnel::connect(host, port, route).await
}

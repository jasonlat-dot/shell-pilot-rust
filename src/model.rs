use crate::error::Error;
use serde::{Deserialize, Serialize};

// Deliberately no Debug: requests may contain passwords and private keys.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionRequest {
    pub connection_id: Option<String>,
    pub connection_name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_type: Option<u8>,
    pub user_id: Option<String>,
    pub password: Option<String>,
    pub private_key: Option<String>,
    pub connect_timeout: Option<u32>,
    pub keepalive_interval: Option<u32>,
    pub startup_command: Option<String>,
    pub compression: Option<bool>,
    pub strict_host_key_check: Option<bool>,
    pub known_hosts: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub connection_id: String,
    pub connection_name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_type: u8,
    pub user_id: String,
    pub encrypted: u8,
    pub created_at: String,
    pub updated_at: String,
    pub connect_timeout: u32,
    pub keepalive_interval: u32,
    pub startup_command: Option<String>,
    pub compression: bool,
    pub strict_host_key_check: bool,
    pub known_hosts: Option<String>,
}

impl ConnectionRequest {
    pub(crate) fn resolve(
        &self,
        id: String,
        previous: Option<&Connection>,
    ) -> Result<Connection, Error> {
        for (field, value) in [
            ("连接名称", &self.connection_name),
            ("主机地址", &self.host),
            ("用户名", &self.username),
        ] {
            if value.trim().is_empty() {
                return Err(Error::Invalid(format!("{field}不能为空")));
            }
        }
        if self.port == 0 {
            return Err(Error::Invalid("端口号不合法".into()));
        }
        let auth_type = self.auth_type.unwrap_or(1);
        if ![1, 2].contains(&auth_type) {
            return Err(Error::Invalid("认证类型必须为 1 或 2".into()));
        }
        let connect_timeout = self
            .connect_timeout
            .or(previous.map(|c| c.connect_timeout))
            .unwrap_or(10);
        if connect_timeout == 0 || connect_timeout > 300 {
            return Err(Error::Invalid("连接超时必须在 1 到 300 秒之间".into()));
        }
        let strict = self
            .strict_host_key_check
            .or(previous.map(|c| c.strict_host_key_check))
            .unwrap_or(true);
        let known_hosts = self
            .known_hosts
            .as_ref()
            .map(|s| s.trim().to_owned())
            .or_else(|| previous.and_then(|c| c.known_hosts.clone()));
        if strict && known_hosts.as_ref().is_none_or(|s| s.is_empty()) {
            return Err(Error::Invalid(
                "开启严格主机密钥检查时，必须填写 known_hosts 主机密钥".into(),
            ));
        }
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let user_id = self
            .user_id
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .cloned()
            .or_else(|| previous.map(|c| c.user_id.clone()))
            .unwrap_or_else(|| "default".into());
        Ok(Connection {
            connection_id: id,
            connection_name: self.connection_name.trim().into(),
            host: self.host.trim().into(),
            port: self.port,
            username: self.username.trim().into(),
            auth_type,
            user_id,
            encrypted: 1,
            created_at: previous
                .map(|c| c.created_at.clone())
                .unwrap_or_else(|| now.clone()),
            updated_at: now,
            connect_timeout,
            keepalive_interval: self
                .keepalive_interval
                .or(previous.map(|c| c.keepalive_interval))
                .unwrap_or(60),
            startup_command: self
                .startup_command
                .clone()
                .or_else(|| previous.and_then(|c| c.startup_command.clone())),
            compression: self
                .compression
                .or(previous.map(|c| c.compression))
                .unwrap_or(false),
            strict_host_key_check: strict,
            known_hosts,
        })
    }
}

// Only the SSH adapter uses this type; never serialize it as an HTTP response.
#[derive(Clone)]
pub struct Credentials {
    pub password: Option<String>,
    pub private_key: Option<String>,
}

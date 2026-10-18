//! Per-turn MCP connections. No redirects, automatic replay, sampling or server-initiated tools.
#[cfg(test)]
mod http_tests;
mod sse;
use super::*;
use config::{Server, Transport};
use futures_util::StreamExt;
use std::{process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
const MAX: usize = 1024 * 1024;
pub struct Client {
    transport: Wire,
    next: u64,
    timeout: Duration,
}
enum Wire {
    Sse(sse::Connection),
    Stdio {
        _child: Box<tokio::process::Child>,
        input: tokio::process::ChildStdin,
        output: BufReader<tokio::process::ChildStdout>,
    },
    Http {
        client: reqwest::Client,
        url: String,
        bearer: Option<String>,
        session: Option<String>,
        version: String,
    },
}

async fn line(reader: &mut BufReader<tokio::process::ChildStdout>) -> Result<Vec<u8>> {
    let mut line = vec![];
    loop {
        let buffer = reader.fill_buf().await.map_err(|_| Error::Unavailable)?;
        if buffer.is_empty() {
            return Err(Error::Unavailable);
        }
        let end = buffer.iter().position(|b| *b == b'\n').map(|n| n + 1);
        let take = end.unwrap_or(buffer.len());
        if line.len() + take > MAX {
            return Err(Error::Invalid);
        }
        line.extend_from_slice(&buffer[..take]);
        reader.consume(take);
        if end.is_some() {
            return Ok(line);
        }
    }
}
impl Client {
    pub async fn connect(
        server: &Server,
        cancel: &CancellationToken,
        bearer: Option<String>,
        values: super::connection_values::Values,
    ) -> Result<Self> {
        let transport = match &server.transport {
            Transport::Stdio { command, args, env } => {
                // Node's Windows installation supplies these launchers as .cmd files.
                #[cfg(windows)]
                let command = match command.to_ascii_lowercase().as_str() {
                    "npx" => "npx.cmd",
                    "npm" => "npm.cmd",
                    _ => command.as_str(),
                };
                let mut command = tokio::process::Command::new(command);
                command
                    .args(args)
                    .env_clear()
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true);
                for key in [
                    "PATH",
                    "SystemRoot",
                    "WINDIR",
                    "TEMP",
                    "TMP",
                    "HOME",
                    "USERPROFILE",
                ] {
                    if let Some(value) = std::env::var_os(key) {
                        command.env(key, value);
                    }
                }
                for key in env {
                    if let Some(value) = std::env::var_os(key) {
                        command.env(key, value);
                    }
                }
                command.envs(&values.env);
                #[cfg(windows)]
                command.creation_flags(0x08000000);
                let mut child = command.spawn().map_err(|_| Error::Unavailable)?;
                let input = child.stdin.take().ok_or(Error::Unavailable)?;
                let output = BufReader::new(child.stdout.take().ok_or(Error::Unavailable)?);
                Wire::Stdio {
                    _child: Box::new(child),
                    input,
                    output,
                }
            }
            Transport::Sse {
                url,
                bearer_env,
                timeout_seconds,
            } => {
                let timeout = Duration::from_secs(*timeout_seconds);
                let open = sse::Connection::open(
                    http_client(url, &values)?,
                    url.clone(),
                    if values.headers.contains_key("authorization") {
                        None
                    } else {
                        bearer_value(bearer_env)?
                    },
                );
                Wire::Sse(
                    tokio::select! {biased;_=cancel.cancelled()=>return Err(Error::Cancelled), result=tokio::time::timeout(timeout,open)=>result.map_err(|_|Error::Unavailable)??},
                )
            }
            Transport::StreamableHttp { url, bearer_env } => {
                let client = http_client(url, &values)?;
                Wire::Http {
                    client,
                    url: url.clone(),
                    bearer: if values.headers.contains_key("authorization") {
                        None
                    } else {
                        match bearer {
                            Some(token) => Some(token),
                            None => bearer_value(bearer_env)?,
                        }
                    },
                    session: None,
                    version: "2025-03-26".into(),
                }
            }
        };
        let timeout = match &server.transport {
            Transport::Sse {
                timeout_seconds, ..
            } => Duration::from_secs(*timeout_seconds),
            _ => Duration::from_secs(45),
        };
        let version = if matches!(transport, Wire::Sse(_)) {
            "2024-11-05"
        } else {
            "2025-03-26"
        };
        let mut client = Self {
            transport,
            next: 1,
            timeout,
        };
        let hello=client.rpc("initialize",json!({"protocolVersion":version,"capabilities":{},"clientInfo":{"name":"ShellPilot","version":env!("CARGO_PKG_VERSION")}}),cancel).await?;
        let version = hello["protocolVersion"]
            .as_str()
            .filter(|v| matches!(*v, "2024-11-05" | "2025-03-26" | "2025-06-18"))
            .ok_or(Error::Invalid)?;
        if let Wire::Http { version: v, .. } = &mut client.transport {
            *v = version.into();
        }
        client
            .exchange(
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                cancel,
            )
            .await?;
        Ok(client)
    }
    pub async fn rpc(
        &mut self,
        method: &str,
        params: Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let id = self.next;
        self.next += 1;
        let response = self
            .exchange(
                json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
                cancel,
            )
            .await?;
        if response["jsonrpc"] != "2.0" || response["id"] != id || response.get("error").is_some() {
            return Err(Error::Unavailable);
        }
        response.get("result").cloned().ok_or(Error::Invalid)
    }
    async fn exchange(&mut self, request: Value, cancel: &CancellationToken) -> Result<Value> {
        let result = tokio::select! {biased; _=cancel.cancelled()=>Err(Error::Cancelled), result=tokio::time::timeout(self.timeout,self.exchange_inner(request))=>result.unwrap_or(Err(Error::Unavailable))};
        if result.is_err() {
            if let Wire::Sse(connection) = &mut self.transport {
                connection.close();
            }
        }
        result
    }
    async fn exchange_inner(&mut self, request: Value) -> Result<Value> {
        let response_expected = request.get("id").is_some();
        match &mut self.transport {
            Wire::Sse(connection) => connection.exchange(&request).await,
            Wire::Stdio { input, output, .. } => {
                let mut bytes = serde_json::to_vec(&request).map_err(|_| Error::Invalid)?;
                bytes.push(b'\n');
                input
                    .write_all(&bytes)
                    .await
                    .map_err(|_| Error::Unavailable)?;
                input.flush().await.map_err(|_| Error::Unavailable)?;
                if !response_expected {
                    return Ok(Value::Null);
                }
                for _ in 0..256 {
                    let response: Value =
                        serde_json::from_slice(&line(output).await?).map_err(|_| Error::Invalid)?;
                    if response.get("method").is_some() {
                        if let Some(id) = response.get("id") {
                            let mut bytes=json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Server requests are not supported"}}).to_string().into_bytes();
                            bytes.push(b'\n');
                            input
                                .write_all(&bytes)
                                .await
                                .map_err(|_| Error::Unavailable)?;
                        }
                        continue;
                    }
                    if response["id"] == request["id"] {
                        return Ok(response);
                    }
                    return Err(Error::Invalid);
                }
                Err(Error::Invalid)
            }
            Wire::Http {
                client,
                url,
                bearer,
                session,
                version,
            } => {
                let mut post = client
                    .post(url.as_str())
                    .header("Accept", "application/json, text/event-stream")
                    .header("MCP-Protocol-Version", version.as_str())
                    .json(&request);
                if let Some(value) = bearer {
                    post = post.bearer_auth(value.as_str());
                }
                if let Some(value) = session.as_ref() {
                    post = post.header("Mcp-Session-Id", value);
                }
                let response = post.send().await.map_err(|_| Error::Unavailable)?;
                if !response.status().is_success() {
                    return Err(Error::Unavailable);
                }
                if request["method"] == "initialize" {
                    if let Some(id) = response.headers().get("Mcp-Session-Id") {
                        let id = id.to_str().map_err(|_| Error::Invalid)?;
                        if id.is_empty()
                            || id.len() > 1024
                            || !id.bytes().all(|b| (33..=126).contains(&b))
                        {
                            return Err(Error::Invalid);
                        }
                        *session = Some(id.into());
                    }
                }
                if !response_expected {
                    return Ok(Value::Null);
                }
                let sse = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|s| s.starts_with("text/event-stream"));
                let mut stream = response.bytes_stream();
                let mut bytes = vec![];
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|_| Error::Unavailable)?;
                    if bytes.len() + chunk.len() > MAX {
                        return Err(Error::Invalid);
                    }
                    bytes.extend_from_slice(&chunk);
                    if sse {
                        let raw = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
                        for event in raw
                            .split("\n\n")
                            .collect::<Vec<_>>()
                            .split_last()
                            .map_or(&[][..], |(_, complete)| complete)
                        {
                            let data = event
                                .lines()
                                .filter_map(|l| l.strip_prefix("data:").map(str::trim_start))
                                .collect::<Vec<_>>()
                                .join("\n");
                            if let Ok(response) = serde_json::from_str::<Value>(&data) {
                                if response["id"] == request["id"]
                                    && response.get("method").is_none()
                                {
                                    return Ok(response);
                                }
                            }
                        }
                    }
                }
                if sse {
                    Err(Error::Invalid)
                } else {
                    serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)
                }
            }
        }
    }
}

fn bearer_value(name: &str) -> Result<Option<String>> {
    if name.is_empty() {
        Ok(None)
    } else {
        std::env::var(name)
            .map(Some)
            .map_err(|_| Error::Unavailable)
    }
}
fn http_client(raw: &str, values: &super::connection_values::Values) -> Result<reqwest::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    for (key, value) in &values.headers {
        let name =
            reqwest::header::HeaderName::from_bytes(key.as_bytes()).map_err(|_| Error::Invalid)?;
        let mut value =
            reqwest::header::HeaderValue::from_str(value).map_err(|_| Error::Invalid)?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    let mut builder = reqwest::Client::builder()
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10));
    if url::Url::parse(raw)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .is_some_and(|h| {
            h == "localhost" || h.parse::<std::net::IpAddr>().is_ok_and(|i| i.is_loopback())
        })
    {
        builder = builder.no_proxy();
    }
    builder.build().map_err(|_| Error::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, Bytes},
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::post,
        Json, Router,
    };
    async fn http(headers: HeaderMap, Json(request): Json<Value>) -> Response {
        if request["method"] != "initialize" {
            assert_eq!(headers["mcp-session-id"], "fixture-session");
            assert_eq!(headers["mcp-protocol-version"], "2025-03-26");
        }
        match request["method"].as_str().unwrap() {
            "initialize" => {
                let mut response=Json(json!({"jsonrpc":"2.0","id":request["id"],"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"test","version":"1"}}})).into_response();
                response
                    .headers_mut()
                    .insert("mcp-session-id", "fixture-session".parse().unwrap());
                response
            }
            "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
            "tools/list" => {
                let raw = format!(
                    "data: {}\r\n\r\ndata: {}\r\n\r\n",
                    json!({"jsonrpc":"2.0","method":"notifications/message","params":{"data":"汉🙂"}}),
                    json!({"jsonrpc":"2.0","id":request["id"],"result":{"tools":[]}})
                );
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(Body::from_stream(futures_util::stream::iter(
                        raw.into_bytes()
                            .into_iter()
                            .map(|b| Ok::<_, std::io::Error>(Bytes::from(vec![b]))),
                    )))
                    .unwrap()
            }
            _ => StatusCode::BAD_REQUEST.into_response(),
        }
    }
    #[tokio::test]
    async fn streamable_http_keeps_session_headers_and_accepts_fragmented_sse_notifications() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/mcp", post(http)))
                .await
                .unwrap();
        });
        let token = CancellationToken::new();
        let mut client = Client::connect(
            &Server {
                name: "fixture".into(),
                description: String::new(),
                connection_values: None,
                saved_headers: vec![],
                saved_env: vec![],
                bearer_token: Default::default(),
                bearer_token_configured: false,
                enabled: true,
                transport: Transport::StreamableHttp {
                    url,
                    bearer_env: String::new(),
                },
            },
            &token,
            None,
            Default::default(),
        )
        .await
        .unwrap();
        let result = client.rpc("tools/list", json!({}), &token).await.unwrap();
        assert!(result["tools"].as_array().unwrap().is_empty());
        token.cancel();
        assert!(matches!(
            client.rpc("tools/list", json!({}), &token).await,
            Err(Error::Cancelled)
        ));
        server.abort();
    }
    #[cfg(windows)]
    #[tokio::test]
    async fn stdio_initialization_notifications_and_rpc_use_a_hidden_scoped_process() {
        let script = r#"while ($null -ne ($line = [Console]::ReadLine())) {
            $r = ConvertFrom-Json $line
            if ($r.method -eq 'notifications/initialized') { continue }
            $result = @{ tools = @() }
            if ($r.method -eq 'initialize') { $result = @{ protocolVersion = '2025-03-26'; capabilities = @{}; serverInfo = @{ name = 'fixture'; version = '1' } } }
            if ($r.method -eq 'fixture/environment') { $result = @{ value = $env:SHELLPILOT_FIXTURE_MCP_ENV } }
            [Console]::WriteLine((@{ jsonrpc = '2.0'; id = $r.id; result = $result } | ConvertTo-Json -Depth 10 -Compress))
        }"#;
        let token = CancellationToken::new();
        let mut client = Client::connect(
            &Server {
                name: "fixture".into(),
                description: String::new(),
                connection_values: None,
                saved_headers: vec![],
                saved_env: vec![],
                bearer_token: Default::default(),
                bearer_token_configured: false,
                enabled: true,
                transport: Transport::Stdio {
                    command: "powershell.exe".into(),
                    args: vec![
                        "-NoLogo".into(),
                        "-NoProfile".into(),
                        "-NonInteractive".into(),
                        "-Command".into(),
                        script.into(),
                    ],
                    env: vec![],
                },
            },
            &token,
            None,
            super::super::connection_values::Values {
                headers: Default::default(),
                env: BTreeMap::from([(
                    "SHELLPILOT_FIXTURE_MCP_ENV".into(),
                    "fixture-env-value".into(),
                )]),
            },
        )
        .await
        .unwrap();
        assert!(
            client.rpc("tools/list", json!({}), &token).await.unwrap()["tools"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            client
                .rpc("fixture/environment", json!({}), &token)
                .await
                .unwrap()["value"],
            "fixture-env-value"
        );
        token.cancel();
        assert!(matches!(
            client.rpc("tools/list", json!({}), &token).await,
            Err(Error::Cancelled)
        ));
        drop(client);
    }
}

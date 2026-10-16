//! Legacy 2024-11-05 HTTP+SSE: one persistent GET and a server-advertised POST URI.
use super::*;
use tokio::sync::mpsc;

struct Event {
    kind: String,
    data: String,
}
#[derive(Default)]
struct Decoder {
    line: Vec<u8>,
    kind: String,
    data: String,
    has_data: bool,
    skip_lf: bool,
    first_line: bool,
    started: bool,
    event_bytes: usize,
}
impl Decoder {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Event>> {
        let mut events = vec![];
        for &byte in bytes {
            if self.skip_lf {
                self.skip_lf = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if byte == b'\r' || byte == b'\n' {
                self.skip_lf = byte == b'\r';
                if let Some(event) = self.finish_line()? {
                    events.push(event);
                }
            } else {
                self.event_bytes += 1;
                if self.event_bytes > MAX {
                    return Err(Error::Invalid);
                }
                self.line.push(byte);
            }
        }
        Ok(events)
    }
    fn finish_line(&mut self) -> Result<Option<Event>> {
        let line = std::mem::take(&mut self.line);
        let mut line = std::str::from_utf8(&line).map_err(|_| Error::Invalid)?;
        if !self.started {
            self.started = true;
            self.first_line = true;
        }
        if self.first_line {
            self.first_line = false;
            line = line.strip_prefix('\u{feff}').unwrap_or(line);
        }
        if line.is_empty() {
            self.event_bytes = 0;
            if !self.has_data {
                self.kind.clear();
                return Ok(None);
            }
            self.has_data = false;
            let mut data = std::mem::take(&mut self.data);
            data.pop();
            let kind = std::mem::take(&mut self.kind);
            return Ok(Some(Event {
                kind: if kind.is_empty() {
                    "message".into()
                } else {
                    kind
                },
                data,
            }));
        }
        if line.starts_with(':') {
            return Ok(None);
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.kind = value.into(),
            "data" => {
                self.has_data = true;
                self.data.push_str(value);
                self.data.push('\n');
            }
            _ => {} // id/retry never cause reconnection or replay.
        }
        Ok(None)
    }
}

pub(super) struct Connection {
    client: reqwest::Client,
    post_url: url::Url,
    bearer: Option<String>,
    events: mpsc::Receiver<Result<Event>>,
    stop: CancellationToken,
    reader: tokio::task::JoinHandle<()>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}
impl Connection {
    pub(super) fn close(&mut self) {
        self.stop.cancel();
        self.reader.abort();
        self.events.close();
    }
    pub(super) async fn open(
        client: reqwest::Client,
        url: String,
        bearer: Option<String>,
    ) -> Result<Self> {
        let mut get = client.get(&url).header("Accept", "text/event-stream");
        if let Some(token) = &bearer {
            get = get.bearer_auth(token);
        }
        let response = get.send().await.map_err(|_| Error::Unavailable)?;
        if !response.status().is_success()
            || !response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|s| {
                    s.split(';')
                        .next()
                        .is_some_and(|v| v.trim().eq_ignore_ascii_case("text/event-stream"))
                })
        {
            return Err(Error::Unavailable);
        }
        let base = url::Url::parse(&url).map_err(|_| Error::Invalid)?;
        let (tx, events) = mpsc::channel(16);
        let stop = CancellationToken::new();
        let token = stop.clone();
        let reader = tokio::spawn(async move {
            let read = async {
                let mut decoder = Decoder::default();
                let mut stream = response.bytes_stream();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|_| Error::Unavailable)?;
                    if chunk.len() > MAX {
                        return Err(Error::Invalid);
                    }
                    for event in decoder.feed(&chunk)? {
                        tx.send(Ok(event)).await.map_err(|_| Error::Cancelled)?;
                    }
                }
                Err::<(), Error>(Error::Unavailable)
            };
            tokio::select! {biased;_=token.cancelled()=>{},result=read=>{token.cancel();if let Err(error)=result {let _=tx.send(Err(error)).await;}}}
        });
        // Own the reader before awaiting the endpoint, so timeout/drop also closes the GET.
        let mut connection = Self {
            client,
            post_url: base.clone(),
            bearer,
            events,
            stop,
            reader,
        };
        let event = connection.events.recv().await.ok_or(Error::Unavailable)??;
        if event.kind != "endpoint" || event.data.len() > 16384 {
            return Err(Error::Invalid);
        }
        let endpoint = base.join(event.data.trim()).map_err(|_| Error::Invalid)?;
        // The server may select a path/session, never a different credential destination.
        if endpoint.origin() != base.origin()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(Error::Invalid);
        }
        connection.post_url = endpoint;
        Ok(connection)
    }
    async fn post(&self, value: &Value) -> Result<()> {
        let mut post = self.client.post(self.post_url.clone()).json(value);
        if let Some(token) = &self.bearer {
            post = post.bearer_auth(token);
        }
        let response = post.send().await.map_err(|_| Error::Unavailable)?;
        if !response.status().is_success() {
            return Err(Error::Unavailable);
        }
        Ok(())
    }
    pub(super) async fn exchange(&mut self, request: &Value) -> Result<Value> {
        if self.stop.is_cancelled() {
            return Err(Error::Unavailable);
        }
        self.post(request).await?;
        if request.get("id").is_none() {
            return Ok(Value::Null);
        }
        for _ in 0..256 {
            let event = self.events.recv().await.ok_or(Error::Unavailable)??;
            if event.kind == "endpoint" {
                return Err(Error::Invalid);
            }
            if event.kind != "message" {
                continue;
            }
            let value: Value = serde_json::from_str(&event.data).map_err(|_| Error::Invalid)?;
            if value["jsonrpc"] != "2.0" {
                return Err(Error::Invalid);
            }
            if value.get("method").is_some() {
                if let Some(id) = value.get("id") {
                    self.post(&json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Server requests are not supported"}})).await?;
                }
                continue;
            }
            if value["id"] != request["id"] {
                return Err(Error::Invalid);
            }
            return Ok(value);
        }
        Err(Error::Invalid)
    }
}

#[cfg(test)]
mod tests;

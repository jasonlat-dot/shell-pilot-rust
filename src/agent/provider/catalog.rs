//! Bounded, read-only model discovery. Wire formats belong to provider strategies.
mod diagnostics;
mod qwen;
use super::*;
use reqwest::{header, redirect::Policy, Client};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashSet, time::Duration};
use tokio_util::sync::CancellationToken;
use url::Url;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogRequest {
    pub provider: ProviderKind,
    #[serde(default)]
    pub supplier: Option<SupplierKind>,
    pub base_url: String,
    pub require_api_key: bool,
    #[serde(default)]
    pub api_key: KeyChange,
    pub channel_id: Option<String>,
    pub revision: Option<u64>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}
pub(super) struct Page {
    models: Vec<CatalogModel>,
    cursor: Option<String>,
}
pub(super) trait CatalogAdapter: Sync {
    fn endpoint(&self, base: &Url, cursor: Option<&str>) -> Result<Url>;
    fn decode(&self, value: Value) -> Result<Page>;
}
pub(super) struct OpenAi;
pub(super) struct Anthropic;
pub(super) struct Google;
pub(super) static OPENAI: OpenAi = OpenAi;
pub(super) static ANTHROPIC: Anthropic = Anthropic;
pub(super) static GOOGLE: Google = Google;
pub(super) static QWEN: qwen::Qwen = qwen::Qwen;
fn endpoint(base: &Url) -> Result<Url> {
    base.join("models")
        .map_err(|_| ModelError::Invalid("invalid models endpoint"))
}
fn string<'a>(item: &'a Value, field: &str, path: &str) -> Result<&'a str> {
    item[field]
        .as_str()
        .ok_or_else(|| diagnostics::invalid(path, "string", item.get(field)))
}
fn model(id: &str, name: Option<&str>, path: &str) -> Result<CatalogModel> {
    let id = id.trim();
    if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        return Err(diagnostics::invalid(
            path,
            "1..256 printable UTF-8 bytes",
            Some(&Value::String(id.into())),
        ));
    }
    Ok(CatalogModel {
        id: id.into(),
        display_name: name
            .filter(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
            .map(str::to_owned),
    })
}
fn data(value: &Value) -> Result<Vec<CatalogModel>> {
    value["data"]
        .as_array()
        .ok_or_else(|| diagnostics::invalid("/data", "array", value.get("data")))?
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let path = format!("/data/{index}/id");
            model(
                string(item, "id", &path)?,
                item["display_name"].as_str().or_else(|| item["name"].as_str()),
                &path,
            )
        })
        .collect()
}
impl CatalogAdapter for OpenAi {
    fn endpoint(&self, base: &Url, _: Option<&str>) -> Result<Url> {
        endpoint(base)
    }
    fn decode(&self, value: Value) -> Result<Page> {
        Ok(Page {
            models: data(&value)?,
            cursor: None,
        })
    }
}
impl CatalogAdapter for Anthropic {
    fn endpoint(&self, base: &Url, cursor: Option<&str>) -> Result<Url> {
        let mut url = endpoint(base)?;
        url.query_pairs_mut().append_pair("limit", "1000");
        if let Some(cursor) = cursor {
            url.query_pairs_mut().append_pair("after_id", cursor);
        }
        Ok(url)
    }
    fn decode(&self, value: Value) -> Result<Page> {
        let models = data(&value)?;
        let more = value
            .get("has_more")
            .map(|v| {
                v.as_bool()
                    .ok_or_else(|| diagnostics::invalid("/has_more", "boolean", Some(v)))
            })
            .transpose()?
            .unwrap_or(false);
        let cursor = if more {
            Some(string(&value, "last_id", "/last_id")?.into())
        } else {
            None
        };
        Ok(Page { models, cursor })
    }
}
impl CatalogAdapter for Google {
    fn endpoint(&self, base: &Url, cursor: Option<&str>) -> Result<Url> {
        let mut url = endpoint(base)?;
        url.query_pairs_mut().append_pair("pageSize", "1000");
        if let Some(cursor) = cursor {
            url.query_pairs_mut().append_pair("pageToken", cursor);
        }
        Ok(url)
    }
    fn decode(&self, value: Value) -> Result<Page> {
        let mut models = Vec::new();
        let mut unadvertised = 0;
        for (index, item) in value["models"]
            .as_array()
            .ok_or_else(|| diagnostics::invalid("/models", "array", value.get("models")))?
            .iter()
            .enumerate()
        {
            if !item.is_object() {
                return Err(diagnostics::invalid(
                    &format!("/models/{index}"),
                    "object",
                    Some(item),
                ));
            }
            let methods = match item.get("supportedGenerationMethods") {
                None | Some(Value::Null) => {
                    unadvertised += 1;
                    continue;
                }
                Some(value) => value.as_array().ok_or_else(|| {
                    diagnostics::invalid(
                        &format!("/models/{index}/supportedGenerationMethods"),
                        "array",
                        Some(value),
                    )
                })?,
            };
            if methods.is_empty() {
                unadvertised += 1;
                continue;
            }
            if !methods
                .iter()
                .any(|v| v.as_str() == Some("generateContent"))
            {
                continue;
            }
            let path = format!("/models/{index}/name");
            let name = string(item, "name", &path)?;
            let id = name.strip_prefix("models/").ok_or_else(|| {
                diagnostics::invalid(&path, "models/<model-id>", item.get("name"))
            })?;
            if !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            {
                return Err(diagnostics::invalid(
                    &path,
                    "Gemini model ID",
                    item.get("name"),
                ));
            }
            models.push(model(id, item["displayName"].as_str(), &path)?);
        }
        diagnostics::unadvertised(unadvertised);
        let cursor = value
            .get("nextPageToken")
            .filter(|value| !value.is_null())
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| diagnostics::invalid("/nextPageToken", "string", Some(v)))
            })
            .transpose()?
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        Ok(Page { models, cursor })
    }
}
/// Credentials never leave native memory. Discovery does not save any draft fields.
pub struct CatalogClient {
    provider: ProviderKind,
    supplier: Option<SupplierKind>,
    base: Url,
    client: Client,
    headers: header::HeaderMap,
    adapter: &'static dyn CatalogAdapter,
}
impl CatalogClient {
    pub(super) fn new(request: &CatalogRequest, key: Option<SecretKey>, proxy: &crate::network::ProxyConfig) -> Result<Self> {
        let base = super::config::normalize_base_url(&request.base_url)?;
        if request.require_api_key && key.is_none() {
            return Err(ModelError::KeyMissing);
        }
        super::supplier::validate(request.supplier.as_ref(), &request.provider)?;
        let policy = super::supplier::select(request.supplier.as_ref());
        let headers = policy.catalog_headers(&request.provider, if request.require_api_key {
            key.as_ref()
        } else {
            None
        })?;
        let builder = Client::builder()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10));
        let builder = proxy.http_client(builder, &base).map_err(|_|ModelError::Invalid("代理配置不可用，请检查其他设置中的网络代理"))?;
        Ok(Self {
            provider: request.provider.clone(),
            supplier: super::supplier::canonical(request.supplier.clone()),
            base,
            client: builder.build().map_err(super::transport::network)?,
            headers,
            adapter: policy.catalog(&request.provider),
        })
    }
    pub async fn list(&self, cancel: &CancellationToken) -> Result<Vec<CatalogModel>> {
        diagnostics::observe(self, Box::pin(self.list_inner(cancel))).await
    }
    async fn list_inner(&self, cancel: &CancellationToken) -> Result<Vec<CatalogModel>> {
        let operation = async {
            let mut models = Vec::new();
            let mut ids = HashSet::new();
            let mut cursors = HashSet::new();
            let mut cursor: Option<String> = None;
            let mut total = 0usize;
            for page_index in 1..=20 {
                let url = self.adapter.endpoint(&self.base, cursor.as_deref())?;
                diagnostics::request(&url, page_index, &self.headers);
                let mut response = self
                    .client
                    .get(url)
                    .headers(self.headers.clone())
                    .header(header::ACCEPT, "application/json")
                    .send()
                    .await
                    .map_err(super::transport::network)?;
                super::diagnostics::response(&response);
                diagnostics::response(&response, page_index);
                if !response.status().is_success() {
                    return Err(super::diagnostics::http_error(response).await);
                }
                const LIMIT: usize = 8 * 1024 * 1024;
                if response
                    .content_length()
                    .is_some_and(|len| len > (LIMIT - total) as u64)
                {
                    return Err(ModelError::ResponseTooLarge);
                }
                let mut bytes = Vec::new();
                while let Some(chunk) = response.chunk().await.map_err(super::transport::network)? {
                    total = total
                        .checked_add(chunk.len())
                        .ok_or(ModelError::ResponseTooLarge)?;
                    if total > LIMIT {
                        return Err(ModelError::ResponseTooLarge);
                    }
                    bytes.extend_from_slice(&chunk);
                }
                let value = serde_json::from_slice(&bytes)
                    .map_err(|error| diagnostics::json_error(&error))?;
                diagnostics::shape(&value, page_index, bytes.len());
                let page = self.adapter.decode(value)?;
                for model in page.models {
                    if ids.insert(model.id.clone()) {
                        models.push(model);
                    }
                    if models.len() > 10000 {
                        return Err(ModelError::ResponseTooLarge);
                    }
                }
                match page.cursor {
                    None => return Ok(models),
                    Some(next)
                        if next.is_empty()
                            || next.len() > 4096
                            || !cursors.insert(next.clone()) =>
                    {
                        diagnostics::pagination("empty_oversized_or_repeated_cursor", page_index);
                        return Err(ModelError::Response);
                    }
                    Some(next) => cursor = Some(next),
                }
            }
            diagnostics::pagination("page_limit", 20);
            Err(ModelError::ResponseTooLarge)
        };
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ModelError::Cancelled),
            result = tokio::time::timeout(Duration::from_secs(30),operation) => result.unwrap_or(Err(ModelError::Timeout)),
        }
    }
}

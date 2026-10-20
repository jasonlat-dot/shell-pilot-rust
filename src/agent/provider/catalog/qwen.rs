use super::*;

pub(in crate::agent::provider) struct Qwen;
impl CatalogAdapter for Qwen {
    fn endpoint(&self, base: &Url, cursor: Option<&str>) -> Result<Url> {
        let mut base = base.clone();
        // Only complete documented API suffixes are mapped; keep proxy origin/prefix.
        let prefix = [
            "/compatible-mode/v1/",
            "/apps/anthropic/v1/",
            "/apps/anthropic/",
        ]
        .into_iter()
        .find_map(|suffix| base.path().strip_suffix(suffix));
        if let Some(prefix) = prefix {
            let path = format!("{prefix}/api/v1/");
            base.set_path(&path);
        }
        let native = base.path().ends_with("/api/v1/");
        let mut url = endpoint(&base)?;
        if native || cursor.is_some() {
            let page = cursor
                .unwrap_or("1")
                .parse::<u64>()
                .ok()
                .filter(|page| (1..=20).contains(page))
                .ok_or(ModelError::Response)?;
            url.query_pairs_mut()
                .append_pair("page_no", &page.to_string())
                .append_pair("page_size", "100");
        }
        Ok(url)
    }
    fn decode(&self, value: Value) -> Result<Page> {
        if value.get("success").is_some_and(|success| success != true) {
            return Err(diagnostics::invalid(
                "/success",
                "true",
                value.get("success"),
            ));
        }
        if value.get("output").is_none() {
            return OPENAI.decode(value);
        }
        let output = &value["output"];
        let number = |field: &str| {
            output[field].as_u64().ok_or_else(|| {
                diagnostics::invalid(
                    &format!("/output/{field}"),
                    "nonnegative integer",
                    output.get(field),
                )
            })
        };
        let total = number("total")?;
        let page = number("page_no")?;
        let size = number("page_size")?;
        if !(1..=20).contains(&page) || !(1..=1000).contains(&size) || total > 10000 {
            return Err(ModelError::ResponseTooLarge);
        }
        let items = output["models"]
            .as_array()
            .ok_or_else(|| diagnostics::invalid("/output/models", "array", output.get("models")))?;
        let models = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let path = format!("/output/models/{index}/model");
                model(string(item, "model", &path)?, item["name"].as_str(), &path)
            })
            .collect::<Result<Vec<_>>>()?;
        let before = (page - 1) * size;
        if items.len() as u64 > size
            || before + items.len() as u64 > total
            || (before + (items.len() as u64) < total && (items.len() as u64) < size)
        {
            return Err(ModelError::Response);
        }
        Ok(Page {
            models,
            cursor: (before + (items.len() as u64) < total).then(|| (page + 1).to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_complete_documented_suffixes_are_mapped_and_cursor_never_controls_origin() {
        for (path, expected) in [
            ("/compatible-mode/v1/", "/api/v1/models"),
            ("/proxy/apps/anthropic/v1/", "/proxy/api/v1/models"),
            ("/proxy/apps/anthropic/", "/proxy/api/v1/models"),
            (
                "/proxy/not-compatible-mode/v1/",
                "/proxy/not-compatible-mode/v1/models",
            ),
            ("/proxy/v1/", "/proxy/v1/models"),
        ] {
            let base = Url::parse(&format!("https://proxy.test{path}")).unwrap();
            let url = Qwen.endpoint(&base, None).unwrap();
            assert_eq!(url.origin(), base.origin());
            assert_eq!(url.path(), expected);
            assert!(Qwen.endpoint(&base, Some("https://evil.test/key")).is_err());
        }
    }
    #[test]
    fn malformed_native_pages_cannot_succeed_as_empty_or_partial_results() {
        assert!(Qwen.decode(json!({"success":false,"data":[{"id":"alias"}]})).is_err());
        let valid = json!({"success":true,"output":{"total":1,"page_no":1,"page_size":100,"models":[{"model":"alias","name":"Model"}]}});
        assert_eq!(Qwen.decode(valid.clone()).unwrap().models[0].id, "alias");
        for (field, value) in [
            ("total", json!("1")),
            ("page_no", json!(0)),
            ("page_size", json!(0)),
            ("total", json!(101)),
            ("models", json!([{"name":"missing id"}])),
        ] {
            let mut bad = valid.clone();
            bad["output"][field] = value;
            assert!(Qwen.decode(bad).is_err(), "{field}");
        }
        let mut bad = valid;
        bad["success"] = false.into();
        assert!(Qwen.decode(bad).is_err());
    }
}

use crate::{config::PRODUCT, Config, Error};
use object_store::gcp::{GoogleCloudStorage, GoogleCloudStorageBuilder};
use reqwest::{Client, Method, Url};
use serde_json::Value;

pub struct Storage {
    client: Client,
    credentials: GoogleCloudStorage,
    buckets: [String; 4],
    base: Url,
}

fn validate_policy(value: &Value) -> Result<(), Error> {
    if !value.is_object()
        || value
            .pointer("/versioning/enabled")
            .is_some_and(|v| v != false)
        || value
            .get("defaultEventBasedHold")
            .is_some_and(|v| v != false)
        || value.get("retentionPolicy").is_some()
        || value
            .pointer("/softDeletePolicy/retentionDurationSeconds")
            .is_some_and(|v| v.as_str() != Some("0"))
    {
        return Err(Error::Configuration);
    }
    Ok(())
}

impl Storage {
    #[cfg(test)]
    pub(crate) fn fixture(&mut self, base: Url) {
        self.base = base;
        self.credentials = GoogleCloudStorageBuilder::new()
            .with_bucket_name(&self.buckets[0])
            .with_skip_signature(true)
            .build()
            .unwrap();
    }
    pub fn new(config: &Config) -> Result<Self, Error> {
        let credentials = GoogleCloudStorageBuilder::new()
            .with_bucket_name(&config.buckets[0])
            .build()
            .map_err(|_| Error::Configuration)?;
        Ok(Self {
            client: config.client()?,
            credentials,
            buckets: config.buckets.clone(),
            base: Url::parse("https://storage.googleapis.com/storage/v1/")
                .map_err(|_| Error::Configuration)?,
        })
    }

    async fn request(&self, method: Method, url: Url) -> Result<Value, Error> {
        let credential = self
            .credentials
            .credentials()
            .get_credential()
            .await
            .map_err(|_| Error::Unavailable)?;
        let mut request = self.client.request(method.clone(), url);
        if !credential.bearer.is_empty() {
            request = request.bearer_auth(&credential.bearer)
        }
        let response = request.send().await.map_err(|_| Error::Unavailable)?;
        if method == Method::DELETE
            && (response.status().is_success()
                || response.status() == reqwest::StatusCode::NOT_FOUND)
        {
            return Ok(Value::Null);
        }
        if !response.status().is_success() {
            return Err(Error::Unavailable);
        }
        crate::upstream::response_json(response).await
    }

    fn url(&self, bucket: &str, object: Option<&str>) -> Result<Url, Error> {
        let mut url = self.base.clone();
        let mut path = url.path_segments_mut().map_err(|_| Error::Configuration)?;
        path.pop_if_empty().push("b").push(bucket);
        if let Some(object) = object {
            path.push("o");
            if !object.is_empty() {
                path.push(object);
            }
        }
        drop(path);
        Ok(url)
    }

    async fn policy(&self, bucket: &str) -> Result<(), Error> {
        let value = self.request(Method::GET, self.url(bucket, None)?).await?;
        if value.get("name").and_then(Value::as_str) != Some(bucket) {
            return Err(Error::Configuration);
        }
        validate_policy(&value)
    }

    pub async fn ready(&self) -> Result<(), Error> {
        for bucket in &self.buckets {
            self.policy(bucket).await?;
        }
        Ok(())
    }

    pub async fn purge(&self, tenant: &str, all: bool) -> Result<(), Error> {
        let suffix = tenant.strip_prefix("ten_SBX_").ok_or(Error::Invalid)?;
        if suffix.len() != 32
            || !suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Invalid);
        }
        let prefix = format!("products/{PRODUCT}/tenants/{tenant}/");
        for bucket in &self.buckets[..if all { 4 } else { 2 }] {
            self.policy(bucket).await?;
            let mut remaining = 64usize;
            loop {
                let mut list = self.url(bucket, Some(""))?;
                list.query_pairs_mut()
                    .append_pair("prefix", &prefix)
                    .append_pair("versions", "true")
                    .append_pair("maxResults", "64");
                let result = self.request(Method::GET, list).await?;
                let objects = result.get("items").and_then(Value::as_array);
                if objects.is_none_or(|items| items.is_empty()) {
                    if result.get("nextPageToken").is_some() {
                        return Err(Error::Unavailable);
                    }
                    break;
                }
                for object in objects.into_iter().flatten() {
                    if remaining == 0 {
                        return Err(Error::Unavailable);
                    }
                    remaining -= 1;
                    let name = object
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|s| s.starts_with(&prefix))
                        .ok_or(Error::Unavailable)?;
                    let generation = object
                        .get("generation")
                        .and_then(Value::as_str)
                        .filter(|s| {
                            !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit())
                        })
                        .ok_or(Error::Unavailable)?;
                    let mut delete = self.url(bucket, Some(name))?;
                    delete
                        .query_pairs_mut()
                        .append_pair("generation", generation)
                        .append_pair("ifGenerationMatch", generation);
                    self.request(Method::DELETE, delete).await?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Storage;
    use axum::{
        extract::State,
        http::{Method, StatusCode, Uri},
        routing::any,
        Json, Router,
    };
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    #[test]
    fn deletion_policy_rejects_recoverable_images() {
        assert!(super::validate_policy(&json!({})).is_ok());
        for value in [
            json!({"versioning":{"enabled":true}}),
            json!({"softDeletePolicy":{"retentionDurationSeconds":"604800"}}),
            json!({"retentionPolicy":{"retentionPeriod":"86400"}}),
            json!({"defaultEventBasedHold":true}),
        ] {
            assert!(super::validate_policy(&value).is_err());
        }
    }

    type DeletionFixture = Arc<Mutex<(bool, Vec<String>, Vec<String>)>>;

    #[tokio::test]
    async fn deletes_exact_generations_and_never_reports_a_failed_delete_as_success() {
        let tenant = "ten_SBX_0123456789abcdef0123456789abcdef";
        let name = format!("products/ocr-sandbox/tenants/{tenant}/quarantine/upl_fixture");
        let state = Arc::new(Mutex::new((
            true,
            vec!["123".to_owned(), "456".to_owned()],
            Vec::<String>::new(),
        )));
        let objects = state.clone();
        let app=Router::new().fallback(any(move |State(state):State<DeletionFixture>,method:Method,uri:Uri| {
            let name=name.clone();async move {
                let mut data=state.lock().unwrap();
                if method==Method::DELETE {
                    let pairs:std::collections::HashMap<_,_>=url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes()).into_owned().collect();
                    assert_eq!(pairs.get("generation"),pairs.get("ifGenerationMatch"));
                    if data.0{return (StatusCode::SERVICE_UNAVAILABLE,Json(json!({})));}
                    let generation=pairs.get("generation").unwrap();data.1.retain(|v|v!=generation);data.2.push(generation.clone());
                    return (StatusCode::NO_CONTENT,Json(json!({})));
                }
                if uri.path().ends_with("/o") {return (StatusCode::OK,Json(json!({"items":data.1.iter().map(|g|json!({"name":name,"generation":g})).collect::<Vec<_>>()})));}
                (StatusCode::OK,Json(json!({"name":uri.path().rsplit('/').next().unwrap()})))
            }
        })).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let storage = Storage {
            client: reqwest::Client::new(),
            credentials: object_store::gcp::GoogleCloudStorageBuilder::new()
                .with_bucket_name("ocr-sandbox-test")
                .with_skip_signature(true)
                .build()
                .unwrap(),
            buckets: [
                "ocr-sandbox-q".into(),
                "ocr-sandbox-s".into(),
                "ocr-sandbox-p".into(),
                "ocr-sandbox-r".into(),
            ],
            base: reqwest::Url::parse(&format!("http://{address}/storage/v1/")).unwrap(),
        };
        assert!(storage.purge(tenant, false).await.is_err());
        assert_eq!(objects.lock().unwrap().1.len(), 2);
        objects.lock().unwrap().0 = false;
        storage.purge(tenant, false).await.unwrap();
        assert_eq!(objects.lock().unwrap().2, vec!["123", "456"]);
        storage.purge(tenant, false).await.unwrap();
        assert!(storage.purge("ten_OTHER", true).await.is_err());
        server.abort();
    }
}

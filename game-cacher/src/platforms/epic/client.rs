//! Minimal Epic launcher API client (protocol per legendary's `api/egs.py`).

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Epic launcher client credentials (public, same ones legendary uses).
pub const CLIENT_ID: &str = "34a02cf8f4414e29b15921876da36f9a";
const CLIENT_SECRET: &str = "daafbccc737745039dffe53d94fc76cf";
const USER_AGENT: &str =
    "UELauncher/11.0.1-14907503+++Portal+Release-Live Windows/10.0.19041.1.256.64bit";
const CATALOG_BATCH: usize = 50;

#[derive(Debug, Clone)]
pub struct Endpoints {
    pub oauth: String,
    pub launcher: String,
    pub catalog: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            oauth: "https://account-public-service-prod03.ol.epicgames.com".into(),
            launcher: "https://launcher-public-service-prod06.ol.epicgames.com".into(),
            catalog: "https://catalog-public-service-prod06.ol.epicgames.com".into(),
        }
    }
}

/// The authorization code / refresh token was refused by Epic.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Rejected(pub String);

#[derive(Debug, Clone, Deserialize)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub expires_in: u64,
    #[serde(default)]
    pub account_id: String,
    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    pub app_name: String,
    #[serde(default)]
    pub build_version: String,
    #[serde(default)]
    pub catalog_item_id: String,
    #[serde(default)]
    pub namespace: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestEntry {
    pub uri: String,
    #[serde(default, rename = "queryParams")]
    pub query_params: Vec<QueryParam>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QueryParam {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
struct ManifestList {
    #[serde(default)]
    elements: Vec<ManifestElement>,
}

#[derive(Debug, Deserialize)]
struct ManifestElement {
    #[serde(default)]
    manifests: Vec<ManifestEntry>,
}

pub struct Client<'a> {
    pub http: &'a reqwest::Client,
    pub ep: &'a Endpoints,
}

impl Client<'_> {
    /// POST to the OAuth token endpoint. 4xx answers are `Rejected`.
    pub async fn token(&self, params: &[(&str, &str)]) -> Result<Token> {
        let resp = self
            .http
            .post(format!("{}/account/api/oauth/token", self.ep.oauth))
            .basic_auth(CLIENT_ID, Some(CLIENT_SECRET))
            .header("User-Agent", USER_AGENT)
            .form(params)
            .send()
            .await
            .context("contacting Epic account service")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if status.is_client_error() {
            let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let msg = v["errorMessage"]
                .as_str()
                .or_else(|| v["errorCode"].as_str())
                .unwrap_or("request rejected");
            return Err(Rejected(msg.to_string()).into());
        }
        if !status.is_success() {
            bail!("Epic account service returned HTTP {status}");
        }
        serde_json::from_str(&body).context("parsing Epic token response")
    }

    pub async fn kill_session(&self, access_token: &str) -> Result<()> {
        self.http
            .delete(format!(
                "{}/account/api/oauth/sessions/kill/{access_token}",
                self.ep.oauth
            ))
            .bearer_auth(access_token)
            .send()
            .await?;
        Ok(())
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: String,
        query: &[(&str, &str)],
        token: &str,
    ) -> Result<T> {
        let resp = self
            .http
            .get(&url)
            .query(query)
            .bearer_auth(token)
            .header("User-Agent", USER_AGENT)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            bail!("GET {url} returned HTTP {status}");
        }
        resp.json().await.with_context(|| format!("parsing {url}"))
    }

    pub async fn assets(&self, token: &str) -> Result<Vec<Asset>> {
        let url = format!("{}/launcher/api/public/assets/Windows", self.ep.launcher);
        self.get_json(url, &[("label", "Live")], token).await
    }

    /// Titles for catalog items of one namespace (`catalog_item_id -> title`).
    pub async fn titles(
        &self,
        token: &str,
        namespace: &str,
        ids: &[String],
    ) -> Result<HashMap<String, String>> {
        let mut out = HashMap::new();
        for batch in ids.chunks(CATALOG_BATCH) {
            let mut q: Vec<(&str, &str)> = batch.iter().map(|i| ("id", i.as_str())).collect();
            q.extend([
                ("includeDLCDetails", "false"),
                ("includeMainGameDetails", "false"),
                ("country", "US"),
                ("locale", "en-US"),
            ]);
            let url = format!(
                "{}/catalog/api/shared/namespace/{namespace}/bulk/items",
                self.ep.catalog
            );
            let v: HashMap<String, serde_json::Value> = self.get_json(url, &q, token).await?;
            for (id, item) in v {
                if let Some(t) = item["title"].as_str() {
                    out.insert(id, t.to_string());
                }
            }
        }
        Ok(out)
    }

    pub async fn manifests(&self, token: &str, asset: &Asset) -> Result<Vec<ManifestEntry>> {
        let url = format!(
            "{}/launcher/api/public/assets/v2/platform/Windows/namespace/{}/catalogItem/{}/app/{}/label/Live",
            self.ep.launcher, asset.namespace, asset.catalog_item_id, asset.app_name
        );
        let list: ManifestList = self.get_json(url, &[], token).await?;
        Ok(list
            .elements
            .into_iter()
            .flat_map(|e| e.manifests)
            .collect())
    }
}

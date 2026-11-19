//! HashiCorp Vault KV v2 over HTTP (token or AppRole auth).
use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::{Method, Response, StatusCode};
use serde_json::{Value, json};
use tokio::sync::RwLock;

use super::{SecretStore, validate_name};
use crate::config::{Config, EnvSecrets};

#[derive(Debug, Clone)]
pub enum VaultAuth {
    Token(String),
    AppRole { role_id: String, secret_id: String },
}

pub struct VaultSecrets {
    client: reqwest::Client,
    addr: String,
    mount: String,
    path: String,
    auth: VaultAuth,
    token: RwLock<String>,
}

impl VaultSecrets {
    pub async fn new(addr: &str, mount: &str, path: &str, auth: VaultAuth) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        let addr = addr.trim_end_matches('/').to_string();
        let token = match &auth {
            VaultAuth::Token(t) => t.clone(),
            VaultAuth::AppRole { role_id, secret_id } => {
                login(&client, &addr, role_id, secret_id).await?
            }
        };
        Ok(Self {
            client,
            addr,
            mount: mount.trim_matches('/').to_string(),
            path: path.trim_matches('/').to_string(),
            auth,
            token: RwLock::new(token),
        })
    }

    /// Token auth is preferred over AppRole when both are configured.
    pub async fn from_config(config: &Config, env: &EnvSecrets) -> Result<Self> {
        let addr = config
            .vault_addr
            .as_deref()
            .context("--vault-addr / GC_VAULT_ADDR is required with --secrets vault")?;
        let auth = match (&env.vault_token, &env.vault_role_id, &env.vault_secret_id) {
            (Some(t), _, _) => VaultAuth::Token(t.clone()),
            (None, Some(r), Some(s)) => VaultAuth::AppRole {
                role_id: r.clone(),
                secret_id: s.clone(),
            },
            _ => anyhow::bail!("Vault needs VAULT_TOKEN or VAULT_ROLE_ID + VAULT_SECRET_ID"),
        };
        Self::new(addr, &config.vault_mount, &config.vault_path, auth).await
    }

    fn url(&self, kind: &str, name: &str) -> Result<String> {
        validate_name(name)?;
        let sep = if self.path.is_empty() { "" } else { "/" };
        Ok(format!(
            "{}/v1/{}/{kind}/{}{sep}{name}",
            self.addr, self.mount, self.path
        ))
    }

    async fn send_once(
        &self,
        method: &Method,
        url: &str,
        body: Option<&Value>,
    ) -> Result<Response> {
        let token = self.token.read().await.clone();
        let mut req = self
            .client
            .request(method.clone(), url)
            .header("X-Vault-Token", token);
        if let Some(b) = body {
            req = req.json(b);
        }
        req.send().await.context("Vault request failed")
    }

    /// Sends a request; on 403 with AppRole auth, re-logs in once and retries.
    async fn send(&self, method: Method, url: &str, body: Option<&Value>) -> Result<Response> {
        let resp = self.send_once(&method, url, body).await?;
        if resp.status() != StatusCode::FORBIDDEN {
            return Ok(resp);
        }
        let VaultAuth::AppRole { role_id, secret_id } = &self.auth else {
            return Ok(resp);
        };
        let new = login(&self.client, &self.addr, role_id, secret_id).await?;
        *self.token.write().await = new;
        self.send_once(&method, url, body).await
    }
}

async fn login(
    client: &reqwest::Client,
    addr: &str,
    role_id: &str,
    secret_id: &str,
) -> Result<String> {
    let resp = client
        .post(format!("{addr}/v1/auth/approle/login"))
        .json(&json!({"role_id": role_id, "secret_id": secret_id}))
        .send()
        .await
        .context("Vault AppRole login request failed")?;
    let resp = check(resp, "AppRole login").await?;
    let v: Value = resp.json().await.context("invalid Vault login response")?;
    v["auth"]["client_token"]
        .as_str()
        .map(str::to_string)
        .context("Vault login response has no auth.client_token")
}

/// Turns a non-success response into an error including Vault's `errors` text.
async fn check(resp: Response, what: &str) -> Result<Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let text = resp.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| {
            v["errors"].as_array().map(|a| {
                a.iter()
                    .filter_map(|e| e.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            })
        })
        .filter(|s| !s.is_empty())
        .unwrap_or(text);
    anyhow::bail!("Vault {what} failed: {status}: {detail}")
}

#[async_trait]
impl SecretStore for VaultSecrets {
    fn name(&self) -> &'static str {
        "vault"
    }

    async fn get(&self, name: &str) -> Result<Option<String>> {
        let resp = self
            .send(Method::GET, &self.url("data", name)?, None)
            .await?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let v: Value = check(resp, "read").await?.json().await?;
        Ok(v["data"]["data"]["value"].as_str().map(str::to_string))
    }

    async fn put(&self, name: &str, value: &str) -> Result<()> {
        let body = json!({"data": {"value": value}});
        let resp = self
            .send(Method::POST, &self.url("data", name)?, Some(&body))
            .await?;
        check(resp, "write").await?;
        Ok(())
    }

    async fn delete(&self, name: &str) -> Result<()> {
        let resp = self
            .send(Method::DELETE, &self.url("metadata", name)?, None)
            .await?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(());
        }
        check(resp, "delete").await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    async fn token_store(server: &MockServer) -> VaultSecrets {
        VaultSecrets::new(
            &server.uri(),
            "secret",
            "gc",
            VaultAuth::Token("tok".into()),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn read_sends_token_and_parses_value() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/secret/data/gc/steam.token"))
            .and(header("X-Vault-Token", "tok"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"data": {"data": {"value": "abc"}, "metadata": {}}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let s = token_store(&server).await;
        assert_eq!(s.get("steam.token").await.unwrap().unwrap(), "abc");
    }

    #[tokio::test]
    async fn read_404_is_none_and_delete_404_ok() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({"errors": []})))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/v1/secret/metadata/gc/x"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let s = token_store(&server).await;
        assert_eq!(s.get("x").await.unwrap(), None);
        s.delete("x").await.unwrap();
    }

    #[tokio::test]
    async fn write_and_delete_requests() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/secret/data/gc/x"))
            .and(header("X-Vault-Token", "tok"))
            .and(body_json(json!({"data": {"value": "v"}})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {}})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/v1/secret/metadata/gc/x"))
            .and(header("X-Vault-Token", "tok"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let s = token_store(&server).await;
        s.put("x", "v").await.unwrap();
        s.delete("x").await.unwrap();
    }

    #[tokio::test]
    async fn errors_include_vault_message_and_token_403_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(403).set_body_json(json!({"errors": ["permission denied"]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let s = token_store(&server).await;
        let err = s.get("x").await.unwrap_err().to_string();
        assert!(err.contains("permission denied"), "{err}");
    }

    #[tokio::test]
    async fn approle_logs_in_and_relogins_on_403() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/approle/login"))
            .and(body_json(json!({"role_id": "r", "secret_id": "s"})))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"auth": {"client_token": "t1"}})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/approle/login"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"auth": {"client_token": "t2"}})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(header("X-Vault-Token", "t1"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({"errors": ["expired"]})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(header("X-Vault-Token", "t2"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"data": {"data": {"value": "ok"}}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let auth = VaultAuth::AppRole {
            role_id: "r".into(),
            secret_id: "s".into(),
        };
        let s = VaultSecrets::new(&server.uri(), "secret", "gc", auth)
            .await
            .unwrap();
        assert_eq!(s.get("x").await.unwrap().unwrap(), "ok");
    }

    #[tokio::test]
    async fn approle_login_failure_reported() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/approle/login"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({"errors": ["invalid role_id"]})),
            )
            .mount(&server)
            .await;
        let auth = VaultAuth::AppRole {
            role_id: "r".into(),
            secret_id: "s".into(),
        };
        let err = VaultSecrets::new(&server.uri(), "secret", "gc", auth)
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("invalid role_id"), "{err}");
    }

    #[tokio::test]
    async fn rejects_invalid_names() {
        let server = MockServer::start().await;
        let s = token_store(&server).await;
        assert!(s.get("../x").await.is_err());
    }
}

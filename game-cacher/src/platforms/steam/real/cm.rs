//! CM (Steam connection manager) session via steam-vent: refresh-token logon,
//! license list, PICS, manifest request codes and CDN server list.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use futures::StreamExt;
use steam_vent::connection::UnAuthenticatedConnection;
use steam_vent::{Connection, ConnectionTrait, ReadonlyConnection, ServerList};
use steam_vent_proto_common::protobuf::EnumOrUnknown;
use steam_vent_proto_steam::steammessages_auth_steamclient::{
    CAuthentication_AccessToken_GenerateForApp_Request, ETokenRenewalType,
};
use steam_vent_proto_steam::steammessages_clientserver::CMsgClientLicenseList;
use steam_vent_proto_steam::steammessages_clientserver_appinfo::{
    CMsgClientPICSAccessTokenRequest, CMsgClientPICSAccessTokenResponse,
    CMsgClientPICSProductInfoRequest, CMsgClientPICSProductInfoResponse,
    cmsg_client_picsproduct_info_request,
};
use steam_vent_proto_steam::steammessages_contentsystem_steamclient::{
    CContentServerDirectory_GetManifestRequestCode_Request,
    CContentServerDirectory_GetServersForSteamPipe_Request,
};
use tokio::sync::OnceCell;
use tokio::time::timeout;

use super::super::appinfo::{self, AppInfo};
use super::super::backend::{CdnServer, Ownership, SteamSession};
use super::super::kv;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const LICENSE_TIMEOUT: Duration = Duration::from_secs(20);
const PICS_BATCH: usize = 100;

pub struct CmSession {
    conn: Connection,
    /// (package id, access token) from CMsgClientLicenseList.
    licenses: Vec<(u32, u64)>,
    renewed: Option<String>,
    ownership: OnceCell<Ownership>,
}

/// rustls is built with both ring and aws-lc-rs in this binary (via different
/// dependencies), so a process default provider must exist before
/// steam-vent's websocket transport builds a `ClientConfig`.
fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub async fn connect(account: &str, refresh_token: &str) -> Result<CmSession> {
    ensure_crypto_provider();
    timeout(CONNECT_TIMEOUT, connect_inner(account, refresh_token))
        .await
        .map_err(|_| anyhow!("timed out connecting to Steam"))?
}

async fn connect_inner(account: &str, refresh_token: &str) -> Result<CmSession> {
    let servers = ServerList::discover()
        .await
        .context("discovering Steam CM servers")?;
    let unauth = UnAuthenticatedConnection::connect(&servers)
        .await
        .context("connecting to Steam CM")?;
    // Register before logon: Steam pushes the license list right after it.
    let licenses = unauth.one::<CMsgClientLicenseList>();
    // steam-vent sends this as CMsgClientLogon.access_token, which is where
    // Steam expects the refresh token for client logons.
    let mut conn = unauth
        .access(account, refresh_token)
        .await
        .context("logging on with refresh token")?;
    conn.set_timeout(REQUEST_TIMEOUT);
    let licenses = timeout(LICENSE_TIMEOUT, licenses)
        .await
        .map_err(|_| anyhow!("timed out waiting for the license list"))?
        .context("reading license list")?;
    let licenses = licenses
        .licenses
        .iter()
        .map(|l| (l.package_id(), l.access_token()))
        .collect();
    let renewed = match renew_refresh_token(&conn, refresh_token).await {
        Ok(t) => t,
        Err(e) => {
            log::debug!("steam refresh token renewal skipped: {e:#}");
            None
        }
    };
    Ok(CmSession {
        conn,
        licenses,
        renewed,
        ownership: OnceCell::new(),
    })
}

/// GenerateAccessTokenForApp with renewal allowed; Steam returns a new refresh
/// token only when the current one is close to expiry. Must run over an
/// authenticated CM session for SteamClient tokens.
async fn renew_refresh_token(conn: &Connection, refresh_token: &str) -> Result<Option<String>> {
    let req = CAuthentication_AccessToken_GenerateForApp_Request {
        refresh_token: Some(refresh_token.into()),
        steamid: Some(u64::from(conn.steam_id())),
        renewal_type: Some(EnumOrUnknown::new(
            ETokenRenewalType::k_ETokenRenewalType_Allow,
        )),
        ..Default::default()
    };
    let resp = conn.service_method(req).await?;
    Ok(Some(resp.refresh_token().to_string()).filter(|t| !t.is_empty()))
}

impl CmSession {
    async fn product_info(
        &self,
        req: CMsgClientPICSProductInfoRequest,
    ) -> Result<Vec<CMsgClientPICSProductInfoResponse>> {
        let stream = self
            .conn
            .job_multi::<_, CMsgClientPICSProductInfoResponse>(req);
        futures::pin_mut!(stream);
        let mut out = Vec::new();
        while let Some(resp) = stream.next().await {
            out.push(resp.context("PICS product info")?);
        }
        Ok(out)
    }

    async fn load_ownership(&self) -> Result<Ownership> {
        let mut owned = Ownership::default();
        for batch in self.licenses.chunks(PICS_BATCH) {
            let req = CMsgClientPICSProductInfoRequest {
                packages: batch
                    .iter()
                    .map(
                        |&(id, token)| cmsg_client_picsproduct_info_request::PackageInfo {
                            packageid: Some(id),
                            access_token: Some(token),
                            ..Default::default()
                        },
                    )
                    .collect(),
                meta_data_only: Some(false),
                ..Default::default()
            };
            for resp in self.product_info(req).await? {
                for pkg in &resp.packages {
                    // Package buffers are a u32 followed by binary KeyValues.
                    let Some(buf) = pkg.buffer.as_deref().filter(|b| b.len() > 4) else {
                        continue;
                    };
                    match kv::parse_binary(&buf[4..]) {
                        Ok((_, kv)) => {
                            let (apps, depots) = appinfo::package_ids(&kv);
                            owned.app_ids.extend(apps);
                            owned.depot_ids.extend(depots);
                        }
                        Err(e) => log::warn!("package {}: {e:#}", pkg.packageid()),
                    }
                }
            }
        }
        Ok(owned)
    }

    async fn access_tokens(&self, app_ids: &[u32]) -> Result<HashMap<u32, u64>> {
        let req = CMsgClientPICSAccessTokenRequest {
            appids: app_ids.to_vec(),
            ..Default::default()
        };
        let resp: CMsgClientPICSAccessTokenResponse =
            self.conn.job(req).await.context("PICS access tokens")?;
        Ok(resp
            .app_access_tokens
            .iter()
            .map(|t| (t.appid(), t.access_token()))
            .collect())
    }
}

#[async_trait]
impl SteamSession for CmSession {
    fn renewed_refresh_token(&self) -> Option<String> {
        self.renewed.clone()
    }

    async fn ownership(&self) -> Result<Ownership> {
        self.ownership
            .get_or_try_init(|| self.load_ownership())
            .await
            .cloned()
    }

    async fn app_info(&self, app_ids: &[u32]) -> Result<Vec<AppInfo>> {
        let mut out = Vec::new();
        for batch in app_ids.chunks(PICS_BATCH) {
            let tokens = self.access_tokens(batch).await?;
            let req = CMsgClientPICSProductInfoRequest {
                apps: batch
                    .iter()
                    .map(|&id| cmsg_client_picsproduct_info_request::AppInfo {
                        appid: Some(id),
                        access_token: Some(tokens.get(&id).copied().unwrap_or(0)),
                        ..Default::default()
                    })
                    .collect(),
                meta_data_only: Some(false),
                ..Default::default()
            };
            for resp in self.product_info(req).await? {
                for app in &resp.apps {
                    let Some(buf) = app.buffer.as_deref().filter(|b| !b.is_empty()) else {
                        continue;
                    };
                    let text = String::from_utf8_lossy(buf);
                    match kv::parse_text(&text) {
                        Ok((_, kv)) => out.push(appinfo::parse_app_info(app.appid(), &kv)),
                        Err(e) => log::warn!("app {}: bad app info: {e:#}", app.appid()),
                    }
                }
            }
        }
        Ok(out)
    }

    async fn manifest_request_code(
        &self,
        app_id: u32,
        depot_id: u32,
        manifest_gid: u64,
        branch: &str,
    ) -> Result<u64> {
        let req = CContentServerDirectory_GetManifestRequestCode_Request {
            app_id: Some(app_id),
            depot_id: Some(depot_id),
            manifest_id: Some(manifest_gid),
            app_branch: Some(branch.into()),
            ..Default::default()
        };
        let resp = self.conn.service_method(req).await?;
        Ok(resp.manifest_request_code())
    }

    async fn cdn_servers(&self) -> Result<Vec<CdnServer>> {
        let req = CContentServerDirectory_GetServersForSteamPipe_Request {
            cell_id: Some(self.conn.cell_id()),
            max_servers: Some(20),
            ..Default::default()
        };
        let resp = self.conn.service_method(req).await?;
        Ok(resp
            .servers
            .iter()
            .map(|s| CdnServer {
                kind: s.type_().to_string(),
                host: s.host().to_string(),
                vhost: s.vhost().to_string(),
                https_support: s.https_support().to_string(),
                allowed_app_ids: s.allowed_app_ids.clone(),
                weighted_load: s.weighted_load(),
                use_as_proxy: s.use_as_proxy(),
            })
            .collect())
    }
}

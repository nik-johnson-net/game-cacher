//! IAuthenticationService over the HTTPS Web API (api.steampowered.com):
//! protobuf requests as `input_protobuf_encoded` (base64), protobuf responses,
//! result code in the `x-eresult` header (node-steam-session's WebApiTransport).

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use steam_vent_proto_common::protobuf::{EnumOrUnknown, Message, MessageField};
use steam_vent_proto_steam::enums::ESessionPersistence;
use steam_vent_proto_steam::steammessages_auth_steamclient::{
    CAuthentication_AllowedConfirmation, CAuthentication_BeginAuthSessionViaCredentials_Request,
    CAuthentication_BeginAuthSessionViaCredentials_Response,
    CAuthentication_BeginAuthSessionViaQR_Request, CAuthentication_BeginAuthSessionViaQR_Response,
    CAuthentication_DeviceDetails, CAuthentication_GetPasswordRSAPublicKey_Request,
    CAuthentication_GetPasswordRSAPublicKey_Response,
    CAuthentication_PollAuthSessionStatus_Request, CAuthentication_PollAuthSessionStatus_Response,
    CAuthentication_UpdateAuthSessionWithSteamGuardCode_Request,
    CAuthentication_UpdateAuthSessionWithSteamGuardCode_Response, EAuthSessionGuardType,
    EAuthTokenPlatformType,
};

use super::super::backend::{AuthSessionStart, GuardKind, PollResult, RsaKey, SteamAuth};

pub const DEFAULT_API_BASE: &str = "https://api.steampowered.com";
const DEVICE_NAME: &str = "game-cacher";
const WEBSITE_ID: &str = "Client";
/// EOSType Win11.
const OS_TYPE_WIN11: i32 = 20;

pub struct WebApiAuth {
    http: reqwest::Client,
    base: String,
}

impl WebApiAuth {
    pub fn new(http: reqwest::Client, base: impl Into<String>) -> Self {
        Self {
            http,
            base: base.into(),
        }
    }

    async fn call<Req: Message, Resp: Message>(
        &self,
        method: &str,
        get: bool,
        req: &Req,
    ) -> Result<Resp> {
        let url = format!("{}/IAuthenticationService/{method}/v1/", self.base);
        let encoded = BASE64_STANDARD.encode(req.write_to_bytes()?);
        let params = [("input_protobuf_encoded", encoded)];
        let builder = if get {
            self.http.get(&url).query(&params)
        } else {
            self.http.post(&url).form(&params)
        };
        let resp = builder
            .send()
            .await
            .with_context(|| format!("calling {method}"))?;
        let status = resp.status();
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let eresult: Option<i32> = header("x-eresult").and_then(|v| v.parse().ok());
        let message = header("x-error_message");
        if !status.is_success() {
            bail!("{method}: HTTP {status}");
        }
        if let Some(code) = eresult.filter(|&c| c != 1) {
            bail!("{method}: {}", describe_eresult(code, message.as_deref()));
        }
        let body = resp.bytes().await?;
        Resp::parse_from_bytes(&body).with_context(|| format!("decoding {method} response"))
    }
}

/// Human-readable text for the EResult codes the login flow commonly hits.
pub fn describe_eresult(code: i32, message: Option<&str>) -> String {
    let text = match code {
        5 => "invalid password",
        9 => "login session not found or expired",
        15 => "access denied",
        20 => "Steam service unavailable",
        27 => "login session expired",
        29 => "duplicate request",
        63 => "account logon denied (Steam Guard required)",
        65 => "invalid Steam Guard code",
        84 => "rate limit exceeded; try again later",
        88 => "invalid two-factor code",
        _ => "",
    };
    match (text, message.filter(|m| !m.is_empty())) {
        ("", Some(m)) => format!("{m} (EResult {code})"),
        ("", None) => format!("EResult {code}"),
        (t, _) => format!("{t} (EResult {code})"),
    }
}

fn device_details() -> MessageField<CAuthentication_DeviceDetails> {
    MessageField::some(CAuthentication_DeviceDetails {
        device_friendly_name: Some(DEVICE_NAME.into()),
        platform_type: Some(EnumOrUnknown::new(
            EAuthTokenPlatformType::k_EAuthTokenPlatformType_SteamClient,
        )),
        os_type: Some(OS_TYPE_WIN11),
        gaming_device_type: Some(1),
        ..Default::default()
    })
}

fn confirmations(list: &[CAuthentication_AllowedConfirmation]) -> Vec<GuardKind> {
    list.iter()
        .map(|c| GuardKind::from_proto(c.confirmation_type.map(|t| t.value()).unwrap_or(0)))
        .collect()
}

#[async_trait]
impl SteamAuth for WebApiAuth {
    async fn password_rsa_key(&self, account: &str) -> Result<RsaKey> {
        let req = CAuthentication_GetPasswordRSAPublicKey_Request {
            account_name: Some(account.into()),
            ..Default::default()
        };
        let resp: CAuthentication_GetPasswordRSAPublicKey_Response =
            self.call("GetPasswordRSAPublicKey", true, &req).await?;
        Ok(RsaKey {
            modulus_hex: resp.publickey_mod().to_string(),
            exponent_hex: resp.publickey_exp().to_string(),
            timestamp: resp.timestamp(),
        })
    }

    async fn begin_qr(&self) -> Result<AuthSessionStart> {
        let req = CAuthentication_BeginAuthSessionViaQR_Request {
            device_friendly_name: Some(DEVICE_NAME.into()),
            platform_type: Some(EnumOrUnknown::new(
                EAuthTokenPlatformType::k_EAuthTokenPlatformType_SteamClient,
            )),
            device_details: device_details(),
            website_id: Some(WEBSITE_ID.into()),
            ..Default::default()
        };
        let resp: CAuthentication_BeginAuthSessionViaQR_Response =
            self.call("BeginAuthSessionViaQR", false, &req).await?;
        Ok(AuthSessionStart {
            client_id: resp.client_id(),
            request_id: resp.request_id().to_vec(),
            interval: resp.interval(),
            allowed_confirmations: confirmations(&resp.allowed_confirmations),
            steam_id: 0,
            challenge_url: resp.challenge_url().to_string(),
        })
    }

    async fn begin_credentials(
        &self,
        account: &str,
        encrypted_password_b64: &str,
        timestamp: u64,
    ) -> Result<AuthSessionStart> {
        let req = CAuthentication_BeginAuthSessionViaCredentials_Request {
            device_friendly_name: Some(DEVICE_NAME.into()),
            account_name: Some(account.into()),
            encrypted_password: Some(encrypted_password_b64.into()),
            encryption_timestamp: Some(timestamp),
            remember_login: Some(true),
            platform_type: Some(EnumOrUnknown::new(
                EAuthTokenPlatformType::k_EAuthTokenPlatformType_SteamClient,
            )),
            persistence: Some(EnumOrUnknown::new(
                ESessionPersistence::k_ESessionPersistence_Persistent,
            )),
            website_id: Some(WEBSITE_ID.into()),
            device_details: device_details(),
            ..Default::default()
        };
        let resp: CAuthentication_BeginAuthSessionViaCredentials_Response = self
            .call("BeginAuthSessionViaCredentials", false, &req)
            .await?;
        Ok(AuthSessionStart {
            client_id: resp.client_id(),
            request_id: resp.request_id().to_vec(),
            interval: resp.interval(),
            allowed_confirmations: confirmations(&resp.allowed_confirmations),
            steam_id: resp.steamid(),
            challenge_url: String::new(),
        })
    }

    async fn submit_guard_code(
        &self,
        client_id: u64,
        steam_id: u64,
        code: &str,
        kind: GuardKind,
    ) -> Result<()> {
        let code_type = match kind {
            GuardKind::EmailCode => EAuthSessionGuardType::k_EAuthSessionGuardType_EmailCode,
            _ => EAuthSessionGuardType::k_EAuthSessionGuardType_DeviceCode,
        };
        let req = CAuthentication_UpdateAuthSessionWithSteamGuardCode_Request {
            client_id: Some(client_id),
            steamid: Some(steam_id),
            code: Some(code.into()),
            code_type: Some(EnumOrUnknown::new(code_type)),
            ..Default::default()
        };
        let _: CAuthentication_UpdateAuthSessionWithSteamGuardCode_Response = self
            .call("UpdateAuthSessionWithSteamGuardCode", false, &req)
            .await?;
        Ok(())
    }

    async fn poll(&self, client_id: u64, request_id: &[u8]) -> Result<PollResult> {
        let req = CAuthentication_PollAuthSessionStatus_Request {
            client_id: Some(client_id),
            request_id: Some(request_id.to_vec()),
            ..Default::default()
        };
        let resp: CAuthentication_PollAuthSessionStatus_Response =
            self.call("PollAuthSessionStatus", false, &req).await?;
        let non_empty = |s: &str| (!s.is_empty()).then(|| s.to_string());
        Ok(PollResult {
            new_client_id: resp.new_client_id,
            new_challenge_url: non_empty(resp.new_challenge_url()),
            refresh_token: non_empty(resp.refresh_token()),
            account_name: non_empty(resp.account_name()),
        })
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{body_string_contains, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn encoded<M: Message>(m: &M) -> String {
        BASE64_STANDARD.encode(m.write_to_bytes().unwrap())
    }

    #[tokio::test]
    async fn rsa_key_via_get_with_protobuf_body() {
        let server = MockServer::start().await;
        let req = CAuthentication_GetPasswordRSAPublicKey_Request {
            account_name: Some("alice".into()),
            ..Default::default()
        };
        let resp = CAuthentication_GetPasswordRSAPublicKey_Response {
            publickey_mod: Some("abcd".into()),
            publickey_exp: Some("010001".into()),
            timestamp: Some(42),
            ..Default::default()
        };
        Mock::given(method("GET"))
            .and(path("/IAuthenticationService/GetPasswordRSAPublicKey/v1/"))
            .and(query_param("input_protobuf_encoded", encoded(&req)))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-eresult", "1")
                    .set_body_bytes(resp.write_to_bytes().unwrap()),
            )
            .mount(&server)
            .await;
        let api = WebApiAuth::new(reqwest::Client::new(), server.uri());
        let key = api.password_rsa_key("alice").await.unwrap();
        assert_eq!(
            key,
            RsaKey {
                modulus_hex: "abcd".into(),
                exponent_hex: "010001".into(),
                timestamp: 42
            }
        );
    }

    #[tokio::test]
    async fn poll_posts_form_and_maps_eresult_errors() {
        let server = MockServer::start().await;
        let resp = CAuthentication_PollAuthSessionStatus_Response {
            new_challenge_url: Some("https://s.team/q/1/2".into()),
            refresh_token: Some(String::new()),
            ..Default::default()
        };
        Mock::given(method("POST"))
            .and(path("/IAuthenticationService/PollAuthSessionStatus/v1/"))
            .and(body_string_contains("input_protobuf_encoded="))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(resp.write_to_bytes().unwrap()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/IAuthenticationService/BeginAuthSessionViaQR/v1/"))
            .respond_with(ResponseTemplate::new(200).insert_header("x-eresult", "84"))
            .mount(&server)
            .await;
        let api = WebApiAuth::new(reqwest::Client::new(), server.uri());
        let poll = api.poll(7, b"req").await.unwrap();
        assert_eq!(
            poll.new_challenge_url.as_deref(),
            Some("https://s.team/q/1/2")
        );
        assert!(poll.refresh_token.is_none());
        let err = api.begin_qr().await.unwrap_err().to_string();
        assert!(err.contains("rate limit"), "{err}");
    }

    #[test]
    fn eresult_text() {
        assert_eq!(describe_eresult(5, None), "invalid password (EResult 5)");
        assert_eq!(describe_eresult(999, Some("boom")), "boom (EResult 999)");
        assert_eq!(describe_eresult(999, None), "EResult 999");
    }
}

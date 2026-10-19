//! Application identity is independent of a viewer capability.
use crate::core::{AppError, Principal};
use axum::http::HeaderMap;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
pub fn token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
pub fn error(status: u16, code: &str, message: &str) -> AppError {
    AppError {
        status,
        code: code.into(),
        message: message.into(),
    }
}
#[derive(Clone)]
pub struct Config {
    pub dev: bool,
    pub origin: String,
    pub tenant: Option<String>,
    pub client: Option<String>,
    pub secret: Option<String>,
    pub gated_tenant: Option<String>,
    pub signing_key: Option<String>,
}
impl Config {
    pub fn from_env(dev: bool, origin: String) -> Self {
        Self {
            dev,
            origin,
            tenant: std::env::var("ARTIFACT_ENTRA_TENANT").ok(),
            client: std::env::var("ARTIFACT_ENTRA_CLIENT_ID").ok(),
            secret: std::env::var("ARTIFACT_ENTRA_CLIENT_SECRET").ok(),
            gated_tenant: std::env::var("ARTIFACT_GATED_TENANT").ok(),
            signing_key: std::env::var("ARTIFACT_SESSION_KEY").ok(),
        }
    }
    pub fn configured(&self) -> bool {
        self.tenant
            .as_ref()
            .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
            && self.client.is_some()
            && self.secret.is_some()
            && self.signing_key.as_ref().is_some_and(|k| k.len() >= 64)
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    nonce: String,
    verifier: String,
    expires: u64,
    exp: u64,
    state: String,
    next: String,
}
#[derive(Serialize, Deserialize)]
struct SessionClaim {
    principal: Principal,
    gated: bool,
    exp: u64,
    aud: String,
}
#[derive(Serialize, Deserialize)]
struct GuestClaim {
    id: String,
    exp: u64,
    aud: String,
}
#[derive(Serialize, Deserialize)]
struct FrameClaim {
    cap: String,
    revision: String,
    exp: u64,
    aud: String,
}
pub struct Auth {
    pub config: Config,
    local_key: String,
    pending: Mutex<HashMap<String, Pending>>,
}
impl Auth {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            local_key: token(),
            pending: Mutex::new(HashMap::new()),
        }
    }
    fn secret(&self) -> &[u8] {
        self.config
            .signing_key
            .as_deref()
            .unwrap_or(&self.local_key)
            .as_bytes()
    }
    pub(crate) fn sign<T: Serialize>(&self, value: &T) -> String {
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(Algorithm::HS256),
            value,
            &jsonwebtoken::EncodingKey::from_secret(self.secret()),
        )
        .expect("Serializable signed claim")
    }
    pub(crate) fn decode<T: serde::de::DeserializeOwned>(
        &self,
        value: &str,
        audience: &str,
    ) -> Option<T> {
        let mut v = Validation::new(Algorithm::HS256);
        v.set_audience(&[audience]);
        v.leeway = 0;
        decode::<T>(value, &DecodingKey::from_secret(self.secret()), &v)
            .ok()
            .map(|t| t.claims)
    }
    fn session_claim(&self, headers: &HeaderMap) -> Option<SessionClaim> {
        self.decode(&cookie(headers, "artifact_session")?, "artifact-session")
    }
    pub fn principal(&self, headers: &HeaderMap) -> Option<Principal> {
        if headers.contains_key("authorization") {
            return crate::oauth::access_claim(self, headers).map(|c| c.principal);
        }
        self.session_claim(headers)
            .map(|s| s.principal)
            .filter(|p| {
                self.config.dev
                    || (self.config.configured()
                        && self
                            .config
                            .tenant
                            .as_ref()
                            .is_some_and(|tenant| p.id.starts_with(&format!("entra:{tenant}:"))))
            })
    }
    pub fn gated_principal(&self, headers: &HeaderMap) -> Option<Principal> {
        if headers.contains_key("authorization") {
            return self
                .principal(headers)
                .filter(|_| self.config.dev || self.config.gated_tenant == self.config.tenant);
        }
        self.session_claim(headers)
            .filter(|s| {
                s.gated
                    && (self.config.dev
                        || (self.config.configured()
                            && self.config.gated_tenant == self.config.tenant))
            })
            .map(|s| s.principal)
    }
    fn retry_identity_configured(&self) -> Result<(), AppError> {
        if !self.config.dev
            && !self
                .config
                .signing_key
                .as_ref()
                .is_some_and(|key| key.len() >= 64)
        {
            return Err(error(
                503,
                "retry_identity_unavailable",
                "A shared signing key is required for durable guest retry identity.",
            ));
        }
        Ok(())
    }
    /// Pseudonymous retry identity only: never grants viewer or management access.
    pub fn mutation_actor(
        &self,
        headers: &HeaderMap,
        p: Option<&Principal>,
    ) -> Result<String, AppError> {
        self.retry_identity_configured()?;
        if let Some(p) = p {
            return Ok(format!("auth:{}", p.id));
        }
        let claim: GuestClaim = cookie(headers, "artifact_guest")
            .and_then(|value| self.decode(&value, "artifact-guest-retry"))
            .ok_or_else(|| error(409, "guest_identity_required", "Initialize guest identity before creating a mutation request. Retain the original identity for retries."))?;
        if claim.id.len() != 64 || !claim.id.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(error(
                409,
                "guest_identity_required",
                "Invalid guest retry identity.",
            ));
        }
        Ok(format!("guest:{}", claim.id))
    }
    pub fn initialize_mutation_actor(
        &self,
        headers: &HeaderMap,
        p: Option<&Principal>,
    ) -> Result<(String, Option<String>), AppError> {
        self.retry_identity_configured()?;
        if let Ok(actor) = self.mutation_actor(headers, p) {
            return Ok((actor, None));
        }
        let id = token();
        let signed = self.sign(&GuestClaim {
            id: id.clone(),
            exp: now() + 30 * 86400,
            aud: "artifact-guest-retry".into(),
        });
        let cookie = format!(
            "artifact_guest={signed}; Path=/; HttpOnly; SameSite=Lax; Max-Age=2592000{}",
            if self.config.dev { "" } else { "; Secure" }
        );
        Ok((format!("guest:{id}"), Some(cookie)))
    }
    pub fn check_mutation_actor(
        &self,
        headers: &HeaderMap,
        p: Option<&Principal>,
        expected: &str,
    ) -> Result<String, AppError> {
        let actual = self.mutation_actor(headers, p)?;
        if actual != expected {
            return Err(error(
                409,
                "actor_changed",
                "The request belongs to a different reviewer identity. Restore its original session before retrying.",
            ));
        }
        Ok(actual)
    }
    pub fn frame_ticket(&self, cap: &str, revision: &str) -> String {
        self.sign(&FrameClaim {
            cap: cap.into(),
            revision: revision.into(),
            exp: now() + 300,
            aud: "artifact-frame".into(),
        })
    }
    pub fn check_frame(
        &self,
        ticket: &str,
        cap: &str,
        revision: &str,
    ) -> Result<Principal, AppError> {
        let c: FrameClaim = self.decode(ticket, "artifact-frame").ok_or_else(|| {
            error(
                403,
                "expired_frame",
                "Preview authorization expired. Refresh the review page.",
            )
        })?;
        if c.cap != cap || c.revision != revision {
            return Err(error(
                403,
                "invalid_frame",
                "Preview authorization is scoped to another revision.",
            ));
        }
        Ok(Principal {
            id: "admitted-content-frame".into(),
            display_name: "Content frame".into(),
            verified_email: None,
            authenticated: true,
        })
    }
    pub fn require(&self, headers: &HeaderMap) -> Result<Principal, AppError> {
        self.principal(headers).ok_or_else(|| {
            error(
                401,
                "unauthenticated",
                "Sign in through the configured Entra tenant to manage shares.",
            )
        })
    }
    pub fn mutation(&self, headers: &HeaderMap) -> Result<(), AppError> {
        if crate::oauth::access_claim(self, headers).is_none()
            && headers
                .get("x-artifact-request")
                .and_then(|v| v.to_str().ok())
                != Some("1")
        {
            return Err(error(403, "csrf", "Missing same-origin request marker."));
        }
        if let Some(origin) = headers.get("origin")
            && origin.to_str().ok() != Some(self.config.origin.as_str())
        {
            return Err(error(403, "csrf", "Cross-origin mutations are forbidden."));
        }
        if headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) == Some("cross-site") {
            return Err(error(403, "csrf", "Cross-site mutation rejected."));
        }
        Ok(())
    }
    fn session(&self, principal: Principal, gated: bool) -> String {
        self.cookie(&self.sign(&SessionClaim {
            principal,
            gated,
            exp: now() + 3600,
            aud: "artifact-session".into(),
        }))
    }
    fn cookie(&self, id: &str) -> String {
        format!(
            "artifact_session={id}; Path=/; HttpOnly; SameSite=Lax; Max-Age=3600{}",
            if self.config.dev { "" } else { "; Secure" }
        )
    }
    pub fn logout(&self, _headers: &HeaderMap) -> String {
        format!(
            "artifact_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}",
            if self.config.dev { "" } else { "; Secure" }
        )
    }
    pub fn dev_login(&self, identity: &str) -> Result<String, AppError> {
        if !self.config.dev {
            return Err(error(404, "not_found", "Development login is disabled."));
        }
        let (id, name) = match identity {
            "owner" => ("dev-owner", "Demo publisher"),
            "reviewer" => ("dev-reviewer", "Demo reviewer"),
            _ => ("", ""),
        };
        if id.is_empty() {
            return Err(error(
                400,
                "invalid_identity",
                "Choose owner or reviewer fixture.",
            ));
        }
        Ok(self.session(
            Principal {
                id: id.into(),
                display_name: name.into(),
                verified_email: Some(format!("{id}@example.invalid")),
                authenticated: true,
            },
            true,
        ))
    }
    pub fn begin(&self, next: &str) -> Result<(String, String), AppError> {
        if !self.config.configured() {
            return Err(error(
                503,
                "identity_not_configured",
                "Entra login is not configured. Production publishing remains locked.",
            ));
        }
        let state = token();
        let nonce = token();
        let verifier = token();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        self.pending
            .lock()
            .unwrap()
            .retain(|_, v| v.expires > now());
        self.pending.lock().unwrap().insert(
            state.clone(),
            Pending {
                nonce: nonce.clone(),
                verifier,
                expires: now() + 600,
                exp: now() + 600,
                state: state.clone(),
                next: safe_next(next).into(),
            },
        );
        let mut url = url::Url::parse(&format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/authorize",
            self.config.tenant.as_ref().unwrap()
        ))
        .unwrap();
        url.query_pairs_mut().extend_pairs([
            ("client_id", self.config.client.as_deref().unwrap()),
            ("response_type", "code"),
            (
                "redirect_uri",
                &format!("{}/auth/callback", self.config.origin),
            ),
            ("response_mode", "query"),
            ("scope", "openid profile"),
            ("state", &state),
            ("nonce", &nonce),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ]);
        let pending = self.pending.lock().unwrap().get(&state).cloned().unwrap();
        let signed = self.sign(&pending);
        Ok((
            url.to_string(),
            format!(
                "artifact_oidc_state={signed}; Path=/auth/callback; HttpOnly; SameSite=Lax; Max-Age=600; Secure"
            ),
        ))
    }
    pub async fn callback(
        &self,
        headers: &HeaderMap,
        state: &str,
        code: &str,
    ) -> Result<(String, String), AppError> {
        if !self.config.configured() {
            return Err(error(
                503,
                "identity_not_configured",
                "Entra login is not configured.",
            ));
        }
        let state_cookie = cookie(headers, "artifact_oidc_state").ok_or_else(|| {
            error(
                403,
                "invalid_state",
                "Login state does not belong to this browser.",
            )
        })?;
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_aud = false;
        validation.leeway = 0;
        let pending = decode::<Pending>(
            &state_cookie,
            &DecodingKey::from_secret(self.secret()),
            &validation,
        )
        .map_err(|_| error(403, "invalid_state", "Login state expired or is invalid."))?
        .claims;
        if pending.state != state {
            return Err(error(
                403,
                "invalid_state",
                "Login state does not belong to this browser.",
            ));
        }
        if pending.expires <= now() {
            return Err(error(403, "expired_state", "Restart login."));
        }
        let tenant = self.config.tenant.as_deref().unwrap();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| error(503, "identity_unavailable", "Identity client unavailable."))?;
        let response: Value = client
            .post(format!(
                "https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"
            ))
            .form(&[
                ("client_id", self.config.client.as_deref().unwrap()),
                ("client_secret", self.config.secret.as_deref().unwrap()),
                ("grant_type", "authorization_code"),
                ("code", code),
                (
                    "redirect_uri",
                    &format!("{}/auth/callback", self.config.origin),
                ),
                ("code_verifier", pending.verifier.as_str()),
            ])
            .send()
            .await
            .map_err(|_| {
                error(
                    503,
                    "identity_unavailable",
                    "Entra token exchange unavailable.",
                )
            })?
            .json()
            .await
            .map_err(|_| error(502, "identity_unavailable", "Invalid Entra response."))?;
        let jwt = response["id_token"].as_str().ok_or_else(|| {
            error(
                403,
                "invalid_login",
                "Entra did not return an identity token.",
            )
        })?;
        let jwks: JwkSet = client
            .get(format!(
                "https://login.microsoftonline.com/{tenant}/discovery/v2.0/keys"
            ))
            .send()
            .await
            .map_err(|_| error(503, "identity_unavailable", "Signing keys unavailable."))?
            .json()
            .await
            .map_err(|_| error(502, "identity_unavailable", "Invalid signing key response."))?;
        let claims = validate_identity(
            jwt,
            &jwks,
            tenant,
            self.config.client.as_deref().unwrap(),
            &pending.nonce,
        )?;
        let p = Principal {
            id: format!("entra:{}:{}", claims.tid, claims.oid),
            display_name: claims.name.unwrap_or_else(|| "Signed-in reviewer".into()),
            verified_email: None,
            authenticated: true,
        };
        Ok((
            self.session(p, self.config.gated_tenant.as_deref() == Some(tenant)),
            pending.next,
        ))
    }
    pub fn status(&self) -> Value {
        json!({"development_fixture":self.config.dev,"entra_configured":self.config.configured(),"gated_admission_configured":self.config.dev||self.config.gated_tenant.is_some()})
    }
}
pub fn safe_next(next: &str) -> &str {
    if let Some(request) = next.strip_prefix("/oauth/consent?request=")
        && !request.is_empty()
        && request.len() <= 16000
        && request
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return next;
    }
    if next.starts_with("/s/")
        && next[3..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'?' | b'='))
    {
        next
    } else {
        "/"
    }
}
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get("cookie")?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|p| {
            let (k, v) = p.trim().split_once('=')?;
            (k == name).then(|| v.to_owned())
        })
}
#[derive(Debug, Deserialize)]
pub struct Claims {
    pub tid: String,
    pub oid: String,
    pub nonce: String,
    pub name: Option<String>,
}
pub fn validate_identity(
    jwt: &str,
    jwks: &JwkSet,
    tenant: &str,
    audience: &str,
    nonce: &str,
) -> Result<Claims, AppError> {
    let hdr =
        decode_header(jwt).map_err(|_| error(403, "invalid_token", "Invalid identity token."))?;
    if hdr.alg != Algorithm::RS256 {
        return Err(error(403, "invalid_token", "Unapproved signing algorithm."));
    }
    let key = jwks
        .find(hdr.kid.as_deref().unwrap_or(""))
        .ok_or_else(|| error(403, "invalid_token", "Unknown signing key."))?;
    let key = DecodingKey::from_jwk(key)
        .map_err(|_| error(403, "invalid_token", "Invalid signing key."))?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&[audience]);
    validation.set_issuer(&[format!("https://login.microsoftonline.com/{tenant}/v2.0")]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "nbf"]);
    validation.validate_nbf = true;
    let claims = decode::<Claims>(jwt, &key, &validation)
        .map_err(|_| {
            error(
                403,
                "invalid_token",
                "Identity token signature, audience, issuer or expiry is invalid.",
            )
        })?
        .claims;
    if claims.tid != tenant || claims.nonce != nonce || uuid::Uuid::parse_str(&claims.oid).is_err()
    {
        return Err(error(
            403,
            "invalid_token",
            "Tenant, subject or login nonce is invalid.",
        ));
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Config {
        Config {
            dev: true,
            origin: "http://127.0.0.1:4317".into(),
            tenant: None,
            client: None,
            secret: None,
            gated_tenant: None,
            signing_key: Some(
                "synthetic-fixture-signing-material-not-a-production-secret-000000000000".into(),
            ),
        }
    }
    fn headers(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("cookie", value.split(';').next().unwrap().parse().unwrap());
        h
    }
    #[test]
    fn guest_retry_identity_initializes_before_mutation_and_survives_restart() {
        let a = Auth::new(config());
        let empty = HeaderMap::new();
        assert_eq!(
            a.mutation_actor(&empty, None).unwrap_err().code,
            "guest_identity_required"
        );
        let (actor, cookie) = a.initialize_mutation_actor(&empty, None).unwrap();
        let cookie = cookie.unwrap();
        assert!(cookie.contains("HttpOnly; SameSite=Lax"));
        let h = headers(&cookie);
        let b = Auth::new(config());
        assert_eq!(b.mutation_actor(&h, None).unwrap(), actor);
        assert!(b.initialize_mutation_actor(&h, None).unwrap().1.is_none());
        assert!(
            b.check_mutation_actor(&h, None, "guest:someone-else")
                .is_err()
        );
        let mut tampered = h.clone();
        tampered.insert(
            "cookie",
            "artifact_guest=invalid.signature".parse().unwrap(),
        );
        assert!(b.mutation_actor(&tampered, None).is_err());
        let expired = a.sign(&GuestClaim {
            id: token(),
            exp: now() - 1,
            aud: "artifact-guest-retry".into(),
        });
        assert!(
            b.mutation_actor(&headers(&format!("artifact_guest={expired}")), None)
                .is_err()
        );

        assert!(
            b.mutation_actor(&headers(&b.dev_login("owner").unwrap()), None)
                .is_err()
        );
        let (other, _) = b.initialize_mutation_actor(&empty, None).unwrap();
        assert_ne!(actor, other);
        let mut missing = config();
        missing.dev = false;
        missing.signing_key = None;
        let missing = Auth::new(missing);
        assert_eq!(
            missing
                .initialize_mutation_actor(&empty, None)
                .unwrap_err()
                .status,
            503
        );
        assert_eq!(
            missing.mutation_actor(&empty, None).unwrap_err().status,
            503
        );
    }
    #[test]
    fn signed_sessions_survive_instance_restart_without_sqlite() {
        let a = Auth::new(config());
        let cookie = a.dev_login("owner").unwrap();
        let b = Auth::new(config());
        let p = b.principal(&headers(&cookie)).unwrap();
        assert_eq!(p.id, "dev-owner");
        assert!(b.gated_principal(&headers(&cookie)).is_some());
        let mut c = config();
        c.signing_key =
            Some("different-fixture-signing-material-00000000000000000000000000000000".into());
        assert!(Auth::new(c).principal(&headers(&cookie)).is_none());
    }
    #[test]
    fn preview_ticket_scope_expiry_and_no_management_privilege() {
        let a = Auth::new(config());
        let t = a.frame_ticket("cap", "revision");
        assert!(a.check_frame(&t, "cap", "revision").is_ok());
        assert!(a.check_frame(&t, "another", "revision").is_err());
        assert!(a.check_frame(&t, "cap", "other").is_err());
        assert!(
            a.principal(&headers(&format!("artifact_session={t}")))
                .is_none()
        );
        let expired = a.sign(&FrameClaim {
            cap: "cap".into(),
            revision: "revision".into(),
            exp: now() - 1,
            aud: "artifact-frame".into(),
        });
        assert!(a.check_frame(&expired, "cap", "revision").is_err());
    }
    #[test]
    fn unsigned_or_wrong_algorithm_entra_identity_is_rejected() {
        let keys: JwkSet = serde_json::from_value(json!({"keys":[]})).unwrap();
        let wrong = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(Algorithm::HS256),
            &json!({"tid":"tenant","oid":"id","nonce":"nonce","exp":now()+500}),
            &jsonwebtoken::EncodingKey::from_secret(b"test-fixture"),
        )
        .unwrap();
        assert!(validate_identity(&wrong, &keys, "tenant", "aud", "nonce").is_err());
        assert!(validate_identity("unsigned.fake.token", &keys, "tenant", "aud", "nonce").is_err());
    }
    #[test]
    fn production_configuration_requires_all_boundaries() {
        let mut c = config();
        c.dev = false;
        assert!(!c.configured());
        c.tenant = Some(uuid::Uuid::new_v4().to_string());
        c.client = Some("client-fixture".into());
        c.secret = Some("fixture-only".into());
        assert!(c.configured());
        c.signing_key = Some("short".into());
        assert!(!c.configured());
    }
}

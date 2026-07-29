use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use auru_pm::oauth::{AuthorizationServerMetadata, discover_authorization_server};
use auru_pm_protocol::AuthenticatedIdentity;
use axum::Json;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use jsonwebtoken::jwk::{Jwk, JwkSet, KeyAlgorithm, KeyOperations, PublicKeyUse};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use reqwest::Client;
use serde_json::{Value, json};

use crate::config::{AuthenticationConfig, OAuthConfig, TokenValidationConfig};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenIdentity {
    pub issuer: String,
    pub subject: String,
    pub display_name: String,
    pub email: Option<String>,
    pub scopes: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthError {
    InvalidToken,
    ProviderUnavailable,
}

#[async_trait]
pub trait TokenVerifier: Send + Sync {
    async fn verify(&self, token: &str) -> Result<TokenIdentity, AuthError>;
}

#[derive(Clone)]
struct TokenValidationPolicy {
    issuer: String,
    audience: String,
    required_scope: String,
    display_name_claims: Vec<String>,
    email_claim: String,
}

impl TokenValidationPolicy {
    fn new(
        issuer: impl Into<String>,
        audience: impl Into<String>,
        required_scope: impl Into<String>,
        display_name_claims: &[String],
        email_claim: impl Into<String>,
    ) -> Self {
        Self {
            issuer: issuer.into(),
            audience: audience.into(),
            required_scope: required_scope.into(),
            display_name_claims: display_name_claims.to_vec(),
            email_claim: email_claim.into(),
        }
    }

    fn identity_from_claims(&self, claims: &Value) -> Result<TokenIdentity, AuthError> {
        let subject = claim_string(claims, "sub").ok_or(AuthError::InvalidToken)?;
        let scopes = claim_string(claims, "scope")
            .unwrap_or_default()
            .split_ascii_whitespace()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        if !scopes.contains(&self.required_scope) {
            return Err(AuthError::InvalidToken);
        }
        let display_name = self
            .display_name_claims
            .iter()
            .find_map(|claim| claim_string(claims, claim))
            .unwrap_or_else(|| subject.clone());
        Ok(TokenIdentity {
            issuer: self.issuer.clone(),
            subject,
            display_name,
            email: claim_string(claims, &self.email_claim),
            scopes,
        })
    }
}

pub struct JwtTokenVerifier {
    policy: TokenValidationPolicy,
    keys: JwtKeys,
}

enum JwtKeys {
    #[cfg(test)]
    Static(DecodingKey),
    Remote {
        client: Client,
        jwks_uri: String,
        cached: RwLock<Vec<JwkVerificationKey>>,
        refresh: tokio::sync::Mutex<Option<std::time::Instant>>,
    },
}

#[derive(Clone)]
struct JwkVerificationKey {
    id: Option<String>,
    algorithm: Option<Algorithm>,
    key: DecodingKey,
}

impl JwtTokenVerifier {
    #[cfg(test)]
    pub fn from_rsa_pem(
        issuer: impl Into<String>,
        audience: impl Into<String>,
        required_scope: impl Into<String>,
        display_name_claims: &[String],
        email_claim: impl Into<String>,
        public_key: &[u8],
    ) -> Result<Self, jsonwebtoken::errors::Error> {
        Ok(Self {
            policy: TokenValidationPolicy::new(
                issuer,
                audience,
                required_scope,
                display_name_claims,
                email_claim,
            ),
            keys: JwtKeys::Static(DecodingKey::from_rsa_pem(public_key)?),
        })
    }

    pub async fn from_jwks(
        issuer: impl Into<String>,
        audience: impl Into<String>,
        required_scope: impl Into<String>,
        display_name_claims: &[String],
        email_claim: impl Into<String>,
        client: Client,
        jwks_uri: impl Into<String>,
    ) -> Result<Self, String> {
        let jwks_uri = jwks_uri.into();
        let cached = fetch_jwks(&client, &jwks_uri).await?;
        Ok(Self {
            policy: TokenValidationPolicy::new(
                issuer,
                audience,
                required_scope,
                display_name_claims,
                email_claim,
            ),
            keys: JwtKeys::Remote {
                client,
                jwks_uri,
                cached: RwLock::new(cached),
                refresh: tokio::sync::Mutex::new(None),
            },
        })
    }

    async fn key_for(
        &self,
        key_id: Option<&str>,
        algorithm: Algorithm,
    ) -> Result<DecodingKey, AuthError> {
        match &self.keys {
            #[cfg(test)]
            JwtKeys::Static(key) => Ok(key.clone()),
            JwtKeys::Remote {
                client,
                jwks_uri,
                cached,
                refresh,
            } => {
                if let Some(key) = select_key(&cached.read().unwrap(), key_id, algorithm) {
                    return Ok(key);
                }
                let mut last_refresh = refresh.lock().await;
                if let Some(key) = select_key(&cached.read().unwrap(), key_id, algorithm) {
                    return Ok(key);
                }
                if last_refresh
                    .is_some_and(|last| last.elapsed() < std::time::Duration::from_secs(30))
                {
                    return Err(AuthError::InvalidToken);
                }
                let refreshed = fetch_jwks(client, jwks_uri).await;
                *last_refresh = Some(std::time::Instant::now());
                let refreshed = refreshed.map_err(|_| AuthError::ProviderUnavailable)?;
                let key = select_key(&refreshed, key_id, algorithm);
                *cached.write().unwrap() = refreshed;
                key.ok_or(AuthError::InvalidToken)
            }
        }
    }
}

#[async_trait]
impl TokenVerifier for JwtTokenVerifier {
    async fn verify(&self, token: &str) -> Result<TokenIdentity, AuthError> {
        let header = decode_header(token).map_err(|_| AuthError::InvalidToken)?;
        if !matches!(
            header.alg,
            Algorithm::RS256
                | Algorithm::RS384
                | Algorithm::RS512
                | Algorithm::PS256
                | Algorithm::PS384
                | Algorithm::PS512
                | Algorithm::ES256
                | Algorithm::ES384
                | Algorithm::EdDSA
        ) {
            return Err(AuthError::InvalidToken);
        }
        let key = self.key_for(header.kid.as_deref(), header.alg).await?;

        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[&self.policy.issuer]);
        validation.set_audience(&[&self.policy.audience]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.leeway = 30;

        let claims = decode::<serde_json::Value>(token, &key, &validation)
            .map_err(|_| AuthError::InvalidToken)?
            .claims;
        self.policy.identity_from_claims(&claims)
    }
}

fn claim_string(claims: &Value, name: &str) -> Option<String> {
    claims
        .get(name)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn select_key(
    keys: &[JwkVerificationKey],
    key_id: Option<&str>,
    algorithm: Algorithm,
) -> Option<DecodingKey> {
    let mut matching = keys.iter().filter(|candidate| {
        candidate
            .algorithm
            .is_none_or(|declared| declared == algorithm)
            && key_id.is_none_or(|key_id| candidate.id.as_deref() == Some(key_id))
    });
    let key = matching.next()?;
    if key_id.is_none() && matching.next().is_some() {
        return None;
    }
    Some(key.key.clone())
}

async fn fetch_jwks(client: &Client, jwks_uri: &str) -> Result<Vec<JwkVerificationKey>, String> {
    let response = client
        .get(jwks_uri)
        .send()
        .await
        .map_err(|error| format!("fetch JWKS: {error}"))?
        .error_for_status()
        .map_err(|error| format!("fetch JWKS: {error}"))?;
    let set: JwkSet = response
        .json()
        .await
        .map_err(|error| format!("decode JWKS: {error}"))?;
    let keys = set
        .keys
        .iter()
        .filter_map(jwk_verification_key)
        .collect::<Vec<_>>();
    if keys.is_empty() {
        return Err("the identity provider JWKS contains no usable signing keys".to_owned());
    }
    Ok(keys)
}

fn jwk_verification_key(jwk: &Jwk) -> Option<JwkVerificationKey> {
    if matches!(jwk.common.public_key_use, Some(PublicKeyUse::Encryption))
        || jwk
            .common
            .key_operations
            .as_ref()
            .is_some_and(|operations| !operations.contains(&KeyOperations::Verify))
    {
        return None;
    }
    let algorithm = match jwk.common.key_algorithm {
        Some(algorithm) => Some(jwk_algorithm(algorithm)?),
        None => None,
    };
    Some(JwkVerificationKey {
        id: jwk.common.key_id.clone(),
        algorithm,
        key: DecodingKey::from_jwk(jwk).ok()?,
    })
}

fn jwk_algorithm(algorithm: KeyAlgorithm) -> Option<Algorithm> {
    match algorithm {
        KeyAlgorithm::RS256 => Some(Algorithm::RS256),
        KeyAlgorithm::RS384 => Some(Algorithm::RS384),
        KeyAlgorithm::RS512 => Some(Algorithm::RS512),
        KeyAlgorithm::PS256 => Some(Algorithm::PS256),
        KeyAlgorithm::PS384 => Some(Algorithm::PS384),
        KeyAlgorithm::PS512 => Some(Algorithm::PS512),
        KeyAlgorithm::ES256 => Some(Algorithm::ES256),
        KeyAlgorithm::ES384 => Some(Algorithm::ES384),
        KeyAlgorithm::EdDSA => Some(Algorithm::EdDSA),
        _ => None,
    }
}

pub struct IntrospectionTokenVerifier {
    policy: TokenValidationPolicy,
    endpoint: String,
    client_id: String,
    client_secret: String,
    client: Client,
}

impl IntrospectionTokenVerifier {
    fn new(
        policy: TokenValidationPolicy,
        endpoint: impl Into<String>,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        client: Client,
    ) -> Self {
        Self {
            policy,
            endpoint: endpoint.into(),
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            client,
        }
    }
}

#[async_trait]
impl TokenVerifier for IntrospectionTokenVerifier {
    async fn verify(&self, token: &str) -> Result<TokenIdentity, AuthError> {
        let response = self
            .client
            .post(&self.endpoint)
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(&[("token", token), ("token_type_hint", "access_token")])
            .send()
            .await
            .map_err(|_| AuthError::ProviderUnavailable)?
            .error_for_status()
            .map_err(|_| AuthError::ProviderUnavailable)?;
        let claims: Value = response
            .json()
            .await
            .map_err(|_| AuthError::ProviderUnavailable)?;
        if claims.get("active").and_then(Value::as_bool) != Some(true) {
            return Err(AuthError::InvalidToken);
        }
        if claim_string(&claims, "iss").as_deref() != Some(self.policy.issuer.as_str()) {
            return Err(AuthError::InvalidToken);
        }
        if !audience_contains(&claims, &self.policy.audience) {
            return Err(AuthError::InvalidToken);
        }
        self.policy.identity_from_claims(&claims)
    }
}

fn audience_contains(claims: &Value, expected: &str) -> bool {
    match claims.get("aud") {
        Some(Value::String(audience)) => audience == expected,
        Some(Value::Array(audiences)) => audiences
            .iter()
            .any(|audience| audience.as_str() == Some(expected)),
        _ => false,
    }
}

#[derive(Clone)]
pub struct AuthState {
    provider_id: String,
    mode: AuthMode,
}

#[derive(Clone)]
enum AuthMode {
    None,
    OAuth(Arc<dyn TokenVerifier>),
}

impl AuthState {
    pub fn none(provider_id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            mode: AuthMode::None,
        }
    }

    pub fn oauth(provider_id: impl Into<String>, verifier: Arc<dyn TokenVerifier>) -> Self {
        Self {
            provider_id: provider_id.into(),
            mode: AuthMode::OAuth(verifier),
        }
    }
}

pub async fn build_auth_state(
    provider_id: &str,
    configuration: &AuthenticationConfig,
) -> Result<AuthState, String> {
    match configuration {
        AuthenticationConfig::None { .. } => Ok(AuthState::none(provider_id)),
        AuthenticationConfig::OAuth(oauth) => {
            let client = Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|error| format!("create identity provider client: {error}"))?;
            let metadata = discover_authorization_server(&client, &oauth.issuer).await?;
            validate_oauth_metadata(oauth, &metadata)?;
            let verifier: Arc<dyn TokenVerifier> = match &oauth.validation {
                TokenValidationConfig::Jwt => {
                    let jwks_uri = metadata.jwks_uri.as_deref().ok_or_else(|| {
                        "identity provider metadata does not publish jwks_uri".to_owned()
                    })?;
                    Arc::new(
                        JwtTokenVerifier::from_jwks(
                            &oauth.issuer,
                            &oauth.audience,
                            &oauth.required_scope,
                            &oauth.display_name_claims,
                            &oauth.email_claim,
                            client,
                            jwks_uri,
                        )
                        .await?,
                    )
                }
                TokenValidationConfig::Introspection {
                    endpoint,
                    client_id,
                    client_secret_env,
                } => {
                    let endpoint = endpoint
                        .as_deref()
                        .or(metadata.introspection_endpoint.as_deref())
                        .ok_or_else(|| {
                            "opaque-token validation requires an introspection endpoint".to_owned()
                        })?;
                    let secret = std::env::var(client_secret_env).map_err(|_| {
                        format!(
                            "environment variable `{client_secret_env}` must contain the introspection client secret"
                        )
                    })?;
                    if secret.is_empty() {
                        return Err(format!(
                            "environment variable `{client_secret_env}` must not be empty"
                        ));
                    }
                    Arc::new(IntrospectionTokenVerifier::new(
                        TokenValidationPolicy::new(
                            &oauth.issuer,
                            &oauth.audience,
                            &oauth.required_scope,
                            &oauth.display_name_claims,
                            &oauth.email_claim,
                        ),
                        endpoint,
                        client_id,
                        secret,
                        client,
                    ))
                }
            };
            Ok(AuthState::oauth(provider_id, verifier))
        }
    }
}

fn validate_oauth_metadata(
    oauth: &OAuthConfig,
    metadata: &AuthorizationServerMetadata,
) -> Result<(), String> {
    if oauth
        .flows
        .contains(&auru_pm_protocol::OAuthFlow::DeviceAuthorization)
        && metadata.device_authorization_endpoint.is_none()
    {
        return Err(
            "device_authorization is configured but the identity provider metadata does not publish device_authorization_endpoint"
                .to_owned(),
        );
    }
    Ok(())
}

pub async fn require_auth(
    State(auth): State<AuthState>,
    mut request: Request,
    next: Next,
) -> Response {
    let token_identity = match &auth.mode {
        AuthMode::None => TokenIdentity {
            issuer: "local".to_owned(),
            subject: "local-user".to_owned(),
            display_name: "Local user".to_owned(),
            email: None,
            scopes: BTreeSet::new(),
        },
        AuthMode::OAuth(verifier) => {
            let Some(token) = bearer_token(request.headers()) else {
                return unauthorized("a bearer token is required");
            };
            match verifier.verify(token).await {
                Ok(identity) => identity,
                Err(AuthError::InvalidToken) => {
                    return unauthorized("the bearer token is invalid");
                }
                Err(AuthError::ProviderUnavailable) => {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({
                            "code": "authentication_unavailable",
                            "message": "the identity provider is temporarily unavailable"
                        })),
                    )
                        .into_response();
                }
            }
        }
    };
    request.extensions_mut().insert(TokenIdentity {
        issuer: token_identity.issuer,
        subject: token_identity.subject.clone(),
        display_name: token_identity.display_name.clone(),
        email: token_identity.email.clone(),
        scopes: token_identity.scopes,
    });
    request.extensions_mut().insert(AuthenticatedIdentity {
        provider_id: auth.provider_id,
        user_id: token_identity.subject,
        display_name: token_identity.display_name,
        email: token_identity.email,
    });
    next.run(request).await
}

fn bearer_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer")
        && !token.is_empty()
        && !token.chars().any(char::is_whitespace))
    .then_some(token)
}

fn unauthorized(message: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, r#"Bearer realm="auru-pm""#)],
        Json(json!({
            "code": "unauthorized",
            "message": message
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use axum::Router;
    use axum::extract::{Form, State};
    use axum::http::HeaderMap;
    use axum::routing::post;
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};

    use super::*;

    const PRIVATE_KEY: &[u8] = br#"-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCuasFNzUMpc2KS
cufzgaZBTNjKIlLSqS+81eFJuzHONkKWYUDVXs0xIUNRlUDkL46wsTh4eGkI3s27
9dLGdbadd3wluE6EuVjPhpJg691uOE/RzGCDkcaNjG1Hjbti3mHzSzs7vmZSXgHz
lfCjH7k5LkHlItlQCtabnQ+nSrJDY2xLe9klaXlGqW2CQVXPURrtUfF9pGM51yuw
E0OxuYj3DbIm/rZTJpRX8JccJAEfqW561nOsPFIuIwwb70bwIsD2+79RKDoV/qWN
pchmHqAw9Chs80yy8RPJn0W7CqLVpI3Ovg7+fPT0LWZUZAfSrGczxIeuI3nG0hpO
MlDJZfW9AgMBAAECggEABRdIHlDTY50YKjf7kGZpzND6U0drsqBDbLKzJqS1uDQU
vvtxOe57Ckf92p6C5vYtZyJmcyZpQxvvwOCqFDwLhm6eUM00+4JGdu48YmMCvazG
mv7TwrC0AoiFXbfjxD0YpPzpP8iXsifyIiLvrm88B+YoqxBt6+cZFbCLVwHNkL0Z
0iwq04ZreowR3TFHxWEios5yFJIQXGaAU2B3Ri69CvjlHeJwZLe9fuCOM3lBx+Xt
Shqj2T4JP78f0gu9XmA99meObJ7n0p9J4SJkBzLFmsexWpDIu4y36/6DfALJEZEU
A5Hw526MMfgAyXL1aAaBYbcAFaTpQzKutX45BlrK+QKBgQDxcd9UoPgFMMtnfJjC
WvOiNx3kvpXZGYzq2ZfsDtljjpWRcH9Aex+VKXwuW+P8K+xJhq/RUFM8CtIxDiV9
90CRxdEr+0Z+yj6QhonuuwPaDqNSJfO3WQnGVLHw1yKIT9BBmCReURUqy/Ff1uMn
brX31LFWkYmPp2R0Up+Ogc/BZQKBgQC47ne5/S2RtdFMrl9u6Nc1HVQVQ7RnsVfz
kaq1QwgKgMGK1XL3T8u6O9HGy0YsvrF3EyQtB98SqkwhGcetdIwWl8aS6QlGIGiu
g/HF+9+o8Q+iWfBSF4VM5JptuRoFiB4/qIbS8WVi1yodM+B4f4r2SMC23RnSMp4Y
VjSkg9sJeQKBgQCbjM9jCGmBfpQs0drgrBP2WCgMLLUBrzJYQ2NbE53+Q+gcUSvK
cQhB4v48J7tTxUBvhjRTV7qoHhiYvhJtexPAVn+SJEqgeM+h8OuAQEAVBgU2cXj4
kIZ5nisdjJyU0UbMW6ZilT5b2hRhuGGUEAFv7zlpGk5TnHZdcrWU7BDa+QKBgE1r
J5wpLWaOoyxi43je6RlHSegNC/1M9PD2zmxLv5YGCQBCE3sNYNB6MnvypVIeEtUy
ojZn0S9TM8O3syweWncq2uqtvEArWSeV/SVRKHTlVhI1bLIxPpDOMwg0MXyXW3Jy
7t5oSHV0diD7ksFfQ6GPG35yWVjx79VoYWlt+cihAoGALye/6H2ZzXLCzFn8mB8a
Rj2wcpw9et+Djy53eRZBkwhuSMJvSc8MbOYfDdlMBb3t5fb/0AJI3uBb0uWk0rIv
5CBpIixovq4SXY9uMbrE9UAo9G+66axYO1hrkK7+UCSsCW1d/OKSmeZP+hbChGeb
jHLwko9orZhhKAGq4aOqqHo=
-----END PRIVATE KEY-----"#;

    const PUBLIC_KEY: &[u8] = br#"-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEArmrBTc1DKXNiknLn84Gm
QUzYyiJS0qkvvNXhSbsxzjZClmFA1V7NMSFDUZVA5C+OsLE4eHhpCN7Nu/XSxnW2
nXd8JbhOhLlYz4aSYOvdbjhP0cxgg5HGjYxtR427Yt5h80s7O75mUl4B85Xwox+5
OS5B5SLZUArWm50Pp0qyQ2NsS3vZJWl5RqltgkFVz1Ea7VHxfaRjOdcrsBNDsbmI
9w2yJv62UyaUV/CXHCQBH6luetZzrDxSLiMMG+9G8CLA9vu/USg6Ff6ljaXIZh6g
MPQobPNMsvETyZ9Fuwqi1aSNzr4O/nz09C1mVGQH0qxnM8SHriN5xtIaTjJQyWX1
vQIDAQAB
-----END PUBLIC KEY-----"#;

    fn token_with_key_id(audience: &str, key_id: Option<&str>) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut header = Header::new(Algorithm::RS256);
        header.kid = key_id.map(str::to_owned);
        encode(
            &header,
            &json!({
                "iss": "https://identity.example.com",
                "aud": audience,
                "sub": "user_123",
                "exp": now + 300,
                "iat": now,
                "scope": "openid profile",
                "name": "Alice Example",
                "email": "alice@example.com"
            }),
            &EncodingKey::from_rsa_pem(PRIVATE_KEY).unwrap(),
        )
        .unwrap()
    }

    fn token(audience: &str) -> String {
        token_with_key_id(audience, None)
    }

    #[tokio::test]
    async fn jwt_verifier_should_accept_only_the_configured_resource_identity() {
        let verifier = JwtTokenVerifier::from_rsa_pem(
            "https://identity.example.com",
            "auru-pm",
            "openid",
            &["name".to_owned(), "preferred_username".to_owned()],
            "email",
            PUBLIC_KEY,
        )
        .expect("JWT verifier");

        let identity = verifier.verify(&token("auru-pm")).await.unwrap();
        assert_eq!(identity.subject, "user_123");
        assert_eq!(identity.display_name, "Alice Example");
        assert_eq!(identity.email.as_deref(), Some("alice@example.com"));
        assert_eq!(
            identity.scopes,
            BTreeSet::from(["openid".to_owned(), "profile".to_owned()])
        );

        assert_eq!(
            verifier.verify(&token("some-other-api")).await,
            Err(AuthError::InvalidToken)
        );
    }

    const RSA_MODULUS: &str = "rmrBTc1DKXNiknLn84GmQUzYyiJS0qkvvNXhSbsxzjZClmFA1V7NMSFDUZVA5C-OsLE4eHhpCN7Nu_XSxnW2nXd8JbhOhLlYz4aSYOvdbjhP0cxgg5HGjYxtR427Yt5h80s7O75mUl4B85Xwox-5OS5B5SLZUArWm50Pp0qyQ2NsS3vZJWl5RqltgkFVz1Ea7VHxfaRjOdcrsBNDsbmI9w2yJv62UyaUV_CXHCQBH6luetZzrDxSLiMMG-9G8CLA9vu_USg6Ff6ljaXIZh6gMPQobPNMsvETyZ9Fuwqi1aSNzr4O_nz09C1mVGQH0qxnM8SHriN5xtIaTjJQyWX1vQ";

    async fn jwks(State(requests): State<Arc<AtomicUsize>>) -> Json<Value> {
        let key_id = if requests.fetch_add(1, Ordering::SeqCst) == 0 {
            "old"
        } else {
            "new"
        };
        Json(json!({
            "keys": [{
                "kty": "RSA",
                "use": "sig",
                "alg": "RS256",
                "kid": key_id,
                "n": RSA_MODULUS,
                "e": "AQAB"
            }]
        }))
    }

    async fn spawn_test_server(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn jwt_verifier_should_refresh_jwks_once_for_an_unknown_key_id() {
        let requests = Arc::new(AtomicUsize::new(0));
        let base = spawn_test_server(
            Router::new()
                .route("/jwks", axum::routing::get(jwks))
                .with_state(requests.clone()),
        )
        .await;
        let verifier = JwtTokenVerifier::from_jwks(
            "https://identity.example.com",
            "auru-pm",
            "openid",
            &["name".to_owned()],
            "email",
            Client::new(),
            format!("{base}/jwks"),
        )
        .await
        .unwrap();

        let identity = verifier
            .verify(&token_with_key_id("auru-pm", Some("new")))
            .await
            .unwrap();
        assert_eq!(identity.subject, "user_123");
        assert_eq!(requests.load(Ordering::SeqCst), 2);
    }

    async fn introspect(
        headers: HeaderMap,
        Form(body): Form<HashMap<String, String>>,
    ) -> Json<Value> {
        assert!(
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("Basic "))
        );
        if body.get("token").map(String::as_str) == Some("active-token") {
            Json(json!({
                "active": true,
                "iss": "https://identity.example.com",
                "sub": "opaque_user",
                "aud": ["auru-pm"],
                "scope": "openid profile",
                "name": "Opaque User"
            }))
        } else if body.get("token").map(String::as_str) == Some("missing-issuer") {
            Json(json!({
                "active": true,
                "sub": "opaque_user",
                "aud": ["auru-pm"],
                "scope": "openid profile"
            }))
        } else {
            Json(json!({"active": false}))
        }
    }

    #[tokio::test]
    async fn introspection_verifier_should_require_active_audience_and_scope() {
        let base = spawn_test_server(Router::new().route("/introspect", post(introspect))).await;
        let verifier = IntrospectionTokenVerifier::new(
            TokenValidationPolicy::new(
                "https://identity.example.com",
                "auru-pm",
                "openid",
                &["name".to_owned()],
                "email",
            ),
            format!("{base}/introspect"),
            "pm-server",
            "secret",
            Client::new(),
        );

        let identity = verifier.verify("active-token").await.unwrap();
        assert_eq!(identity.subject, "opaque_user");
        assert_eq!(identity.display_name, "Opaque User");
        assert_eq!(
            verifier.verify("inactive-token").await,
            Err(AuthError::InvalidToken)
        );
        assert_eq!(
            verifier.verify("missing-issuer").await,
            Err(AuthError::InvalidToken)
        );
    }
}

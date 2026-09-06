//! Shared JWT authentication for every Coordin8 gRPC service.
//!
//! See `.claude/plans/grpc-security/PRD.md` for the full design and the
//! reasoning behind each decision referenced in these doc comments.
//!
//! There is no gateway in Coordin8's architecture (Registry/Space/EventMgr/
//! TransactionMgr/Proxy are independent peer processes), so every service
//! mounts the same [`AuthConfig`] as a `tonic` interceptor on its own server
//! rather than authenticating at a single choke point (Decision 3).
//!
//! Authentication is opt-in, off unless configured (Decision 6): with no
//! secret configured, [`AuthConfig::call`] is a no-op and a service behaves
//! exactly as it does without this crate at all.

// This whole crate's job is producing `tonic::Status` — that's the interop
// point with `tonic::service::Interceptor`, not something worth boxing away
// internally for a lint that already exempts the trait-impl boundary itself.
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use tonic::{Request, Status};

/// Env var carrying the shared HS256 signing secret. Unset (or empty) means
/// authentication is disabled — see [`AuthConfig::from_env`].
pub const SECRET_ENV_VAR: &str = "COORDIN8_JWT_SECRET";

/// Env var toggling [`AuthConfig::verify_signature`]. Any value other than
/// `"false"`/`"0"` is treated as enabled (the default when unset).
pub const VERIFY_SIGNATURE_ENV_VAR: &str = "COORDIN8_AUTH_VERIFY_SIGNATURE";

/// The claims Coordin8 tokens carry. `sub` is the caller's identity (a
/// service name or operator/CLI identity, not an end-user); `scope` and
/// `iss` are optional and only checked if a service configures them to be.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: i64,
    pub iat: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iss: Option<String>,
}

/// A hook a service supplies to layer its own checks on top of the baseline
/// `exp`/`iss` validation (Decision 7) — e.g. requiring a specific `scope`.
/// `Arc` (not `Box`) because [`AuthConfig`] must be `Clone` for `tonic`'s
/// [`tonic::service::interceptor`] layer to clone it per mounted service.
pub type ClaimsCheck = Arc<dyn Fn(&Claims) -> Result<(), Status> + Send + Sync>;

/// Configuration for one service's JWT validation, and the `tonic`
/// interceptor itself (see the `Interceptor` impl below).
#[derive(Clone)]
pub struct AuthConfig {
    /// `None` disables authentication entirely (Decision 6) — the
    /// interceptor becomes a no-op.
    secret: Option<String>,
    /// When `false`, skip cryptographic signature verification and only
    /// decode+check claims — for deployments where an upstream component
    /// (a gateway/mesh sidecar, or another Coordin8 service forwarding an
    /// already-checked token) already verified the signature (Decision 7).
    /// Defaults to `true`. Setting this to `false` means this service's
    /// security now depends on the network path actually guaranteeing that
    /// only already-validated requests can reach it — verify that's true
    /// for your topology before disabling this.
    verify_signature: bool,
    /// If set, tokens must carry a matching `iss` claim.
    issuer: Option<String>,
    /// Additional per-service claim checks, run after the baseline checks.
    claims_check: Option<ClaimsCheck>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            secret: None,
            verify_signature: true,
            issuer: None,
            claims_check: None,
        }
    }
}

impl AuthConfig {
    /// Build from environment variables — the only way services configure
    /// this in v1 (Decision 2: no runtime issuance/config service).
    /// `SECRET_ENV_VAR` unset or empty disables authentication (Decision 6).
    pub fn from_env() -> Self {
        let secret = std::env::var(SECRET_ENV_VAR).ok().filter(|s| !s.is_empty());
        let verify_signature = std::env::var(VERIFY_SIGNATURE_ENV_VAR)
            .map(|v| v != "false" && v != "0")
            .unwrap_or(true);
        Self {
            secret,
            verify_signature,
            issuer: None,
            claims_check: None,
        }
    }

    /// Require a specific `iss` claim on every token.
    pub fn with_issuer(mut self, issuer: impl Into<String>) -> Self {
        self.issuer = Some(issuer.into());
        self
    }

    /// Layer an additional claims check on top of the baseline validation.
    pub fn with_claims_check<F>(mut self, check: F) -> Self
    where
        F: Fn(&Claims) -> Result<(), Status> + Send + Sync + 'static,
    {
        self.claims_check = Some(Arc::new(check));
        self
    }

    /// Whether this config actually enforces anything (Decision 6).
    pub fn enabled(&self) -> bool {
        self.secret.is_some()
    }

    /// The default [`ClientAuthConfig`] for this service's own outbound,
    /// internal Djinn-to-Djinn calls (Decision 8) — self-mint using the same
    /// secret this config validates incoming tokens against when enabled, or
    /// attach nothing when it isn't (so turning auth off is still a true
    /// no-op end to end). This is a sensible default, not the only option —
    /// a deployment that wants a different internal strategy (e.g. an
    /// mTLS-CN-derived provider, or always-trust even with auth enabled)
    /// builds its own `ClientAuthConfig` directly instead of calling this.
    pub fn client_config(&self, subject: &str) -> ClientAuthConfig {
        match &self.secret {
            Some(secret) => ClientAuthConfig::self_minted(secret.clone(), subject),
            None => ClientAuthConfig::trust(),
        }
    }

    fn validate(&self, req: &Request<()>) -> Result<(), Status> {
        let Some(secret) = self.secret.as_deref() else {
            // Decision 6: no secret configured, auth is off.
            return Ok(());
        };

        let token = extract_bearer_token(req)?;
        let claims = if self.verify_signature {
            decode_and_verify(&token, secret)?
        } else {
            decode_unverified(&token)?
        };

        validate_baseline(&claims, self.issuer.as_deref())?;

        if let Some(check) = &self.claims_check {
            check(&claims)?;
        }

        Ok(())
    }
}

impl tonic::service::Interceptor for AuthConfig {
    fn call(&mut self, req: Request<()>) -> Result<Request<()>, Status> {
        self.validate(&req)?;
        Ok(req)
    }
}

fn extract_bearer_token(req: &Request<()>) -> Result<String, Status> {
    let raw = req
        .metadata()
        .get("authorization")
        .ok_or_else(|| Status::unauthenticated("missing authorization metadata"))?
        .to_str()
        .map_err(|_| Status::unauthenticated("authorization metadata is not valid UTF-8"))?;
    raw.strip_prefix("Bearer ")
        .map(str::to_string)
        .ok_or_else(|| Status::unauthenticated("authorization metadata must be a Bearer token"))
}

fn decode_and_verify(token: &str, secret: &str) -> Result<Claims, Status> {
    // exp is checked ourselves in validate_baseline, uniformly for both the
    // verified and unverified paths, rather than relying on jsonwebtoken's
    // own exp check here and a separate one there.
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = false;
    validation.required_spec_claims.clear();
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map(|data| data.claims)
    .map_err(|e| Status::unauthenticated(format!("invalid token: {e}")))
}

fn decode_unverified(token: &str) -> Result<Claims, Status> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.insecure_disable_signature_validation();
    validation.validate_exp = false;
    validation.required_spec_claims.clear();
    // The secret is never checked with signature validation disabled, but
    // the API still requires a `DecodingKey` — content is irrelevant here.
    decode::<Claims>(token, &DecodingKey::from_secret(&[]), &validation)
        .map(|data| data.claims)
        .map_err(|e| Status::unauthenticated(format!("malformed token: {e}")))
}

fn validate_baseline(claims: &Claims, required_issuer: Option<&str>) -> Result<(), Status> {
    let now = chrono::Utc::now().timestamp();
    if claims.exp <= now {
        return Err(Status::unauthenticated("token expired"));
    }
    if let Some(required) = required_issuer {
        if claims.iss.as_deref() != Some(required) {
            return Err(Status::unauthenticated("token issuer not trusted"));
        }
    }
    Ok(())
}

/// Mint a signed token. Used by the `coordin8 auth mint-token` CLI
/// subcommand (Decision 2: static, offline-minted tokens — there is no
/// runtime issuance endpoint in v1).
pub fn mint_token(
    secret: &str,
    subject: &str,
    ttl: chrono::Duration,
    scope: Option<String>,
    issuer: Option<String>,
) -> Result<String, jsonwebtoken::errors::Error> {
    let now = chrono::Utc::now();
    let claims = Claims {
        sub: subject.to_string(),
        iat: now.timestamp(),
        exp: (now + ttl).timestamp(),
        scope,
        iss: issuer,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
}

/// How an outbound (Djinn-to-Djinn) call obtains a token to attach, if any.
/// Returning `None` attaches nothing for that call.
pub type TokenProvider = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// Configuration for the *client* side of internal Coordin8-to-Coordin8
/// calls — self-registration, TransactionMgr calling into a participant,
/// Proxy's remote capability lookups, and similar (Decision 8). Deliberately
/// separate from [`AuthConfig`], which is the *server* side: a service both
/// validates incoming calls (`AuthConfig`) and, independently, decides what
/// (if anything) to attach to its own outgoing calls (`ClientAuthConfig`).
///
/// There is no single right strategy across every deployment, so this is a
/// provider closure rather than a fixed mechanism — see the three
/// constructors below.
#[derive(Clone, Default)]
pub struct ClientAuthConfig {
    provider: Option<TokenProvider>,
}

impl ClientAuthConfig {
    /// Attach no token to outbound calls. The default, and a legitimate
    /// deployment choice in its own right (internal traffic trusted by some
    /// other means — network segmentation, a mesh, etc.) rather than just
    /// "not implemented yet."
    pub fn trust() -> Self {
        Self { provider: None }
    }

    /// Mint a fresh, short-lived token per outbound call using a secret this
    /// service already holds — typically the same secret its own
    /// [`AuthConfig`] validates incoming tokens against. The simplest way to
    /// make turning auth on actually work end-to-end under v1's
    /// HS256-shared-secret model.
    pub fn self_minted(secret: impl Into<String>, subject: impl Into<String>) -> Self {
        let secret = secret.into();
        let subject = subject.into();
        Self {
            provider: Some(Arc::new(move || {
                mint_token(&secret, &subject, chrono::Duration::minutes(5), None, None).ok()
            })),
        }
    }

    /// Supply a token from anywhere else — e.g. one synthesized by
    /// infrastructure from an mTLS certificate's CN, or any other mechanism
    /// this crate has no business knowing about. This is the actual
    /// extension point Decision 8 exists for.
    pub fn from_provider<F>(provider: F) -> Self
    where
        F: Fn() -> Option<String> + Send + Sync + 'static,
    {
        Self {
            provider: Some(Arc::new(provider)),
        }
    }

    /// Build the `tonic` client-side interceptor. Pass to a generated
    /// client's `with_interceptor` constructor.
    pub fn interceptor(&self) -> ClientAuthInterceptor {
        ClientAuthInterceptor {
            provider: self.provider.clone(),
        }
    }
}

/// Attaches whatever [`ClientAuthConfig`] provides (if anything) as
/// `authorization: Bearer <token>` on every outgoing call. A no-op when built
/// from [`ClientAuthConfig::trust`].
#[derive(Clone)]
pub struct ClientAuthInterceptor {
    provider: Option<TokenProvider>,
}

impl tonic::service::Interceptor for ClientAuthInterceptor {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        let Some(provider) = &self.provider else {
            return Ok(req);
        };
        if let Some(token) = provider() {
            let value = format!("Bearer {token}")
                .parse()
                .map_err(|_| Status::internal("token is not valid metadata"))?;
            req.metadata_mut().insert("authorization", value);
        }
        Ok(req)
    }
}

/// The concrete client transport every internal Djinn-to-Djinn caller uses —
/// whether or not it actually attaches a token. `ClientAuthConfig::trust()`
/// produces this exact same type with a no-op interceptor, not a different
/// type, so callers never need to branch on which strategy is in play.
pub type AuthedChannel = tonic::service::interceptor::InterceptedService<
    tonic::transport::Channel,
    ClientAuthInterceptor,
>;

/// Wrap a plain `Channel` with a service's outbound auth strategy (Decision
/// 8). Shared by every internal Djinn-to-Djinn caller so they all produce the
/// same [`AuthedChannel`] type regardless of which strategy is configured.
pub fn wrap_channel(
    channel: tonic::transport::Channel,
    client_auth: &ClientAuthConfig,
) -> AuthedChannel {
    tonic::service::interceptor::InterceptedService::new(channel, client_auth.interceptor())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::service::Interceptor as _;

    fn config(secret: &str) -> AuthConfig {
        AuthConfig {
            secret: Some(secret.to_string()),
            verify_signature: true,
            issuer: None,
            claims_check: None,
        }
    }

    fn bearer_request(token: &str) -> Request<()> {
        let mut req = Request::new(());
        req.metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        req
    }

    #[test]
    fn disabled_config_is_a_no_op() {
        let cfg = AuthConfig::default();
        assert!(!cfg.enabled());
        assert!(cfg.validate(&Request::new(())).is_ok());
    }

    #[test]
    fn missing_token_is_rejected_when_enabled() {
        let cfg = config("shh");
        let err = cfg.validate(&Request::new(())).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn valid_token_is_accepted() {
        let secret = "shh";
        let token =
            mint_token(secret, "greeter", chrono::Duration::minutes(5), None, None).unwrap();
        let cfg = config(secret);
        assert!(cfg.validate(&bearer_request(&token)).is_ok());
    }

    #[test]
    fn expired_token_is_rejected() {
        let secret = "shh";
        let token =
            mint_token(secret, "greeter", chrono::Duration::seconds(-5), None, None).unwrap();
        let cfg = config(secret);
        let err = cfg.validate(&bearer_request(&token)).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn wrong_secret_is_rejected() {
        let token = mint_token(
            "secret-a",
            "greeter",
            chrono::Duration::minutes(5),
            None,
            None,
        )
        .unwrap();
        let cfg = config("secret-b");
        let err = cfg.validate(&bearer_request(&token)).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn verify_signature_false_trusts_an_unsigned_or_wrongly_signed_token() {
        let token = mint_token(
            "secret-a",
            "greeter",
            chrono::Duration::minutes(5),
            None,
            None,
        )
        .unwrap();
        let mut cfg = config("secret-b");
        cfg.verify_signature = false;
        assert!(cfg.validate(&bearer_request(&token)).is_ok());
    }

    #[test]
    fn claims_check_hook_runs_after_baseline_checks() {
        let secret = "shh";
        let token = mint_token(
            secret,
            "greeter",
            chrono::Duration::minutes(5),
            Some("readonly".to_string()),
            None,
        )
        .unwrap();
        let mut cfg = config(secret);
        cfg.claims_check = Some(Arc::new(|claims: &Claims| {
            if claims.scope.as_deref() == Some("admin") {
                Ok(())
            } else {
                Err(Status::permission_denied("requires admin scope"))
            }
        }));
        let err = cfg.validate(&bearer_request(&token)).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn issuer_mismatch_is_rejected() {
        let secret = "shh";
        let token = mint_token(
            secret,
            "greeter",
            chrono::Duration::minutes(5),
            None,
            Some("someone-else".to_string()),
        )
        .unwrap();
        let mut cfg = config(secret);
        cfg.issuer = Some("coordin8".to_string());
        let err = cfg.validate(&bearer_request(&token)).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    fn extract_bearer(req: &Request<()>) -> Option<String> {
        req.metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::to_string)
    }

    #[test]
    fn trust_attaches_nothing() {
        let mut interceptor = ClientAuthConfig::trust().interceptor();
        let req = interceptor.call(Request::new(())).unwrap();
        assert!(extract_bearer(&req).is_none());
    }

    #[test]
    fn self_minted_attaches_a_verifiable_token() {
        let secret = "shh";
        let mut interceptor = ClientAuthConfig::self_minted(secret, "space").interceptor();
        let req = interceptor.call(Request::new(())).unwrap();
        let token = extract_bearer(&req).expect("token attached");

        // Round-trip through the server-side validator to prove it's a real,
        // acceptable token, not just a nonempty string.
        let server_cfg = config(secret);
        assert!(server_cfg.validate(&bearer_request(&token)).is_ok());
    }

    #[test]
    fn custom_provider_attaches_whatever_it_returns() {
        let mut interceptor =
            ClientAuthConfig::from_provider(|| Some("from-mtls-cn".to_string())).interceptor();
        let req = interceptor.call(Request::new(())).unwrap();
        assert_eq!(extract_bearer(&req).as_deref(), Some("from-mtls-cn"));
    }

    #[test]
    fn custom_provider_returning_none_attaches_nothing() {
        let mut interceptor = ClientAuthConfig::from_provider(|| None).interceptor();
        let req = interceptor.call(Request::new(())).unwrap();
        assert!(extract_bearer(&req).is_none());
    }
}

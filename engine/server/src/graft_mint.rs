//! Per-session graft tokens (WI-3229 item 1 / WI-3231 part 2).
//!
//! A pod used to clone with one engine-wide graft token. Now the engine holds
//! a *mint* token (`ENGINE_GRAFT_MINT_TOKEN_FILE`, a service-kind graft token
//! the engine never hands to a pod) and, when it creates a session's pod,
//! calls graft's `AdminService.MintToken` for a token of its own for that
//! session: subject `vogt-session-<id>`, the session id and work item stamped
//! on it, and scopes for the session's repo and line only. Graft enforces that
//! the minted token is a subset of the mint token on every axis, so the engine
//! cannot hand out more than it holds.
//!
//! The token reaches the pod in the bootstrap response, like the session
//! token: it is held in memory ([`crate::agent_stream::AgentAuth`]) from mint
//! until the pod's single bootstrap request, then written by the entrypoint to
//! a 0600 file. It is never an env value, an argument or a log field, and its
//! `Debug` is redacted.
//!
//! Lifetime: graft has no revoke RPC yet (WI-3235), so the TTL is the only
//! expiry: the session's whole-session clock budget plus [`TTL_SLACK_S`],
//! capped at graft's 12 h `max_token_ttl_s`. A woken session gets a new pod and
//! so a new token. [`GraftMinter::revoke`] is the hook for revoke-on-stop once
//! graft grows the RPC; today it does nothing.

use std::{future::Future, path::PathBuf, pin::Pin, time::Duration};

use tonic::{
    codegen::http::uri::PathAndQuery,
    metadata::{Ascii, MetadataValue},
    transport::{Channel, ClientTlsConfig, Endpoint},
    Code,
};
use uuid::Uuid;
use vogt_engine_contract::SessionBudget;

use crate::error::{ApiError, Result};

/// Graft's `max_token_ttl_s` default (12 h): the longest a minted token lives.
pub const MAX_TTL_S: u64 = 12 * 3600;
/// Added to the session's whole-session clock budget, so a token outlives the
/// budget watcher's stop by long enough for the session to land its work.
pub const TTL_SLACK_S: u64 = 30 * 60;

const MINT_PATH: &str = "/graft.v1.AdminService/MintToken";
/// The graft protocol version this client speaks (`graft-protocol` header).
const PROTOCOL_VERSION: &str = "0.1";
const MINT_TIMEOUT: Duration = Duration::from_secs(15);

/// `graft.v1.MintTokenRequest`, hand-written from `graft.proto` so the engine
/// needs neither protoc nor an unpublished crate.
#[derive(Clone, PartialEq, prost::Message)]
pub struct MintTokenRequest {
    #[prost(string, tag = "1")]
    pub subject: String,
    #[prost(string, repeated, tag = "2")]
    pub scopes: Vec<String>,
    #[prost(uint64, tag = "3")]
    pub ttl_s: u64,
    #[prost(string, optional, tag = "4")]
    pub session: Option<String>,
    #[prost(string, optional, tag = "5")]
    pub work_item: Option<String>,
}

/// `graft.v1.Token`. The derived `Debug` would print the token, so this type
/// is private to the module and only ever moved into a [`GraftToken`].
#[derive(Clone, PartialEq, prost::Message)]
struct TokenReply {
    #[prost(string, tag = "1")]
    token: String,
}

/// A minted graft token. `Debug` and `Display` are redacted; the value leaves
/// only through [`GraftToken::expose`], into the bootstrap response.
#[derive(Clone, PartialEq, Eq)]
pub struct GraftToken(String);

impl GraftToken {
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for GraftToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GraftToken(<redacted>)")
    }
}

/// What a session's graft token must allow: its repo and line, its identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionGrant {
    pub session: Uuid,
    pub repo: String,
    pub line: String,
    pub work_item: Option<String>,
    pub ttl_s: u64,
}

impl SessionGrant {
    /// The grant for one session's pod. `repo` and `line` must be plain names:
    /// they are spliced into scope strings.
    pub fn new(
        session: Uuid,
        repo: &str,
        line: &str,
        work_item: Option<&str>,
        budget: Option<&SessionBudget>,
    ) -> Result<Self> {
        for (what, value) in [("repo", repo), ("line", line)] {
            if !is_plain_name(value) {
                return Err(ApiError::BadRequest(format!(
                    "graft {what} {value:?} is not a plain name"
                )));
            }
        }
        Ok(Self {
            session,
            repo: repo.to_owned(),
            line: line.to_owned(),
            // The work item is a free-form label on the session; graft takes
            // only a short token without whitespace, and refuses the whole mint
            // otherwise. A label it would refuse is left off, not fatal.
            work_item: work_item
                .filter(|w| {
                    !w.is_empty()
                        && w.len() <= 128
                        && !w.chars().any(|c| c.is_whitespace() || c.is_control())
                })
                .map(str::to_owned),
            ttl_s: ttl_for(budget),
        })
    }

    pub fn subject(&self) -> String {
        format!("vogt-session-{}", self.session)
    }

    /// The repo's read, write and drafts scopes and `land` on this line only.
    /// Never the base line, never another repo, never admin.
    pub fn scopes(&self) -> Vec<String> {
        let repo = &self.repo;
        vec![
            format!("repo:{repo}:read"),
            format!("repo:{repo}:write"),
            format!("repo:{repo}:drafts"),
            format!("repo:{repo}:land:{}", self.line),
        ]
    }

    fn request(&self) -> MintTokenRequest {
        MintTokenRequest {
            subject: self.subject(),
            scopes: self.scopes(),
            ttl_s: self.ttl_s,
            session: Some(self.session.to_string()),
            work_item: self.work_item.clone(),
        }
    }
}

fn is_plain_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Session budget plus slack, capped at [`MAX_TTL_S`]. A session with no
/// whole-session clock budget gets the cap.
pub fn ttl_for(budget: Option<&SessionBudget>) -> u64 {
    match budget
        .and_then(|b| b.session_wall_clock_minutes)
        .filter(|m| *m > 0)
    {
        Some(minutes) => (u64::from(minutes) * 60 + TTL_SLACK_S).min(MAX_TTL_S),
        None => MAX_TTL_S,
    }
}

pub type MintFuture<'a> = Pin<Box<dyn Future<Output = Result<GraftToken>> + Send + 'a>>;
pub type RevokeFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// Mints graft tokens for sessions. The production implementation is
/// [`GrpcGraftMinter`]; tests substitute a fake.
pub trait GraftMinter: Send + Sync {
    fn mint<'a>(&'a self, grant: &'a SessionGrant) -> MintFuture<'a>;

    /// Revoke the token minted for `session`, when its pod is gone. The hook
    /// for WI-3235: graft has no revoke RPC, so the default does nothing and a
    /// token lives until its TTL. When the RPC exists, an implementation keeps
    /// the token's id from `mint` and calls it here.
    fn revoke(&self, _session: Uuid) -> RevokeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}

/// The `AdminService.MintToken` client.
pub struct GrpcGraftMinter {
    endpoint: String,
    token_file: PathBuf,
    channel: tokio::sync::OnceCell<Channel>,
}

impl GrpcGraftMinter {
    /// `endpoint` is graft's URL (`http://host:port`); `token_file` is the
    /// mounted mint token, read on every mint so a rotation needs no restart.
    pub fn new(endpoint: impl Into<String>, token_file: impl Into<PathBuf>) -> Self {
        Self {
            endpoint: endpoint.into(),
            token_file: token_file.into(),
            channel: tokio::sync::OnceCell::new(),
        }
    }

    async fn channel(&self) -> Result<Channel> {
        self.channel
            .get_or_try_init(|| async {
                let mut endpoint = Endpoint::from_shared(self.endpoint.clone())
                    .map_err(|_| ApiError::Config("graft endpoint is not a valid URL".into()))?
                    .connect_timeout(MINT_TIMEOUT)
                    .timeout(MINT_TIMEOUT);
                if self.endpoint.starts_with("https://") {
                    endpoint = endpoint
                        .tls_config(ClientTlsConfig::new().with_webpki_roots())
                        .map_err(|_| ApiError::Config("graft endpoint TLS setup failed".into()))?;
                }
                Ok(endpoint.connect_lazy())
            })
            .await
            .cloned()
    }

    async fn read_mint_token(&self) -> Result<String> {
        let text = tokio::fs::read_to_string(&self.token_file)
            .await
            .map_err(|e| {
                ApiError::Config(format!(
                    "ENGINE_GRAFT_MINT_TOKEN_FILE is unreadable ({})",
                    e.kind()
                ))
            })?;
        let token = text.trim();
        if token.is_empty() {
            return Err(ApiError::Config(
                "ENGINE_GRAFT_MINT_TOKEN_FILE is empty".into(),
            ));
        }
        Ok(token.to_owned())
    }

    async fn call(&self, grant: &SessionGrant) -> Result<GraftToken> {
        let mint_token = self.read_mint_token().await?;
        let channel = self.channel().await?;
        let mut request = tonic::Request::new(grant.request());
        let md = request.metadata_mut();
        md.insert(
            "graft-protocol",
            MetadataValue::from_static(PROTOCOL_VERSION),
        );
        let mut auth: MetadataValue<Ascii> = format!("Bearer {mint_token}")
            .parse()
            .map_err(|_| ApiError::Config("ENGINE_GRAFT_MINT_TOKEN_FILE is not a token".into()))?;
        auth.set_sensitive(true);
        md.insert("authorization", auth);

        let mut grpc = tonic::client::Grpc::new(channel);
        grpc.ready().await.map_err(|_| unreachable_graft())?;
        let reply = grpc
            .unary::<_, TokenReply, _>(
                request,
                PathAndQuery::from_static(MINT_PATH),
                tonic_prost::ProstCodec::<MintTokenRequest, TokenReply>::default(),
            )
            .await
            .map_err(|status| match status.code() {
                Code::Unauthenticated | Code::PermissionDenied => ApiError::Forbidden(format!(
                    "graft refused the engine's mint token ({:?})",
                    status.code()
                )),
                Code::InvalidArgument | Code::OutOfRange | Code::ResourceExhausted => {
                    ApiError::BadGateway(format!(
                        "graft refused the session token request ({:?})",
                        status.code()
                    ))
                }
                _ => unreachable_graft(),
            })?
            .into_inner();
        if reply.token.is_empty() {
            return Err(ApiError::BadGateway("graft minted an empty token".into()));
        }
        Ok(GraftToken(reply.token))
    }
}

fn unreachable_graft() -> ApiError {
    ApiError::BadGateway("graft could not be reached to mint a session token".into())
}

impl GraftMinter for GrpcGraftMinter {
    fn mint<'a>(&'a self, grant: &'a SessionGrant) -> MintFuture<'a> {
        Box::pin(self.call(grant))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tonic::codegen::{http, Body, StdError};

    fn session() -> Uuid {
        Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap()
    }

    #[test]
    fn scopes_are_the_sessions_repo_and_line_only() {
        let g = SessionGrant::new(session(), "vogt", "rust-2", Some("WI-3229"), None).unwrap();
        assert_eq!(
            g.scopes(),
            [
                "repo:vogt:read",
                "repo:vogt:write",
                "repo:vogt:drafts",
                "repo:vogt:land:rust-2"
            ]
        );
        let req = g.request();
        assert_eq!(
            req.subject,
            "vogt-session-11111111-2222-3333-4444-555555555555"
        );
        assert_eq!(
            req.session.as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
        assert_eq!(req.work_item.as_deref(), Some("WI-3229"));
        // Another line gives another land scope and nothing broader.
        let other = SessionGrant::new(session(), "rusttorrent", "backlog-1", None, None).unwrap();
        assert!(other
            .scopes()
            .contains(&"repo:rusttorrent:land:backlog-1".to_string()));
        assert!(other.scopes().iter().all(|s| !s.contains("vogt")));
        assert_eq!(other.request().work_item, None);
    }

    #[test]
    fn a_work_item_label_graft_would_refuse_is_left_off_not_fatal() {
        let grant =
            |w: &str| SessionGrant::new(session(), "vogt", "rust-2", Some(w), None).unwrap();
        assert_eq!(grant("WI-3229").work_item.as_deref(), Some("WI-3229"));
        assert_eq!(grant("two words").work_item, None);
        assert_eq!(grant("tab\there").work_item, None);
        assert_eq!(grant(&"x".repeat(129)).work_item, None);
        assert_eq!(grant("").work_item, None);
    }

    #[test]
    fn repo_and_line_cannot_smuggle_extra_scopes() {
        for bad in ["", "a:b", "a b", "a\nb", "-x", "a/b", &"x".repeat(65)] {
            assert!(SessionGrant::new(session(), bad, "rust-2", None, None).is_err());
            assert!(SessionGrant::new(session(), "vogt", bad, None, None).is_err());
        }
    }

    #[test]
    fn ttl_is_the_session_budget_plus_slack_capped_at_twelve_hours() {
        let budget = |m| SessionBudget {
            session_wall_clock_minutes: m,
            ..Default::default()
        };
        assert_eq!(ttl_for(Some(&budget(Some(90)))), 90 * 60 + TTL_SLACK_S);
        assert_eq!(ttl_for(Some(&budget(Some(100_000)))), MAX_TTL_S);
        // The cap is exact: 12 h minus the slack still fits, one minute more is cut.
        assert_eq!(ttl_for(Some(&budget(Some(690)))), MAX_TTL_S);
        assert_eq!(ttl_for(Some(&budget(Some(689)))), 689 * 60 + TTL_SLACK_S);
        // No whole-session clock budget (or zero, which means none): the cap.
        assert_eq!(ttl_for(Some(&budget(None))), MAX_TTL_S);
        assert_eq!(ttl_for(Some(&budget(Some(0)))), MAX_TTL_S);
        assert_eq!(ttl_for(None), MAX_TTL_S);
    }

    #[test]
    fn a_token_never_prints() {
        let t = GraftToken::new("secret-graft-token-value");
        assert!(!format!("{t:?}").contains("secret-graft-token-value"));
    }

    /// What the fake graft saw.
    #[derive(Default, Clone)]
    struct Seen {
        authorization: Option<String>,
        protocol: Option<String>,
        path: Option<String>,
        request: Option<MintTokenRequest>,
    }

    /// A fake graft `AdminService` that answers MintToken with `reply`.
    #[derive(Clone)]
    struct FakeGraft {
        seen: Arc<Mutex<Seen>>,
        reply: std::result::Result<String, tonic::Code>,
    }
    impl tonic::server::NamedService for FakeGraft {
        const NAME: &'static str = "graft.v1.AdminService";
    }
    struct Mint(FakeGraft, http::HeaderMap, String);
    impl tonic::server::UnaryService<MintTokenRequest> for Mint {
        type Response = TokenReply;
        type Future =
            std::future::Ready<std::result::Result<tonic::Response<TokenReply>, tonic::Status>>;
        fn call(&mut self, request: tonic::Request<MintTokenRequest>) -> Self::Future {
            let mut seen = self.0.seen.lock().unwrap();
            seen.authorization = self
                .1
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            seen.protocol = self
                .1
                .get("graft-protocol")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            seen.path = Some(self.2.clone());
            seen.request = Some(request.into_inner());
            std::future::ready(match &self.0.reply {
                Ok(token) => Ok(tonic::Response::new(TokenReply {
                    token: token.clone(),
                })),
                Err(code) => Err(tonic::Status::new(*code, "no")),
            })
        }
    }
    impl<B> tower::Service<http::Request<B>> for FakeGraft
    where
        B: Body + Send + 'static,
        B::Error: Into<StdError> + Send + 'static,
    {
        type Response = http::Response<tonic::body::Body>;
        type Error = std::convert::Infallible;
        type Future =
            Pin<Box<dyn Future<Output = std::result::Result<Self::Response, Self::Error>> + Send>>;
        fn poll_ready(
            &mut self,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn call(&mut self, req: http::Request<B>) -> Self::Future {
            let svc = Mint(
                self.clone(),
                req.headers().clone(),
                req.uri().path().to_owned(),
            );
            Box::pin(async move {
                let mut grpc = tonic::server::Grpc::new(tonic_prost::ProstCodec::<
                    TokenReply,
                    MintTokenRequest,
                >::default());
                Ok(grpc.unary(svc, req).await)
            })
        }
    }

    async fn serve(reply: std::result::Result<String, tonic::Code>) -> (String, Arc<Mutex<Seen>>) {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let svc = FakeGraft {
            seen: Arc::clone(&seen),
            reply,
        };
        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(svc)
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
                .unwrap();
        });
        (format!("http://{addr}"), seen)
    }

    fn mint_file(dir: &tempfile::TempDir, body: &str) -> PathBuf {
        let path = dir.path().join("graft-mint-token");
        std::fs::write(&path, body).unwrap();
        path
    }

    #[tokio::test]
    async fn the_client_sends_the_grant_and_the_mint_token_and_returns_the_minted_one() {
        let (url, seen) = serve(Ok("minted-session-token".into())).await;
        let dir = tempfile::tempdir().unwrap();
        let minter = GrpcGraftMinter::new(url, mint_file(&dir, "engine-mint-token\n"));
        let grant = SessionGrant::new(
            session(),
            "vogt",
            "rust-2",
            Some("WI-3229"),
            Some(&SessionBudget {
                session_wall_clock_minutes: Some(90),
                ..Default::default()
            }),
        )
        .unwrap();
        let token = minter.mint(&grant).await.unwrap();
        assert_eq!(token.expose(), "minted-session-token");
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.path.as_deref(), Some(MINT_PATH));
        assert_eq!(
            seen.authorization.as_deref(),
            Some("Bearer engine-mint-token")
        );
        assert_eq!(seen.protocol.as_deref(), Some("0.1"));
        let req = seen.request.unwrap();
        assert_eq!(req.scopes, grant.scopes());
        assert_eq!(req.ttl_s, 90 * 60 + TTL_SLACK_S);
        assert_eq!(req.subject, grant.subject());
        assert_eq!(req.session.as_deref(), Some(session().to_string().as_str()));
        assert_eq!(req.work_item.as_deref(), Some("WI-3229"));
    }

    #[tokio::test]
    async fn a_refusal_names_the_code_and_leaks_no_token() {
        let (url, _) = serve(Err(tonic::Code::PermissionDenied)).await;
        let dir = tempfile::tempdir().unwrap();
        let minter = GrpcGraftMinter::new(url, mint_file(&dir, "engine-mint-token"));
        let grant = SessionGrant::new(session(), "vogt", "rust-2", None, None).unwrap();
        let err = minter.mint(&grant).await.unwrap_err();
        assert!(matches!(err, ApiError::Forbidden(_)), "{err:?}");
        let text = format!("{err} {err:?}");
        assert!(!text.contains("engine-mint-token"), "{text}");
    }

    #[tokio::test]
    async fn an_unreachable_graft_is_a_bad_gateway() {
        let dir = tempfile::tempdir().unwrap();
        // Port 1 on loopback: nothing listens.
        let minter =
            GrpcGraftMinter::new("http://127.0.0.1:1", mint_file(&dir, "engine-mint-token"));
        let grant = SessionGrant::new(session(), "vogt", "rust-2", None, None).unwrap();
        let err = minter.mint(&grant).await.unwrap_err();
        assert!(matches!(err, ApiError::BadGateway(_)), "{err:?}");
    }

    #[tokio::test]
    async fn a_missing_or_empty_mint_token_file_is_a_config_error() {
        let dir = tempfile::tempdir().unwrap();
        let grant = SessionGrant::new(session(), "vogt", "rust-2", None, None).unwrap();
        let missing = GrpcGraftMinter::new("http://127.0.0.1:1", dir.path().join("nope"));
        assert!(matches!(
            missing.mint(&grant).await.unwrap_err(),
            ApiError::Config(_)
        ));
        let empty = GrpcGraftMinter::new("http://127.0.0.1:1", mint_file(&dir, " \n"));
        assert!(matches!(
            empty.mint(&grant).await.unwrap_err(),
            ApiError::Config(_)
        ));
    }

    #[tokio::test]
    async fn the_revoke_hook_is_a_no_op_until_graft_has_the_rpc() {
        let minter = GrpcGraftMinter::new("http://127.0.0.1:1", "/nonexistent");
        minter.revoke(session()).await.unwrap();
    }
}

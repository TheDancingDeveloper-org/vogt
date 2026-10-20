//! The forge adapter can authenticate as a GitHub App installation. Ports
//! `tests/test_github_app_auth.py` (WI-1838).
//!
//! Tested through the transport seam, with fixture IDs and owners only: the
//! JWT is a real RS256 signature over a throwaway key, each installation token
//! is cached until five minutes before expiry, a 401 refreshes it once, the
//! installation is discovered per owner when none is configured, App config
//! wins over the token file, and `create_repo` targets an organisation
//! installation's `/orgs/{owner}/repos`.
//!
//! Not ported: the two `github_collectors` notification tests. That legacy
//! collector set (`adapters/github/collectors.py`) has no Rust port; the forge
//! read collectors resolve through the registry like every other read.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use ring::signature::{UnparsedPublicKey, RSA_PKCS1_2048_8192_SHA256};
use serde_json::{json, Value};

use super::*;
use crate::adapters::forge::registry::github_provider;
use crate::adapters::forge::{ForgeProvider, ForgeResponse, RepoRef, WriteMethod};
use crate::adapters::github::{with_budget, GitHubClient, SharedTransport};
use crate::config::VogtConfig;
use crate::errors::VogtError;

const APP_ID: &str = "12345";
const CLIENT_ID: &str = "Iv1.0123456789abcdef";
const NOW: f64 = 1_800_000_000.0;

/// Fixture installations: owner → (installation id, account type).
const INSTALLS: [(&str, i64, &str); 2] = [
    ("example-org", 111, "Organization"),
    ("example-user", 222, "User"),
];

// A throwaway 2048-bit RSA key generated only for tests — it protects nothing.
// The same fixture `engine/server/src/push_fcm.rs` signs its RS256 test with.
const TEST_RSA_PEM: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCwU2YdZrx4eRtn
jTMCXIkDykCruarfCUskyF7YiOmjqaK4SH7pD+HCyBxqwxvz2Npz9XMU5QSM3z5I
MQC6FsVVVND7J960Bjn7xleXh+hLn0t3fgMTKfxCeh4lBxBbw5cDf9hKSdwwaGlI
7HE2VF2GorP6AM/5xOGdLd/8MFWUtxWIglJjKecXaY2aeqd7qUHZxxMUf9ORRt2s
1sA6mHqjyS1FaN+Er80zsUGZf+yi+nv6s6vTBH1dOROG4FyLGQgAO7OtphP/MpIm
IgKqZaW/dig2+STPE75aZc8iAI1KdgZHLSee2aO7R/lWSS0P9aDJ1zjKtgcqnklZ
gGRN5yLtAgMBAAECggEAFlS5ngeg3vEXk2nCiA4IHD7FKfp9tXmL1sC5olrUnDGj
tgsYZ5PnV6/YSX0kUSGpqP3w8Th/9wde7+2k1eyEWWZAgU5vWgVf2o8oELzZopou
CJgg85Bhrlmg77eRTs3alkreranlBQx0KRQo0mSk46Q/fu4TaOKSYEGrGFlWvVqs
T+lCll1MapYIi9XKrJ/F4M2jg5Dk2FyQ5PM16+97Nc7sry3Nluha3lTPRNCyu+Tx
ma7+W/Xmf6ENvL2Qe+DuRQ6X+4eIYGocuf3C2WQ4Z2vWTKOivClHWLZ21fC9se95
4NScbLGyMs+uyOJRq8lLjqJ00BPQYOWJerkHvhw9AQKBgQDxuIXeU+HM6Y6Ke57D
qJfp48hdfE1VUWo2zRTwoB8N7VtK5K7M6pMbA06CJynAkPIWjuwYxuTpCMoiusyK
Wuzs06PBWPtIXzRim+knePNZj/4TP3W9cmPR80bJkAH5OMouapwGtUbn2v02YKWg
sUWZ2uD5opgbPSl48EyiJeKlHQKBgQC6vezUy8zEMPzX4dcf1TaGdHe6nZzzqYyq
xnZXVzFHCz6OLBfQFO6fegy1CHuxDH2towhYG2nl1SAvqY3sTIEq55njdd6dfCNJ
MswDzyWBABYR/b7jmv2KYgHL0asQ1yf2cuHmRQ0H+2BNfY/5++W2/VWhp2tDQQth
PlBtrAccEQKBgQCiK9tck0h90wB39oJI4Nay68ikt92mbQ3moGb5HWRYUrOaJp0E
DCkPyYMlnSLM+DIDYnYSFXbExcnfzRWniWNFbKSD9q/4GS+rLNEbU3Fo2EttHHlg
1dsUo7QNTRmV3D0BMTNm6L4elfSmQ+c7TVPV6lxf/28vbzRg2E+BxgsuHQKBgD48
ojvhmGMiuIHyoNHZK4zwMB8f/Hkw3tLjxfkh9ChrVPPpOh59e3HnKWkEZMUDFrfS
CCvlJwwUVd/MsKI6dSopeh5W4FS4VMJGjbwPQ76UmsZwPTh6JVoDg/m77Tl1m7oF
W8h05vzsCJKK8HsuOscb83gm5hbKDNLiA8gpd00hAoGADXWzjbTUcY3XUN+C8p19
vclvKPGvKdcMHgwl3de3Aeh5t7/v6G8RAXkHrQSkpp8icU/EcOW9IimrxX4OBRRz
2FHAVfyZtH5NqBFocwHIU4RSWBU31w2dnE7vn1bD6EZpRccVA8uhIPV8tFRS+Pn+
+BxHGVbe+hwuiC/SMdg6iMc=
-----END PRIVATE KEY-----
"#;

fn key() -> RsaKeyPair {
    load_rsa_pem(TEST_RSA_PEM.as_bytes()).expect("the fixture is an RSA PKCS#8 key")
}

fn unb64(segment: &str) -> Vec<u8> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(segment)
        .unwrap()
}

fn iso(epoch: f64) -> String {
    chrono::DateTime::from_timestamp(epoch as i64, 0)
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

type Answer = (u16, Vec<u8>, BTreeMap<String, String>);

fn json_answer(status: u16, payload: Value) -> Answer {
    (status, payload.to_string().into_bytes(), BTreeMap::new())
}

/// A fake GitHub: installations per owner, numbered tokens, plain reads.
struct Fake {
    minted: RefCell<Vec<i64>>,
    expires_at: Cell<f64>,
    revoked: RefCell<HashSet<String>>,
    /// method, path, bearer
    calls: RefCell<Vec<(String, String, String)>>,
    /// Forced status and extra headers for JWT-signed (`/app`, lookup) calls.
    app_status: Cell<Option<u16>>,
    app_headers: RefCell<BTreeMap<String, String>>,
    installations: RefCell<Vec<&'static str>>,
}

impl Fake {
    fn new() -> Rc<Self> {
        Rc::new(Self {
            minted: RefCell::new(Vec::new()),
            expires_at: Cell::new(NOW + 3600.0),
            revoked: RefCell::new(HashSet::new()),
            calls: RefCell::new(Vec::new()),
            app_status: Cell::new(None),
            app_headers: RefCell::new(BTreeMap::new()),
            installations: RefCell::new(INSTALLS.iter().map(|(owner, _, _)| *owner).collect()),
        })
    }

    fn transport(self: &Rc<Self>) -> SharedTransport {
        let fake = Rc::clone(self);
        Rc::new(move |url, headers, _body, method| fake.answer(url, headers, method))
    }

    fn boxed(self: &Rc<Self>) -> crate::adapters::github::Transport {
        let fake = Rc::clone(self);
        Box::new(move |url, headers, _body, method| fake.answer(url, headers, method))
    }

    fn answer(&self, url: &str, headers: &BTreeMap<String, String>, method: &str) -> Answer {
        let path = url.strip_prefix("https://api.github.com").unwrap_or(url);
        let bearer = headers
            .get("Authorization")
            .map(|value| value.strip_prefix("Bearer ").unwrap_or(value).to_owned())
            .unwrap_or_default();
        self.calls
            .borrow_mut()
            .push((method.to_owned(), path.to_owned(), bearer.clone()));
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        if parts[0] == "app" || parts.last() == Some(&"installation") {
            let (status, body, _) = self.app(&parts);
            let status = self.app_status.get().unwrap_or(status);
            return (status, body, self.app_headers.borrow().clone());
        }
        if self.revoked.borrow().contains(&bearer) {
            return json_answer(401, json!({"message": "Bad credentials"}));
        }
        if method == "POST" {
            return json_answer(
                201,
                json!({"name": "fresh", "owner": {"login": "example-org"}}),
            );
        }
        json_answer(200, json!({"ok": true}))
    }

    fn by_id(installation: i64) -> Option<(&'static str, &'static str)> {
        INSTALLS
            .iter()
            .find(|(_, number, _)| *number == installation)
            .map(|(owner, _, kind)| (*owner, *kind))
    }

    fn by_owner(owner: &str) -> Option<(i64, &'static str)> {
        INSTALLS
            .iter()
            .find(|(login, _, _)| *login == owner)
            .map(|(_, number, kind)| (*number, *kind))
    }

    fn app(&self, parts: &[&str]) -> Answer {
        if parts == ["app", "installations"] {
            let listed: Vec<Value> = self
                .installations
                .borrow()
                .iter()
                .map(|owner| {
                    let (number, kind) = Self::by_owner(owner).unwrap();
                    json!({"id": number, "account": {"login": owner, "type": kind}})
                })
                .collect();
            return json_answer(200, Value::Array(listed));
        }
        if parts[0] == "app" {
            let installation: i64 = parts[2].parse().unwrap();
            let Some((owner, kind)) = Self::by_id(installation) else {
                return json_answer(404, json!({"message": "Not Found"}));
            };
            if parts.last() == Some(&"access_tokens") {
                self.minted.borrow_mut().push(installation);
                let token = format!("ghs_{installation}_{}", self.minted.borrow().len());
                return json_answer(
                    201,
                    json!({"token": token, "expires_at": iso(self.expires_at.get())}),
                );
            }
            return json_answer(200, json!({"account": {"login": owner, "type": kind}}));
        }
        if parts.last() == Some(&"installation") {
            let owner = parts[1];
            // A repository lookup finds any installation; the org and user
            // lookups only find their own kind of account.
            let Some((number, kind)) = Self::by_owner(owner) else {
                return json_answer(404, json!({"message": "Not Found"}));
            };
            if parts[0] != "repos" && (parts[0] == "orgs") != (kind == "Organization") {
                return json_answer(404, json!({"message": "Not Found"}));
            }
            return json_answer(
                200,
                json!({"id": number, "account": {"login": owner, "type": kind}}),
            );
        }
        json_answer(404, json!({"message": "Not Found"}))
    }

    fn app_calls(&self) -> Vec<String> {
        self.calls
            .borrow()
            .iter()
            .filter(|(_, path, _)| path.starts_with("/app/") || path.ends_with("/installation"))
            .map(|(_, path, _)| path.clone())
            .collect()
    }

    fn bearers(&self, path: &str) -> Vec<String> {
        self.calls
            .borrow()
            .iter()
            .filter(|(_, called, _)| called == path)
            .map(|(_, _, bearer)| bearer.clone())
            .collect()
    }

    fn posts(&self) -> Vec<String> {
        self.calls
            .borrow()
            .iter()
            .filter(|(method, path, _)| method == "POST" && !path.contains("/app/"))
            .map(|(_, path, _)| path.clone())
            .collect()
    }

    fn minted(&self) -> Vec<i64> {
        self.minted.borrow().clone()
    }
}

fn low() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("X-RateLimit-Remaining".to_owned(), "3".to_owned()),
        (
            "X-RateLimit-Reset".to_owned(),
            (NOW as i64 + 10_000_000).to_string(),
        ),
        ("X-RateLimit-Resource".to_owned(), "core".to_owned()),
    ])
}

fn auth_with(
    fake: &Rc<Fake>,
    clock: &Rc<Cell<f64>>,
    installation_id: Option<i64>,
    app_id: &str,
) -> GitHubAppAuth {
    let clock = Rc::clone(clock);
    GitHubAppAuth::new(
        app_id,
        key(),
        installation_id,
        Some(fake.transport()),
        Some(Rc::new(move || clock.get())),
    )
}

fn auth(fake: &Rc<Fake>, installation_id: Option<i64>) -> GitHubAppAuth {
    auth_with(fake, &Rc::new(Cell::new(NOW)), installation_id, APP_ID)
}

fn app_client(auth: GitHubAppAuth, fake: &Rc<Fake>) -> GitHubClient {
    GitHubClient::with_token_provider(auth, Some(fake.transport()))
}

fn ok() -> ForgeResponse {
    ForgeResponse::Json(json!({"ok": true}))
}

fn unavailable(result: Result<impl std::fmt::Debug, VogtError>) -> String {
    match result {
        Err(VogtError::GitHubUnavailable(message)) => message,
        other => panic!("expected github_unavailable, got {other:?}"),
    }
}

fn rate_limited<T: std::fmt::Debug>(result: Result<T, VogtError>) -> bool {
    matches!(result, Err(VogtError::GitHubRateLimited { .. }))
}

// -- the JWT ------------------------------------------------------------------

#[test]
fn the_jwt_is_rs256_signed_with_the_app_claims() {
    let key = key();
    for issuer in [APP_ID, CLIENT_ID] {
        let token = sign_jwt(&key, issuer, NOW);
        let parts: Vec<&str> = token.split('.').collect();
        let [header, claims, signature] = parts.as_slice() else {
            panic!("three segments: {token}");
        };
        let header_json: Value = serde_json::from_slice(&unb64(header)).unwrap();
        assert_eq!(header_json, json!({"alg": "RS256", "typ": "JWT"}));
        let claims_json: Value = serde_json::from_slice(&unb64(claims)).unwrap();
        assert_eq!(
            claims_json,
            json!({"iat": NOW as i64 - 60, "exp": NOW as i64 + 540, "iss": issuer})
        );
        // Python's `json.dumps(..., separators=(",", ":"))`, byte for byte.
        assert_eq!(
            String::from_utf8(unb64(claims)).unwrap(),
            format!(
                r#"{{"iat":{},"exp":{},"iss":"{issuer}"}}"#,
                NOW as i64 - 60,
                NOW as i64 + 540
            )
        );
        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key.public().as_ref())
            .verify(format!("{header}.{claims}").as_bytes(), &unb64(signature))
            .expect("a valid RS256 signature");
    }
}

#[test]
fn the_token_exchange_is_authenticated_by_the_jwt() {
    let fake = Fake::new();
    assert_eq!(
        auth(&fake, Some(111)).token(None).unwrap().as_deref(),
        Some("ghs_111_1")
    );
    let (method, path, bearer) = fake.calls.borrow()[0].clone();
    assert_eq!(
        (method.as_str(), path.as_str()),
        ("POST", "/app/installations/111/access_tokens")
    );
    let claims: Value = serde_json::from_slice(&unb64(bearer.split('.').nth(1).unwrap())).unwrap();
    assert_eq!(claims["iss"], APP_ID);
}

// -- caching and refresh ------------------------------------------------------

#[test]
fn the_token_is_cached_until_five_minutes_before_expiry() {
    let fake = Fake::new();
    let clock = Rc::new(Cell::new(NOW));
    let auth = auth_with(&fake, &clock, Some(111), APP_ID);
    assert_eq!(auth.token(None).unwrap().as_deref(), Some("ghs_111_1"));
    clock.set(NOW + 3600.0 - 301.0);
    assert_eq!(auth.token(None).unwrap().as_deref(), Some("ghs_111_1"));
    clock.set(NOW + 3600.0 - 300.0);
    fake.expires_at.set(clock.get() + 3600.0);
    assert_eq!(auth.token(None).unwrap().as_deref(), Some("ghs_111_2"));
    assert_eq!(fake.minted(), [111, 111]);
}

#[test]
fn a_401_refreshes_the_token_once_and_retries() {
    let fake = Fake::new();
    let client = app_client(auth(&fake, Some(111)), &fake);
    assert_eq!(client.get("/repos/example-org/r", &[]).unwrap(), ok());
    fake.revoked.borrow_mut().insert("ghs_111_1".into());
    assert_eq!(
        client.get("/repos/example-org/r/releases", &[]).unwrap(),
        ok()
    );
    assert_eq!(
        fake.bearers("/repos/example-org/r/releases"),
        ["ghs_111_1", "ghs_111_2"]
    );
}

#[test]
fn a_second_401_is_not_retried_again() {
    let fake = Fake::new();
    fake.revoked
        .borrow_mut()
        .extend(["ghs_111_1", "ghs_111_2", "ghs_111_3"].map(String::from));
    let client = app_client(auth(&fake, Some(111)), &fake);
    unavailable(client.get("/repos/example-org/r", &[]));
    assert_eq!(fake.minted(), [111, 111]);
}

#[test]
fn a_fixed_token_is_not_refreshed_on_401() {
    let fake = Fake::new();
    fake.revoked.borrow_mut().insert("pat-fixture".into());
    let client = GitHubClient::new(Some("pat-fixture".into()), Some(fake.boxed()));
    assert!(unavailable(client.get("/repos/example-org/r", &[])).contains("401"));
    assert!(fake.minted().is_empty());
    assert_eq!(fake.calls.borrow().len(), 1);
}

// -- installation discovery ---------------------------------------------------

#[test]
fn installations_are_discovered_per_owner() {
    let fake = Fake::new();
    let client = app_client(auth(&fake, None), &fake);
    client.get("/repos/example-org/a", &[]).unwrap();
    client.get("/repos/example-org/b", &[]).unwrap();
    client.get("/repos/example-user/c", &[]).unwrap();
    assert_eq!(fake.bearers("/repos/example-org/a"), ["ghs_111_1"]);
    assert_eq!(fake.bearers("/repos/example-org/b"), ["ghs_111_1"]);
    assert_eq!(fake.bearers("/repos/example-user/c"), ["ghs_222_2"]);
    // One discovery per owner, then remembered.
    assert!(!fake.bearers("/repos/example-org/a/installation").is_empty());
    assert!(fake.bearers("/repos/example-org/b/installation").is_empty());
    assert_eq!(fake.minted(), [111, 222]);
}

#[test]
fn an_owner_without_the_app_falls_back_to_the_pat() {
    // F1: not an error; the owner is read with the personal token.
    let fake = Fake::new();
    let mut auth = auth(&fake, None);
    auth.fallback_token = Some("pat-fixture".into());
    let client = app_client(auth, &fake);
    assert_eq!(client.get("/repos/stranger/r", &[]).unwrap(), ok());
    assert_eq!(fake.bearers("/repos/stranger/r"), ["pat-fixture"]);
    assert!(fake.minted().is_empty());
}

#[test]
fn not_installed_is_cached_per_owner_with_a_ttl() {
    // F1: no further App calls for that owner until the TTL runs out.
    let fake = Fake::new();
    let clock = Rc::new(Cell::new(NOW));
    let mut auth = auth_with(&fake, &clock, None, APP_ID);
    auth.fallback_token = Some("pat-fixture".into());
    let client = app_client(auth, &fake);
    client.get("/repos/stranger/a", &[]).unwrap();
    let app_calls = fake.app_calls().len();
    assert_eq!(app_calls, 3, "repo, org and user installation lookups");
    client.get("/repos/stranger/b", &[]).unwrap();
    clock.set(NOW + NOT_INSTALLED_TTL_SECONDS - 1.0);
    client.get("/repos/stranger/c", &[]).unwrap();
    assert_eq!(fake.app_calls().len(), app_calls);
    clock.set(NOW + NOT_INSTALLED_TTL_SECONDS);
    client.get("/repos/stranger/d", &[]).unwrap();
    assert!(fake.app_calls().len() > app_calls);
}

#[test]
fn an_owner_without_the_app_and_no_pat_is_read_anonymously() {
    let fake = Fake::new();
    let client = app_client(auth(&fake, None), &fake);
    assert_eq!(client.get("/repos/stranger/r", &[]).unwrap(), ok());
    assert_eq!(fake.bearers("/repos/stranger/r"), [""]);
}

#[test]
fn the_fallback_owner_shares_the_pat_budget() {
    let fake = Fake::new();
    let mut auth = auth(&fake, None);
    auth.fallback_token = Some("pat-fixture".into());
    let pat = GitHubClient::new(Some("pat-fixture".into()), None);
    assert_eq!(
        auth.cache_key(Some("/repos/stranger/r")).unwrap(),
        pat.budget_key(None).unwrap()
    );
}

#[test]
fn a_fallback_pat_401_is_not_retried() {
    let fake = Fake::new();
    fake.revoked.borrow_mut().insert("pat-fixture".into());
    let mut auth = auth(&fake, None);
    auth.fallback_token = Some("pat-fixture".into());
    let client = app_client(auth, &fake);
    assert!(unavailable(client.get("/repos/stranger/r", &[])).contains("401"));
    assert_eq!(fake.bearers("/repos/stranger/r"), ["pat-fixture"]);
}

#[test]
fn a_failed_lookup_is_not_cached_as_not_installed() {
    let fake = Fake::new();
    fake.app_status.set(Some(500));
    let auth = auth(&fake, None);
    unavailable(auth.token(Some("/repos/example-org/a")));
    fake.app_status.set(None);
    assert_eq!(
        auth.token(Some("/repos/example-org/a")).unwrap().as_deref(),
        Some("ghs_111_1")
    );
}

// -- F2: the rate budget comes first ----------------------------------------

#[test]
fn a_low_installation_budget_mints_nothing() {
    let fake = Fake::new();
    let auth = auth(&fake, Some(111));
    // The budget reads response headers, which the client lower-cases.
    let lowered: BTreeMap<String, String> = low()
        .into_iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value))
        .collect();
    let key = auth.cache_key(None).unwrap();
    with_budget(|state| state.budget.observe(&key, &lowered));
    let client = app_client(auth, &fake);
    assert!(rate_limited(client.get("/repos/example-org/a", &[])));
    assert!(rate_limited(client.send(
        WriteMethod::Post,
        "/repos/example-org/a/issues",
        Some(&json!({"title": "t"}))
    )));
    assert!(fake.calls.borrow().is_empty());
}

#[test]
fn a_low_app_budget_stops_lookups_and_mints() {
    let fake = Fake::new();
    let auth = auth(&fake, None);
    assert_eq!(
        auth.token(Some("/repos/example-org/a")).unwrap().as_deref(),
        Some("ghs_111_1")
    );
    // A JWT-signed response reports the App's budget is nearly spent.
    *fake.app_headers.borrow_mut() = low();
    *fake.installations.borrow_mut() = vec!["example-org"];
    auth.app_get("/app/installations").unwrap();
    let calls = fake.calls.borrow().len();
    // Would need a lookup.
    assert!(rate_limited(auth.token(Some("/repos/example-user/a"))));
    auth.invalidate(Some("/repos/example-org/a")).unwrap();
    // Would need a mint.
    assert!(rate_limited(auth.token(Some("/repos/example-org/a"))));
    assert_eq!(fake.calls.borrow().len(), calls);
}

#[test]
fn a_rate_limited_lookup_is_not_cached_as_not_installed() {
    let fake = Fake::new();
    fake.app_status.set(Some(403));
    let mut exhausted = low();
    exhausted.insert("X-RateLimit-Remaining".into(), "0".into());
    *fake.app_headers.borrow_mut() = exhausted;
    let mut auth = auth(&fake, None);
    auth.fallback_token = Some("pat-fixture".into());
    assert!(rate_limited(auth.token(Some("/repos/example-org/a"))));
    with_budget(|state| *state = Default::default());
    fake.app_status.set(None);
    fake.app_headers.borrow_mut().clear();
    assert_eq!(
        auth.token(Some("/repos/example-org/a")).unwrap().as_deref(),
        Some("ghs_111_1")
    );
}

#[test]
fn a_configured_installation_skips_discovery() {
    let fake = Fake::new();
    app_client(auth(&fake, Some(222)), &fake)
        .get("/repos/example-org/a", &[])
        .unwrap();
    assert!(!fake
        .calls
        .borrow()
        .iter()
        .any(|(_, path, _)| path.ends_with("/installation")));
    assert_eq!(fake.minted(), [222]);
}

#[test]
fn installations_have_separate_rate_budgets() {
    let fake = Fake::new();
    let auth = auth(&fake, None);
    assert_ne!(
        auth.cache_key(Some("/repos/example-org/a")).unwrap(),
        auth.cache_key(Some("/repos/example-user/a")).unwrap()
    );
    assert_eq!(
        auth.cache_key(Some("/repos/example-org/a")).unwrap(),
        auth.cache_key(Some("/repos/example-org/b")).unwrap()
    );
}

#[test]
fn an_ownerless_request_needs_one_known_installation() {
    let fake = Fake::new();
    let auth = auth(&fake, None);
    assert!(unavailable(auth.token(None)).contains("names no owner"));
    auth.token(Some("/repos/example-org/a")).unwrap();
    assert_eq!(auth.token(None).unwrap().as_deref(), Some("ghs_111_1"));
    auth.token(Some("/repos/example-user/a")).unwrap();
    assert!(unavailable(auth.token(None)).contains("names no owner"));
}

// -- configuration and wiring ------------------------------------------------

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vogt-github-app-{}-{name}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn key_file(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("app.pem");
    std::fs::write(&path, TEST_RSA_PEM).unwrap();
    path
}

fn pat_file(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("github_token");
    std::fs::write(&path, "pat-fixture\n").unwrap();
    path
}

fn config(
    key_file: PathBuf,
    token_file: Option<PathBuf>,
    installation_id: Option<i64>,
) -> VogtConfig {
    VogtConfig {
        github_app_id: Some(APP_ID.into()),
        github_app_installation_id: installation_id,
        github_app_private_key_file: Some(key_file),
        github_token_file: token_file,
        ..VogtConfig::default()
    }
}

#[test]
fn app_config_takes_precedence_over_the_token_file() {
    let dir = scratch("precedence");
    let fake = Fake::new();
    let token_file = pat_file(&dir);
    let config = config(key_file(&dir), Some(token_file.clone()), Some(111));
    let client = github_client_for(&config, Some(&token_file), Some(fake.boxed()))
        .unwrap()
        .unwrap();
    assert_eq!(client.bearer(None).unwrap().as_deref(), Some("ghs_111_1"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn without_the_app_the_token_file_is_used() {
    let dir = scratch("pat");
    let token_file = pat_file(&dir);
    let config = VogtConfig {
        github_token_file: Some(token_file.clone()),
        ..VogtConfig::default()
    };
    let client = github_client_for(&config, Some(&token_file), None)
        .unwrap()
        .unwrap();
    assert_eq!(client.token(), Some("pat-fixture"));
    assert!(client.token_provider().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unreadable_key_falls_back_to_the_token_file() {
    let dir = scratch("bad-key");
    let bad = dir.join("bad.pem");
    std::fs::write(&bad, "not a key").unwrap();
    let token_file = pat_file(&dir);
    let client = github_client_for(
        &config(bad, Some(token_file.clone()), None),
        Some(&token_file),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(client.token(), Some("pat-fixture"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn providers_share_one_cached_installation_token() {
    let dir = scratch("shared");
    let fake = Fake::new();
    let config = config(key_file(&dir), None, None);
    let repo = RepoRef {
        host: "github.com".into(),
        owner: "example-org".into(),
        repo: "a".into(),
    };
    for _ in 0..3 {
        let provider = github_provider(&config, Some(fake.boxed()))
            .unwrap()
            .unwrap();
        assert_eq!(
            provider.clone_token(Some(&repo)).unwrap().as_deref(),
            Some("ghs_111_1")
        );
    }
    assert_eq!(fake.minted(), [111]);
    let _ = std::fs::remove_dir_all(&dir);
}

// -- create_repo ---------------------------------------------------------------

fn create(client: GitHubClient) -> Result<(), VogtError> {
    crate::adapters::forge::registry::github_provider_over(client)
        .create_repo("fresh", true, None)
        .map(|_| ())
}

#[test]
fn create_repo_uses_the_org_endpoint_for_an_org_installation() {
    let fake = Fake::new();
    create(app_client(auth(&fake, Some(111)), &fake)).unwrap();
    assert_eq!(fake.posts(), ["/orgs/example-org/repos"]);
}

#[test]
fn create_repo_uses_the_user_endpoint_for_a_user_installation() {
    let fake = Fake::new();
    create(app_client(auth(&fake, Some(222)), &fake)).unwrap();
    assert_eq!(fake.posts(), ["/user/repos"]);
}

#[test]
fn create_repo_with_a_pat_is_unchanged() {
    let fake = Fake::new();
    create(GitHubClient::new(
        Some("pat-fixture".into()),
        Some(fake.boxed()),
    ))
    .unwrap();
    assert_eq!(fake.posts(), ["/user/repos"]);
}

// -- F3: publishing without a configured installation ------------------------

#[test]
fn publish_discovers_the_only_installation() {
    let fake = Fake::new();
    *fake.installations.borrow_mut() = vec!["example-org"];
    create(app_client(auth(&fake, None), &fake)).unwrap();
    assert!(fake.app_calls().contains(&"/app/installations".to_owned()));
    assert_eq!(fake.posts(), ["/orgs/example-org/repos"]);
}

#[test]
fn publish_refuses_to_guess_between_installations() {
    let fake = Fake::new();
    let message = unavailable(create(app_client(auth(&fake, None), &fake)));
    assert!(message.contains("github_app_installation_id"), "{message}");
    assert!(fake.posts().is_empty());
}

// -- F4: a broken key is loud -------------------------------------------------

#[test]
fn a_missing_key_file_warns_once_and_uses_the_pat() {
    let dir = scratch("absent");
    let token_file = pat_file(&dir);
    let config = config(dir.join("absent.pem"), Some(token_file.clone()), None);
    for _ in 0..3 {
        let client = github_client_for(&config, Some(&token_file), None)
            .unwrap()
            .unwrap();
        assert_eq!(client.token(), Some("pat-fixture"));
    }
    let warned = with_auths(|cache| cache.warned.clone());
    assert_eq!(warned.len(), 1, "one warning per key file, not per build");
    assert!(warned.iter().all(|path| path.ends_with("absent.pem")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_invalid_key_file_warns() {
    let dir = scratch("invalid");
    let bad = dir.join("bad.pem");
    std::fs::write(&bad, "not a key").unwrap();
    assert!(github_client_for(&config(bad, None, None), None, None)
        .unwrap()
        .is_none());
    assert!(with_auths(|cache| cache
        .warned
        .iter()
        .any(|path| path.ends_with("bad.pem"))));
    let _ = std::fs::remove_dir_all(&dir);
}

//! WI-4282: the *Dev Admin Token Lifecycle* exception (WI-4279) admits a
//! session only on engine proof of its oversight role, run in its own
//! transcript: an `auth_whoami` naming one of the identities the rule lists,
//! and a `session_list` row with that same id and `role` `oversight`. These
//! tests run both operations as the core answers them and judge the result
//! against the identity forms the shipped rule names, so the rule and the
//! core cannot drift apart.
//!
//! A person who opens an agent session from the GUI and makes it oversight
//! gets no Vogt record: the engine minted its credential (`session.token`,
//! `agent:engine:<uuid>`), and `session_list` shows it unlinked, with the
//! engine's uuid as its `id`. Before WI-4282 the rule named only
//! `agent:session:<id>`, so that session could never prove its role.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::adapters::engine::EngineClient;
use crate::application::context::{build_context, Built};
use crate::core::{ActorKind, Moment, Principal, StepClock};
use crate::errors::VogtError;

/// The policy every Claude launch gets, workers included.
const SHIPPED: &str = include_str!("../../../../deploy/driven-session-settings.json");
const WHY: &str = "oversight proof test";

const GUI: &str = "0b3c9a52-6d1e-4f7a-9e1c-2a4f8d7e6b10";
const WORKER: &str = "5e7d1c0a-3b2f-4c8e-8a9d-7f6e5d4c3b21";

type Roles = Arc<Mutex<BTreeMap<String, String>>>;

fn engine_row(id: &str, role: &str) -> Value {
    json!({
        "id": id,
        "name": id,
        "activity": "idle",
        "alive": true,
        "exit_code": Value::Null,
        "role": role,
        "cwd": "/srv",
    })
}

/// An engine that keeps each live session's role and changes it on request.
fn stand_in(roles: &Roles) -> EngineClient {
    let roles = Arc::clone(roles);
    EngineClient::new(
        "http://engine",
        None,
        Some(Arc::new(move |path, _, body, method| {
            let route = path.split("/api/").nth(1).unwrap_or(path);
            let route = route.split('?').next().unwrap_or(route);
            let parts: Vec<&str> = route.split('/').collect();
            let mut roles = roles.lock().unwrap();
            match (method, parts.as_slice()) {
                ("GET", ["sessions"]) => {
                    let rows: Vec<Value> = roles
                        .iter()
                        .map(|(id, role)| engine_row(id, role))
                        .collect();
                    (200, serde_json::to_vec(&rows).unwrap())
                }
                ("GET", ["sessions", id]) => match roles.get(*id) {
                    Some(role) => (200, serde_json::to_vec(&engine_row(id, role)).unwrap()),
                    None => (404, Vec::new()),
                },
                ("POST", ["sessions", id, "role"]) => {
                    let asked: Value = serde_json::from_slice(body).unwrap();
                    let role = asked["role"].as_str().unwrap().to_string();
                    match roles.get_mut(*id) {
                        Some(held) => {
                            *held = role.clone();
                            (200, serde_json::to_vec(&engine_row(id, &role)).unwrap())
                        }
                        None => (404, Vec::new()),
                    }
                }
                _ => (404, Vec::new()),
            }
        })),
    )
}

/// A store with two engine sessions an operator opened from the GUI, both
/// workers until a person says otherwise, and a context for each principal.
struct Estate {
    dir: std::path::PathBuf,
    roles: Roles,
}

impl Estate {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vogt-oversight-proof-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let mut roles = BTreeMap::new();
        roles.insert(GUI.to_string(), "worker".to_string());
        roles.insert(WORKER.to_string(), "worker".to_string());
        Self {
            dir,
            roles: Arc::new(Mutex::new(roles)),
        }
    }

    /// The engine's own credential, which an estate whose stack secret was
    /// first issued by hand binds to a person (WI-1000).
    fn as_engine_bound_to_person(&self) -> Built {
        build_context(
            crate::config::VogtConfig {
                data_dir: self.dir.clone(),
                bootstrap_core_token_actor: "local:ada".to_string(),
                ..crate::config::VogtConfig::default()
            },
            Some(person()),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            None,
            None,
            Some(stand_in(&self.roles)),
            None,
            None,
        )
        .unwrap()
    }

    fn as_(&self, principal: Principal) -> Built {
        build_context(
            crate::config::VogtConfig {
                data_dir: self.dir.clone(),
                ..crate::config::VogtConfig::default()
            },
            Some(principal),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            None,
            None,
            Some(stand_in(&self.roles)),
            None,
            None,
        )
        .unwrap()
    }
}

fn person() -> Principal {
    Principal::new("local:ada", ActorKind::Human, "Ada").unwrap()
}

/// The principal an engine-launched agent session's token resolves to.
fn engine_session(id: &str) -> Principal {
    Principal::new(&format!("agent:engine:{id}"), ActorKind::Agent, "agent").unwrap()
}

fn run(built: &Built, op: &str, params: Value) -> Result<Value, VogtError> {
    crate::registry::default_registry()
        .get(op)
        .unwrap_or_else(|_| panic!("{op} is registered"))
        .run(Some(built), params)
}

fn make_oversight(built: &Built, id: &str) -> Result<Value, VogtError> {
    run(
        built,
        "session.set_role",
        json!({"id": id, "role": "oversight", "reason": WHY}),
    )
}

/// The identity forms the shipped rule accepts from `auth_whoami`, as the
/// prefixes before `<id>`: every `` `agent:…:<id>` `` its proof clause names.
fn accepted_identities() -> Vec<String> {
    let policy: Value = serde_json::from_str(SHIPPED).unwrap();
    let rule = policy["autoMode"]["allow"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .find(|r| r.starts_with("Dev Admin Token Lifecycle:"))
        .expect("the shipped policy carries the dev admin token rule");
    let proof = &rule[..rule
        .find("It covers")
        .expect("the rule says what it covers")];
    proof
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|quoted| quoted.starts_with("agent:"))
        .filter_map(|quoted| quoted.strip_suffix("<id>").map(str::to_string))
        .collect()
}

/// Whether a whoami result and a session_list result, both produced for the
/// same caller, satisfy the rule's proof of the oversight role.
fn proves_oversight(whoami: &Value, listed: &Value) -> bool {
    let identity = whoami["identity_ref"].as_str().unwrap_or_default();
    let Some(id) = accepted_identities()
        .iter()
        .find_map(|prefix| identity.strip_prefix(prefix.as_str()))
        .map(str::to_string)
    else {
        return false;
    };
    listed["sessions"].as_array().is_some_and(|rows| {
        rows.iter()
            .any(|row| row["id"] == json!(id) && row["role"] == "oversight")
    })
}

/// What the caller can show: its own whoami and the session list it reads.
/// The default digest rows leave `role` out, so the proof asks for full rows.
fn evidence(built: &Built) -> (Value, Value) {
    let whoami = run(built, "auth.whoami", json!({})).unwrap();
    let listed = run(built, "session.list", json!({"mode": "full"})).unwrap();
    (whoami, listed)
}

#[test]
fn an_operator_started_oversight_session_can_prove_its_role() {
    let estate = Estate::new("operator");
    let gui = estate.as_(engine_session(GUI));
    // A worker until a person says otherwise.
    let (whoami, listed) = evidence(&gui);
    assert!(!proves_oversight(&whoami, &listed));

    make_oversight(&estate.as_(person()), GUI).unwrap();

    let (whoami, listed) = evidence(&gui);
    assert_eq!(whoami["identity_ref"], format!("agent:engine:{GUI}"));
    let row = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == GUI)
        .expect("the unlinked session is listed under the engine's id");
    assert_eq!(row["linked"], false);
    assert_eq!(row["role"], "oversight");
    assert!(
        proves_oversight(&whoami, &listed),
        "the rule accepts only {:?}, so the GUI oversight session ({}) cannot prove its role",
        accepted_identities(),
        whoami["identity_ref"],
    );
}

#[test]
fn a_worker_cannot_prove_the_oversight_role() {
    let estate = Estate::new("worker");
    let worker = estate.as_(engine_session(WORKER));

    // It cannot nominate itself.
    let refused = make_oversight(&worker, WORKER).unwrap_err();
    assert!(
        matches!(refused, VogtError::RoleRefused(ref m) if m.contains("only a person nominates")),
        "{refused:?}"
    );
    let (whoami, listed) = evidence(&worker);
    assert!(!proves_oversight(&whoami, &listed));

    // Nor borrow an overseer's row: its whoami names itself, not the overseer.
    make_oversight(&estate.as_(person()), GUI).unwrap();
    let (whoami, listed) = evidence(&worker);
    assert_eq!(whoami["identity_ref"], format!("agent:engine:{WORKER}"));
    assert!(!proves_oversight(&whoami, &listed));
}

#[test]
fn the_pods_shared_token_proves_nothing() {
    // A `claude` typed into a plain shell keeps the pod's token: whoami names
    // the pod, not a session, whatever role the shell's session holds.
    let estate = Estate::new("pod");
    make_oversight(&estate.as_(person()), GUI).unwrap();
    let pod = estate.as_(Principal::new("agent:pod:vogt-dev", ActorKind::Agent, "pod").unwrap());
    let (whoami, listed) = evidence(&pod);
    assert!(!proves_oversight(&whoami, &listed));
}

#[test]
fn the_rule_still_accepts_a_session_vogt_started() {
    assert!(accepted_identities().contains(&"agent:session:".to_string()));
}

/// WI-1000: each gate that only a person may pass refuses the engine's own
/// credential even when the stack secret is bound to a person, and still lets
/// a real person through (the gate passes, so the call fails later or works).
#[test]
fn the_engine_credential_bound_to_a_person_fails_every_person_gate() {
    let estate = Estate::new("engine-as-person");
    let engine = estate.as_engine_bound_to_person();
    let ada = estate.as_(person());

    // session.set_role
    let refused = make_oversight(&engine, WORKER).unwrap_err();
    assert!(matches!(refused, VogtError::RoleRefused(_)), "{refused:?}");
    make_oversight(&ada, WORKER).unwrap();

    // session.start with permission_mode=bypass
    let bypass = json!({"reason": WHY, "project": "none", "permission_mode": "bypass"});
    let refused = run(&engine, "session.start", bypass.clone()).unwrap_err();
    assert!(
        matches!(refused, VogtError::BypassRefused(_)),
        "{refused:?}"
    );
    let passed = run(&ada, "session.start", bypass).unwrap_err();
    assert!(!matches!(passed, VogtError::BypassRefused(_)), "{passed:?}");

    // decision.answer
    let answer = json!({"id": "dec_none", "label": "x", "reason": WHY});
    let refused = run(&engine, "decision.answer", answer.clone()).unwrap_err();
    assert!(
        matches!(refused, VogtError::PersonRequired(_)),
        "{refused:?}"
    );
    let passed = run(&ada, "decision.answer", answer).unwrap_err();
    assert!(
        !matches!(passed, VogtError::PersonRequired(_)),
        "{passed:?}"
    );

    // charter.set on the workspace scope
    let charter = json!({"scope": "workspace", "body": "b", "reason": WHY});
    let refused = run(&engine, "charter.set", charter.clone()).unwrap_err();
    assert!(
        matches!(refused, VogtError::PersonRequired(_)),
        "{refused:?}"
    );
    let passed = run(&ada, "charter.set", charter);
    assert!(
        !matches!(passed, Err(VogtError::PersonRequired(_))),
        "{passed:?}"
    );
}

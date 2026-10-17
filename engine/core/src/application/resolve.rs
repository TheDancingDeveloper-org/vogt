//! Turning what callers type into what the store holds.
//! Ports `src/vogt/application/services/_resolve.py`.
//!
//! Callers name things the way humans and agents do — `WI-7`, a project slug,
//! an actor's `identity_ref`. Everything resolves here so that a mistyped
//! reference produces "no work item WI-70" rather than a foreign-key error
//! surfacing from three layers down.
//!
//! `work_item` resolves declared rows only. Every operation that must also
//! accept an upstream subject key resolves through the upstream service, which
//! tries this first and then the observed mirror. The two cannot disagree: a
//! `WI-` ref never contains `:`, so the namespaces are disjoint.

use crate::core::{py_repr, Actor, Initiative, Project, WorkItem};
use crate::errors::VogtError;
use crate::storage::interface::ReadView;

pub fn project(view: &impl ReadView, slug: &str) -> Result<Project, VogtError> {
    match view.project_by_slug(slug)? {
        Some(found) => Ok(found),
        None => Err(VogtError::NotFound(format!(
            "no project with slug {}",
            py_repr(slug)
        ))),
    }
}

pub fn work_item(view: &impl ReadView, reference: &str) -> Result<WorkItem, VogtError> {
    match view.work_item_by_ref(reference)? {
        Some(found) => Ok(found),
        None => Err(VogtError::NotFound(format!(
            "no work item {}",
            py_repr(reference)
        ))),
    }
}

pub fn actor(view: &impl ReadView, identity_ref: &str) -> Result<Actor, VogtError> {
    match view.actor_by_identity(identity_ref)? {
        Some(found) => Ok(found),
        None => Err(VogtError::NotFound(format!(
            "no actor with identity {} — create it with `actor create` first",
            py_repr(identity_ref)
        ))),
    }
}

pub fn initiative(view: &impl ReadView, slug: &str) -> Result<Initiative, VogtError> {
    match view.initiative_by_slug(slug)? {
        Some(found) => Ok(found),
        None => Err(VogtError::NotFound(format!(
            "no initiative with slug {}",
            py_repr(slug)
        ))),
    }
}

pub fn label_exists(view: &impl ReadView, name: &str) -> Result<(), VogtError> {
    if view.label_by_name(name)?.is_none() {
        return Err(VogtError::NotFound(format!(
            "no label named {} — create it with `label create` first",
            py_repr(name)
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{actor, initiative, label_exists, project, work_item};
    use crate::core::{
        ActorKind, Initiative, InitiativeState, Label, LinkState, Moment, Origin, Principal,
        Priority, Project, SequentialIds, StepClock, TrustState, WorkItem, WorkKind,
    };
    use crate::errors::VogtError;
    use crate::storage::interface::{DeclaredStore, WriteTxn};
    use crate::storage::sqlite::declared::SqliteDeclaredStore;

    fn moment() -> Moment {
        Moment::from_unix(1_700_000_000, 0)
    }

    /// A migrated, bootstrapped store holding one project, one work item
    /// (`WI-7`), one initiative and one label. The bootstrapped human is
    /// `local:test-user`.
    fn seeded() -> SqliteDeclaredStore<StepClock, SequentialIds> {
        let dir = std::env::temp_dir().join(format!(
            "vogt-resolve-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = SqliteDeclaredStore::new(
            dir.join("declared.sqlite3"),
            StepClock::new(moment()),
            SequentialIds::new(None).unwrap(),
        );
        store.migrate().unwrap();
        let principal = Principal::new("local:test-user", ActorKind::Human, "Test").unwrap();
        store.bootstrap(&principal).unwrap();

        let mut txn = store.write().unwrap();
        let mut alpha = Project::new("prj_alpha", "alpha", "Alpha", "/srv/alpha", moment());
        alpha.link_state = LinkState::Unlinked;
        txn.insert_project(&alpha).unwrap();
        txn.insert_work_item(&WorkItem {
            id: "wrk_7".to_string(),
            reference: "WI-7".to_string(),
            kind: WorkKind::Feature,
            title: "The thing".to_string(),
            body: String::new(),
            state: "open".to_string(),
            priority: Priority::P2,
            effort: None,
            project_id: Some("prj_alpha".to_string()),
            project_slug: Some("alpha".to_string()),
            initiative_id: None,
            origin: Origin::Created,
            trust_state: TrustState::Unverified,
            assignee_actor_id: None,
            assignee_identity_ref: None,
            labels: Vec::new(),
            relations: Vec::new(),
            superseded_by: None,
            created_at: moment(),
            updated_at: moment(),
        })
        .unwrap();
        txn.insert_initiative(&Initiative {
            id: "ini_1".to_string(),
            slug: "the-push".to_string(),
            title: "The push".to_string(),
            body: String::new(),
            state: InitiativeState::Open,
            weight: 0,
            created_at: moment(),
            updated_at: moment(),
        })
        .unwrap();
        txn.insert_label(&Label {
            id: "lbl_1".to_string(),
            name: "red".to_string(),
            color: None,
            created_at: moment(),
        })
        .unwrap();
        txn.commit().unwrap();
        store
    }

    fn not_found(error: VogtError) -> String {
        match error {
            VogtError::NotFound(message) => message,
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn a_work_item_resolves_by_its_ref() {
        let store = seeded();
        let view = store.read().unwrap();
        let item = work_item(&view, "WI-7").unwrap();
        assert_eq!(item.id, "wrk_7");
        assert_eq!(item.title, "The thing");
    }

    #[test]
    fn a_missing_work_item_is_not_found_and_names_the_ref() {
        let store = seeded();
        let view = store.read().unwrap();
        let message = not_found(work_item(&view, "WI-70").unwrap_err());
        assert_eq!(message, "no work item 'WI-70'");
    }

    #[test]
    fn a_project_resolves_by_its_slug() {
        let store = seeded();
        let view = store.read().unwrap();
        assert_eq!(project(&view, "alpha").unwrap().id, "prj_alpha");
    }

    #[test]
    fn a_missing_project_is_not_found_and_names_the_slug() {
        let store = seeded();
        let view = store.read().unwrap();
        let message = not_found(project(&view, "beta").unwrap_err());
        assert_eq!(message, "no project with slug 'beta'");
    }

    #[test]
    fn an_actor_resolves_by_its_identity_ref() {
        let store = seeded();
        let view = store.read().unwrap();
        assert_eq!(
            actor(&view, "local:test-user").unwrap().identity_ref,
            "local:test-user"
        );
    }

    #[test]
    fn a_missing_actor_is_not_found_and_says_how_to_create_it() {
        let store = seeded();
        let view = store.read().unwrap();
        let message = not_found(actor(&view, "agent:ghost").unwrap_err());
        assert!(
            message.starts_with("no actor with identity 'agent:ghost'"),
            "{message}"
        );
        assert!(message.contains("`actor create`"), "{message}");
    }

    #[test]
    fn an_initiative_resolves_by_its_slug_and_a_missing_one_is_not_found() {
        let store = seeded();
        let view = store.read().unwrap();
        assert_eq!(initiative(&view, "the-push").unwrap().id, "ini_1");
        let message = not_found(initiative(&view, "gone").unwrap_err());
        assert_eq!(message, "no initiative with slug 'gone'");
    }

    #[test]
    fn a_label_that_exists_passes_and_a_missing_one_is_not_found() {
        let store = seeded();
        let view = store.read().unwrap();
        label_exists(&view, "red").unwrap();
        let message = not_found(label_exists(&view, "blue").unwrap_err());
        assert!(message.starts_with("no label named 'blue'"), "{message}");
        assert!(message.contains("`label create`"), "{message}");
    }
}

//! Import: merge an export into the live declared store. Ports
//! `instance_merge.py`.
//!
//! The merge is a plan first. Every entity is decided against the target —
//! created, updated, left as a conflict, skipped, or unchanged — and the plan
//! is what a dry run reports. Applying writes that plan in one audited
//! transaction, so a refusal writes nothing.
//!
//! Identity is never imported. Projects arrive with forge write-back off and
//! trust unverified, and a project's path and link state here are kept.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use serde_json::{json, Value};

use crate::application::context::{AppContext, Built};
use crate::core::{Actor, Comment, IdFactory, Initiative, Label, Moment, Project, WorkItem};
use crate::errors::VogtError;
use crate::merge::decide;
use crate::storage::interface::{DeclaredStore, ProjectUpdate, ReadView, WorkItemUpdate, WriteTxn};

const APPLICABLE_FORMAT: i64 = 2;
const IMPORT_EVENT: &str = "instance.imported";
const BIG: i64 = 100_000;

const ENTITIES: [&str; 7] = [
    "actor",
    "label",
    "project",
    "initiative",
    "work_item",
    "relation",
    "comment",
];

const DECISION_DETAIL: [(&str, &str); 4] = [
    ("unchanged", ""),
    (
        "take_incoming",
        "changed only in the export since the baseline; taken",
    ),
    (
        "keep_target",
        "changed only here since the baseline; this version kept",
    ),
    (
        "conflict",
        "changed on both sides (or no baseline); this version kept",
    ),
];

fn detail_of(decision: crate::merge::MergeDecision) -> String {
    DECISION_DETAIL
        .iter()
        .find(|(name, _)| {
            *name
                == match decision {
                    crate::merge::MergeDecision::Unchanged => "unchanged",
                    crate::merge::MergeDecision::TakeIncoming => "take_incoming",
                    crate::merge::MergeDecision::KeepTarget => "keep_target",
                    crate::merge::MergeDecision::Conflict => "conflict",
                }
        })
        .map(|(_, text)| (*text).to_string())
        .map(|v| v.to_string())
        .unwrap_or_default()
}

/// One row of the plan: what happened to one entity, and why.
#[derive(Clone)]
struct Change {
    entity: String,
    key: String,
    action: String,
    item_ref: Option<String>,
    incoming_ref: Option<String>,
    fields: Vec<String>,
    detail: String,
}

impl Change {
    fn to_json(&self) -> Value {
        json!({
            "entity": self.entity,
            "key": self.key,
            "action": self.action,
            "ref": self.item_ref,
            "incoming_ref": self.incoming_ref,
            "fields": self.fields,
            "detail": self.detail,
        })
    }
}

struct ConflictNote {
    work_item_id: String,
    comment_id: String,
    body: String,
}

/// What an import would do. The lists are what applying writes; `changes` is
/// what the caller reads.
struct Plan {
    actors: Vec<Actor>,
    labels: Vec<Label>,
    projects: Vec<Project>,
    project_updates: Vec<(String, ProjectUpdate)>,
    initiatives: Vec<Initiative>,
    initiative_updates: Vec<Initiative>,
    items: Vec<(WorkItem, usize)>,
    item_updates: Vec<(String, WorkItemUpdate)>,
    relations: Vec<(String, String, String)>,
    comments: Vec<Comment>,
    conflict_notes: Vec<ConflictNote>,
    changes: Vec<Change>,
    unchanged: BTreeMap<String, i64>,
}

impl Plan {
    fn new() -> Self {
        Self {
            actors: Vec::new(),
            labels: Vec::new(),
            projects: Vec::new(),
            project_updates: Vec::new(),
            initiatives: Vec::new(),
            initiative_updates: Vec::new(),
            items: Vec::new(),
            item_updates: Vec::new(),
            relations: Vec::new(),
            comments: Vec::new(),
            conflict_notes: Vec::new(),
            changes: Vec::new(),
            unchanged: BTreeMap::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record(
        &mut self,
        entity: &str,
        key: &str,
        action: &str,
        detail: &str,
        fields: &[String],
        item_ref: Option<String>,
        incoming_ref: Option<String>,
    ) -> usize {
        self.changes.push(Change {
            entity: entity.to_string(),
            key: key.to_string(),
            action: action.to_string(),
            item_ref,
            incoming_ref,
            fields: fields.to_vec(),
            detail: detail.to_string(),
        });
        self.changes.len() - 1
    }

    fn same(&mut self, entity: &str) {
        *self.unchanged.entry(entity.to_string()).or_insert(0) += 1;
    }

    fn conflicts(&self) -> Vec<&Change> {
        self.changes
            .iter()
            .filter(|change| change.action == "conflict")
            .collect()
    }
}

/// The export as entities. Parsed once, then planned against the target.
struct Export {
    format_version: i64,
    instance_id: String,
    exported_at: Option<Moment>,
    projects: Vec<Project>,
    items: Vec<WorkItem>,
    initiatives: Vec<Initiative>,
    labels: Vec<Label>,
    actors: Vec<Actor>,
    comments: Vec<Comment>,
    clone_stamp: Option<Value>,
}

fn parse_export(raw: &Value, source: &Path) -> Result<Export, VogtError> {
    let list = |name: &str| -> Result<Vec<Value>, VogtError> {
        match raw.get(name) {
            Some(Value::Array(values)) => Ok(values.clone()),
            _ => Err(VogtError::InvalidRequest(format!(
                "{source} has no {name} list",
                source = source.display()
            ))),
        }
    };
    let decode = |name: &str, value: &Value| -> Result<Value, VogtError> {
        Ok(value.clone()).and_then(|value| {
            serde_json::from_value::<serde_json::Value>(value.clone()).map_err(|err| {
                VogtError::InvalidRequest(format!(
                    "{source}: a {name} is not readable: {err}",
                    source = source.display()
                ))
            })
        })
    };
    let _ = decode;
    let projects = list("projects")?
        .iter()
        .map(|value| decode_entity("project", value, source))
        .collect::<Result<Vec<Project>, _>>()?;
    let items = list("work_items")?
        .iter()
        .map(|value| decode_entity("work item", value, source))
        .collect::<Result<Vec<WorkItem>, _>>()?;
    let initiatives = list("initiatives")?
        .iter()
        .map(|value| decode_entity("initiative", value, source))
        .collect::<Result<Vec<Initiative>, _>>()?;
    let labels = list("labels")?
        .iter()
        .map(|value| decode_entity("label", value, source))
        .collect::<Result<Vec<Label>, _>>()?;
    let actors = list("actors")?
        .iter()
        .map(|value| decode_entity("actor", value, source))
        .collect::<Result<Vec<Actor>, _>>()?;
    let comments = list("comments")?
        .iter()
        .map(|value| decode_entity("comment", value, source))
        .collect::<Result<Vec<Comment>, _>>()?;
    Ok(Export {
        format_version: raw
            .get("export_format_version")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        instance_id: raw
            .get("instance_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        exported_at: match raw.get("exported_at").and_then(Value::as_str) {
            Some(text) => Some(crate::core::from_iso(text).map_err(VogtError::InvalidRequest)?),
            None => None,
        },
        projects,
        items,
        initiatives,
        labels,
        actors,
        comments,
        clone_stamp: raw.get("clone_stamp").cloned(),
    })
}

fn decode_entity<T: serde::de::DeserializeOwned>(
    name: &str,
    value: &Value,
    source: &Path,
) -> Result<T, VogtError> {
    serde_json::from_value(value.clone()).map_err(|err| {
        VogtError::InvalidRequest(format!(
            "{source}: a {name} is not readable: {err}",
            source = source.display()
        ))
    })
}

/// Plan the merge of `export` into `view`. Writes nothing.
struct Planner<'a, V: ReadView> {
    view: &'a V,
    export: &'a Export,
    base: Option<Moment>,
    now: Moment,
    ids: &'a dyn Fn(&str) -> String,
    items: Vec<WorkItem>,
    comments: Vec<Comment>,
    projects: Vec<Project>,
    initiatives: Vec<Initiative>,
    labels: Vec<Label>,
    actors: Vec<Actor>,
    incoming_initiative_slug: HashMap<String, String>,
    incoming_actor_ref: HashMap<String, String>,
    actor_ids: HashMap<String, String>,
    label_names: HashSet<String>,
    label_ids: HashSet<String>,
    project_ids: HashMap<String, String>,
    initiative_ids: HashMap<String, String>,
    present_items: HashSet<String>,
    target_comments: HashMap<String, HashSet<String>>,
    plan: Plan,
}

impl<'a, V: ReadView> Planner<'a, V> {
    fn new(
        view: &'a V,
        export: &'a Export,
        scope: Option<&str>,
        base: Option<Moment>,
        ids: &'a dyn Fn(&str) -> String,
        now: Moment,
    ) -> Result<Self, VogtError> {
        if let Some(scope) = scope {
            if !export.projects.iter().any(|project| project.slug == scope) {
                return Err(VogtError::NotFound(format!(
                    "the export holds no project with slug {scope:?}"
                )));
            }
        }
        let items: Vec<WorkItem> = export
            .items
            .iter()
            .filter(|item| scope.is_none_or(|slug| item.project_slug.as_deref() == Some(slug)))
            .cloned()
            .collect();
        let item_ids: HashSet<&str> = items.iter().map(|item| item.id.as_str()).collect();
        let comments: Vec<Comment> = export
            .comments
            .iter()
            .filter(|comment| item_ids.contains(comment.work_item_id.as_str()))
            .cloned()
            .collect();
        let mut projects: Vec<Project> = export
            .projects
            .iter()
            .filter(|project| scope.is_none_or(|slug| project.slug == slug))
            .cloned()
            .collect();
        let mut initiatives = export.initiatives.clone();
        let mut labels = export.labels.clone();
        let mut actors = export.actors.clone();
        if scope.is_some() {
            let used_initiatives: HashSet<&str> = items
                .iter()
                .filter_map(|item| item.initiative_id.as_deref())
                .collect();
            let used_labels: HashSet<&str> = items
                .iter()
                .flat_map(|item| item.labels.iter().map(String::as_str))
                .collect();
            let used_actors: HashSet<&str> = items
                .iter()
                .filter_map(|item| item.assignee_identity_ref.as_deref())
                .collect();
            let authors: HashSet<&str> = comments.iter().map(|c| c.actor_id.as_str()).collect();
            initiatives.retain(|i| used_initiatives.contains(i.id.as_str()));
            labels.retain(|label| used_labels.contains(label.name.as_str()));
            actors.retain(|actor| {
                used_actors.contains(actor.identity_ref.as_str())
                    || authors.contains(actor.id.as_str())
            });
        }
        let _ = &mut projects;
        let existing = view.list_labels(BIG, 0)?;
        Ok(Self {
            view,
            export,
            base,
            now,
            ids,
            items,
            comments,
            projects,
            initiatives,
            labels,
            actors,
            incoming_initiative_slug: export
                .initiatives
                .iter()
                .map(|i| (i.id.clone(), i.slug.clone()))
                .collect(),
            incoming_actor_ref: export
                .actors
                .iter()
                .map(|a| (a.id.clone(), a.identity_ref.clone()))
                .collect(),
            actor_ids: HashMap::new(),
            label_names: existing.iter().map(|label| label.name.clone()).collect(),
            label_ids: existing.iter().map(|label| label.id.clone()).collect(),
            project_ids: HashMap::new(),
            initiative_ids: HashMap::new(),
            present_items: HashSet::new(),
            target_comments: HashMap::new(),
            plan: Plan::new(),
        })
    }

    fn decide(
        &self,
        equal: bool,
        target_at: Moment,
        incoming_at: Moment,
    ) -> crate::merge::MergeDecision {
        decide(equal, self.base, target_at, incoming_at)
    }

    fn fresh_id(&self, wanted: &str, prefix: &str, taken: bool) -> String {
        if taken {
            (self.ids)(prefix)
        } else {
            wanted.to_string()
        }
    }

    fn run(mut self) -> Result<Plan, VogtError> {
        self.actors()?;
        self.labels()?;
        self.projects()?;
        self.initiatives()?;
        self.work_items()?;
        self.relations()?;
        self.comments()?;
        Ok(self.plan)
    }

    fn actors(&mut self) -> Result<(), VogtError> {
        for incoming in self.actors.clone() {
            if let Some(existing) = self.view.actor_by_identity(&incoming.identity_ref)? {
                self.actor_ids
                    .insert(incoming.identity_ref.clone(), existing.id);
                self.plan.same("actor");
                continue;
            }
            let taken = self.view.actor_by_id(&incoming.id)?.is_some();
            let actor_id = self.fresh_id(&incoming.id, "act", taken);
            let mut created = incoming.clone();
            created.id = actor_id.clone();
            self.actor_ids
                .insert(incoming.identity_ref.clone(), actor_id);
            self.plan.actors.push(created);
            self.plan.record(
                "actor",
                &incoming.identity_ref,
                "created",
                "",
                &[],
                None,
                None,
            );
        }
        Ok(())
    }

    fn labels(&mut self) -> Result<(), VogtError> {
        for incoming in self.labels.clone() {
            if self.label_names.contains(&incoming.name) {
                self.plan.same("label");
                continue;
            }
            let label_id =
                self.fresh_id(&incoming.id, "lbl", self.label_ids.contains(&incoming.id));
            let mut created = incoming.clone();
            created.id = label_id.clone();
            self.label_names.insert(incoming.name.clone());
            self.label_ids.insert(label_id);
            self.plan.labels.push(created);
            self.plan
                .record("label", &incoming.name, "created", "", &[], None, None);
        }
        Ok(())
    }

    fn projects(&mut self) -> Result<(), VogtError> {
        for incoming in self.projects.clone() {
            let target = self.view.project_by_slug(&incoming.slug)?;
            if target.is_none() {
                self.create_project(&incoming)?;
                continue;
            }
            let target = target.unwrap();
            self.project_ids
                .insert(incoming.slug.clone(), target.id.clone());
            let mut notes = Vec::new();
            if target.root_path != incoming.root_path {
                notes.push(format!(
                    "root_path differs ({incoming_path} in the export); kept {kept}",
                    incoming_path = incoming.root_path,
                    kept = target.root_path
                ));
            }
            let mut diff: BTreeMap<&str, String> = BTreeMap::new();
            for (name, theirs, ours) in [
                (
                    "lifecycle_state",
                    incoming.lifecycle_state.to_string(),
                    target.lifecycle_state.to_string(),
                ),
                (
                    "repo_url",
                    incoming.repo_url.clone().unwrap_or_default(),
                    target.repo_url.clone().unwrap_or_default(),
                ),
                (
                    "current_version",
                    incoming.current_version.clone().unwrap_or_default(),
                    target.current_version.clone().unwrap_or_default(),
                ),
            ] {
                if theirs != ours {
                    diff.insert(name, theirs);
                }
            }
            let mut incoming_exclusions = incoming.exclusions.clone();
            incoming_exclusions.sort();
            let mut target_exclusions = target.exclusions.clone();
            target_exclusions.sort();
            let exclusions_differ = incoming_exclusions != target_exclusions;
            let decision = self.decide(
                diff.is_empty() && !exclusions_differ,
                target.updated_at,
                incoming.updated_at,
            );
            if decision == crate::merge::MergeDecision::Unchanged {
                if notes.is_empty() {
                    self.plan.same("project");
                } else {
                    self.plan.record(
                        "project",
                        &incoming.slug,
                        "skipped",
                        &notes.join("; "),
                        &[],
                        None,
                        None,
                    );
                }
                continue;
            }
            let mut fields: Vec<String> = diff.keys().map(|name| (*name).to_string()).collect();
            if exclusions_differ {
                fields.push("exclusions".to_string());
            }
            fields.sort();
            let action = if decision == crate::merge::MergeDecision::TakeIncoming {
                let mut update = ProjectUpdate::default();
                for (name, value) in &diff {
                    let set = if value.is_empty() {
                        None
                    } else {
                        Some(value.clone())
                    };
                    match *name {
                        "lifecycle_state" => update.lifecycle_state = set,
                        "repo_url" => update.repo_url = set,
                        "current_version" => update.current_version = set,
                        _ => {}
                    }
                    if value.is_empty() {
                        notes.push(format!("{name} cannot be cleared by an import; kept"));
                    }
                }
                if exclusions_differ {
                    update.exclusions = Some(incoming.exclusions.clone());
                }
                let writes = update.lifecycle_state.is_some()
                    || update.repo_url.is_some()
                    || update.current_version.is_some()
                    || update.exclusions.is_some();
                if writes {
                    self.plan.project_updates.push((target.id.clone(), update));
                }
                if writes {
                    "updated"
                } else {
                    "skipped"
                }
            } else if decision == crate::merge::MergeDecision::Conflict {
                "conflict"
            } else {
                "skipped"
            };
            let mut detail = vec![detail_of(decision)];
            detail.extend(notes);
            self.plan.record(
                "project",
                &incoming.slug,
                action,
                &detail.join("; "),
                &fields,
                None,
                None,
            );
        }
        Ok(())
    }

    fn create_project(&mut self, incoming: &Project) -> Result<(), VogtError> {
        let taken = self.view.project_by_id(&incoming.id)?.is_some();
        let project_id = self.fresh_id(&incoming.id, "prj", taken);
        let mut notes = vec![format!("root_path {} taken as-is", incoming.root_path)];
        if incoming.write_back.to_string() != "none" {
            notes.push(format!(
                "write_back {} not imported (none)",
                incoming.write_back
            ));
        }
        if incoming.link_state.to_string() != "unlinked" {
            notes.push(format!(
                "link_state {} not imported (unlinked)",
                incoming.link_state
            ));
        }
        let mut created = incoming.clone();
        created.id = project_id.clone();
        created.write_back = crate::core::WriteBack::Disabled;
        created.link_state = crate::core::LinkState::Unlinked;
        created.compliance_status = crate::core::ComplianceStatus::NotChecked;
        created.compliance_checked_at = None;
        created.trust_state = crate::core::TrustState::Unverified;
        self.project_ids.insert(incoming.slug.clone(), project_id);
        self.plan.projects.push(created);
        self.plan.record(
            "project",
            &incoming.slug,
            "created",
            &notes.join("; "),
            &[],
            None,
            None,
        );
        Ok(())
    }

    fn initiatives(&mut self) -> Result<(), VogtError> {
        for incoming in self.initiatives.clone() {
            let target = self.view.initiative_by_slug(&incoming.slug)?;
            if target.is_none() {
                let taken = self.view.initiative_by_id(&incoming.id)?.is_some();
                let initiative_id = self.fresh_id(&incoming.id, "ini", taken);
                let mut created = incoming.clone();
                created.id = initiative_id.clone();
                self.initiative_ids
                    .insert(incoming.id.clone(), initiative_id);
                self.plan.initiatives.push(created);
                self.plan
                    .record("initiative", &incoming.slug, "created", "", &[], None, None);
                continue;
            }
            let target = target.unwrap();
            self.initiative_ids
                .insert(incoming.id.clone(), target.id.clone());
            let fields: Vec<String> = ["title", "body", "state", "weight"]
                .into_iter()
                .filter(|name| match *name {
                    "title" => incoming.title != target.title,
                    "body" => incoming.body != target.body,
                    "state" => incoming.state != target.state,
                    "weight" => incoming.weight != target.weight,
                    _ => false,
                })
                .map(str::to_string)
                .collect();
            let decision = self.decide(fields.is_empty(), target.updated_at, incoming.updated_at);
            if decision == crate::merge::MergeDecision::Unchanged {
                self.plan.same("initiative");
                continue;
            }
            if decision == crate::merge::MergeDecision::TakeIncoming {
                let mut updated = target.clone();
                updated.title = incoming.title.clone();
                updated.body = incoming.body.clone();
                updated.state = incoming.state;
                updated.weight = incoming.weight;
                updated.updated_at = self.now;
                self.plan.initiative_updates.push(updated);
            }
            self.plan.record(
                "initiative",
                &incoming.slug,
                action_of(decision),
                &detail_of(decision),
                &fields,
                None,
                None,
            );
        }
        Ok(())
    }

    fn work_items(&mut self) -> Result<(), VogtError> {
        for incoming in self.items.clone() {
            self.work_item(&incoming)?;
        }
        Ok(())
    }

    fn work_item(&mut self, incoming: &WorkItem) -> Result<(), VogtError> {
        if incoming.superseded_by.is_some() {
            self.plan.record(
                "work_item",
                &incoming.id,
                "skipped",
                &format!(
                    "retired upstream in the export ({})",
                    incoming.superseded_by.as_deref().unwrap_or("")
                ),
                &[],
                None,
                Some(incoming.reference.clone()),
            );
            return Ok(());
        }
        let target = self.view.work_item_by_id(&incoming.id)?;
        if let Some(target) = &target {
            if target.superseded_by.is_some() {
                self.present_items.insert(target.id.clone());
                self.plan.record(
                    "work_item",
                    &incoming.id,
                    "skipped",
                    &format!(
                        "retired upstream here ({})",
                        target.superseded_by.as_deref().unwrap_or("")
                    ),
                    &[],
                    Some(target.reference.clone()),
                    Some(incoming.reference.clone()),
                );
                return Ok(());
            }
        }
        let project_id = if let Some(slug) = &incoming.project_slug {
            let (project_id, project) = self.project_id(slug)?;
            if project_id.is_none() {
                self.plan.record(
                    "work_item",
                    &incoming.id,
                    "skipped",
                    &format!("its project {slug} is neither here nor in the export"),
                    &[],
                    target.as_ref().map(|item| item.reference.clone()),
                    Some(incoming.reference.clone()),
                );
                if let Some(target) = &target {
                    self.present_items.insert(target.id.clone());
                }
                return Ok(());
            }
            if project
                .as_ref()
                .is_some_and(|project| project.link_state.to_string() == "linked")
            {
                if let Some(target) = &target {
                    self.present_items.insert(target.id.clone());
                }
                self.plan.record(
                    "work_item",
                    &incoming.id,
                    "skipped",
                    &format!("project {slug} is upstream-truth here; its items live on the forge"),
                    &[],
                    target.as_ref().map(|item| item.reference.clone()),
                    Some(incoming.reference.clone()),
                );
                return Ok(());
            }
            project_id
        } else {
            None
        };
        let states = self
            .view
            .workflow_for(&incoming.kind.to_string())?
            .transitions;
        if !states
            .iter()
            .any(|(state, _)| state == &incoming.state.to_string())
        {
            if let Some(target) = &target {
                self.present_items.insert(target.id.clone());
            }
            self.plan.record(
                "work_item",
                &incoming.id,
                "skipped",
                &format!(
                    "state {:?} is not in this instance's {} workflow",
                    incoming.state, incoming.kind
                ),
                &[],
                target.as_ref().map(|item| item.reference.clone()),
                Some(incoming.reference.clone()),
            );
            return Ok(());
        }
        let initiative_id = incoming
            .initiative_id
            .as_ref()
            .and_then(|id| self.initiative_ids.get(id).cloned());
        let assignee_id = self.actor_id(incoming.assignee_identity_ref.as_deref())?;
        let Some(target) = target else {
            for name in &incoming.labels {
                self.ensure_label(name);
            }
            let mut created = incoming.clone();
            created.reference = String::new();
            created.project_id = project_id;
            created.initiative_id = initiative_id;
            created.assignee_actor_id = assignee_id;
            created.relations = Vec::new();
            let index = self.plan.record(
                "work_item",
                &incoming.id,
                "created",
                "a fresh ref is assigned from this instance's counter",
                &[],
                None,
                Some(incoming.reference.clone()),
            );
            self.plan.items.push((created, index));
            self.present_items.insert(incoming.id.clone());
            return Ok(());
        };
        self.present_items.insert(target.id.clone());
        let theirs = self.incoming_view(incoming);
        let ours = self.target_view(&target)?;
        let fields: Vec<String> = theirs
            .keys()
            .filter(|name| theirs.get(*name) != ours.get(*name))
            .map(|name| (*name).to_string())
            .collect();
        let mut decision = self.decide(fields.is_empty(), target.updated_at, incoming.updated_at);
        if decision == crate::merge::MergeDecision::TakeIncoming
            && fields.iter().any(|field| field == "kind")
        {
            decision = crate::merge::MergeDecision::Conflict;
        }
        if decision == crate::merge::MergeDecision::Unchanged {
            self.plan.same("work_item");
            return Ok(());
        }
        let mut detail = detail_of(decision);
        if decision == crate::merge::MergeDecision::TakeIncoming {
            if fields.iter().any(|field| field == "labels") {
                for name in &incoming.labels {
                    self.ensure_label(name);
                }
            }
            self.plan.item_updates.push((
                target.id.clone(),
                item_update(
                    &fields,
                    incoming,
                    &target,
                    project_id,
                    initiative_id,
                    assignee_id,
                ),
            ));
        } else if decision == crate::merge::MergeDecision::Conflict {
            let note = conflict_note(
                &target,
                incoming,
                &fields,
                &theirs,
                &self.export.instance_id,
            );
            if self.comment_ids(&target.id)?.contains(&note.comment_id) {
                detail.push_str("; the incoming version is already recorded as a comment");
            } else {
                self.plan.conflict_notes.push(note);
                detail.push_str("; the incoming version is attached as a comment");
            }
        }
        self.plan.record(
            "work_item",
            &incoming.id,
            action_of(decision),
            &detail,
            &fields,
            Some(target.reference.clone()),
            Some(incoming.reference.clone()),
        );
        Ok(())
    }

    fn incoming_view(&self, item: &WorkItem) -> BTreeMap<&'static str, Value> {
        let mut view: BTreeMap<&'static str, Value> = BTreeMap::new();
        view.insert("kind", json!(item.kind.to_string()));
        view.insert("title", json!(item.title));
        view.insert("body", json!(item.body));
        view.insert("state", json!(item.state));
        view.insert("priority", json!(item.priority.to_string()));
        view.insert("effort", json!(item.effort.map(|v| v.to_string())));
        view.insert("project", json!(item.project_slug));
        view.insert(
            "initiative",
            json!(item
                .initiative_id
                .as_ref()
                .and_then(|id| self.incoming_initiative_slug.get(id).cloned())),
        );
        view.insert("assignee", json!(item.assignee_identity_ref));
        let mut labels = item.labels.clone();
        labels.sort();
        view.insert("labels", json!(labels));
        view
    }

    fn target_view(&self, item: &WorkItem) -> Result<BTreeMap<&'static str, Value>, VogtError> {
        let initiative = match &item.initiative_id {
            Some(id) => self.view.initiative_by_id(id)?.map(|found| found.slug),
            None => None,
        };
        let mut view = BTreeMap::new();
        view.insert("kind", json!(item.kind.to_string()));
        view.insert("title", json!(item.title));
        view.insert("body", json!(item.body));
        view.insert("state", json!(item.state));
        view.insert("priority", json!(item.priority.to_string()));
        view.insert("effort", json!(item.effort.map(|v| v.to_string())));
        view.insert("project", json!(item.project_slug));
        view.insert("initiative", json!(initiative));
        view.insert("assignee", json!(item.assignee_identity_ref));
        let mut labels = item.labels.clone();
        labels.sort();
        view.insert("labels", json!(labels));
        Ok(view)
    }

    fn project_id(&self, slug: &str) -> Result<(Option<String>, Option<Project>), VogtError> {
        if let Some(id) = self.project_ids.get(slug) {
            return Ok((Some(id.clone()), self.view.project_by_slug(slug)?));
        }
        match self.view.project_by_slug(slug)? {
            Some(project) => Ok((Some(project.id.clone()), Some(project))),
            None => Ok((None, None)),
        }
    }

    fn actor_id(&mut self, identity_ref: Option<&str>) -> Result<Option<String>, VogtError> {
        let Some(identity_ref) = identity_ref else {
            return Ok(None);
        };
        if let Some(id) = self.actor_ids.get(identity_ref) {
            return Ok(Some(id.clone()));
        }
        match self.view.actor_by_identity(identity_ref)? {
            Some(actor) => {
                self.actor_ids
                    .insert(identity_ref.to_string(), actor.id.clone());
                Ok(Some(actor.id))
            }
            None => Ok(None),
        }
    }

    fn ensure_label(&mut self, name: &str) {
        if self.label_names.contains(name) {
            return;
        }
        let label_id = (self.ids)("lbl");
        self.plan.labels.push(Label {
            id: label_id.clone(),
            name: name.to_string(),
            color: None,
            created_at: self.now,
        });
        self.label_names.insert(name.to_string());
        self.label_ids.insert(label_id);
        self.plan.record(
            "label",
            name,
            "created",
            "referenced by an item",
            &[],
            None,
            None,
        );
    }

    fn comment_ids(&mut self, work_item_id: &str) -> Result<&HashSet<String>, VogtError> {
        if !self.target_comments.contains_key(work_item_id) {
            let ids = self
                .view
                .comments_for(work_item_id, BIG)?
                .iter()
                .map(|comment| comment.id.clone())
                .collect();
            self.target_comments.insert(work_item_id.to_string(), ids);
        }
        Ok(&self.target_comments[work_item_id])
    }

    fn relations(&mut self) -> Result<(), VogtError> {
        for incoming in self.items.clone() {
            if !self.present_items.contains(&incoming.id) {
                continue;
            }
            let target = self.view.work_item_by_id(&incoming.id)?;
            let existing: HashSet<(String, String)> = target
                .as_ref()
                .map(|item| {
                    item.relations
                        .iter()
                        .map(|relation| (relation.kind.to_string(), relation.related_id.clone()))
                        .collect()
                })
                .unwrap_or_default();
            for relation in &incoming.relations {
                let key = format!(
                    "{} -{}-> {}",
                    incoming.reference, relation.kind, relation.related_ref
                );
                if existing.contains(&(relation.kind.to_string(), relation.related_id.clone())) {
                    self.plan.same("relation");
                    continue;
                }
                if !self.present_items.contains(&relation.related_id)
                    && self.view.work_item_by_id(&relation.related_id)?.is_none()
                {
                    self.plan.record(
                        "relation",
                        &key,
                        "skipped",
                        &format!("{} is not here", relation.related_ref),
                        &[],
                        None,
                        None,
                    );
                    continue;
                }
                self.plan.relations.push((
                    incoming.id.clone(),
                    relation.related_id.clone(),
                    relation.kind.to_string(),
                ));
                self.plan
                    .record("relation", &key, "created", "", &[], None, None);
            }
        }
        Ok(())
    }

    fn comments(&mut self) -> Result<(), VogtError> {
        for incoming in self.comments.clone() {
            if !self.present_items.contains(&incoming.work_item_id) {
                self.plan.record(
                    "comment",
                    &incoming.id,
                    "skipped",
                    "its work item was not imported",
                    &[],
                    None,
                    None,
                );
                continue;
            }
            if self
                .comment_ids(&incoming.work_item_id)?
                .contains(&incoming.id)
            {
                self.plan.same("comment");
                continue;
            }
            let author_ref = self.incoming_actor_ref.get(&incoming.actor_id).cloned();
            let actor_id = self.actor_id(author_ref.as_deref())?;
            let Some(actor_id) = actor_id else {
                self.plan.record(
                    "comment",
                    &incoming.id,
                    "skipped",
                    "its author is neither here nor in the export",
                    &[],
                    None,
                    None,
                );
                continue;
            };
            let mut created = incoming.clone();
            created.actor_id = actor_id;
            self.plan.comments.push(created);
            self.plan
                .record("comment", &incoming.id, "created", "", &[], None, None);
        }
        Ok(())
    }
}

fn action_of(decision: crate::merge::MergeDecision) -> &'static str {
    match decision {
        crate::merge::MergeDecision::TakeIncoming => "updated",
        crate::merge::MergeDecision::Conflict => "conflict",
        _ => "skipped",
    }
}

fn item_update(
    fields: &[String],
    incoming: &WorkItem,
    target: &WorkItem,
    project_id: Option<String>,
    initiative_id: Option<String>,
    assignee_id: Option<String>,
) -> WorkItemUpdate {
    let has = |name: &str| fields.iter().any(|field| field == name);
    let mut update = WorkItemUpdate::default();
    if has("title") {
        update.title = Some(incoming.title.clone());
    }
    if has("body") {
        update.body = Some(incoming.body.clone());
    }
    if has("state") {
        update.state = Some(incoming.state.clone());
    }
    if has("priority") {
        update.priority = Some(incoming.priority.to_string());
    }
    if has("effort") {
        if incoming.effort.is_none() {
            update.clear_effort = true;
        } else {
            update.effort = incoming.effort.map(|v| v.to_string());
        }
    }
    if has("project") {
        if let Some(project_id) = project_id {
            update.project_id = Some(project_id);
        }
    }
    if has("initiative") {
        match initiative_id {
            Some(id) => update.initiative_id = Some(id),
            None => update.clear_initiative = true,
        }
    }
    if has("assignee") {
        match assignee_id {
            Some(id) => update.assignee_actor_id = Some(id),
            None => update.clear_assignee = true,
        }
    }
    if has("labels") {
        let incoming_labels: BTreeSet<&str> = incoming.labels.iter().map(String::as_str).collect();
        let target_labels: BTreeSet<&str> = target.labels.iter().map(String::as_str).collect();
        update.add_labels = incoming_labels
            .difference(&target_labels)
            .map(|name| (*name).to_string())
            .collect();
        update.remove_labels = target_labels
            .difference(&incoming_labels)
            .map(|name| (*name).to_string())
            .collect();
    }
    update
}

/// The incoming version, kept beside the target's as a comment. Its id is
/// derived from the item and the incoming version, so importing the same export
/// twice records the conflict once.
fn conflict_note(
    target: &WorkItem,
    incoming: &WorkItem,
    fields: &[String],
    theirs: &BTreeMap<&str, Value>,
    instance_id: &str,
) -> ConflictNote {
    use sha2::{Digest, Sha256};
    // json.dumps([id, to_iso(updated_at), theirs], sort_keys=True): the map's
    // keys come out sorted, and the separators are ", " and ": ".
    let dumped = crate::decisions::python_json_dumps(
        &json!([
            incoming.id,
            crate::core::to_iso(incoming.updated_at),
            theirs
        ]),
        true,
    );
    let digest = Sha256::digest(dumped.as_bytes());
    let digest = hex_of(&digest)[..26].to_string();
    let mut lines = vec![
        format!(
            "Import conflict: {ref_} changed here and in instance {instance_id} (as {incoming_ref}, \
             updated {updated}). This instance's version was kept. The incoming version of each \
             differing field:",
            ref_ = target.reference,
            incoming_ref = incoming.reference,
            updated = crate::core::to_iso(incoming.updated_at)
        ),
        String::new(),
    ];
    for name in fields {
        let rendered = theirs
            .get(name.as_str())
            .map(|value| crate::decisions::python_json_dumps(value, false))
            .unwrap_or_else(|| "null".to_string());
        lines.push(format!("- {name}: {rendered}"));
    }
    ConflictNote {
        work_item_id: target.id.clone(),
        comment_id: format!("cmt_import_{digest}"),
        body: lines.join("\n"),
    }
}

fn hex_of(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn tallies(plan: &Plan) -> Value {
    // ImportTally fills every counter it was not given, so a bucket that holds
    // one action still reports the other four as zero. The keys stay in the
    // model's field order; a sorted map would put "conflict" first.
    const ACTIONS: [&str; 5] = ["created", "updated", "conflict", "skipped", "unchanged"];
    let mut counts: BTreeMap<&str, [i64; 5]> = BTreeMap::new();
    let add = |bucket: &mut [i64; 5], action: &str| {
        if let Some(index) = ACTIONS.iter().position(|name| *name == action) {
            bucket[index] += 1;
        }
    };
    for change in &plan.changes {
        add(
            counts.entry(change.entity.as_str()).or_default(),
            &change.action,
        );
    }
    for (name, count) in &plan.unchanged {
        counts.entry(name.as_str()).or_default()[4] = *count;
    }
    let mut result = serde_json::Map::new();
    for name in ENTITIES {
        if let Some(bucket) = counts.get(name) {
            let mut ordered = serde_json::Map::new();
            for (index, action) in ACTIONS.iter().enumerate() {
                ordered.insert((*action).to_string(), json!(bucket[index]));
            }
            result.insert(name.to_string(), Value::Object(ordered));
        }
    }
    Value::Object(result)
}

fn total(plan: &Plan, action: &str) -> i64 {
    plan.changes
        .iter()
        .filter(|change| change.action == action)
        .count() as i64
}

/// When the two instances last agreed, and how that is known.
fn baseline<V: ReadView>(export: &Export, instance_id: &str, view: &V) -> (Option<Moment>, String) {
    if export.instance_id == instance_id {
        return match export.exported_at {
            Some(exported_at) => (
                Some(exported_at),
                "an export of this same instance: its exported_at".to_string(),
            ),
            None => (
                None,
                "an export of this instance with no exported_at".to_string(),
            ),
        };
    }
    if let Some(stamp) = &export.clone_stamp {
        if stamp.get("source_instance_id").and_then(Value::as_str) == Some(instance_id) {
            if let Some(moment) = stamp
                .get("backup_taken_at")
                .and_then(Value::as_str)
                .and_then(|taken| crate::core::from_iso(taken).ok())
            {
                return (
                    Some(moment),
                    format!(
                        "instance {} is a clone of this one; the cloned backup's as-of time",
                        export.instance_id
                    ),
                );
            }
        }
    }
    if let Some(stamp) = view.clone_stamp().ok().flatten() {
        if stamp.source_instance_id == export.instance_id {
            return (
                Some(stamp.backup_taken_at),
                format!(
                    "this instance is a clone of {}; the cloned backup's as-of time",
                    export.instance_id
                ),
            );
        }
    }
    (
        None,
        format!(
            "no clone relationship between this instance and {}: no baseline, so every difference \
             is a conflict",
            export.instance_id
        ),
    )
}

/// Everything the result document needs beyond the plan itself.
struct Report<'a> {
    source: &'a Path,
    export: &'a Export,
    scope: Option<&'a str>,
    base: Option<Moment>,
    base_source: &'a str,
    applied: bool,
    detail: &'a str,
}

fn result_of(report: &Report<'_>, plan: &Plan) -> Value {
    json!({
        "source": report.source.display().to_string(),
        "instance_id": report.export.instance_id,
        "export_format_version": report.export.format_version,
        "projects": report.export.projects.len(),
        "work_items": report.export.items.len(),
        "applied": report.applied,
        "detail": report.detail,
        "project": report.scope,
        // The answer renders a moment the way pydantic's mode="json" does, with
        // a Z. The stored event summary keeps to_iso, which is +00:00.
        "base": report.base.map(|moment| moment.to_json()),
        "base_source": report.base_source,
        "created": total(plan, "created"),
        "updated": total(plan, "updated"),
        "conflicted": total(plan, "conflict"),
        "skipped": total(plan, "skipped"),
        "unchanged": plan.unchanged.values().sum::<i64>(),
        "by_entity": tallies(plan),
        "changes": plan.changes.iter().map(Change::to_json).collect::<Vec<_>>(),
    })
}

use crate::application::writes::WriteOutcome;

/// Apply one planned import inside the audited transaction.
///
/// What applying one plan needs, owned so the write closure can move it in.
struct ApplyRequest {
    plan: Plan,
    base: Option<Moment>,
    base_source: String,
    source_text: String,
    instance_id: String,
    scope: Option<String>,
    strict: bool,
    now: Moment,
}

/// A free function, not a closure written at the call site: the closure
/// `audited_write` takes must not capture anything whose type mentions the
/// clock or the id factory, or the compiler demands `'static` for them.
fn apply_import<T: WriteTxn>(
    txn: &mut T,
    actor: &Actor,
    request: ApplyRequest,
) -> Result<WriteOutcome<(Plan, Option<Moment>, String)>, VogtError> {
    let ApplyRequest {
        mut plan,
        base,
        base_source,
        source_text,
        instance_id,
        scope,
        strict,
        now,
    } = request;
    if strict && !plan.conflicts().is_empty() {
        let named = plan
            .conflicts()
            .iter()
            .take(20)
            .map(|change| {
                format!(
                    "{} {}",
                    change.entity,
                    change.item_ref.as_deref().unwrap_or(&change.key)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let more = if plan.conflicts().len() > 20 {
            format!(" and {} more", plan.conflicts().len() - 20)
        } else {
            String::new()
        };
        return Err(VogtError::Conflict(format!(
            "--strict: {} entities changed on both sides ({named}{more}); nothing was imported",
            plan.conflicts().len()
        )));
    }
    apply_plan(txn, &mut plan, actor, now)?;
    let summary = json!({
        "source": source_text,
        "source_instance_id": instance_id,
        "project": scope,
        "base": base.map(crate::core::to_iso),
        "counts": tallies(&plan),
    });
    Ok(WriteOutcome {
        result: (plan, base, base_source),
        entity_kind: "instance".to_string(),
        entity_id: txn.instance_id()?,
        payload: summary.clone(),
        event_kind: IMPORT_EVENT.to_string(),
        summary,
    })
}

/// The closure lives here, in a function generic over the store's write
/// transaction, so it captures nothing and the `'static` bound holds.
fn apply_audited<C, I, D>(
    write: &mut crate::application::writes::WriteContext<'_, C, I, D>,
    reason: &str,
    request: ApplyRequest,
) -> Result<(Plan, Option<Moment>, String), VogtError>
where
    C: crate::core::Clock,
    I: IdFactory,
    D: DeclaredStore,
{
    crate::application::writes::audited_write(write, "import", reason, |txn, actor| {
        apply_import(txn, actor, request)
    })
}

/// Plan the merge, and write it only when asked to.
pub fn merge_export<C: crate::core::Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let source = crate::application::services::lifecycle::expand_user(Path::new(
        params.get("source").and_then(Value::as_str).unwrap_or(""),
    ));
    if !source.is_file() {
        return Err(VogtError::NotFound(format!(
            "no such export: {}",
            source.display()
        )));
    }
    let text = std::fs::read_to_string(&source).map_err(|err| {
        VogtError::InvalidRequest(format!(
            "{source} is not a readable export: {err}",
            source = source.display()
        ))
    })?;
    let raw: Value = serde_json::from_str(&text).map_err(|err| {
        VogtError::InvalidRequest(format!(
            "{source} is not a readable export: {err}",
            source = source.display()
        ))
    })?;
    if !raw.is_object() {
        return Err(VogtError::InvalidRequest(format!(
            "{source} is not a Vogt export",
            source = source.display()
        )));
    }
    let version = match raw.get("export_format_version") {
        // An export written before the field existed is format 1.
        None => 1,
        Some(Value::Number(number)) => number.as_i64().ok_or_else(|| {
            VogtError::InvalidRequest(format!("export_format_version {number} is not a number"))
        })?,
        Some(other) => {
            let shown = match other {
                Value::String(text) => crate::core::py_repr(text),
                Value::Bool(true) => "True".to_string(),
                Value::Bool(false) => "False".to_string(),
                Value::Null => "None".to_string(),
                value => value.to_string(),
            };
            return Err(VogtError::InvalidRequest(format!(
                "export_format_version {shown} is not a number"
            )));
        }
    };
    if version > APPLICABLE_FORMAT {
        return Err(VogtError::InvalidRequest(format!(
            "export format {version} is newer than {APPLICABLE_FORMAT}; run a newer build rather \
             than guessing what it contains"
        )));
    }
    if version < APPLICABLE_FORMAT {
        if params.get("apply").and_then(Value::as_bool) == Some(true) {
            return Err(VogtError::InvalidRequest(format!(
                "{source} is a format-{version} export: it carries no comments or clone stamp, so \
                 it can be reported on but not applied. Export again from the source with this build.",
                source = source.display()
            )));
        }
        return Ok(json!({
            "source": source.display().to_string(),
            "instance_id": raw.get("instance_id").and_then(Value::as_str).unwrap_or(""),
            "export_format_version": version,
            "projects": raw.get("projects").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "work_items": raw.get("work_items").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "applied": false,
            "detail": format!(
                "A format-{version} export is report-only: it predates applying import and carries \
                 no comments or clone stamp. Export again with this build to merge it."
            ),
            // ImportResult fills these even when the export was never planned.
            "project": Value::Null,
            "base": Value::Null,
            "base_source": "",
            "created": 0,
            "updated": 0,
            "conflicted": 0,
            "skipped": 0,
            "unchanged": 0,
            "by_entity": {},
            "changes": [],
        }));
    }
    let export = parse_export(&raw, &source)?;
    let apply = params.get("apply").and_then(Value::as_bool) == Some(true);
    if apply && params.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Err(VogtError::InvalidRequest(format!(
            "this merges {source} (instance {instance}) into the live store. Run it without \
             --apply to read the plan, then pass --confirm.",
            source = source.display(),
            instance = export.instance_id
        )));
    }
    // Owned, not borrowed: the apply closure moves its captures in, and a
    // borrow of `params` would tie it to this frame for longer than the
    // compiler allows a closure passed to `audited_write`.
    let scope_owned = params
        .get("project")
        .and_then(Value::as_str)
        .filter(|slug| !slug.is_empty())
        .map(str::to_string);
    let scope = scope_owned.as_deref();
    let strict = params.get("strict").and_then(Value::as_bool) == Some(true);
    let now = ctx.clock.lock().expect("clock").now();
    // Fresh ids are minted here, before the write opens, so the plan and the
    // write do not both borrow the context. The counter starts past the clock
    // read, which is enough to keep them unique within one import.
    let minted = std::cell::Cell::new(0u64);
    let ids = |prefix: &str| {
        let next = minted.get();
        minted.set(next + 1);
        format!("{prefix}_import_{next}")
    };

    if !apply {
        let view = ctx.declared.read()?;
        let (base, base_source) = baseline(&export, &view.instance_id()?, &view);
        let plan = Planner::new(&view, &export, scope, base, &ids, now)?.run()?;
        let mut detail =
            "Dry run: nothing was written. Pass --apply --confirm to merge.".to_string();
        if strict && !plan.conflicts().is_empty() {
            detail.push_str(&format!(
                " --strict would refuse it: {} conflicts.",
                plan.conflicts().len()
            ));
        }
        return Ok(result_of(
            &Report {
                source: &source,
                export: &export,
                scope,
                base,
                base_source: &base_source,
                applied: false,
                detail: &detail,
            },
            &plan,
        ));
    }

    let reason = params
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let planned = {
        let view = ctx.declared.read()?;
        let (base, base_source) = baseline(&export, &view.instance_id()?, &view);
        let plan = Planner::new(&view, &export, scope, base, &ids, now)?.run()?;
        (plan, base, base_source)
    };
    let (plan, base, base_source) = planned;
    let (declared, _observed, principal, clock, ids_arc, _config) =
        crate::application::services::context_parts(ctx);
    let mut write =
        crate::application::services::write_context(declared, principal, clock, ids_arc);
    let source_text = source.display().to_string();
    let instance_id = export.instance_id.clone();
    let scope_for_write = scope_owned.clone();
    let outcome = apply_audited(
        &mut write,
        &reason,
        ApplyRequest {
            plan,
            base,
            base_source,
            source_text,
            instance_id,
            scope: scope_for_write,
            strict,
            now,
        },
    )?;
    let (plan, base, base_source) = outcome;
    Ok(result_of(
        &Report {
            source: &source,
            export: &export,
            scope,
            base,
            base_source: &base_source,
            applied: true,
            detail: "Applied in one audited write (operation import).",
        },
        &plan,
    ))
}

fn apply_plan<T: WriteTxn>(
    txn: &mut T,
    plan: &mut Plan,
    actor: &Actor,
    now: Moment,
) -> Result<(), VogtError> {
    for actor in &plan.actors {
        txn.insert_actor(actor)?;
    }
    for label in &plan.labels {
        txn.insert_label(label)?;
    }
    for project in &plan.projects {
        txn.insert_project(project)?;
    }
    for (project_id, update) in &plan.project_updates {
        txn.update_project(project_id, update, now)?;
    }
    for initiative in &plan.initiatives {
        txn.insert_initiative(initiative)?;
    }
    for initiative in &plan.initiative_updates {
        txn.update_initiative(initiative)?;
    }
    for (item, index) in &plan.items {
        let work_ref = txn.next_work_ref()?;
        let mut created = item.clone();
        created.reference = work_ref.clone();
        txn.insert_work_item(&created)?;
        plan.changes[*index].item_ref = Some(work_ref);
    }
    for (item_id, update) in &plan.item_updates {
        txn.update_work_item(item_id, update, now)?;
    }
    for (from_id, to_id, kind) in &plan.relations {
        txn.insert_relation(
            from_id,
            to_id,
            kind.parse().unwrap_or(crate::core::RelationKind::RelatesTo),
            now,
        )?;
    }
    for comment in &plan.comments {
        txn.insert_comment(comment)?;
    }
    for note in &plan.conflict_notes {
        txn.insert_comment(&Comment {
            id: note.comment_id.clone(),
            work_item_id: note.work_item_id.clone(),
            actor_id: actor.id.clone(),
            actor_display_name: actor.display_name.clone(),
            body: note.body.clone(),
            created_at: now,
        })?;
    }
    Ok(())
}

pub fn import_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::application::services::dispatch!(ctx, merge_export, &params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::build_context;
    use crate::application::services::lifecycle::export_instance;
    use crate::core::{ActorKind, Clock, Principal, SequentialIds, StepClock};
    use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView};

    fn opened() -> Built {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vogt-import-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let built = build_context(
            crate::config::VogtConfig {
                data_dir: dir,
                ..crate::config::VogtConfig::default()
            },
            Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        match &built {
            Built::StepSequential(ctx) => {
                ctx.declared.migrate().unwrap();
                ctx.declared
                    .bootstrap(
                        &Principal::new("local:test-user", ActorKind::Human, "Test").unwrap(),
                    )
                    .unwrap();
                ctx.observed.migrate().unwrap();
            }
            _other => panic!("expected a step clock"),
        }
        built
    }

    fn ctx(built: &Built) -> &AppContext<StepClock, SequentialIds> {
        match built {
            Built::StepSequential(ctx) => ctx,
            _other => panic!("expected a step clock"),
        }
    }

    #[test]
    fn importing_something_absent_says_so() {
        let built = opened();
        let error = merge_export(
            ctx(&built),
            &json!({"source": "/nope.json", "reason": "why"}),
        )
        .unwrap_err();
        assert!(
            matches!(error, VogtError::NotFound(message) if message.contains("no such export"))
        );
    }

    #[test]
    fn a_dry_run_reports_the_plan_and_writes_nothing() {
        let source = opened();
        let destination = ctx(&source).config.resolved_data_dir().join("export.json");
        export_instance(
            ctx(&source),
            &json!({"destination": destination.display().to_string(), "reason": "why"}),
        )
        .unwrap();
        let target = opened();
        let before = ctx(&target)
            .declared
            .read()
            .unwrap()
            .list_events(0, 50, None)
            .unwrap()
            .len();

        let result = merge_export(
            ctx(&target),
            &json!({"source": destination.display().to_string(), "reason": "why"}),
        )
        .unwrap();

        assert_eq!(result["applied"], json!(false));
        assert!(result["detail"].as_str().unwrap().contains("Dry run"));
        let after = ctx(&target)
            .declared
            .read()
            .unwrap()
            .list_events(0, 50, None)
            .unwrap()
            .len();
        assert_eq!(before, after, "a dry run writes nothing");
    }

    #[test]
    fn apply_needs_confirmation() {
        let source = opened();
        let destination = ctx(&source).config.resolved_data_dir().join("export.json");
        export_instance(
            ctx(&source),
            &json!({"destination": destination.display().to_string(), "reason": "why"}),
        )
        .unwrap();
        let target = opened();
        let error = merge_export(
            ctx(&target),
            &json!({"source": destination.display().to_string(), "apply": true, "reason": "why"}),
        )
        .unwrap_err();
        assert!(
            matches!(error, VogtError::InvalidRequest(message) if message.contains("--confirm"))
        );
    }

    /// A project in an export is what used to be unreadable: `WriteBack` was
    /// deserialized from a borrowed string, which `serde_json::from_value`
    /// cannot lend. This round-trips a real export that holds one.
    #[test]
    fn an_export_holding_a_project_imports_into_a_fresh_instance() {
        let source = opened();
        let now = ctx(&source).clock.lock().unwrap().now();
        let (declared, _observed, principal, clock, ids, _config) =
            crate::application::services::context_parts(ctx(&source));
        let mut write =
            crate::application::services::write_context(declared, principal, clock, ids);
        crate::application::writes::audited_write(
            &mut write,
            "project.create",
            "seed a project",
            |txn, _actor| {
                let project = Project::new("prj_seed", "alpha", "Alpha", "/work/alpha", now);
                txn.insert_project(&project)?;
                Ok(crate::application::writes::WriteOutcome::new(
                    (),
                    "project",
                    &project.id,
                    json!({"slug": "alpha"}),
                    "project.created",
                    json!({"slug": "alpha"}),
                ))
            },
        )
        .unwrap();

        let destination = ctx(&source).config.resolved_data_dir().join("export.json");
        export_instance(
            ctx(&source),
            &json!({"destination": destination.display().to_string(), "reason": "why"}),
        )
        .unwrap();

        let target = opened();
        let result = merge_export(
            ctx(&target),
            &json!({
                "source": destination.display().to_string(),
                "apply": true,
                "confirm": true,
                "reason": "why"
            }),
        )
        .unwrap();
        assert_eq!(result["applied"], json!(true));
        assert!(result["projects"].as_i64().unwrap() >= 1);

        let view = ctx(&target).declared.read().unwrap();
        let imported = view
            .project_by_slug("alpha")
            .unwrap()
            .expect("the project arrived");
        assert_eq!(imported.name, "Alpha");
        assert_eq!(imported.write_back, crate::core::WriteBack::Disabled);

        let events = view.list_events(0, 50, None).unwrap();
        let imported_event = events
            .iter()
            .find(|event| event.kind == "instance.imported")
            .expect("the import was evented");
        let project_counts = &imported_event.summary["counts"]["project"];
        for counter in ["created", "updated", "conflict", "skipped", "unchanged"] {
            assert!(
                project_counts.get(counter).is_some(),
                "{counter} is present"
            );
        }
        let order: Vec<&str> = project_counts
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            order,
            ["created", "updated", "conflict", "skipped", "unchanged"]
        );
    }

    #[test]
    fn reimporting_the_same_instances_export_uses_its_exported_at() {
        let source = opened();
        let destination = ctx(&source).config.resolved_data_dir().join("export.json");
        export_instance(
            ctx(&source),
            &json!({"destination": destination.display().to_string(), "reason": "why"}),
        )
        .unwrap();
        let result = merge_export(
            ctx(&source),
            &json!({"source": destination.display().to_string(), "reason": "why"}),
        )
        .unwrap();
        assert!(result["base"].as_str().unwrap().ends_with('Z'));
        assert_eq!(
            result["base_source"],
            json!("an export of this same instance: its exported_at")
        );
    }

    #[test]
    fn an_export_with_no_format_version_is_format_1() {
        let built = opened();
        let destination = ctx(&built).config.resolved_data_dir().join("old.json");
        std::fs::write(&destination, "{\"instance_id\": \"ins_old\"}").unwrap();
        let result = merge_export(
            ctx(&built),
            &json!({"source": destination.display().to_string(), "reason": "why"}),
        )
        .unwrap();
        assert_eq!(result["export_format_version"], json!(1));
        assert_eq!(result["applied"], json!(false));
        assert!(result["base"].is_null());
        assert_eq!(result["by_entity"], json!({}));
        assert_eq!(result["changes"], json!([]));
    }
}

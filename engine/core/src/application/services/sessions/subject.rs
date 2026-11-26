use super::*;

pub(super) struct Subject {
    pub(super) session_id: String,
    pub(super) project_id: String,
    pub(super) project_slug: String,
    pub(super) work_item_id: Option<String>,
    pub(super) work_item_ref: Option<String>,
    pub(super) cwd: String,
    pub(super) name: String,
    pub(super) brief: ComposedBrief,
    pub(super) is_scratch: bool,
    pub(super) source: Option<vogt_engine_contract::source::SourceDescriptor>,
}

/// The project and work item a session opens on, and the directory it runs in.
/// Every session has a project: a work item without one, and no project named,
/// falls back to the deployment's scratch project.
pub(super) fn subject_of<C, I>(
    ctx: &AppContext<C, I>,
    params: &StartSessionParams,
    session_id: &str,
) -> Result<Subject, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    // A review is of one work item: `review_of` names it, and a different
    // `work_item` or `from_handoff` beside it is a contradiction, refused
    // before anything starts. Naming it is enough: it becomes the work item.
    let mut work_ref = params.work_item.clone();
    if let Some(review) = &params.review_of {
        if let Some(work) = params.work_item.as_ref().filter(|work| *work != review) {
            return Err(VogtError::InvalidRequest(format!(
                "review_of names {review}, but work_item names {work}"
            )));
        }
        if let Some(from) = params.from_handoff.as_ref().filter(|from| *from != review) {
            return Err(VogtError::InvalidRequest(format!(
                "review_of names {review}, but from_handoff names {from}"
            )));
        }
        work_ref = Some(resolve::work_item(&ctx.declared.read()?, review)?.reference);
    }
    // `from_handoff` is sugar for "a fresh session on the item the hand-off
    // belongs to". The brief carries the hand-off because it always does once
    // the item is the subject; an item with no hand-off is refused.
    if let Some(from) = &params.from_handoff {
        if let Some(work) = work_ref.as_ref().filter(|work| *work != from) {
            return Err(VogtError::InvalidRequest(format!(
                "from_handoff names {from}, but work_item names {work}"
            )));
        }
        let view = ctx.declared.read()?;
        let item = resolve::work_item(&view, from)?;
        let comments = view.comments_for(&item.id, 200, true)?;
        if !comments.iter().any(|one| one.kind == "handoff") {
            return Err(VogtError::InvalidRequest(format!(
                "{} has no hand-off comment to start a session from",
                item.reference
            )));
        }
        work_ref = Some(item.reference);
    }
    // Both is a bad request, and it is reported before either is resolved
    // (beyond the review and hand-off checks above) so a missing work item or
    // project does not hide it.
    if work_ref.is_some() && params.project.is_some() {
        return Err(VogtError::InvalidRequest(
            "give at most one of --work-item or --project".to_string(),
        ));
    }
    let work_item = match &work_ref {
        Some(reference) => Some(resolve::work_item(&ctx.declared.read()?, reference)?),
        None => None,
    };
    let named = match &params.project {
        Some(slug) => Some(resolve::project(&ctx.declared.read()?, slug)?),
        None => None,
    };
    if work_item.is_some() && named.is_some() {
        return Err(VogtError::InvalidRequest(
            "give at most one of --work-item or --project".to_string(),
        ));
    }
    if let (Some(item), Some(project)) = (&work_item, &named) {
        if item.project_id.as_deref() != Some(project.id.as_str()) {
            return Err(VogtError::InvalidRequest(format!(
                "work item {} is not in project {}",
                py_repr(&item.reference),
                py_repr(&project.slug)
            )));
        }
    }
    let mut is_scratch = false;
    let project = match named {
        Some(project) => project,
        None => match work_item
            .as_ref()
            .and_then(|item| item.project_id.as_deref())
        {
            Some(project_id) => {
                ctx.declared
                    .read()?
                    .project_by_id(project_id)?
                    .ok_or_else(|| {
                        VogtError::InvalidRequest(format!(
                            "work item {} names a project that does not exist",
                            py_repr(
                                work_item
                                    .as_ref()
                                    .map(|item| item.reference.as_str())
                                    .unwrap_or("")
                            )
                        ))
                    })?
            }
            None => {
                let Some(slug) = ctx.config.session_scratch_project.as_deref() else {
                    return Err(VogtError::InvalidRequest(
                        "a session needs a project or a work item to open in, and no scratch project is configured: set `session_scratch_project` to a registered project slug, or name a project or work_item".to_string(),
                    ));
                };
                is_scratch = true;
                resolve::project(&ctx.declared.read()?, slug)?
            }
        },
    };
    let cwd = if project.root_path.is_empty() {
        return Err(VogtError::InvalidRequest(format!(
            "project {} has no checkout to open a session in",
            py_repr(&project.slug)
        )));
    } else {
        project.root_path.clone()
    };
    let name = params
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .map(str::trim)
        .map(str::to_string)
        .unwrap_or_else(|| match &work_item {
            Some(item) => item.title.clone(),
            None => project.slug.clone(),
        });
    let brief = match &work_item {
        Some(item) => {
            let review = params.review_of.is_some();
            // The score explanation is optional on purpose: a brief that
            // refused to be written because a score could not be computed
            // would make the ranking a precondition for starting work. A
            // review does not rank the item it checks.
            let ranking: Option<WhyResult> = if review {
                None
            } else {
                crate::application::views::why(ctx, &item.reference)
                    .ok()
                    .and_then(|value| serde_json::from_value(value).ok())
            };
            let view = ctx.declared.read()?;
            let comments = view.comments_for(&item.id, 200, true)?;
            let mut related = BTreeMap::new();
            for relation in &item.relations {
                if let Some(found) = view.work_item_by_id(&relation.related_id)? {
                    related.insert(relation.related_id.clone(), found);
                }
            }
            let author = decision_author(ctx, &view);
            let input = WorkBrief {
                item,
                session_id,
                ranking: ranking.as_ref(),
                project: Some(&project),
                comments: &comments,
                related: &related,
                decision_author: &author,
            };
            if review {
                brief::review_brief_for_work_item(&input)
            } else {
                brief::brief_for_work_item(&input)?
            }
        }
        None => brief::brief_for_project(&project.slug, session_id, Some(&project)),
    };
    let (source, used_main) = project_source_with_fallback(&project);
    if used_main {
        // The last resort (WI-4026): no stated, recorded or backfilled branch,
        // so the pod clones `main`. Say so, so a wrong branch is traceable.
        tracing::warn!(
            project = %project.slug,
            "project has no recorded default branch; the pod clones main as a last resort"
        );
    }
    Ok(Subject {
        session_id: session_id.to_string(),
        project_id: project.id,
        project_slug: project.slug,
        work_item_id: work_item.as_ref().map(|item| item.id.clone()),
        work_item_ref: work_item.map(|item| item.reference),
        cwd,
        name,
        brief,
        is_scratch,
        source,
    })
}

/// What a backfill should write for a project whose branch is not recorded
/// (WI-4026): the remote's HEAD when it names one, else `None` (the caller
/// falls back to `main` and says so). A project with a recorded branch, or
/// with no repository, is never backfilled.
fn backfill_target(project: &crate::core::Project, remote_head: Option<String>) -> Option<String> {
    let recorded = project
        .default_branch
        .as_deref()
        .map(str::trim)
        .filter(|branch| !branch.is_empty());
    if recorded.is_some()
        || project
            .repo_url
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
    {
        return None;
    }
    remote_head
        .map(|branch| branch.trim().to_string())
        .filter(|branch| !branch.is_empty())
}

/// Resolve and persist the default branch of a project registered before it
/// was recorded (WI-4026). Reads the remote's HEAD with no clone, writes it
/// through an audited update, and logs the choice. A remote that cannot be read
/// is logged here; the session then runs on the logged `main` last resort.
pub(super) fn backfill_default_branch<C, I>(
    ctx: &AppContext<C, I>,
    params: &StartSessionParams,
) -> Result<(), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let Some(project_slug) = params.project.as_deref() else {
        return Ok(());
    };
    let Some(project) = ctx.declared.read()?.project_by_slug(project_slug)? else {
        return Ok(());
    };
    let recorded = project
        .default_branch
        .as_deref()
        .is_some_and(|branch| !branch.trim().is_empty());
    let Some(url) = project
        .repo_url
        .clone()
        .filter(|url| !url.trim().is_empty())
    else {
        return Ok(());
    };
    if recorded {
        return Ok(());
    }
    let remote_head = crate::adapters::git::remote_default_branch(&url, None, None);
    let Some(branch) = backfill_target(&project, remote_head.clone()) else {
        tracing::warn!(
            project = %project.slug,
            "project default branch is not recorded and the remote's HEAD could not be read; the session falls back to main"
        );
        return Ok(());
    };
    tracing::info!(
        project = %project.slug,
        branch = %branch,
        "backfilled the project's default branch from the remote's HEAD"
    );
    let mut writing = write_of(ctx);
    let project_id = project.id.clone();
    let at = super::super::now_of(&ctx.clock);
    audited_write(
        &mut writing,
        "project.default_branch.backfill",
        "record the branch the remote names as HEAD (WI-4026)",
        move |txn, _actor| {
            txn.update_project(
                &project_id,
                &crate::storage::interface::ProjectUpdate {
                    default_branch: Some(branch.clone()),
                    ..crate::storage::interface::ProjectUpdate::default()
                },
                at,
            )?;
            Ok(WriteOutcome::new(
                (),
                "project",
                &project_id,
                json!({"default_branch": branch}),
                "project.default_branch.backfilled",
                json!({"project": project_id}),
            ))
        },
    )?;
    Ok(())
}

pub(super) fn project_source(
    project: &crate::core::Project,
) -> Option<vogt_engine_contract::source::SourceDescriptor> {
    project_source_with_fallback(project).0
}

/// The project's source descriptor, and whether its branch is the `main`
/// last resort rather than a stated or recorded one (WI-4026). Precedence: an
/// explicit landing source, then the landing `base_branch`, then the branch
/// recorded at import, then `main`.
pub(super) fn project_source_with_fallback(
    project: &crate::core::Project,
) -> (Option<vogt_engine_contract::source::SourceDescriptor>, bool) {
    #[derive(serde::Deserialize)]
    struct Landing {
        #[serde(default)]
        source: Option<vogt_engine_contract::source::SourceDescriptor>,
        #[serde(default)]
        base_branch: String,
        #[serde(default)]
        worker_lands_by: String,
    }
    let landing: Option<Landing> = project
        .agent_policy
        .landing
        .as_ref()
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    if let Some(source) = landing.as_ref().and_then(|landing| landing.source.clone()) {
        return (Some(source), false);
    }
    let Some(url) = project
        .repo_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
    else {
        return (None, false);
    };
    let mode = match landing.as_ref() {
        Some(landing) if landing.worker_lands_by == "open_pr" => {
            vogt_engine_contract::source::LandingMode::OpenPr
        }
        _ => vogt_engine_contract::source::LandingMode::PushBranch,
    };
    let stated = landing
        .as_ref()
        .map(|landing| landing.base_branch.trim().to_string())
        .filter(|branch| !branch.is_empty());
    let recorded = project
        .default_branch
        .as_deref()
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
        .map(str::to_string);
    let (reference, used_main) = match (stated, recorded) {
        (Some(branch), _) | (None, Some(branch)) => (branch, false),
        (None, None) => ("main".to_string(), true),
    };
    (
        Some(vogt_engine_contract::source::SourceDescriptor {
            location: vogt_engine_contract::source::SourceLocation::Git {
                url: url.to_string(),
                repo: url.to_string(),
            },
            reference,
            landing: mode,
        }),
        used_main,
    )
}

use super::*;

/// Write to a session's terminal.
pub fn input<C, I>(
    ctx: &AppContext<C, I>,
    params: &InputParams,
) -> Result<SessionInputResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    let engine = engine_of(ctx)?;
    let text = params.text.clone().unwrap_or_default();
    let keys = params.keys.clone().unwrap_or_default();
    for key in &keys {
        if !INPUT_KEYS.contains(&key.as_str()) {
            return Err(VogtError::InvalidRequest(format!(
                "unknown input key {} (known: {})",
                py_repr(key),
                INPUT_KEYS.join(", ")
            )));
        }
    }
    // Enter by either route is a submit: `keys: ["enter"]` used to report
    // `submitted: false` (WI-918).
    let submitted = params.submit || keys.iter().any(|key| key == "enter");
    if text.is_empty() && keys.is_empty() && !params.submit {
        return Err(VogtError::InvalidRequest(
            "nothing to send: give text, keys, or submit".to_string(),
        ));
    }
    let size = text.len();
    if size > SESSION_INPUT_MAX_BYTES {
        return Err(VogtError::InvalidRequest(format!(
            "text is {size} bytes; the engine accepts at most {SESSION_INPUT_MAX_BYTES} bytes per write"
        )));
    }
    let found = resolve_target(ctx, &params.id)?;
    let engine_id = found.engine_session_id.clone();
    let linked = found.session.is_some();

    // Text, then each named key, then Enter — each its own engine write, so a
    // terminal reading an Esc does not take the bytes after it as an Alt-chord.
    let mut writes_in_order: Vec<(String, bool)> = Vec::new();
    if !text.is_empty() {
        writes_in_order.push((text.clone(), false));
    }
    for key in &keys {
        writes_in_order.push((key_bytes(key), false));
    }
    if params.submit {
        writes_in_order.push((String::new(), true));
    }
    let person = answers_as_person(ctx)?;
    // An oversight session is relayed by its engine id: the engine lets it
    // answer a worker's permission prompt and audits it (WI-2028).
    let overseer = relayed_overseer(ctx, engine, person)?;
    let confirming = submitted && params.confirm;
    let seen = if confirming {
        engine.get_session(&engine_id).ok().flatten()
    } else {
        None
    };
    let mut before = seen.as_ref().map(|seen| seen.activity.clone());
    let stepwise = confirming && params.submit && !text.is_empty() && keys.is_empty();
    let mut woke = false;
    let mut stepwise_verdict = None;
    let sent = if stepwise && seen.as_ref().is_some_and(has_input_box) {
        if seen.as_ref().is_some_and(EngineSession::hibernated) {
            Err(VogtError::Conflict("hibernated".to_string()))
        } else {
            type_then_submit(
                engine,
                &engine_id,
                &params.id,
                &text,
                before.as_deref(),
                person,
            )
            .map(|verdict| stepwise_verdict = Some(verdict))
        }
    } else {
        send_all(
            engine,
            &engine_id,
            &params.id,
            &writes_in_order,
            person,
            overseer.as_deref(),
        )
    };
    if let Err(refused) = sent {
        // The engine refuses input to a hibernated session. Typing into one is
        // asking for it back: wake it, wait until it is at its prompt, then
        // type. Anything else the engine refused stays refused.
        let VogtError::Conflict(_) = refused else {
            return Err(refused);
        };
        let current = engine.get_session(&engine_id)?;
        if current.as_ref().is_none_or(|live| !live.hibernated()) {
            return Err(refused);
        }
        wake_resolved(
            ctx,
            engine,
            &found,
            &format!("woken to deliver input: {reason}"),
            false,
        )?;
        woke = true;
        let waited = engine.wait_session(
            &engine_id,
            "ready",
            Duration::from_secs(params.wake_timeout_s.max(0) as u64),
        )?;
        let from_relay = overseer.is_some() || person;
        let ready = waited
            .as_ref()
            .is_some_and(|waited| at_prompt_for_input(waited, from_relay));
        if !ready {
            let outcome = waited
                .as_ref()
                .map(|waited| waited.outcome.clone())
                .unwrap_or_else(|| "no answer".to_string());
            return Err(VogtError::Conflict(format!(
                "woke session {}, but it was not ready for input within {}s ({outcome}); nothing was typed. {}",
                py_repr(&params.id),
                params.wake_timeout_s,
                not_ready_next_step(&outcome)
            )));
        }
        // Freshly woken and at its prompt: nothing was running.
        before = Some("waiting-for-input".to_string());
        if stepwise && current.as_ref().is_some_and(has_input_box) {
            stepwise_verdict = Some(type_then_submit(
                engine,
                &engine_id,
                &params.id,
                &text,
                before.as_deref(),
                person,
            )?);
        } else {
            send_all(
                engine,
                &engine_id,
                &params.id,
                &writes_in_order,
                person,
                overseer.as_deref(),
            )?;
        }
    }
    let verdict = if let Some(verdict) = stepwise_verdict {
        verdict
    } else if confirming {
        confirm_delivery(
            engine,
            &engine_id,
            before.as_deref(),
            (!text.is_empty()).then_some(text.as_str()),
        )
    } else {
        delivery::judge(submitted, None, &[], None)
    };
    let entity = found
        .session
        .as_ref()
        .map(|session| session.id.clone())
        .unwrap_or_else(|| engine_id.clone());
    let detail = json!({
        "engine_session_id": engine_id,
        "linked": linked,
        "bytes": size,
        "keys": keys,
        "submit": params.submit,
        "submitted": submitted,
        "delivery": verdict.delivery.as_str(),
        "woke": woke,
        "person": person,
        "oversight": overseer.is_some(),
    });
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        SESSION_INPUT,
        &reason,
        "session",
        &entity,
        &detail,
        "session.input",
        Some(&detail),
    )?;
    let digest = input_digest(ctx, engine, &found, submitted, &reason);
    Ok(SessionInputResult {
        id: params.id.clone(),
        engine_session_id: engine_id,
        linked,
        bytes: size as i64,
        keys,
        submitted,
        delivery: verdict.delivery.as_str().to_string(),
        delivery_evidence: verdict.evidence,
        woke,
        digest,
    })
}

/// Audit a write the core itself typed into a session's terminal: a CI
/// conclusion notice, the context-rotation ask. These are system actions, not
/// a caller's `session.input`, but they put text on a PTY all the same, so each
/// leaves the same `session.input` row: the session (the `ses_` id when linked,
/// otherwise the engine UUID), the actor, the byte count and the key names
/// (none here), never the text. `source` names the system path.
pub(crate) fn audit_system_input<C, I>(
    ctx: &AppContext<C, I>,
    engine_id: &str,
    source: &str,
    reason: &str,
    bytes: usize,
    submitted: bool,
) -> Result<(), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let linked = ctx.declared.read()?.session_by_engine_id(engine_id)?;
    let entity = linked
        .as_ref()
        .map(|session| session.id.clone())
        .unwrap_or_else(|| engine_id.to_string());
    let detail = json!({
        "engine_session_id": engine_id,
        "linked": linked.is_some(),
        "bytes": bytes,
        "keys": Vec::<String>::new(),
        "submit": submitted,
        "submitted": submitted,
        "person": false,
        "oversight": false,
        "system": true,
        "source": source,
    });
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        SESSION_INPUT,
        reason,
        "session",
        &entity,
        &detail,
        "session.input",
        Some(&detail),
    )
}

fn bare_acknowledgement(text: &str, keys: &[String], submit: bool) -> bool {
    text.trim().is_empty() && (submit || keys.iter().any(|key| key == "enter"))
}

fn key_bytes(key: &str) -> String {
    match key {
        "enter" => "\r".to_string(),
        "esc" => "\u{1b}".to_string(),
        "tab" => "\t".to_string(),
        "backspace" => "\u{7f}".to_string(),
        "ctrl-c" => "\u{3}".to_string(),
        "ctrl-d" => "\u{4}".to_string(),
        "up" => "\u{1b}[A".to_string(),
        "down" => "\u{1b}[B".to_string(),
        "right" => "\u{1b}[C".to_string(),
        "left" => "\u{1b}[D".to_string(),
        other => other.to_string(),
    }
}

fn input_bytes(text: &str, keys: &[String], submit: bool) -> i64 {
    let keys: usize = keys.iter().map(|key| key_bytes(key).len()).sum();
    (text.len() + keys + usize::from(submit)) as i64
}

fn confirm_delivery(
    engine: &EngineClient,
    engine_id: &str,
    before: Option<&str>,
    pending: Option<&str>,
) -> delivery::Verdict {
    let mut seen = Vec::new();
    for _ in 0..CONFIRM_READS {
        std::thread::sleep(CONFIRM_INTERVAL);
        let Ok(Some(screen)) = engine.session_screen(engine_id, 0) else {
            break;
        };
        seen.push(delivery::Observation {
            activity: screen.activity.clone(),
            lines: screen.lines.clone(),
        });
        if matches!(
            screen.activity.as_deref(),
            Some("running") | Some("awaiting-approval")
        ) {
            break;
        }
    }
    delivery::judge(true, before, &seen, pending)
}

fn has_input_box(session: &EngineSession) -> bool {
    matches!(
        session.conversation_agent.as_deref(),
        Some("claude" | "codex")
    )
}

fn read_screen(engine: &EngineClient, engine_id: &str) -> Option<delivery::Observation> {
    let screen = engine.session_screen(engine_id, 0).ok()??;
    Some(delivery::Observation {
        activity: screen.activity,
        lines: screen.lines,
    })
}

pub(super) fn type_then_submit(
    engine: &EngineClient,
    engine_id: &str,
    named: &str,
    text: &str,
    before: Option<&str>,
    person: bool,
) -> Result<delivery::Verdict, VogtError> {
    let mut boxed = read_screen(engine, engine_id)
        .is_some_and(|seen| delivery::composer_text(&seen.lines).is_some());
    if !boxed && !matches!(before, Some("running" | "awaiting-approval")) {
        let _ = engine.wait_session(engine_id, "ready", READY_GATE);
        boxed = read_screen(engine, engine_id)
            .is_some_and(|seen| delivery::composer_text(&seen.lines).is_some());
    }
    send_all(
        engine,
        engine_id,
        named,
        &[(text.to_string(), false)],
        person,
        None,
    )?;
    if boxed {
        // WI-4459: the Enter goes out only once the text is in the composer.
        // A text that never shows is reported unsent, not submitted.
        let mut shown = false;
        for _ in 0..TYPED_READS {
            if let Some(seen) = read_screen(engine, engine_id) {
                if delivery::composer_holds(&seen.lines, text) {
                    shown = true;
                    break;
                }
            }
            std::thread::sleep(CONFIRM_INTERVAL);
        }
        if !shown {
            return Ok(delivery::Verdict {
                delivery: delivery::Delivery::Unconfirmed,
                evidence: "the text never appeared in the input box, so no Enter was sent"
                    .to_string(),
            });
        }
    }
    send_all(
        engine,
        engine_id,
        named,
        &[(String::new(), true)],
        person,
        None,
    )?;
    submit_until_taken(engine, engine_id, named, before, text, person)
}

fn submit_until_taken(
    engine: &EngineClient,
    engine_id: &str,
    named: &str,
    before: Option<&str>,
    text: &str,
    person: bool,
) -> Result<delivery::Verdict, VogtError> {
    let mut verdict = settled(
        engine,
        engine_id,
        text,
        confirm_delivery(engine, engine_id, before, Some(text)),
    );
    for _ in 0..MAX_ENTERS - 1 {
        if !matches!(
            verdict.delivery,
            delivery::Delivery::Unconfirmed | delivery::Delivery::Stuck
        ) {
            break;
        }
        if engine.wait_session(engine_id, "ready", SETTLE).is_err() {
            break;
        }
        let Some(seen) = read_screen(engine, engine_id) else {
            break;
        };
        if !delivery::safe_to_resubmit(&seen, text) {
            if delivery::composer_text(&seen.lines).is_some() {
                verdict = delivery::Verdict {
                    delivery: delivery::Delivery::Delivered,
                    evidence: "the input box cleared after the Enter".to_string(),
                };
            }
            break;
        }
        send_all(
            engine,
            engine_id,
            named,
            &[(String::new(), true)],
            person,
            None,
        )?;
        verdict = settled(
            engine,
            engine_id,
            text,
            confirm_delivery(engine, engine_id, before, Some(text)),
        );
    }
    Ok(verdict)
}

fn settled(
    engine: &EngineClient,
    engine_id: &str,
    text: &str,
    verdict: delivery::Verdict,
) -> delivery::Verdict {
    if verdict.delivery != delivery::Delivery::Delivered {
        return verdict;
    }
    std::thread::sleep(CONFIRM_INTERVAL);
    let Some(seen) = read_screen(engine, engine_id) else {
        return verdict;
    };
    if seen.activity.as_deref() == Some("running") {
        return verdict;
    }
    delivery::judge(true, None, &[seen], Some(text))
}

/// The engine id to relay as `overseer` when the caller is a session the
/// engine marks `oversight` (WI-2028), so the engine lets it answer a worker's
/// permission prompt. `None` for a person, who needs no relay, and for every
/// other caller, which the engine still refuses with `PersonRequired`.
pub(crate) fn relayed_overseer<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    person: bool,
) -> Result<Option<String>, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    if person {
        return Ok(None);
    }
    let Some(caller) = crate::application::session_commit::caller_engine_session_id(ctx)? else {
        return Ok(None);
    };
    Ok(engine
        .get_session(&caller)?
        .filter(|found| found.role == "oversight")
        .map(|_| caller))
}

/// One engine write per chunk. `Ok(false)` is the engine saying it has no live
/// session by that id — said with the name the caller used.
fn send_all(
    engine: &EngineClient,
    engine_id: &str,
    named: &str,
    chunks: &[(String, bool)],
    person: bool,
    overseer: Option<&str>,
) -> Result<(), VogtError> {
    for (text, submit) in chunks {
        let sent = engine.send_input(engine_id, text, *submit, person, overseer)?;
        if !sent {
            return Err(VogtError::NotFound(format!(
                "the engine has no live session {}",
                py_repr(named)
            )));
        }
    }
    Ok(())
}

/// Whether a wait that ended for a woken session leaves it at its prompt, able
/// to take input (WI-2232). A reported block is a waiting state, not a busy
/// one: a session that reported itself blocked and sits idle at its prompt is
/// ready for the overseer or person who is answering that block. A permission
/// dialog is never ready here; that answer belongs to a person or an overseer's
/// `session_answer`.
pub(super) fn at_prompt_for_input(waited: &EngineWait, from_relay: bool) -> bool {
    if waited.outcome == "ready" {
        return true;
    }
    waited.outcome == "blocked"
        && from_relay
        && waited.screen.ready == Some(true)
        && matches!(
            waited.screen.activity.as_deref(),
            Some("idle" | "waiting-for-input")
        )
}

/// What the caller should do next when a woken session is not ready for input,
/// by why the wait ended (WI-2232).
pub(super) fn not_ready_next_step(outcome: &str) -> &'static str {
    match outcome {
        "blocked" => {
            "It reported itself blocked and is not at an input prompt. Read session_screen, and wait with session_wait (until ready) for it to reach its prompt before sending input again; do not repeat this call until it has."
        }
        "awaiting-approval" => {
            "It is showing a permission prompt. Answer it with session_answer, then send any steering text with session_input once the prompt is gone."
        }
        _ => "Read session_screen — it may be showing a dialog — then retry",
    }
}

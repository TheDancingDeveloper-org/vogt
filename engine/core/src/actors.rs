//! Who caused an Inbox entry. Ports `src/vogt/core/actors.py`.
//!
//! Pure classification over facts a collector already stored. A bot is never
//! external. Unknown is reported as unknown, never guessed.

#![allow(dead_code)]

pub const DEFAULT_BOT_LOGINS: [&str; 4] =
    ["dependabot", "renovate", "renovate-bot", "github-actions"];

const MEMBER_ASSOCIATIONS: [&str; 2] = ["MEMBER", "OWNER"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    Human,
    Bot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorRelation {
    OrgMember,
    External,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorFacts {
    pub login: Option<String>,
    pub user_type: Option<String>,
    pub association: Option<String>,
    /// None when the membership list could not be read.
    pub org_member: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorClass {
    pub login: Option<String>,
    pub kind: Option<ActorKind>,
    pub relation: ActorRelation,
}

/// Every system-originated entry is attributed to the instance itself.
pub fn system_actor() -> ActorClass {
    ActorClass {
        login: None,
        kind: Some(ActorKind::Bot),
        relation: ActorRelation::OrgMember,
    }
}

/// Lower-case, `[bot]`-stripped, blank-free — the comparison form.
pub fn normalise_bot_logins(logins: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = logins
        .iter()
        .filter(|login| !login.trim().is_empty())
        .map(|login| base_login(login))
        .collect();
    out.sort();
    out.dedup();
    out
}

pub fn is_bot(login: Option<&str>, user_type: Option<&str>, bots: &[String]) -> bool {
    if user_type.is_some_and(|kind| kind.eq_ignore_ascii_case("bot")) {
        return true;
    }
    let Some(login) = login.filter(|login| !login.is_empty()) else {
        return false;
    };
    login.to_lowercase().ends_with("[bot]") || bots.iter().any(|bot| bot == &base_login(login))
}

pub fn classify(facts: &ActorFacts, bots: &[String]) -> ActorClass {
    if facts.login.is_none() && facts.user_type.is_none() {
        return ActorClass {
            login: None,
            kind: None,
            relation: ActorRelation::Unknown,
        };
    }
    let kind = if is_bot(facts.login.as_deref(), facts.user_type.as_deref(), bots) {
        ActorKind::Bot
    } else {
        ActorKind::Human
    };
    ActorClass {
        login: facts.login.clone(),
        kind: Some(kind),
        relation: relation(facts),
    }
}

/// Whether an entry's actor passes the `inbox.list` filter. `external` and
/// `org` are about people: a bot passes neither, and neither does an unknown.
pub fn matches(actor: &ActorClass, wanted: &str) -> bool {
    match wanted {
        "any" => true,
        "bot" => actor.kind == Some(ActorKind::Bot),
        "external" => {
            actor.kind == Some(ActorKind::Human) && actor.relation == ActorRelation::External
        }
        "org" => actor.kind == Some(ActorKind::Human) && actor.relation == ActorRelation::OrgMember,
        _ => false,
    }
}

fn relation(facts: &ActorFacts) -> ActorRelation {
    let association = facts.association.as_deref().unwrap_or("").to_uppercase();
    if facts.org_member == Some(true) || MEMBER_ASSOCIATIONS.contains(&association.as_str()) {
        return ActorRelation::OrgMember;
    }
    if facts.org_member == Some(false) || !association.is_empty() {
        return ActorRelation::External;
    }
    ActorRelation::Unknown
}

fn base_login(login: &str) -> String {
    login
        .trim()
        .to_lowercase()
        .trim_end_matches("[bot]")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(
        login: &str,
        user_type: &str,
        association: &str,
        org_member: Option<bool>,
    ) -> ActorFacts {
        ActorFacts {
            login: (!login.is_empty()).then(|| login.to_string()),
            user_type: (!user_type.is_empty()).then(|| user_type.to_string()),
            association: (!association.is_empty()).then(|| association.to_string()),
            org_member,
        }
    }

    #[test]
    fn the_rules_from_the_module_doc() {
        let bots = normalise_bot_logins(&DEFAULT_BOT_LOGINS);
        // A bot is never external, even with a collaborator association.
        let bot = classify(
            &facts("dependabot[bot]", "Bot", "CONTRIBUTOR", Some(false)),
            &bots,
        );
        assert_eq!(bot.kind, Some(ActorKind::Bot));
        assert_eq!(bot.relation, ActorRelation::External);
        // The configured list catches a bot the forge did not mark.
        assert!(is_bot(Some("Renovate"), None, &bots));
        // A member, by list or by association.
        assert_eq!(
            classify(&facts("ada", "User", "", Some(true)), &bots).relation,
            ActorRelation::OrgMember
        );
        assert_eq!(
            classify(&facts("ada", "User", "OWNER", None), &bots).relation,
            ActorRelation::OrgMember
        );
        // A human the list does not contain is external, and so is any other association.
        assert_eq!(
            classify(&facts("grace", "User", "", Some(false)), &bots).relation,
            ActorRelation::External
        );
        assert_eq!(
            classify(&facts("grace", "User", "COLLABORATOR", None), &bots).relation,
            ActorRelation::External
        );
        // Nothing to judge by is unknown, never guessed.
        assert_eq!(
            classify(&facts("", "", "", None), &bots).relation,
            ActorRelation::Unknown
        );
        assert_eq!(
            classify(&facts("grace", "User", "", None), &bots).relation,
            ActorRelation::Unknown
        );
    }

    #[test]
    fn the_filter_is_about_people() {
        let bots = normalise_bot_logins(&DEFAULT_BOT_LOGINS);
        let bot = classify(&facts("renovate[bot]", "Bot", "MEMBER", Some(true)), &bots);
        assert!(matches(&bot, "bot"));
        assert!(!matches(&bot, "org"));
        assert!(!matches(&bot, "external"));
        let member = classify(&facts("ada", "User", "MEMBER", None), &bots);
        assert!(matches(&member, "org"));
        assert!(!matches(&member, "external"));
        assert!(matches(&system_actor(), "bot"));
    }
}

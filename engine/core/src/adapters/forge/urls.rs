//! Repository-URL parsing shared by the forge providers.
//!
//! The host is what `urlsplit` calls the netloc, cut at the first of `/`, `?`
//! or `#`. Userinfo and a port are stripped afterwards, so a `?` or `#` that
//! arrives *before* the path can never be read as part of the userinfo — that
//! is what made `https://evil.example?@github.com/acme/widget` look like a
//! GitHub repository.

/// `(host, path, has_query)` from a repository URL, or `None` with no host.
///
/// `host` is lowercased with userinfo and port removed. `path` has its query
/// and fragment removed; `has_query` says whether there was one, because the
/// two forges disagree about whether a query disqualifies a URL. The `git@host:`
/// scp form is recognised only for an exact `git@` prefix, and its colon stands
/// in for the first slash.
pub fn split_repo_url(candidate: &str) -> Option<(String, &str, bool)> {
    let (rest, scp) = match candidate.strip_prefix("git@") {
        Some(rest) => (rest, true),
        None => match candidate.find("://") {
            Some(at) => (&candidate[at + 3..], false),
            None => (candidate, false),
        },
    };
    // The netloc ends at the first of `/`, `?` or `#`, exactly as urlsplit reads
    // it. In the scp form the colon before the path is a boundary too.
    let mut boundary = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    if scp {
        boundary = boundary.min(rest.find(':').unwrap_or(boundary));
    }
    let raw_host = &rest[..boundary];
    let mut after = &rest[boundary..];
    if scp && after.starts_with(':') {
        // The colon only marked the boundary; the path begins after it.
        after = &after[1..];
    }
    if raw_host.is_empty() {
        return None;
    }
    let has_query = after.contains(['?', '#']);
    let path = match after.find(['?', '#']) {
        Some(at) => &after[..at],
        None => after,
    };
    let path = path.strip_prefix('/').unwrap_or(path);
    let host = raw_host.rsplit('@').next().unwrap_or(raw_host);
    let host = host.split_once(':').map_or(host, |(name, _)| name);
    if host.is_empty() {
        return None;
    }
    Some((host.to_ascii_lowercase(), path, has_query))
}

//! How the app reaches Vogt.
//!
//! The UI never waits on the network. It asks a [`Backend`] for something and
//! reads the answers back as [`Reply`] values from [`Backend::poll`] on a later
//! frame. [`HttpBackend`] talks to a real engine on desktop and in the browser;
//! [`FixtureBackend`] answers from memory so tests and demos run offline.

mod fixture;
mod http;

pub use fixture::{FixtureBackend, session};
pub use http::HttpBackend;

use crate::api::{ApiError, Auth, LoginResult, ServerEvent, Session, WorkDetail};

/// An answer from the backend.
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    /// The login for `base` succeeded.
    LoggedIn {
        /// The base that was logged in to.
        base: String,
        /// The bearer.
        token: String,
        /// Who signed in.
        actor: String,
    },
    /// The login failed.
    LoginFailed(ApiError),
    /// The session list.
    Sessions(Result<Vec<Session>, ApiError>),
    /// A work item, by the reference it was asked for under.
    WorkItem(String, Result<WorkDetail, ApiError>),
    /// The event stream with this generation opened.
    StreamOpened(u64),
    /// An event arrived on stream `generation`.
    StreamEvent(u64, ServerEvent),
    /// The stream with this generation ended or failed to open.
    StreamClosed(u64, ApiError),
}

/// A source of Vogt data. Every call returns at once; results come back
/// through [`Backend::poll`].
pub trait Backend {
    /// Exchange a username and password for a bearer.
    fn login(&mut self, base: &str, username: &str, password: &str);
    /// Read the session list.
    fn fetch_sessions(&mut self, auth: &Auth);
    /// Read one work item, with its comments.
    fn fetch_work_item(&mut self, auth: &Auth, reference: &str);
    /// Open the event stream as `generation`. Opening a new generation closes
    /// the previous one; replies tagged with an older generation are stale.
    fn open_events(&mut self, auth: &Auth, generation: u64);
    /// Close any open event stream.
    fn close_events(&mut self);
    /// Take the replies that arrived since the last call.
    fn poll(&mut self) -> Vec<Reply>;
}

/// Turn a login response into a [`Reply`].
pub(crate) fn login_reply(base: &str, result: Result<LoginResult, ApiError>) -> Reply {
    match result {
        Ok(login) => Reply::LoggedIn {
            base: base.to_owned(),
            token: login.secret,
            actor: if login.actor.display_name.is_empty() { login.actor.id } else { login.actor.display_name },
        },
        Err(error) => Reply::LoginFailed(error),
    }
}

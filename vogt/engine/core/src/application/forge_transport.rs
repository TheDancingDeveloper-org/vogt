//! The forge-network stand-in on [`AppContext`].
//!
//! Lives apart from `context` only so the addition is its own file. The
//! credentialed forge resolvers take an optional [`github::Transport`], and a
//! service body reads one off the context with [`AppContext::forge_transport`].
//!
//! The stored form is `Arc<dyn Fn + Send + Sync>`: [`github::Transport`] is a
//! boxed closure and not `Clone`, while `with_stores_at` rebuilds a context
//! from a borrow and one request spends the stand-in on more than one
//! provider. The accessor hands back the plain boxed form the resolvers take.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::adapters::github::Transport as ForgeTransport;

use super::context::AppContext;

/// The stand-in shared across a rebuilt context. `Send + Sync` live here and
/// nowhere else: [`github::Transport`] is the boxed form the resolvers take,
/// and [`AppContext::forge_transport`] hands exactly that back.
pub(crate) type SharedForgeTransport = Arc<
    dyn Fn(&str, &BTreeMap<String, String>, &[u8], &str) -> (u16, Vec<u8>, BTreeMap<String, String>)
        + Send
        + Sync,
>;

impl<C, I> AppContext<C, I> {
    /// The stand-in the forge resolvers take, or `None` when they talk HTTP.
    ///
    /// Each call yields a fresh boxed closure over the same stand-in, so one
    /// request can build several providers from it. A service passes it straight
    /// through: `provider_for_token(host, token, Some(&ctx.config),
    /// ctx.forge_transport())`.
    pub fn forge_transport(&self) -> Option<ForgeTransport> {
        self.forge_transport.as_ref().map(|transport| {
            let transport = Arc::clone(transport);
            Box::new(
                move |url: &str, headers: &BTreeMap<String, String>, body: &[u8], method: &str| {
                    transport(url, headers, body, method)
                },
            ) as ForgeTransport
        })
    }

    /// Install the stand-in a test built itself. The constructors leave it
    /// `None`, which is the deployed shape: the HTTP and MCP routes build
    /// through [`super::context::context_on`] and talk HTTP.
    ///
    /// Takes the closure directly rather than a [`github::Transport`]: the
    /// stored form is shared across threads, and a bare boxed closure carries
    /// no `Send` or `Sync` bound to recover.
    pub fn with_forge_transport(
        mut self,
        transport: impl Fn(
                &str,
                &BTreeMap<String, String>,
                &[u8],
                &str,
            ) -> (u16, Vec<u8>, BTreeMap<String, String>)
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.forge_transport = Some(Arc::new(transport));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::{build_context, with_stores_at, Built};
    use crate::config::VogtConfig;
    use crate::core::{Moment, SequentialIds, StepClock};

    /// A context built the way a test builds one, with its own clock and ids so
    /// the hook environment is never consulted for either.
    fn built(dir: &std::path::Path) -> Built {
        build_context(
            VogtConfig {
                data_dir: dir.to_path_buf(),
                ..VogtConfig::default()
            },
            None,
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap()
    }

    #[test]
    fn the_stand_in_comes_back_out_and_survives_a_rebuild() {
        let dir = std::env::temp_dir().join(format!("vogt-ctx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let Built::StepSequential(ctx) = built(&dir) else {
            panic!("an explicit step clock and sequential ids");
        };
        // The constructors leave it unset, which is the deployed shape.
        assert!(ctx.forge_transport().is_none());

        let ctx = ctx.with_forge_transport(|url, _, body, method| {
            (
                200,
                format!("{method} {url} {}", body.len()).into_bytes(),
                BTreeMap::new(),
            )
        });
        let transport = ctx.forge_transport().expect("the stand-in");
        let (status, body, _) = transport("https://forge/x", &BTreeMap::new(), b"abc", "GET");
        assert_eq!(status, 200);
        assert_eq!(body, b"GET https://forge/x 3");

        // One request spends the stand-in on more than one provider.
        let again = ctx.forge_transport().expect("the same stand-in");
        assert_eq!(
            again("https://forge/y", &BTreeMap::new(), b"", "POST").1,
            b"POST https://forge/y 0"
        );

        let rebuilt = with_stores_at(&ctx, &dir.join("copy"));
        let carried = rebuilt.forge_transport().expect("the rebuild keeps it");
        assert_eq!(
            carried("https://forge/z", &BTreeMap::new(), b"z", "PUT").1,
            b"PUT https://forge/z 1"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

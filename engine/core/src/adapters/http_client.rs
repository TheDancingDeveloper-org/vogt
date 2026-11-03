//! Shared configuration for outbound ureq clients.

use super::engine::http1;

/// Build the core's explicit TLS and proxy policy.
///
/// ureq 2 followed five redirects. ureq 3 strips `Authorization` on a
/// redirect, which is the safe compatible behavior for these clients.
pub(crate) fn builder() -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    ureq::Agent::config_builder()
        .tls_config(http1::tls_config())
        // ureq 2 did not read HTTP(S)_PROXY/ALL_PROXY from the environment.
        .proxy(None)
        .max_redirects(5)
}

#[cfg(test)]
mod tests {
    use super::builder;

    #[test]
    fn shared_agent_has_explicit_tls_and_ignores_proxy_environment() {
        // The builder must be safe even when a deployment exports HTTPS_PROXY
        // or ALL_PROXY: ureq 2 did not consult those variables.
        let config = builder().build();
        assert!(config.proxy().is_none());
        assert_eq!(config.max_redirects(), 5);
        assert_eq!(
            config.tls_config().provider(),
            ureq::tls::TlsProvider::Rustls
        );
    }
}

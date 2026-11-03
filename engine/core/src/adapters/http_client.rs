//! Shared configuration for outbound ureq clients.

use super::engine::http1;

/// Build the core's explicit TLS and proxy policy.
///
/// ureq 2 followed five redirects. ureq 3 strips `Authorization` on a
/// redirect, which is the safe compatible behavior for these clients.
pub(crate) fn builder() -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    builder_with_tls(http1::tls_config())
}

fn builder_with_tls(
    tls: ureq::tls::TlsConfig,
) -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    ureq::Agent::config_builder()
        .tls_config(tls)
        // ureq 2 did not read HTTP(S)_PROXY/ALL_PROXY from the environment.
        .proxy(None)
        .max_redirects(5)
}

#[cfg(test)]
mod tests {
    use super::{builder, builder_with_tls};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;

    fn test_tls(cert: &rustls::pki_types::CertificateDer<'static>) -> ureq::tls::TlsConfig {
        ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::new_with_certs(&[
                ureq::tls::Certificate::from_der(cert.as_ref()).to_owned(),
            ]))
            .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .build()
    }

    #[test]
    fn shared_agent_makes_a_real_https_round_trip() {
        let generated = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert = generated.cert.der().clone();
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(
            rustls::pki_types::PrivatePkcs8KeyDer::from(generated.key_pair.serialize_der()),
        );
        let server_config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], key)
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let thread = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut stream = rustls::StreamOwned::new(
                rustls::ServerConnection::new(Arc::new(server_config)).unwrap(),
                stream,
            );
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello")
                .unwrap();
            stream.flush().unwrap();
        });

        let agent: ureq::Agent = builder_with_tls(test_tls(&cert))
            .timeout_global(Some(std::time::Duration::from_secs(2)))
            .build()
            .into();
        let response = agent
            .get(format!("https://localhost:{}/", address.port()))
            .call();
        assert_eq!(
            response.unwrap().into_body().read_to_string().unwrap(),
            "hello"
        );
        thread.join().unwrap();
    }

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

    #[test]
    fn shared_agent_ignores_https_and_all_proxy_environment() {
        const CHILD: &str = "VOGT_PROXY_POLICY_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "adapters::http_client::tests::shared_agent_ignores_https_and_all_proxy_environment",
                ])
                .env(CHILD, "1")
                .env("HTTPS_PROXY", "http://127.0.0.1:9")
                .env("ALL_PROXY", "http://127.0.0.1:9")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        assert_eq!(std::env::var("HTTPS_PROXY").unwrap(), "http://127.0.0.1:9");
        assert_eq!(std::env::var("ALL_PROXY").unwrap(), "http://127.0.0.1:9");
        // The proxy setting is explicit rather than inherited from either
        // environment variable, so these bogus proxies cannot be selected.
        let config = builder().build();
        assert!(config.proxy().is_none());
    }
}

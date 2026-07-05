//! Generates (and persists) a self-signed TLS certificate for `localhost`, shared by every
//! emulator module that needs a real TLS listener - e.g. the Service Bus AMQPS listener and
//! the Storage (Blob) HTTPS listener.
//!
//! Why this exists: Azure SDK clients only skip TLS when constructed from a *connection
//! string* (Service Bus's `UseDevelopmentEmulator=true`, or Blob's plain `http://` endpoint
//! with an account key). Clients configured with a `TokenCredential` instead (e.g. to
//! locally replicate how a deployed app authenticates via Managed Identity, since real
//! managed identity/IMDS isn't reachable outside Azure, developers typically fall back to
//! `DefaultAzureCredential` picking up their own `az login`/Visual Studio session) always
//! connect over TLS, with no bypass flag - Azure Core's bearer-token auth policy refuses to
//! attach a token to a non-HTTPS request outright. So a TLS listener is required to support
//! that auth style at all, even though this emulator never validates the token it receives.
//!
//! The certificate is generated once and persisted to disk so it keeps the same key across
//! restarts - once a developer trusts it (see [`DevCertificate::trust`]), a marker file next
//! to it remembers that, so [`DevCertificate::is_trusted`] can report it without re-running
//! `certutil` (or prompting again) on every subsequent launch.

use std::path::PathBuf;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// A loaded (or freshly generated) self-signed dev certificate, plus its parsed TLS server
/// config, ready for any protocol (raw TLS, HTTPS, ...) to build its own acceptor from.
pub struct DevCertificate {
    /// PEM-encoded certificate, useful for importing into an OS trust store.
    pub cert_pem: String,
    /// Absolute path the certificate is persisted at, so callers can point a developer at it
    /// (e.g. for a "trust this certificate" action).
    pub cert_path: PathBuf,
    pub server_config: Arc<rustls::ServerConfig>,
}

impl DevCertificate {
    /// Convenience wrapper for protocols (like AMQP) that accept raw TCP streams and want a
    /// ready-to-use `tokio-rustls` acceptor rather than a bare `rustls::ServerConfig`.
    pub fn tls_acceptor(&self) -> tokio_rustls::TlsAcceptor {
        tokio_rustls::TlsAcceptor::from(self.server_config.clone())
    }

    /// Whether this exact certificate has already been successfully trusted (tracked via a
    /// marker file written by [`DevCertificate::trust`] on success, since there's no cheap,
    /// portable way to query the OS trust store directly).
    pub fn is_trusted(&self) -> bool {
        trusted_marker_path().exists()
    }

    /// Installs this certificate into the current Windows user's Trusted Root store, so TLS
    /// clients (e.g. the Azure SDK) accept it with zero extra configuration - the same
    /// one-time step `dotnet dev-certs https --trust` automates for local HTTPS development.
    /// On success, writes the marker file [`DevCertificate::is_trusted`] checks, so callers
    /// only need to prompt/attempt this once per certificate (not on every launch).
    ///
    /// Callers (the GUI) are expected to ask the user's permission before calling this -
    /// this crate deliberately never runs it automatically, since silently modifying the
    /// OS certificate store without asking is surprising behavior for a local dev tool.
    pub fn trust(&self) -> anyhow::Result<()> {
        // `-user` scopes this to the current Windows user's store (no admin/UAC needed)
        // rather than the machine-wide store. `certutil -addstore` is itself idempotent -
        // re-running it against an already-trusted cert is a harmless no-op.
        let output = std::process::Command::new("certutil")
            .args(["-user", "-addstore", "Root"])
            .arg(&self.cert_path)
            .output()?;
        if !output.status.success() {
            anyhow::bail!(
                "certutil exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let _ = std::fs::write(trusted_marker_path(), "");
        Ok(())
    }
}

/// Loads the persisted dev certificate/key, generating and persisting a fresh self-signed pair
/// for `localhost` if none exists yet (or the existing files can't be parsed).
pub fn load_or_generate() -> anyhow::Result<DevCertificate> {
    init_crypto_provider();

    let dir = cert_dir();
    let cert_path = dir.join("dev-cert.pem");
    let key_path = dir.join("dev-key.pem");

    let (cert_pem, key_pem) = match (
        std::fs::read_to_string(&cert_path),
        std::fs::read_to_string(&key_path),
    ) {
        (Ok(c), Ok(k)) if !c.trim().is_empty() && !k.trim().is_empty() => (c, k),
        _ => {
            let (cert_pem, key_pem) = generate_pem()?;
            std::fs::write(&cert_path, &cert_pem)?;
            std::fs::write(&key_path, &key_pem)?;
            // A freshly generated cert/key invalidates any previous trust marker - the old
            // trust (if any) was for a different key and doesn't carry over.
            let _ = std::fs::remove_file(trusted_marker_path());
            (cert_pem, key_pem)
        }
    };

    let cert_chain = parse_cert_pem(&cert_pem)?;
    let private_key = parse_key_pem(&key_pem)?;
    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, private_key)?;

    Ok(DevCertificate {
        cert_pem,
        cert_path,
        server_config: Arc::new(server_config),
    })
}

/// Marker file written by [`DevCertificate::trust`] on success, so [`DevCertificate::is_trusted`]
/// doesn't need to repeat that check (or callers to re-prompt) on every launch.
fn trusted_marker_path() -> PathBuf {
    cert_dir().join("dev-cert.trusted")
}

fn generate_pem() -> anyhow::Result<(String, String)> {
    let names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    let rcgen::CertifiedKey { cert, key_pair } = rcgen::generate_simple_self_signed(names)?;
    Ok((cert.pem(), key_pair.serialize_pem()))
}

fn parse_cert_pem(pem: &str) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let mut reader = std::io::Cursor::new(pem.as_bytes());
    let certs = rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>, _>>()?;
    Ok(certs)
}

fn parse_key_pem(pem: &str) -> anyhow::Result<PrivateKeyDer<'static>> {
    let mut reader = std::io::Cursor::new(pem.as_bytes());
    rustls_pemfile::private_key(&mut reader)?
        .ok_or_else(|| anyhow::anyhow!("no private key found in generated dev certificate PEM"))
}

/// Installs `ring` as the process-wide default `rustls` crypto provider. Idempotent - safe to
/// call from every emulator instance's startup path even though only one install can ever
/// succeed per process.
fn init_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Directory persisted dev-certificate files live in, created on demand.
fn cert_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("EmuEngine").join("certs");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

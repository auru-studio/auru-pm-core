//! TLS termination, for reaching this server from a device.
//!
//! A deployment puts the server behind a reverse proxy that owns TLS — that is
//! what `public_base_url` describes, and nothing here changes it. This module
//! exists because a mobile client has no proxy to sit behind: Android has
//! refused cleartext HTTP by default since API 28, so neither an emulator nor a
//! phone on the LAN can reach a plain-HTTP development server at all.
//!
//! Two modes. `files` serves a chain somebody else issued — mkcert, an internal
//! CA, a staging certificate. `development_certificate` mints a self-signed
//! authority and issues a fresh leaf from it on every start, so the certificate
//! always covers whatever addresses this run actually listens on while the
//! authority — the one thing a client has to be told to trust — stays put.
//!
//! The authority lives per user, not per data directory: `~/.local/share/
//! auru-pm/development-tls` by default. A client bakes that anchor in at build
//! time, so tying it to `data_dir` would mean every `--data-dir` minted an
//! authority the already-installed client had never heard of — a handshake
//! failure with no visible cause. mkcert keeps its root in one place per
//! machine for the same reason.

use std::env;
use std::fs;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
};
use rustls::ServerConfig as RustlsConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::config::TlsConfig;

/// The Android emulator's fixed alias for the host machine's loopback address.
///
/// Present in every development certificate whether or not this run is for
/// Android: it costs one subject alternative name, and leaving it out is the
/// difference between the emulator working and a TLS handshake failure whose
/// message names no address at all.
const ANDROID_EMULATOR_HOST_ALIAS: &str = "10.0.2.2";

/// Everything the listener needs, plus what the operator has to be told.
pub struct TlsMaterial {
    pub server_config: RustlsConfig,
    /// Present only for a self-signed development chain, which is useless to a
    /// client that has not been handed the authority to trust.
    pub development_authority: Option<DevelopmentAuthority>,
}

/// The trust anchor a development client has to install.
pub struct DevelopmentAuthority {
    pub certificate_path: PathBuf,
    pub subject_alt_names: Vec<String>,
    /// SHA-256 over the certificate DER, in the usual colon-separated hex.
    ///
    /// Printed at startup so "does the client trust this server" is a glance
    /// rather than an investigation: a client bundles the anchor at build time,
    /// and the failure when it bundled a different one is a handshake error
    /// that names neither certificate.
    pub fingerprint: String,
}

/// Build a TLS configuration for `config`, minting a development authority
/// under `data_dir` when the configuration asks for one.
pub fn prepare(
    config: &TlsConfig,
    data_dir: &Path,
    listen: SocketAddr,
) -> Result<TlsMaterial, String> {
    match config {
        TlsConfig::Files {
            certificate,
            private_key,
        } => {
            let chain = read_pem(certificate, "tls.certificate")?;
            let key = read_pem(private_key, "tls.private_key")?;
            Ok(TlsMaterial {
                server_config: server_config(&chain, &key)?,
                development_authority: None,
            })
        }
        TlsConfig::DevelopmentCertificate {
            subject_alt_names,
            authority_directory,
        } => {
            let directory = authority_directory
                .clone()
                .or_else(user_authority_directory)
                // No home directory to put it in — better a working server with
                // a data-directory-local authority than a refusal to start.
                .unwrap_or_else(|| data_dir.join("development-tls"));
            let authority = load_or_create_authority(&directory)?;
            let names = resolve_subject_alt_names(listen, subject_alt_names);
            let (chain, key) = issue_leaf(&authority, &names)?;
            Ok(TlsMaterial {
                server_config: server_config(chain.as_bytes(), key.as_bytes())?,
                development_authority: Some(DevelopmentAuthority {
                    fingerprint: fingerprint(&authority.certificate_pem)
                        .unwrap_or_else(|| "unavailable".to_owned()),
                    certificate_path: authority.certificate_path,
                    subject_alt_names: names,
                }),
            })
        }
    }
}

fn read_pem(path: &Path, field: &str) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|error| format!("{field}: read {}: {error}", path.display()))
}

fn server_config(chain_pem: &[u8], key_pem: &[u8]) -> Result<RustlsConfig, String> {
    let chain = CertificateDer::pem_slice_iter(chain_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("TLS certificate chain: {error}"))?;
    if chain.is_empty() {
        return Err("TLS certificate chain contains no CERTIFICATE block".to_owned());
    }
    let key = PrivateKeyDer::from_pem_slice(key_pem)
        .map_err(|error| format!("TLS private key: {error}"))?;

    // The provider is named rather than taken from rustls' global default: the
    // process also links reqwest, and a server that silently depended on
    // whichever crate installed a default first would be a puzzle to debug.
    let mut config =
        RustlsConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|error| format!("TLS protocol versions: {error}"))?
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .map_err(|error| format!("TLS certificate and key do not match: {error}"))?;
    // Without ALPN a client that offers h2 gets no answer and falls back, or
    // fails outright; axum serves both, so advertise both.
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

// ── The development authority ────────────────────────────────────────────────

struct Authority {
    issuer: Issuer<'static, KeyPair>,
    certificate_pem: String,
    certificate_path: PathBuf,
}

/// `$XDG_DATA_HOME/auru-pm/development-tls`, or the usual fallback under `$HOME`.
fn user_authority_directory() -> Option<PathBuf> {
    let base = match env::var_os("XDG_DATA_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => PathBuf::from(env::var_os("HOME")?).join(".local/share"),
    };
    Some(base.join("auru-pm/development-tls"))
}

fn load_or_create_authority(directory: &Path) -> Result<Authority, String> {
    let certificate_path = directory.join("development-ca.pem");
    let key_path = directory.join("development-ca.key.pem");

    let (certificate_pem, key_pem) = match (
        read_if_present(&certificate_path)?,
        read_if_present(&key_path)?,
    ) {
        (Some(certificate), Some(key)) => (certificate, key),
        (None, None) => {
            // Loud, because every client that bundled the previous authority
            // stops being able to complete a handshake the moment this happens.
            tracing::warn!(
                "TLS: minting a new development authority in {}; rebuild and reinstall any client that bundled the previous one",
                directory.display()
            );
            create_authority(directory, &certificate_path, &key_path)?
        }
        // Half a pair is not repaired by overwriting it. Re-minting would throw
        // away the key every installed client's trust anchor derives from, so
        // the destructive step is left to a human who can weigh it.
        (certificate, _) => {
            let (present, missing) = if certificate.is_some() {
                (&certificate_path, &key_path)
            } else {
                (&key_path, &certificate_path)
            };
            return Err(format!(
                "development authority is incomplete: {} exists but {} does not. \
                 Delete {} to mint a fresh authority — every client bundling the old \
                 one must then be rebuilt.",
                present.display(),
                missing.display(),
                present.display(),
            ));
        }
    };

    let key = KeyPair::from_pem(&key_pem)
        .map_err(|error| format!("development CA key {}: {error}", key_path.display()))?;
    // Rebuilt from the same parameters that generated it rather than parsed
    // back out of the certificate: the issuer needs only the distinguished
    // name, key usages, and key identifier method, all of which this file
    // decides, and reconstructing them keeps an X.509 parser out of the build.
    Ok(Authority {
        issuer: Issuer::new(authority_params(), key),
        certificate_pem,
        certificate_path,
    })
}

/// `Ok(None)` only when the file is genuinely absent.
///
/// Any other error is reported rather than treated as "mint a replacement": a
/// permissions problem or a half-written file is a reason to stop, not a reason
/// to silently invalidate every client that bundled the existing anchor.
fn read_if_present(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("read {}: {error}", path.display())),
    }
}

/// SHA-256 over the certificate DER — the value `openssl x509 -fingerprint
/// -sha256` prints, so it compares directly against any other tool's output.
pub fn fingerprint(certificate_pem: &str) -> Option<String> {
    let der = CertificateDer::from_pem_slice(certificate_pem.as_bytes()).ok()?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &der);
    Some(
        digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}

fn create_authority(
    directory: &Path,
    certificate_path: &Path,
    key_path: &Path,
) -> Result<(String, String), String> {
    fs::create_dir_all(directory)
        .map_err(|error| format!("create {}: {error}", directory.display()))?;
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
        .map_err(|error| format!("generate development CA key: {error}"))?;
    let certificate = authority_params()
        .self_signed(&key)
        .map_err(|error| format!("sign development CA certificate: {error}"))?;

    let certificate_pem = certificate.pem();
    let key_pem = key.serialize_pem();
    write_file(certificate_path, certificate_pem.as_bytes(), false)?;
    write_file(key_path, key_pem.as_bytes(), true)?;
    Ok((certificate_pem, key_pem))
}

fn authority_params() -> CertificateParams {
    // Validity is left at rcgen's wide default. A private anchor that a
    // developer installs deliberately is not subject to the 398-day ceiling
    // public certificates are, and an expiry would surface months later as an
    // unexplained handshake failure on a machine nobody had touched.
    let mut params = CertificateParams::default();
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, "Auru PM development CA");
    name.push(DnType::OrganizationName, "Auru PM");
    params.distinguished_name = name;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    params
}

/// Issue a leaf for `names`, returning its PEM chain and private key.
fn issue_leaf(authority: &Authority, names: &[String]) -> Result<(String, String), String> {
    let mut params = CertificateParams::new(names.to_vec())
        .map_err(|error| format!("subject alternative names {names:?}: {error}"))?;
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, "Auru PM development server");
    params.distinguished_name = name;
    params.is_ca = IsCa::NoCa;
    params.use_authority_key_identifier_extension = true;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];

    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
        .map_err(|error| format!("generate development server key: {error}"))?;
    let certificate = params
        .signed_by(&key, &authority.issuer)
        .map_err(|error| format!("sign development server certificate: {error}"))?;

    // Leaf first, then the authority: a client that already trusts the anchor
    // does not need the second entry, but one that was handed the leaf alone
    // cannot build a path without it.
    let chain = format!("{}{}", certificate.pem(), authority.certificate_pem);
    Ok((chain, key.serialize_pem()))
}

fn write_file(path: &Path, bytes: &[u8], private: bool) -> Result<(), String> {
    fs::write(path, bytes).map_err(|error| format!("write {}: {error}", path.display()))?;
    if private {
        restrict_to_owner(path).map_err(|error| format!("restrict {}: {error}", path.display()))?;
    }
    Ok(())
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> io::Result<()> {
    Ok(())
}

// ── Subject alternative names ────────────────────────────────────────────────

/// Every name a development certificate should answer to for this run.
///
/// A listener bound to a wildcard address is reachable at every interface the
/// machine has, and which one a phone will use is not knowable here — so all of
/// them go in the certificate. Names an operator configured come last but are
/// never dropped.
fn resolve_subject_alt_names(listen: SocketAddr, configured: &[String]) -> Vec<String> {
    let mut names = vec![
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
        "::1".to_owned(),
        ANDROID_EMULATOR_HOST_ALIAS.to_owned(),
    ];
    if listen.ip().is_unspecified() {
        names.extend(interface_addresses());
    } else {
        names.push(listen.ip().to_string());
    }
    names.extend(configured.iter().cloned());

    let mut seen = std::collections::BTreeSet::new();
    names.retain(|name| seen.insert(name.clone()));
    names
}

/// Non-loopback interface addresses, so a phone on the same network works.
///
/// Ordered with real network interfaces first: a container or VM bridge address
/// is a valid address of this machine and belongs in the certificate, but it is
/// never the one a phone can reach, and the startup banner tells the reader to
/// paste the first URL it prints.
///
/// Failure is not fatal: the loopback names above still cover an emulator, and
/// a certificate that omits the LAN address produces a clearer error than a
/// server that refused to start.
pub fn interface_addresses() -> Vec<String> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut addresses: Vec<(bool, String)> = interfaces
        .into_iter()
        .filter(|interface| {
            let ip = interface.addr.ip();
            !ip.is_loopback() && !is_link_local(&ip)
        })
        .map(|interface| {
            (
                is_virtual_interface(&interface.name),
                interface.addr.ip().to_string(),
            )
        })
        .collect();
    // Stable, so interfaces of the same kind keep the order the OS reported.
    addresses.sort_by_key(|(is_virtual, _)| *is_virtual);
    addresses.into_iter().map(|(_, address)| address).collect()
}

/// Whether `name` looks like a container, VM, or tunnel interface.
///
/// A name-prefix heuristic, deliberately: these addresses are still published
/// in the certificate, so a wrong guess costs ordering in a banner and nothing
/// else. `docker0`, `br-1a2b…`, `virbr0`, `vmnet1`, `tun0`, `veth…`.
fn is_virtual_interface(name: &str) -> bool {
    const VIRTUAL_PREFIXES: [&str; 7] = ["docker", "br-", "virbr", "vmnet", "tun", "tap", "veth"];
    VIRTUAL_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

fn is_link_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        // `Ipv6Addr::is_unicast_link_local` is unstable; `fe80::/10` by hand.
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listen(address: &str) -> SocketAddr {
        address.parse().expect("test address")
    }

    #[test]
    fn loopback_names_and_the_emulator_alias_are_always_present() {
        let names = resolve_subject_alt_names(listen("127.0.0.1:4242"), &[]);
        for expected in ["localhost", "127.0.0.1", "::1", ANDROID_EMULATOR_HOST_ALIAS] {
            assert!(names.iter().any(|name| name == expected), "{names:?}");
        }
    }

    #[test]
    fn a_specific_listen_address_is_covered() {
        let names = resolve_subject_alt_names(listen("192.168.1.40:4242"), &[]);
        assert!(names.iter().any(|name| name == "192.168.1.40"), "{names:?}");
    }

    #[test]
    fn configured_names_are_kept_and_duplicates_collapse() {
        let names = resolve_subject_alt_names(
            listen("127.0.0.1:4242"),
            &["studio.local".to_owned(), "127.0.0.1".to_owned()],
        );
        assert!(names.iter().any(|name| name == "studio.local"), "{names:?}");
        assert_eq!(
            names.iter().filter(|name| *name == "127.0.0.1").count(),
            1,
            "{names:?}"
        );
    }

    #[test]
    fn container_bridges_sort_after_real_interfaces() {
        assert!(is_virtual_interface("docker0"));
        assert!(is_virtual_interface("br-456d74430626"));
        assert!(is_virtual_interface("virbr0"));
        assert!(!is_virtual_interface("enp8s0"));
        assert!(!is_virtual_interface("wlan0"));
        assert!(!is_virtual_interface("eth0"));
    }

    #[test]
    fn a_development_authority_is_reused_across_starts() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = TlsConfig::DevelopmentCertificate {
            subject_alt_names: Vec::new(),
            authority_directory: Some(directory.path().join("development-tls")),
        };

        let first = prepare(&config, directory.path(), listen("127.0.0.1:4242"))
            .expect("first start mints an authority");
        let authority = first
            .development_authority
            .expect("development mode reports its authority");
        let minted = fs::read_to_string(&authority.certificate_path).expect("CA certificate");

        let second = prepare(&config, directory.path(), listen("127.0.0.1:4242"))
            .expect("second start reuses the authority");
        let reused = fs::read_to_string(
            &second
                .development_authority
                .expect("development mode reports its authority")
                .certificate_path,
        )
        .expect("CA certificate");

        // The leaf is reissued every start; the anchor a client installed is not.
        assert_eq!(minted, reused);
    }

    #[test]
    fn two_data_directories_share_one_authority() {
        // The bug this prevents: a client bakes in the anchor at build time, so
        // a second `--data-dir` minting its own authority breaks an already
        // installed client with a handshake failure and no visible cause.
        let home = tempfile::tempdir().expect("temporary home");
        let shared = home.path().join("development-tls");
        let config = TlsConfig::DevelopmentCertificate {
            subject_alt_names: Vec::new(),
            authority_directory: Some(shared.clone()),
        };
        let first = tempfile::tempdir().expect("first data directory");
        let second = tempfile::tempdir().expect("second data directory");

        for data_dir in [first.path(), second.path()] {
            let material =
                prepare(&config, data_dir, listen("127.0.0.1:4242")).expect("development material");
            assert_eq!(
                material
                    .development_authority
                    .expect("development mode reports its authority")
                    .certificate_path,
                shared.join("development-ca.pem")
            );
            assert!(!data_dir.join("development-tls").exists());
        }
    }

    #[test]
    fn a_half_present_authority_is_not_silently_replaced() {
        // The way a shared trust anchor gets lost: one file goes missing and the
        // server mints a replacement, so every installed client's bundled anchor
        // is now for an authority that no longer exists.
        let home = tempfile::tempdir().expect("temporary home");
        let shared = home.path().join("development-tls");
        let config = TlsConfig::DevelopmentCertificate {
            subject_alt_names: Vec::new(),
            authority_directory: Some(shared.clone()),
        };
        let data_dir = tempfile::tempdir().expect("data directory");

        let first = prepare(&config, data_dir.path(), listen("127.0.0.1:4242"))
            .expect("first start mints an authority");
        let original = first.development_authority.expect("authority").fingerprint;
        fs::remove_file(shared.join("development-ca.key.pem")).expect("remove the key");

        let Err(error) = prepare(&config, data_dir.path(), listen("127.0.0.1:4242")) else {
            panic!("an incomplete authority must stop the server, not be overwritten");
        };
        assert!(error.contains("incomplete"), "{error}");
        // The certificate every client bundled is still on disk, untouched.
        let surviving = fs::read_to_string(shared.join("development-ca.pem")).expect("certificate");
        assert_eq!(fingerprint(&surviving).expect("fingerprint"), original);
    }

    #[test]
    fn the_fingerprint_identifies_the_authority_a_client_must_bundle() {
        let home = tempfile::tempdir().expect("temporary home");
        let config = TlsConfig::DevelopmentCertificate {
            subject_alt_names: Vec::new(),
            authority_directory: Some(home.path().join("development-tls")),
        };
        let data_dir = tempfile::tempdir().expect("data directory");

        let authority = prepare(&config, data_dir.path(), listen("127.0.0.1:4242"))
            .expect("material")
            .development_authority
            .expect("authority");
        let on_disk = fs::read_to_string(&authority.certificate_path).expect("certificate");
        assert_eq!(
            fingerprint(&on_disk).as_deref(),
            Some(authority.fingerprint.as_str())
        );
        assert_eq!(
            authority.fingerprint.len(),
            32 * 3 - 1,
            "{}",
            authority.fingerprint
        );
    }

    #[test]
    fn a_supplied_chain_and_key_are_served_as_given() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let source = prepare(
            &TlsConfig::DevelopmentCertificate {
                subject_alt_names: Vec::new(),
                authority_directory: Some(directory.path().join("development-tls")),
            },
            directory.path(),
            listen("127.0.0.1:4242"),
        )
        .expect("development material");
        drop(source);

        // Reissue by hand so the test has a chain/key pair on disk to point at.
        let authority =
            load_or_create_authority(&directory.path().join("development-tls")).expect("authority");
        let (chain, key) = issue_leaf(&authority, &["localhost".to_owned()]).expect("leaf");
        let chain_path = directory.path().join("chain.pem");
        let key_path = directory.path().join("key.pem");
        fs::write(&chain_path, &chain).expect("write chain");
        fs::write(&key_path, &key).expect("write key");

        let material = prepare(
            &TlsConfig::Files {
                certificate: chain_path,
                private_key: key_path,
            },
            directory.path(),
            listen("127.0.0.1:4242"),
        )
        .expect("file-backed material");
        assert!(material.development_authority.is_none());
    }

    #[test]
    fn a_mismatched_key_is_refused() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let authority =
            load_or_create_authority(&directory.path().join("development-tls")).expect("authority");
        let (chain, _) = issue_leaf(&authority, &["localhost".to_owned()]).expect("leaf");
        let (_, other_key) = issue_leaf(&authority, &["localhost".to_owned()]).expect("leaf");

        let error = server_config(chain.as_bytes(), other_key.as_bytes())
            .expect_err("a key from a different leaf must not be accepted");
        assert!(error.contains("do not match"), "{error}");
    }
}

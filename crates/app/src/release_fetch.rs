//! Network side of updates: HTTP agents (with the proxy chosen in
//! Settings), the latest-release request, the verified Download and the
//! engine-driven Update Now install. No UI here; `workbench/updates.rs`
//! drives it from background tasks.

use std::time::Duration;

use ora_core::update::{self, ReleaseInfo};

use crate::release_verify::{self, ReleaseProof};

/// The proxy in use. The URL may carry a password, so it only ever lives in
/// memory here; `settings.json` holds the URL without it and the OS keychain
/// holds the password (see `ora_core::store::resolve_proxy_in`).
struct ProxyState {
    /// `None` falls back to the `HTTPS_PROXY` / `ALL_PROXY` environment
    /// variables (ureq's default).
    url: Option<String>,
    /// Settings were read (and an old plaintext password migrated).
    resolved: bool,
}

static HTTP_PROXY: std::sync::Mutex<ProxyState> = std::sync::Mutex::new(ProxyState {
    url: None,
    resolved: false,
});
/// Held while the keychain is read, so only one thread does it.
static RESOLVING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Use `url` as the proxy now (what the user just typed in Settings).
pub(crate) fn set_http_proxy(url: Option<String>) {
    if let Ok(mut state) = HTTP_PROXY.lock() {
        state.url = url.map(|u| u.trim().to_string()).filter(|u| !u.is_empty());
        state.resolved = true;
    }
}

/// Load the saved proxy once: settings plus password from the keychain,
/// moving an old plaintext password into the keychain. Blocks on the
/// keychain, so call it off the UI thread. Returns a warning for the user.
pub(crate) fn ensure_proxy_resolved() -> Option<String> {
    let _guard = RESOLVING.lock().ok()?;
    if HTTP_PROXY.lock().ok()?.resolved {
        return None;
    }
    let found =
        ora_core::store::resolve_proxy_in(&ora_core::store::config_dir(), crate::keychain::store());
    let mut state = HTTP_PROXY.lock().ok()?;
    // The user may have typed a proxy meanwhile; that wins.
    if !state.resolved {
        state.url = found.url;
        state.resolved = true;
    }
    found.warning
}

/// An error as text that is safe to show and log: a proxy URL inside it
/// loses its password.
fn net_err(err: impl std::fmt::Display) -> String {
    let text = err.to_string();
    let secret = HTTP_PROXY
        .lock()
        .ok()
        .and_then(|s| s.url.clone())
        .and_then(|u| ora_core::redact::split_proxy_password(&u).1);
    let text = match secret {
        Some(pw) => ora_core::redact::scrub_secret(&text, &pw),
        None => text,
    };
    ora_core::redact::mask_url_credentials(&text)
}

/// `Ok(None)` keeps the default; a bad URL is reported instead of ignored.
fn configured_proxy() -> Result<Option<ureq::Proxy>, String> {
    ensure_proxy_resolved();
    let url = HTTP_PROXY.lock().ok().and_then(|state| state.url.clone());
    url.map(|u| {
        ureq::Proxy::new(&u).map_err(|e| {
            // Show the URL without its password.
            format!(
                "Invalid proxy ({}): {}",
                ora_core::redact::split_proxy_password(&u).0,
                net_err(e)
            )
        })
    })
    .transpose()
}

fn http_agent() -> Result<ureq::Agent, String> {
    let proxy = configured_proxy()?;
    let mut builder = ureq::Agent::config_builder();
    if proxy.is_some() {
        builder = builder.proxy(proxy);
    }
    Ok(builder
        .timeout_global(Some(Duration::from_secs(10)))
        // OS trust store (macOS Keychain, Windows, Linux CA bundle), so
        // corporate TLS-inspecting proxies work like in the browser.
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .user_agent(concat!("kilat/", env!("CARGO_PKG_VERSION")))
        .build()
        .into())
}

/// Blocking fetch of the latest release; run on a background thread.
pub(crate) fn fetch_release(url: &str) -> Result<ReleaseInfo, String> {
    let agent = http_agent()?;
    let mut resp = agent
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(net_err)?;
    let body = resp
        .body_mut()
        .with_config()
        .limit(2 * 1024 * 1024)
        .read_to_string()
        .map_err(net_err)?;
    update::parse_release(&body).ok_or_else(|| "Could not read the latest release.".to_string())
}

/// Where downloads go: `~/Downloads` when it exists, else the temp dir.
fn download_dir() -> std::path::PathBuf {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    home.map(|h| std::path::PathBuf::from(h).join("Downloads"))
        .filter(|d| d.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

/// Largest release file the app downloads.
const MAX_DOWNLOAD_BYTES: u64 = 400 * 1024 * 1024;

fn download_agent() -> Result<ureq::Agent, String> {
    let proxy = configured_proxy()?;
    let mut builder = ureq::Agent::config_builder();
    if proxy.is_some() {
        builder = builder.proxy(proxy);
    }
    Ok(builder
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_body(Some(Duration::from_secs(60)))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .user_agent(concat!("kilat/", env!("CARGO_PKG_VERSION")))
        .build()
        .into())
}

/// Blocking GET of a small text asset (checksum list, signature).
fn fetch_text(agent: &ureq::Agent, asset: &update::Asset) -> Result<String, String> {
    let mut resp = agent.get(&asset.url).call().map_err(net_err)?;
    resp.body_mut()
        .with_config()
        .limit(1024 * 1024)
        .read_to_string()
        .map_err(|e| format!("{}: {}", asset.name, net_err(e)))
}

/// Fetch and check the signed `SHA256SUMS` and `<asset>.minisig` of
/// `asset`. A release without them is refused here, before the file
/// itself is downloaded.
fn fetch_proof(info: &ReleaseInfo, asset: &update::Asset) -> Result<ReleaseProof, String> {
    let find = |name: &str| {
        info.assets.iter().find(|a| a.name == name).ok_or_else(|| {
            format!("this release is not signed ({name} is missing), so it was not downloaded")
        })
    };
    let sums = find(release_verify::SUMS_NAME)?;
    let sums_sig = find(&release_verify::signature_name(release_verify::SUMS_NAME))?;
    let asset_sig = find(&release_verify::signature_name(&asset.name))?;
    let agent = http_agent()?;
    ReleaseProof::new(
        &asset.name,
        &info.version.to_string(),
        &fetch_text(&agent, sums)?,
        &fetch_text(&agent, sums_sig)?,
        &fetch_text(&agent, asset_sig)?,
        release_verify::release_public_key(),
    )
}

/// Blocking: download `asset` into a fresh owner-only temp directory,
/// verify it, and only then copy it into the Downloads folder. A file that
/// fails any check is deleted with the temp directory and never reaches
/// Downloads.
pub(crate) fn download_asset(
    info: &ReleaseInfo,
    asset: &update::Asset,
) -> Result<std::path::PathBuf, String> {
    use std::io::Write as _;
    let proof = fetch_proof(info, asset)?;
    if asset.size > MAX_DOWNLOAD_BYTES {
        return Err(format!("{} is larger than the download limit", asset.name));
    }
    let staging = tempfile::Builder::new()
        .prefix("kilat-download-")
        .tempdir()
        .map_err(net_err)?;
    let temp = staging.path().join(&asset.name);
    {
        let limit = if asset.size > 0 {
            asset.size
        } else {
            MAX_DOWNLOAD_BYTES
        };
        let mut resp = download_agent()?.get(&asset.url).call().map_err(net_err)?;
        let mut file = std::fs::File::create(&temp).map_err(net_err)?;
        let mut reader = resp.body_mut().with_config().limit(limit).reader();
        let copied = std::io::copy(&mut reader, &mut file).map_err(net_err)?;
        file.flush().map_err(net_err)?;
        if asset.size > 0 && copied != asset.size {
            return Err(format!(
                "{} is incomplete ({copied} of {} bytes)",
                asset.name, asset.size
            ));
        }
    }
    proof.verify_and_deliver(&temp, &download_dir(), release_verify::release_public_key())
}

/// The release the app already fetched, shaped for gpui-updater-core: the
/// asset for this OS with the checksum and signature from its
/// [`ReleaseProof`] (already checked against the signed `SHA256SUMS`).
struct ReleaseSource {
    info: ReleaseInfo,
    asset: update::Asset,
    proof: ReleaseProof,
}

impl gpui_updater_core::UpdateSource for ReleaseSource {
    fn fetch_latest(&self) -> gpui_updater_core::Result<gpui_updater_core::Release> {
        use gpui_updater_core as up;
        let version = up::Version::parse(&self.info.version.to_string())
            .map_err(|e| up::Error::Parse(e.to_string()))?;
        let asset = &self.asset;
        Ok(up::Release::new(
            version,
            up::Asset::new(asset.name.clone(), asset.url.clone(), asset.size),
        )
        .with_sha256(self.proof.sha256.clone())
        .with_minisign_signature(self.proof.signature.clone()))
    }
}

/// Blocking: check, download, verify, install. Returns the relaunch path.
/// The engine checks the file under `Strict` (checksum and signature both
/// required), then the same [`ReleaseProof::verify_file`] as the Download
/// button runs on it before it is installed.
pub(crate) fn install_release(info: &ReleaseInfo) -> Result<Option<std::path::PathBuf>, String> {
    use gpui_updater_core as up;
    let asset = update::pick_installable(&info.assets, std::env::consts::OS)
        .cloned()
        .ok_or_else(|| "No update file for this operating system.".to_string())?;
    let proof = fetch_proof(info, &asset)?;
    let current =
        up::Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| up::Version::new(0, 0, 0));
    let engine = up::UpdateEngine::new(
        ReleaseSource {
            info: info.clone(),
            asset,
            proof: proof.clone(),
        },
        up::EngineConfig::new(current)
            .minisign_public_key(release_verify::release_public_key())
            .verification(up::Verification::Strict)
            .version_binding(up::VersionBinding::Required)
            .max_download_size(MAX_DOWNLOAD_BYTES),
    );
    let release = engine
        .check()
        .map_err(net_err)?
        .ok_or_else(|| "You're up to date.".to_string())?;
    let artifact = engine.download(&release, |_, _| {}).map_err(net_err)?;
    proof.verify_file(artifact.path(), release_verify::release_public_key())?;
    let installed = engine.install(&artifact).map_err(net_err)?;
    Ok(installed.restart_path)
}

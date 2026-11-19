//! Update check against GitHub Releases. At startup (at most once a day,
//! silent on failure) and from "Check for Updates" (menu / Settings). A
//! newer release shows a banner with its notes and a button that opens the
//! release page in the browser (the banner is one minimal line; a
//! "What's new" tab next to Home lists the full release notes). The tab
//! also has "Update sekarang": gpui-updater-core downloads the asset for
//! this OS, checks SHA256SUMS and the minisign signature (key in
//! release-key.pub), swaps the app in, then "Mulai ulang" relaunches.
//! The request is one anonymous GET of the public API: no identifiers.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::component::progress::Progress;
use gpui_kit::component::spinner::Spinner;
use ora_core::store::{self, UpdatePrefs};
use ora_core::update::{self, ReleaseInfo};

use super::*;

enum CheckDialog {
    Checking,
    UpToDate,
    Available,
    Failed(String),
}

pub(super) struct UpdateState {
    prefs: UpdatePrefs,
    checking: bool,
    check_generation: u64,
    dialog: Option<CheckDialog>,
    dialog_focus: gpui_kit::FocusHandle,
    previous_focus: Option<gpui_kit::FocusHandle>,
    /// Newest release, when newer than this build.
    available: Option<ReleaseInfo>,
    /// Banner visible (hidden by "Nanti" until the next newer version).
    banner: bool,
    /// "What's new" tab exists (next to Home) and is the shown page.
    tab_open: bool,
    tab_active: bool,
    installed_notes_selected: bool,
    installed_notes: Option<Result<ReleaseInfo, String>>,
    /// Result of the last manual check, shown in Settings.
    status: Option<String>,
    /// Download of the release file in progress / its outcome.
    downloading: bool,
    download_note: Option<(bool, String)>,
    /// In-app update running (download, verify, install).
    installing: bool,
    /// Update installed; relaunch path (None = relaunch the same path).
    staged: Option<Option<std::path::PathBuf>>,
}

impl UpdateState {
    pub(super) fn load(cx: &mut Context<Workbench>) -> Self {
        let prefs = store::load_update_prefs();
        set_http_proxy(prefs.proxy.clone());
        Self {
            prefs,
            checking: false,
            check_generation: 0,
            dialog: None,
            dialog_focus: cx.focus_handle(),
            previous_focus: None,
            available: None,
            banner: false,
            tab_open: false,
            tab_active: false,
            installed_notes_selected: false,
            installed_notes: None,
            status: None,
            downloading: false,
            download_note: None,
            installing: false,
            staged: None,
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Proxy URL chosen in Settings; `None` falls back to the `HTTPS_PROXY` /
/// `ALL_PROXY` environment variables (ureq's default).
static HTTP_PROXY: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub(super) fn set_http_proxy(url: Option<String>) {
    if let Ok(mut slot) = HTTP_PROXY.lock() {
        *slot = url.map(|u| u.trim().to_string()).filter(|u| !u.is_empty());
    }
}

/// `Ok(None)` keeps the default; a bad URL is reported instead of ignored.
fn configured_proxy() -> Result<Option<ureq::Proxy>, String> {
    let url = HTTP_PROXY.lock().ok().and_then(|slot| slot.clone());
    url.map(|u| ureq::Proxy::new(&u).map_err(|e| format!("Proxy tidak valid ({u}): {e}")))
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
        .user_agent(concat!("gpui-like-toad/", env!("CARGO_PKG_VERSION")))
        .build()
        .into())
}

/// Blocking fetch of the latest release; run on a background thread.
fn fetch_release(url: &str) -> Result<ReleaseInfo, String> {
    let agent = http_agent()?;
    let mut resp = agent
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?;
    let body = resp
        .body_mut()
        .with_config()
        .limit(2 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    update::parse_release(&body).ok_or_else(|| "Could not read the latest release.".to_string())
}

/// Where downloads go: `~/Downloads` when it exists, else the temp dir.
fn download_dir() -> std::path::PathBuf {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    home.map(|h| std::path::PathBuf::from(h).join("Downloads"))
        .filter(|d| d.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

/// Blocking download of one release asset; returns the saved path. The
/// file is written under a `.part` name and renamed when complete.
fn download_asset(asset: &update::Asset) -> Result<std::path::PathBuf, String> {
    use std::io::Write as _;
    const MAX_BYTES: u64 = 400 * 1024 * 1024;
    let proxy = configured_proxy()?;
    let mut builder = ureq::Agent::config_builder();
    if proxy.is_some() {
        builder = builder.proxy(proxy);
    }
    let agent: ureq::Agent = builder
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_body(Some(Duration::from_secs(60)))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .user_agent(concat!("gpui-like-toad/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut resp = agent.get(&asset.url).call().map_err(|e| e.to_string())?;
    let dir = download_dir();
    let target = dir.join(&asset.name);
    let part = dir.join(format!("{}.part", asset.name));
    let mut file = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut reader = resp.body_mut().with_config().limit(MAX_BYTES).reader();
    let copied = std::io::copy(&mut reader, &mut file).map_err(|e| e.to_string());
    let finish = copied.and_then(|_| file.flush().map_err(|e| e.to_string()));
    drop(file);
    if let Err(err) = finish {
        let _ = std::fs::remove_file(&part);
        return Err(err);
    }
    std::fs::rename(&part, &target).map_err(|e| e.to_string())?;
    Ok(target)
}

/// Base64 minisign public key (last line of `release-key.pub`).
fn release_public_key() -> &'static str {
    include_str!("../../../../release-key.pub")
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
}

/// The release the app already fetched, shaped for gpui-updater-core: the
/// asset for this OS, its `.minisig`, and its hash from `SHA256SUMS`.
struct ReleaseSource {
    info: ReleaseInfo,
}

impl gpui_updater_core::UpdateSource for ReleaseSource {
    fn fetch_latest(&self) -> gpui_updater_core::Result<gpui_updater_core::Release> {
        use gpui_updater_core as up;
        let info = &self.info;
        let asset = update::pick_installable(&info.assets, std::env::consts::OS).ok_or(
            up::Error::NoMatchingAsset {
                target_os: std::env::consts::OS,
                target_arch: std::env::consts::ARCH,
            },
        )?;
        let version = up::Version::parse(&info.version.to_string())
            .map_err(|e| up::Error::Parse(e.to_string()))?;
        let mut release = up::Release::new(
            version,
            up::Asset::new(asset.name.clone(), asset.url.clone(), asset.size),
        );
        let sig_name = format!("{}.minisig", asset.name);
        if let Some(sig) = info.assets.iter().find(|a| a.name == sig_name) {
            release = release.with_minisign_signature_url(sig.url.clone());
        }
        if let Some(sums) = info.assets.iter().find(|a| a.name == "SHA256SUMS") {
            let mut resp = http_agent()
                .map_err(up::Error::Http)?
                .get(&sums.url)
                .call()
                .map_err(|e| up::Error::Http(e.to_string()))?;
            let text = resp
                .body_mut()
                .with_config()
                .limit(1024 * 1024)
                .read_to_string()
                .map_err(|e| up::Error::Http(e.to_string()))?;
            if let Some(hash) = update::sha256_from_sums(&text, &asset.name) {
                release = release.with_sha256(hash);
            }
        }
        Ok(release)
    }
}

fn update_engine(info: &ReleaseInfo) -> gpui_updater_core::UpdateEngine<ReleaseSource> {
    use gpui_updater_core as up;
    let current =
        up::Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| up::Version::new(0, 0, 0));
    up::UpdateEngine::new(
        ReleaseSource { info: info.clone() },
        up::EngineConfig::new(current)
            .minisign_public_key(release_public_key())
            // Signature and checksum are both required: a release that
            // lacks either is refused, never installed unchecked.
            .verification(up::Verification::Strict),
    )
}

/// Blocking: check, download, verify, install. Returns the relaunch path.
fn install_release(info: &ReleaseInfo) -> Result<Option<std::path::PathBuf>, String> {
    let engine = update_engine(info);
    match engine.update_now(|_, _| {}) {
        Ok(Some(installed)) => Ok(installed.restart_path),
        Ok(None) => Err("Sudah versi terbaru.".to_string()),
        Err(e) => Err(e.to_string()),
    }
}

impl Workbench {
    /// Download the file for this OS, then show it in the file manager.
    fn download_update(&mut self, cx: &mut Context<Self>) {
        let u = &mut self.updates;
        if u.downloading {
            return;
        }
        let Some(asset) = u
            .available
            .as_ref()
            .and_then(|r| update::pick_asset(&r.assets, std::env::consts::OS))
            .cloned()
        else {
            u.download_note = Some((
                true,
                "Tidak ada file untuk OS ini; buka halaman rilis.".into(),
            ));
            cx.notify();
            return;
        };
        u.downloading = true;
        u.download_note = Some((false, format!("Mengunduh {}...", asset.name)));
        let task = cx
            .background_executor()
            .spawn(async move { download_asset(&asset) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                this.updates.downloading = false;
                this.updates.download_note = Some(match result {
                    Ok(path) => {
                        cx.reveal_path(&path);
                        (false, format!("Tersimpan di {}", path.display()))
                    }
                    Err(err) => (true, format!("Gagal mengunduh: {err}")),
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Download, verify and install the update in place.
    fn install_update(&mut self, cx: &mut Context<Self>) {
        let u = &mut self.updates;
        if u.installing || u.downloading {
            return;
        }
        let Some(info) = u.available.clone() else {
            return;
        };
        u.installing = true;
        u.download_note = Some((false, format!("Mengunduh dan memasang {}...", info.version)));
        let task = cx
            .background_executor()
            .spawn(async move { install_release(&info) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                this.updates.installing = false;
                this.updates.download_note = Some(match result {
                    Ok(path) => {
                        this.updates.staged = Some(path);
                        (
                            false,
                            "Update terpasang. Mulai ulang untuk memakainya.".into(),
                        )
                    }
                    Err(err) => (true, format!("Update gagal: {err}")),
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Relaunch into the installed update.
    fn restart_into_update(&mut self, cx: &mut Context<Self>) {
        if let Some(Some(path)) = self.updates.staged.clone() {
            cx.set_restart_path(path);
        }
        cx.restart();
    }

    /// Startup check: only if enabled and not checked within the interval.
    pub fn auto_check_updates(&mut self, cx: &mut Context<Self>) {
        // Windows: delete the `.old.exe` a previous update left behind.
        #[cfg(target_os = "windows")]
        if let Ok(root) = gpui_updater_core::current_install_root() {
            gpui_updater_core::install::remove_leftovers(&root);
        }
        self.show_installed_update(cx);
        let u = &self.updates;
        if update::should_auto_check(u.prefs.auto_check, u.prefs.last_checked, now_secs()) {
            self.check_updates(cx);
        }
    }

    fn show_installed_update(&mut self, cx: &mut Context<Self>) {
        let current_text = env!("CARGO_PKG_VERSION");
        let Some(current) = update::Version::parse(current_text) else {
            return;
        };
        let previous = self.updates.prefs.last_launched_version.as_deref();
        if previous == Some(current_text) {
            return;
        }
        let upgraded = previous.and_then(update::Version::parse).map_or_else(
            // Legacy builds did not record their running version. A prior
            // check distinguishes an existing installation from a clean one.
            || self.updates.prefs.last_checked != 0,
            |version| version < current,
        );
        self.updates.prefs.last_launched_version = Some(current_text.to_string());
        self.save_update_prefs();
        if !upgraded {
            return;
        }
        self.updates.installed_notes_selected = true;
        self.open_whats_new();

        let task = cx.background_executor().spawn(async move {
            let url = format!(
                "https://api.github.com/repos/abu-abbas/gpui-like-toad/releases/tags/v{current}"
            );
            let release = fetch_release(&url)?;
            if release.version == current {
                Ok(release)
            } else {
                Err("The release does not match the installed version.".into())
            }
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                this.updates.installed_notes = Some(result);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn open_update_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let u = &mut self.updates;
        if u.dialog.is_none() {
            u.previous_focus = window.focused(cx);
        }
        u.dialog = Some(CheckDialog::Checking);
        u.status = None;
        window.focus(&u.dialog_focus, cx);
        self.check_updates(cx);
        cx.notify();
    }

    fn check_updates(&mut self, cx: &mut Context<Self>) {
        if self.updates.checking {
            return;
        }
        self.updates.checking = true;
        self.updates.check_generation += 1;
        let generation = self.updates.check_generation;
        let task = cx
            .background_executor()
            .spawn(async move { fetch_release(update::LATEST_RELEASE_API) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                if this.updates.check_generation == generation {
                    this.finish_update_check(result);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn finish_update_check(&mut self, result: Result<ReleaseInfo, String>) {
        let u = &mut self.updates;
        u.checking = false;
        let manual = matches!(u.dialog, Some(CheckDialog::Checking));
        match result {
            Err(err) => {
                if manual {
                    u.status = Some(format!("Could not check for updates. {err}"));
                    u.dialog = Some(CheckDialog::Failed(err));
                }
            }
            Ok(release) => {
                u.prefs.last_checked = now_secs();
                match update::newer_than(release, env!("CARGO_PKG_VERSION")) {
                    Some(r) => {
                        let dismissed = u.prefs.dismissed_version.as_deref() == Some(&r.tag);
                        u.status = Some(format!("Version {} is available.", r.version));
                        u.banner = manual || !dismissed;
                        u.tab_open |= u.banner;
                        u.available = Some(r);
                        if manual {
                            u.dialog = Some(CheckDialog::Available);
                        }
                    }
                    None => {
                        u.available = None;
                        u.banner = false;
                        if manual {
                            u.status = Some("You're up to date.".to_string());
                            u.dialog = Some(CheckDialog::UpToDate);
                        }
                    }
                }
                self.save_update_prefs();
            }
        }
    }

    fn close_update_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let u = &mut self.updates;
        if matches!(u.dialog.take(), Some(CheckDialog::Checking)) {
            // The blocking HTTP request can finish on its thread. Ignore its
            // response so a later check owns the result and the UI.
            u.checking = false;
            u.check_generation += 1;
        }
        let focus = u
            .previous_focus
            .take()
            .unwrap_or_else(|| self.focus_handle.clone());
        window.focus(&focus, cx);
        cx.notify();
    }

    pub(super) fn update_check_open(&self) -> bool {
        self.updates.dialog.is_some()
    }

    pub(super) fn render_update_check(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let u = &self.updates;
        let dialog = u.dialog.as_ref()?;
        let checking = matches!(dialog, CheckDialog::Checking);
        let available = matches!(dialog, CheckDialog::Available);
        let version = env!("CARGO_PKG_VERSION");
        let (title, detail) = match dialog {
            CheckDialog::Checking => ("Checking for updates…", None),
            CheckDialog::UpToDate => (
                "You're up to date",
                Some(format!("{} {version} is the latest version.", app_name!())),
            ),
            CheckDialog::Available => (
                "Update available",
                u.available.as_ref().map(|release| {
                    format!(
                        "Version {} is available. You're running {version}.",
                        release.version
                    )
                }),
            ),
            CheckDialog::Failed(err) => ("Couldn't check for updates", Some(err.clone())),
        };
        Some(
            div()
                .id("update-check-overlay")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .justify_center()
                .items_center()
                .bg(gpui_kit::black().opacity(0.25))
                .child(
                    div()
                        .id("update-check-dialog")
                        .track_focus(&u.dialog_focus)
                        .on_key_down(cx.listener(
                            |this, event: &gpui_kit::KeyDownEvent, window, cx| {
                                if event.keystroke.key == "escape" {
                                    cx.stop_propagation();
                                    this.close_update_check(window, cx);
                                }
                            },
                        ))
                        .flex()
                        .flex_col()
                        .w_full()
                        .max_w(px(380.))
                        .rounded(px(RADIUS_MD))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .shadow_lg()
                        .gap(px(SPACE_3))
                        .p(px(SPACE_4))
                        .child(
                            div()
                                .flex()
                                .gap(px(SPACE_3))
                                .items_start()
                                .child(crate::about::app_icon(40.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .flex()
                                        .flex_col()
                                        .gap(px(SPACE_2))
                                        .child(
                                            div()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(theme.popover_foreground)
                                                .child(title),
                                        )
                                        .children(detail.map(|text| body_text(text, theme)))
                                        .when(checking, |content| {
                                            content.child(
                                                Progress::new("check-update-progress")
                                                    .loading(true)
                                                    .small()
                                                    .accessibility_label("Checking for updates"),
                                            )
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(SPACE_2))
                                .child(
                                    Button::new("update-check-close")
                                        .small()
                                        .label(if checking || available {
                                            "Cancel"
                                        } else {
                                            "OK"
                                        })
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.close_update_check(window, cx)
                                        })),
                                )
                                .when(available, |actions| {
                                    actions.child(
                                        Button::new("update-check-details")
                                            .small()
                                            .primary()
                                            .label("View Update")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.close_update_check(window, cx);
                                                if this.settings.open {
                                                    this.close_settings(window, cx);
                                                }
                                                this.open_available_update();
                                                cx.notify();
                                            })),
                                    )
                                }),
                        ),
                )
                .into_any_element(),
        )
    }

    fn save_update_prefs(&mut self) {
        if let Err(err) = store::save_update_prefs(&self.updates.prefs) {
            self.push_message(format!("Gagal menyimpan pengaturan update: {err}"));
        }
    }

    pub(super) fn set_auto_check_updates(&mut self, on: bool) {
        self.updates.prefs.auto_check = on;
        self.save_update_prefs();
    }

    /// Settings > Updates > Proxy: apply and remember the proxy URL (blank =
    /// use the system environment variables).
    pub(super) fn set_proxy(&mut self, text: &str) {
        let url = Some(text.trim().to_string()).filter(|u| !u.is_empty());
        if url == self.updates.prefs.proxy {
            return;
        }
        set_http_proxy(url.clone());
        self.updates.prefs.proxy = url;
        self.save_update_prefs();
    }

    fn dismiss_update_banner(&mut self) {
        self.updates.banner = false;
        if let Some(r) = &self.updates.available {
            self.updates.prefs.dismissed_version = Some(r.tag.clone());
            self.save_update_prefs();
        }
    }

    /// Settings > Updates section.
    pub(super) fn render_update_settings(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let u = &self.updates;
        let mut section = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_2))
            .child(form_label("Updates", theme))
            .child(
                Checkbox::new("auto-check-updates")
                    .label("Cek update otomatis saat start")
                    .checked(u.prefs.auto_check)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.set_auto_check_updates(*checked);
                        cx.notify();
                    })),
            )
            .child(
                div().flex().flex_row().child(
                    Button::new("check-updates")
                        .label("Check for Updates")
                        .icon(IconName::RefreshCw)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_update_check(window, cx)),
                        ),
                ),
            );
        if let Some(status) = &u.status {
            section = section.child(caption_text(status.clone(), theme));
        }
        section = section
            .child(div().h(px(SPACE_2)))
            .child(form_label("Proxy", theme))
            .child(Input::new(&self.settings.proxy_input).small().w_full())
            .child(caption_text(
                "Isi kalau jaringan kantor harus lewat proxy, contoh http://proxy.kantor:8080 \
                 (bisa http://user:pass@host:port). Kosong = pakai HTTPS_PROXY / ALL_PROXY \
                 dari environment. Berlaku untuk cek dan unduh update."
                    .to_string(),
                theme,
            ));
        section.child(caption_text(
            "Cek ke GitHub Releases (maksimal sehari sekali); tidak ada data kamu yang dikirim."
                .to_string(),
            theme,
        ))
    }

    pub(super) fn whats_new_active(&self) -> bool {
        self.updates.tab_open && self.updates.tab_active
    }

    pub(super) fn show_whats_new_tab(&self) -> bool {
        self.updates.tab_open
    }

    pub(super) fn leave_whats_new(&mut self) {
        self.updates.tab_active = false;
    }

    fn open_whats_new(&mut self) {
        self.updates.tab_open = true;
        self.updates.tab_active = true;
        // Suspend query shortcuts while release notes are active.
        self.show_empty_workspace = true;
    }

    fn open_available_update(&mut self) {
        self.updates.installed_notes_selected = false;
        self.open_whats_new();
    }

    /// Minimal one-line banner above the whole window when a newer release
    /// exists; clicking the text or "Lihat detail" opens the popup.
    pub(super) fn render_update_banner(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let u = &self.updates;
        let release = u.available.as_ref().filter(|_| u.banner)?;
        let text = format!(
            "Versi {} tersedia (kamu pakai {}).",
            release.version,
            env!("CARGO_PKG_VERSION")
        );
        Some(
            div()
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(SPACE_2))
                .h(px(TOOLBAR_HEIGHT))
                .px(px(SPACE_3))
                .bg(theme.accent)
                .border_b_1()
                .border_color(theme.border)
                .child(icon(IconName::Download, 14., theme.primary))
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .text_size(crate::theme::ui_size(&theme, FONT_LABEL))
                        .line_height(crate::theme::ui_size(&theme, LEADING_LABEL))
                        .text_color(theme.accent_foreground)
                        .child(text),
                )
                .child(
                    Button::new("update-details")
                        .label("Lihat detail")
                        .primary()
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.open_available_update();
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("update-dismiss")
                        .icon(IconName::X)
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.dismiss_update_banner();
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    /// The "What's new" tab button, placed right after Home.
    pub(super) fn render_whats_new_tab(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let active = self.whats_new_active();
        let mut container = div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .h(px(28.))
            .rounded(px(RADIUS_MD));
        container = if active {
            container
                .bg(theme.tab_active)
                .border_1()
                .border_color(theme.border)
        } else {
            container.hover(|this| this.bg(theme.secondary))
        };
        let color = if active {
            theme.tab_active_foreground
        } else {
            theme.tab_foreground
        };
        container
            .child(
                div()
                    .id("tab-whats-new")
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(relative(1.0))
                    .gap(px(SPACE_1))
                    .pl(px(SPACE_2))
                    .pr(px(SPACE_1))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.open_whats_new();
                        cx.notify();
                    }))
                    .child(icon(
                        IconName::Sparkles,
                        14.,
                        if active {
                            theme.primary
                        } else {
                            theme.muted_foreground
                        },
                    ))
                    .child(
                        div()
                            .text_size(crate::theme::ui_size(&theme, FONT_LABEL))
                            .line_height(crate::theme::ui_size(&theme, LEADING_LABEL))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(color)
                            .child("What's new"),
                    ),
            )
            .child(
                div()
                    .id("tab-whats-new-close")
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(20.))
                    .w(px(20.))
                    .mr(px(SPACE_1))
                    .rounded(px(RADIUS_SM))
                    .hover(|this| this.bg(theme.danger).text_color(theme.danger_foreground))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.updates.tab_open = false;
                        this.updates.tab_active = false;
                        cx.notify();
                    }))
                    .child(icon(IconName::X, 12., theme.muted_foreground)),
            )
            .into_any_element()
    }

    /// The What's new page: every note of the release, plus the button
    /// that opens the release page in the browser.
    pub(super) fn render_whats_new_page(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let mut page = div()
            .id("whats-new-page")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .gap(px(SPACE_2))
            .p(px(SPACE_6));
        if self.updates.installed_notes_selected {
            let current = env!("CARGO_PKG_VERSION");
            let url =
                format!("https://github.com/abu-abbas/gpui-like-toad/releases/tag/v{current}");
            return page
                .child(
                    div()
                        .text_size(theme::ui_size(theme, FONT_HEADING_XL))
                        .line_height(theme::ui_size(theme, LEADING_HEADING_XL))
                        .child(format!("What's new in {current}")),
                )
                .child(
                    div().flex().child(
                        Button::new("installed-release-page")
                            .small()
                            .label("Open release page")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
                )
                .children(match &self.updates.installed_notes {
                    Some(Ok(release)) if !release.notes.trim().is_empty() => release
                        .notes
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .map(|line| body_text(line.to_string(), theme))
                        .collect::<Vec<_>>(),
                    Some(Ok(_)) => vec![caption_text(
                        "No release notes were published.".into(),
                        theme,
                    )],
                    Some(Err(_)) => {
                        vec![caption_text("Couldn't load release notes.".into(), theme)]
                    }
                    None => vec![caption_text("Loading release notes…".into(), theme)],
                })
                .into_any_element();
        }
        let Some(release) = self.updates.available.as_ref() else {
            return page
                .child(
                    div()
                        .text_size(crate::theme::ui_size(&theme, FONT_HEADING_XL))
                        .line_height(crate::theme::ui_size(&theme, LEADING_HEADING_XL))
                        .child("What's new"),
                )
                .child(caption_text(
                    format!(
                        "Kamu memakai versi terbaru ({}).",
                        env!("CARGO_PKG_VERSION")
                    ),
                    theme,
                ))
                .into_any_element();
        };
        let url = release.url.clone();
        let can_download = update::pick_asset(&release.assets, std::env::consts::OS).is_some();
        let can_install = update::pick_installable(&release.assets, std::env::consts::OS).is_some();
        let staged = self.updates.staged.is_some();
        let installing = self.updates.installing;
        let downloading = self.updates.downloading || installing;
        let note = self.updates.download_note.clone();
        page = page
            .child(
                div()
                    .text_size(crate::theme::ui_size(&theme, FONT_HEADING_XL))
                    .line_height(crate::theme::ui_size(&theme, LEADING_HEADING_XL))
                    .child(format!("What's new di {}", release.version)),
            )
            .child(caption_text(
                format!("Kamu memakai {}.", env!("CARGO_PKG_VERSION")),
                theme,
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(SPACE_2))
                    .py(px(SPACE_2))
                    .when(staged, |row| {
                        row.child(
                            Button::new("update-restart")
                                .label("Mulai ulang")
                                .icon(IconName::Play)
                                .primary()
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.restart_into_update(cx)),
                                ),
                        )
                    })
                    .when(can_install && !staged, |row| {
                        row.child(
                            Button::new("update-install")
                                .label("Update sekarang")
                                .icon(IconName::Download)
                                .primary()
                                .loading(installing)
                                .disabled(downloading)
                                .on_click(cx.listener(|this, _, _, cx| this.install_update(cx))),
                        )
                    })
                    .when(can_download && !staged, |row| {
                        row.child({
                            let b = Button::new("update-download")
                                .label("Download")
                                .icon(IconName::Download)
                                .disabled(downloading)
                                .on_click(cx.listener(|this, _, _, cx| this.download_update(cx)));
                            if can_install {
                                b
                            } else {
                                b.primary()
                            }
                        })
                    })
                    .child(
                        Button::new("update-open")
                            .label("Buka halaman rilis")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
            );
        if let Some((is_error, text)) = note {
            let line = if is_error {
                div()
                    .text_size(crate::theme::ui_size(&theme, FONT_CAPTION))
                    .line_height(crate::theme::ui_size(&theme, LEADING_CAPTION))
                    .text_color(theme.danger)
                    .child(text)
            } else {
                caption_text(text, theme)
            };
            // A spinner next to the note while the update downloads/installs.
            page = page.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2))
                    .when(downloading, |row| {
                        row.child(Spinner::new().small().color(theme.primary))
                    })
                    .child(line),
            );
        }
        let notes = release.notes.trim();
        if notes.is_empty() {
            page = page.child(empty_state_text(
                "Tidak ada catatan rilis.".to_string(),
                theme,
            ));
        }
        for line in notes.lines().filter(|l| !l.trim().is_empty()) {
            page = page.child(body_text(line.to_string(), theme));
        }
        page.into_any_element()
    }
}

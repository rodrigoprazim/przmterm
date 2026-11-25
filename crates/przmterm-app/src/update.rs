//! Checking for a newer release: asks GitHub for the project's releases, in
//! the background, and says when there is one newer than this build, with
//! its notes. GitHub's API answers only 60 questions an hour from one
//! address, which a company network shares: past that, the releases' feed
//! and files on github.com, which have no such limit, answer instead. When the user asks, [`Download`] fetches the release for this
//! system, checks it against the checksums signed with the release key
//! ([`crate::openpgp`]) and installs it in place of this copy, or, when it
//! cannot, leaves it in Downloads and opens it.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const RELEASES: &str = "https://api.github.com/repos/rodrigoprazim/przmterm/releases?per_page=20";
/// The releases' Atom feed: no rate limit, no drafts.
const FEED: &str = "https://github.com/rodrigoprazim/przmterm/releases.atom";
const REPO_PAGE: &str = "https://github.com/rodrigoprazim/przmterm";
const RAW: &str = "https://raw.githubusercontent.com/rodrigoprazim/przmterm";

/// A release newer than this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub version: String,
    /// The release page.
    pub url: String,
    /// What is new (Markdown, from the changelog).
    pub notes: String,
    /// The file for this system, if the release has one.
    pub asset: Option<Asset>,
    /// `SHA256SUMS.txt` and its signature.
    pub sums: Option<(String, String)>,
}

/// A file of a release.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Asset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub url: String,
    #[serde(default)]
    pub size: u64,
}

/// The end of the Windows setup program's name.
const SETUP_SUFFIX: &str = "-windows-x86_64-setup.exe";

/// The ends of the release file's name for this system, the best first.
fn asset_suffixes() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &["-macos-universal.dmg"]
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        if installed_by_setup() {
            &[SETUP_SUFFIX, "-windows-x86_64.zip"]
        } else {
            &["-windows-x86_64.zip"]
        }
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        if installed_by_deb() {
            &[DEB_SUFFIX, "-linux-x86_64.tar.gz"]
        } else {
            &["-linux-x86_64.tar.gz"]
        }
    } else {
        &[]
    }
}

/// The folder this program runs from.
fn exe_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    Some(exe.parent()?.to_path_buf())
}

/// Windows: this copy was installed by the setup program, whose
/// uninstaller lies next to it.
fn installed_by_setup() -> bool {
    cfg!(windows) && exe_dir().is_some_and(|d| d.join("unins000.exe").is_file())
}

/// The end of the Debian / Ubuntu package's name.
const DEB_SUFFIX: &str = "-linux-x86_64.deb";

/// Linux: this copy came from the `.deb` (dpkg knows the package and the
/// program is in /usr/bin).
fn installed_by_deb() -> bool {
    cfg!(target_os = "linux")
        && exe_dir().is_some_and(|d| d == Path::new("/usr/bin"))
        && Path::new("/var/lib/dpkg/info/przmterm.list").is_file()
}

/// Whether `file` is a Debian package.
fn is_deb(file: &Path) -> bool {
    file.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(DEB_SUFFIX))
}

/// Whether `program` is a setup program to run rather than PrzmTerm itself.
pub fn is_setup(program: &Path) -> bool {
    program
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(SETUP_SUFFIX))
}

/// What a check came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    UpToDate,
    Newer(Found),
    Failed(String),
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<Asset>,
}

/// The newest of `releases` above `current`. Pre-releases count while this
/// build is one (any 0.x, -dev or -rc): someone on 0.9 hears of 1.0.0-rc.1,
/// someone on 1.0.0 does not.
fn newest(current: &str, releases: &[Release]) -> Option<Found> {
    let current = Version::parse(current).ok()?;
    let early = current.major == 0 || !current.pre.is_empty();
    releases
        .iter()
        .filter(|r| !r.draft && (early || !r.prerelease))
        .filter_map(|r| {
            let version = Version::parse(r.tag_name.trim_start_matches('v')).ok()?;
            (early || version.pre.is_empty()).then_some((version, r))
        })
        .filter(|(version, _)| version.cmp_precedence(&current).is_gt())
        .max_by(|(a, _), (b, _)| a.cmp_precedence(b))
        .map(|(version, r)| {
            let named = |name: &str| r.assets.iter().find(|a| a.name == name);
            Found {
                version: version.to_string(),
                url: r.html_url.clone(),
                notes: r.body.clone().unwrap_or_default().trim().to_string(),
                asset: asset_suffixes()
                    .iter()
                    .find_map(|suffix| r.assets.iter().find(|a| a.name.ends_with(suffix)).cloned()),
                sums: named("SHA256SUMS.txt")
                    .zip(named("SHA256SUMS.txt.asc"))
                    .map(|(sums, sig)| (sums.url.clone(), sig.url.clone())),
            }
        })
}

fn agent(timeout: Option<Duration>) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(timeout)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .proxy(ureq::Proxy::try_from_env())
        .user_agent(concat!("PrzmTerm/", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent()
}

fn ask_github() -> Outcome {
    let current = env!("CARGO_PKG_VERSION");
    match ask_api() {
        Ok(releases) => match newest(current, &releases) {
            Some(found) => Outcome::Newer(found),
            None => Outcome::UpToDate,
        },
        Err(api) => {
            tracing::warn!("update check: GitHub's API: {api}; trying the releases' feed");
            match ask_feed(current) {
                Ok(Some(found)) => Outcome::Newer(found),
                Ok(None) => Outcome::UpToDate,
                Err(feed) => {
                    tracing::warn!("update check: the releases' feed: {feed}");
                    Outcome::Failed(api)
                }
            }
        }
    }
}

/// The releases, from GitHub's API. When it refuses, why: its message
/// and, past the hourly limit, when it answers again.
fn ask_api() -> Result<Vec<Release>, String> {
    let mut response = agent(Some(Duration::from_secs(15)))
        .get(RELEASES)
        .header("Accept", "application/vnd.github+json")
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .map_err(|e| e.to_string())?;
    let status = response.status();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let remaining = header("x-ratelimit-remaining");
    let reset = header("x-ratelimit-reset");
    tracing::debug!(
        "update check: GitHub's API answered {status} ({} questions left this hour)",
        remaining.as_deref().unwrap_or("?")
    );
    if status.is_success() {
        return response
            .body_mut()
            .read_json::<Vec<Release>>()
            .map_err(|e| e.to_string());
    }
    #[derive(Deserialize)]
    struct Refusal {
        message: String,
    }
    let message = response
        .body_mut()
        .with_config()
        .limit(64 << 10)
        .read_json::<Refusal>()
        .map(|r| r.message)
        .unwrap_or_default();
    let mut why = format!("GitHub answered {status}");
    if remaining.as_deref() == Some("0") {
        why.push_str(": no more questions from this network this hour");
        if let Some(at) = reset
            .and_then(|r| r.parse::<i64>().ok())
            .and_then(|r| chrono::DateTime::from_timestamp(r, 0))
        {
            let at = at.with_timezone(&chrono::Local);
            why.push_str(&format!(" (until {})", at.format("%H:%M")));
        }
    }
    if !message.is_empty() {
        why.push_str(&format!(" — {message}"));
    }
    Err(why)
}

fn fetch_text(url: &str) -> Result<String, String> {
    agent(Some(Duration::from_secs(15)))
        .get(url)
        .call()
        .and_then(|mut r| r.body_mut().with_config().limit(4 << 20).read_to_string())
        .map_err(|e| format!("{url}: {e}"))
}

/// The newest release above `current` from the releases' feed, its files
/// from its checksums and its notes from the changelog it was made from.
fn ask_feed(current: &str) -> Result<Option<Found>, String> {
    let feed = fetch_text(FEED)?;
    let tags = feed_tags(&feed);
    if tags.is_empty() {
        return Err("no releases in the feed".into());
    }
    let listed: Vec<Release> = tags
        .iter()
        .map(|tag| feed_release(tag, &[], None))
        .collect();
    let Some(found) = newest(current, &listed) else {
        return Ok(None);
    };
    let tag = tags
        .iter()
        .find(|t| t.trim_start_matches('v') == found.version)
        .ok_or("the release's tag is gone")?;
    let download = format!("{REPO_PAGE}/releases/download/{tag}");
    let files = sums_files(&fetch_text(&format!("{download}/SHA256SUMS.txt"))?);
    let notes = fetch_text(&format!("{RAW}/{tag}/CHANGELOG.md"))
        .ok()
        .and_then(|changelog| changelog_section(&changelog, &found.version));
    let release = feed_release(tag, &files, notes);
    Ok(newest(current, std::slice::from_ref(&release)))
}

/// A release as the feed and its files tell it.
fn feed_release(tag: &str, files: &[String], notes: Option<String>) -> Release {
    let version = tag.trim_start_matches('v');
    let download = format!("{REPO_PAGE}/releases/download/{tag}");
    Release {
        tag_name: tag.to_string(),
        html_url: format!("{REPO_PAGE}/releases/tag/{tag}"),
        draft: false,
        prerelease: Version::parse(version).is_ok_and(|v| !v.pre.is_empty()),
        body: notes,
        assets: files
            .iter()
            .chain(
                [
                    "SHA256SUMS.txt".to_string(),
                    "SHA256SUMS.txt.asc".to_string(),
                ]
                .iter(),
            )
            .map(|name| Asset {
                name: name.clone(),
                url: format!("{download}/{name}"),
                size: 0,
            })
            .collect(),
    }
}

/// The tags of the releases in an Atom feed of GitHub's.
fn feed_tags(feed: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for part in feed.split("/releases/tag/").skip(1) {
        let tag: String = part
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
            .collect();
        if !tag.is_empty() && !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags
}

/// The file names in `SHA256SUMS.txt` (`sha256sum` lines).
fn sums_files(sums: &str) -> Vec<String> {
    sums.lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .map(|name| name.trim_start_matches('*').to_string())
        .collect()
}

/// The changelog's section of `version`, as the release's notes are made.
fn changelog_section(changelog: &str, version: &str) -> Option<String> {
    let head = format!("## [{version}]");
    let start = changelog.lines().position(|l| l.starts_with(&head))?;
    let section: Vec<&str> = changelog
        .lines()
        .skip(start + 1)
        .take_while(|l| !l.starts_with("## ["))
        .collect();
    let text = section.join("\n").trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// A check running in the background, or done.
#[derive(Clone, Default)]
pub struct Check {
    outcome: Arc<Mutex<Option<Outcome>>>,
}

impl Check {
    /// Starts checking; `ctx` is repainted when the answer comes.
    pub fn start(ctx: &egui::Context) -> Self {
        let check = Self::default();
        let (slot, ctx) = (check.outcome.clone(), ctx.clone());
        std::thread::Builder::new()
            .name("update-check".into())
            .spawn(move || {
                let outcome = ask_github();
                match &outcome {
                    Outcome::UpToDate => tracing::info!("update check: up to date"),
                    Outcome::Newer(found) => {
                        tracing::info!("update check: {} available", found.version);
                    }
                    Outcome::Failed(e) => tracing::warn!("update check failed: {e}"),
                }
                if let Ok(mut slot) = slot.lock() {
                    *slot = Some(outcome);
                }
                ctx.request_repaint();
            })
            .ok();
        check
    }

    /// The answer, once it came.
    pub fn outcome(&self) -> Option<Outcome> {
        self.outcome.lock().ok()?.clone()
    }
}

/// Where a release can be installed: this copy, when it is an installed
/// release (not a development build) whose files can be replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// macOS: the `PrzmTerm.app` this runs from.
    Bundle(PathBuf),
    /// Windows and Linux: the folder of `przmterm` and `przm`.
    Folder(PathBuf),
    /// Windows: the folder the setup program installed into; the new
    /// setup program installs over it.
    Setup(PathBuf),
    /// Linux: the `.deb`'s /usr/bin; the new package installs over it
    /// (`pkexec dpkg -i`, which asks for an administrator's password).
    Deb(PathBuf),
}

/// Where this copy would be updated, or `None` to only download: copies
/// built by hand (`cargo build`) are never replaced, only release builds
/// (the release workflow sets `PRZMTERM_RELEASE_BUILD`).
pub fn place() -> Option<Place> {
    option_env!("PRZMTERM_RELEASE_BUILD")?;
    let dir = exe_dir()?;
    if cfg!(target_os = "macos") {
        // …/PrzmTerm.app/Contents/MacOS/przmterm
        let bundle = dir.parent()?.parent()?;
        (bundle.extension()? == "app").then(|| Place::Bundle(bundle.to_path_buf()))
    } else if installed_by_setup() {
        Some(Place::Setup(dir))
    } else if installed_by_deb() {
        Some(Place::Deb(dir))
    } else {
        Some(Place::Folder(dir))
    }
}

/// How far a download got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    Fetching {
        done: u64,
        total: u64,
    },
    Installing,
    /// Installed: start the new version from here. On Windows, when the
    /// setup program installed this copy, the new setup program (see
    /// [`is_setup`]), which installs it once this copy has quit.
    Installed(PathBuf),
    /// Not installed (a development build, or no right to replace the
    /// files): the checked file, left in Downloads and opened.
    Saved {
        file: PathBuf,
        why: Option<String>,
    },
    Failed(String),
    Cancelled,
}

/// A release being downloaded, checked and installed in the background.
pub struct Download {
    pub version: String,
    stage: Arc<Mutex<Stage>>,
    cancel: Arc<AtomicBool>,
}

impl Download {
    pub fn start(ctx: &egui::Context, found: &Found) -> Self {
        let stage = Arc::new(Mutex::new(Stage::Fetching { done: 0, total: 0 }));
        let cancel = Arc::new(AtomicBool::new(false));
        let version = found.version.clone();
        let (found, ctx2) = (found.clone(), ctx.clone());
        let (slot, stop) = (stage.clone(), cancel.clone());
        std::thread::Builder::new()
            .name("update-download".into())
            .spawn(move || {
                let set = |s: Stage| {
                    if let Ok(mut slot) = slot.lock() {
                        *slot = s;
                    }
                    ctx2.request_repaint();
                };
                let end = match fetch_and_install(&found, &stop, &set) {
                    Ok(stage) => stage,
                    Err(_) if stop.load(Ordering::Relaxed) => Stage::Cancelled,
                    Err(e) => Stage::Failed(e),
                };
                set(end);
            })
            .ok();
        Self {
            version,
            stage,
            cancel,
        }
    }

    pub fn stage(&self) -> Stage {
        self.stage
            .lock()
            .map(|s| s.clone())
            .unwrap_or(Stage::Failed("lost track of the download".into()))
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn fetch_and_install(
    found: &Found,
    stop: &AtomicBool,
    set: &dyn Fn(Stage),
) -> Result<Stage, String> {
    let asset = found
        .asset
        .as_ref()
        .ok_or("this release has no file for this system")?;
    let (sums_url, sig_url) = found
        .sums
        .as_ref()
        .ok_or("this release has no signed checksums")?;
    let net = agent(None);
    let small = |url: &str| -> Result<String, String> {
        net.get(url)
            .call()
            .and_then(|mut r| r.body_mut().with_config().limit(1 << 20).read_to_string())
            .map_err(|e| e.to_string())
    };
    // The checksums first: nothing is kept unless the release key signed
    // them and the file matches.
    let sums = small(sums_url)?;
    let signature = small(sig_url)?;
    crate::openpgp::verify(crate::openpgp::RELEASE_KEY, sums.as_bytes(), &signature)
        .map_err(|e| format!("the checksums are not signed by the release key: {e}"))?;
    let expected = expected_sum(&sums, &asset.name)
        .ok_or_else(|| format!("{} is not in the signed checksums", asset.name))?;

    let work = std::env::temp_dir().join(format!("przmterm-update-{}", found.version));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let file = work.join(&asset.name);
    let mut response = net.get(&asset.url).call().map_err(|e| e.to_string())?;
    let total = response
        .body()
        .content_length()
        .unwrap_or(asset.size)
        .max(1);
    let mut reader = response.body_mut().with_config().limit(1 << 32).reader();
    let mut out = std::fs::File::create(&file).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let (mut done, mut buf) = (0u64, vec![0u8; 64 * 1024]);
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        hasher.update(&buf[..n]);
        done += n as u64;
        set(Stage::Fetching { done, total });
    }
    out.flush().map_err(|e| e.to_string())?;
    drop(out);
    let got = hex(&hasher.finalize());
    if got != expected {
        let _ = std::fs::remove_dir_all(&work);
        return Err(format!(
            "the download does not match the signed checksum ({got} instead of {expected})"
        ));
    }

    set(Stage::Installing);
    let why = match place() {
        None => Some("this is not an installed release".to_string()),
        Some(place) => match install(&file, &place, &work) {
            Ok(start) => {
                let _ = std::fs::remove_dir_all(&work);
                return Ok(Stage::Installed(start));
            }
            Err(e) => Some(e),
        },
    };
    // Kept for the user to install by hand.
    let downloads = dirs::download_dir()
        .or_else(dirs::home_dir)
        .ok_or("no Downloads folder")?;
    let saved = downloads.join(&asset.name);
    std::fs::copy(&file, &saved).map_err(|e| format!("{}: {e}", saved.display()))?;
    let _ = std::fs::remove_dir_all(&work);
    if cfg!(target_os = "macos") {
        // Opening the disk image shows PrzmTerm.app to drag to Applications.
        let _ = std::process::Command::new("open").arg(&saved).spawn();
    } else if is_deb(&saved) {
        // The system's software installer opens it.
        let _ = crate::desktop::open_text(&saved);
    } else {
        let _ = crate::desktop::reveal(&saved);
    }
    Ok(Stage::Saved { file: saved, why })
}

/// The SHA-256 of `name` in a `sha256sum` listing.
fn expected_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (sum, file) = line.split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then(|| sum.to_ascii_lowercase())
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Puts the checked release in place of this copy; returns what to start.
fn install(file: &Path, place: &Place, work: &Path) -> Result<PathBuf, String> {
    match place {
        Place::Bundle(bundle) => install_bundle(file, bundle, work),
        Place::Folder(dir) => install_folder(file, dir),
        Place::Setup(_) if is_setup(file) => {
            // Kept out of `work`, which goes; removed at the next start.
            let setup = std::env::temp_dir().join(file.file_name().ok_or("no file name")?);
            std::fs::copy(file, &setup).map_err(|e| format!("{}: {e}", setup.display()))?;
            Ok(setup)
        }
        // A release without a setup program: its files, if they can be
        // written.
        Place::Setup(dir) => install_folder(file, dir),
        Place::Deb(dir) if is_deb(file) => {
            run(
                "pkexec",
                &["dpkg".as_ref(), "-i".as_ref(), file.as_os_str()],
            )
            .map_err(|e| format!("the package was not installed ({e})"))?;
            Ok(dir.join(PROGRAMS[0]))
        }
        // /usr/bin is not ours to write: left in Downloads.
        Place::Deb(_) => Err("this copy came from the .deb, and the release has none".into()),
    }
}

/// Runs a command; its error output becomes the error.
fn run(program: &str, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{program}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// macOS: mounts the disk image, copies its PrzmTerm.app next to this one,
/// then swaps them; the old copy goes away only once the new one is there.
fn install_bundle(dmg: &Path, bundle: &Path, work: &Path) -> Result<PathBuf, String> {
    let parent = bundle.parent().ok_or("the app has no folder")?;
    writable(parent)?;
    let mount = work.join("mount");
    std::fs::create_dir_all(&mount).map_err(|e| e.to_string())?;
    run(
        "hdiutil",
        &[
            "attach".as_ref(),
            "-nobrowse".as_ref(),
            "-readonly".as_ref(),
            "-noautoopen".as_ref(),
            "-mountpoint".as_ref(),
            mount.as_os_str(),
            dmg.as_os_str(),
        ],
    )?;
    let copied = (|| {
        let app = std::fs::read_dir(&mount)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|x| x == "app"))
            .ok_or("the disk image has no app")?;
        let staged = parent.join(".PrzmTerm-update.app");
        let _ = std::fs::remove_dir_all(&staged);
        run("ditto", &[app.as_os_str(), staged.as_os_str()])?;
        Ok::<_, String>(staged)
    })();
    let _ = run(
        "hdiutil",
        &["detach".as_ref(), mount.as_os_str(), "-quiet".as_ref()],
    );
    let staged = copied?;
    let old = parent.join(".PrzmTerm-old.app");
    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(bundle, &old).map_err(|e| format!("{}: {e}", bundle.display()))?;
    if let Err(e) = std::fs::rename(&staged, bundle) {
        let _ = std::fs::rename(&old, bundle);
        return Err(format!("{}: {e}", bundle.display()));
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(bundle.to_path_buf())
}

/// The programs a release carries, by their file names.
const PROGRAMS: [&str; 2] = if cfg!(windows) {
    ["PrzmTerm.exe", "przm.exe"]
} else {
    ["przmterm", "przm"]
};

/// Windows and Linux: takes `przmterm` and `przm` out of the archive and
/// puts them in place of these. A running program cannot be overwritten on
/// Windows, but it can be renamed: the old one becomes `….old`, deleted at
/// the next start ([`clean_up`]).
fn install_folder(archive: &Path, dir: &Path) -> Result<PathBuf, String> {
    writable(dir)?;
    let mut new = Vec::new();
    unpack(archive, &mut |name, data| {
        // Windows does not tell `przmterm.exe` (before 1.0.0-rc.4) from
        // `PrzmTerm.exe`.
        let Some(program) = PROGRAMS
            .iter()
            .find(|p| **p == name || (cfg!(windows) && p.eq_ignore_ascii_case(name)))
        else {
            return Ok(());
        };
        let path = dir.join(format!("{program}.new"));
        std::fs::write(&path, data).map_err(|e| format!("{}: {e}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
        }
        new.push((path, dir.join(program)));
        Ok(())
    })?;
    if !new.iter().any(|(_, to)| to.ends_with(PROGRAMS[0])) {
        for (path, _) in &new {
            let _ = std::fs::remove_file(path);
        }
        return Err("the archive has no przmterm".into());
    }
    for (from, to) in &new {
        if cfg!(windows) && to.exists() {
            let old = to.with_extension("exe.old");
            let _ = std::fs::remove_file(&old);
            std::fs::rename(to, &old).map_err(|e| format!("{}: {e}", to.display()))?;
        }
        std::fs::rename(from, to).map_err(|e| format!("{}: {e}", to.display()))?;
    }
    Ok(dir.join(PROGRAMS[0]))
}

/// Receives each file of an archive: its name and contents.
type Take<'a> = dyn FnMut(&str, &[u8]) -> Result<(), String> + 'a;

/// Calls `take(file name, contents)` for each file in the release archive.
#[allow(unused_variables)]
fn unpack(archive: &Path, take: &mut Take) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
        for entry in tar.entries().map_err(|e| e.to_string())? {
            let mut entry = entry.map_err(|e| e.to_string())?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let name = entry
                .path()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_default();
            let mut data = Vec::new();
            entry.read_to_end(&mut data).map_err(|e| e.to_string())?;
            take(&name, &data)?;
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
            if !entry.is_file() {
                continue;
            }
            let name = entry
                .enclosed_name()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_default();
            let mut data = Vec::new();
            entry.read_to_end(&mut data).map_err(|e| e.to_string())?;
            take(&name, &data)?;
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    Err("no archive to unpack on this system".into())
}

/// Fails, saying why, when files cannot be written in `dir`.
fn writable(dir: &Path) -> Result<(), String> {
    let probe = dir.join(".przmterm-update-check");
    std::fs::write(&probe, b"")
        .map(|()| {
            let _ = std::fs::remove_file(&probe);
        })
        .map_err(|e| format!("cannot write in {}: {e}", dir.display()))
}

/// Deletes what an update on Windows left behind (`….exe.old`, the setup
/// program).
pub fn clean_up() {
    if !cfg!(windows) {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            let ours = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("przmterm-"));
            if ours && is_setup(&path) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        for program in PROGRAMS {
            let _ = std::fs::remove_file(dir.join(program).with_extension("exe.old"));
        }
    }
}

/// Starts `program` (the new version) once this process has ended.
pub fn start_after_exit(program: &Path) -> Result<(), String> {
    let mut cmd;
    if cfg!(windows) && is_setup(program) {
        // Installs quietly over this copy, for everyone or for this user as
        // before (Windows asks for an administrator for everyone), closes
        // what still runs and starts the new version.
        let for_user = exe_dir()
            .zip(std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
            .is_some_and(|(dir, local)| {
                local
                    .canonicalize()
                    .is_ok_and(|local| dir.starts_with(local))
            });
        cmd = std::process::Command::new(program);
        cmd.args([
            "/SILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/CLOSEAPPLICATIONS",
            "/SP-",
            if for_user {
                "/CURRENTUSER"
            } else {
                "/ALLUSERS"
            },
        ]);
    } else if cfg!(windows) {
        // Its files were renamed out of the way: it can start right away.
        cmd = std::process::Command::new(program);
    } else {
        let start = if cfg!(target_os = "macos") {
            "open \"$2\""
        } else {
            "exec \"$2\""
        };
        cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(format!(
                "while kill -0 \"$1\" 2>/dev/null; do sleep 0.2; done; {start}"
            ))
            .arg("sh")
            .arg(std::process::id().to_string())
            .arg(program);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, prerelease: bool) -> Release {
        Release {
            tag_name: tag.into(),
            html_url: format!("https://example.invalid/{tag}"),
            draft: false,
            prerelease,
            body: Some(format!("notes of {tag}")),
            assets: [
                "-macos-universal.dmg",
                "-windows-x86_64.zip",
                SETUP_SUFFIX,
                "-linux-x86_64.tar.gz",
                DEB_SUFFIX,
            ]
            .iter()
            .map(|suffix| format!("przmterm-{tag}{suffix}"))
            .chain(["SHA256SUMS.txt".into(), "SHA256SUMS.txt.asc".into()])
            .map(|name| Asset {
                url: format!("https://example.invalid/{name}"),
                name,
                size: 1,
            })
            .collect(),
        }
    }

    #[test]
    fn finds_the_newest_release_above_this_one() {
        let list = [
            release("v0.8.0", true),
            release("v0.9.0", true),
            release("v1.0.0-rc.1", true),
            release("not-a-version", false),
        ];
        let found = |current| newest(current, &list).map(|f| f.version);
        // Early builds hear of release candidates.
        assert_eq!(found("0.9.0").as_deref(), Some("1.0.0-rc.1"));
        assert_eq!(found("1.0.0-dev").as_deref(), Some("1.0.0-rc.1"));
        assert_eq!(found("1.0.0-rc.1"), None);
        // A stable build only of stable releases.
        let list = [release("v1.0.0", false), release("v1.1.0-rc.1", true)];
        assert_eq!(newest("1.0.0", &list), None);
        let list = [release("v1.0.0", false), release("v1.0.1", false)];
        assert_eq!(
            newest("1.0.0", &list).unwrap().url,
            "https://example.invalid/v1.0.1"
        );
        // The notes, this system's file and the checksums come along.
        let found = newest("1.0.0", &list).unwrap();
        assert_eq!(found.notes, "notes of v1.0.1");
        if let Some(suffix) = asset_suffixes().first() {
            assert!(found.asset.unwrap().name.ends_with(suffix));
        }
        assert_eq!(
            found.sums.unwrap().1,
            "https://example.invalid/SHA256SUMS.txt.asc"
        );
        // Drafts never.
        let mut draft = release("v9.0.0", false);
        draft.draft = true;
        assert_eq!(newest("1.0.0", &[draft]), None);
    }

    #[test]
    fn reads_the_releases_feed_and_its_files() {
        let feed = r#"<feed><entry><link rel="alternate" type="text/html"
            href="https://github.com/o/r/releases/tag/v1.0.0-rc.10"/></entry>
            <entry><link href="https://github.com/o/r/releases/tag/v1.0.0-rc.9"/>
            <content>see &lt;a href=&quot;https://github.com/o/r/releases/tag/v1.0.0-rc.9&quot;&gt;</content></entry></feed>"#;
        assert_eq!(feed_tags(feed), ["v1.0.0-rc.10", "v1.0.0-rc.9"]);
        let listed: Vec<Release> = feed_tags(feed)
            .iter()
            .map(|t| feed_release(t, &[], None))
            .collect();
        assert!(listed[0].prerelease);
        assert_eq!(
            newest("1.0.0-rc.9", &listed).map(|f| f.version).as_deref(),
            Some("1.0.0-rc.10")
        );

        let sums = "ab12  przmterm-1.0.0-rc.10-windows-x86_64-setup.exe\n\
                    cd34 *przmterm-1.0.0-rc.10-linux-x86_64.tar.gz\n";
        let files = sums_files(sums);
        assert_eq!(files[1], "przmterm-1.0.0-rc.10-linux-x86_64.tar.gz");
        let release = feed_release("v1.0.0-rc.10", &files, Some("notes".into()));
        let found = newest("1.0.0-rc.9", &[release]).unwrap();
        assert_eq!(
            found.sums.unwrap().0,
            "https://github.com/rodrigoprazim/przmterm/releases/download/v1.0.0-rc.10/SHA256SUMS.txt"
        );
        assert_eq!(found.notes, "notes");

        let changelog = "# Changelog\n\n## [Unreleased]\n\n- soon\n\n\
                         ## [1.0.0-rc.10] - 2026-11-30\n\n### Fixed\n\n- a fix\n\n\
                         ## [1.0.0-rc.9] - 2026-11-22\n\n- older\n";
        assert_eq!(
            changelog_section(changelog, "1.0.0-rc.10").as_deref(),
            Some("### Fixed\n\n- a fix")
        );
        assert_eq!(changelog_section(changelog, "2.0.0"), None);
    }

    #[test]
    fn tells_the_debian_package_apart() {
        assert!(is_deb(Path::new(
            "/tmp/przmterm-1.0.0-rc.4-linux-x86_64.deb"
        )));
        assert!(!is_deb(Path::new(
            "/tmp/przmterm-1.0.0-rc.4-linux-x86_64.tar.gz"
        )));
    }

    #[test]
    fn tells_the_setup_program_apart() {
        assert!(is_setup(Path::new(
            "C:/Temp/przmterm-1.0.0-rc.4-windows-x86_64-setup.exe"
        )));
        assert!(!is_setup(Path::new("C:/PrzmTerm/PrzmTerm.exe")));
        assert!(!is_setup(Path::new(
            "przmterm-1.0.0-rc.4-windows-x86_64.zip"
        )));
    }

    #[test]
    fn finds_the_checksum_of_a_file() {
        let sums = include_str!("../testdata/rc1-SHA256SUMS.txt");
        assert_eq!(
            expected_sum(sums, "przmterm-1.0.0-rc.1-windows-x86_64.zip").as_deref(),
            Some("4326a4c1d90eb4a011ab059419c6e4316940d52b80c5525ba00d7e6e4f37ebb9")
        );
        assert_eq!(expected_sum(sums, "przmterm-1.0.0-rc.1-windows"), None);
        assert_eq!(
            expected_sum("abc  *x.zip\n", "x.zip").as_deref(),
            Some("abc")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn installs_the_programs_of_an_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("r.tar.gz");
        {
            let gz = flate2::write::GzEncoder::new(
                std::fs::File::create(&archive).unwrap(),
                flate2::Compression::fast(),
            );
            let mut tar = tar::Builder::new(gz);
            for (name, data) in [
                ("przmterm-9/przmterm", &b"new app"[..]),
                ("przmterm-9/przm", b"new cli"),
                ("przmterm-9/README.md", b"readme"),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                tar.append_data(&mut header, name, data).unwrap();
            }
            tar.into_inner().unwrap().finish().unwrap();
        }
        let dir = tmp.path().join("bin");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("przmterm"), "old").unwrap();
        let start = install_folder(&archive, &dir).unwrap();
        assert_eq!(start, dir.join("przmterm"));
        assert_eq!(std::fs::read(dir.join("przmterm")).unwrap(), b"new app");
        assert_eq!(std::fs::read(dir.join("przm")).unwrap(), b"new cli");
        assert!(!dir.join("README.md").exists());
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("przm"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "executable");
    }
}

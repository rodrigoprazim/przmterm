//! Checking for a newer release: asks GitHub for the project's releases, in
//! the background, and says when there is one newer than this build. Nothing
//! is downloaded or installed: the user gets the release page.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use semver::Version;
use serde::Deserialize;

const RELEASES: &str = "https://api.github.com/repos/rodrigoprazim/przmterm/releases?per_page=20";

/// A release newer than this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub version: String,
    pub url: String,
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
        .map(|(version, r)| Found {
            version: version.to_string(),
            url: r.html_url.clone(),
        })
}

fn ask_github() -> Outcome {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .proxy(ureq::Proxy::try_from_env())
        .user_agent(concat!("PrzmTerm/", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent();
    let releases = agent
        .get(RELEASES)
        .header("Accept", "application/vnd.github+json")
        .call()
        .and_then(|mut response| response.body_mut().read_json::<Vec<Release>>());
    match releases {
        Ok(releases) => match newest(env!("CARGO_PKG_VERSION"), &releases) {
            Some(found) => Outcome::Newer(found),
            None => Outcome::UpToDate,
        },
        Err(e) => Outcome::Failed(e.to_string()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, prerelease: bool) -> Release {
        Release {
            tag_name: tag.into(),
            html_url: format!("https://example.invalid/{tag}"),
            draft: false,
            prerelease,
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
        // Drafts never.
        let mut draft = release("v9.0.0", false);
        draft.draft = true;
        assert_eq!(newest("1.0.0", &[draft]), None);
    }
}

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::{cleanup::HttpClient, permissions};

const RELEASES_API: &str = "https://api.github.com/repos/iniandrew/yaptic/releases/latest";
const RELEASES_URL: &str = "https://github.com/iniandrew/yaptic/releases/latest";

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct UpdateInfo {
    current: String,
    latest: String,
    available: bool,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
}

/// "v0.2.0" or "0.2.0" becomes (0, 2, 0). Anything else is rejected,
/// including pre-release suffixes and versions with fewer than three parts.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    fn number(part: &str) -> Option<u64> {
        // `parse` alone would also accept a leading "+".
        part.bytes().all(|b| b.is_ascii_digit()).then(|| part.parse().ok()).flatten()
    }
    let mut parts = s.strip_prefix('v').unwrap_or(s).split('.');
    let version = (
        number(parts.next()?)?,
        number(parts.next()?)?,
        number(parts.next()?)?,
    );
    parts.next().is_none().then_some(version)
}

/// Compares the running version with a release tag. Both strings in the
/// result are rebuilt from the parsed numbers, so nothing from GitHub is
/// passed through as text.
fn compare(current: &str, tag: &str) -> Result<UpdateInfo, String> {
    let current = parse_version(current).ok_or("This app's version number is unreadable.")?;
    let latest = parse_version(tag).ok_or("The latest release has an unrecognized version tag.")?;
    let text = |v: (u64, u64, u64)| format!("{}.{}.{}", v.0, v.1, v.2);
    Ok(UpdateInfo {
        current: text(current),
        latest: text(latest),
        available: latest > current,
    })
}

/// Asks GitHub for the latest published release. Only runs when the user
/// clicks "Check for updates".
#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> Result<UpdateInfo, String> {
    let current = app.package_info().version.to_string();
    let client = app.state::<HttpClient>().0.clone();
    let response = client
        .get(RELEASES_API)
        // GitHub rejects requests without a User-Agent.
        .header("User-Agent", format!("yaptic/{current}"))
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|_| "Couldn't reach GitHub. Check your connection.".to_string())?;

    match response.status().as_u16() {
        200..=299 => {}
        403 | 429 => return Err("GitHub is limiting requests right now. Try again later.".into()),
        404 => return Err("No published release was found.".into()),
        code => return Err(format!("GitHub answered with HTTP {code}.")),
    }

    let release: Release = response
        .json()
        .await
        .map_err(|_| "GitHub sent an answer Yaptic couldn't read.".to_string())?;
    compare(&current, &release.tag_name)
}

/// Opens the releases page in the default browser.
#[tauri::command]
pub fn open_release_page() {
    permissions::open_url(RELEASES_URL);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_v_prefixed_versions() {
        assert_eq!(parse_version("0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("v0.1.1"), Some((0, 1, 1)));
        assert_eq!(parse_version("v10.20.30"), Some((10, 20, 30)));
    }

    #[test]
    fn rejects_anything_that_is_not_three_numbers() {
        for bad in ["", "v", "1.2", "1.2.3.4", "1.2.3-beta", "1.2.x", "abc", "v+1.2.3", "1..3", "-1.2.3"] {
            assert_eq!(parse_version(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn newer_release_is_available() {
        let info = compare("0.1.1", "v0.2.0").unwrap();
        assert!(info.available);
        assert_eq!((info.current.as_str(), info.latest.as_str()), ("0.1.1", "0.2.0"));
    }

    #[test]
    fn compares_numbers_not_text() {
        assert!(compare("0.9.0", "v0.10.0").unwrap().available);
        assert!(!compare("0.10.0", "v0.9.0").unwrap().available);
    }

    #[test]
    fn same_or_older_release_is_not_available() {
        assert!(!compare("0.2.0", "v0.2.0").unwrap().available);
        assert!(!compare("0.3.0", "v0.2.9").unwrap().available);
    }

    #[test]
    fn bad_tag_is_an_error_that_does_not_echo_it() {
        let err = compare("0.2.0", "<script>nightly</script>").unwrap_err();
        assert!(!err.contains("script"));
    }
}

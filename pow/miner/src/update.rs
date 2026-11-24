//! "Update available" notice: compares the latest GitHub release with the version this binary
//! was built as (`SM64COIN_VERSION`, set by the release workflow; absent = "dev" = no check).
//! A release whose body has a line `Update: required` is reported as required.

pub const LATEST_URL: &str = "https://api.github.com/repos/Gumbo64/sm64coin/releases/latest";
/// The version this binary was built as ("dev" outside release builds).
pub const VERSION: &str = match option_env!("SM64COIN_VERSION") { Some(v) => v, None => "dev" };

/// "v0.2.8" / "0.2.8-rc1" -> (0, 2, 8).
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches('v');
    let core = s.split(|c: char| !(c.is_ascii_digit() || c == '.')).next()?;
    let mut it = core.split('.');
    let v = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    Some(v)
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    matches!((parse_version(latest), parse_version(current)), (Some(a), Some(b)) if a > b)
}

/// A line consisting of `Update: required` (any case) anywhere in the release body.
pub fn is_required(body: &str) -> bool {
    body.lines().any(|l| {
        let l = l.trim().to_ascii_lowercase();
        l.strip_prefix("update:").is_some_and(|r| r.trim() == "required")
    })
}

/// The notice text for a release JSON, if it is newer than `current`.
pub fn notice(release: &serde_json::Value, current: &str) -> Option<String> {
    let tag = release.get("tag_name")?.as_str()?;
    let (a, b, c) = parse_version(tag)?;
    if !is_newer(tag, current) {
        return None;
    }
    let url = release.get("html_url").and_then(|u| u.as_str()).unwrap_or("https://github.com/Gumbo64/sm64coin/releases/latest");
    let required = release.get("body").and_then(|b| b.as_str()).is_some_and(is_required);
    Some(format!("SM64Coin {a}.{b}.{c} is available{}: {url}", if required { " (UPDATE REQUIRED)" } else { "" }))
}

/// Logs one line to stderr if a newer release exists. Runs in a background thread with a 3 s
/// timeout; silent on any failure, never blocks. Off for "dev" builds and with
/// `SM64COIN_NO_UPDATE_CHECK=1`.
pub fn spawn_check() {
    if std::env::var("SM64COIN_NO_UPDATE_CHECK").is_ok_and(|v| !v.is_empty() && v != "0") || parse_version(VERSION).is_none() {
        return;
    }
    let url = std::env::var("SM64COIN_UPDATE_URL").unwrap_or_else(|_| LATEST_URL.to_string());
    let _ = std::thread::Builder::new().name("update-check".into()).spawn(move || {
        let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(3)).build();
        let Ok(r) = agent.get(&url).set("Accept", "application/vnd.github+json").call() else { return };
        let Ok(j) = r.into_json::<serde_json::Value>() else { return };
        if let Some(n) = notice(&j, VERSION) {
            eprintln!("{n}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.2.8"), Some((0, 2, 8)));
        assert_eq!(parse_version("0.2.8-rc1"), Some((0, 2, 8)));
        assert_eq!(parse_version("dev"), None);
        assert!(is_newer("v0.2.8", "0.2.7"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("0.2.8", "0.2.8"));
        assert!(!is_newer("0.2.7", "0.2.8"));
        assert!(!is_newer("v0.2.8", "dev"));
    }
    #[test]
    fn required_and_notice() {
        assert!(is_required("notes\r\nUpdate: required\r\nmore"));
        assert!(is_required("update:Required"));
        assert!(!is_required("an update: required reading"));
        let j = serde_json::json!({"tag_name": "v0.3.0", "html_url": "https://x/r", "body": "Update: required"});
        assert_eq!(notice(&j, "0.2.9").unwrap(), "SM64Coin 0.3.0 is available (UPDATE REQUIRED): https://x/r");
        let j = serde_json::json!({"tag_name": "v0.3.0", "html_url": "https://x/r", "body": ""});
        assert_eq!(notice(&j, "0.2.9").unwrap(), "SM64Coin 0.3.0 is available: https://x/r");
        assert!(notice(&j, "0.3.0").is_none());
    }
}

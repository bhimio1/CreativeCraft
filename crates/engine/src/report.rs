//! "Report a bug": the issue tracker for an app, or for CreativeCraft itself. GitHub trackers get
//! a new-issue link pre-filled with a short template and the system details (app and version, OS,
//! CreativeCraft version). Nothing is sent anywhere: the browser opens and the user writes and
//! submits the report.

use creativecraft_catalog::AppSpec;

/// CreativeCraft's own repository, from the workspace `repository`.
pub const HUB_REPO_URL: &str = env!("CARGO_PKG_REPOSITORY");
const HUB_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where to report a bug in `app` (installed at `version`, if known), or in CreativeCraft when
/// `app` is `None`.
pub fn bug_report_url(app: Option<&AppSpec>, version: Option<&str>) -> String {
    let os = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    match app {
        None => {
            let repo = HUB_REPO_URL.strip_prefix("https://github.com/").unwrap_or(HUB_REPO_URL).trim_end_matches('/');
            github_new_issue(repo, &[("CreativeCraft", HUB_VERSION.to_string()), ("OS", os)])
        }
        Some(app) => match app.github_issues_repo() {
            Some(repo) => {
                let ver = version.map(|v| v.trim_start_matches('v').to_string()).unwrap_or_else(|| "not installed by CreativeCraft".into());
                github_new_issue(&repo, &[(app.name.as_str(), ver), ("OS", os), ("Reported from", format!("CreativeCraft {HUB_VERSION}"))])
            }
            None => app.bugs.clone().unwrap_or_else(|| app.homepage.clone()),
        },
    }
}

fn github_new_issue(repo: &str, details: &[(&str, String)]) -> String {
    let mut body = String::from("**What happened?**\n\n\n**What did you expect?**\n\n\n**Steps to reproduce**\n1. \n\n---\n");
    for (k, v) in details {
        body.push_str(&format!("- {k}: {v}\n"));
    }
    format!("https://github.com/{repo}/issues/new?body={}", percent_encode(&body))
}

/// Query-string encoding: everything but unreserved characters (RFC 3986) is escaped.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use creativecraft_catalog::Catalog;

    #[test]
    fn craft_apps_prefill_github_issues() {
        let c = Catalog::builtin().unwrap();
        let url = bug_report_url(c.get("photocraft"), Some("v0.5.0"));
        assert!(url.starts_with("https://github.com/storytold/photocraft/issues/new?body="), "{url}");
        assert!(url.contains("PhotoCraft%3A%200.5.0"), "{url}");
        assert!(!url.contains(' ') && !url.contains('\n'));
    }

    #[test]
    fn external_apps_use_their_tracker() {
        let c = Catalog::builtin().unwrap();
        assert_eq!(bug_report_url(c.get("krita"), None), "https://bugs.kde.org/enter_bug.cgi?product=krita");
        // a GitHub tracker gets the pre-filled form too
        assert!(bug_report_url(c.get("godot"), None).starts_with("https://github.com/godotengine/godot/issues/new?body="));
    }

    #[test]
    fn every_app_has_somewhere_to_report() {
        for app in Catalog::builtin().unwrap().apps {
            assert!(bug_report_url(Some(&app), None).starts_with("https://"), "{}", app.id);
        }
    }

    #[test]
    fn hub_reports_go_to_creativecraft() {
        let url = bug_report_url(None, None);
        assert!(url.starts_with("https://github.com/") && url.contains("/issues/new?body="), "{url}");
        assert!(url.contains("CreativeCraft%3A%20"), "{url}");
    }

    #[test]
    fn encoding() {
        assert_eq!(percent_encode("a b/c?&é"), "a%20b%2Fc%3F%26%C3%A9");
    }
}

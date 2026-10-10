//! Is there something newer than what's installed? Checked against where the app installs from
//! (its official releases, a fork's releases, or a git branch), without installing anything.

use crate::source::{Source, remote_head};
use crate::{InstallError, Installed, github};
use creativecraft_catalog::AppSpec;

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCheck {
    UpToDate {
        have: String,
    },
    /// `latest` is a release tag, or `git-<commit>` for a git build.
    Available {
        have: String,
        latest: String,
    },
}

impl UpdateCheck {
    pub fn latest(&self) -> Option<&str> {
        match self {
            UpdateCheck::Available { latest, .. } => Some(latest),
            UpdateCheck::UpToDate { .. } => None,
        }
    }
}

/// Compare `installed` with what `source` offers now.
pub fn check_update(app: &AppSpec, source: &Source, installed: &Installed) -> Result<UpdateCheck, InstallError> {
    let have = installed.version.clone();
    let built_from_git = have.starts_with("git-");
    let latest = match source {
        Source::Release => {
            let repo = app.repo.as_deref().ok_or_else(|| InstallError::External(app.name.clone()))?;
            github::latest_release(repo)?.tag
        }
        Source::Fork { repo } => github::latest_release(repo)?.tag,
        Source::Git { url, branch } => format!("git-{}", &remote_head(app, url, branch)?[..10]),
    };
    let newer = match source {
        // a different commit on the branch is an update (history only moves forward for us)
        Source::Git { .. } => latest != have,
        // a release after a git build (or the other way round) means the source changed
        _ if built_from_git => true,
        _ => creativecraft_platform::cmp_versions(&latest, &have).is_gt(),
    };
    Ok(if newer { UpdateCheck::Available { have, latest } } else { UpdateCheck::UpToDate { have } })
}

#[cfg(test)]
mod tests {
    use crate::source::head_from_ls_remote;

    #[test]
    fn ls_remote_output_gives_the_branch_commit() {
        let out = "3f2c1a9b8e7d6c5b4a39281706f5e4d3c2b1a090\tHEAD\n0123456789abcdef0123456789abcdef01234567\trefs/heads/dev\n";
        assert_eq!(head_from_ls_remote(out, "refs/heads/dev").as_deref(), Some("0123456789abcdef0123456789abcdef01234567"));
        assert_eq!(head_from_ls_remote(out, "HEAD").as_deref(), Some("3f2c1a9b8e7d6c5b4a39281706f5e4d3c2b1a090"));
        assert_eq!(head_from_ls_remote(out, "refs/heads/main"), None);
    }
}

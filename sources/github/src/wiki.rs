//! Wiki history is a Git repository, not a GitHub REST resource. Clones are transient and bare.

use serde::{Deserialize, Serialize};

use crate::memcastle::source::host::run_process;

const MAX_REVISIONS: usize = 10_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Revision {
    pub repo: String,
    pub commit: String,
    pub path: String,
    pub occurred_at: String,
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.ends_with(".md")
        && !path.starts_with('/')
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        && !path.chars().any(char::is_control)
}

fn git(args: &[String]) -> Result<Vec<u8>, String> {
    let output = run_process("git", args, None)
        .map_err(|_| "cannot run git; install it on the daemon's PATH".to_string())?;
    if output.status != 0 {
        // Git may print a credential-bearing remote URL or a helper diagnostic. Never relay its stderr.
        return Err(
            "Git wiki request failed; check repository wiki access, Git and token permissions"
                .into(),
        );
    }
    Ok(output.stdout)
}

struct Clone {
    path: String,
}

impl Drop for Clone {
    fn drop(&mut self) {
        // The path is generated here, never supplied by GitHub or a caller. The `--` prevents option injection.
        let _ = run_process("rm", &["-rf".into(), "--".into(), self.path.clone()], None);
    }
}

impl Clone {
    fn open(repo: &str) -> Result<Self, String> {
        // Unpredictable paths prevent another local user from planting a directory our cleanup would remove.
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| "cannot create random Git scratch path")?;
        let path = format!("/tmp/memcastle-github-{}", hex(&nonce));
        let clone = Self { path };
        // This helper is fixed code: Git invokes it only for github.com and reads the granted variable itself.
        // The token is never a command argument or a Git configuration value on disk.
        const HELPER: &str = "!f() { host=; while IFS= read -r line; do case \"$line\" in host=*) host=${line#host=};; esac; done; if [ \"$host\" = github.com ]; then printf 'username=x-access-token\\npassword=%s\\n' \"${GH_TOKEN:-$GITHUB_TOKEN}\"; fi; }; f";
        let mut args = vec![
            "-c".into(),
            "credential.helper=".into(),
            "-c".into(),
            "core.askPass=/bin/false".into(),
            "-c".into(),
            "http.followRedirects=false".into(),
        ];
        if crate::api::token().is_some() {
            args.extend(["-c".into(), format!("credential.helper={HELPER}")]);
        }
        args.extend([
            "clone".into(),
            "--bare".into(),
            "--quiet".into(),
            format!("https://github.com/{repo}.wiki.git"),
            clone.path.clone(),
        ]);
        git(&args)?;
        Ok(clone)
    }

    fn command(&self, arguments: &[&str]) -> Result<String, String> {
        let mut args = vec!["--git-dir".to_string(), self.path.clone()];
        args.extend(arguments.iter().map(|arg| (*arg).to_string()));
        String::from_utf8(git(&args)?).map_err(|_| "Git wiki content is not UTF-8".into())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn revisions(repo: &str) -> Result<Vec<Revision>, String> {
    let clone = Clone::open(repo)?;
    let output = clone.command(&[
        "log",
        "--reverse",
        "--format=MC:%H:%cI",
        "--name-only",
        "--diff-filter=AMR",
        "HEAD",
    ])?;
    let mut revisions = Vec::new();
    let mut commit = String::new();
    let mut occurred_at = String::new();
    for line in output.lines() {
        if let Some(header) = line.strip_prefix("MC:") {
            let (sha, time) = header
                .split_once(':')
                .ok_or("Git wiki history has no commit timestamp")?;
            if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err("Git wiki history has an invalid commit ID".into());
            }
            commit = sha.into();
            occurred_at = time.into();
        } else if valid_path(line) {
            if commit.is_empty() {
                return Err("Git wiki history has a page without a commit".into());
            }
            revisions.push(Revision {
                repo: repo.into(),
                commit: commit.clone(),
                path: line.into(),
                occurred_at: occurred_at.clone(),
            });
            if revisions.len() > MAX_REVISIONS {
                return Err(
                    "Git wiki history exceeds 10,000 page revisions; narrow the wiki scope".into(),
                );
            }
        }
    }
    Ok(revisions)
}

pub(crate) fn page(revision: &Revision) -> Result<String, String> {
    if revision.commit.len() != 40
        || !revision.commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !valid_path(&revision.path)
    {
        return Err("invalid Git wiki page or commit".into());
    }
    Clone::open(&revision.repo)?
        .command(&["show", &format!("{}:{}", revision.commit, revision.path)])
}

//! The shared agent skills under `skills/` (`docs/skills.md`) hold to what this release actually exposes.
//!
//! A skill is plain text, so nothing compiles it: a renamed tool, a removed route or a mistyped command would
//! leave an agent following instructions that fail at run time. These tests read every skill the way a client's skill
//! discovery would, and check each capability it names against the daemon, the CLI and the documented routes.
//!
//! What this cannot prove, and stays with the reviewer: that the advice is good. It only proves the advice is
//! about things that exist.

use crate::common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use common::TestDaemon;
use common::mcp::{connect, fixture};

/// A skill as a client's discovery sees it: its directory, its frontmatter and the instructions below it.
struct Skill {
    /// The directory the skill lives in, which Agent Skills clients require to equal `name`.
    directory: String,
    /// The frontmatter's `name`.
    name: String,
    /// The frontmatter's `description`, which is all a client shows an agent before it loads the skill.
    description: String,
    /// The frontmatter's one-level `metadata` map.
    metadata: BTreeMap<String, String>,
    /// Every other top-level frontmatter field.
    fields: BTreeMap<String, String>,
    /// The Markdown below the frontmatter.
    body: String,
}

/// The repository's `skills/` directory, from the manifest and not the working directory, so the test runs from
/// anywhere on any operating system.
fn skills_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("skills")
}

/// Read `path` as UTF-8, and say which file when it cannot be read.
fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

/// Strip one pair of surrounding quotes, which YAML allows and a skill uses for a value like `"0.2"`.
fn unquote(value: &str) -> &str {
    let value = value.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// Parse a `SKILL.md`. Only the subset of YAML a skill uses is understood (flat `key: value` lines and one
/// `metadata:` map), and anything else fails loudly, so a skill never silently carries a field no client reads.
fn parse(directory: &str, text: &str) -> Skill {
    let text = text.replace("\r\n", "\n");
    let rest = text.strip_prefix("---\n").unwrap_or_else(|| {
        panic!("{directory}/SKILL.md must start with a `---` frontmatter block")
    });
    let (frontmatter, body) = rest
        .split_once("\n---\n")
        .unwrap_or_else(|| panic!("{directory}/SKILL.md frontmatter is never closed by `---`"));

    let mut fields = BTreeMap::new();
    let mut metadata = BTreeMap::new();
    let mut in_metadata = false;
    for line in frontmatter.lines() {
        if let Some(entry) = line.strip_prefix("  ") {
            assert!(
                in_metadata,
                "{directory}: an indented line outside `metadata:`: {line}"
            );
            let (key, value) = entry
                .split_once(':')
                .unwrap_or_else(|| panic!("{directory}: not a `key: value` line: {line}"));
            metadata.insert(key.trim().to_owned(), unquote(value).to_owned());
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .unwrap_or_else(|| panic!("{directory}: not a `key: value` line: {line}"));
        in_metadata = key == "metadata";
        if !in_metadata {
            fields.insert(key.trim().to_owned(), unquote(value).to_owned());
        }
    }

    Skill {
        directory: directory.to_owned(),
        name: fields.remove("name").unwrap_or_default(),
        description: fields.remove("description").unwrap_or_default(),
        metadata,
        fields,
        body: body.to_owned(),
    }
}

/// Every skill under `root`, the way Agent Skills discovery finds them: a `SKILL.md` one level down.
fn discover(root: &Path) -> Vec<Skill> {
    let mut skills: Vec<Skill> = std::fs::read_dir(root)
        .unwrap_or_else(|error| panic!("cannot list {}: {error}", root.display()))
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.join("SKILL.md").is_file())
        .map(|path| {
            let directory = path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("a UTF-8 directory name")
                .to_owned();
            parse(&directory, &read(&path.join("SKILL.md")))
        })
        .collect();
    skills.sort_by(|a, b| a.directory.cmp(&b.directory));
    skills
}

/// Every skill in the repository, and at least one, so an emptied directory fails instead of passing vacuously.
fn skills() -> Vec<Skill> {
    let skills = discover(&skills_root());
    assert!(
        !skills.is_empty(),
        "skills/ holds no skill, so there is nothing to validate"
    );
    skills
}

/// Copy `from` into `to`, recursively, which is what a user does to install a skill by hand.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create the destination");
    for entry in std::fs::read_dir(from).expect("list the source") {
        let entry = entry.expect("a directory entry");
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy a file");
        }
    }
}

/// Every `memcastle_<tool>` identifier in `text`. A trailing underscore is punctuation, not part of a name.
fn tool_names(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find("memcastle_") {
        // A longer identifier that merely ends in the prefix (`my_memcastle_x`) is not a tool name.
        let preceded_by_word = rest[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let tail = &rest[start..];
        let length = tail
            .find(|c: char| !(c.is_ascii_lowercase() || c == '_'))
            .unwrap_or(tail.len());
        let name = tail[..length].trim_end_matches('_');
        if !preceded_by_word && name.len() > "memcastle_".len() {
            found.insert(name.to_owned());
        }
        rest = &tail[length.max(1)..];
    }
    found
}

/// The text a skill writes as code: fenced blocks and inline `spans`. Commands are only ever written there, and
/// prose such as "the memcastle binary" must not be read as a command.
fn code_segments(body: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut fenced = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if fenced {
            segments.push(line.to_owned());
        } else {
            // Inline spans are the odd-numbered pieces between backticks.
            segments.extend(line.split('`').skip(1).step_by(2).map(str::to_owned));
        }
    }
    segments
}

/// The words after each `memcastle ` in the code of `body`: the subcommand, and the one after it. Flags
/// (`--version`) are not subcommands and end the command.
fn cli_invocations(body: &str) -> BTreeSet<Vec<String>> {
    let mut found = BTreeSet::new();
    for segment in code_segments(body) {
        let mut rest = segment.as_str();
        while let Some(start) = rest.find("memcastle ") {
            let at_word_start = rest[..start]
                .chars()
                .next_back()
                .is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '-' | '_' | '/' | '.')));
            let tail = &rest[start + "memcastle ".len()..];
            if at_word_start {
                let words: Vec<String> = tail
                    .split_whitespace()
                    .take(2)
                    .take_while(|word| {
                        word.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                            && !word.starts_with('-')
                    })
                    .map(str::to_owned)
                    .collect();
                if !words.is_empty() {
                    found.insert(words);
                }
            }
            rest = tail;
        }
    }
    found
}

/// What `memcastle <args> --help` prints, which is the CLI's own statement of what exists.
fn help(args: &[&str]) -> String {
    let output = Command::cargo_bin("memcastle")
        .expect("the memcastle binary is built")
        .args(args)
        .arg("--help")
        .output()
        .expect("memcastle --help runs");
    assert!(
        output.status.success(),
        "`memcastle {args:?} --help` failed"
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The command names under a `Commands:` heading of clap help output.
fn listed_commands(help: &str) -> BTreeSet<String> {
    help.lines()
        .skip_while(|line| !line.starts_with("Commands:"))
        .skip(1)
        .take_while(|line| line.starts_with("  "))
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

/// Every `/api/...` path in `text`, with a trailing slash or punctuation dropped.
fn rest_paths(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find("/api/") {
        let tail = &rest[start..];
        let length = tail
            .find(|c: char| {
                !(c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '{' | '}'))
            })
            .unwrap_or(tail.len());
        found.insert(tail[..length].trim_end_matches('/').to_owned());
        rest = &tail[length..];
    }
    found
}

/// The routes `docs/mcp-and-api.md` documents: the second word of each `| `GET /api/...` |` table row.
fn documented_routes() -> BTreeSet<String> {
    let page = read(&Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/mcp-and-api.md"));
    page.lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|rest| rest.split_once('`'))
        .filter_map(|(route, _)| route.split_once(' '))
        .filter(|(method, _)| method.chars().all(|c| c.is_ascii_uppercase()))
        .map(|(_, path)| path.to_owned())
        .collect()
}

/// The range a skill declares in `metadata.memcastle-version`, parsed the way a reader would: a missing key or a value
/// that is not a semver range fails naming the skill, so a typo cannot pass as "no constraint".
fn declared_range(skill: &Skill) -> semver::VersionReq {
    let declared = skill
        .metadata
        .get("memcastle-version")
        .unwrap_or_else(|| panic!("skill `{}` declares no `memcastle-version`", skill.name));
    semver::VersionReq::parse(declared).unwrap_or_else(|error| {
        panic!(
            "skill `{}`: `{declared}` is not a semver range such as `>=0.2.0` ({error})",
            skill.name
        )
    })
}

#[test]
fn every_skill_directory_has_a_skill_file_with_a_matching_name_and_a_description() {
    // Name rule shared by Agent Skills clients: lowercase alphanumerics in hyphen-separated words, at most 64.
    let valid_name = |name: &str| {
        !name.is_empty()
            && name.len() <= 64
            && name.split('-').all(|word| {
                !word.is_empty()
                    && word
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            })
    };

    for skill in skills() {
        assert!(
            valid_name(&skill.name),
            "`{}` is not a valid skill name",
            skill.name
        );
        assert_eq!(
            skill.name, skill.directory,
            "a client rejects a skill whose name differs from its directory"
        );
        assert!(
            !skill.description.is_empty() && skill.description.len() <= 1024,
            "{}: the description must be 1 to 1024 characters, it is what an agent routes on",
            skill.name
        );
        assert!(
            skill.description.contains("Use "),
            "{}: the description must say when to use the skill, not only what it is",
            skill.name
        );
        assert!(
            skill.fields.contains_key("license"),
            "{}: a distributed skill declares its license",
            skill.name
        );
        assert!(
            !skill.body.trim().is_empty(),
            "{}: the skill has no instructions",
            skill.name
        );
    }
}

#[test]
fn every_directory_under_skills_is_a_skill_and_every_skill_is_documented() {
    let on_disk: BTreeSet<String> = skills().into_iter().map(|skill| skill.directory).collect();

    // A directory without a SKILL.md is invisible to every client, so it is a mistake and not a draft.
    for entry in std::fs::read_dir(skills_root()).expect("list skills/") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            assert!(on_disk.contains(&name), "skills/{name} has no SKILL.md");
        }
    }

    // The docs page lists the skills in the table under `## The skills`, one `name` per row.
    let page = read(&Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/skills.md"));
    let section = page
        .split("\n## ")
        .find(|section| section.starts_with("The skills"))
        .expect("docs/skills.md has a `## The skills` section");
    let documented: BTreeSet<String> = section
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|rest| rest.split_once('`'))
        .map(|(name, _)| name.to_owned())
        .collect();
    assert_eq!(
        documented, on_disk,
        "docs/skills.md and skills/ must list the same skills"
    );

    let readme = read(&skills_root().join("README.md"));
    for name in &on_disk {
        assert!(
            // The README lists them in a layout block, as `name/`, so match the directory form.
            readme.contains(&format!("{name}/")),
            "skills/README.md does not mention `{name}`"
        );
    }
}

#[tokio::test]
async fn every_tool_a_skill_names_is_registered_with_the_daemon() {
    let daemon = TestDaemon::start().await;
    let session = connect(&daemon.base_url).await;
    let registered: BTreeSet<String> = session
        .peer()
        .list_all_tools()
        .await
        .expect("tools/list")
        .iter()
        .map(|tool| tool.name.to_string())
        .collect();

    for skill in skills() {
        for tool in tool_names(&skill.body) {
            assert!(
                registered.contains(&tool),
                "skill `{}` names `{tool}`, which this release does not register",
                skill.name
            );
        }
    }

    session.cancel().await.expect("close session");
    daemon.shutdown().await;
}

#[test]
fn every_command_a_skill_names_exists_in_the_cli() {
    let top_level = listed_commands(&help(&[]));
    assert!(
        top_level.contains("status"),
        "the CLI help lists no commands"
    );

    for skill in skills() {
        for words in cli_invocations(&skill.body) {
            let command = &words[0];
            assert!(
                top_level.contains(command),
                "skill `{}` runs `memcastle {command}`, which the CLI does not have",
                skill.name
            );
            // A second word is only a subcommand when the first has any (`daemon start`); otherwise it is an
            // argument (`search formatter`) that only the user's data can validate.
            if let Some(sub) = words.get(1) {
                let subcommands = listed_commands(&help(&[command]));
                assert!(
                    subcommands.is_empty() || subcommands.contains(sub),
                    "skill `{}` runs `memcastle {command} {sub}`, which the CLI does not have",
                    skill.name
                );
            }
        }
    }
}

#[test]
fn every_route_a_skill_names_is_a_documented_one() {
    let documented = documented_routes();
    assert!(
        documented.contains("/api/health"),
        "docs/mcp-and-api.md lists no routes, so nothing can be checked"
    );

    for skill in skills() {
        for route in rest_paths(&skill.body) {
            assert!(
                documented.contains(&route),
                "skill `{}` names `{route}`, which docs/mcp-and-api.md does not document",
                skill.name
            );
        }
    }
}

#[test]
fn a_skill_never_points_an_agent_at_storage_or_the_database_endpoint() {
    // These are operator tools with no MCP surface (ADR-014, ADR-015): an agent following a skill must not reach
    // for them, and a skill must not become a second route to credentials or the database.
    let forbidden = [
        "/api/db",
        "/api/auth",
        "memcastle db",
        "memcastle migrate",
        "surrealdb",
        "surrealkv",
    ];
    for skill in skills() {
        let text = skill.body.to_lowercase();
        for word in forbidden {
            // `memcastle auth generate` stays allowed in the setup skill: it is a CLI step for the user, whereas the
            // token routes themselves (`/api/auth`) are never described to an agent.
            assert!(
                !text.contains(word),
                "skill `{}` mentions `{word}`, which agents are not meant to use",
                skill.name
            );
        }
    }
}

#[test]
fn a_skill_that_calls_a_gated_tool_says_what_to_do_when_the_mode_refuses_it() {
    let modes = fixture("modes.json");
    let code = modes["forbidden_code"]
        .as_str()
        .expect("a forbidden code")
        .to_owned();
    // `memcastle_repair` is ungated as a dry run and gated when applied, so it cannot be classified by name.
    let gated: BTreeSet<&str> = modes["operations"]
        .as_array()
        .expect("operations")
        .iter()
        .filter(|operation| operation["class"] != "ungated")
        .filter_map(|operation| operation["tool"].as_str())
        .filter(|tool| *tool != "memcastle_repair")
        .collect();

    for skill in skills() {
        let uses_gated_tool = tool_names(&skill.body)
            .iter()
            .any(|tool| gated.contains(tool.as_str()));
        if uses_gated_tool {
            assert!(
                skill.body.contains(&code),
                "skill `{}` calls a mode-gated tool but never names `{code}`",
                skill.name
            );
            assert!(
                skill.body.contains("memcastle_set_mode"),
                "skill `{}` must say not to call `memcastle_set_mode` to get around a refusal",
                skill.name
            );
        }
    }
}

#[test]
fn a_skill_is_self_contained_so_installing_one_directory_alone_works() {
    for skill in skills() {
        // Markdown links of the form `](target)`.
        for piece in skill.body.split("](").skip(1) {
            let target = piece.split(')').next().unwrap_or_default();
            if target.starts_with("http://")
                || target.starts_with("https://")
                || target.starts_with('#')
            {
                continue;
            }
            let resolved = skills_root().join(&skill.directory).join(target);
            assert!(
                !target.starts_with("..") && resolved.exists(),
                "skill `{}` links to `{target}`, which is outside or missing from its own directory",
                skill.name
            );
        }
    }
}

#[test]
fn copying_the_skills_directory_into_a_client_location_leaves_every_skill_discoverable() {
    // The documented install is a plain copy into `.agents/skills`, `.claude/skills` or similar; discovery on the
    // copy must find exactly what the repository holds, with no build step in between.
    let target = tempfile::tempdir().expect("a tempdir");
    let installed_root = target.path().join(".agents").join("skills");
    for skill in skills() {
        copy_dir(
            &skills_root().join(&skill.directory),
            &installed_root.join(&skill.directory),
        );
    }

    let original: Vec<(String, String)> = skills()
        .into_iter()
        .map(|skill| (skill.name, skill.description))
        .collect();
    let installed: Vec<(String, String)> = discover(&installed_root)
        .into_iter()
        .map(|skill| (skill.name, skill.description))
        .collect();
    assert_eq!(original, installed);
}

#[test]
fn the_setup_skill_calls_no_tool_but_status_so_it_works_before_any_daemon_exists() {
    let setup = skills()
        .into_iter()
        .find(|skill| skill.name == "memcastle-setup")
        .expect("the memcastle-setup skill exists");
    let allowed: BTreeSet<String> = ["memcastle_status".to_owned()].into();
    assert_eq!(
        tool_names(&setup.body),
        allowed,
        "the setup skill must be usable with no daemon, so its only tool call is the final verification"
    );
}

#[test]
fn a_skill_declares_a_version_range_that_the_crate_satisfies() {
    // A pre-release crate version (`0.3.0-rc.1`) does not match a plain range such as `>=0.2.0` under semver's
    // rules, so this fails loudly if `Cargo.toml` ever carries one, which is the cue to decide what skills mean then.
    let version = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .expect("the crate version is valid semver");
    for skill in skills() {
        let range = declared_range(&skill);
        // A skill whose range excludes this checkout describes a release this checkout does not have, typically a
        // floor raised for a feature that `Cargo.toml` has not been bumped to yet. The floor is raised once it has.
        assert!(
            range.matches(&version),
            "skill `{}` is written for `{range}`, which this crate ({version}) does not satisfy",
            skill.name
        );
    }
}

#[test]
fn a_skill_range_has_a_lower_bound_so_it_names_the_release_it_relies_on() {
    use semver::Op;
    for skill in skills() {
        let range = declared_range(&skill);
        // `*` and an upper-bound-only range say nothing about which release introduced what the skill uses, which is
        // the one thing the field is for.
        assert!(
            range.comparators.iter().any(|comparator| matches!(
                comparator.op,
                Op::Greater | Op::GreaterEq | Op::Exact | Op::Caret | Op::Tilde
            )),
            "skill `{}`: `{range}` has no lower bound, so it does not name the release the skill relies on",
            skill.name
        );
    }
}

#[test]
fn the_integration_contract_still_records_skills_as_plain_text_with_no_daemon_operation() {
    let manifest = fixture("capabilities.json");
    let skills_row = manifest["capabilities"]
        .as_array()
        .expect("capabilities")
        .iter()
        .find(|capability| capability["id"] == "skills")
        .expect("the manifest has a `skills` capability");
    assert!(
        skills_row["operations"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "a skill is agent text: giving the capability a daemon operation would make MemCastle enforce it"
    );
    assert!(
        skills_row["daemon_test"].is_null(),
        "nothing about the skills row is tested against a daemon; tests/in_process/skills.rs checks the files"
    );
}

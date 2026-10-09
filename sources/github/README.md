# GitHub source

The `github` source is a bundled WebAssembly component that mines selected GitHub repositories.
It reads repository metadata, issues, pull requests, optional conversation comments and reviews from the official REST API.
Repository wikis are separate Git repositories; this source reads their available commit history with Git and files each changed Markdown page at each commit as a separate document.
No semantic inference or GitHub SDK is part of the MemCastle core.

Enable the bundled source with `memcastle source enable github`.
Install `curl` and `git` on the daemon's `PATH` before mining; Git also needs a writable `/tmp` for temporary bare clones.
For private repositories, set `GH_TOKEN` or `GITHUB_TOKEN` in the **daemon's** environment (in that precedence order), never in `memcastle.toml`.
A public repository can be mined without a token, subject to GitHub's rate limits.
The source grants these two environment names, the three programs `curl`, `git` and `rm`, and no WASI filesystem or ambient network access.
Git runs only against `github.com` wiki clone URLs; a fixed credential helper gives Git the token through its environment, not an argument or a file.

```toml
[[miners]]
name = "acme-github"
source = "github"
credential = { type = "env", name = "GH_TOKEN" }

[miners.options]
include = ["acme/*"]
exclude = ["acme/private-experiment"]
wiki_include = ["acme/docs"]

comments = true
reviews = true
since = "2026-01-01"
```

`include` is required: there is no implicit account-wide scope.
An exact `owner/repo` names one repository; `org/*` enumerates an organization's repositories and `owner/repo-*` selects a subset.
An explicit owner wildcard (`*/repo`) enumerates accessible repositories and needs a token.
`exclude` takes precedence over `include`.
`wiki_include` is a separate set of patterns **within** the selected repositories, and `wiki_exclude` takes precedence over it.
To mine only wikis, set `issues = false`, `pulls = false` and `metadata = false`, as well as `wiki_include`.
The `labels` filter selects issues and PRs with at least one named label; `topics` selects repositories, and `paths` selects wiki page paths.
`comments` and `reviews` default to false, `issues`, `pulls` and `metadata` to true.

The adapter validates and canonicalizes scope before creating a job.
Changing a scope selector creates an independent source identity and cursor; `since` narrows a run without changing identity, so use `memcastle miner run acme-github --full` to move the bootstrap boundary backward.
REST and Git pagination/history are bounded; narrow your selectors if a list exceeds 10,000 entries.
Git wiki commit history is discovered in Git's history order, and a rewritten history requires a full mine.
A changed issue, comment or review is discovered by its `updated_at` timestamp; changes whose upstream timestamps move backward require a full mine.
Polling and scheduled triggers can run the miner, but no trigger is enabled by defining it and webhook bodies never enter the source.

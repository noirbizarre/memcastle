# ChatGPT conversation source

The `chatgpt` source mines one document per conversation, without making model calls.
It accepts an official ChatGPT export ZIP with a top-level `conversations.json`, or a standalone `conversations.json`.
The `mode=web` option is an **experimental** alternative using ChatGPT's undocumented private web interface.
It has been tested with synthetic responses, not a fresh live session; its authentication and endpoints may change.

```sh
memcastle source install sources/chatgpt --enable
memcastle mine chatgpt /path/to/chatgpt-export.zip
# Or, after extracting the archive:
memcastle mine chatgpt /path/to/conversations.json
```

Both paths must exist on the daemon's machine.
The source is identified by the canonical path to the export file: replacing that file with a newer export preserves its history.
Choosing a different path creates a different source identity.
Each run scans all conversations and the pipeline skips documents whose content hash has not changed.
At most 128 MiB of JSON and 16 MiB per conversation are accepted; split larger exports before importing.
Raw conversation JSON is retained in MemCastle, including branch relationships, message metadata and any attachment/reference descriptors the export contains.
Only text messages by the user and assistant become searchable segments; attachments are not downloaded, and unsupported content is left in raw JSON.
No export can restore conversations already deleted at the origin.

## Experimental live acquisition

The captured desktop app sent both a Bearer token and cookies, but a fresh desktop-compatible PKCE sign-in proved a
renewed bearer alone can read history and projects when curl sends a browser-style User-Agent.
Plain curl and Python's default User-Agent received an HTML 403 edge challenge even with the same bearer; storing a
cookie from `auth.openai.com` does not fix that.
The source declares the tested desktop OAuth client and keeps its refresh token through MemCastle's credential store;
the daemon hands the source a renewed access token for each web call, and no browser cookie is stored.
OpenAI's *documented third-party* Sign in with ChatGPT flow does not grant history access, so this desktop-compatible
private interface may change without notice.
Sign in on the daemon's machine (browser OAuth needs its loopback callback), then mine:

```sh
memcastle source auth chatgpt
memcastle mine chatgpt mode=web account=personal
```

Export ZIP/JSON imports remain usable while signed out.
The old manual-session environment fallback also remains available when a fresh OAuth sign-in is not possible:

```text
MEMCASTLE_CHATGPT_BEARER=<fresh ChatGPT web access token>
MEMCASTLE_CHATGPT_COOKIE=<fresh ChatGPT web cookie header, if required>
```

Set these outside MemCastle's config and restart the daemon so it inherits them.
An environment bearer takes precedence over OAuth, so remove it to use `source auth chatgpt`.
Do not put them in `memcastle mine` options, project files, a shell history entry or a saved HAR.
The source's process grant runs `curl` with a fixed `https://chatgpt.com` origin, sends headers through standard input, refuses redirects and caps each command's response at 16 MiB.
Installing the source shows the `locator` read grant (for a file, its parent directory), `curl` process grant and the two environment-variable grants for consent.
Use an account label that is **not** a token and that differs across accounts:

```sh
memcastle mine chatgpt mode=web account=personal
```

An unfiltered web run reads ordinary and project conversations, including archived projects.
To mine only named projects, use `projects=name:Exact Name`, or `projects=id:PROJECT_ID` for an unambiguous selection.
Separate multiple selectors with commas: `projects=id:ID_A,name:Other Project`.
Names must be exact and unique; a renamed project stops matching, and a newly created project with the old name may
be selected on a later run, so use IDs for unattended miners.
A name containing a comma must use its ID instead.
Each selected set of projects has its own source identity and cursor; omitting the option keeps the original unfiltered
identity.
An export file does not support `projects` because its project membership has not been verified.
The project sidebar only discovers names and IDs; this source follows the per-project conversation pages for history.
Project membership in raw document metadata comes from those listings, even when conversation details omit it.
Large accounts may need a higher `mining.source_timeout_secs` for discovery; each call remains bounded by both the daemon
setting and this source's 600-second maximum.

Define a persistent, scoped miner using the existing miner settings:

```sh
memcastle miner set chatgpt-project --source chatgpt \
  --scope 'projects=name:Exact Name' --setting mode=web --setting account=personal
memcastle miner run chatgpt-project
```

The daemon must have `curl` installed.
OAuth tokens are renewed by MemCastle; manually supplied sessions still expire without automatic renewal.
HTTP 401/403 means check the sign-in and session or retry when the private interface changes; HTTP 429 means retry later.
Conversation lists can change during offset paging: the source rejects detected drift and rescans on a later run.
The source revisits the list on each run so edited old conversations are not hidden behind a timestamp watermark.
Message pagination is bounded and fails instead of storing a partial transcript.
The interface may change without notice, including the need for additional headers or a different sign-in.
Validate it with a fresh user-owned session before relying on unattended mining.

## Development

`mise run sources:test -- chatgpt` builds the component and runs its synthetic export conformance cases.
`tests/wasm_chatgpt.rs` also exercises the source against a stand-in `curl`, not a real account.
Keep exports, session credentials and traffic captures private: they contain conversation text and account access.

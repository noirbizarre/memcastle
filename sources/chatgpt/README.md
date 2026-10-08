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

The captured ChatGPT web application used a Bearer authorization header **and** cookies; it did not show how a session is issued or renewed.
There is no demonstrated public-client OAuth grant authorizing these endpoints.
The source therefore does not declare OAuth and does not accept an OpenAI API key.
If you choose the experimental backend, arrange for a **fresh** session in the daemon's environment:

```text
MEMCASTLE_CHATGPT_BEARER=<fresh ChatGPT web access token>
MEMCASTLE_CHATGPT_COOKIE=<fresh ChatGPT web cookie header, if required>
```

Set these outside MemCastle's config and restart the daemon so it inherits them.
Do not put them in `memcastle mine` options, project files, a shell history entry or a saved HAR.
The source's process grant runs `curl` with a fixed `https://chatgpt.com` origin, sends headers through standard input, refuses redirects and caps each command's response at 16 MiB.
Installing the source shows the `locator` read grant (for a file, its parent directory), `curl` process grant and the two environment-variable grants for consent.
Use an account label that is **not** a token and that differs across accounts:

```sh
memcastle mine chatgpt mode=web account=personal
```

The daemon must have `curl` installed.
Sessions expire and cannot currently be renewed by this source.
HTTP 401/403 means replace the session; HTTP 429 means retry later.
Conversation lists can change during offset paging: the source rejects detected drift and rescans on a later run.
The source revisits the list on each run so edited old conversations are not hidden behind a timestamp watermark.
Message pagination is bounded and fails instead of storing a partial transcript.
The interface may change without notice, including the need for additional headers or a different sign-in.
Validate it with a fresh user-owned session before relying on unattended mining.

## Development

`mise run sources:test -- chatgpt` builds the component and runs its synthetic export conformance cases.
`tests/wasm_chatgpt.rs` also exercises the source against a stand-in `curl`, not a real account.
Keep exports, session credentials and traffic captures private: they contain conversation text and account access.

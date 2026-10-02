# Changelog

All notable changes to this project will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.1.0 - 2026-10-02

### 💫 Features

- **app** Add MemoryMode with per-session enforcement ([#55](https://github.com/noirbizarre/memcastle/issues/55)) - ([b2af56c](https://github.com/noirbizarre/memcastle/commit/b2af56c86fd4e54bd0089548c66ecf32e29a16a6))
- **app** Add AppServices::recall and AppServices::wake_up ([#54](https://github.com/noirbizarre/memcastle/issues/54)) - ([daab62e](https://github.com/noirbizarre/memcastle/commit/daab62ef5b67ec8a3ae65f8037b8214cfbe7a49d))
- **app** Add AppServices::diary_write / diary_read (direct call) ([#53](https://github.com/noirbizarre/memcastle/issues/53)) - ([e6eda2f](https://github.com/noirbizarre/memcastle/commit/e6eda2f89cee440a8049aef5eb66d88aad02a4d5))
- **audit** Add JobKind::Audit, a read-only palace consistency report ([#56](https://github.com/noirbizarre/memcastle/issues/56)) - ([8d1b7e1](https://github.com/noirbizarre/memcastle/commit/8d1b7e1e9fa9407ccad6409de99b4b73b11b38f2))
- **auth** Add optional token authentication for the daemon ([#111](https://github.com/noirbizarre/memcastle/issues/111)) - ([ce30604](https://github.com/noirbizarre/memcastle/commit/ce30604a99de5a26cdbb33e5543be7214332e2dd))
- **checkpoint** Add JobKind::Checkpoint and its handler ([#52](https://github.com/noirbizarre/memcastle/issues/52)) - ([6c2cea6](https://github.com/noirbizarre/memcastle/commit/6c2cea6190700f61a443ed78a12f70a24fd9a510))
- **cli**  🚨 **breaking** Report daemon, datastore and endpoint state from memcastle status ([#104](https://github.com/noirbizarre/memcastle/issues/104)) - ([b27b9a0](https://github.com/noirbizarre/memcastle/commit/b27b9a0125f18509c2c0f156765d333e83d3b400))
- **config**  🚨 **breaking** Configurable daemon bind address and port ([#103](https://github.com/noirbizarre/memcastle/issues/103)) - ([ddd874e](https://github.com/noirbizarre/memcastle/commit/ddd874e7b641b731ebb667316b0901c0409ab80a))
- **config** Resolve config, data and state paths from Unix XDG dirs ([#102](https://github.com/noirbizarre/memcastle/issues/102)) - ([92a5da5](https://github.com/noirbizarre/memcastle/commit/92a5da533d63a4f7940e21fdeb47e8d76cb81cd2))
- **db** Expose the embedded SurrealDB through an opt-in admin endpoint ([#118](https://github.com/noirbizarre/memcastle/issues/118)) - ([18dc808](https://github.com/noirbizarre/memcastle/commit/18dc8080f55ffb267edb9bcd7d4fe035bb1fa881))
- **domain** Add Priority enum and rework job priority representation ([#46](https://github.com/noirbizarre/memcastle/issues/46)) - ([c52cc89](https://github.com/noirbizarre/memcastle/commit/c52cc892f5963ff8a50af63265557999ca1a8bbb))
- **jobs** Hold running jobs by a heartbeat lease so a remote palace can be shared (ADR-006) - ([6c92cad](https://github.com/noirbizarre/memcastle/commit/6c92cad430db601aa07e5b0afe246e98ead664d2))
- **jobs** Let audit and repair pause and stop on shutdown; configurable drain timeout - ([0068900](https://github.com/noirbizarre/memcastle/commit/00689009f34b4b17d4ecd8b1152ed585a1dff4f0))
- **logging** Add structured daemon logging with JSON output and job lifecycle events - ([27890cd](https://github.com/noirbizarre/memcastle/commit/27890cdf8278c8f0e0cb1a0ab36752e51d2840e0))
- **mcp** Expose repair and job control, and generate the server instructions - ([d9754aa](https://github.com/noirbizarre/memcastle/commit/d9754aa04ae65095fe13aa19d6c13da8fd81ad99))
- **migrate** Add versioned data migrations and SurrealKit-backed schema sync ([#62](https://github.com/noirbizarre/memcastle/issues/62)) - ([32c6ba6](https://github.com/noirbizarre/memcastle/commit/32c6ba6d910987b706ff892b6d5ad2b461e34377))
- **packaging** Ship the systemd user unit in the .deb and .rpm - ([0545cf8](https://github.com/noirbizarre/memcastle/commit/0545cf8c5c33c0e63cf1e91abd050b36fcf27881))
- **packaging** Build .deb and .rpm release packages with nfpm - ([cab0be6](https://github.com/noirbizarre/memcastle/commit/cab0be6986c78c54556eff1201f77be69839ce24))
- **packaging** Add Arch systemd user unit and authentication docs - ([b6fae2e](https://github.com/noirbizarre/memcastle/commit/b6fae2e112c593870125a55230bc8ee80627d07f))
- **release** Add runtime asset resolution and reproducible release packaging ([#109](https://github.com/noirbizarre/memcastle/issues/109)) - ([e6220b2](https://github.com/noirbizarre/memcastle/commit/e6220b2e86dca946971b9f81e5ab3a97ad58f675))
- **release** Enable AUR and Homebrew publishing - ([b941872](https://github.com/noirbizarre/memcastle/commit/b941872ded4a93c5978d6bf61dc6c674c8fb1af5))
- **repair** Add JobKind::Repair, a narrow dry-run-first repair job ([#57](https://github.com/noirbizarre/memcastle/issues/57)) - ([fad38e4](https://github.com/noirbizarre/memcastle/commit/fad38e4280499957147a1c549c42031c433f1d92))
- **search** Add wing/room scope to lexical_search ([#50](https://github.com/noirbizarre/memcastle/issues/50)) - ([d648211](https://github.com/noirbizarre/memcastle/commit/d648211aeda35db3ac834340800e7b9256a79018))
- **store** Add knowledge-graph entity/relationship operations ([#51](https://github.com/noirbizarre/memcastle/issues/51)) - ([e762721](https://github.com/noirbizarre/memcastle/commit/e762721da08a71e221b5edc3d97bda273ed3a5a5))
- **store** Use surrealkv as the only embedded storage backend ([#49](https://github.com/noirbizarre/memcastle/issues/49)) - ([57de598](https://github.com/noirbizarre/memcastle/commit/57de59838dda9301e284ded84ff4a94a214ddac3))
- One error and logging surface for MCP, REST and the CLI - ([442507c](https://github.com/noirbizarre/memcastle/commit/442507c983fbc0b256a50ca2ef424d6c29f7a54e))
- Implement the core daemon architecture - ([1a2bb0a](https://github.com/noirbizarre/memcastle/commit/1a2bb0ae6d3794996b2257017cea94db28a36616))

### 🐛 Bug Fixes

- **api** Report bad job ids and checkpoint payloads like MCP and the CLI - ([0a025cf](https://github.com/noirbizarre/memcastle/commit/0a025cf20327a3db4b1a14158921c82a4a7c2618))
- **api** Report malformed requests with the shared error body - ([a3ed48a](https://github.com/noirbizarre/memcastle/commit/a3ed48a881391bc01dce5395ff2f4f79694cb9e3))
- **api** Structured error bodies and honest client errors - ([f03d8ae](https://github.com/noirbizarre/memcastle/commit/f03d8ae51c1eaeca3a63106f372fb2a6b1d4075c))
- **api** Default repair dry_run to true when omitted - ([102d787](https://github.com/noirbizarre/memcastle/commit/102d787e9f44a8c7bd7af3b8c23053849cd88e01))
- **app** Reject relative mine paths at submission and name recall in its own rejection - ([976da90](https://github.com/noirbizarre/memcastle/commit/976da908868a57f1e2043f12afa0bae5c7ea8126))
- **app** Give provenance.requested_by and source.agent one meaning across writers - ([9a97e22](https://github.com/noirbizarre/memcastle/commit/9a97e226c38da80e55419e32ae0ecac1d8b4ed0e))
- **app** Read-only operations must not write - ([e2c2033](https://github.com/noirbizarre/memcastle/commit/e2c20331611c2a859ae928aa70fb8d71a8f2549e))
- **app** Stop leaking checkpoint content through job endpoints - ([208c542](https://github.com/noirbizarre/memcastle/commit/208c542861913a73d1a6d6eec16b47a955c441fc))
- **ci** Regenerate mise.lock with full platform lockfile data - ([53a5e8a](https://github.com/noirbizarre/memcastle/commit/53a5e8a777c8c5d920f7f4773ff0e18db72cf28c))
- **cli** Wait for the old daemon to be gone before restarting; portable mining test - ([ee35973](https://github.com/noirbizarre/memcastle/commit/ee3597396ae24e710c32330a86e31e17f11c4871))
- **cli** Restart flags, absolute mine paths, working -v, and --mode - ([b4db200](https://github.com/noirbizarre/memcastle/commit/b4db2008c1444ce268ad9d5e327ae79bb37b4b38))
- **config** Redact the remote database password like the auth token - ([d785b93](https://github.com/noirbizarre/memcastle/commit/d785b9384598d4aa9114ce02a5f3b9a70a562d59))
- **config** Reject malformed MEMCASTLE_* overrides instead of silently ignoring them - ([e2f31f3](https://github.com/noirbizarre/memcastle/commit/e2f31f340173b7599b0270cba6c3bd0cf456b169))
- **config** Honour logging.level when initializing tracing - ([2e0e7d2](https://github.com/noirbizarre/memcastle/commit/2e0e7d28299a389e3e2aa9852bcd3dad4b787495))
- **error** Name statuses, events and modes by their wire names in messages - ([60ba139](https://github.com/noirbizarre/memcastle/commit/60ba139f6d341bdf8d3a988d30d4027ed1064386))
- **hooks** Catch multi-line grouped imports and all DDL kinds - ([e369acd](https://github.com/noirbizarre/memcastle/commit/e369acd06860f2ddefefd93731c00c9cf78febed))
- **jobs** Guard cancel, resume and retry on the status they read - ([2dc82db](https://github.com/noirbizarre/memcastle/commit/2dc82db114ebdb079ca7c11cb9e0f54e9dd01566))
- **jobs** Count crash recoveries, not every claim, against the attempt budget - ([3ae9cf5](https://github.com/noirbizarre/memcastle/commit/3ae9cf52ace524c32af69c2c9118b964e755d61e))
- **jobs** Persist pause and cancel requests - ([253b5ba](https://github.com/noirbizarre/memcastle/commit/253b5ba1b88d71b55153a04758139b526dffc598))
- **jobs** Make resume after a crash idempotent - ([8bb8c08](https://github.com/noirbizarre/memcastle/commit/8bb8c086c217e73170b6cd7fb6ac8dca8efa274e))
- **jobs** Release the lease when a job leaves Running - ([82f1fd1](https://github.com/noirbizarre/memcastle/commit/82f1fd14ebbd170db15f95a81ffd52fb21a111d4))
- **jobs** Retry a failed job through Job::apply instead of a hand-rolled mutation ([#48](https://github.com/noirbizarre/memcastle/issues/48)) - ([af45b11](https://github.com/noirbizarre/memcastle/commit/af45b11797506b3d10fab568a6733ea34c546955))
- **mcp** Scope a session's mode to that session and refuse headerless set_mode - ([9762e94](https://github.com/noirbizarre/memcastle/commit/9762e9437ae59fd187bc76b071f1ebe0bd07ef26))
- **repair** Validate based_on_job when a repair is submitted - ([0d12602](https://github.com/noirbizarre/memcastle/commit/0d12602cbb62109b8c8681f6664509aee5a68b85))
- **server** Drain in-flight jobs on shutdown - ([bc000bd](https://github.com/noirbizarre/memcastle/commit/bc000bdd1aefa8e5091f69ca2946d106f9dac98d))
- **store** Retry job writes that lose a SurrealDB write conflict - ([5344d7b](https://github.com/noirbizarre/memcastle/commit/5344d7b44df9c8e3fc82f33ba62ed63213ef9ae6))
- **test** Poll for job completion instead of a fixed sleep - ([1b8a55b](https://github.com/noirbizarre/memcastle/commit/1b8a55bf31620566f88553ec3ad799e3503bfc75))
- Align the REST, MCP and client surfaces on shared types and behaviour - ([5b2f694](https://github.com/noirbizarre/memcastle/commit/5b2f69457847e76d195f078a6184e8339687866c))
- Report truncation, skip oversize wake-up highlights, reject missing relationships - ([07983ac](https://github.com/noirbizarre/memcastle/commit/07983aca4bb09dd88dc07cc8f661099f5b288e32))

### 🔨 Refactor

- **app** Use the submit_ prefix for every job-submitting method and share the repair default - ([201a620](https://github.com/noirbizarre/memcastle/commit/201a62011a070721b92517644c293f48196b9575))
- **app**  🚨 **breaking** Consistent AppServices and DaemonClient signatures - ([02cdeb6](https://github.com/noirbizarre/memcastle/commit/02cdeb641a2f3e446d204c994eedead2b119751f))
- **error**  🚨 **breaking** Diagnostic code and variant hygiene - ([e6b5590](https://github.com/noirbizarre/memcastle/commit/e6b5590c7affe2beb6f2bd69ff3cc6a16a954424))
- **error** Typed diagnostics instead of Error::Config misuse - ([4c39da9](https://github.com/noirbizarre/memcastle/commit/4c39da9f407eff21fde2b47ea76cb9617c4d2a20))
- **jobs** Share the stop check and the resume-index parse across handlers - ([1b5464b](https://github.com/noirbizarre/memcastle/commit/1b5464b6ccb1a246a61f6eb5cfc5420454624bc2))
- **jobs** Give every job handler the run(ctx, job, params) shape - ([f3f65c2](https://github.com/noirbizarre/memcastle/commit/f3f65c24db42c43d41025401fb4ce7cb29666ba4))
- **mcp** Name the job list tool and mode labels consistently with job_get - ([ea3f089](https://github.com/noirbizarre/memcastle/commit/ea3f08939630a93dfb6ff4b39e4118916f912046))
- **mining** Introduce a MiningSource seam behind JobKind::Mine ([#58](https://github.com/noirbizarre/memcastle/issues/58)) - ([8b9d531](https://github.com/noirbizarre/memcastle/commit/8b9d5311fd84d4b9ed9eaf1b37b70cb1aa4d7dcf))
- **store** Pin one canonical timestamp form and record why (ADR-005) - ([1638ece](https://github.com/noirbizarre/memcastle/commit/1638ece9be248bde9e13f5317e54ec6c0cd318de))
- **store** Consistent naming, ownership and no duplicate methods - ([478763d](https://github.com/noirbizarre/memcastle/commit/478763d742ad5d06b796b8db1b84d788e54d2903))
- Share scheduler defaults and the diary room name, and declare tokio's net feature - ([610171e](https://github.com/noirbizarre/memcastle/commit/610171ebe0ed482ca54f01b232df9321f61a374f))
- Share the job id and status parsers and the channel names across cli, api and mcp - ([15ed33f](https://github.com/noirbizarre/memcastle/commit/15ed33f93eb5e7d79bd98624a642a13568f18ac2))
- Share drawer construction, checkpointing and defaults - ([e2c6926](https://github.com/noirbizarre/memcastle/commit/e2c6926a3ddd7b9ce08df48a0b4da77b8724edad))

### 📚 Documentation

- **adr** Date the amendments and point to them from Status and the index - ([74d407b](https://github.com/noirbizarre/memcastle/commit/74d407b8f1cb72b26a2ded662c59eca5f98d272e))
- **adr** Mark ADR-011's rejected warning as reversed - ([d39a136](https://github.com/noirbizarre/memcastle/commit/d39a1364db7ea9855bf6f55cda7a031fcf634754))
- **adr** Align ADR-010 with the documented Windows support - ([ae73c16](https://github.com/noirbizarre/memcastle/commit/ae73c16786202ca6f42dec6320ddbc39874282be))
- **adr** Mark ADR-002's gated-operation list as amended by ADR-007 - ([d258095](https://github.com/noirbizarre/memcastle/commit/d258095c5cb6191d068650cec75d3ec9f22096de))
- **adr** Record the Phase 1 hardening decisions - ([6267b82](https://github.com/noirbizarre/memcastle/commit/6267b826b7610bef277f2425d8d503332214197d))
- **agents** List auth-token among the REST routes - ([b1f06b8](https://github.com/noirbizarre/memcastle/commit/b1f06b82a568ddc25b261915ec196082745d36ff))
- **api** Name job reads among the gated routes - ([292eef6](https://github.com/noirbizarre/memcastle/commit/292eef69c4ddd9d48e6b96ffd1c4eb823416f927))
- **architecture** Stop listing .deb/.rpm packages as a non-goal - ([b241478](https://github.com/noirbizarre/memcastle/commit/b24147852000106c3f2ab7ac0e15eab4104e2d58))
- **architecture** Document Phase 1 decisions and add ADRs ([#59](https://github.com/noirbizarre/memcastle/issues/59)) - ([947c357](https://github.com/noirbizarre/memcastle/commit/947c357984efd6579bc43616ba256de20f5500e2))
- **auth** State that auth::not_configured fires after migrations - ([ca61891](https://github.com/noirbizarre/memcastle/commit/ca61891e016ebccf632028038749200c8f02b802))
- **cli** List the commands that do not fail with not_running or print only JSON - ([5cfb3a8](https://github.com/noirbizarre/memcastle/commit/5cfb3a8c095ace95b3d3fa4fb47e029532890b99))
- **code** Correct stale doc comments that contradicted the code or the docs - ([a28faca](https://github.com/noirbizarre/memcastle/commit/a28faca75008bbfe12dc84a92edbd9ef4104f380))
- **config** Correct the section count in the module comment - ([c2c312e](https://github.com/noirbizarre/memcastle/commit/c2c312e22b018ce379cf31ec2ac9f588d8abb19a))
- **configuration** Align the --mode scope with the CLI reference - ([60832d3](https://github.com/noirbizarre/memcastle/commit/60832d3b5bac818bd46b9dd585738d5a5e92069c))
- **contributing** Use `mise cli` in the task table - ([61f7e80](https://github.com/noirbizarre/memcastle/commit/61f7e8084f50d37ead3a3f17eb6f9aaa87187672))
- **daemon** Job started logs the attempt, not the elapsed time - ([42bbd96](https://github.com/noirbizarre/memcastle/commit/42bbd96c8ee4d44b5ab8e754b9ed026413300c40))
- **daemon** Say what actually runs before the listener binds and when migrations run - ([4eb27a5](https://github.com/noirbizarre/memcastle/commit/4eb27a568130fffd3a58cef5f4ad7b3cbfee0ee4))
- **daemon** Note the packaged unit pins MEMCASTLE_LOG=warn - ([592e616](https://github.com/noirbizarre/memcastle/commit/592e616798bc45b8020f32f927aa90d74f0b1a07))
- **development** List config_assets and dependencies tests - ([750f060](https://github.com/noirbizarre/memcastle/commit/750f060bc7b6512c35b1e6e1304e82babaee239a))
- **development** Describe tests/auth.rs as walking a route list - ([82eb9e8](https://github.com/noirbizarre/memcastle/commit/82eb9e809342ae7f4549a55d0de62c126169e28a))
- **installation** Count the AUR package's licence file - ([1aa435f](https://github.com/noirbizarre/memcastle/commit/1aa435f4c81e5ddd4111af89517c0005a591e6b5))
- **logo** Added the Shaipe SVG and both icon renders ([#117](https://github.com/noirbizarre/memcastle/issues/117)) - ([ac9e360](https://github.com/noirbizarre/memcastle/commit/ac9e360a5bfebb33e6b6ce20cb15cb95b40f3da3))
- **scaffold** Call out non-goals in integrations/skills READMEs ([#60](https://github.com/noirbizarre/memcastle/issues/60)) - ([adef6d1](https://github.com/noirbizarre/memcastle/commit/adef6d1693d32a844e8f3165227fbacfb2948049))
- **storage** Say a second serve on the same port fails with bind_failed first - ([4b0ed0a](https://github.com/noirbizarre/memcastle/commit/4b0ed0a54ee4ae9b8e40084f7f166d02ac9271fa))
- **storage** Document the dot-directory and symlink skips in mining - ([c93210b](https://github.com/noirbizarre/memcastle/commit/c93210be8d361155d9ba900f1981ce89d3e16741))
- **storage** Remote stores use their configured namespace and database - ([1ba1ef0](https://github.com/noirbizarre/memcastle/commit/1ba1ef0192cdea8343a139271cf7795359d0d7e4))
- Link database access from the indexes and align the unit, hook and layering wording - ([1a13ec0](https://github.com/noirbizarre/memcastle/commit/1a13ec0cf52925918242084cb113df38195e62da))
- Rewrap lines to satisfy markdownlint - ([e24a4c6](https://github.com/noirbizarre/memcastle/commit/e24a4c6ac1c5917b46d39641bf1d90507dae6fa1))
- Say mise run ci also covers the docs build - ([3fe2160](https://github.com/noirbizarre/memcastle/commit/3fe2160a181b4ea21ea883c126a0c38d3074330c))
- Link the Authentication guide from the index and README - ([e8f6e01](https://github.com/noirbizarre/memcastle/commit/e8f6e01770a1d7e996afa1fa3bd7e5149f770046))
- Say mine needs an absolute path over REST as well as MCP (fixup) - ([c345b7f](https://github.com/noirbizarre/memcastle/commit/c345b7f85a33ef56876acae6ba61ca3ece52ae5e))
- Mention the /mcp Host-header restriction where binding is described - ([e752511](https://github.com/noirbizarre/memcastle/commit/e75251102137e130dd7561e4b4832991ec0618bc))
- Say mine needs an absolute path over REST as well as MCP - ([7e3ab15](https://github.com/noirbizarre/memcastle/commit/7e3ab150d5f085060369b704a43547cc8ad9be95))
- Show the auth line in the sample status output - ([967665f](https://github.com/noirbizarre/memcastle/commit/967665fcc15e86d4a71a387ff554840c812ba0de))
- List --assets-dir among the flags restart forwards - ([dc8405c](https://github.com/noirbizarre/memcastle/commit/dc8405c3e03d9fcac752dcbff1c60a191f2325a0))
- Overhaul user documentation and add architecture diagrams - ([3d5aa58](https://github.com/noirbizarre/memcastle/commit/3d5aa589b3b388c3800840f27639c877a9efd297))
- Correct stale docstrings and comments that no longer match the code - ([7219607](https://github.com/noirbizarre/memcastle/commit/7219607c49da7765a505bed2d692ae49fcf1540d))
- Reconcile the palace, ADR, recovery, server-deployment and configuration wording - ([2aafd7d](https://github.com/noirbizarre/memcastle/commit/2aafd7db9e571fb90ea2e9a4773ab51769c6c718))
- Name restart and migrate as the exceptions to the thin-client rule - ([bafd5e8](https://github.com/noirbizarre/memcastle/commit/bafd5e8a29f6071d4fdc1acf569167cd4e6596d0))
- Fix the toolchain and git-tpl pointers in the development guide - ([9a684f2](https://github.com/noirbizarre/memcastle/commit/9a684f2acbf88854c5c319feea39c55bc87976f4))
- Scope the one-writer invariant to embedded palaces - ([c3ae697](https://github.com/noirbizarre/memcastle/commit/c3ae697a1a2b119e664e7b8e6e4e2e00a397e3aa))
- Fix drift found by the consistency check - ([85460b1](https://github.com/noirbizarre/memcastle/commit/85460b1f1983432ce1b2ff7dbae9420138e79eaf))
- Document the migrate exception, retry transition and Phase 1 status - ([390dc44](https://github.com/noirbizarre/memcastle/commit/390dc448cb52b012735459013defad6315e2e4f3))
- Add V1 architecture roadmap and integrations/skills scaffold - ([91df823](https://github.com/noirbizarre/memcastle/commit/91df8235573967ec15ed6456ee45360c3eadbf1d))

### 🧪 Tests

- **concurrency** Compare completion times instead of racing a poll - ([434345b](https://github.com/noirbizarre/memcastle/commit/434345b12e6c9ef8dc3be59a906b6f951b0343ff))
- **jobs** Exercise Scheduler::recover for every job state - ([132e2cc](https://github.com/noirbizarre/memcastle/commit/132e2ccb74b00085f171856b6a2df1257860da31))
- **mcp** Add an end-to-end MCP smoke test ([#105](https://github.com/noirbizarre/memcastle/issues/105)) - ([95bf22f](https://github.com/noirbizarre/memcastle/commit/95bf22fe2a23ced41aad489f468ffeb6c8e7800d))
- **mcp** Cover recall, wake_up, diary and emergency checkpoint by mode - ([b5888ed](https://github.com/noirbizarre/memcastle/commit/b5888ed112f831e1773d8f778aa088aefb601604))
- Enforce invariants 2 and 5 with a full transition table and two guard hooks - ([0465a3b](https://github.com/noirbizarre/memcastle/commit/0465a3b76e6e1c2c4b6f90e3984e8f54065d005b))
- Capture restart's output through files so Windows does not hang - ([e4bd2a5](https://github.com/noirbizarre/memcastle/commit/e4bd2a5b3677c55ae394812031d9150f7e012639))
- Read diagnostic codes from the errors instead of spelling jobs codes in mcp and api - ([30709c6](https://github.com/noirbizarre/memcastle/commit/30709c61dc5d297ed743252987a740af0ff03196))

### 🏗️ Build

- Run the architecture guards in check and ci, and align how the docs describe them - ([37034f6](https://github.com/noirbizarre/memcastle/commit/37034f6eb2869eb0a94897eafe27c1dcbaa0a04e))

### 🔧 CI

- **guard** Widen the architecture guard to jobs, client and main.rs - ([31e0e88](https://github.com/noirbizarre/memcastle/commit/31e0e88d8c67322520d0bf24776f6ef155d6b940))
- Cache Rust compilation with sccache, faster linkers per OS ([#45](https://github.com/noirbizarre/memcastle/issues/45)) - ([8cf2120](https://github.com/noirbizarre/memcastle/commit/8cf2120dc274917482c9ab4dff8bb6c2c5c28448))

### 🧹 Chores

- Drop unused dependencies, assert what two CLI tests promise, share one job-wait helper - ([3da89bf](https://github.com/noirbizarre/memcastle/commit/3da89bf3251eb351a8b038c2d1bab6bc08e13fe7))
- Initialize project from rust.tpl - ([1fc71a6](https://github.com/noirbizarre/memcastle/commit/1fc71a6f98fb3a46c175dcc25a1083d0b9525495))

### Tpl

- Render rust at main - ([9e0948b](https://github.com/noirbizarre/memcastle/commit/9e0948bc1e48d5f5d0cc890df22428dd94996d39))
- Render rust at <worktree> - ([807de5e](https://github.com/noirbizarre/memcastle/commit/807de5e7307dc17a497a4c90bd09b5acc34b3c01))

## ❤️ New Contributors

* @noirbizarre made their first contribution in [#119](https://github.com/noirbizarre/memcastle/pull/119)

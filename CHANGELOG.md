# Changelog

## [0.25.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.24.0...v0.25.0) (2026-09-26)


### ⚠ BREAKING CHANGES

* **runtime:** ProfileKey::token returns Option, ProfileKey gains Deferred, and Endpoint::token is Option<EndpointToken>.
* **runtime-api:** one TrajectoryId for the engine, the log and the runtime ([#478](https://github.com/archestra-ai/OpenAPPA/issues/478))
* **eventlog:** receipts keyed by session scope and binding; one leased surface ([#473](https://github.com/archestra-ai/OpenAPPA/issues/473))
* **runtime:** let a host that embeds the runtime bring its own adapter ([#384](https://github.com/archestra-ai/OpenAPPA/issues/384))

### Features

* **appa-agent:** expose identified parallel calls in Python ([#404](https://github.com/archestra-ai/OpenAPPA/issues/404)) ([8c653aa](https://github.com/archestra-ai/OpenAPPA/commit/8c653aa8e1580b833921f43b8ed81ed4bedc73a6))
* **batteries:** add monday.com policy battery ([#398](https://github.com/archestra-ai/OpenAPPA/issues/398)) ([2e66d2d](https://github.com/archestra-ai/OpenAPPA/commit/2e66d2d48c65370d97ce3d8c729fb789d0a39f5d))
* **batteries:** jev battery, an annotator answered by TypeSafe's Jev model ([#394](https://github.com/archestra-ai/OpenAPPA/issues/394)) ([57f05c1](https://github.com/archestra-ai/OpenAPPA/commit/57f05c10ba3fe5ee4335621e951f7e52803a8b89))
* **batteries:** trust follows who can write the text ([#455](https://github.com/archestra-ai/OpenAPPA/issues/455)) ([ec689f0](https://github.com/archestra-ai/OpenAPPA/commit/ec689f046fb95cdfb9c9338b4b006bf75dc59e24))
* **batteries:** xmemory battery over the instance and admin MCP servers ([#420](https://github.com/archestra-ai/OpenAPPA/issues/420)) ([3ce2b0a](https://github.com/archestra-ai/OpenAPPA/commit/3ce2b0ac64de5e643e53131f568a2eced18c31e7))
* **bench:** adapt concurrency to live pressure ([c8a9f35](https://github.com/archestra-ai/OpenAPPA/commit/c8a9f35f054c785a831d86e77bda70d103e94c4a))
* **bench:** auto-scaling for benchmarks ([#405](https://github.com/archestra-ai/OpenAPPA/issues/405)) ([c8a9f35](https://github.com/archestra-ai/OpenAPPA/commit/c8a9f35f054c785a831d86e77bda70d103e94c4a))
* **claude-code:** reassess the shipped default for clappa ([#477](https://github.com/archestra-ai/OpenAPPA/issues/477)) ([6171f36](https://github.com/archestra-ai/OpenAPPA/commit/6171f3624bd472715db6d98c5fcb483dbac07a2b))
* **engine:** array arguments in selector placeholders and argument selectors ([#411](https://github.com/archestra-ai/OpenAPPA/issues/411)) ([c320247](https://github.com/archestra-ai/OpenAPPA/commit/c3202472d0a4e43cd8ac4b453bd0186d8deef484))
* **files:** make taint ledger session-local ([#412](https://github.com/archestra-ai/OpenAPPA/issues/412)) ([a4350b7](https://github.com/archestra-ai/OpenAPPA/commit/a4350b7491f30d25c3736768f7fce2ef682b93cd))
* **init:** clappa carries APPA's status line; the user's statusLine stays theirs ([#476](https://github.com/archestra-ai/OpenAPPA/issues/476)) ([c12c75f](https://github.com/archestra-ai/OpenAPPA/commit/c12c75ff14e4246f19e230ab3f96e05fb50525a4))
* **jev:** native jev annotator builtin with a pooled, hedged client ([#423](https://github.com/archestra-ai/OpenAPPA/issues/423)) ([5d5fb9a](https://github.com/archestra-ai/OpenAPPA/commit/5d5fb9a603009ce02b93c717dff60b01d350df7a))
* **jev:** retry transient TypeSafe failures and emit a diagnostics line ([#401](https://github.com/archestra-ai/OpenAPPA/issues/401)) ([f6db908](https://github.com/archestra-ai/OpenAPPA/commit/f6db9087c3463c957d0c28c1b9debeefb914fef5))
* **marketplace:** archestra battery ([#414](https://github.com/archestra-ai/OpenAPPA/issues/414)) ([9dda479](https://github.com/archestra-ai/OpenAPPA/commit/9dda4795a7821c929dadef32678c04353bff7f68))
* **repo:** add battery script linter ([#397](https://github.com/archestra-ai/OpenAPPA/issues/397)) ([8dcbd75](https://github.com/archestra-ai/OpenAPPA/commit/8dcbd750a83b0a9e33a401f86e28bf91183931d1))
* **runtime:** a hosted root document declares its batteries and their credentials ([#386](https://github.com/archestra-ai/OpenAPPA/issues/386)) ([8dd1272](https://github.com/archestra-ai/OpenAPPA/commit/8dd1272f4e093856a11ddc11d653180397afdc7d))
* **runtime:** annotator inputs a program of the deployment answers; the claude-code battery establishes the repository a push reaches ([#395](https://github.com/archestra-ai/OpenAPPA/issues/395)) ([159eb95](https://github.com/archestra-ai/OpenAPPA/commit/159eb95a43e96b569193597c2d6cefc9ee27c430))
* **runtime:** export bounded OpenTelemetry signals ([5555b90](https://github.com/archestra-ai/OpenAPPA/commit/5555b90ba963ef968d0778637b77de53d0cb9a0c))
* **runtime:** implement bounded OpenTelemetry observability ([#430](https://github.com/archestra-ai/OpenAPPA/issues/430)) ([5555b90](https://github.com/archestra-ai/OpenAPPA/commit/5555b90ba963ef968d0778637b77de53d0cb9a0c))
* **runtime:** let a host that embeds the runtime bring its own adapter ([#384](https://github.com/archestra-ai/OpenAPPA/issues/384)) ([4f2b89e](https://github.com/archestra-ai/OpenAPPA/commit/4f2b89e05baef8a74c4a4ded3b8214996de01ae5))
* **runtime:** pin a deployment per dispatch and take a host credential lookup ([#433](https://github.com/archestra-ai/OpenAPPA/issues/433)) ([397cd7e](https://github.com/archestra-ai/OpenAPPA/commit/397cd7e4c098c51c3a9d2af22e1661d175a838f2))
* **runtime:** record external consults for an embedding host ([#402](https://github.com/archestra-ai/OpenAPPA/issues/402)) ([7468837](https://github.com/archestra-ai/OpenAPPA/commit/7468837da348135cc018fc24cc82e3d18318e1e6))
* **runtime:** session principal answers the self audience ([#406](https://github.com/archestra-ai/OpenAPPA/issues/406)) ([03c5f4b](https://github.com/archestra-ai/OpenAPPA/commit/03c5f4b84986baa3417e16b52fae9ac79b1726f6))
* **runtime:** share one label guide between jev and the model annotators ([#424](https://github.com/archestra-ai/OpenAPPA/issues/424)) ([c7c5381](https://github.com/archestra-ai/OpenAPPA/commit/c7c5381b07eb2833f2ed31716e11d6fcf65b4e54))
* **website:** header mascot dances to the song ([#418](https://github.com/archestra-ai/OpenAPPA/issues/418)) ([61a1ab5](https://github.com/archestra-ai/OpenAPPA/commit/61a1ab5bed3300be4410964a93699d0849ba7b75))
* **website:** one-line summaries on comparison pages ([#425](https://github.com/archestra-ai/OpenAPPA/issues/425)) ([f0e66ed](https://github.com/archestra-ai/OpenAPPA/commit/f0e66eda8b484a170024882318172d99156d4dcf))
* **website:** replace pronunciation clip with song in hero button ([#399](https://github.com/archestra-ai/OpenAPPA/issues/399)) ([e39fe0b](https://github.com/archestra-ai/OpenAPPA/commit/e39fe0b81d7ffda3baa07d7dc50a4e6330b68e1c))
* **website:** song survives navigation, Claude Auto mode benchmark row ([#431](https://github.com/archestra-ai/OpenAPPA/issues/431)) ([a358122](https://github.com/archestra-ai/OpenAPPA/commit/a3581223b4a606c9e1bc85e9887e254a460a9712))


### Bug Fixes

* **batteries:** bundle only git-listed files; correct the Slack approval example ([#456](https://github.com/archestra-ai/OpenAPPA/issues/456)) ([cc3106d](https://github.com/archestra-ai/OpenAPPA/commit/cc3106d1eaf5a8ac55dba23a291cfe678ff5ec97))
* **batteries:** external-script bugs and tests that could not fail ([#445](https://github.com/archestra-ai/OpenAPPA/issues/445)) ([c193f4d](https://github.com/archestra-ai/OpenAPPA/commit/c193f4d8aa3cc8fbebcde91871c997361e03ff61))
* **batteries:** Linear rule order, claude-code repository targets ([#448](https://github.com/archestra-ai/OpenAPPA/issues/448)) ([b0909b0](https://github.com/archestra-ai/OpenAPPA/commit/b0909b0ec1d32938446f357ec8ade301691aba0f))
* **claude-code:** stop flagging the runtime's server and service status output ([#385](https://github.com/archestra-ai/OpenAPPA/issues/385)) ([79801fe](https://github.com/archestra-ai/OpenAPPA/commit/79801fef5681e86827af89fc38df2ec63655192f))
* **demo:** deploy appa-demo from main ([#413](https://github.com/archestra-ai/OpenAPPA/issues/413)) ([f07e3ac](https://github.com/archestra-ai/OpenAPPA/commit/f07e3ac6b94d0f8aa4f1a8ebf87edd2220920272))
* **demo:** install system CA certificates ([#416](https://github.com/archestra-ai/OpenAPPA/issues/416)) ([cbad757](https://github.com/archestra-ai/OpenAPPA/commit/cbad7574ddd395861c27613606c6db0461687236))
* **demo:** pin the runtime and backport Terra support ([#429](https://github.com/archestra-ai/OpenAPPA/issues/429)) ([5f75f7f](https://github.com/archestra-ai/OpenAPPA/commit/5f75f7f91408f7ccff4f704c72d3f0df7bd8d634))
* **engine:** a call clears under any standing derivation of its bytes ([#383](https://github.com/archestra-ai/OpenAPPA/issues/383)) ([4effe0f](https://github.com/archestra-ai/OpenAPPA/commit/4effe0fcf0c60fcb12b2292798aa60149c75ff2f))
* **engine:** an offer stands exactly while the trajectory's label does ([#382](https://github.com/archestra-ai/OpenAPPA/issues/382)) ([7c74934](https://github.com/archestra-ai/OpenAPPA/commit/7c749345c5b2d8c1fe307efc276bd1a86e562dc9))
* **eventlog:** key receipts by organization ([#464](https://github.com/archestra-ai/OpenAPPA/issues/464)) ([dc2a3ac](https://github.com/archestra-ai/OpenAPPA/commit/dc2a3aceef747621f1bcc8f79ca0699707ae7a91))
* **eventlog:** never follow a parent symlink in the file ledger or executor ([#453](https://github.com/archestra-ai/OpenAPPA/issues/453)) ([5e525c5](https://github.com/archestra-ai/OpenAPPA/commit/5e525c5f08352739e70609594455a9f6c711b456))
* **files:** accept workspace aliases, trim the ledger API, surface metadata errors ([#461](https://github.com/archestra-ai/OpenAPPA/issues/461)) ([4dd9065](https://github.com/archestra-ai/OpenAPPA/commit/4dd90653dd8cca06f9aaaaa6f85d3e33bbc40049))
* **files:** label workspace files on first touch instead of hashing the workspace at bind ([#454](https://github.com/archestra-ai/OpenAPPA/issues/454)) ([aae0426](https://github.com/archestra-ai/OpenAPPA/commit/aae042627c5b3e698a065d899ba71fdb116c34ae))
* **init:** a blank Claude Code settings.json is an empty one ([#441](https://github.com/archestra-ai/OpenAPPA/issues/441)) ([d43fd4d](https://github.com/archestra-ai/OpenAPPA/commit/d43fd4d23b210dbd1133bbf6e7f0f407a347dbd8))
* **init:** activation leaves a secret it cannot see to the runtime ([#443](https://github.com/archestra-ai/OpenAPPA/issues/443)) ([79a82da](https://github.com/archestra-ai/OpenAPPA/commit/79a82da41aed8103a7eb62e85cc1b73ce1f5cb5a))
* **init:** release the Claude profile lock explicitly on drop ([#471](https://github.com/archestra-ai/OpenAPPA/issues/471)) ([501da71](https://github.com/archestra-ai/OpenAPPA/commit/501da7153002554b9926d47fe601faf7b8574699))
* **installation:** Claude Code install path sweep — native stderr, bundle state, revision test ([#436](https://github.com/archestra-ai/OpenAPPA/issues/436)) ([de5a4a4](https://github.com/archestra-ai/OpenAPPA/commit/de5a4a42a6c65cbbd56e2256c163a9bd09fc4e13))
* **installation:** refuse unrecoverable Claude removal, honest agent-yell, https-only installer ([#439](https://github.com/archestra-ai/OpenAPPA/issues/439)) ([06fe06b](https://github.com/archestra-ai/OpenAPPA/commit/06fe06b28031648a09f2e27e4928dce9d8c86dc9))
* **installation:** release the install lock on drop even when a forked child shares it ([#470](https://github.com/archestra-ai/OpenAPPA/issues/470)) ([d75f63a](https://github.com/archestra-ai/OpenAPPA/commit/d75f63a6d9e48d65a392748e467eb6802321bb2d))
* **install:** installer refuses a directory target; upgrade test reaches the rename ([#437](https://github.com/archestra-ai/OpenAPPA/issues/437)) ([ca3f214](https://github.com/archestra-ai/OpenAPPA/commit/ca3f21463b5d891a0dbdc5cddf2c7aa352bf0a09))
* **jev:** settle each choice label on its likeliest option ([#440](https://github.com/archestra-ai/OpenAPPA/issues/440)) ([70e0ad3](https://github.com/archestra-ai/OpenAPPA/commit/70e0ad33f8312af528f88271314df79f92f0ac08))
* **runtime:** expose audience in return remedies ([#452](https://github.com/archestra-ai/OpenAPPA/issues/452)) ([8f9ad51](https://github.com/archestra-ai/OpenAPPA/commit/8f9ad515b0cddfeaf2f0be0b44590f7790463a9f))
* **runtime:** start child processes one at a time on macOS ([#467](https://github.com/archestra-ai/OpenAPPA/issues/467)) ([7338c91](https://github.com/archestra-ai/OpenAPPA/commit/7338c91121e1c638caee4d1752a525048aa4717a))
* **runtime:** validate a hosted document without its model keys ([#479](https://github.com/archestra-ai/OpenAPPA/issues/479)) ([fcecb62](https://github.com/archestra-ai/OpenAPPA/commit/fcecb62a428bc15c63115c641053a74f592ca5f4))
* **taubench:** complete the PR 363 replication package ([#403](https://github.com/archestra-ai/OpenAPPA/issues/403)) ([e783366](https://github.com/archestra-ai/OpenAPPA/commit/e783366b7d10bd1ee25ddbeb4bce727124b79521))
* **website:** fixed header and full-height drawer for iOS Safari ([#458](https://github.com/archestra-ai/OpenAPPA/issues/458)) ([afa1675](https://github.com/archestra-ai/OpenAPPA/commit/afa1675bca0af016ba3c366e35c4eec558f733fb))
* **windows:** make Claude Code plugin fresh installs portable ([#451](https://github.com/archestra-ai/OpenAPPA/issues/451)) ([a4264f2](https://github.com/archestra-ai/OpenAPPA/commit/a4264f243aadf6af08bd9ad6f3e481a1d521d7ba))


### Documentation

* add video to how-it-works page ([d3e503a](https://github.com/archestra-ai/OpenAPPA/commit/d3e503a8b9cc1ad5a3f1f88ca3a86d69fff1004c))
* **batteries:** correct stale battery text and website battery pages ([#444](https://github.com/archestra-ai/OpenAPPA/issues/444)) ([3252061](https://github.com/archestra-ai/OpenAPPA/commit/32520615aef09dfd90481ded5b1b7900a194e175))
* **bench:** add bench README on scoring, judge use, and result sources ([#450](https://github.com/archestra-ai/OpenAPPA/issues/450)) ([05e64b9](https://github.com/archestra-ai/OpenAPPA/commit/05e64b934a8ba641bd0c0701770d10a7d7619dc7))
* **bench:** explain Corp archive provenance mismatch ([#447](https://github.com/archestra-ai/OpenAPPA/issues/447)) ([6646d13](https://github.com/archestra-ai/OpenAPPA/commit/6646d134e5560d74451bd1bfb624c2629b7fea87))
* **bench:** index archived Corp and AgentThreatBench runs ([#435](https://github.com/archestra-ai/OpenAPPA/issues/435)) ([510129a](https://github.com/archestra-ai/OpenAPPA/commit/510129a251b963e7c64e2a92d7e8aa557b5ba2df))
* clarify integration prompt links and TLDR labels ([#434](https://github.com/archestra-ai/OpenAPPA/issues/434)) ([5d3933a](https://github.com/archestra-ai/OpenAPPA/commit/5d3933a1b9e0aaa929527dba3af211bcfb4be8f9))
* compare OpenAPPA with Claude Code auto mode ([#410](https://github.com/archestra-ai/OpenAPPA/issues/410)) ([c0f49b2](https://github.com/archestra-ai/OpenAPPA/commit/c0f49b2e485bf2dcbbecc3392a3b5370a4289d66))
* point policy configuration at the appa-guide skill ([#438](https://github.com/archestra-ai/OpenAPPA/issues/438)) ([c5dd338](https://github.com/archestra-ai/OpenAPPA/commit/c5dd338fde664b1d5dde8fd65d5195fc818acb69))
* proofread recently updated pages ([#422](https://github.com/archestra-ai/OpenAPPA/issues/422)) ([0e3db52](https://github.com/archestra-ai/OpenAPPA/commit/0e3db528ed9950e996c72462f1cc0aafa089d91b))
* remove `#dependency-release-age` section ([#408](https://github.com/archestra-ai/OpenAPPA/issues/408)) ([0703c8a](https://github.com/archestra-ai/OpenAPPA/commit/0703c8a265bdc443e2c98dc027b385937775addb))
* rework landing page storyline and readability ([#417](https://github.com/archestra-ai/OpenAPPA/issues/417)) ([c778c37](https://github.com/archestra-ai/OpenAPPA/commit/c778c37a2663c56eb2408efa0bbcd066126604c8))
* **taubench:** record parallel archive publication ([#432](https://github.com/archestra-ai/OpenAPPA/issues/432)) ([985b7c3](https://github.com/archestra-ai/OpenAPPA/commit/985b7c3615550892065b4e77175dd6ed64748225))
* **website:** add CLI installation to validation guide ([#427](https://github.com/archestra-ai/OpenAPPA/issues/427)) ([02bcd9e](https://github.com/archestra-ai/OpenAPPA/commit/02bcd9e753f9f07f997df1364e6a340516b1a1e4))
* **website:** add integration and operations guides ([#419](https://github.com/archestra-ai/OpenAPPA/issues/419)) ([650a9d3](https://github.com/archestra-ai/OpenAPPA/commit/650a9d3aa8de25d07cab0b02b9daf95eb10c87f8))
* **website:** clarify integration prompt links and TLDR labels ([5d3933a](https://github.com/archestra-ai/OpenAPPA/commit/5d3933a1b9e0aaa929527dba3af211bcfb4be8f9))


### Dependencies

* bump astral-sh/setup-uv from 10.0.1 to 10.1.0 ([#387](https://github.com/archestra-ai/OpenAPPA/issues/387)) ([b15bce1](https://github.com/archestra-ai/OpenAPPA/commit/b15bce15edde625e2a6457a899f3479683be599c))
* bump base64 from 0.22.1 to 0.23.1 ([#393](https://github.com/archestra-ai/OpenAPPA/issues/393)) ([9303cc0](https://github.com/archestra-ai/OpenAPPA/commit/9303cc0e419030198c470dc86453dc0770b0ee72))
* bump docker/build-push-action from 7.3.0 to 7.4.0 ([#389](https://github.com/archestra-ai/OpenAPPA/issues/389)) ([ae1018c](https://github.com/archestra-ai/OpenAPPA/commit/ae1018c361c82f7c315763471b4664884a9460cb))
* bump docker/setup-buildx-action from 4.3.0 to 4.4.0 ([#388](https://github.com/archestra-ai/OpenAPPA/issues/388)) ([b27827f](https://github.com/archestra-ai/OpenAPPA/commit/b27827fe447e3e187d2bc55912d9b71a576d6283))
* bump jsonschema from 0.53.0 to 0.56.0 ([#392](https://github.com/archestra-ai/OpenAPPA/issues/392)) ([7f2f91b](https://github.com/archestra-ai/OpenAPPA/commit/7f2f91bcbba550d8d068d7074c1aa114ef3fd086))
* bump the rust-dependencies group across 1 directory with 8 updates ([#349](https://github.com/archestra-ai/OpenAPPA/issues/349)) ([57cde33](https://github.com/archestra-ai/OpenAPPA/commit/57cde33659d33cee58a9588f19fbdf8c1fb3aefb))
* bump the rust-dependencies group with 3 updates ([#390](https://github.com/archestra-ai/OpenAPPA/issues/390)) ([11b2ee7](https://github.com/archestra-ai/OpenAPPA/commit/11b2ee7e1196cf7db15ca82f08335a7804f54418))


### Code Refactoring

* **eventlog:** receipts keyed by session scope and binding; one leased surface ([#473](https://github.com/archestra-ai/OpenAPPA/issues/473)) ([ec97360](https://github.com/archestra-ai/OpenAPPA/commit/ec97360f4969896a2e75b413aa85861e095bd725))
* **files:** pins, reservations and receipts as enums ([#474](https://github.com/archestra-ai/OpenAPPA/issues/474)) ([de72da2](https://github.com/archestra-ai/OpenAPPA/commit/de72da2d4f748b82dc0b9b3aa4457bf063fa6663))
* **policy:** narrow the policy surface; validate hints and templates at construction ([#472](https://github.com/archestra-ai/OpenAPPA/issues/472)) ([577dfe1](https://github.com/archestra-ai/OpenAPPA/commit/577dfe19bbb36aaaf8ca0d47d56825514da811ec))
* **policy:** split appa-policy into modules ([#457](https://github.com/archestra-ai/OpenAPPA/issues/457)) ([77ce06e](https://github.com/archestra-ai/OpenAPPA/commit/77ce06efd7da4ae37f27a79f9b626c3989347dae))
* **runtime-api:** one TrajectoryId for the engine, the log and the runtime ([#478](https://github.com/archestra-ai/OpenAPPA/issues/478)) ([7d1f9cc](https://github.com/archestra-ai/OpenAPPA/commit/7d1f9cc2bfdf95802e1255722564ac8579802419))
* **runtime:** one contract for the model annotator builtins ([#460](https://github.com/archestra-ai/OpenAPPA/issues/460)) ([47c7bd2](https://github.com/archestra-ai/OpenAPPA/commit/47c7bd2d8f98a52dd436770f01664c40c99dd3d6))
* **runtime:** rename adapter tool derivation to identification ([#409](https://github.com/archestra-ai/OpenAPPA/issues/409)) ([b02dd7e](https://github.com/archestra-ai/OpenAPPA/commit/b02dd7e2f5b8fb3bf80f72aed63ad4103c24eca2))


### Miscellaneous Chores

* **batteries:** tooling test that could not fail, linter dead code, stale runtime comments ([#446](https://github.com/archestra-ai/OpenAPPA/issues/446)) ([2a1818b](https://github.com/archestra-ai/OpenAPPA/commit/2a1818b8530c70669b3d54d26e47ede0f75d379d))

## [0.24.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.23.1...v0.24.0) (2026-09-19)


### ⚠ BREAKING CHANGES

* **eventlog:** pool PostgreSQL connections and lease one per dispatch ([#377](https://github.com/archestra-ai/OpenAPPA/issues/377))

### Features

* **eventlog:** pool PostgreSQL connections and lease one per dispatch ([#377](https://github.com/archestra-ai/OpenAPPA/issues/377)) ([a2720de](https://github.com/archestra-ai/OpenAPPA/commit/a2720de2cbdfada272ad9fd2d252b6937a454d16))
* **runtime:** add independent root forks alongside subagent forks ([#380](https://github.com/archestra-ai/OpenAPPA/issues/380)) ([919d674](https://github.com/archestra-ai/OpenAPPA/commit/919d674c5203b8fb6423de906ae8e007a8f76207))


### Bug Fixes

* a parallel fan-out of substituting remedies makes progress ([#378](https://github.com/archestra-ai/OpenAPPA/issues/378)) ([feaeb93](https://github.com/archestra-ai/OpenAPPA/commit/feaeb93477a2f6fec1a1047bac2c97380b2bcd36))
* **runtime:** ask the hitl reviewer by round trip on MCP 2026-07-28 ([#376](https://github.com/archestra-ai/OpenAPPA/issues/376)) ([f0cb138](https://github.com/archestra-ai/OpenAPPA/commit/f0cb138dab315b982cfc38485023763da85b9a0b))

## [0.23.1](https://github.com/archestra-ai/OpenAPPA/compare/v0.23.0...v0.23.1) (2026-09-18)


### Bug Fixes

* find the AWS secret access key next to `=` and after an adjacent run ([#375](https://github.com/archestra-ai/OpenAPPA/issues/375)) ([1ae5487](https://github.com/archestra-ai/OpenAPPA/commit/1ae5487d6a4e9e1ef65850d26d6dceeaae0da95f))
* mask the AWS secret access key in redact-secrets ([#373](https://github.com/archestra-ai/OpenAPPA/issues/373)) ([4dd3036](https://github.com/archestra-ai/OpenAPPA/commit/4dd3036895f30cb33c155d5b3ae0dc9e25d074ed))
* **runtime:** do not treat parallel result close as an unidentified call ([#370](https://github.com/archestra-ai/OpenAPPA/issues/370)) ([835e5dc](https://github.com/archestra-ai/OpenAPPA/commit/835e5dcde74e65544723033bd69d2cb07b08983b))
* **runtime:** mask the AWS secret access key in redact-secrets ([4dd3036](https://github.com/archestra-ai/OpenAPPA/commit/4dd3036895f30cb33c155d5b3ae0dc9e25d074ed))


### Code Refactoring

* split the Claude Code integration layer into the jobs it does ([#372](https://github.com/archestra-ai/OpenAPPA/issues/372)) ([53a9b63](https://github.com/archestra-ai/OpenAPPA/commit/53a9b6344c7d6635bba932e04fccf4cb2d8385f1))

## [0.23.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.22.0...v0.23.0) (2026-09-17)


### Features

* **eventlog:** persist typed receipts on sqlite and memory ([#368](https://github.com/archestra-ai/OpenAPPA/issues/368)) ([31b7fb6](https://github.com/archestra-ai/OpenAPPA/commit/31b7fb6bc1e66723a1e9d05c8876022ab4a50edb))

## [0.22.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.21.0...v0.22.0) (2026-09-17)


### ⚠ BREAKING CHANGES

* **eventlog:** index host keys instead of scanning payloads ([#365](https://github.com/archestra-ai/OpenAPPA/issues/365))
* **runtime:** hosts pass the document text, not bindings ([#364](https://github.com/archestra-ai/OpenAPPA/issues/364))

### Features

* **eventlog:** index host keys instead of scanning payloads ([#365](https://github.com/archestra-ai/OpenAPPA/issues/365)) ([8bb7c61](https://github.com/archestra-ai/OpenAPPA/commit/8bb7c612f806a0b60a04b8fc828964288a4a657d))


### Documentation

* update benchmark results on the website ([#363](https://github.com/archestra-ai/OpenAPPA/issues/363)) ([801a1df](https://github.com/archestra-ai/OpenAPPA/commit/801a1dfbcf4c7ddad4a0828c1171bdeb0ab5ca2b))


### Dependencies

* bump sha2 from 0.10.9 to 0.11.0 ([#75](https://github.com/archestra-ai/OpenAPPA/issues/75)) ([37577ab](https://github.com/archestra-ai/OpenAPPA/commit/37577ab118730a6d79ad03d2b6ace3ef87882a1e))


### Code Refactoring

* **eventlog, runtime:** host stream in the log, one host reducer ([#360](https://github.com/archestra-ai/OpenAPPA/issues/360)) ([6cbf8a9](https://github.com/archestra-ai/OpenAPPA/commit/6cbf8a9ccbc851121e562c5425fd8b48b3ad1ae2))
* **runtime:** expose typed embedded remedies and durable receipts ([#366](https://github.com/archestra-ai/OpenAPPA/issues/366)) ([11d8d53](https://github.com/archestra-ai/OpenAPPA/commit/11d8d53aba0d6cc25b4882166c02fe567c49c12d))
* **runtime:** hosts pass the document text, not bindings ([#364](https://github.com/archestra-ai/OpenAPPA/issues/364)) ([35be0c3](https://github.com/archestra-ai/OpenAPPA/commit/35be0c36ab257e02acdff9a527c4de96ff314efc))
* **runtime:** the server stack behind a `daemon` feature ([#362](https://github.com/archestra-ai/OpenAPPA/issues/362)) ([2fc46ff](https://github.com/archestra-ai/OpenAPPA/commit/2fc46ff96e0105be5d8b91f7da2064fa4c048f63))

## [0.21.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.20.0...v0.21.0) (2026-09-16)


### Features

* Add PostgreSQL storage and embedded remedy execution ([#303](https://github.com/archestra-ai/OpenAPPA/issues/303)) ([9f60d30](https://github.com/archestra-ai/OpenAPPA/commit/9f60d30bc0c37e83ac9aace393c8489f970831ec))
* **batteries:** claude-code battery covers Grep, Write and Edit of the requester's secrets ([#345](https://github.com/archestra-ai/OpenAPPA/issues/345)) ([c7428fc](https://github.com/archestra-ai/OpenAPPA/commit/c7428fc00d8a5310663c2ed69c9628af63c21f69))
* **batteries:** databricks battery over Genie One and Databricks SQL ([#341](https://github.com/archestra-ai/OpenAPPA/issues/341)) ([613836b](https://github.com/archestra-ai/OpenAPPA/commit/613836b78e9a124722bea8ac1669d35822e70800))
* **batteries:** hugging face battery with a per-repository resolver ([#344](https://github.com/archestra-ai/OpenAPPA/issues/344)) ([03b6658](https://github.com/archestra-ai/OpenAPPA/commit/03b6658946e8fec8d80b4e2383ffaa5ee6440c86))
* **batteries:** microsoft-learn, cloudflare docs and observability, launchdarkly, posthog ([#325](https://github.com/archestra-ai/OpenAPPA/issues/325)) ([987d750](https://github.com/archestra-ai/OpenAPPA/commit/987d7506a37c8b43045b13b71bae8ec0e535b5ef))
* **batteries:** name credentials after install; github falls back to the gh login ([#337](https://github.com/archestra-ai/OpenAPPA/issues/337)) ([486ccef](https://github.com/archestra-ai/OpenAPPA/commit/486cceff805f9037ebf12fc59ad1c09e4c11b3e5))
* **batteries:** pagerduty battery ([#338](https://github.com/archestra-ai/OpenAPPA/issues/338)) ([592d0d1](https://github.com/archestra-ai/OpenAPPA/commit/592d0d13ca505f26c53bf7e0b35d3d81b72651ca))
* **bench:** restore the Tau harness on the current engine ([#328](https://github.com/archestra-ai/OpenAPPA/issues/328)) ([2cbb61e](https://github.com/archestra-ai/OpenAPPA/commit/2cbb61e9025c0f8167d787c1b05c1fcffe5f433c))
* **claude-code:** allow parallel tool calls ([#282](https://github.com/archestra-ai/OpenAPPA/issues/282)) ([d7586de](https://github.com/archestra-ai/OpenAPPA/commit/d7586deb142dd5291ddd53116bc49807fcdca2d0))
* compare APPA with Claude Auto mode ([#306](https://github.com/archestra-ai/OpenAPPA/issues/306)) ([54bc8c8](https://github.com/archestra-ai/OpenAPPA/commit/54bc8c882acc916cd095c8ac3bcd5bc82b49a5db))
* **engine:** catch-all attention permit, reserved blocked mark, widened human authority ([#331](https://github.com/archestra-ai/OpenAPPA/issues/331)) ([39d0a46](https://github.com/archestra-ai/OpenAPPA/commit/39d0a46658e957cf4c90149abc7ac19c43dfa623))
* **installation:** plugins require batteries by manifest, installs suggest the rest ([#327](https://github.com/archestra-ai/OpenAPPA/issues/327)) ([1779b73](https://github.com/archestra-ai/OpenAPPA/commit/1779b731a9d4c1989b5981c54dd42e4d5f513c94))
* **runtime:** add opt-in file mediation for Claude Code ([#313](https://github.com/archestra-ai/OpenAPPA/issues/313)) ([8bc5586](https://github.com/archestra-ai/OpenAPPA/commit/8bc55866f1db83f1876c0934658b049f8364da1d))
* **runtime:** stock redact-secrets sanitizer masks the battery's credential reads ([#339](https://github.com/archestra-ai/OpenAPPA/issues/339)) ([0845d6e](https://github.com/archestra-ai/OpenAPPA/commit/0845d6e41c9f59a7c7825f8111c34255c2953bef))
* **yell:** support embedded reporting and label Slack sources ([#359](https://github.com/archestra-ai/OpenAPPA/issues/359)) ([e206581](https://github.com/archestra-ai/OpenAPPA/commit/e2065813a7635759fef5ccfe8fbe73dfc44039c9))


### Bug Fixes

* **batteries:** redact-secrets is offered for every withheld Bash result ([#343](https://github.com/archestra-ai/OpenAPPA/issues/343)) ([2118a91](https://github.com/archestra-ai/OpenAPPA/commit/2118a91794cb3dbc18f4f05add11f0afaa99ed69))
* **deps:** patch vulnerable website packages and yanked chacha20 ([#350](https://github.com/archestra-ai/OpenAPPA/issues/350)) ([f754eed](https://github.com/archestra-ai/OpenAPPA/commit/f754eedaa83685e588911b2004ca5ac9df1e0c63))
* **describe:** name where a config fails, and one line for tools no host observed ([#346](https://github.com/archestra-ai/OpenAPPA/issues/346)) ([a102352](https://github.com/archestra-ai/OpenAPPA/commit/a10235221be01049e6b99cc92c44ee0a47966e71))
* **engine:** a sanitizer's transition is read against the value, never pre-fetched ([#336](https://github.com/archestra-ai/OpenAPPA/issues/336)) ([856f5f6](https://github.com/archestra-ai/OpenAPPA/commit/856f5f66a2f0e307e7684bc968848854e36c9d21))
* **engine:** preserve symbolic audience checks during approval ([#356](https://github.com/archestra-ai/OpenAPPA/issues/356)) ([167fd72](https://github.com/archestra-ai/OpenAPPA/commit/167fd72d0358aec3e4e7db2c0aafe0cea058c3c1))
* **eventlog:** compile PostgreSQL support with bound facts ([#358](https://github.com/archestra-ai/OpenAPPA/issues/358)) ([8ed2293](https://github.com/archestra-ai/OpenAPPA/commit/8ed22934068f030faea9310eee2eb0ddef756a4d))
* **eventlog:** preserve host call bindings with PostgreSQL ([#355](https://github.com/archestra-ai/OpenAPPA/issues/355)) ([62746a9](https://github.com/archestra-ai/OpenAPPA/commit/62746a94cc1109eec75170fa28cc0e36eb25ea82))
* **installation:** battery discovery follow-ups from review ([#330](https://github.com/archestra-ai/OpenAPPA/issues/330)) ([889315b](https://github.com/archestra-ai/OpenAPPA/commit/889315b2a571c611a8699b0b49c07a136ef0495a))
* **runtime:** a subagent hears the session context at its start ([#342](https://github.com/archestra-ai/OpenAPPA/issues/342)) ([818846f](https://github.com/archestra-ai/OpenAPPA/commit/818846f893f426d05eadde5675472911dbc1482b))
* **runtime:** install from source with the locked dependency set ([#348](https://github.com/archestra-ai/OpenAPPA/issues/348)) ([8ed349a](https://github.com/archestra-ai/OpenAPPA/commit/8ed349a8e87eb890f446b0f507347076034764ca))
* **runtime:** reword the below-floor fork heading ([#353](https://github.com/archestra-ai/OpenAPPA/issues/353)) ([f8abd24](https://github.com/archestra-ai/OpenAPPA/commit/f8abd24028b0b0c4121c35dcbaefed4c84483f26))
* **skills:** bring appa-guide and appa-debug in line with the current runtime ([#352](https://github.com/archestra-ai/OpenAPPA/issues/352)) ([0dbe98a](https://github.com/archestra-ai/OpenAPPA/commit/0dbe98af07fa48488c7e53539b9cd789963470b1))
* **website:** clarify pronunciation playback control ([d5a7906](https://github.com/archestra-ai/OpenAPPA/commit/d5a790698bdebc591e5307182587b9c876340711))
* **website:** make pronunciation control look like a play button ([#326](https://github.com/archestra-ai/OpenAPPA/issues/326)) ([d5a7906](https://github.com/archestra-ai/OpenAPPA/commit/d5a790698bdebc591e5307182587b9c876340711))


### Documentation

* **website:** add comparison with agent auto-modes (Claude Code, Codex) ([#357](https://github.com/archestra-ai/OpenAPPA/issues/357)) ([47bd36f](https://github.com/archestra-ai/OpenAPPA/commit/47bd36f021cebcb78061be8c1a560a317af455c7))
* **website:** de-LLM-ify integration and battery docs from recent feature PRs ([#329](https://github.com/archestra-ai/OpenAPPA/issues/329)) ([403d05f](https://github.com/archestra-ai/OpenAPPA/commit/403d05f76178c3e9f91f7469d0878d730df0369a))
* **website:** describe OpenAPPA as a security engine ([#332](https://github.com/archestra-ai/OpenAPPA/issues/332)) ([aed88bb](https://github.com/archestra-ai/OpenAPPA/commit/aed88bb3973cdd81b9ca276949f6a74134453da2))


### Dependencies

* bump actions/setup-go from 6.5.0 to 7.0.0 ([#257](https://github.com/archestra-ai/OpenAPPA/issues/257)) ([d2525c4](https://github.com/archestra-ai/OpenAPPA/commit/d2525c4e8795a69fd63dd42677f40324936221c1))
* bump getrandom from 0.3.4 to 0.4.3 ([#73](https://github.com/archestra-ai/OpenAPPA/issues/73)) ([e054318](https://github.com/archestra-ai/OpenAPPA/commit/e05431823462bdd69ec1ce076ba1e51bb26d5d22))


### Code Refactoring

* **batteries:** one cloudflare battery over the documentation and observability servers ([#351](https://github.com/archestra-ai/OpenAPPA/issues/351)) ([51a7bd1](https://github.com/archestra-ai/OpenAPPA/commit/51a7bd1d7b66a1e3e3b3b90c2192db94f192ed6e))

## [0.20.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.19.1...v0.20.0) (2026-09-14)


### Features

* **batteries:** one store beside the config, and one include spelling ([#305](https://github.com/archestra-ai/OpenAPPA/issues/305)) ([80c468d](https://github.com/archestra-ai/OpenAPPA/commit/80c468df8adbd31a392fa39a877c7f93a6e07e1a))


### Bug Fixes

* **ci:** let a retried release keep its already-published images ([#312](https://github.com/archestra-ai/OpenAPPA/issues/312)) ([90d4fd0](https://github.com/archestra-ai/OpenAPPA/commit/90d4fd092b307fdc7a2dd0c5be7e78b85b9ac5c8))
* **engine:** an open annotator mandate carries no other annotator's selector placeholder ([#321](https://github.com/archestra-ai/OpenAPPA/issues/321)) ([308e7d9](https://github.com/archestra-ai/OpenAPPA/commit/308e7d96ff3a55cd5aebb5e9f6f25207c1360d59))
* **example-agent:** a refused call costs the call, not the run ([#320](https://github.com/archestra-ai/OpenAPPA/issues/320)) ([439aa2f](https://github.com/archestra-ai/OpenAPPA/commit/439aa2f90826c49ad14e26ffde3948572f9cb232))
* **runtime:** a runtime started inside a Claude Code session sheds that session's variables ([#319](https://github.com/archestra-ai/OpenAPPA/issues/319)) ([ce98b53](https://github.com/archestra-ai/OpenAPPA/commit/ce98b535356e733f2c018ed78611f46e580770a5))
* **runtime:** name the annotator's error and deny an unmapped audience level honestly ([#317](https://github.com/archestra-ai/OpenAPPA/issues/317)) ([18ca2fb](https://github.com/archestra-ai/OpenAPPA/commit/18ca2fb9eac01f9348f79e6cb4607c30ab665bb4))


### Code Refactoring

* **examples:** batteries live in the marketplace only ([#311](https://github.com/archestra-ai/OpenAPPA/issues/311)) ([688fa8c](https://github.com/archestra-ai/OpenAPPA/commit/688fa8c98a77b142edf087edbe18ddda37b0f4ab))

## [0.19.1](https://github.com/archestra-ai/OpenAPPA/compare/v0.19.0...v0.19.1) (2026-09-11)


### Bug Fixes

* **runtime:** revert observability release changes ([#314](https://github.com/archestra-ai/OpenAPPA/issues/314)) ([ecb5250](https://github.com/archestra-ai/OpenAPPA/commit/ecb52507e7fa54ac20ee744c3759058b0a7a115d))

## [0.19.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.18.0...v0.19.0) (2026-09-11)


### Features

* **runtime:** add OpenTelemetry observability ([#168](https://github.com/archestra-ai/OpenAPPA/issues/168)) ([b47d59a](https://github.com/archestra-ai/OpenAPPA/commit/b47d59a6758c5b8259ca8ffdef6404e6fa9c13f7))


### Bug Fixes

* **ci:** drop the removed --archive flag from the release verify probes ([#307](https://github.com/archestra-ai/OpenAPPA/issues/307)) ([4de1d12](https://github.com/archestra-ai/OpenAPPA/commit/4de1d12a0ea5d11acb0fd41867efebae32215679))

## [0.18.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.17.1...v0.18.0) (2026-09-11)


### Features

* **audience:** declared selector templates and selector placeholders ([#295](https://github.com/archestra-ai/OpenAPPA/issues/295)) ([787eb5c](https://github.com/archestra-ai/OpenAPPA/commit/787eb5cc7bfc3f210140552e851ac2a2e5166079))
* **batteries:** add Linear MCP policy ([#272](https://github.com/archestra-ai/OpenAPPA/issues/272)) ([0d41b6b](https://github.com/archestra-ai/OpenAPPA/commit/0d41b6be491d29631616e4710a4ac99b6585e137))
* **batteries:** per-resource audiences for slack, github, linear; sentry and notion batteries ([#296](https://github.com/archestra-ai/OpenAPPA/issues/296)) ([7faab86](https://github.com/archestra-ai/OpenAPPA/commit/7faab867c2674a1d47b399fc05cc1cb5c4970221))
* **batteries:** per-resource audiences for slack, github, linear; sentry and notion batteries ([#302](https://github.com/archestra-ai/OpenAPPA/issues/302)) ([7faab86](https://github.com/archestra-ai/OpenAPPA/commit/7faab867c2674a1d47b399fc05cc1cb5c4970221))
* **bench:** report token and USD overhead ([#300](https://github.com/archestra-ai/OpenAPPA/issues/300)) ([1ea1c21](https://github.com/archestra-ai/OpenAPPA/commit/1ea1c2128ebdf40fefa2b0f1ca22b59732b2acac))
* **cli:** appa runtime stop, and a purge of the deployment ([#301](https://github.com/archestra-ai/OpenAPPA/issues/301)) ([31f52ab](https://github.com/archestra-ai/OpenAPPA/commit/31f52abe2cbde3af5401ce668cc4f6d82509caeb))
* **cli:** one install for every build; remove `appa init` ([#271](https://github.com/archestra-ai/OpenAPPA/issues/271)) ([ba6966d](https://github.com/archestra-ai/OpenAPPA/commit/ba6966dcb2976afb06fc9ff4b2ea416f17f6e0af))
* the appa binary owns the Claude Code hooks ([#293](https://github.com/archestra-ai/OpenAPPA/issues/293)) ([8223949](https://github.com/archestra-ai/OpenAPPA/commit/8223949be0839c24cc4e02f7d84fe28508898aa5))
* **website:** add custom 404/error pages and alert mark in branding ([#290](https://github.com/archestra-ai/OpenAPPA/issues/290)) ([e0b5f49](https://github.com/archestra-ai/OpenAPPA/commit/e0b5f49638c2dffbf3123b050b32ca1d7aa72822))


### Bug Fixes

* **ci:** make release init probe offline ([#281](https://github.com/archestra-ai/OpenAPPA/issues/281)) ([3cfad99](https://github.com/archestra-ai/OpenAPPA/commit/3cfad994559af26054ce0c7908067c564c55b8ca))
* **kagent:** close delegation, approval, and demo gaps ([#285](https://github.com/archestra-ai/OpenAPPA/issues/285)) ([91d432e](https://github.com/archestra-ai/OpenAPPA/commit/91d432e16533d00eafa66c9bdecd2a6b45af8cc6))
* **kagent:** simplify demo adapters and correct policy bugs ([#284](https://github.com/archestra-ai/OpenAPPA/issues/284)) ([282bbf9](https://github.com/archestra-ai/OpenAPPA/commit/282bbf9374b04f7760c0178bf3b5798ca4741366))
* **kagent:** trim quickstart chats to five website demos ([#280](https://github.com/archestra-ai/OpenAPPA/issues/280)) ([ace0fa6](https://github.com/archestra-ai/OpenAPPA/commit/ace0fa6cfe4aba61ab3362f4217f8f3b2d466267))
* **website:** keep the docs rail's scroll position across navigation ([#292](https://github.com/archestra-ai/OpenAPPA/issues/292)) ([50ecddd](https://github.com/archestra-ai/OpenAPPA/commit/50ecdddb5c322adbc55e99bc71d61867dd3eb70a))


### Documentation

* align Cedar, Dogwood, and OPA comparisons ([#286](https://github.com/archestra-ai/OpenAPPA/issues/286)) ([b8bf2ce](https://github.com/archestra-ai/OpenAPPA/commit/b8bf2ce340d8a567abc5174779b4d269bc5a5719))
* **batteries:** clarify when to use annotators ([#289](https://github.com/archestra-ai/OpenAPPA/issues/289)) ([392afad](https://github.com/archestra-ai/OpenAPPA/commit/392afad01ceca64b6f8395566fd4d2c67da2c56f))


### Code Refactoring

* **release:** the plugin archive is the batteries archive ([#294](https://github.com/archestra-ai/OpenAPPA/issues/294)) ([6efbff0](https://github.com/archestra-ai/OpenAPPA/commit/6efbff0c753b7d6d656f32be228e6f3f60e4385e))
* **release:** the plugin archive is the batteries archive ([#298](https://github.com/archestra-ai/OpenAPPA/issues/298)) ([6efbff0](https://github.com/archestra-ai/OpenAPPA/commit/6efbff0c753b7d6d656f32be228e6f3f60e4385e))

## [0.17.1](https://github.com/archestra-ai/OpenAPPA/compare/v0.17.0...v0.17.1) (2026-09-09)


### Bug Fixes

* **kagent:** repair delegation gating and release image annotations ([#274](https://github.com/archestra-ai/OpenAPPA/issues/274)) ([6ad5b4d](https://github.com/archestra-ai/OpenAPPA/commit/6ad5b4d71aa95be6cb97dd9b3f1ac3c25f184498))


### Documentation

* add OpenAPPA vs Dogwood comparison ([#277](https://github.com/archestra-ai/OpenAPPA/issues/277)) ([99022c9](https://github.com/archestra-ai/OpenAPPA/commit/99022c9bb07020982e77ad3aae2e7670789defb8))

## [0.17.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.16.0...v0.17.0) (2026-09-09)


### Features

* **runtime:** shared plugin protocol with native-name tool validation ([#228](https://github.com/archestra-ai/OpenAPPA/issues/228)) ([242e412](https://github.com/archestra-ai/OpenAPPA/commit/242e4126dbf5ccf4c68c9f072ee7538db5d9715e))


### Bug Fixes

* **appa-yell:** label the Slack object link "gzip" instead of the full path ([#276](https://github.com/archestra-ai/OpenAPPA/issues/276)) ([fb5b88c](https://github.com/archestra-ai/OpenAPPA/commit/fb5b88c01b4bb92a962a2e173a322eb302407bcd))
* **kagent:** keep covered tools usable and repair demo battery wiring ([#267](https://github.com/archestra-ai/OpenAPPA/issues/267)) ([16f43d2](https://github.com/archestra-ai/OpenAPPA/commit/16f43d2713a458312dd17dfbbef0a6aa964ebd1f))
* land marketplace installation changes from PR229 on main ([#264](https://github.com/archestra-ai/OpenAPPA/issues/264)) ([0b0ec35](https://github.com/archestra-ai/OpenAPPA/commit/0b0ec350bdb0fb9602c2c62a1a003bd60cf3d091))


### Documentation

* compare OpenAPPA with Cedar ([#268](https://github.com/archestra-ai/OpenAPPA/issues/268)) ([ada3de7](https://github.com/archestra-ai/OpenAPPA/commit/ada3de7a7e02e47941484083f66fc8c225a4b224))
* explain Cedar business-rule flexibility ([#273](https://github.com/archestra-ai/OpenAPPA/issues/273)) ([54666e2](https://github.com/archestra-ai/OpenAPPA/commit/54666e22fc98dcc0935010a93971fa47dfd5be4a))
* **kagent:** remove bloated tool names and startup requirements section ([#266](https://github.com/archestra-ai/OpenAPPA/issues/266)) ([a9de424](https://github.com/archestra-ai/OpenAPPA/commit/a9de4243f61a9d4584765eed818b196bf0074701))
* **kagent:** streamline quickstart, humanize appa-guide, and harden agent protections ([#265](https://github.com/archestra-ai/OpenAPPA/issues/265)) ([9b46eef](https://github.com/archestra-ai/OpenAPPA/commit/9b46eeffedee4b0a1f00dd967fc3eba9c856db8d))
* replace Cedar comparison and update sidebar ([#270](https://github.com/archestra-ai/OpenAPPA/issues/270)) ([6ab8cae](https://github.com/archestra-ai/OpenAPPA/commit/6ab8caeb3f4cca7353a58b88d91b20f8c62b2763))


### Code Refactoring

* **audience:** the member id a source reports is the reader ([#262](https://github.com/archestra-ai/OpenAPPA/issues/262)) ([9174de9](https://github.com/archestra-ai/OpenAPPA/commit/9174de9c43750e72467af98a34f5323b875d8d1d))

## [0.16.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.15.0...v0.16.0) (2026-09-08)


### Features

* **yell:** default send prompt to [Y/n] and forward reports to Slack ([#259](https://github.com/archestra-ai/OpenAPPA/issues/259)) ([cd7e05b](https://github.com/archestra-ai/OpenAPPA/commit/cd7e05b2d2df202f1092f6d26471b695c4f22a55))


### Bug Fixes

* **kagent:** unblock non-root and ARM64 demo quickstarts ([#260](https://github.com/archestra-ai/OpenAPPA/issues/260)) ([f1fc206](https://github.com/archestra-ai/OpenAPPA/commit/f1fc2068a76b0e0aea298968a12aa662b10653c2))

## [0.15.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.14.1...v0.15.0) (2026-09-08)


### Features

* **yell:** the agent's own report, vouched by the hook that saw the call ([#218](https://github.com/archestra-ai/OpenAPPA/issues/218)) ([c68704a](https://github.com/archestra-ai/OpenAPPA/commit/c68704a6e0134fe9f31250ae045e75b68d633bac))
* **yell:** the diagnostic projection, the report envelope, and `appa yell` ([#214](https://github.com/archestra-ai/OpenAPPA/issues/214)) ([66bceac](https://github.com/archestra-ai/OpenAPPA/commit/66bceacbaf41aa4c9bed6b1e3c55dcdbd48b9343))
* **yell:** the openappa.yell.v1 receiver ([#221](https://github.com/archestra-ai/OpenAPPA/issues/221)) ([d7311b3](https://github.com/archestra-ai/OpenAPPA/commit/d7311b3d2ee88b9623d4af1a10d8803ef4e92d73))


### Bug Fixes

* **appa-runtime:** shorten agent-reporting consent prompt ([#255](https://github.com/archestra-ai/OpenAPPA/issues/255)) ([f698115](https://github.com/archestra-ai/OpenAPPA/commit/f6981150f0ccc98ba8e540b7aa7fd7c32a9fab84))
* **docs:** fix remedy diagram label placement and wording ([#252](https://github.com/archestra-ai/OpenAPPA/issues/252)) ([b809fb6](https://github.com/archestra-ai/OpenAPPA/commit/b809fb6cce217554647b6ed1e7d0ce817183ea5e))
* **docs:** keep remedy diagram labels inside cards ([b809fb6](https://github.com/archestra-ai/OpenAPPA/commit/b809fb6cce217554647b6ed1e7d0ce817183ea5e))
* **runtime:** battery names under symlinks, one vouch record, and a readable appa yell ([#254](https://github.com/archestra-ai/OpenAPPA/issues/254)) ([5b496e8](https://github.com/archestra-ai/OpenAPPA/commit/5b496e8ac6cf86246d292f77427033a01f78ebd6))
* **yell:** clarify agent tool description and confusion guidance ([#258](https://github.com/archestra-ai/OpenAPPA/issues/258)) ([12483e6](https://github.com/archestra-ai/OpenAPPA/commit/12483e646feafdecd32d77ed10cbd85a0d4c140b))


### Documentation

* **kagent:** streamline quickstart, expand showcase scenarios, and polish prose ([#251](https://github.com/archestra-ai/OpenAPPA/issues/251)) ([530af64](https://github.com/archestra-ai/OpenAPPA/commit/530af642d423eaf86e507cd6126c544d9596b2ee))
* reorganize and clarify policy configuration reference ([#256](https://github.com/archestra-ai/OpenAPPA/issues/256)) ([5b679d1](https://github.com/archestra-ai/OpenAPPA/commit/5b679d1c76c42c75ba311cdb5bfc5e6fc018a121))
* simplify How it works and reorganize reference sections ([#250](https://github.com/archestra-ai/OpenAPPA/issues/250)) ([8b8c20c](https://github.com/archestra-ai/OpenAPPA/commit/8b8c20cb76115f1ff59b2bde22693243d47cf1d0))

## [0.14.1](https://github.com/archestra-ai/OpenAPPA/compare/v0.14.0...v0.14.1) (2026-09-06)


### Bug Fixes

* **kagent:** land demo contracts on init so secret reads are gated ([#244](https://github.com/archestra-ai/OpenAPPA/issues/244)) ([bfee821](https://github.com/archestra-ai/OpenAPPA/commit/bfee82121baecb8fd89da32322434ee527ebf621))
* **kagent:** restore guide-skill pins after the docs polish ([#246](https://github.com/archestra-ai/OpenAPPA/issues/246)) ([256f37e](https://github.com/archestra-ai/OpenAPPA/commit/256f37e34bbbbbf50289673d5a9f3f31fc12808b))


### Documentation

* **kagent:** add live A2A prompts and name the seeded chats ([#243](https://github.com/archestra-ai/OpenAPPA/issues/243)) ([ffdec89](https://github.com/archestra-ai/OpenAPPA/commit/ffdec89c74a201b33913dfe7481b476ea63d84d1))
* **kagent:** install the adapter image before the runtime ([#241](https://github.com/archestra-ai/OpenAPPA/issues/241)) ([ad170ed](https://github.com/archestra-ai/OpenAPPA/commit/ad170eda92bb3522b158eaa883b1b1a97df37200))
* **kagent:** polish integration page and guides to follow ASD-STE100 ([#245](https://github.com/archestra-ai/OpenAPPA/issues/245)) ([44f00d5](https://github.com/archestra-ai/OpenAPPA/commit/44f00d5c6217123937ae5b4900d89ba465390c0c))

## [0.14.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.13.0...v0.14.0) (2026-09-06)


### Features

* **kagent:** streamline guided setup ([#240](https://github.com/archestra-ai/OpenAPPA/issues/240)) ([3c9915e](https://github.com/archestra-ai/OpenAPPA/commit/3c9915e2dfd7fb548c087a61b72c5a8605ba0a2f))


### Bug Fixes

* **kagent:** refresh unmodified bootstrap policy on helm upgrade ([#237](https://github.com/archestra-ai/OpenAPPA/issues/237)) ([935a6ef](https://github.com/archestra-ai/OpenAPPA/commit/935a6ef12826b494ee5dcda80e27ff9bd46c9f64))
* **website:** keep docs table headers and kagent prompts from wrapping ([#238](https://github.com/archestra-ai/OpenAPPA/issues/238)) ([46b7bd5](https://github.com/archestra-ai/OpenAPPA/commit/46b7bd52276523f1155b90273120c5ad59edaaf7))

## [0.13.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.12.0...v0.13.0) (2026-09-06)


### Features

* **release:** publish v* and rolling CI tags to appa-public ([#230](https://github.com/archestra-ai/OpenAPPA/issues/230)) ([5b5f455](https://github.com/archestra-ai/OpenAPPA/commit/5b5f45584b0cf66dbe4706f912514449fb0698fc))
* **runtime:** isolate vouched appa-guide management ([#235](https://github.com/archestra-ai/OpenAPPA/issues/235)) ([c9f7bfd](https://github.com/archestra-ai/OpenAPPA/commit/c9f7bfd0937a91d204970008c7b5fd43f14c494f))


### Bug Fixes

* **guide:** verify reloaded policy and parse apply manifests ([#236](https://github.com/archestra-ai/OpenAPPA/issues/236)) ([dcf9d26](https://github.com/archestra-ai/OpenAPPA/commit/dcf9d2688d80dac7c553e3d1b9096bc891880741))
* **release:** authenticate before historical source checkout ([#233](https://github.com/archestra-ai/OpenAPPA/issues/233)) ([fdb52f3](https://github.com/archestra-ai/OpenAPPA/commit/fdb52f3eae8213c81c406735fd46aa3f5092bc6f))
* **release:** keep historical auth cleanup available ([#234](https://github.com/archestra-ai/OpenAPPA/issues/234)) ([cc0dfdb](https://github.com/archestra-ai/OpenAPPA/commit/cc0dfdb67758e120e0b5a90dd103f9c56762bdce))
* **release:** support registry-only release recovery ([#232](https://github.com/archestra-ai/OpenAPPA/issues/232)) ([6ffb731](https://github.com/archestra-ai/OpenAPPA/commit/6ffb731fa40cfe2294e0a429d4fbc0e75dcc5f2d))

## [0.12.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.11.1...v0.12.0) (2026-09-05)


### Features

* **kagent:** keep demo fixtures off the runtime chart ([#226](https://github.com/archestra-ai/OpenAPPA/issues/226)) ([2e7b664](https://github.com/archestra-ai/OpenAPPA/commit/2e7b664ff27ae45d0eab92b9023fa0d74d236e8d))

## [0.11.1](https://github.com/archestra-ai/OpenAPPA/compare/v0.11.0...v0.11.1) (2026-09-05)


### Bug Fixes

* **kagent:** report init inventory accurately ([#224](https://github.com/archestra-ai/OpenAPPA/issues/224)) ([ec82083](https://github.com/archestra-ai/OpenAPPA/commit/ec820834b63d4d4cf8d6923853ce6b3ce03873f0))

## [0.11.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.10.0...v0.11.0) (2026-09-05)


### Features

* **kagent:** use one remote OpenAPPA runtime ([#222](https://github.com/archestra-ai/OpenAPPA/issues/222)) ([417383c](https://github.com/archestra-ai/OpenAPPA/commit/417383cca925c1ad5472efb747f42a5a7ce07962))


### Bug Fixes

* **kagent:** resolve appa-guide reference path ([#220](https://github.com/archestra-ai/OpenAPPA/issues/220)) ([d29b332](https://github.com/archestra-ai/OpenAPPA/commit/d29b3327acdb56d9aa27244b4cd3013d14dd1d07))
* **release:** derive chart test versions ([#223](https://github.com/archestra-ai/OpenAPPA/issues/223)) ([aeded73](https://github.com/archestra-ai/OpenAPPA/commit/aeded737ce11b3ec75e9d3a2e596df3236ac299e))
* **release:** upload Helm charts separately ([#219](https://github.com/archestra-ai/OpenAPPA/issues/219)) ([57f16ff](https://github.com/archestra-ai/OpenAPPA/commit/57f16ffea1b3e88a9eb8c9dcf4ae5fd2666f962f))


### Documentation

* **kagent:** explain the default demo fleet ([#216](https://github.com/archestra-ai/OpenAPPA/issues/216)) ([082354e](https://github.com/archestra-ai/OpenAPPA/commit/082354e57dde2be083ae9618ad499462f4c18fda))

## [0.10.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.9.0...v0.10.0) (2026-09-04)


### Features

* **annotator:** admit symbolic audiences in mandates and answers ([#213](https://github.com/archestra-ai/OpenAPPA/issues/213)) ([52381a7](https://github.com/archestra-ai/OpenAPPA/commit/52381a76ff09b531f5b044fec74900f763896f46))
* **kagent:** manage OpenAPPA through appa-guide ([#212](https://github.com/archestra-ai/OpenAPPA/issues/212)) ([299209c](https://github.com/archestra-ai/OpenAPPA/commit/299209c2da0b75f5b95ac5a832cddb4b3378d19b))
* **kagent:** publish guided demo quickstart ([#211](https://github.com/archestra-ai/OpenAPPA/issues/211)) ([5b322df](https://github.com/archestra-ai/OpenAPPA/commit/5b322dfee7eeac5ed521a8e7b08acd988f6446ac))


### Bug Fixes

* **kagent:** allow appa-guide skills and stabilize upgrades ([#215](https://github.com/archestra-ai/OpenAPPA/issues/215)) ([a90373f](https://github.com/archestra-ai/OpenAPPA/commit/a90373ff211415e28c8804fcff593d1883e8aa8f))
* **website:** retain common language grammars in markdown highlighter ([#201](https://github.com/archestra-ai/OpenAPPA/issues/201)) ([68576b3](https://github.com/archestra-ai/OpenAPPA/commit/68576b3a749b05024f7db83e9204d7d2b4a14e67))


### Performance Improvements

* **ci:** overlap e2e setup with runner cleanup ([#200](https://github.com/archestra-ai/OpenAPPA/issues/200)) ([f5f89f4](https://github.com/archestra-ai/OpenAPPA/commit/f5f89f4e7dffc2863c3b1d607b7d4f743251db74))
* **ci:** skip heavy jobs on docs-only pull requests ([#205](https://github.com/archestra-ai/OpenAPPA/issues/205)) ([1d3314c](https://github.com/archestra-ai/OpenAPPA/commit/1d3314cb0ba075397d844e969eed3e46602a0da5))


### Documentation

* improve agent integration guide ([#207](https://github.com/archestra-ai/OpenAPPA/issues/207)) ([7bfc535](https://github.com/archestra-ai/OpenAPPA/commit/7bfc53552b85cb18c00efc2384ffe8e894038735))
* **kagent:** convert manifests and helm commands to single copy-paste snippets ([#210](https://github.com/archestra-ai/OpenAPPA/issues/210)) ([0d66d94](https://github.com/archestra-ai/OpenAPPA/commit/0d66d94d9f086f6bfb9e57e822a3ad0161a3f601))
* **kagent:** disable unused sample agents and bound install timeouts ([#206](https://github.com/archestra-ai/OpenAPPA/issues/206)) ([901c7f3](https://github.com/archestra-ai/OpenAPPA/commit/901c7f3c85be2d3e928223877c0a998a98f1b156))
* **kagent:** guide cluster-wide runtime setup and appa-guide skill workflows ([#204](https://github.com/archestra-ai/OpenAPPA/issues/204)) ([d7fb5a3](https://github.com/archestra-ai/OpenAPPA/commit/d7fb5a31886b2f2eeb9d9ec4bb43ff0b70cf623f))
* **kagent:** use public artifacts and document provider configuration ([#208](https://github.com/archestra-ai/OpenAPPA/issues/208)) ([a12a192](https://github.com/archestra-ai/OpenAPPA/commit/a12a1928c7a302bdc7c6543a1a8cb2155343ccd2))
* streamline agent integration guide ([#209](https://github.com/archestra-ai/OpenAPPA/issues/209)) ([f52a721](https://github.com/archestra-ai/OpenAPPA/commit/f52a721dd6b70dc2c6fd3af4f8703ca84a5ecc25))

## [0.9.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.8.0...v0.9.0) (2026-09-04)


### Features

* **k8s:** ship shared appa-runtime Helm deployment ([#199](https://github.com/archestra-ai/OpenAPPA/issues/199)) ([8026fc9](https://github.com/archestra-ai/OpenAPPA/commit/8026fc925ded33254cc2f64ebb343faddb9b6022))


### Bug Fixes

* **ci:** align and gate live kagent e2e ([#191](https://github.com/archestra-ai/OpenAPPA/issues/191)) ([2e3c3d2](https://github.com/archestra-ai/OpenAPPA/commit/2e3c3d291493bcac47c63ed8ced2231739fc4855))
* **engine:** encode policy digests explicitly ([#194](https://github.com/archestra-ai/OpenAPPA/issues/194)) ([4c10285](https://github.com/archestra-ai/OpenAPPA/commit/4c10285c1e4986af46b0b271b20c5efd9534d82b))


### Performance Improvements

* **ci:** speed Rust tests and A2A checks ([#192](https://github.com/archestra-ai/OpenAPPA/issues/192)) ([765b48a](https://github.com/archestra-ai/OpenAPPA/commit/765b48aeedf3b93b62dc83b84f7630a604235cc7))


### Documentation

* add GitHub battery replay example ([#195](https://github.com/archestra-ai/OpenAPPA/issues/195)) ([a4e28e8](https://github.com/archestra-ai/OpenAPPA/commit/a4e28e8f67211f4b79df98af9a5ff29f7cef2957))
* clarify battery example link ([#196](https://github.com/archestra-ai/OpenAPPA/issues/196)) ([9fe3889](https://github.com/archestra-ai/OpenAPPA/commit/9fe38892c0c96ad47f1104b17da2b274d9307bf7))
* link complete battery replay example ([9fe3889](https://github.com/archestra-ai/OpenAPPA/commit/9fe38892c0c96ad47f1104b17da2b274d9307bf7))
* streamline battery composition guide ([#198](https://github.com/archestra-ai/OpenAPPA/issues/198)) ([9afbcbf](https://github.com/archestra-ai/OpenAPPA/commit/9afbcbffd979da6a31c7fe58f8a08e7825ebb7f4))

## [0.8.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.7.1...v0.8.0) (2026-09-03)


### Features

* **kagent:** add the appa-guide routing skill on the stock tool server ([#184](https://github.com/archestra-ai/OpenAPPA/issues/184)) ([12c0a7f](https://github.com/archestra-ai/OpenAPPA/commit/12c0a7fb69d8dfb0176c996408663c0fff523eba))
* **kagent:** an APPA_ENABLED knob, the subagent return gate, and a gated-path suite ([#170](https://github.com/archestra-ai/OpenAPPA/issues/170)) ([2de7716](https://github.com/archestra-ai/OpenAPPA/commit/2de7716904150c69a91e1576d99fc60d0ed3f3e2))
* **kagent:** gate kagent declarative agents with OpenAPPA ([#154](https://github.com/archestra-ai/OpenAPPA/issues/154)) ([d0c7c3d](https://github.com/archestra-ai/OpenAPPA/commit/d0c7c3d55e1695e823f69bc0e063cda6ed0f5e34))
* **policy:** default audiences and a leaner Claude Code config ([#181](https://github.com/archestra-ai/OpenAPPA/issues/181)) ([b141c89](https://github.com/archestra-ai/OpenAPPA/commit/b141c89c88ae65ae1078911ff58f44a9ca9ff6f8))
* **replay:** add appa replay for trace-file policy tests ([#162](https://github.com/archestra-ai/OpenAPPA/issues/162)) ([27e8246](https://github.com/archestra-ai/OpenAPPA/commit/27e82463c290d8fc594b4118e85cfc2f47529108))
* **runtime:** declare the subagent's return at the spawn, return at SubagentStop ([#169](https://github.com/archestra-ai/OpenAPPA/issues/169)) ([769d16d](https://github.com/archestra-ai/OpenAPPA/commit/769d16da2651a0cde692fe245d0e24aaad242c23))
* **website:** pronunciation button on the landing hero ([#163](https://github.com/archestra-ai/OpenAPPA/issues/163)) ([73ab516](https://github.com/archestra-ai/OpenAPPA/commit/73ab51644dc2cb3e66ac6683293f58b2fc811f4d))


### Bug Fixes

* **bench:** harden canary against provider timeouts and rejections ([#177](https://github.com/archestra-ai/OpenAPPA/issues/177)) ([7c4ffd1](https://github.com/archestra-ai/OpenAPPA/commit/7c4ffd1317ccc3c454d382d9536bfdde00c1df38))
* **ci:** stabilize release checks ([#188](https://github.com/archestra-ai/OpenAPPA/issues/188)) ([0759b92](https://github.com/archestra-ai/OpenAPPA/commit/0759b92291a71c83d9233f9eaaf2acf9c5b49cf6))
* **engine:** floor-aware fork advice and a return-floor hint that does not spell the current label ([#178](https://github.com/archestra-ai/OpenAPPA/issues/178)) ([56346ec](https://github.com/archestra-ai/OpenAPPA/commit/56346ec779d5284d10b614bf103657e9aa4d1eb8))
* **install:** validate the archive before unpacking it ([#173](https://github.com/archestra-ai/OpenAPPA/issues/173)) ([e8a250a](https://github.com/archestra-ai/OpenAPPA/commit/e8a250ab48508d97e2a849cebc770f62bebd4f63))
* **install:** validate the archive before unpacking it, and refuse in one voice ([e8a250a](https://github.com/archestra-ai/OpenAPPA/commit/e8a250ab48508d97e2a849cebc770f62bebd4f63))
* **release:** build recovery artifacts from draft tag ([#159](https://github.com/archestra-ai/OpenAPPA/issues/159)) ([29a815d](https://github.com/archestra-ai/OpenAPPA/commit/29a815d8ce70edca6a766ceef8921965e2748734))
* **release:** make the release gate assert something, and give its checks one definition each ([#172](https://github.com/archestra-ai/OpenAPPA/issues/172)) ([11d2a6a](https://github.com/archestra-ai/OpenAPPA/commit/11d2a6afb78c9f5645ea3c9c54bd054104a08819))
* **release:** tolerate expected Windows init failure ([#157](https://github.com/archestra-ai/OpenAPPA/issues/157)) ([b38aec9](https://github.com/archestra-ai/OpenAPPA/commit/b38aec916e32e6416a9404db99550d7e9773bf76))
* **runtime:** clarify agent-facing policy block and remedy feedback ([#183](https://github.com/archestra-ai/OpenAPPA/issues/183)) ([fce9a16](https://github.com/archestra-ai/OpenAPPA/commit/fce9a1611a3a5c9117b6241ada48bd5a581b3800))
* **runtime:** clarify init compatibility messages ([#161](https://github.com/archestra-ai/OpenAPPA/issues/161)) ([49c59bc](https://github.com/archestra-ai/OpenAPPA/commit/49c59bccde3f1d9dc3a90aefa46b1d877533621e))
* **runtime:** disable the classifier CLI's background traffic ([#160](https://github.com/archestra-ai/OpenAPPA/issues/160)) ([0989ec1](https://github.com/archestra-ai/OpenAPPA/commit/0989ec1f4304160bc1c92f73654f4dd88d454d08))
* **test:** allow runtime startup under load ([#189](https://github.com/archestra-ai/OpenAPPA/issues/189)) ([716c5c6](https://github.com/archestra-ai/OpenAPPA/commit/716c5c6d9861977972621b63f3ba65b34fe1076f))
* **website:** drop the custom cursor on the pronunciation button ([#174](https://github.com/archestra-ai/OpenAPPA/issues/174)) ([b9117a9](https://github.com/archestra-ai/OpenAPPA/commit/b9117a9a9fbdd65bf286ecea826ef1dbd0bd8a4a))
* **website:** raise docs text contrast ([#165](https://github.com/archestra-ai/OpenAPPA/issues/165)) ([68961ed](https://github.com/archestra-ai/OpenAPPA/commit/68961edfc31742379638eeb910f799d01fcd071a))
* **website:** set prose back to 15px ([#166](https://github.com/archestra-ai/OpenAPPA/issues/166)) ([b24cabd](https://github.com/archestra-ai/OpenAPPA/commit/b24cabd1136a3f08e5ca2436f83f33cd1a033c3d))


### Documentation

* add batteries catalog and author guide ([#179](https://github.com/archestra-ai/OpenAPPA/issues/179)) ([9ac2ae0](https://github.com/archestra-ai/OpenAPPA/commit/9ac2ae0e34216687bd09e9fea2b35460a67a2797))
* **kagent:** add operator guide, existing cluster setup, and interactive figure ([#185](https://github.com/archestra-ai/OpenAPPA/issues/185)) ([9ad3fee](https://github.com/archestra-ai/OpenAPPA/commit/9ad3fee62c53c61d803b4c9425fad164d5483024))
* **kagent:** streamline operator guide and strip release markers from rendered code ([#187](https://github.com/archestra-ai/OpenAPPA/issues/187)) ([19fc101](https://github.com/archestra-ai/OpenAPPA/commit/19fc101312f8437c40b124e2e68c1962cb1ce50f))
* **replay:** add validation guide ([#164](https://github.com/archestra-ai/OpenAPPA/issues/164)) ([b17aeb1](https://github.com/archestra-ai/OpenAPPA/commit/b17aeb1611d927d7ac5ed9c1ad4b74b5faef99d6))
* **replay:** shorten the validation guide ([#167](https://github.com/archestra-ai/OpenAPPA/issues/167)) ([201c782](https://github.com/archestra-ai/OpenAPPA/commit/201c7822207dcef236818e88176aef1aab9bf02f))
* **website:** add agent integration guide and architecture diagram ([#186](https://github.com/archestra-ai/OpenAPPA/issues/186)) ([d5ebd49](https://github.com/archestra-ai/OpenAPPA/commit/d5ebd49576c225b2f146a124b44a9d1525e84ca3))


### Code Refactoring

* **init:** give the endpoint protocol, the fingerprint and the receipt one definition each ([#175](https://github.com/archestra-ai/OpenAPPA/issues/175)) ([23b834e](https://github.com/archestra-ai/OpenAPPA/commit/23b834ed24098313cdc25758be1911edd551f391))
* **init:** split init.rs into the five things it does ([#176](https://github.com/archestra-ai/OpenAPPA/issues/176)) ([3e9474a](https://github.com/archestra-ai/OpenAPPA/commit/3e9474a0afad77123f33b35589d01bd2d821c3b3))

## [0.7.1](https://github.com/archestra-ai/OpenAPPA/compare/v0.7.0...v0.7.1) (2026-09-02)


### Bug Fixes

* **release:** make packaged runtime verification portable ([#155](https://github.com/archestra-ai/OpenAPPA/issues/155)) ([903fd5b](https://github.com/archestra-ai/OpenAPPA/commit/903fd5b7ce18144a0507ccda53d4302ba255da92))

## [0.7.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.6.1...v0.7.0) (2026-09-02)


### ⚠ BREAKING CHANGES

* an audience source's token moves from OPENAPPA_<PROVIDER>_TOKEN to APPA_PROVIDER_<PROVIDER>_TOKEN.
* a verified-email principal is the bare address; logs and policies spelling email:<address> refuse.
* replace dynamic resolvers, casts, and Unknown with the Annotator boundary ([#122](https://github.com/archestra-ai/OpenAPPA/issues/122))

### Features

* **cli:** make appa own Claude Code initialization ([#111](https://github.com/archestra-ai/OpenAPPA/issues/111)) ([4c43d6d](https://github.com/archestra-ai/OpenAPPA/commit/4c43d6d4a9417b1daaf1418c2f82e0ab425b9b29))
* first-class symbolic audiences and battery-backed audience sources ([#134](https://github.com/archestra-ai/OpenAPPA/issues/134)) ([426df5f](https://github.com/archestra-ai/OpenAPPA/commit/426df5fa30fcfd1d7fbfdb05e59ada4267b39b34))
* **install:** add a curl | sh installer for release binaries ([#152](https://github.com/archestra-ai/OpenAPPA/issues/152)) ([d02542d](https://github.com/archestra-ai/OpenAPPA/commit/d02542db1166243cb76d81ed5b56a63a1666b502))
* nightly defended-vs-empty canary benchmark ([#123](https://github.com/archestra-ai/OpenAPPA/issues/123)) ([3c827db](https://github.com/archestra-ai/OpenAPPA/commit/3c827db6ef1968337382a62ecb4ba31d16b34be1))
* replace dynamic resolvers, casts, and Unknown with the Annotator boundary ([#122](https://github.com/archestra-ai/OpenAPPA/issues/122)) ([9b1dfff](https://github.com/archestra-ai/OpenAPPA/commit/9b1dfff9f6dc55b3b8c102212fc2df8ca229621e))


### Bug Fixes

* archestra-style Slack board for the canary verdict ([#124](https://github.com/archestra-ai/OpenAPPA/issues/124)) ([c0aefd4](https://github.com/archestra-ai/OpenAPPA/commit/c0aefd4d705e538baff3c0b684af66a453a9d8c6))
* **bench:** answer channel is not an attack sink; generous nightly timeouts ([#126](https://github.com/archestra-ai/OpenAPPA/issues/126)) ([df69b8a](https://github.com/archestra-ai/OpenAPPA/commit/df69b8a1adf9206cc2923b95fadd17f9d26224ae))
* **bench:** label finance search statically; provider faults do not red the canary ([#129](https://github.com/archestra-ai/OpenAPPA/issues/129)) ([237e718](https://github.com/archestra-ai/OpenAPPA/commit/237e718fb5dbcf3993a63bf07a82a102606ef7ad))
* bind a command external's credential to that command ([#140](https://github.com/archestra-ai/OpenAPPA/issues/140)) ([310ab67](https://github.com/archestra-ai/OpenAPPA/commit/310ab67c6b7945a76333c1f812ce88bdbb4d68ec))
* drop first-visit email popup, fix mobile drawer overflow, formal cookie notice ([#120](https://github.com/archestra-ai/OpenAPPA/issues/120)) ([7ca3466](https://github.com/archestra-ai/OpenAPPA/commit/7ca3466bdffba8e00dfa4b1f09b44fdf6f5aca61))
* **example-agent:** separate an elapsed request deadline from a transport fault ([#130](https://github.com/archestra-ai/OpenAPPA/issues/130)) ([89c6b1f](https://github.com/archestra-ai/OpenAPPA/commit/89c6b1f8f15852504438e12de41d532938245761))
* **init:** automate matching plugin installs and runtime recovery ([#137](https://github.com/archestra-ai/OpenAPPA/issues/137)) ([f6ebe37](https://github.com/archestra-ai/OpenAPPA/commit/f6ebe37cf48d1b3eff4c8eea5c800e35959d7bd1))
* **init:** install the plugin and binary as one coherent bundle ([#127](https://github.com/archestra-ai/OpenAPPA/issues/127)) ([71b5080](https://github.com/archestra-ai/OpenAPPA/commit/71b5080ad2a49e21493887c5bf71a45c620e924f))
* **init:** run the runtime's startup refusals before the runtime starts ([#142](https://github.com/archestra-ai/OpenAPPA/issues/142)) ([c5fd66a](https://github.com/archestra-ai/OpenAPPA/commit/c5fd66ac35eca8fd52f0971f3a2257dfa693e8a2))
* **release:** sync shared workspace version ([#125](https://github.com/archestra-ai/OpenAPPA/issues/125)) ([977006e](https://github.com/archestra-ai/OpenAPPA/commit/977006ef5379928add87c003e5a6f3bf9b2c22c4))
* remove /landing2 experiment ([#121](https://github.com/archestra-ai/OpenAPPA/issues/121)) ([ae1b656](https://github.com/archestra-ai/OpenAPPA/commit/ae1b656e0721c737b8655788fe435667f620f092))
* **runtime:** close at the next prompt a call an interrupted turn left open ([#138](https://github.com/archestra-ai/OpenAPPA/issues/138)) ([0d56bb9](https://github.com/archestra-ai/OpenAPPA/commit/0d56bb9fab2dbf3df4439975975644bb631b0a34))
* **runtime:** consult budgets and policy reconciliation on init ([#146](https://github.com/archestra-ai/OpenAPPA/issues/146)) ([c8d7e8a](https://github.com/archestra-ai/OpenAPPA/commit/c8d7e8a20bf90ea88edccc735e562a09858a84d2))
* **runtime:** name APPA in the outstanding-call refusal ([#136](https://github.com/archestra-ai/OpenAPPA/issues/136)) ([b62639d](https://github.com/archestra-ai/OpenAPPA/commit/b62639ddc64ba09b28531e08cc41b395071fea05))
* **runtime:** render an inhabited enum for an empty mandate vocabulary ([#141](https://github.com/archestra-ai/OpenAPPA/issues/141)) ([d2f3f47](https://github.com/archestra-ai/OpenAPPA/commit/d2f3f47ac910c5948ba669af9f8fc0fee4aeafd9))


### Documentation

* AppaPluginKagent rename and remedy-plan execution coverage ([#149](https://github.com/archestra-ai/OpenAPPA/issues/149)) ([098f3c6](https://github.com/archestra-ai/OpenAPPA/commit/098f3c61bea571df68ac96b3e3a14a2275521996))
* cover annotators and label flow in the kagent docs ([#150](https://github.com/archestra-ai/OpenAPPA/issues/150)) ([70f26e2](https://github.com/archestra-ai/OpenAPPA/commit/70f26e293e4b28d55a4bb6dacad41f2fffe78979))
* cover both runtimes in the kagent proposal and fix the plan link ([#145](https://github.com/archestra-ai/OpenAPPA/issues/145)) ([1ae2e87](https://github.com/archestra-ai/OpenAPPA/commit/1ae2e877f0dde13977626b72495c754ad0d0a754))
* drop the proposal wrapper from the Batteries page ([#131](https://github.com/archestra-ai/OpenAPPA/issues/131)) ([48e5c46](https://github.com/archestra-ai/OpenAPPA/commit/48e5c46910f244061ff754a46b9f50f2f1212aa7))
* finalize the kagent proposal on the no-fork ADK plugin lane ([#139](https://github.com/archestra-ai/OpenAPPA/issues/139)) ([87e96b9](https://github.com/archestra-ai/OpenAPPA/commit/87e96b934d7d3062440fe88396785bdb0b104d7b))
* give the kagent runtime images their own names ([#147](https://github.com/archestra-ai/OpenAPPA/issues/147)) ([64393be](https://github.com/archestra-ai/OpenAPPA/commit/64393be0b124438afbb76d8c3636913e7a387e80))
* kagent target matrix, per-ADK mappings, quickstart, and a leaner proposal page ([#148](https://github.com/archestra-ai/OpenAPPA/issues/148)) ([2f2ab64](https://github.com/archestra-ai/OpenAPPA/commit/2f2ab645e0a86615a58015155855f09adc14c313))
* **kagent:** cover out-of-band ADK flows and boundary-review fixes ([#151](https://github.com/archestra-ai/OpenAPPA/issues/151)) ([b4e6270](https://github.com/archestra-ai/OpenAPPA/commit/b4e627020e706896dfa4cf5b7cc6dbc03bf2a4fd))
* keep Google ADK unmodified in the kagent proposal ([#133](https://github.com/archestra-ai/OpenAPPA/issues/133)) ([d87d6f8](https://github.com/archestra-ai/OpenAPPA/commit/d87d6f822fdde2968d607b05f2be4c6140b67342))
* propose the kagent integration ([#113](https://github.com/archestra-ai/OpenAPPA/issues/113)) ([9a66708](https://github.com/archestra-ai/OpenAPPA/commit/9a6670840a3f03eee7e31e8206f96a0a0bd7e27b))
* redesign kagent integration for dynamic ADK plugins ([#128](https://github.com/archestra-ai/OpenAPPA/issues/128)) ([77230bb](https://github.com/archestra-ai/OpenAPPA/commit/77230bbb177d72ac740173036b715ff2b3f1ae24))
* redraw the kagent diagrams and split the plan by lane and runtime ([#144](https://github.com/archestra-ai/OpenAPPA/issues/144)) ([5babba6](https://github.com/archestra-ai/OpenAPPA/commit/5babba600ceff754d9e87810c923fe150d0c7620))
* reorganize docs nav categories ([#132](https://github.com/archestra-ai/OpenAPPA/issues/132)) ([fec0ffe](https://github.com/archestra-ai/OpenAPPA/commit/fec0ffe2f2e4cc8659a9600a0d9c7f221bea9da4))
* reorganize nav — Batteries to Deep Dive, Integrations category, kAgent title ([fec0ffe](https://github.com/archestra-ai/OpenAPPA/commit/fec0ffe2f2e4cc8659a9600a0d9c7f221bea9da4))
* retarget the kagent proposal to stable kagent v0.9.12 ([#143](https://github.com/archestra-ai/OpenAPPA/issues/143)) ([ba9c549](https://github.com/archestra-ai/OpenAPPA/commit/ba9c5497bfb81088654910a5f1b6d02be6ff90c8))
* split kagent adapter from appa-runtime ([#135](https://github.com/archestra-ai/OpenAPPA/issues/135)) ([c0facc4](https://github.com/archestra-ai/OpenAPPA/commit/c0facc49e32950b72d7f8afb98f22c35f91dbcab))


### Code Refactoring

* consolidate the day's merges ([#153](https://github.com/archestra-ai/OpenAPPA/issues/153)) ([e13c408](https://github.com/archestra-ai/OpenAPPA/commit/e13c408782a6ebe39159215e40e2ec84cfcb7155))

## [0.6.1](https://github.com/archestra-ai/OpenAPPA/compare/v0.6.0...v0.6.1) (2026-08-28)


### Bug Fixes

* give tool classifiers an evidence-based neutral baseline ([#117](https://github.com/archestra-ai/OpenAPPA/issues/117)) ([816eb59](https://github.com/archestra-ai/OpenAPPA/commit/816eb59a5ad731f015ebba7d48553dd46e01874e))

## [0.6.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.5.1...v0.6.0) (2026-08-28)


### Features

* undeclared tools are cast work, and a requirement slot can be unknown ([#115](https://github.com/archestra-ai/OpenAPPA/issues/115)) ([21b8c75](https://github.com/archestra-ai/OpenAPPA/commit/21b8c7577f8db18ee7b07ee5a5e3922381f47ce1))


### Bug Fixes

* constrain dynamic audiences and surface hook errors ([#114](https://github.com/archestra-ai/OpenAPPA/issues/114)) ([238aaa1](https://github.com/archestra-ai/OpenAPPA/commit/238aaa18c3e1be512260e2d5fe6e844d49d24c1e))

## [0.5.1](https://github.com/archestra-ai/OpenAPPA/compare/v0.5.0...v0.5.1) (2026-08-27)


### Performance Improvements

* post hooks from the runtime binary, pin the replay crates, move to toml 1 ([#105](https://github.com/archestra-ai/OpenAPPA/issues/105)) ([5004ec8](https://github.com/archestra-ai/OpenAPPA/commit/5004ec8b5cd148cd16815026c2eb66ce70b891cc))

## [0.5.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.4.0...v0.5.0) (2026-08-27)


### Features

* **website:** consent-gated session replay ([#92](https://github.com/archestra-ai/OpenAPPA/issues/92)) ([58f7a5e](https://github.com/archestra-ai/OpenAPPA/commit/58f7a5eb5426fdd0847e8c4d1fa87706fe079c43))


### Bug Fixes

* **bench:** ship corporate benchmark corpora ([#93](https://github.com/archestra-ai/OpenAPPA/issues/93)) ([bfa61d2](https://github.com/archestra-ai/OpenAPPA/commit/bfa61d2dbae39f2f01b2d1510c9edab67885e6d1))
* derive attention vocabulary from policy ([c7ead24](https://github.com/archestra-ai/OpenAPPA/commit/c7ead2446c0e1102a3d3d048ae418a6d5b11e596))
* derive resolver attention vocabulary from policy ([#107](https://github.com/archestra-ai/OpenAPPA/issues/107)) ([c7ead24](https://github.com/archestra-ai/OpenAPPA/commit/c7ead2446c0e1102a3d3d048ae418a6d5b11e596))
* **engine:** keep an offer standing across label-neutral admissions and reuse pinned resolver answers ([#97](https://github.com/archestra-ai/OpenAPPA/issues/97)) ([97ccace](https://github.com/archestra-ai/OpenAPPA/commit/97ccace3bb54f7f21877b818209c385045911b84))
* **engine:** serialize every digest as hex, not a 32-integer array ([#96](https://github.com/archestra-ai/OpenAPPA/issues/96)) ([7f719d7](https://github.com/archestra-ai/OpenAPPA/commit/7f719d751f7d1d9402154273e1f5f9dda9d75087))
* **plugin:** replace a running runtime whose binary an install replaced ([#101](https://github.com/archestra-ai/OpenAPPA/issues/101)) ([f324cd5](https://github.com/archestra-ai/OpenAPPA/commit/f324cd5145acff57b5a9274eab6649eaec0a36c5))
* **runtime:** compile Claude backend on Windows ([#94](https://github.com/archestra-ai/OpenAPPA/issues/94)) ([4d443df](https://github.com/archestra-ai/OpenAPPA/commit/4d443dfa23e281d8da463dea7d525cb33d7f3736))
* **website:** label FIDES baseline as FIDES (Microsoft) in benchmark banner ([#98](https://github.com/archestra-ai/OpenAPPA/issues/98)) ([f183ca8](https://github.com/archestra-ai/OpenAPPA/commit/f183ca8d8891b10479be1fb73498f8d50453ab9b))


### Performance Improvements

* **build:** drop the duplicate crypto provider and size the release profile ([#99](https://github.com/archestra-ai/OpenAPPA/issues/99)) ([ed8a351](https://github.com/archestra-ai/OpenAPPA/commit/ed8a351dd8d7afb751c87ef2a3ba38d67c2940f0))


### Documentation

* refresh public benchmark results ([#89](https://github.com/archestra-ai/OpenAPPA/issues/89)) ([a1ccf3f](https://github.com/archestra-ai/OpenAPPA/commit/a1ccf3fecc893c17afc494b0e4cf07c4c532e717))


### Miscellaneous Chores

* serve /paper from arXiv and drop superseded bench snapshots ([#104](https://github.com/archestra-ai/OpenAPPA/issues/104)) ([da89e0e](https://github.com/archestra-ai/OpenAPPA/commit/da89e0ea164a3eabdb87062619f1ad5ca6db86d0))

## [0.4.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.3.0...v0.4.0) (2026-08-26)


### Features

* **batteries:** classify sensitive read paths ([#90](https://github.com/archestra-ai/OpenAPPA/issues/90)) ([06279bb](https://github.com/archestra-ai/OpenAPPA/commit/06279bb9ca7a49f655513166df31224f29bf78bb))
* **engine:** match several arguments in one tool selector ([#87](https://github.com/archestra-ai/OpenAPPA/issues/87)) ([be3a559](https://github.com/archestra-ai/OpenAPPA/commit/be3a559dda37ffd4af279d286843c52ffb09a769))
* wire every external component the same way, and let a model answer any of them ([#83](https://github.com/archestra-ai/OpenAPPA/issues/83)) ([b51784e](https://github.com/archestra-ai/OpenAPPA/commit/b51784e34d4765efc02fd33aae37ae7b4f14b50f))


### Bug Fixes

* **ci:** unblock the release binary build ([#86](https://github.com/archestra-ai/OpenAPPA/issues/86)) ([1e42524](https://github.com/archestra-ai/OpenAPPA/commit/1e425246c2fc06c913814c2f048b325c96a52280))

## [0.3.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.2.0...v0.3.0) (2026-08-26)


### Features

* a resolver answer survives a sanitizer's input substitution ([#59](https://github.com/archestra-ai/OpenAPPA/issues/59)) ([064000e](https://github.com/archestra-ai/OpenAPPA/commit/064000ecf65038e93309b2d59ba96fc17a734c09))
* a rewrite that selects another ordered contract is a new call under it ([#80](https://github.com/archestra-ai/OpenAPPA/issues/80)) ([9514963](https://github.com/archestra-ai/OpenAPPA/commit/9514963306f01cc76f9360b7b135542b45e08cfc))
* add first-visit reader-ping prompt ([#33](https://github.com/archestra-ai/OpenAPPA/issues/33)) ([de8d686](https://github.com/archestra-ai/OpenAPPA/commit/de8d686584eab3a58f9a50470c02d51757b77cf4))
* add ordered tool contracts ([#68](https://github.com/archestra-ai/OpenAPPA/issues/68)) ([a87380f](https://github.com/archestra-ai/OpenAPPA/commit/a87380f97d3bdd28ada85534c6658629fe7a5874))
* add proposal docs UI ([#55](https://github.com/archestra-ai/OpenAPPA/issues/55)) ([2ddfbd4](https://github.com/archestra-ai/OpenAPPA/commit/2ddfbd4b5ea005549fdb9d2c013aa2a6a7a0761d))
* **bench:** add AgentThreatBench harness with OpenAPPA and FIDES comparison arms ([#40](https://github.com/archestra-ai/OpenAPPA/issues/40)) ([e031ab3](https://github.com/archestra-ai/OpenAPPA/commit/e031ab318b24eec2e9ee1ea5657e097adbeb63c8))
* **bench:** add bench-corp suite and reflect 20-scenario empirical results on website ([#31](https://github.com/archestra-ai/OpenAPPA/issues/31)) ([5b3cc34](https://github.com/archestra-ai/OpenAPPA/commit/5b3cc34562f1a30a3ab21d051b14705b6155acd8))
* **bench:** add native FIDES and bounded task isolation ([#50](https://github.com/archestra-ai/OpenAPPA/issues/50)) ([dd49ee3](https://github.com/archestra-ai/OpenAPPA/commit/dd49ee3f535a4784820cbb35d0f43bf8a63bb92e))
* close the Unknown and cast gaps across engine, runtime, and docs ([#65](https://github.com/archestra-ai/OpenAPPA/issues/65)) ([a84115e](https://github.com/archestra-ai/OpenAPPA/commit/a84115eb5878481057fe8f624e1caec0b5e35959))
* compose battery configuration ([#69](https://github.com/archestra-ai/OpenAPPA/issues/69)) ([28596ec](https://github.com/archestra-ai/OpenAPPA/commit/28596ecc9a17290f1df53ef17309f2338dbb9fb5))
* dynamic resolvers declare inputs and results, tools use them ([#57](https://github.com/archestra-ai/OpenAPPA/issues/57)) ([7d8ce6b](https://github.com/archestra-ai/OpenAPPA/commit/7d8ce6b5977540d98a2559e4a0fd31775d486465))
* **plugin:** forbid tool-switch detours around blocks, and have tool-sync offer a demo prompt ([#26](https://github.com/archestra-ai/OpenAPPA/issues/26)) ([4f40262](https://github.com/archestra-ai/OpenAPPA/commit/4f40262ce0b501732fd8a31d3be6e24f09970cc3))
* **plugin:** tool-sync leads the demo offer with a bold new-session warning ([#27](https://github.com/archestra-ai/OpenAPPA/issues/27)) ([1d75280](https://github.com/archestra-ai/OpenAPPA/commit/1d752801569d4aac94d1279e9ae323c03e7d9732))
* **python:** child branches and schema-attested returns ([#36](https://github.com/archestra-ai/OpenAPPA/issues/36)) ([c2ea1b0](https://github.com/archestra-ai/OpenAPPA/commit/c2ea1b0dece7e72c2a2ce9b721f7e3d98e6e7794))
* rewrite APPA config guide and allow unbound authorities ([#85](https://github.com/archestra-ai/OpenAPPA/issues/85)) ([3ef7aca](https://github.com/archestra-ai/OpenAPPA/commit/3ef7aca8cf3a340e8ffcdf8ba699ba625647d3f3))
* run local battery resolvers ([#70](https://github.com/archestra-ai/OpenAPPA/issues/70)) ([0fa8406](https://github.com/archestra-ai/OpenAPPA/commit/0fa8406a870762ebc99c7e7137f3b6edff7fd4b6))
* ship batteries for Claude Code, Slack, GitHub, and Grain ([#79](https://github.com/archestra-ai/OpenAPPA/issues/79)) ([5d220f7](https://github.com/archestra-ai/OpenAPPA/commit/5d220f7ecdff2198c72a580d70a4fae4f526d698))
* suppress reader-ping via ?popup=no ([#42](https://github.com/archestra-ai/OpenAPPA/issues/42)) ([7da41a7](https://github.com/archestra-ai/OpenAPPA/commit/7da41a7a5814cfecfd5310176aa49ab260328e3a))
* unified tool-level dynamic resolvers with a hardened claude-code classifier ([#45](https://github.com/archestra-ai/OpenAPPA/issues/45)) ([e248b96](https://github.com/archestra-ai/OpenAPPA/commit/e248b96298ede5d2055b49cec8808ffa8c79bee7))
* **website:** consent-gated PostHog analytics, and a consented install count ([#67](https://github.com/archestra-ai/OpenAPPA/issues/67)) ([3e7bb58](https://github.com/archestra-ai/OpenAPPA/commit/3e7bb582d1524e71cd3cc1a67a3833a11256226d))


### Bug Fixes

* **agent:** make branch and inference limits explicit ([#39](https://github.com/archestra-ai/OpenAPPA/issues/39)) ([52906eb](https://github.com/archestra-ai/OpenAPPA/commit/52906eb275a9f08106bfaf86a322f8b078be3dbd))
* **batteries:** check the file name, not the path, in read-sensitivity ([#82](https://github.com/archestra-ai/OpenAPPA/issues/82)) ([271a66b](https://github.com/archestra-ai/OpenAPPA/commit/271a66bb5b274773cc0085b7641e86392eab8973))
* **plugin:** tighten the appa-tool-sync skill ([#81](https://github.com/archestra-ai/OpenAPPA/issues/81)) ([bcbd758](https://github.com/archestra-ai/OpenAPPA/commit/bcbd7587dcb265a3d24763eb5e061c9a37fb909b))
* **website:** state what happens to the reader-ping address ([#48](https://github.com/archestra-ai/OpenAPPA/issues/48)) ([4ed796e](https://github.com/archestra-ai/OpenAPPA/commit/4ed796e7265ecc2a3381423e52b9e3a2a73d09a0))


### Documentation

* 🚧 propose resolver envelope v2 ([#49](https://github.com/archestra-ai/OpenAPPA/issues/49)) ([b7e74c1](https://github.com/archestra-ai/OpenAPPA/commit/b7e74c149bcad9cdb52413ae34db86bc073b9f86))
* **bench:** report 5-rep baseline and redteam-chaos benchmark evaluation ([#32](https://github.com/archestra-ai/OpenAPPA/issues/32)) ([06affe5](https://github.com/archestra-ai/OpenAPPA/commit/06affe5f7ce0a87e0ba0896fb929c2148c076a29))
* clarify headings and technical copy in how-it-works and contracts ([#29](https://github.com/archestra-ai/OpenAPPA/issues/29)) ([3bca9b6](https://github.com/archestra-ai/OpenAPPA/commit/3bca9b632c22b5794badd7950f690d196076f42b))
* clarify user-facing documentation in contracts and how-it-works ([#61](https://github.com/archestra-ai/OpenAPPA/issues/61)) ([690c61e](https://github.com/archestra-ai/OpenAPPA/commit/690c61e8dd8da8fdf61549eb934c7f7a3ad74581))
* drop the duplicated merge-commit entries from the 0.2.0 changelog ([#23](https://github.com/archestra-ai/OpenAPPA/issues/23)) ([306a91d](https://github.com/archestra-ai/OpenAPPA/commit/306a91d4bc4323d1afe92a8a356006e17736ec6d))
* invite readers to the Discord server ([#30](https://github.com/archestra-ai/OpenAPPA/issues/30)) ([27d87a8](https://github.com/archestra-ai/OpenAPPA/commit/27d87a87d3a769e66ec30c31001eaef6f00becf2))
* propose composable tool batteries ([#66](https://github.com/archestra-ai/OpenAPPA/issues/66)) ([8bf7ced](https://github.com/archestra-ai/OpenAPPA/commit/8bf7cedf670f4f998457a50ae519fd2ee99dc151))
* propose resolver envelope v2 ([b7e74c1](https://github.com/archestra-ai/OpenAPPA/commit/b7e74c149bcad9cdb52413ae34db86bc073b9f86))
* refine OpenAPPA onboarding ([#28](https://github.com/archestra-ai/OpenAPPA/issues/28)) ([5e89421](https://github.com/archestra-ai/OpenAPPA/commit/5e894216ece4cf643f4dc4845b15674fea7ce23f))
* refine proposed resolver interface ([#51](https://github.com/archestra-ai/OpenAPPA/issues/51)) ([9dcd7d4](https://github.com/archestra-ai/OpenAPPA/commit/9dcd7d447874f0ceffb77521c8b46d31c056d392))
* restore Batteries proposal presentation ([#76](https://github.com/archestra-ai/OpenAPPA/issues/76)) ([9065fe6](https://github.com/archestra-ai/OpenAPPA/commit/9065fe6db5924e328cfd9ff3f53b215d4c22ae65))
* restructure dynamic resolvers, cut implementation leaks ([#60](https://github.com/archestra-ai/OpenAPPA/issues/60)) ([5fc6033](https://github.com/archestra-ai/OpenAPPA/commit/5fc603376f7b13904d3d2ad86241b28d48a418ea))
* simplify proposed resolver syntax ([#54](https://github.com/archestra-ai/OpenAPPA/issues/54)) ([039b701](https://github.com/archestra-ai/OpenAPPA/commit/039b70166eba7d5d756d1c3c45e786ddb2712ca3))
* the uninstall removes the statusline entry it wrote ([#25](https://github.com/archestra-ai/OpenAPPA/issues/25)) ([e3d0f6c](https://github.com/archestra-ai/OpenAPPA/commit/e3d0f6c8b878dc5a635f876ec813962f90c9bf57))
* use resolver result paths ([#53](https://github.com/archestra-ai/OpenAPPA/issues/53)) ([279f5fd](https://github.com/archestra-ai/OpenAPPA/commit/279f5fd6d8dd3ad624c304209b6da40e8a82c53b))


### Code Refactoring

* rename runtime-v2 to runtime and flatten its crates ([#34](https://github.com/archestra-ai/OpenAPPA/issues/34)) ([e65427e](https://github.com/archestra-ai/OpenAPPA/commit/e65427ec8a1773848bb320bd7c04b86134028822))
* **runtime:** collapse the engine test seam and the accretion around it ([#44](https://github.com/archestra-ai/OpenAPPA/issues/44)) ([6934442](https://github.com/archestra-ai/OpenAPPA/commit/6934442b084d543dc39e16d9195604bf027aeb21))

## [0.2.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.1.0...v0.2.0) (2026-08-18)


### Features

* branding page and GitHub link ([#20](https://github.com/archestra-ai/OpenAPPA/issues/20)) ([69746fe](https://github.com/archestra-ai/OpenAPPA/commit/69746fea20e8ec12d700cd716bff86e899c3a81f))
* cast resolution — classify unknown values and hold results until classified ([f2dc0cb](https://github.com/archestra-ai/OpenAPPA/commit/f2dc0cb46156c441d8376de8a77052346073453a))


### Bug Fixes

* close at the turn end a call the harness never ran ([60e54b6](https://github.com/archestra-ai/OpenAPPA/commit/60e54b64ff3d3fc24a3c268bc927c42c7f342db9))
* **plugin:** start the runtime at install, and stop a fresh install from failing to start it at all ([#22](https://github.com/archestra-ai/OpenAPPA/issues/22)) ([44845e4](https://github.com/archestra-ai/OpenAPPA/commit/44845e47dcd9043d9a2c8df4fdb0656d143fefe7))


### Documentation

* **plugin:** fall back to gh cli when the curl download fails ([e9186e7](https://github.com/archestra-ai/OpenAPPA/commit/e9186e789a076da6be70a1ab3d1443efd1c9c285))
* **plugin:** install downloads with curl, no GitHub CLI needed (repo is public) ([90267b4](https://github.com/archestra-ai/OpenAPPA/commit/90267b41c5096b54625e4bb71eec3cb5a2d2173b))
* **plugin:** report install-time runtime state as starts-with-clappa, not "not started" ([26a05c6](https://github.com/archestra-ai/OpenAPPA/commit/26a05c6c7c0d384bf0648836dec98c1e740f1b37))
* rewrite README ([#21](https://github.com/archestra-ai/OpenAPPA/issues/21)) ([5c95856](https://github.com/archestra-ai/OpenAPPA/commit/5c95856f748a994119f28af0dbb1f0b408935ca3))
* show a blocked flow screenshot on the Claude Code page ([7ff55c2](https://github.com/archestra-ai/OpenAPPA/commit/7ff55c2f17fd4a3618783ac28b170b67c183fb41))
* stop the runtime in the Claude Code uninstall steps ([7d49778](https://github.com/archestra-ai/OpenAPPA/commit/7d49778827d39665feb1a7820e5de5b9a0392b72))
* stop the runtime in the README uninstall steps too ([dd8003c](https://github.com/archestra-ai/OpenAPPA/commit/dd8003c641e7eb9f905b890f21b293b83fb5ddd1))


### Miscellaneous Chores

* add MIT license ([f998d70](https://github.com/archestra-ai/OpenAPPA/commit/f998d709d6f8472b85926567ab029e1a9621f6ac))

## 0.1.0 (2026-08-18)


### Bug Fixes

* appa gate env plugin ([40c3e11](https://github.com/archestra-ai/OpenAPPA/commit/40c3e1122007db0832d345d7a09789288af93675))
* update installation instructions in README ([8797e6d](https://github.com/archestra-ai/OpenAPPA/commit/8797e6dcbd74238216b92ea0a9c763c6b40bf7b9))


### Documentation

* bring back manual uninstall commands beside the Claude prompt ([2fbfbfa](https://github.com/archestra-ai/OpenAPPA/commit/2fbfbfa392cbf8084ca25e9604a6bd04cd961f55))
* make uninstall a Claude prompt; align website install with README ([776ab1e](https://github.com/archestra-ai/OpenAPPA/commit/776ab1e735ee2fb53fb9e5cdf04163ea92d19911))
* make uninstall a Claude prompt; align website install with README ([eed6421](https://github.com/archestra-ai/OpenAPPA/commit/eed6421e397d9a30faf1466668b32ad18ee3d28e))
* setup finishes with a /appa-tool-sync tip for initial policy ([39c036c](https://github.com/archestra-ai/OpenAPPA/commit/39c036c4ae43533813f453e4d5b430ba2468f9b5))
* setup finishes with a /appa-tool-sync tip for initial policy ([1720ce4](https://github.com/archestra-ai/OpenAPPA/commit/1720ce41b830c791b984b5ff6f95dd62bd9525df))
* trim the tool-sync tip to just the initial policy ([f020dd0](https://github.com/archestra-ai/OpenAPPA/commit/f020dd0cedfaa3d4d2e6bf40faec9942373d639f))
* uninstall is manual commands only ([dd1d2ab](https://github.com/archestra-ai/OpenAPPA/commit/dd1d2ab187a7a26f8a9bb9c5429ac270958d4008))
* uninstall is manual commands only; drop prompts and statusLine edit ([cb1d94c](https://github.com/archestra-ai/OpenAPPA/commit/cb1d94ce9966b832b69c64125878a72e502133fa))

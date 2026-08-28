# Changelog

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

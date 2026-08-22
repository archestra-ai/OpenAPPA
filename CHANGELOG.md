# Changelog

## [0.3.0](https://github.com/archestra-ai/OpenAPPA/compare/v0.2.0...v0.3.0) (2026-08-22)


### Features

* add first-visit reader-ping prompt ([#33](https://github.com/archestra-ai/OpenAPPA/issues/33)) ([de8d686](https://github.com/archestra-ai/OpenAPPA/commit/de8d686584eab3a58f9a50470c02d51757b77cf4))
* add proposal docs UI ([#55](https://github.com/archestra-ai/OpenAPPA/issues/55)) ([2ddfbd4](https://github.com/archestra-ai/OpenAPPA/commit/2ddfbd4b5ea005549fdb9d2c013aa2a6a7a0761d))
* **bench:** add AgentThreatBench harness with OpenAPPA and FIDES comparison arms ([#40](https://github.com/archestra-ai/OpenAPPA/issues/40)) ([e031ab3](https://github.com/archestra-ai/OpenAPPA/commit/e031ab318b24eec2e9ee1ea5657e097adbeb63c8))
* **bench:** add bench-corp suite and reflect 20-scenario empirical results on website ([#31](https://github.com/archestra-ai/OpenAPPA/issues/31)) ([5b3cc34](https://github.com/archestra-ai/OpenAPPA/commit/5b3cc34562f1a30a3ab21d051b14705b6155acd8))
* dynamic resolvers declare inputs and results, tools use them ([#57](https://github.com/archestra-ai/OpenAPPA/issues/57)) ([7d8ce6b](https://github.com/archestra-ai/OpenAPPA/commit/7d8ce6b5977540d98a2559e4a0fd31775d486465))
* **plugin:** forbid tool-switch detours around blocks, and have tool-sync offer a demo prompt ([#26](https://github.com/archestra-ai/OpenAPPA/issues/26)) ([4f40262](https://github.com/archestra-ai/OpenAPPA/commit/4f40262ce0b501732fd8a31d3be6e24f09970cc3))
* **plugin:** tool-sync leads the demo offer with a bold new-session warning ([#27](https://github.com/archestra-ai/OpenAPPA/issues/27)) ([1d75280](https://github.com/archestra-ai/OpenAPPA/commit/1d752801569d4aac94d1279e9ae323c03e7d9732))
* **python:** child branches and schema-attested returns ([#36](https://github.com/archestra-ai/OpenAPPA/issues/36)) ([c2ea1b0](https://github.com/archestra-ai/OpenAPPA/commit/c2ea1b0dece7e72c2a2ce9b721f7e3d98e6e7794))
* suppress reader-ping via ?popup=no ([#42](https://github.com/archestra-ai/OpenAPPA/issues/42)) ([7da41a7](https://github.com/archestra-ai/OpenAPPA/commit/7da41a7a5814cfecfd5310176aa49ab260328e3a))
* unified tool-level dynamic resolvers with a hardened claude-code classifier ([#45](https://github.com/archestra-ai/OpenAPPA/issues/45)) ([e248b96](https://github.com/archestra-ai/OpenAPPA/commit/e248b96298ede5d2055b49cec8808ffa8c79bee7))


### Bug Fixes

* **agent:** make branch and inference limits explicit ([#39](https://github.com/archestra-ai/OpenAPPA/issues/39)) ([52906eb](https://github.com/archestra-ai/OpenAPPA/commit/52906eb275a9f08106bfaf86a322f8b078be3dbd))
* **website:** state what happens to the reader-ping address ([#48](https://github.com/archestra-ai/OpenAPPA/issues/48)) ([4ed796e](https://github.com/archestra-ai/OpenAPPA/commit/4ed796e7265ecc2a3381423e52b9e3a2a73d09a0))


### Documentation

* 🚧 propose resolver envelope v2 ([#49](https://github.com/archestra-ai/OpenAPPA/issues/49)) ([b7e74c1](https://github.com/archestra-ai/OpenAPPA/commit/b7e74c149bcad9cdb52413ae34db86bc073b9f86))
* **bench:** report 5-rep baseline and redteam-chaos benchmark evaluation ([#32](https://github.com/archestra-ai/OpenAPPA/issues/32)) ([06affe5](https://github.com/archestra-ai/OpenAPPA/commit/06affe5f7ce0a87e0ba0896fb929c2148c076a29))
* clarify headings and technical copy in how-it-works and contracts ([#29](https://github.com/archestra-ai/OpenAPPA/issues/29)) ([3bca9b6](https://github.com/archestra-ai/OpenAPPA/commit/3bca9b632c22b5794badd7950f690d196076f42b))
* drop the duplicated merge-commit entries from the 0.2.0 changelog ([#23](https://github.com/archestra-ai/OpenAPPA/issues/23)) ([306a91d](https://github.com/archestra-ai/OpenAPPA/commit/306a91d4bc4323d1afe92a8a356006e17736ec6d))
* invite readers to the Discord server ([#30](https://github.com/archestra-ai/OpenAPPA/issues/30)) ([27d87a8](https://github.com/archestra-ai/OpenAPPA/commit/27d87a87d3a769e66ec30c31001eaef6f00becf2))
* propose resolver envelope v2 ([b7e74c1](https://github.com/archestra-ai/OpenAPPA/commit/b7e74c149bcad9cdb52413ae34db86bc073b9f86))
* refine OpenAPPA onboarding ([#28](https://github.com/archestra-ai/OpenAPPA/issues/28)) ([5e89421](https://github.com/archestra-ai/OpenAPPA/commit/5e894216ece4cf643f4dc4845b15674fea7ce23f))
* refine proposed resolver interface ([#51](https://github.com/archestra-ai/OpenAPPA/issues/51)) ([9dcd7d4](https://github.com/archestra-ai/OpenAPPA/commit/9dcd7d447874f0ceffb77521c8b46d31c056d392))
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

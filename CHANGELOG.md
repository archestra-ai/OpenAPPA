# Changelog

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

# Runs behind REPORT.md

The published tables pool two sets of run directories under `runs/`, which is
not committed. Rows do not record the code revision; the revisions below follow
from file times and commit order.

| Set | Arms used | Harness | Last row written (2026-09-24, CEST) |
|---|---|---|---|
| `release2-r1..r3` | `none`, `rules`, `optimal`, `sticky`, `sticky-intent` | as of `b704359b` | 15:17 |
| `final-r1..r3` | `appa-q`, `none-q` | as committed in `baf95e10` | 20:36 |

`release2` also holds an earlier `appa-q`, from before the policy's trust ranks
were renamed; the final replays supersede it. Commits after `b704359b` change
no prompt, tool, guardrail or scoring for the other arms. Every row has
`"error": null`.

The published tables:

```sh
uv run python -m appa_aicomp.headline \
  --corpus runs/release2-r*/corpus-*/ runs/final-r*/corpus-*/ \
  --triage runs/release2-r*/triage-*/ runs/final-r*/triage-*/ \
  --exclude 'appa-q=release2-r*/*'
uv run python -m appa_aicomp.control runs/final-r*/triage-*/
```

| Directory | Model | Rows per arm | Errors | sha256 of `rows.jsonl` |
|---|---|---|---:|---|
| `release2-r1/corpus-gemma4-26b` | `google/gemma-4-26b-a4b-it` | appa-q 187, none 187, optimal 187, rules 187, sticky 187, sticky-intent 187 | 0 | `552186c94cb0f5baf1ca57cda962ce7639de887c51eea0aaaaf68df85e6c010d` |
| `release2-r1/corpus-glm53flash` | `z-ai/glm-5.3-flash` | appa-q 187, none 187, optimal 187, rules 187, sticky 187, sticky-intent 187 | 0 | `e3629b6fbe9c36d634c6aad8eeb73c1458a90506133dd62b2d3918b4860b1cf1` |
| `release2-r1/corpus-gpt6luna` | `openai/gpt-6-luna` | appa-q 187, none 187, optimal 187, rules 187, sticky 187, sticky-intent 187 | 0 | `ac8db2172b5b5bff812d2b9730e7ae465054d41e181f1621faec3342db294b41` |
| `release2-r1/corpus-gptoss20b` | `openai/gpt-oss-20b` | appa-q 187, none 187, optimal 187, rules 187, sticky 187, sticky-intent 187 | 0 | `0a671c4eeccf4792b924a73da1a9924842348f089cac9f81a4ce50ddd239ab35` |
| `release2-r1/triage-gemma4-26b` | `google/gemma-4-26b-a4b-it` | appa-q 319, none 319, optimal 319, rules 319, sticky 319, sticky-intent 319 | 0 | `9776e600e827ea1ecc8aa3369eb236a5dce0b4ffd3325cc55b9df287af8b2ca8` |
| `release2-r1/triage-glm53flash` | `z-ai/glm-5.3-flash` | appa-q 319, none 319, optimal 319, rules 319, sticky 319, sticky-intent 319 | 0 | `887c8932d2285156ac7c0082acc5186bbf70870466db615e710391b35bfbcee8` |
| `release2-r1/triage-gpt6luna` | `openai/gpt-6-luna` | appa-q 319, none 319, optimal 319, rules 319, sticky 319, sticky-intent 319 | 0 | `d9addbe67cd4e42b62b41125325714b233290de0c7ad190ebd226344769c1f73` |
| `release2-r1/triage-gptoss20b` | `openai/gpt-oss-20b` | appa-q 319, none 319, optimal 319, rules 319, sticky 319, sticky-intent 319 | 0 | `d04c913a8d3f56e77373302a28e28f689de955a0d4c27f9c86265f93318a2d56` |
| `release2-r2/corpus-gemma4-26b` | `google/gemma-4-26b-a4b-it` | appa-q 187, none 187, optimal 187, rules 187, sticky 187, sticky-intent 187 | 0 | `1b54fb200625b1deac0de7890d257bad5d06af5251e312b7121c21f7cb316d37` |
| `release2-r2/corpus-glm53flash` | `z-ai/glm-5.3-flash` | appa-q 187, none 187, optimal 187, rules 187, sticky 187, sticky-intent 187 | 0 | `28581c2774a6eca4e2279875499a1c22ac22bd1ee1e842a0cff27e2853412f43` |
| `release2-r2/corpus-gpt6luna` | `openai/gpt-6-luna` | appa-q 187, none 187, optimal 187, rules 187, sticky 187, sticky-intent 187 | 0 | `3112182595a969529eed5f913aab4e536c2108053aae5b88825cfafd0ef0f419` |
| `release2-r2/corpus-gptoss20b` | `openai/gpt-oss-20b` | appa-q 187, none 187, optimal 187, rules 187, sticky 187, sticky-intent 187 | 0 | `15fe3a12326fb41042f410b45579d63a2db419dd86a01c7a383794a174844167` |
| `release2-r2/triage-gemma4-26b` | `google/gemma-4-26b-a4b-it` | appa-q 319, none 319, optimal 319, rules 319, sticky 319, sticky-intent 319 | 0 | `39a9377be4bc2e82b685d9705ec7e9dcc494c459c0f050f915453403bad0d8d1` |
| `release2-r2/triage-glm53flash` | `z-ai/glm-5.3-flash` | appa-q 319, none 319, optimal 319, rules 319, sticky 319, sticky-intent 319 | 0 | `9b386f98c1945c7a20e82874eaf7bb835e5089eab87ac70d77f7550328aafbd6` |
| `release2-r2/triage-gpt6luna` | `openai/gpt-6-luna` | appa-q 319, none 319, optimal 319, rules 319, sticky 319, sticky-intent 319 | 0 | `1dc29274e8387dc18b74333c56326e40f4c5a707412845613e828583bbebae9b` |
| `release2-r2/triage-gptoss20b` | `openai/gpt-oss-20b` | appa-q 319, none 319, optimal 319, rules 319, sticky 319, sticky-intent 319 | 0 | `bf436046c49390aaeda47fb93a6ed5326759591f64dd8fcb44bb9159c04dd470` |
| `release2-r3/corpus-gemma4-26b` | `google/gemma-4-26b-a4b-it` | appa-q 187, none 187, optimal 187, rules 187, sticky 187, sticky-intent 187 | 0 | `f0815aca390c62d612c9e8de7885c88d61000c887bf4005b25bfe38f663d2f0e` |
| `release2-r3/corpus-glm53flash` | `z-ai/glm-5.3-flash` | appa-q 187, none 187, optimal 187, rules 187, sticky 187, sticky-intent 187 | 0 | `4d57e15df7bd1f8b0bc6b653b568c9f000f211d0645a15252ac7bc5ec5f5391b` |
| `release2-r3/corpus-gpt6luna` | `openai/gpt-6-luna` | appa-q 187, none 187, optimal 187, rules 187, sticky 187, sticky-intent 187 | 0 | `79106ed0d5b61f25248fe5864174d46ebdac404d86d720d77e3c3547876710ee` |
| `release2-r3/corpus-gptoss20b` | `openai/gpt-oss-20b` | appa-q 187, none 187, optimal 187, rules 187, sticky 187, sticky-intent 187 | 0 | `64b92ce0e05d66f89cee2de263ddbf05bb7be49a5b5db20c3171ebb39a3cc5a2` |
| `release2-r3/triage-gemma4-26b` | `google/gemma-4-26b-a4b-it` | appa-q 319, none 319, optimal 319, rules 319, sticky 319, sticky-intent 319 | 0 | `b843f896ac3adab36c83ac1d6a3702dcb0c8382e545966c586039bf8a0e6d7e1` |
| `release2-r3/triage-glm53flash` | `z-ai/glm-5.3-flash` | appa-q 319, none 319, optimal 319, rules 319, sticky 319, sticky-intent 319 | 0 | `8566c63d554261fb8c9c4df938e598b9a466194cfea4fb19353a6f8bda8dcdce` |
| `release2-r3/triage-gpt6luna` | `openai/gpt-6-luna` | appa-q 319, none 319, optimal 319, rules 319, sticky 319, sticky-intent 319 | 0 | `4280c919ae53d90413adcd2b3cda31c34eac79a678f0609c29ac8f2355a269c8` |
| `release2-r3/triage-gptoss20b` | `openai/gpt-oss-20b` | appa-q 319, none 319, optimal 319, rules 319, sticky 319, sticky-intent 319 | 0 | `6bad9a9961c2bee1531ad2c1837e77fa1798679b68dd3a0770c3669c7f956245` |
| `final-r1/corpus-gemma4-26b` | `google/gemma-4-26b-a4b-it` | appa-q 187 | 0 | `5a0ca995a92be32f28ada171c18615b369343563f2fd47f216a7e33db565e6fb` |
| `final-r1/corpus-glm53flash` | `z-ai/glm-5.3-flash` | appa-q 187 | 0 | `e9f1e8fe6b7652e988a8002b8c6e7a1e8330d2475e4d105dc7cffa8325fdf454` |
| `final-r1/corpus-gpt6luna` | `openai/gpt-6-luna` | appa-q 187 | 0 | `c0a0dc0845233ef95270065839b2405c9595c82d3f35eb16de4dae0b6e3cde3e` |
| `final-r1/corpus-gptoss20b` | `openai/gpt-oss-20b` | appa-q 187 | 0 | `dd8411920bbb802a22b3814cef6b29a6fd8c51bf66494cb10451884eb6b70279` |
| `final-r1/triage-gemma4-26b` | `google/gemma-4-26b-a4b-it` | appa-q 319, none-q 319 | 0 | `b11cea719d625d74a1d434efdfc6f838ef2e9fb2c31a923b259685e81dc607bf` |
| `final-r1/triage-glm53flash` | `z-ai/glm-5.3-flash` | appa-q 319, none-q 319 | 0 | `0c152ec7423a460149397f00461730aee5e2b5ae37e5b364165df6d63038e60c` |
| `final-r1/triage-gpt6luna` | `openai/gpt-6-luna` | appa-q 319, none-q 319 | 0 | `9a643f8529defdd4a0958dcc06205edd7fe8af927c6dece2912aebcc873f598c` |
| `final-r1/triage-gptoss20b` | `openai/gpt-oss-20b` | appa-q 319, none-q 319 | 0 | `83bac3c1c8cee44ffceaac521eb8014ceffea1f25c539fddf9050e025bbc6057` |
| `final-r2/corpus-gemma4-26b` | `google/gemma-4-26b-a4b-it` | appa-q 187 | 0 | `6fd8a9fe6716ef0a69d8e6da96f740f9b68b6a9b10c4d70bbdc6cc8ae7cd1494` |
| `final-r2/corpus-glm53flash` | `z-ai/glm-5.3-flash` | appa-q 187 | 0 | `9a7a7cbebf32fffb7c0bd8e1a0eba6b76e291c439590fe17225bce17da5f7df5` |
| `final-r2/corpus-gpt6luna` | `openai/gpt-6-luna` | appa-q 187 | 0 | `35f826c963ac8ab0913013e35153c21e08842bff69440344ef40c532f9a8ab68` |
| `final-r2/corpus-gptoss20b` | `openai/gpt-oss-20b` | appa-q 187 | 0 | `2072065e63da241c546b1808e54ab8d3d1746b9b3b3a7d78bced554d00450b80` |
| `final-r2/triage-gemma4-26b` | `google/gemma-4-26b-a4b-it` | appa-q 319, none-q 319 | 0 | `8da8d3748b44ce5a4428a88eda6a1c65592cab80732d2e4473eb5e34c3c1ed37` |
| `final-r2/triage-glm53flash` | `z-ai/glm-5.3-flash` | appa-q 319, none-q 319 | 0 | `51d7424a7a11772faf87c83a57f7a74ac48ca294dca5c4997a029d868c0ac4db` |
| `final-r2/triage-gpt6luna` | `openai/gpt-6-luna` | appa-q 319, none-q 319 | 0 | `ef37119acbc91520b941292b3e7e0f783858933995cf35aee70775cbe6bcee03` |
| `final-r2/triage-gptoss20b` | `openai/gpt-oss-20b` | appa-q 319, none-q 319 | 0 | `411400492d34325c61de80d8900b47aa0a8f3a6c8d5bcd3eb7551bfd5fe3ef49` |
| `final-r3/corpus-gemma4-26b` | `google/gemma-4-26b-a4b-it` | appa-q 187 | 0 | `69b70e0750c130e4642a328db679d957344badfe58c50f0e3627b20cb030118f` |
| `final-r3/corpus-glm53flash` | `z-ai/glm-5.3-flash` | appa-q 187 | 0 | `a9dca537e7b04c7846a41ba0dec7436e1716366ef86db8e844e710de92d9cece` |
| `final-r3/corpus-gpt6luna` | `openai/gpt-6-luna` | appa-q 187 | 0 | `ba4d96c62f0dba7eb3ca2fd8cbfadac5c528eacba607c6589556f8d48923cdcf` |
| `final-r3/corpus-gptoss20b` | `openai/gpt-oss-20b` | appa-q 187 | 0 | `c42b256ed778e24646debb741856f8d295a08875c3ef4391857e413c156f0fef` |
| `final-r3/triage-gemma4-26b` | `google/gemma-4-26b-a4b-it` | appa-q 319, none-q 319 | 0 | `55428bd3e9dca6cd6277c7ade9e99d2bd2921edc06c86dd07a055a0006399342` |
| `final-r3/triage-glm53flash` | `z-ai/glm-5.3-flash` | appa-q 319, none-q 319 | 0 | `a1509270cb90b489b7946072658b4717d4bd6a28e997d072f90e17e31a111e1c` |
| `final-r3/triage-gpt6luna` | `openai/gpt-6-luna` | appa-q 319, none-q 319 | 0 | `76efd8c7f38e7c74d57d91fcda0bc39321a3d8832d49e0c59c55747952d18cc7` |
| `final-r3/triage-gptoss20b` | `openai/gpt-oss-20b` | appa-q 319, none-q 319 | 0 | `a66deffc25005138ff1877850ebaca7c860041cdcbf0a809e0330eee6e71c15e` |

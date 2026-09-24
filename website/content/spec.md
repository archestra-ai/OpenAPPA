# APPA specification

**Status: draft.** This document is the normative specification of the APPA model. It specifies what an implementation MUST do. It does not explain why. The design arguments exist in `rationale.md`. The document `../website/content/docs/how-it-works.md` provides a readable introduction.

Rules carry IDs by family. Cite rule IDs from tests, issues, and paper references. Cite IDs rather than section numbers. A rule keeps its ID when moved. Therefore, family numbering does not always follow document order. Every normative statement carries an ID. `MUST`, `MUST NOT`, `SHOULD`, and `MAY` hold RFC 2119 meanings.

**Rule Families:**
`POS` position and coverage · `LBL` labels · `CHK` the check · `RMD` remedy plans · `AUT` authorities · `RUL` rulings · `SAN` sanitizers and casts · `LOG` effects and history · `BRN` branching · `UNK` Unknown · `CFG` load-time rules · `EXT` external interfaces · `IMP` implementation shape · `THR` threat model.

This document records logic only, not implementation status. Implementation status lives in `engine.md`. A **Design direction** tag marks a decision that is agreed but not yet specified.

---

## 1. Position and coverage

APPA is a kernel that MUST observe the complete trajectory and gate the release of every tool call. The baseline host is an **inference gateway**: every model request and response of the deployment passes through it. Components behind the gateway — a tool gateway, harness hooks, a branch adapter, a trajectory-token channel — extend what the deployment **covers**. They change which features load, never what a check decides (`POS-3`). A harness CAN host the engine directly instead: hosting discharges the execution assumptions of `THR-8` by construction, and the remaining coverage is declared the same way (`POS-10`).

A pure MCP gateway CANNOT host APPA. An MCP gateway sees tool calls, but it does not observe the trajectory that labels them.

APPA removes no capability from a deployment. A deployment declares what it covers, and the loader refuses only the policy constructs whose feature is uncovered (`POS-10`). What stays uncovered is an **open vector**: named, explicit, and auditable, never silent (`THR-3`).

- **[POS-1]** The engine MUST check every proposed tool call before any component can execute it, and MUST fold every admitted value into the label. At the gateway, the check runs while the engine holds the model response: a refused call never reaches the harness.
- **[POS-2]** A deployment is **confining** at an application point if it CAN keep the raw result out of model context there. Pending-cast admission (§11.1), quarantined branches (§10.1), and output sanitization at ordinary tool-result admissions (`SAN-2`) depend on this exact capability. These features exist ONLY where the deployment covers it. Confinement is a model-context property: the harness MAY still hold the raw value. The declaration is boolean per application point (`POS-10`).
- **[POS-3]** Checks and label propagation MUST behave identically under every coverage. Coverage decides which features load and which remedy plans exist. Coverage NEVER affects which flows pass.
- **[POS-4]** A channel that already shows every message to a fixed reader set regardless of label is out of scope. The label CANNOT un-show what the channel exposes.
- **[POS-5]** A deployment is **context-controlling** if it CAN choose what a child branch sees and receive what the branch returns. Branching (§10) exists ONLY in context-controlling deployments. Context control is weaker than confinement. A host CAN bound child context while showing its own model every tool result. Quarantined branches require both capabilities. In a context-controlling deployment a spawn opens a branch (§10). The child starts under the parent's label and the parent's recorded denials (`BRN-3`), so a parent cannot use a spawn to hand a child something a check would have refused. A deployment treats a spawn as a flow to an uncontrolled sink only when it cannot control that child: the spawn then carries an ordinary contract, the spawned run is not a branch — no snapshot, no shared log, no `submit_result` (§10 does not apply) — and its return is admitted as an ordinary tool result. A deployment that can control the child MUST open a branch, never an uncontrolled sink.
- **[POS-6]** In a deployment that rebuilds model requests, the transcript head — the system and developer messages opening every request — is host configuration. The transcript head MUST NOT be client input.
- **[POS-7]** Release and execution are distinct acts: the engine releases a checked call, and an executor runs it. **Dispatch control** — the executor runs only engine-released calls, unchanged, at most once — holds per tool, not per deployment.
  - A tool is **enforced** when a component consumes the engine's release before execution: a tool gateway that consumes one open `DispatchId` per call, or a pre-tool harness hook. A tool with no such component is **assumed**: it executes on the assumptions of `THR-8`.
  - A `CallApproval` is earlier than release. It records one exact call that the agent selected from an offer and may propose next. It is not a `DispatchId`, reserves no effects, changes no label, and lets no executor run. A later matching singleton proposal consumes the approval and opens the real dispatch (`RUL-5`).
  - `DispatchId` is the sole engine-facing execution correlation handle. It binds the trajectory, canonical call digest, and one occurrence. A host reports `DispatchId` plus the outcome; the engine accepts a result only for the matching open dispatch and at most once. There is no separate execution grant, receipt, or transport-security token in the engine API. Runtime and hosts MAY keep additional diagnostics, but those grant no engine authority.
  - A **provider-run tool** executes inside the inference call, before the engine sees anything. It is therefore not a checked call but an ingestion surface: its contract MAY declare a static `delta` and `emits` — a `requires`, dynamic delta, or pending-cast delta on it is a load error (`CFG-23`) — and it MUST NOT appear in any remedy plan — the check and the plans govern dispatches the engine can gate (`POS-1`, `RMD-1`). A proposed call that names a provider-run tool is malformed, exactly as an unknown tool is (`CHK-18`): no executor of the deployment can run it. When the response exposes a provider-run result, runtime submits it in the same proposal batch as that response's proposed calls (`IMP-1`). The engine admits it as an ordinary value with provider provenance under the tool's declared `delta` and records the declared effects in that same pre-sibling admission checkpoint. This is best-effort observation after provider execution, not a dispatch success: it has no release, reservation, `DispatchId`, or typed outcome, and an execution whose result is not exposed establishes no effect. The admission and observed effects precede sibling checks because the model has already read the result. A provider surface whose results the response does not expose cannot be mediated this way; the deployment refuses it or declares it an open vector (`POS-10`).
  - A deployment MAY strip provider-run tools from the request, which closes the vector. A deployment that allows them declares their dispatch — the model-authored input leaving to the provider — an open vector (`POS-10`).
- **[POS-8]** The host MUST bind every engine-relevant event — a proposal batch at a gateway, a harness event at a hook — to exactly one trajectory before any check runs, and MUST persist the outcome — the bound trajectory, a mint, a refusal — in its outer records. Request identity, transport retry, transcript lifecycle, and user turns are host concerns: they are not engine events or facts and grant no engine authority (`IMP-1`, `IMP-4`). Two binding modes exist, and the deployment declares one (`POS-10`):
  - **Token binding:** The host mints an opaque **trajectory token** when a trajectory starts and returns it outside model content. The client MUST return the token on every continuation. A request with no token MAY ask the host to start a trajectory; request idempotency and recovery of a lost opening response belong to that host API. The host MUST refuse a request with an invalid token.
    - The token is a proportionate routing handle, not APPA's authentication boundary: at least 128 random bits from the host OS (a UUIDv4 is sufficient), no embedded claims, signing, encryption, refresh protocol, or independent rotation. Transport protection and principal authentication belong to the trusted surrounding deployment.
    - Runtime durably maps the token to exactly one internal trajectory and authenticated tenant/principal. The binding prevents accidental and opportunistic cross-principal use; APPA does not claim to resist a principal or host already malicious inside that trusted boundary. Unknown and foreign tokens receive the same generic refusal.
    - Token lifetime is trajectory lifetime. Deleting the trajectory deletes the mapping; there is no separate expiry clock or grace set. A restart restores the mapping with the trajectory. Missing mapping state is a binding failure and MUST be refused, never reconstructed by guessing or replaced with a fresh trajectory.
    - Minting the root, mapping the token, and writing `TrajectoryOpened` are one durable operation. A token collision retries the mint before anything is published.
    - Successful and refused binding outcomes are outer runtime records. A refusal carries the host request identity, when one exists, and a generic reason class for replay and debugging; it is not a security-alert protocol.
  - **Harness binding:** The harness hosts or feeds the engine (`THR-8`) and names the trajectory on every event it delivers. No model request crosses a wire the host must match: the binding holds by construction of the harness, and accepting its ids is part of the declared `THR-8` trust. The host MUST refuse an event that names a trajectory it does not know, and MUST refuse a new trajectory under an id that already carries history within its root — a reused id MUST NOT continue another trajectory. Identity is qualified by the root: the engine reads one root's log and judges a child id against that log alone, so two roots MAY carry the same child id and neither continues the other's history. Every event therefore names its root as well as its trajectory. Branching stays available where the harness controls child context (`POS-5`).
  - **Invariants:** The model MUST NOT see binding data in any mode. The token map is outer-layer state (`IMP-3`): its loss degrades to a binding failure, never to a mis-bind.
  - Token-binding conformance MUST cover restart recovery, token collision retry,
    same-principal continuation, generic cross-principal/unknown refusal,
    mapping-loss refusal, and invalidation when trajectory retention deletes the mapping.
- **[POS-9]** Principal-supplied context — a user message, pasted document, or imported transcript — is outside engine policy. It is not an engine event, fact, or labeled value; it contributes nothing to the fold and needs no admission. What the principal supplies is the principal's act (`THR-2`). A host MAY retain it in an outer transcript or trace. A deployment that needs labeled ingestion routes the content through a tool contract, and the ordinary machinery applies.
  - A provider-side reference that dereferences into model context — a stored file, a prior response — admits content the engine never sees. For each such surface the deployment MUST choose one of three: mediate it, refuse requests that carry it, or declare it an open vector that voids label completeness for the trajectory (`POS-10`, `THR-3`).
- **[POS-10]** A deployment declares its **coverage** in the policy file. One immutable, typed
  **deployment profile** — the `[deployment]` table beside the contracts it governs — is the
  declaration for every complete outer-layer host. It carries:
  - The starting label (`LBL-8`).
  - Per tool, the executor class: `enforced`, `assumed`, or `provider_run` (`POS-7`). The table
    names one deployment default and per-tool exceptions.
  - Per application point, confinement as a boolean (`POS-2`).
  - Context control (`POS-5`) and the binding mode (`POS-8`).
  - The allowed provider surfaces (`POS-7`, `POS-9`): each listed surface is `mediated` or
    `open`. A surface the table does not list is refused.
  - A policy file with no `[deployment]` table declares no coverage: every tool is assumed, no
    point is confined, context is uncontrolled, no provider surface is allowed, binding is
    `harness`, and the starting label is the neutral value (`LBL-8`). Transcript bytes, user
    turns, host runs, and request/response delivery records
    remain outside engine facts in every declaration (`POS-6`, `POS-9`, `IMP-4`).
  - **Typed shape.** The executor class is a closed three-state union that cannot confuse an
    enforced executor, an assumed executor, and a provider-run ingestion surface. Binding is
    `token` or `harness`; neither carries a recovery choice (`POS-8`).
  - **Validation ownership.** The inner layer owns these types and the one pure policy × profile
    validation matrix, and the loader runs it: policy and declaration validate together in one
    load. The outer runtime owns the only complete `open` operation: it checks that concrete
    host bindings implement the declaration and establishes the durable opening record. A
    gateway or SDK facade enforces the validated result on requests; it MUST NOT maintain a
    second interpretation of coverage.
  - **Validation & Enforcement.** A policy construct that names an engine behavior the deployment
    cannot perform is a load error, while a weaker executor class or an allowed provider surface
    loads, and its weakness is an **open vector**. Open vectors are derived canonically from the
    normalized declaration — they are not a caller-supplied acknowledgement list. Exactly three
    vector kinds exist: an assumed executor, an allowed provider-run dispatch, and an `open`
    provider surface. The host MUST enforce the declaration on every request: it strips an
    undeclared provider surface, or refuses the request. The declaration and its derived open
    vectors are explicit and auditable (`THR-3`).
  - **Policy identity.** The policy digest is the typed digest of `PolicyIdentityV1`: the
    canonical form of the normalized engine-visible policy and deployment declaration. The
    canonical form preserves semantic sequences and sorts true maps and sets. It excludes
    source syntax, runtime bindings, `[limits]`, and hints (`CFG-21`, `CFG-25`).
  - **Durable opening.** The first record of every root trajectory family is `TrajectoryOpened`.
    It carries the policy dialect version, the canonical declaration in full, the policy digest,
    and the derived open-vector set. The starting label is initialized
    from this record, never from a synthetic admitted value. Children inherit the family opening
    and carry their own fork-time seed (`BRN-3`). The opening record binds the root trajectory
    and its children to the opening policy. This binding does not change. A policy or
    declaration change does not change a root trajectory that is already open. A new root
    trajectory opens under the policy that is loaded at that time. The engine MUST fold and
    check a root's log only under that root's opening policy. Cold replay refuses a supplied
    policy whose digest differs from the opening record.
  - **Policy retention.** When a root trajectory opens, the host stores the policy file bytes
    it opens under. The key of the stored file is a hash of its exact bytes. If the key is
    already present, the host stores nothing. One stored file serves each root trajectory that
    opened under the same bytes. A new entry appears only when the file changed. Two different files MAY have the same policy
    digest, because the digest excludes source syntax, runtime bindings, `[limits]`, and hints.
    The exact-bytes key identifies exactly one file. Each opening's durable record stores the
    exact-bytes key and the policy digest: the `TrajectoryOpened` record carries both, so the
    binding is durable exactly when the opening append is. A stored file is write-once. The host
    MUST NOT replace the bytes under a key. The host MUST NOT delete them. To decide an event on a root
    trajectory or its children, the host loads the stored file of that root's opening. If the
    stored file is missing, the host MUST refuse those events. If the host cannot load the
    file's dialect version, the host MUST refuse those events. The host MUST NOT decide them
    under a different policy.
  - **Externals of an earlier policy.** A check or a remedy step MAY name an external component
    that the deployment no longer runs. The host MUST refuse that call as an operational
    refusal (`EXT-1`). The refusal creates no engine fact. The refusal reaches the operator.
    The call can run later, if the component returns. If the deployment wires a different
    backend to the same component name, the current backend answers (`CFG-15`). This is a
    change of the trusted base. The deployment accepts this change.
  The complete outer API has this shape; concrete language bindings MAY rename the types but not
  split validation among facades:

  ```text
  deployment = Deployment.open(policy, store, host_bindings)
  trajectory = deployment.open_trajectory()
  ```

  A host may wrap the trajectory in request, run, or transcript abstractions. Those wrappers are
  not a second profile and do not change engine semantics.

  Conformance MUST cover at least: starting-label refusal; every `CFG-23` policy/coverage pair;
  provider-run tools excluded from plans and refused as proposals, with exposed results'
  observed deltas and effects ingested; exact derived open vectors for every weak choice; request enforcement against
  undeclared provider surfaces; principal context never entering engine records; and cold replay
  reconstructing the same starting label, coverage, and open vectors using only the durable family
  records and a policy with the recorded digest.

---

## 2. Labels — `LBL`

- **[LBL-1]** A label has exactly two dimensions: **trust** and **audience**. The label structure is fixed. Deployment configuration defines specific instances as data.
- **[LBL-2]** Trust is a finite ordered chain of ranks. Default instance: `suspicious < trusted`. A deployment MAY supply its own chain.
- **[LBL-3]** Audience is a set of readers. A reader ID is an opaque atom (`THR-6`). `public` is the one unrestricted audience state: it denotes the absence of audience restriction, not an enumerated set. Concrete readers enter through policy literals and resolver answers.
- **[LBL-4]** In the current dialect, every restricted audience reaching the algebra is an explicit ID list; `public` is the one non-list state (`LBL-3`). Audience intersection ($\cap$) and subset ($\subseteq$) operations are exact set operations. A deployment expresses customer classification schemes by writing each tier as its member readers.
- **[LBL-5]** Each trajectory projection retains a **partial label** per dimension: an established bound plus the set of source `ValueId`s whose contribution remains `unknown`. Folding intersects established audiences, selects the minimum established trust rank, and unions the unresolved source sets. An `unknown` contribution is identity for the established bound, not permission to erase restrictions already known (`UNK-2`). The engine MUST NOT truncate these sets or refuse an otherwise valid admission because their cardinality crossed an engine-configured ceiling: every entry names a durable source whose contribution is still undecided, so dropping one would manufacture permission. A host MAY stop accepting further activity under an outer resource quota, but that operational refusal creates no engine fact and changes no label.
- **[LBL-6]** Every `delta` MUST be restrictive. No permissive delta exists. Engine operations MUST NOT widen a label on either dimension.
- **[LBL-7]** The current partial label is the fold of the starting label with the source-indexed label of every admitted value (`LBL-5`, `LBL-10`). Each admission persists the label it folds on its record (`LBL-14`); a cast fact establishes one source's complete label atomically. The engine MUST recompute both established bounds and unresolved source sets from the log alone. Cached folds are views, never independent state.
- **[LBL-8]** The deployment profile defines the starting label, and each root family's
  `TrajectoryOpened` record snapshots it (`POS-10`). The neutral, least restrictive value is the
  `public` audience and the top rank of the trust chain (`LBL-2`) — `{audience: public, trust:
  trusted}` under the default chain. Only that opening initializes the root fold; outer context is
  not an admitted value (`POS-9`). Both starting dimensions MUST be established: an `unknown`
  starting dimension has no source value a cast could resolve and is a profile-load error.
- **[LBL-9]** A ruling MUST NOT change the label. See `RUL`.
- **[LBL-10]** All data flows through engine policy as a **labeled value**: a value bound to its information label. The admitted values are a tool result bound to its call, a registered derivation admitted in its place (`SAN-2`), an exposed provider-run result (`POS-7`), and a child return (`BRN-5`). The engine MUST NOT separate value from label: an engine policy operation takes both or neither. Runtime adapters may transport content alone after admission because they do not perform policy operations.
- **[LBL-11]** The label folds ONLY from **admitted** values. A call that succeeds but admits no value — such as an oversized body or a refused derivation — appends its effects and folds nothing.
- **[LBL-12]** Deriving a cleaner value from an admitted value MUST NOT undo the fold. Sanitizing data after admission CANNOT clear a trajectory. The derivation carries its own label, and the run keeps the label it took.
- **[LBL-13]** Configuration MAY write a **group** — `@name` — where it writes reader IDs, and an actual string argument read by an `includes($recipient)` placeholder MAY name the same form (`CHK-9`). A group is a name for a reader set held by a directory. The registered **membership resolver** (§13) turns the name into the literal set. The algebra never stores a group name: labels, records, and rulings hold resolved reader IDs only (`LBL-4`). `public` is a reserved audience state, not a reader ID: it is never a group member, and a resolution answer that contains it is malformed (`EXT-1`). As a placeholder argument it denotes the Public audience directly; it is not sent to membership resolution or wrapped as a reader ID (`CHK-9`). A written reader list resolves to the union of its literal reader IDs and the members of each group it names. The deployment starting label and the boundary label are literal: no operation resolves them (`LBL-8`), so a group written there is a load error.
- **[LBL-14]** A group resolves when the engine first reads it for an operation: at the admission of an exposed provider-run result (`POS-7`), at the check that computes the committed label (`CHK-2`, `CHK-4`), at mandate validation (`RUL-4`), at sanitizer application (`SAN-4`), at the selection of a cast for an unresolved source and at the validation of its answer against its constant or ceiling (`SAN-8`).
  - The check operation extends through its block and the block's plan enumeration: a mandate ceiling or a transition read there (`RMD-9`, `RMD-18`) shares the act's resolutions, and a group first read there resolves and pins within the same act.
  - The resolution is pinned for that operation: the set the check resolved is the set the admission folds (`CHK-7`), so an accepted narrowing cannot drift between check and admission, and a block cannot report a gap against one reader set and offer plans against another.
  - Across operations, resolution is fresh: a member added to the directory reaches the next operation. Removal reaches only future resolutions: a set already resolved stands. One release is one resolution: an operation that executes a recorded offer, consumes a recorded approval, or admits a recorded dispatch's result starts from the resolutions that record persists, and only a group not pinned there resolves fresh in it (`RUL-5`, `CHK-7`).
  - Every consumed resolution is persisted on the record of the outcome it fed — the decision and its dispatch, the block and its plan offers, the approval and its ruling, the derivation, the child return, the cast record, and a provider-run admission. Records hold the resolved literal readers, never the group name (`LBL-13`). Therefore `LBL-7` and `IMP-1` stand: replay reads records, never the directory.
- **[LBL-15]** Only a successful membership answer enters the engine. If runtime obtains no answer, it does not resume the consuming operation and no group, gap, Unknown contribution, or engine fact is created (`EXT-1`). A successful empty reader set is valid evidence and is distinct from no answer. In a cast ceiling, submitted evidence is still checked against the resolved literal ceiling (`SAN-8`).
- **[LBL-16]** A dynamic resolver maps one top-level string argument to literal reader IDs. It is distinct from `@group` membership resolution. The runtime MUST extract the configured argument from the proposed call. A missing or non-string argument fails resolution.
- **[LBL-17]** Dynamic resolution occurs once when a proposed call first checks. The answer remains pinned through rechecks, plans, rulings, dispatch, and admission. A group named by an actual `includes($recipient)` argument has the same proposed-call lifetime: its first successful membership answer is pinned with the call, while a new proposed call resolves again. The dispatch record MUST persist every consumed call-bound resolution. Blocks persist the resulting literal narrowing or gap. Rulings persist covered readers.
- **[LBL-18]** A successful dynamic answer MAY contain multiple readers or no readers. It MUST NOT contain `public` or an `@group`; such submitted evidence is malformed. Only a successful answer enters the engine. If runtime obtains no answer, it does not resume the proposal and creates no unresolved-recipient gap or Unknown delta (`EXT-1`). A successful empty reader set is valid evidence and is distinct from no answer.

---

## 3. The check — `CHK`

- **[CHK-1]** The engine MUST check every proposed call before dispatch. The outcome is `allow`, or `block` carrying what stopped the call — the unmet requirements, the narrowing where one fired, the values whose needed dimension no registered cast could establish — and the remedy plans.

```ts
type CheckOutcome =
  | { outcome: "allow" }
  | { outcome: "block";
      requirement_gaps: RequirementGap[];  // unmet entries of `requires`
      narrowing?: Narrowing;               // present when the call's own delta fired CHK-2
      unestablished?: UnestablishedValue[]; // sources + all unresolved dimensions no cast fully established (CHK-16)
      remedy_plans: RemedyPlan[] };
```

- **[CHK-16]** APPA turns unknowns into knowns before it decides.
  - When a check consumes any unresolved dimension, the engine requests the applicable registered casts (`SAN-9`) for each source in registration order. Resolution is automatic. It is NEVER an agent choice and NEVER a remedy plan (§5.1). Runtime may try the requested casts in that order and submits only a complete answer. If none answers, runtime does not resume the act and no normative check outcome is produced (`EXT-1`). If no applicable cast exists, `unestablished` names the source once together with all unresolved dimensions.
  - Each submitted complete resolution is re-validated against the cast declaration (`SAN-8`) before admission. Partial, mismatched, or out-of-ceiling evidence is rejected with no batch. Dimensions already established must match exactly (`SAN-7`). One complete cast fact clears an `unestablished` source; no ruling can clear it.
  - The pure core performs no IO. The runtime drives resolution, and the engine admits results (`UNK-8`). A runtime MAY attempt a cast as early as admission — a §11.1 pending-cast tool declares exactly that. An ordinary fold or branch merge does not trigger cast IO; merge-time resolution occurs only where the return policy itself consumes an unresolved dimension (`BRN-14`).

### 3.1 Ordering and clocks

- **[CHK-2]** The **narrowing check** runs first on the established bounds of committed partial label $L' = L \sqcap \delta(c)$. If $L' \sqsubset L$ strictly on either dimension, the engine MUST block $c$ with `narrowing` populated. A raw-result path MUST record acceptance of exactly $L \to L'$ before dispatch. An output-sanitizer path MAY dispatch without accepting that raw narrowing only because the raw result is withheld: the dispatch pins $L$, the bound sanitizer, and its declared first candidate contribution (`RMD-18`). After a derivation arrives, every confined candidate is compared with that pinned receiving bound. It admits immediately where it narrows nothing; otherwise it offers acceptance of exactly its current residual and MAY first progress through further sanitizers (`RMD-19`). Any required acceptance is recorded before that candidate's admission, and binds the pinned dispatch/candidate lineage rather than whatever the live fold later becomes (`UNK-16`). Unresolved sources remain alongside the established bounds and do not erase them (`UNK-15`). A narrowing-only block is a **soft-block**: the raw acceptance plan always exists (`RMD-11`), so the agent CAN always clear it alone.
- **[CHK-3]** If $L' = L$, `CHK-2` does not fire. A repeat call that restricts nothing further passes without a narrowing block.
- **[CHK-4]** **Label requirements** evaluate on the committed label $L' = L \sqcap \delta(c)$. For a call with no delta, $L'$ is the current label. This rule is normative: a requirement evaluated on $L$ alone would let a call outrun its own consequences. One block therefore reports the narrowing and the requirement gaps together, both stated against $L'$. One exception exists: the `includes` check of an input-sanitizer substitution reads the derivation `to` in place of the committed audience (`SAN-3`).
- **[CHK-5]** **History requirements** evaluate on the log as it stands at check time; `no_prior` additionally consults unsettled reservations (`CHK-17`). A call's own `emits` MUST NOT satisfy its own precondition.
- **[CHK-6]** Neither label check is a configuration entity. Both derive from the contract `requires` and `delta`. The evaluation order of §3.1 is fixed. The configuration surface MUST NOT offer a way to reorder or defer checks.
- **[CHK-7]** Effects of an engine-released call append when the call succeeds; an exposed provider-run result records its declared effects at observed admission per `POS-7` and `LOG-2`. The fold commits at admission and takes the admitted value's label: for a released call's result value, the delta as the check resolved it; for an exposed provider-run result, the delta as its admission resolves it (`LBL-14`); or the derivation label where a registered derivation is admitted in its place (`SAN-2`, `RMD-18`).
- **[CHK-18]** **Proposals are one batch.** A model response or harness event MAY propose several calls. The engine checks the complete ordered set as one atomic `ProposalBatch`, placed in the serialized order of `LOG-9`:
  - Exposed provider-run results admit first: they record what the model has already read and the declared effects best-effort observation establishes, so their admission checkpoints persist whatever happens to the siblings (`POS-7`, `LOG-2`). Canonical call construction then precedes the act. If any proposed sibling names an unknown tool, names a provider-run tool (`POS-7`), or has arguments that fail its registered schema, the engine returns one explicit `InvalidCall` error and appends nothing beyond those checkpoints; malformed agent output is not a policy outcome and no sibling is partially mediated (`RUL-3`).
  - No append and no sibling result interleaves the act, so every check in it reads one state. One exception is internal: the cast resolutions the act's own checks require (`CHK-16`) land inside the act, in check order, and an established dimension is state for every later sibling — a resolution is a discovered fact, not a sibling result.
  - Label requirements and the narrowing comparison read that state for every sibling: no call can carry a sibling's result, because all were composed from the same context, and the fold is commutative, so admission order cannot change the label.
  - History requirements evaluate per `CHK-5` on the log as the act sees it, plus the reservations accumulated by earlier siblings (`CHK-17`). Released calls MAY dispatch concurrently; admissions serialize per `LOG-9`.
  - Refused calls return their blocks in proposal order, each carrying its ordered plans (`RMD-15`); the engine imposes no other order across siblings, because the algebra has no magnitude to sort by (`LOG-6`). If the batch also changes policy state, the engine fully re-checks every refused call against the batch's final state. It then derives new blocks and plans and stamps their post-decision `PolicyBasis` (`RMD-8`). It MUST NOT reuse gaps, narrowing, or plans from an earlier sibling snapshot.
  - A proposal that consumes a `CallApproval` MUST be a singleton batch whose one canonical call exactly matches the approval. A different malformed or policy-blocked proposal does not stale the approval on its own; the approval stays current only while no other policy content in the batch — an exposed provider-run admission included — advances its basis. If required external evidence is unavailable, runtime does not resume the proposal and the approval likewise stays current. If a different call is released and changes the relevant `PolicyBasis`, the approval becomes stale in that same decision (`RMD-8`, `RUL-5`).

### 3.2 Requirement kinds

- **[CHK-8]** A **trust floor** (`trust = "r"`) accepts any rank at or above $r$.
- **[CHK-9]** An **`includes`** requirement holds when `audience` $\supseteq$ `recipients`. Static contracts declare recipients directly. A placeholder reads its actual call argument as one audience expression: an ordinary string is one literal reader ID; `public` is the Public audience, so only a Public committed audience includes it; and `@name` is the group whose membership resolver supplies the pinned literal recipient set (`LBL-13`, `LBL-17`). Its contract points only to a required string field (`CFG-14`), so a missing or non-string value is an ordinary `InvalidCall` schema error before the check (`RUL-3`), not a gap. Without successful membership evidence the check does not complete (`LBL-15`, `EXT-1`).
- **[CHK-10]** A **cap** requirement holds when `audience` $\subseteq C$. A cap requirement follows the same clock as every label requirement. A read that narrows into the cap passes and surfaces as an ordinary narrowing block.
- **[CHK-11]** `no_prior(k)` holds when no matching effect exists in the log and no unsettled reservation contains a matching emit (`CHK-17`). Checking does not consume the effect. A ruling MAY waive `no_prior(k)` for one dispatch if the issuer mandate covers the waiver.
- **[CHK-12]** `prior(k)` holds when a matching effect exists in the log. A reservation never satisfies it (`CHK-17`). Nothing is waivable. The remedy is to make the effect happen. A positive `prior(k)` proves either that an engine-released tool reported success or that an exposed provider-run result was observed, and nothing more about the outer world.
- **[CHK-17]** **Reservation.** Release reserves a dispatch's declared `emits`. The reservation stays unsettled until the engine observes success or failure.
  - Check, release, and reservation are one atomic step, placed in the same serialized order that `LOG-9` gives appends: a call that passes its check is released and reserved in that step — no other append or concurrent proposal batch can intervene — so two concurrent checks CANNOT both pass a `no_prior` that either one's reservation fails.
  - A matching reservation fails `no_prior(k)` exactly as an appended effect does, and never satisfies `prior(k)`: both directions fail closed.
  - The reservation **settles** at the observed result: success turns it into the appended effects (`LOG-2`, `UNK-11`), failure evaporates it, and nothing appended. Settlement is not dispatch close: a pending-cast dispatch settles its reservation at observed success and stays open until its confined offer is accepted, invalidated, or made stale by `RMD-8`, so the open-dispatch state of `LOG-5` replays to one answer.
  - Closing a dispatch without an observed result does not settle its reservation. Cancellation and abandonment are such closes. The call MAY have executed, but the engine does not know the outcome, so the reservation stands and fails `no_prior(k)`. A ruling MAY waive that requirement for one dispatch (`CHK-11`).
  - A reservation is lifecycle state, recomputable from the log (`LOG-5`). Therefore, a batch sibling (`CHK-18`) and an in-flight branch dispatch (`BRN-6`) are visible to `no_prior` before their effects land.
  - Provider-run tools have no release and therefore no reservation. Their declared effects begin failing `no_prior` only after an exposed result is durably admitted. Two provider executions may already have happened before either result reaches the serialized log; the provider-run open vector therefore cannot provide the pre-execution exclusion guarantee of a controlled dispatch (`POS-7`, `POS-10`).
- **[CHK-13]** An **attention demand** is per-call and MUST NOT be satisfied by history. An attention demand is met ONLY by a ruling from an authority that attends the mark, committed with the exact dispatch release. A repeat dispatch takes a fresh ruling.
- **[CHK-14]** Narrowing MUST be reported in its own slot and NEVER as a requirement gap. Nothing in `requires` failed. An acceptance, rather than a ruling, answers a narrowing.
- **[CHK-15]** A single call MAY carry both a restrictive delta and a requirement gap. Both gates then apply. Neither gate substitutes for the other.

---

## 4. Tool contracts

A contract declares one contribution per piece of state, plus its requirements and its routing tags.

- **[CFG-1]** `delta` is the label action, checked before it is applied.
- **[CFG-2]** `emits` are the effects a successful call appends, recorded as an unordered set in one append. They are applied and NEVER checked.
- **[CFG-3]** `requires` carries label requirements, history requirements, and attention demands.
- **[CFG-4]** `tags` have no algebraic life. Tags MUST NOT fold, enter a check, or reach the log. The sole use of tags is routing: authority, cast, and sanitizer scope (`AUT-7`, `SAN-9`).
- **[CFG-5]** A contract MAY carry `delta`, `requires`, both, or neither. A call carrying both is checked on both.
- **[CFG-14]** Contracts MAY be **static**, **static with placeholders**, or **dynamic**. A placeholder MUST name a top-level field that the tool-input schema declares as a required string; otherwise the contract is a load error. `[[dynamic_resolver]]` registers each named dynamic resolver before use. A dynamic audience form contains `resolver` and `argument`. It MAY appear directly under `delta.audience`. It MAY appear under `requires.audience.includes`. The resolver maps that top-level string argument to a reader set (`LBL-16`). Dynamic resolvers form part of the deployer trusted base.

---

## 5. Remedy plans — `RMD`

- **[RMD-1]** Every block MUST carry `remedy_plans`: the sound remedies available under registered configuration and deployment coverage (`POS-10`).
- **[RMD-2]** Every plan with an **engine-side** step becomes an executable **offer** carrying an
  `OfferId`. Remedy planning belongs exclusively to the stateless inner engine: it derives the
  complete ordered plan templates from the event-log view, immutable deployment settings, and the
  blocked call. A runtime MUST NOT add, remove, reorder, or reinterpret them. A template has a
  deterministic `PlanKey` within that derived block; `PlanKey` is not a capability and is never the
  model-facing execution argument.
  - For each engine act that may surface offers, the outer runtime supplies one fresh 256-bit random
    `OfferNonce`. The engine derives the `BlockId` and every `OfferId` by domain-separated hashing of
    that nonce with the trajectory, `ProposalBatchId`, call position and digest, deterministic plan
    position, and canonical plan digest. It binds the resulting IDs to its own plans and returns both the
    model-visible outcome and the appendable facts in one call. Runtime supplies entropy and
    persists the result; it never allocates or binds individual offer IDs. A collision with an ID in
    the supplied view is a retryable refusal requiring a fresh nonce.
  - `OfferId`s MUST be fresh per surfaced block. The `OfferId` is the engine's canonical identity.
    The runtime renders it to the model in a short form, and resolves that form back to one
    canonical `OfferId` before any engine event. The rendered form names a choice. It is not a
    capability and carries no authority (`RUL-6`). The runtime MUST refuse a rendered form that
    resolves to no offer or to more than one. Runtime MUST expose the
    model-visible `execute_remedy_plan(offer_id)` control tool from the start of the run. It MUST NOT
    inject the tool after a block. `execute_remedy_plan` is a control act, not an ordinary tool
    proposal. Resolving the rendered form and admitting the control act happen in the host, before
    the engine, and append no facts. Runtime submits the resolved act as a separate `ExecuteOffer`
    event through the single engine
    boundary of `IMP-1`. A host event that mixes this control act with ordinary proposed calls MUST
    be refused before either part enters the engine. `execute_remedy_plan` is not a second engine
    method. A plan with no engine-side step (`RMD-13`) names a call the agent makes for itself. It
    creates no offer because there is nothing for the engine to execute.

  The complete ordered proposal batch enters one pure call, as `CHK-18` requires. The host may
  obtain it from a model response or a harness hook; prose and request lifecycle remain outside the
  engine event:

  ```text
  decision = engine.handle(view, ProposalBatch {
      trajectory_id, proposal_batch_id, proposals,
      provider_results, spawn_mark, offer_nonce
  })
  runtime.compare_and_append(decision.append)
  runtime.perform(decision.then)
  ```
- **[RMD-3]** When the agent selects a terminal call plan, `OfferAccepted` and its exact
  `CallApproved` record land together. The matching later proposal lands every ruling the plan
  carries and `DispatchOpened` together (`RUL-5`). A selection that stops before approval logs what happened instead: a
  denial lands the denial and affected `OfferDenied` records (`RMD-16`), and a consult with no
  answer appends no governance or offer-lifecycle event (`EXT-1`, `RMD-6`). An agent-side plan
  (`RMD-13`) is advice: its execution is an ordinary separately-checked call, logged as any call,
  with no offer record. Sanitizer progress hops that do not reach dispatch follow `RMD-19`.
- **[RMD-4]** At each stage the list MUST enumerate every sound **next** alternative, not every complete future sequence (`RMD-19`). A sanitizer is a progress hop only when its declared transition can help on the happy path and its actual engine-validated derivation strictly improves the current candidate under the application point's predicate (`SAN-3`, `RMD-18`, `BRN-12`). Authorities never form progress hops: every current requirement gap independently chooses among its competent authorities, choices group into canonical per-authority covers, and one terminal plan exists for each distinct grouped assignment that covers every gap. At a call stage, each grouped assignment combines with each sound raw or output-sanitizer release path (`RMD-17`). The engine validates the complete authority-evidence set for that plan when it prepares `CallApproval` for the final canonical call (`RUL-5`). A block carrying a `prior(k)` or cap gap offers the tool plans of `RMD-13`; the remaining gaps meet their plans at re-proposal.
- **[RMD-5]** Enumeration MUST be total at every stage. The alternative bound is enforced at load: a registry whose worst current-stage set — sanitizer progress hops, grouped terminal authority assignments multiplied by their sound release paths, and the other registered plan families — would exceed the planner cap (`CFG-20`) is refused as a configuration-shape error. Runtime truncation is FORBIDDEN. Chains do not multiply this bound because the engine persists one chosen candidate and re-plans; it never pre-enumerates sanitizer sequences (`RMD-19`).
- **[RMD-15]** The list MUST be ordered least-mandate-first, so the agent reaches the least powerful entity that can help before a stronger entity.
  - Plans compare gap by gap on assigned mandate power within each gap currency (trust ceilings by rank order, audience ceilings and waiver sets by inclusion, attention by identity). An input-sanitizer hop assigns its mandate `to` as its power for each gap it strictly improves (`SAN-3`); transitions compare with transitions by `to` inclusion. Across kinds, a hop ranks strictly below every ruling cover that addresses the same gap: it releases only derived bytes and consumes no ruling. Acceptance and output-sanitizer release paths assign no requirement-gap power and take no part in the comparison.
  - Plan A precedes Plan B when every gap assigned power in A is at most B and at least one is strictly less.
  - Plans incomparable under that order MAY appear in any relative order. Ordering is presentation only. Enumeration stays total per `RMD-4`.
- **[RMD-6]** If runtime obtains no answer from a consult, it does not resume the engine act: no offer-lifecycle event is
  appended, the offer stands, and a later `ExecuteOffer` control act MAY deliberately try it again. A denial
  appends the `Denial` and terminal `OfferDenied` records for every pending offer naming the denying
  authority for this rendered call in one batch (`RMD-16`). The denial changes the trajectory
  basis, so earlier offers become stale. Plans naming other authorities remain valid alternatives:
  the same decision re-plans the blocked call and opens fresh offers at the new basis. Therefore,
  every newly advertised alternative is executable.
- **[RMD-7]** Re-proposal rate control is host policy outside the model, like a timeout. A host refusal is not a check outcome. It MUST NOT surface as an empty plan list, which would falsify `RMD-10`. Inside the model, only a denial restricts re-proposal, through `RMD-16`.
- **[RMD-16]** A denial is sticky for exactly its rendered call. Once an authority has denied a plan for a rendered call (tool plus canonical digest), no later block of that same rendered call in the trajectory MAY offer a plan naming that authority. The denial is a recorded governance event (`LOG-3`). Therefore, the exclusion replays from the log. Plans naming other authorities remain valid alternatives and receive fresh offers after the denial changes the basis (`RMD-6`, `RMD-8`). Changed arguments change the digest and lift the exclusion. At its fork, a child starts under the denials its parent has recorded (`BRN-3`). Each branch then records its own denials independently. A merge carries no exclusion back to the parent. A consult that returns no answer — including the timeout and error cases of `EXT-1` — is not a denial and does not stick.
- **[RMD-8]** Offers are first-class durable lifecycle records, projected from the log rather than
  held in an outer-layer pending collection. APPA has no turn or clock-based offer lifetime.
  - `OfferOpened` binds one engine-derived plan to its `OfferId`, `BlockId`, trajectory,
    originating `ProposalBatchId`, canonical call digest, typed subject, and `PolicyBasis`. The
    subject is an ordinary remedy plan, a pending-cast resolution bound to its dispatch and raw
    digest, or a pending child return. Opening facts and the model-visible feedback carrying their
    IDs land in one atomic batch.
  - `PolicyBasis` has three replay-derived counters: `family_policy_version`,
    `trajectory_flow_version`, and `subject_generation`. The raw log basis — the count of accepted
    batches — is only the CAS frontier. It does not define offer lifetime. Transcript, delivery, and diagnostic records do
    not change a basis.
  - Each component changes for a specific kind of policy state:

    | basis component | changes when |
    |---|---|
    | `family_policy_version` | a release reserves effects; an outcome settles a reservation or records effects; or a shared source gets a cast result |
    | `trajectory_flow_version` | that trajectory's label, unresolved sources, or denials change; or the agent releases a call whose result can restrict the label, stay unresolved, or use a bound sanitizer |
    | `subject_generation` | the exact candidate, call approval, dispatch, or return advances, is consumed, or is abandoned |

    One decision MAY change more than one component. A call with declared `delta = {}` and no
    effects does not change the family or trajectory component for unrelated offers.
  - Runtime discussion, user requests, responses with no policy content (`IMP-1`), a proposal batch
    that only opens blocks
    and offers, clocks, and external no-answer outcomes do not advance any component. Sibling offers
    opened together share one basis. A decision that both mutates policy state and opens offers
    derives those offers against the decision's final state and stamps the post-decision basis; it
    MUST NOT relabel plans derived from an earlier sibling snapshot.
  - An offer is pending only while its recorded basis equals the current basis for its trajectory
    and subject. A mismatch makes it stale. Replay reaches the same result from the recorded basis
    and version changes. The offer cannot revive if later state happens to look equal.
    `ExecuteOffer` also performs exact live revalidation. The engine refuses an unknown, wrong-trajectory,
    stale, or terminal offer. It also refuses an offer whose call or candidate no longer matches live state. A live-state mismatch at an equal basis appends
    `OfferInvalidated`.
  - `OfferAccepted` lands atomically with the plan's `CallApproved`,
    derived-candidate, or admission facts. A later matching call moves the prepared acceptance and
    authority evidence into the `Acceptance`, `Ruling`, and `DispatchOpened` release batch (`RUL-5`).
    `OfferDenied` follows `RMD-6`. When one candidate advances,
    the other offers for that same candidate become stale. Offers for different blocked calls change
    only when their own family, trajectory, or subject basis changes. No user message, final answer, host-run end, or later proposal batch is itself
    a terminal cause.
  - `PolicyBasis` governs whether an offer may create a `CallApproval` and whether that approval may
    release its exact dispatch. An approval whose recorded basis no longer matches is stale and
    cannot revive. After `DispatchOpened` lands, the release has happened. Later basis changes do
    not revoke that open dispatch. Its exact call and result now follow the `DispatchId` lifecycle
    of `POS-7`, `LOG-2`, and `LOG-5`.

- **[RMD-19]** **Remedies form a staged candidate pipeline.** The engine never precomputes a chain. It derives every currently helpful sanitizer hop and every currently terminal acceptance/authority plan from the live candidate, event-log view, and immutable settings.
  - A candidate is typed by application point: canonical call arguments before dispatch, a confined tool result after dispatch, or a confined child return. Each successful sanitizer creates a durable derived candidate. It references its predecessor, accepted offer, sanitizer, application point, payload or value digest/body, derived label, and sanitizer lineage. A canonical-call candidate records its canonical payload, and the same batch opens offers from its re-check. It has no receiving bound or narrowing residual. A confined-value candidate pins the receiving established bound that measures its narrowing. Every successor inherits that bound and records its exact residual against it. Runtime supplies the external bytes; the engine validates and constructs the record.
  - If the derivation makes the candidate immediately admissible, its sanitizer fact and terminal dispatch or admission land atomically. Otherwise the accepted hop, derived candidate, invalidation of sibling offers on the predecessor, and the next stage's `OfferOpened` records land in one batch with their model-visible feedback. Any terminal dispatch or admission likewise invalidates every other offer on its consumed candidate in the same batch. A failed or invalid derivation appends nothing and leaves the current candidate and offer pending.
  - One sanitizer name may occur at most once in a lineage. Every hop must strictly help, and the registered set is finite, so a chain terminates. Different sanitizers may run in the order the agent chooses; their content transformations need not commute.
  - Any chosen input-sanitizer hops precede acceptance and authority review on that route. Authorities see and rule on only the final canonical call candidate. A raw terminal call plan prepares `CallApproval` with the intended acceptance and the complete evidence set from all assigned authorities. A matching later proposal releases the call and records acceptance before the rulings and dispatch. An output-sanitizer terminal plan prepares the same authority evidence and the sanitizer binding, but no raw acceptance; its exact residual is handled only after derivation (`RMD-17`, `RMD-18`). Authorities do not transform candidates, so partial authority progress is never persisted. Runtime may collect evidence in any order or concurrently; transport orchestration is the unresolved runtime contract of §13, behind the typed evidence boundary of `EXT-3`, and cannot change engine semantics.
  - Output sanitizers run only on the confined successful result after dispatch. Child returns enter at the same confined-value stage. At either point the engine may offer another applicable helpful sanitizer or acceptance of the exact current residual; it never accepts a narrowing implicitly. A stale, invalidated, or superseded candidate crosses nothing.

### 5.1 What the planner enumerates

| Gap | Enumerated remedy |
|---|---|
| Unmet trust floor | In-scope authorities whose mandate ceiling reaches the rank |
| Unmet `includes` | In-scope authorities whose mandate can cover the readers |
| Failed `no_prior(k)` | In-scope authorities whose mandate waives $k$ |
| Failed `prior(k)` | Registered tools whose `emits` include $k$ |
| Failed cap | Registered tools whose restrictive delta drops offending readers |
| Unmet attention mark | Authorities attending the mark |
| Narrowing | Raw acceptance plan (`RMD-11`). In a mixed block, each raw release plan includes acceptance plus any required rulings. An output-sanitizer release plan handles any remaining narrowing after the sanitizer runs (`RMD-17`, `RMD-18`). |
| Narrowing on a transitioned dimension | Dispatch paths bound to an applicable output sanitizer; any derived residual is handled at its confined candidate stage (`RMD-18`, `RMD-19`) |
| Consumed `unknown` | Applicable casts, attempted by runtime per `CHK-16` — never surfaced as a plan object |
| Unmet `includes` an input sanitizer can strictly improve | Staged input-sanitizer progress hop (`SAN-3`, `RMD-19`) |

- **[RMD-9]** A **nonempty** list asserts that a sound terminal plan or strictly helpful next hop exists relative to registered configuration, the typed external evidence supplied for this act, and current recorded facts. It does NOT assert that execution or any later stage succeeds. If required evidence is unavailable, the act has no normative plan-list result (`EXT-1`).
- **[RMD-10]** An **empty** list asserts that NO terminal plan or permitted first progress hop exists — evaluated at the same instant as `RMD-9` against registered configuration, supplied evidence, and recorded denials for this rendered call (`RMD-16`). Because every sanitizer chain must begin with one strictly helpful hop, the staged assertion is complete without enumerating sequences. Later evidence does not retroactively falsify it. The assertion concerns requirement gaps: an `unestablished` entry offers no plan by design, because a fact clears it (`CHK-16`). In a mixed block, the assertion reads from plan contents — no offered plan covers or progresses the gap.
- **[RMD-11]** The raw acceptance plan is ALWAYS available for a narrowing-only block, from no registry entry, because it grants nothing. A narrowing-only block is therefore never terminal. In a mixed block, each raw release plan includes acceptance plus any required rulings. An output-sanitizer release plan waits for the sanitizer result before it handles any remaining narrowing (`RMD-17`, `RMD-18`). Terminality follows the requirement gaps, and the emptiness assertion of `RMD-10` concerns those gaps only.

### 5.2 Who executes a plan

- **[RMD-12]** The agent selects every engine-side offer through the runtime-owned `execute_remedy_plan` control tool. Runtime reports that control act as a separate `ExecuteOffer` event (`IMP-1`). It MUST NOT mix that act with ordinary proposed calls in one host event. A sanitizer progress hop commits only its validated derived candidate and next-stage lifecycle batch. Immediate admissibility is the exception. For a terminal call plan, `ExecuteOffer` consumes the offer and records one exact `CallApproval` with the complete validated acceptance and authority evidence. It does not yet append an `Acceptance`, `Ruling`, effect reservation, or `DispatchOpened`. The agent must propose the exact call next; `RUL-5` governs its release. External sanitizer or authority IO MAY occur while the offer is pending. No partial engine governance state lands (`RMD-19`, `RUL-5`).
- **[RMD-13]** A plan for a failed `prior(k)` or a failed cap carries no engine-side step. It names a registered tool whose contribution clears the gap: `emits` including $k$ for a `prior(k)`, a restrictive delta dropping the offending readers for a cap. The agent dispatches that tool as an ordinary separately-checked call, then re-proposes. For a cap, the narrowing is accepted at the named tool's own block.
- **[RMD-17]** Any input-sanitizer progress chosen for a route happens before that route's terminal plan. For each live canonical-call candidate, each grouped authority assignment combines with every sound release path. A **raw** path carrying a narrowing orders acceptance first, then the complete ruling evidence set, then dispatch. Selecting that offer is the agent's informed choice; its `Acceptance` fact lands only when the matching call consumes `CallApproval` and releases (`RUL-5`, `RUL-6`). An **output-sanitizer** path instead orders the complete ruling evidence set and dispatch with that sanitizer bound; it accepts no raw or guessed residual, and `RMD-18` governs the confined result. A mixed block offers no standalone acceptance plan because acceptance clears no requirement gap. In either path every authority reviews exactly the call that dispatches, and no ruling subset lands. Acceptance changes nothing without admission of its bound contribution (`CHK-7`).
- **[RMD-18]** A block carrying a narrowing MUST additionally offer a dispatch path for each applicable **output sanitizer** whose declared transition can strictly help the eventual confined result: `on` includes `tool_output`, scope covers the called tool (`SAN-9`), the transition `from` admits its declared output label, and the deployment can withhold at that application point (`SAN-2`). Requirement gaps are still cleared before dispatch by the terminal authority assignment; an output sanitizer NEVER covers one. The chosen sanitizer binds to the dispatch without accepting an unknown residual. On success the raw result is withheld, the engine validates the first derivation, and `RMD-19` either admits it or opens the next confined sanitizer/acceptance stage. At every such later stage, another output sanitizer is helpful only when its declared derivation is globally no narrower than the current candidate and strictly reduces its residual against the pinned receiving bound in at least one dimension; applicability and the once-per-lineage rule are rechecked. A tool with a pending-cast output dimension offers none — a cast establishes its label at admission (§11.1), and the two confinement paths do not compose. A sanitizer whose declared transition cannot help MUST NOT be offered.

---

## 6. Authorities and mandates — `AUT`

- **[AUT-1]** Authorities are the single home of discretionary judgment. Every act of human or policy discretion is a ruling.
- **[AUT-2]** There are no ruling kinds at runtime. The typology lives in **mandates**, which declare what an authority's rulings MAY cover.
- **[AUT-3]** Mandate powers, each naming the currency it acts on:
  - **Cover up to a ceiling**: Admits a dispatch over an unmet trust floor up to a declared rank, or over an unmet `includes` up to a declared reader set. The label does not move. The ceiling bounds the gap one ruling MAY cover.
  - **Named waiver**: Covers a failed `no_prior` for the admitted dispatch only, naming event kinds it MAY waive.
  - **Attends**: The attention marks whose demands this authority's rulings satisfy.
- **[AUT-4]** A single ruling by an attending authority MAY cover both a label or history gap and an attention demand on the same call. One reviewer therefore means one review: forcing a second, independent pair of eyes on the same call takes a second attention mark attended by a different authority.
- **[AUT-5]** Accepting a narrowing MUST NOT be a mandate power. A deployer wanting a human on expensive narrowings attaches an attention mark to the narrowing tool.
- **[AUT-6]** An authority whose mandate covers nothing MUST produce a load error, never a no-op. The emptiness assertion of `RMD-10` depends on it.
- **[AUT-7]** Requirement gaps route by **tags, exclusively**. An authority `scope` names the tags it covers. An authority with no declared scope covers every call.
- **[AUT-8]** Attention gaps route by their own currency: a demand reaches exactly the authorities that attend its mark, and scope tags are NOT consulted.
- **[AUT-9]** Trust, audience, and effects are checked currencies and MUST NOT double as routing keys.
- **[AUT-10]** Tags CANNOT break soundness, only coverage. A mis-tagged catalog MAY route a gap to an authority that cannot help (who still cannot exceed their mandate) or route it to no one. The worst a mis-tagging produces is a block reported terminal while a competent authority sits unconsulted.

---

## 7. Rulings — `RUL`

- **[RUL-1]** A ruling admits a dispatch despite a requirement gap and MUST NOT edit the trajectory. The trajectory changes ONLY through what the admitted call commits — its `delta` and its `emits`.
- **[RUL-2]** A ruling MUST NOT substitute for agent acceptance of a narrowing, and an acceptance clears no requirement. The two gates compose independently.
- **[RUL-3]** Every ruling is **call-scoped**: it covers exactly the engine-rendered call it names
  for one dispatch. Tool arguments enter the engine as untrusted JSON bytes. Only the engine may
  construct `CanonicalArguments`: it strictly accepts one JSON object, rejects duplicate keys and
  forms outside the registered `APPA Tool Parameters v1` schema (`CFG-25`), and serializes it by the language-independent
  JSON Canonicalization Scheme (RFC 8785). The call digest is domain-separated over the tool name
  and those canonical bytes. Equal argument objects therefore bind the same call regardless of
  source key order or whitespace. Runtime MUST dispatch exactly the canonical tool name/argument
  bytes the engine released; it may not re-render them. An unknown tool, arguments that cannot
  construct schema-valid `CanonicalArguments`, and a marked spawn call whose `return_schema` does
  not compile to a canonical `ReturnShape` (`BRN-16`) return an explicit `InvalidCall` engine
  error. They
  never become a check block or remedy gap; in a proposal batch, `CHK-18` governs the atomic error.
- **[RUL-4]** A call dispatches iff every requirement gap is covered by the rulings that admit it and either the narrowing its raw admission would fold is accepted or a selected output sanitizer will confine the result under `CHK-2`. Each issuer mandate MUST cover the gaps assigned to its ruling; all rulings bind the same rendered call and are consumed together in one atomic step. A confined derived result crosses only after any exact residual acceptance required by its candidate stage.
- **[RUL-5]** **Prepare, then atomically release one exact call.** A ruling-carrying offer names one canonical grouped authority assignment. Runtime may collect its typed responses in any order or concurrently, but the stateless engine accepts only the complete evidence set bound to the final canonical call and live offer. On full approval, `OfferAccepted` and `CallApproved` land together. `CallApproved` stores the exact call, source offer, current `PolicyBasis`, intended acceptance, and complete validated authority evidence. It reserves no effects and lets no executor run.
  - The agent receives guidance to propose that exact call. A matching singleton `ProposalBatch` consumes the approval. If its recorded basis is still current, the engine appends `CallApprovalConsumed`, any required `Acceptance`, every `Ruling`, the effect reservation, and `DispatchOpened` in one batch. Only this second commit releases the call. The external tool runs later and reports its outcome under the new `DispatchId`.
  - A different released call makes the approval stale when it advances the relevant basis. This includes any explicit delta other than `delta = {}`, a missing delta whose result label is Unknown, a dynamic or pending-cast delta, and any declared effects. A call with declared `delta = {}` and no effects does not stale it. A malformed or policy-blocked proposal releases nothing and does not stale it. If required external evidence is unavailable, runtime does not resume the proposal and likewise leaves the approval current. Discussion and responses with no policy content also do nothing; an exposed provider-run admission advances the basis like a restricting release.
  - Once `DispatchOpened` lands, later basis changes do not revoke that dispatch. A denial and no-answer while preparing the approval follow `RMD-6`. A ruling cannot cover a swapped call, and the decision trail replays from the log. Transport order, retry, and late-response handling remain the unresolved runtime contract of §13; `EXT-3` fixes only the typed engine boundary.
- **[RUL-6]** An acceptance MUST name the offer it accepts. The runtime takes the trajectory that
  makes the acceptance from the harness channel that carries the control act. It MUST NOT derive
  that trajectory from the agent's argument. Possession of an offer identity proves nothing. It
  shows neither delivery into model context nor model authorship, and a host MAY execute an offer
  with no model involved. An acceptance MUST be refused when the named offer is not pending for
  that trajectory, live subject, and current `PolicyBasis`. The trajectory test is exact equality
  against the offer's pursuer, which for a blocked child return is the parent (`BRN-11`). No turn,
  clock, or in-memory round counter is needed. Ruling-carrying paths that require no
  raw acceptance — including output-sanitizer paths — are not acceptance-gated, but every assigned
  authority sees the staged review and the offer identity and lifecycle are validated identically.
- **[RUL-7]** Authorities MUST rule on the exact engine-rendered call, NEVER on the agent
  paraphrase. Concretely, an approval UI or resolver payload presents the tool name and canonical
  argument bytes exactly as the engine would dispatch them. Arbitrary model output cannot prove
  which earlier value influenced which argument location, so the engine and host MUST NOT claim an
  argument-location-to-`ValueId` mapping. Model-authored arguments are conservatively influenced by
  the whole model context, whose security summary is the current trajectory label. The agent own
  account of what it is doing — the sentence a confused agent writes under injected instructions —
  NEVER reaches the authority as the thing to approve.
- **[RUL-8]** **The staged review.** What crosses to the authority is the exact call identity and
  payload (tool, canonical arguments, canonical digest) plus its typed context: the trajectory label
  fold at review time and the gaps the ruling would cover. The typed context MUST be persisted
  verbatim on the resulting ruling; the dispatched call persists the payload once. No per-value or
  per-argument provenance claim is part of a ruling.
- **[RUL-9]** The staged review carries the rendered call argument payload: an authority judging `send_email(text, recipient)` sees the text it is asked to release. Authorities sit in the deployer trusted base per `THR-3`. The review crossing to its authority is disclosure to a trusted judge, not a flow the algebra checks. A deployer unwilling to show an authority the bytes it judges SHOULD NOT register that authority over those calls. The recipients of a proposed release cross typed as the `includes` requirement gap of `RUL-8` in any case. The payload is persisted once: the ruling binds the engine-owned `CanonicalArguments` by digest, the dispatched call in the log carries those exact bytes, and the ruling persisted context stays the typed context of `RUL-8`.
- **[RUL-10]** No grant object appears in configuration or on any wire. The public vocabulary is mandates, rulings, and log records.

---

## 8. Sanitizers and casts — `SAN`

- **[SAN-1]** A **sanitizer** is a registered transformer deriving a new value under the label its mandate authorizes. The raw source keeps its own label.
- **[SAN-2]** At **tool output**, the derivation is admitted by the context that would otherwise receive the raw value, and the raw value is withheld from it. That takes a host able to withhold, and there are two such points:
  - At the **child-return crossing** the raw stays behind in the child and the derivation crosses to the parent; this needs only context control (`POS-5`), because the raw was never in the parent context to withhold.
  - At the **tool result** the host takes the body the tool returned, admits the derivation, and never shows the raw to the model that asked for it; this needs confinement (`POS-2`), the capability to keep a result out of model context.
  - A deployment that does not cover confinement at that point offers no plan there (`POS-3`: coverage decides which remedies exist, never which flows pass). A failed derivation admits nothing (`LBL-11`), and the call effects stand (`LOG-2`).
- **[SAN-3]** At **tool input**, a sanitizer derives a replacement for the **whole argument set** of one rendered call. There is no per-argument application.
  - The raw arguments are model-authored, so the raw contribution carries the current label, and the mandate `from` MUST cover it before the `to` applies (`SAN-4`). The sanitizer receives the engine's canonical raw argument bytes and returns one complete replacement JSON object — never a patch, partial merge, or tool-name replacement. Its bytes are untrusted input. The engine strictly parses and schema-validates the object, constructs new `CanonicalArguments`, and substitutes that value into the rendered call. The harness dispatches exactly the substituted canonical bytes, and dynamic placeholders (`CHK-9`) read them.
  - At the check, the derivation `to` stands in for the argument contribution in the **`includes` check only** — the one requirement that guards bytes reaching recipients. Every other requirement keeps reading the committed label (`CHK-4`): a cap bounds the run's own reach, not one call's bytes, and a trust floor guards the decision to call, which is steered by what the trajectory ingested and is not rewritten with the bytes.
  - A `tool_input` sanitizer therefore mandates the audience dimension; a trust mandate on `tool_input` has no application, and the loader MUST refuse it. From the live argument candidate, the engine offers a progress hop when registered scope and mandate show that the sanitizer can strictly improve an `includes` gap on the happy path; it need not clear every gap itself. An offer asserts capability, not a successful external answer (`RMD-9`). Runtime invokes the sanitizer and resolves any dynamic fields when the offer is executed. It supplies the complete replacement bytes and pinned external answers as evidence; the engine validates their identity, ceilings, and binding to those replacement arguments, then re-runs the check. The replacement is helpful only when the missing-audience value of every indexed `includes` requirement is no greater under audience inclusion and at least one is strictly smaller; a changed placeholder that introduces or enlarges a gap therefore fails the hop. Runtime resolves and orchestrates; engine suggests and validates.
  - A valid strictly helpful replacement commits as the next durable canonical-call candidate and invalidates sibling offers on its predecessor (`RMD-19`). The engine then either dispatches it if immediately admissible or enumerates the next sanitizer hops and terminal acceptance/authority plans. Thus sanitizer chaining is staged rather than precomputed, and every authority reviews only the final substituted canonical call (`RUL-3`). A malformed, non-object, schema-invalid, unresolved, non-helpful, or otherwise invalid derivation produces no sanitizer governance event and no dispatch; the offer remains pending for a later deliberate retry. The trajectory label is untouched. This protects the sink and CANNOT un-leak context.
- **[SAN-4]** A sanitizer mandate binds the one transition it MAY claim, declared on one dimension as a `from` and a `to`. The raw value MUST satisfy the transition `from` before the `to` applies. The `to` is declared at registration: a sanitizer does not decide its derivation label per value, as a resolver-implemented cast does under `SAN-8`, so the declared `to` is the transition ceiling. A group written in a mandate resolves at application (`LBL-14`): the declaration is fixed, and the directory owns what its name means. Trust and audience are bound on the same terms. The undeclared dimension is untouched: the derivation carries the raw value label on it unchanged.
- **[SAN-5]** A mandate binds a transition, not the information over which it is claimed. Scope narrows where a component is offered (`SAN-9`); it never changes what a mandate claims.
- **[SAN-6]** Registering a sanitizer vouches for its implementation. Registration verifies nothing about outputs. Implementations are `builtin` or `resolver` per `CFG-15`. The builtins to expect are basic ones: a scrubber that drops API keys and tokens from a fetched body, or an email address redactor — and the reserved `attest-schema` of the quarantine exit (`BRN-16`). Registering either kind is the vouching act of `SAN-6`.
- **[SAN-7]** A **cast** resolves one immutable source value, not one dimension in isolation. It is either **constant** or **resolver-implemented**, NEVER both. Its candidate is one complete label. Every dimension already established on the source MUST match exactly, and every unresolved dimension MUST become concrete. The engine admits the complete source resolution in one `CastApplied` fact or admits nothing; partial per-dimension knowledge is never logged. A consumer of either dimension triggers this whole-source attempt, while unused unresolved values may remain unresolved indefinitely (`UNK-4`).
- **[SAN-8]** A resolver-implemented cast MUST declare the complete product ceiling to which it MAY cast: allowed trust ranks and an audience `cap` (`CFG-8`). The ceiling keeps a sloppy or compromised classifier from becoming a laundering endpoint. The engine checks each formerly unresolved dimension against its ceiling and checks each already-established dimension for exact equality. A restricted audience answer contains literal reader IDs: no group. A `public` resolution is admitted ONLY under a `public` cap. That cap is an open gate the deployment chose (`THR-3`): one covered answer lifts the audience restriction entirely. A constant cast similarly declares one complete label and is applicable only where it agrees with every established source dimension.
- **[SAN-9]** A cast or a sanitizer MAY declare a scope by tags, as an authority does (`AUT-7`). It applies to values whose originating tool contract carries a covered tag: the called tool for a result or a cast, the callee for a `tool_input` substitution (`SAN-3`). A child return originates from no tool, so only unscoped components apply there: unscoped sanitizers are offered (`BRN-12`), unscoped casts attempted when a return operation consumes an unresolved dimension (`CHK-16`), and a policy-bound `return_sanitizer` is a direct binding, not an offer (`BRN-15`). A component with no declared scope applies to every value. Among applicable whole-source casts, registration order decides and the first complete valid answer stands (`CHK-16`). A mis-scoped cast or authority costs coverage, never soundness: answers stay within `may_cast` (`SAN-8`) and mandates (`AUT-10`). A sanitizer scope is part of the vouch of `SAN-6`: a scope wider than what the implementation cleans extends the vouch to content it cannot clean, and the deployment owns that (`THR-3`).

---

## 9. Effects and history — `LOG`

- **[LOG-1]** Every engine fact lives in one append-only family log. Outer request, transcript, delivery, and diagnostic traces are not engine facts (`IMP-4`). Engine-log appends MUST NOT be gated by any policy check.
- **[LOG-2]** **Effects** are declared by contracts as `emits`. For an engine-released dispatch they append when runtime reports the typed tool outcome `Success` — the dispatch effect append point. `Success` is independent of result availability: `Available(body)`, `Unavailable`, and `Rejected` all commit the same declared effects, while only an available usable value can proceed to admission. The engine emits the success/effect checkpoint before runtime awaits any output sanitizer or cast. That checkpoint records the identity of the observed result: the digest of an available body, or that no body was available. A later report of the same dispatch MUST carry the same observation, and evidence derived from that body MUST name the recorded digest (`EXT-3`). With no such external step, success close, effects, and value admission MAY share one engine batch. `Failure` commits no effects. `Indeterminate` commits no effects and leaves the reservation standing because execution may have happened (`CHK-17`). For a provider-run tool, admission of an exposed result is the separate best-effort observation append point: the engine records the contract's declared effects atomically with that value before sibling checks. It does not create or imply a dispatch lifecycle, and no exposed result means no recorded effect (`POS-7`). Mapping an executor's HTTP status, stream, exit, or callback into typed outcomes, and mapping provider response parts into exposed results, are runtime-adapter contracts rather than engine semantics.
- **[LOG-3]** **Governance events** are authority rulings and denials, the dispatches that consume rulings, acceptances, sanitizer applications, casts, and branch boundary events.
- **[LOG-4]** A **boundary event** is branch punctuation the engine appends when a branch ends: at the merge that consumes a child return, and at a void return. It never gates a flow itself. A fork appends no boundary event; fork identity and the records it binds let replay and audit distinguish branch structure without any turn boundary.
- **[LOG-5]** The log is consulted in exactly five ways: history requirements, ruling validity, **lifecycle validity** (whether a dispatch is still open, whether a child has already returned, whether an offer is pending, and whether a `CallApproval` is still current on its `PolicyBasis`), **denial exclusion** at plan enumeration (`RMD-16`), and audit. Every other read is a projection rather than a consultation: labels and policy-basis versions are views of the engine log per `IMP-2`, the log's own state in another shape. Lifecycle state is recomputable from the log exactly as a view is; it sits on the consultation side because its reads refuse admissions, and a projection never gates anything.
- **[LOG-6]** Every history check is **kind-containment only**. `prior(k)` and `no_prior(k)` ask whether a matching effect exists, NEVER how many or how large.
- **[LOG-7]** The engine MUST NOT keep a magnitude view or feed one to any authority. Counting and summing are outside the model.
- **[LOG-8]** Summaries such as the set of effect kinds seen so far are views computed from the log and cached. They MUST NOT become independent state.
- **[LOG-9]** The runtime MUST serialize appends across concurrent branches. At the engine boundary, one engine-produced `ValidatedFactBatch` is compared against the log position its decision was computed on and is either appended in full or not accepted; no partial batch may become visible. A history check is only as sound as the log it has seen. Persistence strength, acknowledgement, and recovery after a storage error belong to the runtime contract and are not engine protocols.
- **[LOG-10]** The effect vocabulary is deployment configuration. A deployment MAY encode a bespoke gating ritual as an effect plus a dynamic authority. An authority whose decision needs an accumulated magnitude keeps that account in its own systems.
- **[LOG-11]** **APPA has trajectories and proposal batches, not turns.** A `ProposalBatchId`
  identifies one complete canonical policy-content payload for one trajectory: the ordered
  proposals, each exposed provider-run result, and the optional spawn mark (`IMP-1`). Repeating
  the ID with the same canonical payload returns its recorded decision; reusing it with different
  bytes in any of these parts or another trajectory is an identity conflict and MUST be refused. Host requests, user
  messages, final answers, runs, provider attempts, delivery retries, cancellation, clocks, and
  retention do not create or close an engine epoch.
  - A validated engine decision records every `PolicyBasis` version advance explicitly so replay
    does not infer atomic decision boundaries from flattened facts. Each family or trajectory
    component advances at most once in one decision, exactly when the corresponding policy
    projection of `RMD-8` changes. New offers and call approvals in that decision bind the post-decision basis.
  - A response with no policy content — no ordinary proposal, no exposed provider-run result, and
    no spawn mark — creates no engine event. A provider-bearing response with no proposed call is
    an admission-only proposal batch; its admissions fold and advance the affected basis like any
    restricting admission. A block-only proposal batch may open
    offers without advancing their policy basis. Releasing a call advances the affected basis
    immediately if its result can restrict the label, stay unresolved, use a bound sanitizer, or
    reserve effects. A call with declared `delta = {}` and no effects does not change unrelated
    offers. The raw CAS revision still serializes both kinds under `LOG-9`.
  - A `ToolOutcome` remains attributable solely through its open `DispatchId`, and a child return
    through its fork identity. A provider-run admission is attributable through its admitted value
    and originating tool (`POS-7`); no dispatch exists for it. Later host activity neither cancels
    nor reassigns any of these lifecycles.

---

## 10. Branching — `BRN`

- **[BRN-1]** The core trajectory is linear. Branching is a host capability requiring `POS-5`, because the snapshot of `BRN-4` and the single return channel are both bounds on child context. A host that branches MUST implement the rules of this section in full, or MUST NOT let branch results cross back. Quarantined branches (§10.1) additionally require confinement (`POS-2`), since they withhold bytes.
- **[BRN-2]** **Fork preparation.** A context-controlled spawn release atomically opens its dispatch and appends `ForkPrepared`. Runtime names the spawn: the proposal batch marks at most one proposed call as the deployment's context-controlled spawn, and the engine refuses the mark when the deployment does not declare context control (`POS-5`). The marked call is checked and released like any tool call, under an ordinary registered contract, with one exclusion: its block offers no `tool_input` substitution (`SAN-3`), because a substituted release prepares no fork and the child could never bind (`BRN-3`). The engine derives `ForkId` deterministically from that unique `DispatchId`; runtime does not mint it. `ForkPrepared` binds the parent trajectory and exact canonical spawn call and freezes the source values that contributed to the parent's current label: the parent's inherited source set plus every parent value admitted before release. It carries the fully established base label, frozen inherited `ValueId` set, derived seed partial label, optional canonical `ReturnShape`, and return policy. This is a content snapshot, not a copy of the values. Later parent values do not enter the child, while a later fact about an inherited immutable source — such as a cast resolution — is shared knowledge and applies wherever that source contributes (`BRN-14`, `UNK-8`). An `unknown` parent label is therefore not a reason to refuse preparation.
- **[BRN-3]** **Fork binding.** When the host later knows its child trajectory identity, runtime submits `BindFork { fork_id, child_trajectory_id }`. The engine accepts it only for one live prepared fork and an unused, non-parent child ID, then appends `ForkOpened` in the same family log. That record binds the child to `ForkPrepared`; it does not recompute the snapshot. The append MUST succeed before any child engine event. Repeating the same fork/child binding is idempotent; naming another child for the fork or reusing the child for another fork is refused, and concurrent binders serialize through `LOG-9`. A recorded spawn `Failure` before binding makes the preparation unbindable. A runtime outage or missing start signal creates no engine event and leaves preparation retryable. The child starts from the prepared parent label, NEVER from the neutral starting label, and every return addresses the same `ForkId`. Nested preparations flatten the visible inherited set plus their parent's local pre-fork values, so replay does not infer ancestry from mutable runtime state.
- **[BRN-4]** The model-visible handoff is bounded by a snapshot: at most every completed ancestor message through the fork plus its child task. The host MAY hand the child less, down to the child task alone. The child receives neither an ancestor incomplete tool-call exchange, later ancestor activity, nor sibling activity. Nested children are bounded by the corresponding completed prefix from each ancestor. `submit_result` is the ONLY channel carrying child-derived data back, and the host declares its realization: a model-visible tool, or the branch-terminal message — the child's final message is then the submission, and a terminal message that carries no value is the void return (`BRN-9`). A host MUST NOT open another return channel: the snapshot bounds what enters a child, the single return bounds what leaves it, and without both the merge rules govern nothing.
- **[BRN-5]** **Merge.** The returned value label folds into the parent like any other read. Nothing the child did CAN widen the parent, since intersection cannot add readers.
- **[BRN-6]** History needs no merging. There is one shared log to which every branch appends in real time. Therefore, a child `egress` fails a parent `no_prior(egress)`. An in-flight child dispatch is visible the same way: its reservation fails a parent `no_prior` before the effect appends (`CHK-17`). There is no branch-scoped `prior(k)`.
- **[BRN-7]** A ruling issued in a branch is a record and not a token. It was consumed when the engine released its exact dispatch. Its presence in the shared log gives the parent nothing to reuse.
- **[BRN-8]** A child submits **at most once**. `submit_result` — with a value or void — ends the branch. A later `submit_result` MUST be refused. A value submission that may need later parent action transfers custody in a durable child-attributed `ReturnSubmitted` record: return identity, `ForkId`, child and parent trajectory, child-fold label, raw digest and body, and bound return policy. The shared log stores the bytes, but the parent label projection does not expose them. The raw payload is persisted once; later offers and crossings reference the return identity. If a mandatory sanitizer is inapplicable, the engine records only the digest and typed terminal reason, not a body no future operation may consume.
- **[BRN-9]** A **void return** — `submit_result` with no value — ends the branch and crosses no value. It contributes nothing to the parent label. The branch effects and governance events are in the shared log as any branch effects are. What a void return withholds is a label contribution, not a trace.
- **[BRN-10]** Finalization is trivial for every started branch whatever its fate. A branch that died, returned void, or whose return candidate never crossed contributed no value, and nothing needs recovery.
- **[BRN-11]** A candidate return that would **narrow** the parent MUST soft-block at merge exactly like a narrowing call, carrying **return plans**. A raw candidate that narrows nothing merges without a block; a successful mandatory-sanitizer derivation follows the specialized candidate and plan rules of `BRN-15`. The branch itself has ended (`BRN-8`). `OfferOpened` and its safe model-visible feedback belong to the parent trajectory and exact `ForkId`; the child never receives or executes the menu. The parent model may receive and execute it in any later proposal batch while its `PolicyBasis` remains current (`RMD-8`).
- **[BRN-12]** At each confined return-candidate stage, plans are: acceptance of exactly the current parent-to-candidate narrowing, always; and one progress hop for every **applicable and helpful** registered output sanitizer not already used in that candidate's lineage. Applicability requires the candidate label to satisfy the mandate `from` and every sanitizer-specific precondition to hold. Helpfulness is globally monotone: after merging into the parent, the declared derivation MUST be no narrower than the current candidate overall and strictly less narrow in at least one dimension. Equal, worse, incomparable, and no-op derivations are not remedies and MUST NOT be offered. A successful hop persists only the derived candidate and re-plans from its smaller residual; it is never pre-bundled with acceptance. `attest-schema` uses these same predicates; its fork-bound schema checks are additional applicability preconditions, not privileged fallback behavior. An offer asserts that the registered mandate can help on the happy path, never that external execution will succeed (`RMD-9`, `RMD-19`).
- **[BRN-13]** A narrowing on a dimension that no applicable sanitizer mandate transitions crosses ONLY by acceptance.
- **[BRN-14]** A branch MAY resolve a locally admitted value, an ancestor value in its frozen inherited source set, or an unresolved source identity that a merge carried into its partial label. It MUST NOT target any other value: a sibling-only or post-fork value stays out of reach until a merge carries its identity in. `CastApplied` records both the acting trajectory and the immutable source `ValueId`, but the resolution belongs to the source rather than the actor. Parent, child, and siblings whose snapshots contain that source all reuse the same resolution. A raw merge itself does not force cast IO: unresolved source identities cross into the parent's partial label and retain every established restriction. Resolution runs only where the return policy or a later operation consumes the dimension (`UNK-4`). The narrowing comparison of `BRN-11` reads established bounds (`UNK-15`).
- **[BRN-15]** A policy-bound `return_sanitizer` is mandatory and is not part of the plan choice. The binding names its sanitizer directly: scope governs where a component is offered (`SAN-5`), and a binding is not an offer. On submission the engine applies the same **applicability** predicate as `BRN-12` before external IO; helpfulness gates optional offers, not a mandatory binding. If the child fold does not satisfy `from` or a sanitizer-specific precondition fails, the engine does not invoke the sanitizer or fall back to raw. It ends the child with a typed `ReturnRejected` reason, records the raw digest without retaining the body, admits nothing to the parent, and surfaces fixed safe feedback for the parent fork call.
  - Where applicable, the ended child's `ReturnSubmitted` record owns the confined raw body while runtime resolves the mandatory sanitizer. External failure crosses nothing and leaves that durable raw handoff available for the unresolved runtime retry contract of §13; `EXT-3` fixes how any eventual answer binds back into the engine.
  - The first valid engine-checked derivation ends that retry path and becomes the only candidate return; raw fallback is forbidden. A durable `ReturnDerived` record references the submission and mandatory sanitizer, owns the derived value, label, and digest, and starts its sanitizer lineage. If its merge narrows nothing, `ReturnDerived` and admission land atomically. If it still narrows the parent, `ReturnDerived` owns the confined body and enters the staged pipeline of `BRN-12` and `RMD-19`: the parent may accept exactly the current residual or choose another applicable helpful sanitizer not already in the lineage. Each successful hop replaces the current candidate and re-plans; a stale, invalidated, or superseded candidate crosses nothing.

  The handoff remains a pure engine transition over the runtime-owned family log:

  ```text
  decision = engine.handle(view, ChildReturn {
      fork_id, child, raw_body, offer_nonce
  })
  runtime.compare_and_append(decision.append)
  runtime.perform(decision.then)
  ```

  Conformance MUST cover at least: an immediate non-narrowing merge; a narrowing return whose offers
  and feedback remain bound to the parent trajectory and `ForkId`; restart before acceptance;
  delayed submission and execution in a later host run; state-change staleness; raw acceptance and
  staged sanitizer/acceptance execution by the parent; child re-drive and second-submit refusal; every
  `from`/help/precondition exclusion including `attest-schema`; mandatory-sanitizer rejection with no
  external call or raw fallback; and external failure leaving an applicable durable handoff confined.

### 10.1 Structured quarantined branches

A child handles suspicious content and returns through `submit_result` with a structured output bound at fork. The exit MAY raise trust on the return — the dual-LLM pattern: the parent decided what to do with a typed answer before any suspicious byte was read.

- **[BRN-17]** Schema validation alone MUST NOT raise a label: structure is not provenance. The raise is claimed by a mandated `tool_output` sanitizer at the child-return crossing (`SAN-2`), ONLY within its mandate, in one of two grades: a **content-inspecting** sanitizer vouches the returned bytes it examined, and the reserved builtin `attest-schema` vouches the channel shape (`BRN-16`).
- **[BRN-16]** `attest-schema` derives the return unchanged: it withholds nothing, and what it adds is the claimed transition. Its mandate MUST bind trust; an `attest-schema` declaration carrying an audience mandate is a load error. It claims the transition from three preconditions checked at application:
  1. First, every field of the returned structure is **shape-bounded**, recursively: a number within a declared range and precision, a boolean, a closed enum of declared literals, a bounded format from a declared closed list, or an array or object of shape-bounded fields under a declared length bound — free text is never shape-bounded.
  2. Second, the structure was bound at fork, before the child read anything; the fork record carries its canonical form, so the precondition replays from the log (`BRN-3`, `IMP-1`).
  3. Third, the parent's fork-time trust rank covers the mandate `to` — the answer cannot come back cleaner than the context that asked.
  - The parent agent authors the optional `return_schema` in the same canonical `fork` call as the child task. Runtime transports it without interpreting it. The engine accepts a strict, closed subset of JSON Schema and compiles it to its own canonical `ReturnShape`. Supported leaves are booleans, bounded numbers, closed literal enums, and formats from an engine-declared closed list; bounded arrays and closed objects compose them. Free strings, open objects, unbounded collections or numbers, references, combinators, unknown keywords, and unsupported formats are refused. The refusal surface is `RUL-3`: a marked spawn call whose `return_schema` does not compile is an invalid call, and no fork is prepared. Immutable engine settings bound nesting, fields, enum members, array length, numeric precision, and canonical bytes.
  - The fork record stores the full normalized `ReturnShape`, not a caller claim that validation happened. Runtime mechanically renders the child-specific `submit_result` tool from that stored shape. A non-void return is strictly parsed, canonicalized, and validated by the engine against the same shape before any return plan or crossing is produced. A submission that fails this validation is a typed refusal: the engine appends no fact, the branch does not end, and the return channel stays open for a corrected submission. Under a `[child]` `attest-schema` binding, the same mismatch is the inapplicable-mandatory-sanitizer rejection of `BRN-15`. A fork with no `return_schema` is an ordinary unstructured branch and `attest-schema` is inapplicable.
  - The preconditions are engine-held facts, so no resolver variant exists, and shape CANNOT be validated at policy load: the agent chooses it at fork, and the engine rechecks the returned value at application, like resolver re-validation (`SAN-8`). The sanitizer's trust-only mandate is validated at load and its `from`, `to`, parent-ceiling, applicability, and helpfulness are validated through the ordinary rules at planning/application. As a return plan, the offer exists ONLY where the preconditions hold (`BRN-12`). Under a `[child]` binding, a failed precondition is the same inapplicable mandatory-sanitizer rejection as any other `BRN-15` failure; no unraised fallback derivation crosses.
- **[BRN-18]** Both grades claim **instruction-cleanliness only**: the exit bounds the adversary to selecting among the structure's declared values — it removes free-text carriage, never the value channel, and the adversary who wrote what the child read chose the values that came back. The channel's capacity is the schema's declared capacity, per return; repeated forks accumulate it, as any values accumulate. The claim never covers a value's honesty. A sink whose stakes ride on the returned value keeps its own gates: an `attention` mark, an authority, or a trust floor no exit mandate reaches — the `trust_chain` is deployment vocabulary, and a deployment that must separate vouched-clean from vetted-at-source inserts a rank between them.

---

## 11. Unknown — `UNK`

- **[UNK-1]** Both dimensions support **`unknown`**, meaning "this source contribution has not been established yet". `unknown` is not a rank: `trusted < unknown < suspicious` does NOT exist. A run dimension is unresolved while its partial-label projection has any unresolved sources, even though it still retains an established bound from the known contributions.
- **[UNK-2]** Unresolved source identity is absorbing under the fold by set union; known restrictions are not. The partial-label monoid combines `(established_bound, unresolved_sources)` as `(meet(bounds), union(sources))`. One atomic whole-source cast removes that `ValueId` from every unresolved dimension it occupied and folds its complete validated label into the established bound. No inverse label operation is needed because replay retains the source-indexed contributions.
- **[UNK-3]** A requirement that **consumes** an `unknown` dimension MUST NOT pass. Resolution is requested first per `CHK-16`. When no applicable cast exists, the source is reported by value in the block `unestablished` slot, NEVER as a blanket `unknown` result or a bare failure verdict. When applicable casts exist but runtime obtains no evidence, the act does not complete (`EXT-1`).
- **[UNK-4]** A call whose requirements consume no unresolved dimension proceeds. Runtime resolves lazily: an ordinary fold does not trigger cast IO. Resolution is required when a requirement, sanitizer applicability/mandate check, or an explicitly pending-cast admission consumes the dimension. An unresolved run fails closed at the consumers that care and nowhere else.
- **[UNK-5]** A tool listed with **no `delta` key at all** is unannotated: its results are admitted as `unknown` in both dimensions.
- **[UNK-6]** Within a *declared* delta, an omitted dimension contributes the fold identity. `delta = {}` is the explicit "this result carries nothing" annotation.
- **[UNK-7]** An unannotated tool MUST NOT declare label requirements, and the combination MUST be refused at load. Its unestablished contribution evaluates as identity at check time (`UNK-15`), so its own consequence could outrun its requirement. History and attention requirements compose fine.
- **[UNK-8]** A registered cast fills every unresolved dimension of its target source atomically, per `SAN-7`. Its mapping is a context-independent fact about one immutable source value. The first complete admitted label for that source stands family-wide, and every trajectory whose fixed source set contains the value folds that resolution. Identical bytes admitted as another `ValueId` are another source and do not inherit it.
- **[UNK-15]** An unresolved source is identity for the established bound at the narrowing check, while its identity remains in the unresolved set. `CHK-2` therefore sees every known restriction even before all sources resolve. An unannotated result adds unresolved sources without forcing acceptance or cast IO; the cost lands at the consuming operations (`UNK-3`, `UNK-4`). A pending-cast resolution that narrows owes acceptance per `UNK-9`.

### 11.1 Pending-cast admission

- **[UNK-9]** A resolution is measured against the receiving bound its dispatch pinned (`UNK-16`), never against the live fold. A resolution that narrows nothing against that bound admits directly. A resolution that narrows it MUST NOT admit on its own: it soft-blocks, like a call delta at check (`CHK-2`) and a raw return at merge (`BRN-11`). The offer names the full label change admission would cause, established dimensions included, because the live label MAY have moved since dispatch.
- **[UNK-10]** While the offer is current, the raw result stays confined. On acceptance, the cast record, the acceptance, and the admitted value land in one atomic commit. If its `PolicyBasis` changes first, the offer becomes stale and crosses nothing (`RMD-8`). The successful dispatch effects stand. The durable candidate may be planned again against the new basis, but the stale offer and any acceptance bound to it can never be reused. If runtime obtains no valid cast evidence, it does not resume the admission act: no exhaustion or `PendingCastUnresolved` fact exists, and the durable confined candidate stays pending and retryable.
- **[UNK-11]** The call effects MUST append the moment success is observed, independent of the offer fate. A later history check then sees them while the result stays confined. Acceptance is owed for narrowings, not for casts.

- **[UNK-16]** **Released result labels are pinned contributions.** A raw result pins its contribution and receiving established bound at dispatch. An output-sanitizer path pins that receiving bound and the first declared derivation contribution at dispatch; every later sanitizer candidate inherits the same bound and pins its own validated contribution when created (`RMD-19`). A raw narrowing already required dispatch-bound acceptance. A confined derived narrowing requires acceptance of its exact candidate residual before admission, but never before the raw-crossing-safe dispatch. If a pinned contribution did not narrow its receiving bound, monotonicity of the established-label meet proves later restrictive folds cannot make it newly narrowing; if it did, its pinned acceptance remains sufficient and later folds can only reduce the unaccepted difference. Admission therefore never asks for a race-dependent second acceptance and never discards a valid result merely because the fold moved. The result remains attributed to its original `DispatchId`; unrelated host runs do not reassign it. Explicitly cancelled or closed dispatches remain closed, and pending-cast results follow `UNK-9`, `UNK-10`, and the state-basis rule of `RMD-8`.

---

## 12. Configuration surface — `CFG`

A draft dialect, but authoritative: every configuration example in these documents is written in it.

```toml
version = 1

trust_chain = ["suspicious", "trusted"]   # LBL-2; omitted, this default stands

[deployment]                 # POS-10; omitted, the no-coverage default stands
starting_label = { trust = "trusted", audience = "public" }   # LBL-8; both dimensions established
binding = "harness"          # POS-8: "token" or "harness"
context_control = true       # POS-5
dispatch = "enforced"        # POS-7 executor-class default; exceptions below
assumed_tools = []
provider_run_tools = []
confined_results = ["scan_inbox"]   # POS-2: result points the host can withhold
confined_child_return = true
provider_surfaces = {}       # each entry "mediated" or "open"; unlisted surfaces are refused

[limits]
planner_cap = 64             # CFG-20; load refuses a registry whose worst case exceeds it

[[tool]]
name  = "fetch_ticket"
tags  = ["finance"]
delta = { trust = "suspicious", audience = { exactly = ["finance"] } }

[[tool]]
name  = "scan_inbox"
tags  = ["finance"]             # inside content-classifier's scope (SAN-9)
delta = { trust = "unknown" }   # pending-cast; "unknown" is reserved, never a rank
                                # name, and at most one dimension

[child]
return_sanitizer = "pii-redactor"

[membership]                 # one per deployment; every @group resolves here.
name = "corp-directory"      # Registration only, as below: the deployment binds it

[[dynamic_resolver]]         # registration only; the deployment binds the endpoint
name = "crm-acl"

[[dynamic_resolver]]
name = "channel-members"

[[tool]]
name = "lookup_customer"
parameters = { type = "object", properties = { customer_id = { type = "string" } }, required = ["customer_id"] }
delta = { audience = { resolver = "crm-acl", argument = "customer_id" } }

[[tool]]
name = "send_message"
parameters = { type = "object", properties = { channel = { type = "string" } }, required = ["channel"] }
requires = { audience = { includes = { resolver = "channel-members", argument = "channel" } } }
delta = {}

[[tool]]
name     = "send_report"
tags     = ["finance", "external-comms"]
requires = { trust = "trusted",
             audience  = { includes = ["finance"] },
             effects   = { has = ["backup.completed"],     # prior(k)
                           has_no = ["email.sent"] },      # no_prior(k)
             attention = ["finance-signoff"] }
delta    = { trust = "trusted", audience = { exactly = ["finance"] } }
effects  = ["email.sent", "finance.spend"]                 # emits

[[tool]]
name     = "post_note"
tags     = ["finance"]
requires = { audience = { cap = ["finance", "@auditors"] } }  # audience ⊆ the cap;
                             # @auditors is a group, resolved at read (LBL-14)
delta    = { }               # "carries nothing" (UNK-6); no delta key at all
                             # would be unannotated and refused with requires (UNK-7)

[[authority]]
name = "finance-officer"
hint = "The desk that signs off spend. Consult it to release a payment."   # advisory

[authority.mandate]
can_cover_trust_to = "trusted"                # cover ceiling, trust
can_cover_readers  = { may_add = ["public"] } # cover ceiling, audience
can_waive          = ["email.sent"]           # named waiver
attends            = ["finance-signoff"]      # attention marks

[authority.scope]
tags = ["finance"]           # jurisdiction; omitted scope = every call

[[sanitizer]]
name = "pii-redactor"
on   = ["tool_output"]
hint = "Removes personal details. Use it to publish a finance record."     # advisory

[sanitizer.mandate]                       # one transition, keyed by dimension
audience = { from = { includes = ["finance"] }, to = { exactly = ["public"] } }
# trust  = { from = "suspicious", to = "trusted" }   # same terms, other dimension

[[cast]]                     # constant XOR resolver, never both
name     = "content-classifier"
resolver = { may_cast = { trust = ["suspicious"],
                          audience = { cap = ["public"] } } }   # complete ceiling;
                             # the ceiling is policy, the endpoint is deployment

[cast.scope]
tags = ["finance"]           # SAN-9; omitted scope = every value

[[cast]]
name     = "paranoid-default"        # unscoped constant fallback; registered
constant = { trust = "suspicious",
             audience = { exactly = ["public"] } }  # complete label; last, so it shadows nothing (CFG-19)
```

The policy above registers components. A **deployment file** carries it and binds
each implementation (`CFG-15`):

```toml
[policy]
include = ["batteries/claude-code.toml", "./local-policy.toml"]
# The deployment MAY instead write version and declarations here.

[externals]
timeout_ms        = 5000     # one bound on every machine consult
review_timeout_ms = 600000   # a person reads the staged review and thinks (RMD-6)
max_body_bytes    = 65536

[externals.authorities.finance-officer]
url = "https://approver.corp/rule"
# token_env = "APPA_APPROVER_TOKEN"  # names the variable; the file never holds the value
# builtin = "hitl"                   # same authority, human elicitation
# builtin = "approve"                # in-process auto-approval

[externals.sanitizers.pii-redactor]
builtin = "redact-email"

[externals.dynamic]          # one implementation serves every registered resolver
command = ["node", "./resolvers.mjs"] # managed versioned NDJSON (EXT-2)
# url = "https://acl.corp/readers"     # alternative versioned HTTP POST

[externals.membership]       # the one directory every @group resolves through (EXT-4)
url = "https://directory.corp/members"
```

Whatever surface ships MUST keep:

- **[CFG-6]** **Mandates only**, per `RUL-10`.
- **[CFG-7]** **The no-empty-mandate rule** of `AUT-6`.
- **[CFG-8]** **Explicit set relations.** Every audience mention carries its operator — `includes`, `exactly`, `cap`, `may_add` — because a bare list is ambiguous between narrow and wide. A list without its operator is a load error.
- **[CFG-9]** **Scope routed by tags only**, per `AUT-7` and `SAN-9`.
- **[CFG-10]** **Casts declared constant xor resolver-implemented**, per `SAN-7`.
- **[CFG-11]** **Surface names map onto model terms.** `effects = [...]` declares `emits`; `effects.has` and `effects.has_no` are `prior(k)` and `no_prior(k)`; `attention` marks are the per-call demands of `CHK-13`.
- **[CFG-12]** **Mandate powers name the currency they act on** and nothing else. Tool and authority NEVER name each other.
- **[CFG-13]** Block messages MUST surface applicable remedy plans, naming eligible authorities where a plan carries a ruling.
- **[CFG-15]** **Implementations are `builtin` or `resolver`**, a closed set: in-process, or dynamic behind a registered endpoint. **The policy registers a component; the deployment binds its implementation in `[externals]`.** A binding written inside the policy is a load error. The policy states what a component MAY do; the deployment states who performs it. A sanitizer declares where it MAY apply (`on`) and its transition (`mandate`). HITL is the reserved builtin `"hitl"` — the harness hosts elicitation, and no channel concept exists. `attest-schema` is the reserved builtin sanitizer of the quarantine exit (`BRN-16`): the policy registers it by name alone, the engine applies it itself, and an explicit `[externals]` binding for it is a load error. Mandate powers do NOT depend on the implementation behind them: wiring `builtin = "approve"` to a covering mandate is an open gate the deployer chose, legitimate per `THR-3` and visible in review.
- **[CFG-16]** At most **one dimension** MAY be declared pending-cast (`delta = { trust = "unknown" }`), and a `requires` on that same dimension is a load error — the requirement would evaluate before the resolution that establishes it. `"unknown"` is reserved, so a trust rank of that name is refused. A pending-cast declaration is a load error when the deployment does not cover confinement at that tool's result point (`POS-2`, `POS-10`): the offer of `UNK-10` needs a raw result the model has not seen, so degrading to an ordinary Unknown admission is refused.
- **[CFG-17]** A `[child] return_sanitizer` binding is validated at load: the named sanitizer MUST exist and MUST carry the `tool_output` point.
- **[CFG-18]** The transcript head is NOT part of this configuration surface. It is host configuration per `POS-6`, and a field declaring it is a load error. The head instructs the model while this surface declares what MAY flow. Therefore, a deployment that keeps them in one file gives its prompt text the same review path as its contracts.
- **[CFG-19]** An unreachable cast is a load error, as an empty mandate is (`AUT-6`): dead registration never loads. Casts are whole-source, so there is no partial per-dimension shadowing. An earlier resolver never proves a later cast unreachable because it may return no answer. An earlier constant shadows a later cast only when its scope covers the later scope and, for every registered tool origin the later cast can receive, the constant agrees with all dimensions that origin already establishes and fills every dimension it can leave unresolved. Scope is disjunctive by tag: unscoped covers every scope; otherwise a scope covers another when its tag set is a superset. The loader evaluates the finite registered tool catalogue rather than guessing about unregistered origins.
- **[CFG-20]** `[limits] planner_cap` bounds worst-case plan enumeration (`RMD-5`). When omitted, the cap is 64. Load computes the registry worst case and refuses an excess as a configuration-shape error.
- **[CFG-21]** An authority and a sanitizer MAY each carry a `hint`: one or two sentences, in the deployer words, on what the entity is for. The loader bounds its length. Every model- or reviewer-visible remedy-plan presentation that names the entity carries its hint, so the agent chooses among plans on stated purpose and a reviewer reads the intent beside the mandate. The semantic plan MAY instead name the entity and compose with the deployment's immutable registry at presentation; it need not copy the hint into plan identity, execution, or durable facts. A hint is advisory and grants nothing. It MUST NOT enter a check, change which plans the planner enumerates, change their order, or make a semantic plan stale. Therefore, a hint that overstates the mandate misleads a reader and wastes turns, and widens no power.
- **[CFG-22]** A group is written `@name` (`LBL-13`). A name without the mark is a literal reader ID. The mark is reserved: a reader ID that starts with `@` is a load error, and so is a group mention in a configuration that registers no membership resolver.
- **[CFG-23]** The loader validates the policy against the deployment coverage declaration (`POS-10`). A construct that names an engine behavior the deployment cannot perform is a load error, and the error names the missing coverage: a `tool_output` sanitizer with no covered application point — neither confinement at a result point nor the child-return crossing (`SAN-2`); a pending-cast delta without confinement at the tool's result point (`CFG-16`); a `[child]` section without context control (`POS-5`); a `requires`, dynamic delta, or pending-cast delta on a provider-run tool (`POS-7`).
- **[CFG-24]** Each dynamic audience form MUST name a registered dynamic resolver and one top-level argument. Resolver names MUST be unique. The tool-input schema MUST declare that argument as a required top-level string property. Dynamic forms MUST NOT appear in `exactly`, `cap`, `may_add`, or static reader lists.
- **[CFG-25]** **Tool inputs use `APPA Tool Parameters v1`, not general JSON Schema.** When a tool declares `parameters`, the root MUST be an object schema and every schema node MUST carry exactly one string `type`. The accepted types are `object`, `array`, `string`, `integer`, `number`, and `boolean`. The accepted keywords are closed:
  - every node MAY use `description`;
  - a scalar node MAY use either one type-correct `const` or a nonempty, type-correct, duplicate-free scalar `enum`;
  - an object node MAY use `properties`, `required`, and Boolean `additionalProperties`; omitted `properties` and `required` mean `{}` and `[]`, and omitted `additionalProperties` means `false`; every `required` name MUST be unique and name a declared property; `additionalProperties = true` admits arbitrary bounded JSON values for undeclared properties;
  - an array node MUST use one schema-valued `items` and MAY use `minItems` and `maxItems`;
  - a string node MAY use `minLength` and `maxLength`; and
  - an integer or number node MAY use `minimum`, `maximum`, `exclusiveMinimum`, and `exclusiveMaximum`.
  Boolean schemas, type arrays, `null`, references and definitions, unions and combinators, conditionals, `pattern`, `format`, tuple arrays, schema-valued `additionalProperties`, defaults, coercion hints, and every unknown keyword are load errors. Type-specific keywords on the wrong node type are also load errors. Length and item bounds are nonnegative integers, count Unicode scalar values and array items respectively, and MUST have minimum no greater than maximum. A numeric schema MAY declare at most one inclusive or exclusive lower bound and at most one inclusive or exclusive upper bound; the resulting interval MUST be nonempty.
  The v1 limits are: 64 KiB schema source, depth 16, 256 schema nodes, 64 properties and required names per object, 64 enum values, 128 UTF-8 bytes per property name, and 512 Unicode scalar values per description; and 256 KiB canonical arguments, depth 32, 4,096 container nodes (objects and arrays — scalars are bounded by the byte and token limits), 4,096 array elements, 256 KiB decoded UTF-8 per string, and 64 source bytes per number token. Numbers MUST be finite IEEE-754 binary64 values. Integers MUST be exact safe integers in `[-(2^53-1), 2^53-1]`. These are dialect limits, not planner limits. The schema-source limit counts the bytes of the authored schema's canonical JSON encoding, so configuration-format spelling cannot change the measure.
  Tool-call input is one raw JSON object. Parsing rejects duplicate keys, trailing data, invalid Unicode, unsupported numeric forms, and limit overflow before schema validation. Validation does not coerce values, insert defaults, strip unknown fields, or normalize text. The engine validates first and then produces RFC 8785 canonical bytes. Schema normalization inserts the object defaults above, rejects duplicate `required` names, sorts `required` names lexicographically by Unicode scalar value, and sorts scalar `enum` members by canonical JSON bytes. Object property order is removed by canonical JSON serialization. The normalized schema is part of the immutable `ToolContract` and policy identity. A sanitizer replacement object MUST enter through the same constructor. A placeholder or dynamic binding therefore names an explicit required top-level string property (`CFG-14`, `CFG-24`). When `parameters` is omitted, it normalizes to `{ "type": "object", "properties": {}, "required": [], "additionalProperties": true }`: any JSON object is accepted under the v1 global input limits. This compatibility default leaves that tool's argument shape untyped; an explicit schema remains strict and defaults `additionalProperties` to `false`. A host that shows the model a tool input schema MUST show exactly the normalized schema — including this default when `parameters` is omitted — never a second rendering of it.
- **[CFG-26]** A deployment MAY compose its `[policy]` from local files with `include = ["path", ...]`.
  - Each path is absolute or relative to the deployment file's directory.
  - An included file contains a policy value without a `[policy]` wrapper.
  - Each included file MUST declare the same dialect version.
  - An included file MUST NOT contain `include` or deployment bindings.
  - The deployment's inline declarations merge after the included declarations.
  - Duplicate named declarations and duplicate singleton declarations MUST refuse the complete deployment.
  - The runtime MUST flatten the result before policy validation and storage.
  - A reload MUST reread every include before it replaces the serving deployment.

Contract language leads with `requires` as a surface convention; a delta reads best as a stated consequence. Source deltas are derivable, so a dynamic resolver mapping a document to its ACL reader set CAN auto-generate `audience ∩ readers(doc)`.

---

## 13. External interfaces — `EXT`

**Runtime design gap.** Most wire protocols between runtime and its registered externals remain unspecified. `EXT-2` specifies the dynamic resolver protocol and `EXT-4` the membership resolver protocol. The other external services have no complete transport contract. This does not leave the engine interface open: `EXT-3` defines how runtime-resolved evidence enters the pure core.

External interface status:

Bindings live in the deployment's `[externals]` table (`CFG-15`), which also carries
the one timeout every machine consult shares.

| Interface | Carries | Binding | Transport |
|---|---|---|---|
| Authority resolver | A staged review (`RUL-8`), returns a ruling, a denial, or no answer | `[externals.authorities.<name>]` `url` | Payload unspecified |
| HITL elicitation | The same staged review, through human elicitation | `[externals.authorities.<name>]` `builtin = "hitl"` | Unspecified |
| Sanitizer resolver | A value, returns a derivation under declared transition | `[externals.sanitizers.<name>]` `url` | Payload unspecified |
| Dynamic resolver | A tool's named string argument, returns literal readers (`LBL-16`) | `[externals.dynamic]` with one `url` or `command` | Versioned JSON (`EXT-2`) |
| Cast resolver | `ValueId` + typed source metadata, returns one complete source label within `may_cast` | Unspecified | Payload unspecified |
| Membership resolver | A group name, returns its reader set (`LBL-13`) | `[externals.membership]` `url`, one endpoint for the one registered resolver | Versioned JSON POST (`EXT-4`) |

Each remaining runtime interface needs a request schema, a response schema, a versioning rule, and a timeout. Failure semantics are fixed for every interface:

- **[EXT-1]** **Only actual domain evidence enters the engine.** If an external times out, returns an error, abstains, or produces malformed or oversized data, runtime MUST NOT resume the consuming engine act with that outcome. No engine event or fact is created, and the existing offer, candidate, source, approval, or handoff stays unchanged. Runtime MAY retry and MAY show operational feedback outside the APPA log. No external failure MAY count as approval, denial, derivation, cast label, membership, dynamic readers, Unknown, or exhaustion. This is distinct from a real tool's `Success::Unavailable`, which is a typed tool outcome under `LOG-2`.
- **[EXT-2]** A dynamic resolver uses versioned JSON. The request is `{version:1,resolver,tool,argument,value}`. The response is `{version:1,readers:[...]}`. Each named field MUST be present. `readers` MUST contain only strings. The implementation MUST impose a response-size limit. Timeouts, malformed payloads, oversized payloads, and unsupported versions fail resolution.
  - A `url` binding sends one HTTP POST per request. A non-2xx response fails resolution.
  - A `command` binding is a nonempty executable and argument array. The runtime MUST NOT invoke a shell.
  - A relative executable path and the child working directory use the deployment file's directory.
  - One managed child serves one deployment. The runtime serializes request and response lines as newline-delimited JSON.
  - The child MUST first write `{version:1,resolvers:[...]}`. The names MUST equal the registered resolver names.
  - A startup failure MUST refuse deployment activation. A runtime failure MUST fail the pending resolution and stop that child.
  - The runtime MAY start a new child for a later resolution. Dropping the deployment MUST stop its child.
- **[EXT-3]** **External answers enter the engine as domain-specific typed evidence, never as a generic transport envelope.** Runtime supplies only actual evidence directly to the engine operation that consumes it. Stable engine identities bind it: authority evidence names the authority, live offer, and canonical call digest; sanitizer evidence names the sanitizer, live offer or pending handoff, application point, and source/call digest; cast evidence names the cast and source value identity/digest; membership evidence names the resolver, group, and consuming operation. The engine validates the live binding and the configured mandate, transition, or ceiling before producing facts. Wrong, stale, malformed, or out-of-bounds submitted evidence is rejected with no batch and no state change. Successful evidence is persisted on the outcome it fed, so replay never re-queries the external. Engine evidence types contain no no-answer, unavailable, failure, exhaustion, generic external-request, or attempt variant. Runtime may use any correlation, authentication, transport, retry, timeout, and operational-failure representation behind this boundary.
- **[EXT-4]** A membership resolver uses JSON over HTTP POST. The request is `{version:1,resolver,group}`, where `group` is the name without its `@` mark. The response is `{version:1,readers:[...]}`. Each named field MUST be present. `readers` MUST contain only strings, and each string MUST be a literal reader ID: a response that contains `public` or an `@`-marked name is malformed (`LBL-13`). An empty `readers` list is a successful answer. Non-2xx responses, timeouts, malformed payloads, and oversized payloads fail closed. The implementation MUST impose a response-size limit. Unsupported versions fail resolution.

---

## 14. Implementation shape

The engine has two layers.

- **[IMP-1]** The **inner layer** is a stateless pure decision core. Its only steady-state public operation is `Engine::handle(&EngineView, EngineEvent) -> EngineDecision`.
  - `EngineEvent` is a closed typed enum. `ProposalBatch` carries the model response's policy content and nothing else: its ordinary proposed calls, each exposed provider-run result (`POS-7`), and at most one runtime mark naming a proposed call as the deployment's context-controlled spawn; a matching singleton batch may consume one live `CallApproval` and open its exact dispatch. Releasing a marked call also creates `ForkPrepared` in that batch (`BRN-2`). `ExecuteOffer` is a separate control act that may prepare one exact `CallApproval`. Other variants cover positive typed external-evidence continuation, tool outcome, `BindFork`, and child return. External operational failures are not engine events (`EXT-1`). Requests, user turns, transcripts, and host runs are not engine events. Runtime MUST NOT split one logical policy act into separate policy commits.
  - There is no generic refresh or resume event. Runtime MAY repeat the same stable semantic call against a later view. The engine MUST use the call identity and durable facts already in that view to return the current next step idempotently: request the still-missing evidence, return the current offers or guidance, continue from a successful-tool or submitted-return checkpoint, or return the already-recorded terminal outcome. A repeated call creates no duplicate lifecycle transition. Runtime failure, retry, and restart therefore change no engine algebra.
  - `EngineDecision` carries an optional sealed, revisioned fact batch and one typed follow-up package. Runtime MUST append the batch before it performs any follow-up item.
  - A `ProposalBatch` follow-up contains every released canonical invocation and all model-visible blocked feedback. It can contain both. Runtime MUST NOT inspect the opaque fact batch to reconstruct this work.
  - The core has zero IO, zero clock, and no owned event log. It is a function of the event log and immutable deployment settings. Explicit entropy such as `OfferNonce` is input data, never engine state.
  - Every decision and lifecycle transition is replayable from durable records. Remedy planning, offer binding, and transition validation belong to the core. Storage, compare-and-swap append, randomness, retry orchestration, and delivery belong to runtime.
- **[IMP-2]** `EngineView` is an opaque, disposable cache derived from the event log. Runtime stores it, but only the engine constructs, advances, or rebuilds it. Runtime advances the cache only from an accepted sealed batch. On cache loss, the engine rebuilds it from the validated record stream of `IMP-4`. The event log remains the source of truth.
- **[IMP-3]** The **outer layer** owns state: the event log, conditional append, serialization, persistence, acknowledgement, and recovery. A harness author embeds the outer layer with whatever store they already run. The decision core NEVER sees IO and does not distinguish an in-memory benchmark adapter from a durable or replicated backend; it relies only on the whole-batch revision contract of `LOG-9`.
- **[IMP-4]** External labels, authority decisions, sanitizers, and dynamic resolvers are trusted inputs. Together they form the trusted base. Invariants on state changes MUST be enforced structurally: by the implementation language type system where it CAN express them, and otherwise by one pure engine-owned transition validator.
  - The public `handle` operation sends each complete candidate transition through that validator. It returns engine mutations only as a sealed `ValidatedFactBatch`. Open-time construction and replay use the same validator, but are not alternative orchestration paths. A caller cannot publicly construct or mutate a validated batch. This is structural validity, not the policy check forbidden as an append gate by `LOG-1`.
  - Runtime treats a validated batch as opaque engine output and only performs the whole-batch revision operation of `LOG-9`; it MUST NOT recreate engine transition rules at the store boundary.
  - The validator owns `PolicyBasis` changes (`RMD-8`, `LOG-11`). It records each family or trajectory version advance at most once per decision, stamps every new offer and `CallApproval` with the post-decision basis, and refuses any batch that leaves an old-basis offer or approval executable.
  - Serialization removes the in-process seal. Cold replay, import, and recovery therefore validate raw engine facts sequentially through the same transition rules before exposing trusted views. Validation MAY stream and fail at the first impossible record; it need not materialize a second log.
  - Runtime-owned transcript, delivery, storage, and diagnostic records that are algebraically inert and cannot affect an engine projection are outside this semantic validator. They retain their own outer schema/version checks and MUST NOT be accepted as engine facts.
- **[IMP-5]** The checker MUST stay free of ad-hoc conditionals. Registered contracts and authorities are the ONLY sources of a decision. Every decision reduces to label arithmetic or a log query. Anything imperative — an approval flow, a model that vets content, a lookup resolving a recipient to readers — lives in a registered external and NEVER in the engine.

### 14.1 Accepted gaps

The invoke/append crash gap is out of scope in this version: effects append when the call succeeds. A host failing between a successful invocation and the append MAY lose effects. Its confined-result cousin is accepted on the same terms. Hardening — a durable outbox committing invocation and effects as one record — is future work for the outer layer.

A multi-item follow-up package can be only partly delivered when runtime fails. The engine log records
the decision, not transport delivery. It cannot distinguish an unsent invocation from a sent invocation
whose outcome was lost. Any per-item cursor or outbox is runtime-owned state and MAY use the stable
engine identities in the package. This specification leaves that recovery protocol open.

The document `rationale.md` lists these gaps with their reasoning, alongside the failed-send egress window.

---

## 15. Threat model

- **[THR-1]** The agent is **benign but confusable**: steerable by injected prompt instructions, but not itself adversarial. A malicious model constructing covert channels is out of scope.
- **[THR-2]** Malice enters exclusively through content below the top trust rank, or `unknown` until resolved. Content at the top rank is trusted by definition. APPA offers no protection when a top-rank source is malicious.
- **[THR-3]** Authorities, sanitizers, casts, dynamic resolvers, and configuration form the deployer trusted base. A permissive configuration is legitimate and voids corresponding guarantees explicitly and auditably.
- **[THR-4]** The engine assumes the runtime supplies a serialized event log with the atomic revision behavior of `LOG-9`. Persistence and failure-recovery strength are runtime/deployment properties, not claims made or enforced by the engine.
- **[THR-5]** Approval UX and contract coverage bootstrapping are adoption concerns. They are out of scope here. APPA is exactly as good as the authorities and contracts registered into it.
- **[THR-6]** External identity machinery — OAuth, SAML, the directory saying who sits behind an address — is outside APPA. APPA trusts what identity machinery returns. A reader ID is an opaque atom to the algebra. Establishing that the atom names the right person is the deployment job.
- **[THR-7]** Deployments where tool credentials exceed the end-user read rights are out of scope in this version. APPA assumes that showing the user a fetched result releases nothing the user could not fetch alone.
- **[THR-8]** Two assumption sets bound what a gateway deployment can verify. For each tool without dispatch control (`POS-7`), the engine assumes the harness executes a released call unchanged and at most once, and executes no call the engine did not release. For every deployment whose client builds the requests, the engine assumes the client returns the trajectory token where token binding is declared (`POS-8`), and puts nothing into a request except what the gateway served and what the principal supplied. A harness that breaks these is misconfigured, not an attacker — APPA does not defend against a harness that works against it. The engine still refuses an outcome for an unknown, mismatched, or already-consumed `DispatchId`; that catches integration mistakes but cannot prove what an uncontrolled host executed. Operator diagnostics and alerting are runtime/host concerns. A binding failure is not a break: `POS-8` governs it.
- **[THR-9]** The surrounding host authenticates principals and admits requests inside a trusted environment. APPA's trajectory token prevents cheap guessing and accidental/casual cross-principal binding, but is not a replacement identity or authorization system. A malicious authenticated principal with arbitrary APPA request access, a compromised host, or an attacker already inside that trusted boundary is out of scope. This assumption removes no fail-closed engine check and does not permit sequential or shared trajectory handles (`POS-8`).

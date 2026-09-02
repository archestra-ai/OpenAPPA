# Annotator model prompt plan

## Goal

Make the stock model Annotator understand its policy-specific vocabulary and receive enough context to produce one complete annotation. Keep the existing conservative judgment where the call is ambiguous.

## Decisions

### 1. No separate abstention result

Do not add an `unknown`, `abstain`, or `refuse` variant to the annotation answer. The fallback is the default, neutral annotation:

```json
{"delta":{},"requires":{"history":[],"attention":[]},"emits":[]}
```

The current prompt already states this exact default and names neutral examples. Preserve that instruction when editing the prompt. If implementation work reveals a path where the model does not receive this definition, add it there.

When the visible evidence supports a safer trust rank or audience, the Annotator should choose that value instead of treating uncertainty as proof of trusted or public data. This conservative classification is the resolution to the absence of a separate abstention result.

### 2 and 8. Add policy-authored Annotator hints

Add an optional `hint` to `[[annotator]]` in `appa.toml`, following the existing Authority and Sanitizer convention. The hint is trusted deployer instruction and may define:

- the meanings of policy-specific trust ranks, audience atoms, attention marks, and effects;
- the evidence that selects each value;
- complete, deployment-specific annotation examples.

Carry the hint through policy parsing, the engine's Annotator declaration, the consult declaration, and the model system prompt. Keep the mandate closed: a hint explains when to use values but cannot expand the values accepted by answer validation or the generated schema.

Update the policy reference and all matching golden terminology when the public contract changes. Add parser, consult serialization, prompt-rendering, and answer-validation coverage.

### 3. Preserve call context with mapped inputs

Fix mapped Annotator inputs so data minimization does not erase the context needed to annotate the call. A mapped consult must tell the Annotator:

- the tool name;
- the policy-declared tool description, when present;
- each input alias and the `$tool_call` source from which it was selected;
- only the selected argument values, rather than all unselected arguments.

Use one unambiguous wire shape for complete-call and mapped-input consults. Update the preamble so it describes that shape rather than claiming every artifact visibly contains the complete call. Update HTTP, command, module, and model transport tests because they share the consult envelope.

### 4. Audience terminology

No change. Audience atoms are opaque and may denote readership domains containing multiple concrete readers. Do not impose a one-atom-per-person interpretation.

### 5. Conservative classification under ambiguity

Keep the instruction to prefer the safer supported trust rank or audience when evidence is materially ambiguous. This is the chosen solution to problem 1, not a separate defect to remove.

### 6. Public-destination heuristic

No change for now. Continue treating a hosted destination as public unless the visible call proves a narrower readership.

### 7. Remove the field-presence contradiction

Replace “fill every field” with precise shape guidance: always return the three required top-level fields, while optional leaves are omitted only to assert their identity behavior. Keep `requires.history` and `requires.attention` present even when empty.

### 9. Subjective judgment

No change. The Annotator must make contextual judgments that cannot be reduced to mechanical prompt rules. Retain language such as “most reasonable,” “materially ambiguous,” and “concrete evidence”; policy-authored hints provide deployment-specific calibration.

## Verification

- A hint round-trips from TOML into the Annotator model's system prompt and never enters the untrusted artifact.
- The generated schema remains bounded by the mandate, regardless of hint contents.
- A mapped-input prompt identifies the tool and every selected value's source without exposing unselected arguments.
- A complete-call prompt still carries the complete call.
- The prompt states the exact neutral annotation and no longer says to fill every optional field.
- Existing public-destination and conservative-ambiguity instructions remain intact.

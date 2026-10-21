// Which operational fault a refuse names, as the runtime spells it (RefusalClass in appa-runtime-api).
export const REFUSAL_CLASSES = [
  'storage', 'untrusted_log', 'policy_unavailable', 'policy_moved', 'legacy_family', 'inventory_refused',
  'engine_invariant', 'contended', 'resolution_diverged', 'annotation_refused', 'undeclared_tool',
  'undeclared_spawn', 'peer_message', 'principal', 'outstanding_call', 'call_id_reused', 'trajectory_ended',
  'trajectory', 'dispatch', 'remedy', 'spawn_not_taken', 'spawn',
] as const

export type RefusalClass = (typeof REFUSAL_CLASSES)[number]
export type Refusal = { class: RefusalClass; detail: string }

function isRefusalClass(value: unknown): value is RefusalClass {
  return (REFUSAL_CLASSES as readonly unknown[]).includes(value)
}

/** A refuse decision's class and detail; undefined when either is outside the wire. */
export function refusal(reply: Record<string, unknown>): Refusal | undefined {
  const { class: refused, detail } = reply
  return isRefusalClass(refused) && typeof detail === 'string' ? { class: refused, detail } : undefined
}

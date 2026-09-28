import { CLAUDE_POLICY_TIMING, CLAUDE_SESSION_CHOICE } from "@/lib/directive-content";

export function ClaudePolicyTiming() {
  return (
    <aside className="claude-policy-timing" aria-label="Policy setup timing">
      <div className="claude-policy-duration">
        <strong>{CLAUDE_POLICY_TIMING.duration}</strong>
        <span>{CLAUDE_POLICY_TIMING.durationNote}</span>
      </div>
      <div className="claude-policy-timing-copy">
        <p>{CLAUDE_POLICY_TIMING.lead}</p>
        <p>
          <strong>{CLAUDE_POLICY_TIMING.emphasis}</strong> {CLAUDE_POLICY_TIMING.rest}
        </p>
      </div>
    </aside>
  );
}

export function ClaudeSessionChoice() {
  return (
    <div className="claude-session-choice" aria-label="Choose a Claude Code session">
      {CLAUDE_SESSION_CHOICE.map((option) => (
        <div className={option.protected ? "claude-session-protected" : undefined} key={option.command}>
          <code>{option.command}</code>
          <strong>{option.title}</strong>
          <span>{option.note}</span>
        </div>
      ))}
    </div>
  );
}

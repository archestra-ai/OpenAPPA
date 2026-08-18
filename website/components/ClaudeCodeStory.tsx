export function ClaudePolicyTiming() {
  return (
    <aside className="claude-policy-timing" aria-label="Policy setup timing">
      <div className="claude-policy-duration">
        <strong>~10 min</strong>
        <span>for the demo policy</span>
      </div>
      <div className="claude-policy-timing-copy">
        <p>
          Claude typically needs about ten minutes to inspect your tools, ask any necessary questions,
          and generate the initial policy.
        </p>
        <p>
          <strong>In a corporate deployment, this is a one-time governance step.</strong> The policy is
          generated, reviewed, and approved once, then shared across the protected agent surfaces.
        </p>
      </div>
    </aside>
  );
}

export function ClaudeSessionChoice() {
  return (
    <div className="claude-session-choice" aria-label="Choose a Claude Code session">
      <div>
        <code>$ claude</code>
        <strong>Standard Claude Code</strong>
        <span>The plugin leaves this path unchanged.</span>
      </div>
      <div className="claude-session-protected">
        <code>$ clappa</code>
        <strong>Claude Code + OpenAPPA</strong>
        <span>Tool flows are checked against your policy.</span>
      </div>
    </div>
  );
}

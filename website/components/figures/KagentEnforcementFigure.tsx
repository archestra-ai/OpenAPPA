export function KagentProfileFigure() {
  return (
    <figure className="kagent-profile-figure" aria-labelledby="kagent-profile-caption">
      <figcaption id="kagent-profile-caption" className="sr-only">
        The kagent Go runtime sends tool calls and results to appa-runtime. appa-runtime returns policy decisions over local HTTP.
      </figcaption>

      <div className="kagent-figure-title">
        <span>runtime request and decision flow</span>
      </div>

      <div className="kagent-profile-actor">
        <div className="kagent-profile-process-flow">
          <div className="kagent-profile-process">
            <strong>kagent-go-adk</strong>
            <small>Google ADK callbacks</small>
            <small>OpenAPPA Go extension</small>
          </div>

          <div className="kagent-profile-exchange" aria-hidden="true">
            <span className="kagent-profile-request">ToolCall / ToolResult</span>
            <span className="kagent-profile-response">HookDecision</span>
          </div>

          <div className="kagent-profile-process kagent-profile-process-appa">
            <strong>appa-runtime</strong>
            <small>appa-adapter-kagent → Engine</small>
            <small>append-only trajectory log</small>
          </div>
        </div>

        <div className="kagent-profile-runtime-meta">
          <span>local HTTP · 127.0.0.1:8787/hook</span>
        </div>
      </div>
    </figure>
  );
}

export function KagentEnforcementFigure() {
  return (
    <figure className="kagent-figure" aria-labelledby="kagent-figure-caption">
      <figcaption id="kagent-figure-caption" className="sr-only">
        Google ADK calls OpenAPPA before tool dispatch and after the terminal tool outcome. OpenAPPA can block the call or replace its result.
      </figcaption>

      <div className="kagent-figure-title">
        <span>two Google ADK enforcement points</span>
      </div>

      <div className="kagent-gates">
        <div className="kagent-gate">
          <span>01 · before dispatch</span>
          <strong>BeforeToolCallbacks</strong>
          <small>HookEvent::ToolCall → /hook</small>
          <small>AllowCall or DenyCall</small>
        </div>

        <div className="kagent-execution">
          <span>AllowCall</span>
          <strong>tool.Run</strong>
          <span>outcome</span>
        </div>

        <div className="kagent-gate">
          <span>02 · after execution</span>
          <strong>AfterToolCallbacks</strong>
          <small>HookEvent::ToolResult → /hook</small>
          <small>Ack, ReplaceOutput, or Block</small>
          <small>OnToolErrorCallbacks record failure</small>
        </div>
      </div>
    </figure>
  );
}

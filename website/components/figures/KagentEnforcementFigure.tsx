export function KagentDeploymentFigure() {
  return (
    <figure className="kagent-deployment-figure" aria-labelledby="kagent-deployment-caption">
      <figcaption id="kagent-deployment-caption" className="sr-only">
        The operator installs resources. kagent prepares a revision and ActorTemplate. The user creates an AgentInstance. Substrate creates the Actor.
      </figcaption>

      <div className="kagent-figure-title">
        <span>deployment flow</span>
      </div>

      <div className="kagent-deployment-flow">
        <div className="kagent-deployment-phase">
          <span>01 · cluster operator</span>
          <strong>install resources</strong>
          <div>
            <small>CRD bundle</small>
            <small>patched control plane</small>
            <small>Harness</small>
            <small>policy ConfigMap</small>
            <small>AgentTemplate</small>
          </div>
        </div>

        <span className="kagent-deployment-arrow" aria-hidden="true">→</span>

        <div className="kagent-deployment-phase kagent-deployment-phase-kagent">
          <span>02 · kagent controller</span>
          <strong>prepare immutable runtime</strong>
          <div>
            <small>prepared revision</small>
            <small>ActorTemplate</small>
            <small>image digest</small>
            <small>policy digest</small>
          </div>
        </div>

        <span className="kagent-deployment-arrow" aria-hidden="true">→</span>

        <div className="kagent-deployment-phase">
          <span>03 · application team</span>
          <strong>create AgentInstance</strong>
          <div>
            <small>select AgentTemplate</small>
            <small>select Harness</small>
          </div>
        </div>

        <span className="kagent-deployment-arrow" aria-hidden="true">→</span>

        <div className="kagent-deployment-phase">
          <span>04 · Substrate</span>
          <strong>create the Actor</strong>
          <div>
            <small>pull Actor image</small>
            <small>start supervisor</small>
            <small>mount durable /data</small>
            <small>expose A2A endpoint</small>
          </div>
        </div>
      </div>
    </figure>
  );
}

export function KagentProfileFigure() {
  return (
    <figure className="kagent-profile-figure" aria-labelledby="kagent-profile-caption">
      <figcaption id="kagent-profile-caption" className="sr-only">
        The kagent Go runtime sends tool calls and results to appa-runtime. appa-runtime returns policy decisions over local HTTP.
      </figcaption>

      <div className="kagent-figure-title">
        <span>component architecture</span>
        <span>inside one Substrate Actor</span>
      </div>

      <div className="kagent-profile-actor">
        <div className="kagent-profile-actor-title">
          <span>one container</span>
          <strong>digest-pinned kagent-openappa Actor image</strong>
        </div>

        <div className="kagent-profile-supervisor">
          <span>PID 1</span>
          <strong>kagent-openappa-supervisor</strong>
          <small>starts, monitors, signals, and stops both child processes</small>
        </div>

        <div className="kagent-profile-process-flow">
          <div className="kagent-profile-process">
            <span>kagent-go-adk process</span>
            <strong>Google ADK</strong>
            <small>OpenAPPA extension implements callbacks and plugin</small>
          </div>

          <div className="kagent-profile-exchange" aria-hidden="true">
            <span className="kagent-profile-request">ToolCall</span>
            <span className="kagent-profile-response">HookDecision</span>
          </div>

          <div className="kagent-profile-process kagent-profile-process-appa">
            <span>appa-runtime process</span>
            <strong>appa-adapter-kagent + Engine</strong>
            <small>apply policy and append facts to /data/openappa/appa.db</small>
          </div>
        </div>

        <div className="kagent-profile-runtime-meta">
          <span>local HTTP · 127.0.0.1:8787/hook</span>
          <span>durable state · /data/openappa/appa.db</span>
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
        <span>Google ADK extension points</span>
        <span>kagent bridge ↔ OpenAPPA</span>
      </div>

      <div className="kagent-gates">
        <div className="kagent-gate">
          <span>01 · before dispatch</span>
          <strong>BeforeToolCallbacks</strong>
          <small>kagent sends HookEvent::ToolCall</small>
          <small>OpenAPPA returns AllowCall or DenyCall</small>
        </div>

        <div className="kagent-execution">
          <span>AllowCall</span>
          <strong>tool.Run</strong>
          <span>outcome</span>
        </div>

        <div className="kagent-gate">
          <span>02 · after execution</span>
          <strong>AfterToolCallbacks</strong>
          <small>OnToolErrorCallbacks capture failure first</small>
          <small>kagent sends HookEvent::ToolResult</small>
        </div>
      </div>

      <div className="kagent-runtime-band">
        <span>kagent OpenAPPA extension</span>
        <span aria-hidden="true">↔</span>
        <strong>HTTP /hook</strong>
        <span aria-hidden="true">↔</span>
        <span>appa-adapter-kagent + Engine</span>
      </div>

      <div className="kagent-decision-strip">
        <span><strong>DenyCall</strong> blocks dispatch</span>
        <span><strong>Ack</strong> keeps the result</span>
        <span><strong>ReplaceOutput</strong> substitutes before model delivery</span>
        <span><strong>Block</strong> withholds the result</span>
      </div>
    </figure>
  );
}

import { PixelMark } from "@/components/Logo";

/* Static architecture figure for the integration guide. It follows the
   site's native figure language: quiet panels, mono labels, restrained
   accent color, and explicit rails for the two enforcement directions. */

function PanelTitle({ x, title, subtitle }: { x: number; title: string; subtitle?: string }) {
  return (
    <>
      <text x={x} y="76" className="rof-title">
        {title}
      </text>
      {subtitle && (
        <text x={x} y="99" className="rof-subtitle">
          {subtitle}
        </text>
      )}
    </>
  );
}

function Card({ x, y, width, title, detail }: { x: number; y: number; width: number; title: string; detail: string }) {
  return (
    <g>
      <rect x={x} y={y} width={width} height="58" rx="6" className="rof-card" />
      <text x={x + 14} y={y + 23} className="rof-card-title">
        {title}
      </text>
      <text x={x + 14} y={y + 42} className="rof-card-detail">
        {detail}
      </text>
    </g>
  );
}

export function RuntimeOverviewFigure() {
  return (
    <div className="runtime-overview-figure">
      <svg
        viewBox="0 0 900 390"
        role="img"
        aria-labelledby="runtime-overview-title runtime-overview-description"
      >
        <title id="runtime-overview-title">OpenAPPA integration architecture</title>
        <desc id="runtime-overview-description">
          The agent harness intercepts lifecycle events via middleware, callbacks, or plugins and sends them to the OpenAPPA runtime at POST /hook. Inside the runtime, an adapter decodes the event and the policy engine evaluates security rules to return a decision. Remediation runs through the runtime MCP service.
        </desc>
        <defs>
          <marker id="rof-arrow" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto">
            <path d="M 0 0 L 8 4 L 0 8 z" fill="var(--icon)" />
          </marker>
          <marker id="rof-arrow-accent" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto">
            <path d="M 0 0 L 8 4 L 0 8 z" fill="var(--accent)" />
          </marker>
        </defs>

        {/* Two ownership boundaries: the agent harness and the OpenAPPA runtime. */}
        <rect x="30" y="42" width="390" height="292" rx="9" className="rof-panel rof-panel-bridge" />
        <rect x="480" y="42" width="390" height="292" rx="9" className="rof-panel" />

        {/* Left: Agent Harness (can be in-process middleware, callbacks, or external plugin) */}
        <PanelTitle x={54} title="Agent harness" subtitle="agent loop · in-process middleware, callbacks, or plugin" />
        <Card x={54} y={122} width={342} title="Agent execution loop" detail="prompts model & plans proposed tool calls" />
        <path d="M 225 180 L 225 195" className="rof-rail" markerEnd="url(#rof-arrow)" />
        <Card x={54} y={198} width={342} title="Lifecycle hooks & enforcement" detail="intercepts calls & results · enforces allow, block, replace" />
        <g className="rof-fail-closed">
          <circle cx="65" cy="286" r="4" />
          <text x="77" y="290">fail closed on any error</text>
        </g>

        {/* Right: OpenAPPA Runtime */}
        <g transform="translate(504 61)" aria-hidden="true">
          <PixelMark size={24} />
        </g>
        <text x="538" y="76" className="rof-title">OpenAPPA runtime</text>
        <text x="504" y="99" className="rof-subtitle">daemon (appa-runtime) · policy engine & adapters</text>
        <Card x={504} y={122} width={342} title="Adapter (hook receiver)" detail="receives POST /hook · decodes payload into HookEvent" />
        <path d="M 675 180 L 675 195" className="rof-rail" markerEnd="url(#rof-arrow)" />
        <Card x={504} y={198} width={342} title="Policy engine (APPA core)" detail="evaluates security labels, tool contracts & remedies" />
        <g className="rof-fail-closed">
          <circle cx="515" cy="286" r="4" />
          <text x="527" y="290">deterministic policy rules</text>
        </g>

        {/* Request and response rails between harness and runtime. */}
        <path d="M 420 150 L 477 150" className="rof-rail rof-rail-accent" markerEnd="url(#rof-arrow-accent)" />
        <text x="450" y="137" className="rof-rail-label" textAnchor="middle">POST /hook</text>

        <path d="M 480 231 L 423 231" className="rof-rail" markerEnd="url(#rof-arrow)" />
        <text x="450" y="219" className="rof-rail-label" textAnchor="middle">decision</text>

        {/* Remedies use the runtime's MCP surface rather than the hook codec. */}
        <path d="M 225 334 C 225 385, 675 385, 675 334" className="rof-remedy-rail" markerEnd="url(#rof-arrow)" />
        <rect x="338" y="343" width="224" height="27" rx="13.5" className="rof-remedy-chip" />
        <text x="450" y="361" className="rof-remedy-label" textAnchor="middle">/mcp · execute_remedy_plan</text>
      </svg>
    </div>
  );
}

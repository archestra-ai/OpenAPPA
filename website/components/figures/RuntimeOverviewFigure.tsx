import { PixelMark } from "@/components/Logo";

export function RuntimeOverviewFigure({ overview = false }: { overview?: boolean }) {
  return (
    <div className="runtime-overview-figure">
      <svg
        viewBox="0 0 900 360"
        role="img"
        aria-labelledby="runtime-overview-title runtime-overview-description"
      >
        <title id="runtime-overview-title">{overview ? "Your agent and the OpenAPPA runtime" : "Agent hooks and the OpenAPPA runtime"}</title>
        <desc id="runtime-overview-description">
          {overview
            ? "Your agent submits calls and results through an in-process SDK or HTTP. The runtime returns decisions. Remedy execution uses the in-process API or the runtime's MCP endpoint."
            : "The agent loop and hooks send POST /hook to the adapter and core, which return a decision. The agent calls the execute_remedy_plan tool through /mcp."}
        </desc>
        <defs>
          <marker id="rof-arrow" viewBox="0 0 12 12" refX="12" refY="6" markerWidth="12" markerHeight="12" markerUnits="userSpaceOnUse" orient="auto">
            <path d="M0 0H4V2H8V4H12V8H8V10H4V12H0Z" fill="context-stroke" />
          </marker>
        </defs>

        <g aria-hidden="true">
          <g transform="translate(58 36)"><PixelMark size={48} /></g>
          <g transform="translate(758 20)"><PixelMark size={72} /></g>
          <g transform="translate(838 62)"><PixelMark size={24} /></g>
          <path d="M735 32V44M729 38H741M849 24V36M843 30H855" className="rof-spark" />
        </g>

        <text x="152" y="116" className="rof-title" textAnchor="middle">Agent</text>
        <text x="748" y="116" className="rof-title" textAnchor="middle">OpenAPPA</text>
        <path d="M42 136H264V144H272V220H264V228H42V220H34V144H42Z" className="rof-panel" />
        <path d="M636 136H858V144H866V220H858V228H636V220H628V144H636Z" className="rof-panel" />
        <text x="152" y="189" className="rof-block-label" textAnchor="middle">{overview ? "Agent loop" : "Loop + Hooks"}</text>
        <text x="748" y="189" className="rof-block-label" textAnchor="middle">Adapter + Core</text>

        <text x="450" y="142" className="rof-rail-label" textAnchor="middle">{overview ? "SDK or HTTP" : "POST /hook"}</text>
        <path d="M288 156H612" className="rof-rail rof-rail-accent" markerEnd="url(#rof-arrow)" />
        <text x="450" y="192" className="rof-rail-label" textAnchor="middle">decision</text>
        <path d="M612 206H288" className="rof-rail" markerEnd="url(#rof-arrow)" />

        <path d="M152 244V290H748V244" className="rof-rail rof-rail-accent" markerEnd="url(#rof-arrow)" />
        <text x="450" y="278" className="rof-rail-label" textAnchor="middle">{overview ? "Remedy execution" : "/mcp"}</text>
        <text x="450" y="324" className="rof-rail-label" textAnchor="middle">{overview ? "In-process API or /mcp" : "execute_remedy_plan tool"}</text>
      </svg>
    </div>
  );
}

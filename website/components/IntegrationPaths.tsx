import { PixelMark } from "@/components/Logo";

const paths = [
  { href: "#embed-the-sdk-in-your-agent", method: "Your own agent", detail: "Any language. You own the agent loop and tool execution." },
  { href: "#connect-a-coding-agent-through-hooks", method: "Agents through hooks", detail: "Claude Code, or harnesses such as omp and Hermes with a custom adapter." },
  { href: "#use-appa-at-the-llm-proxy", method: "LLM proxy", detail: "Apply policies centrally through Archestra." },
];

export function IntegrationPaths() {
  return (
    <nav aria-label="Choose an integration path" className="not-prose my-7 grid gap-3 sm:grid-cols-3">
      {paths.map((path, index) => (
        <a key={path.href} href={path.href} style={{ textDecoration: "none" }} className="group rounded-lg border border-[var(--border)] bg-[var(--bg-weak)] p-4 transition-colors hover:border-[var(--accent)] focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-[var(--accent)]">
          <div aria-hidden="true" className="my-4 flex h-28 items-center justify-center text-[var(--text-weak)]">
            <div className={`grid h-28 w-32 grid-rows-[28px_20px_44px] content-center justify-items-center [&_.appa-mark-float]:animate-none ${index === 0 ? "rounded border border-[var(--border)] bg-[var(--bg)]" : ""}`}>
              <div className="flex items-center gap-2 font-mono text-xs">
                {index === 0 ? <span>your agent</span> : (
                  Array.from({ length: index === 1 ? 1 : 3 }, (_, agent) => (
                    <span key={agent} className="rounded border border-[var(--border)] bg-[var(--bg)] px-2 py-1">&gt;_</span>
                  ))
                )}
              </div>
              <span className="text-[var(--accent)] leading-5">{index === 0 ? "" : "↓"}</span>
              <PixelMark size={48} />
            </div>
          </div>
          <strong className="my-2 flex min-h-12 items-start justify-between gap-2 text-base leading-6 text-[var(--text-strong)] group-hover:text-[var(--accent)]"><span>{path.method}</span><span aria-hidden="true" className="shrink-0">→</span></strong>
          <span className="block text-sm leading-relaxed text-[var(--text)]">{path.detail}</span>
        </a>
      ))}
    </nav>
  );
}

export function IntegrationCheckpoints() {
  return (
    <div className="not-prose my-6 divide-y divide-[var(--border)] rounded-lg border border-[var(--border)] text-sm">
      {[
        ["Before a tool runs", "Submit the proposed call and wait. Execute only on allow_call. On deny_call, return APPA’s feedback and remedy offers to the model without running the tool."],
        ["Before a result reaches the model", "Submit the tool outcome before adding it to the transcript. Deliver only admitted content. Use APPA’s replacement when supplied; withhold a blocked result."],
        ["When the agent requests a remedy", "Route the selected offer through APPA. A request is not approval: APPA checks the authority or runs the sanitizer before permitting the flow."],
      ].map(([title, body]) => (
        <details key={title} className="px-4 py-3">
          <summary className="cursor-pointer font-semibold text-[var(--text-strong)] focus-visible:outline-2 focus-visible:outline-[var(--accent)]">{title}</summary>
          <p className="mb-1 mt-3 leading-relaxed text-[var(--text)]">{body}</p>
        </details>
      ))}
    </div>
  );
}

import Link from "next/link";

const BATTERIES = [
  {
    name: "Slack",
    description: "Rules for 19 Slack tools, with audiences from Slack users and groups.",
    href: "/battery-slack",
  },
  {
    name: "Claude Code tools",
    description: "Rules for Claude Code's Bash and Read tools.",
    href: "/battery-claude-code",
  },
  {
    name: "GitHub",
    description: "Rules for 44 repository, issue, pull request, and user tools.",
    href: "/battery-github",
  },
  {
    name: "Grain",
    description: "Rules for 49 meeting, transcript, deal, and admin tools.",
    href: "/battery-grain",
  },
  {
    name: "Google Workspace",
    description: "Uses your Workspace directory and groups to build audiences.",
    href: "/battery-google-workspace",
  },
  {
    name: "Add your own",
    href: "/write-a-battery",
    add: true,
  },
] as const;

export function BatteryCatalog() {
  return (
    <section className="battery-catalog" aria-label="Available OpenAPPA batteries">
      <div className="battery-catalog-grid">
        {BATTERIES.map((battery) => (
          <Link
            className={`battery-card${"add" in battery ? " battery-card-add" : ""}`}
            href={battery.href}
            key={battery.name}
          >
            {"add" in battery ? (
              <>
                <span className="battery-card-plus" aria-hidden="true">+</span>
                <strong className="battery-card-name">{battery.name}</strong>
              </>
            ) : (
              <>
                <span className="battery-card-heading">
                  <strong className="battery-card-name">{battery.name}</strong>
                  <span className="battery-card-arrow" aria-hidden="true">→</span>
                </span>
                <span className="battery-card-description">{battery.description}</span>
              </>
            )}
          </Link>
        ))}
      </div>
    </section>
  );
}

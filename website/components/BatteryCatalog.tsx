import Link from "next/link";

const BATTERIES = [
  {
    name: "Slack",
    description: "Rules for 19 Slack tools, with audiences from Slack channels, users, and groups.",
    href: "/battery-slack",
    logo: "/images/batteries/slack.svg",
  },
  {
    name: "Claude Code tools",
    description: "Rules for Bash and Read, with a built-in Bash annotator.",
    href: "/battery-claude-code",
    logo: "/images/batteries/claude.svg",
  },
  {
    name: "GitHub",
    description: "Rules for 44 repository, issue, pull request, and user tools; each repository's visibility decides its readers.",
    href: "/battery-github",
    logo: "/images/batteries/github.svg",
  },
  {
    name: "Linear",
    description: "Rules for 65 tools with per-issue, per-team, and per-project audiences, and reviewed writes.",
    href: "/battery-linear",
    logo: "/images/batteries/linear.svg",
  },
  {
    name: "Grain",
    description: "Rules for 49 meeting, transcript, deal, and admin tools.",
    href: "/battery-grain",
    logo: "/images/batteries/grain.svg",
  },
  {
    name: "Google Workspace",
    description: "Uses your Workspace directory and groups to build audiences.",
    href: "/battery-google-workspace",
    logo: "/images/batteries/google-workspace.svg",
  },
  {
    name: "Sentry",
    description: "Rules for 9 listed and 55 catalog tools, internal reads, and reviewed writes.",
    href: "/battery-sentry",
    logo: "/images/batteries/sentry.svg",
  },
  {
    name: "Notion",
    description: "Rules for 36 tools; reads are internal because Notion exposes no page permissions.",
    href: "/battery-notion",
    logo: "/images/batteries/notion.svg",
  },
  {
    name: "Microsoft Learn",
    description: "Public documentation search and page fetches with untrusted result labeling.",
    href: "/battery-microsoft-learn",
    logo: "/images/batteries/microsoft-learn.svg",
  },
  {
    name: "Cloudflare docs",
    description: "Public developer documentation search and guide fetches.",
    href: "/battery-cloudflare-docs",
    logo: "/images/batteries/cloudflare.svg",
  },
  {
    name: "Cloudflare Observability",
    description: "Read-only coverage for Workers logs, metrics, telemetry, and bundle downloads.",
    href: "/battery-cloudflare-observability",
    logo: "/images/batteries/cloudflare.svg",
  },
  {
    name: "LaunchDarkly",
    description: "Rules for 20 flag, environment, AI Config, and audit tools; every write is reviewed.",
    href: "/battery-launchdarkly",
    logo: "/images/batteries/launchdarkly.svg",
  },
  {
    name: "PostHog",
    description: "Rules for 44 analytics, flag, experiment, and survey tools; internal reads and reviewed writes.",
    href: "/battery-posthog",
    logo: "/images/batteries/posthog.svg",
  },
  {
    name: "PagerDuty",
    description: "Rules for 18 incident, schedule, team, and status page tools; internal reads and every write reviewed.",
    href: "/battery-pagerduty",
    logo: "/images/batteries/pagerduty.svg",
  },
  {
    name: "Hugging Face",
    description: "Rules for 11 account, search, repository, Space, job, and sandbox tools; each repository's Hub visibility decides its readers.",
    href: "/battery-huggingface",
    logo: "/images/batteries/huggingface.svg",
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
                  <span className="battery-card-title">
                    <img
                      alt=""
                      className={`battery-card-logo${battery.name === "GitHub" ? " battery-card-logo-github" : ""}`}
                      height="22"
                      src={battery.logo}
                      width="22"
                    />
                    <strong className="battery-card-name">{battery.name}</strong>
                  </span>
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

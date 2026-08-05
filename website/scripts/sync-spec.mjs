// Copies the repository's normative spec (docs/spec.md) into the website's
// content directory so the MCP server can serve rule lookups. The copy is
// build-local and gitignored — docs/spec.md stays the single source. When the
// spec is absent (e.g. a build context without the repo root), the MCP
// server's lookup_rule degrades gracefully, so this script never fails.
import { copyFileSync, existsSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const websiteRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const source = join(websiteRoot, "..", "docs", "spec.md");
const target = join(websiteRoot, "content", "spec.md");

if (existsSync(source)) {
  mkdirSync(dirname(target), { recursive: true });
  copyFileSync(source, target);
  console.log("sync-spec: copied docs/spec.md -> content/spec.md");
} else {
  console.log("sync-spec: docs/spec.md not found, skipping (lookup_rule will report the spec as unavailable)");
}

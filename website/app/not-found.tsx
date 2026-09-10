import type { Metadata } from "next";
import Link from "next/link";

import { Header } from "@/components/Header";

export const metadata: Metadata = {
  title: "Page Not Found",
  description: "The requested path does not exist or was redacted by policy.",
};

export default function NotFound() {
  return (
    <>
      <Header />
      <main className="error-page" role="main">
        <div className="error-card">
          <div className="error-mascot-wrap">
            <img
              src="/brand/appa-yell.png"
              alt="OpenAPPA mascot sounding the alarm: 404 page not found"
              width={160}
              height={160}
            />
          </div>
          <div className="error-badge">
            <span className="inline-block h-1.5 w-1.5 rounded-full bg-[var(--danger)]" />
            <span>404 · Destination unreachable</span>
          </div>
          <h1 className="error-title">Trajectory halted</h1>
          <p className="error-desc">
            The policy algebra evaluated every trajectory, but no destination matches this path.
            Either this address never existed, or it was redacted by policy.
          </p>
          <div className="error-actions">
            <Link href="/" className="error-btn-primary">
              Return home
            </Link>
          </div>
        </div>
      </main>
      <footer className="site-footer">
        <span>© {new Date().getFullYear()} OpenAPPA</span>
        <span className="site-footer-links">
          <a href="https://discord.gg/B5fmSxHKZ7" target="_blank" rel="noreferrer">
            Discord
          </a>
          <a href="https://github.com/archestra-ai/OpenAPPA" target="_blank" rel="noreferrer">
            GitHub
          </a>
        </span>
      </footer>
    </>
  );
}

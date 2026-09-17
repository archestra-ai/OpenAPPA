"use client";

import { useEffect } from "react";
import Link from "next/link";

import { Header } from "@/components/Header";

export default function ErrorPage({
  error,
  reset,
}: {
  error: Error & { digest?: string };
  reset: () => void;
}) {
  useEffect(() => {
    console.error("Runtime error caught by boundary:", error);
  }, [error]);

  return (
    <>
      <Header />
      <main className="error-page" role="main">
        <div className="error-card">
          <div className="error-mascot-wrap">
            <img
              src="/brand/appa-yell.png"
              alt="OpenAPPA mascot sounding the alarm: execution interrupted"
              width={160}
              height={160}
            />
          </div>
          <div className="error-badge">
            <span className="inline-block h-1.5 w-1.5 rounded-full bg-[var(--danger)]" />
            <span>500 · Runtime exception</span>
          </div>
          <h1 className="error-title">Execution interrupted</h1>
          <p className="error-desc">
            An unexpected exception interrupted this trajectory. The runtime chokepoint caught it
            before state could corrupt.
          </p>
          <div className="error-actions">
            <button type="button" onClick={() => reset()} className="error-btn-primary">
              Retry trajectory
            </button>
            <Link href="/" className="error-btn-secondary">
              Return home
            </Link>
          </div>
          {error?.digest && (
            <details className="error-details">
              <summary>Diagnostic digest</summary>
              <pre>{error.digest}</pre>
            </details>
          )}
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

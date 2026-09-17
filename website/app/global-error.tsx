"use client";

export default function GlobalError({
  reset,
}: {
  error: Error & { digest?: string };
  reset: () => void;
}) {
  return (
    <html lang="en">
      <body
        style={{
          fontFamily: "system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif",
          margin: 0,
          padding: 0,
          background: "hsl(40, 25%, 99%)",
          color: "hsl(30, 8%, 11%)",
        }}
      >
        <main
          style={{
            minHeight: "100vh",
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            justifyContent: "center",
            padding: "4rem 1.5rem",
            textAlign: "center",
          }}
        >
          <div style={{ maxWidth: "32rem", margin: "0 auto" }}>
            <img
              src="/brand/appa-yell.png"
              alt="OpenAPPA mascot sounding the alarm: fatal exception"
              width={160}
              height={160}
              style={{ display: "block", margin: "0 auto 2rem", imageRendering: "pixelated" }}
            />
            <div
              style={{
                display: "inline-block",
                padding: "4px 12px",
                marginBottom: "1rem",
                borderRadius: "9999px",
                fontFamily: "monospace",
                fontSize: "12px",
                background: "hsl(6, 70%, 95%)",
                color: "hsl(4, 60%, 42%)",
              }}
            >
              Fatal exception
            </div>
            <h1 style={{ fontSize: "2rem", margin: "0 0 1rem", letterSpacing: "-0.02em" }}>
              Root context halted
            </h1>
            <p style={{ fontSize: "15px", color: "hsl(32, 4%, 40%)", lineHeight: 1.6, margin: "0 0 2rem" }}>
              The root application context encountered an unrecoverable failure.
            </p>
            <button
              type="button"
              onClick={() => reset()}
              style={{
                display: "inline-flex",
                alignItems: "center",
                justifyContent: "center",
                padding: "10px 20px",
                borderRadius: "8px",
                background: "hsl(151, 55%, 28%)",
                color: "#fff",
                fontFamily: "monospace",
                fontSize: "13px",
                fontWeight: 500,
                border: "none",
                cursor: "pointer",
              }}
            >
              Retry
            </button>
          </div>
        </main>
      </body>
    </html>
  );
}

import type { Metadata } from "next";
import { IBM_Plex_Mono } from "next/font/google";

import "./globals.css";

const plexMono = IBM_Plex_Mono({
  subsets: ["latin"],
  weight: ["400", "500", "600"],
  variable: "--font-plex-mono",
});

export const metadata: Metadata = {
  title: {
    default: "OpenAPPA",
    template: "%s — OpenAPPA",
  },
  description: "An information-flow policy engine for LLM agents.",
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" className={plexMono.variable}>
      <body style={{ fontFamily: "var(--font-plex-mono), ui-monospace, monospace" }}>
        {children}
      </body>
    </html>
  );
}

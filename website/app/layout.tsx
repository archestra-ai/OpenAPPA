import type { Metadata } from "next";
import { IBM_Plex_Mono, IBM_Plex_Sans } from "next/font/google";

import { MobileNavProvider } from "@/components/MobileNav";
import { ReaderPing } from "@/components/ReaderPing";
import { SearchProvider } from "@/components/SearchProvider";
import { THEME_INIT_SCRIPT } from "@/components/ThemeToggle";

import "./globals.css";

const plexMono = IBM_Plex_Mono({
  subsets: ["latin"],
  weight: ["400", "500", "600"],
  variable: "--font-plex-mono",
});

const plexSans = IBM_Plex_Sans({
  subsets: ["latin"],
  weight: ["400", "500", "600"],
  variable: "--font-plex-sans",
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
    <html lang="en" className={`${plexMono.variable} ${plexSans.variable}`} suppressHydrationWarning>
      <head>
        <script dangerouslySetInnerHTML={{ __html: THEME_INIT_SCRIPT }} />
      </head>
      <body style={{ fontFamily: "var(--font-plex-sans), system-ui, sans-serif" }}>
        <MobileNavProvider>
          <SearchProvider>{children}</SearchProvider>
        </MobileNavProvider>
        {/* Mounted at the root so it greets a reader landing on any page, not
            only the home page. Shows once per browser, then never again. */}
        <ReaderPing />
      </body>
    </html>
  );
}

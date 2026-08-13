import { Header } from "@/components/Header";

// The chat fills the viewport below the header — no footer, no page scroll,
// the chat pane scrolls internally — but sits on the site's centred column,
// so its header aligns with every other page's.
export default function ChatLayout({ children }: { children: React.ReactNode }) {
  return (
    <>
      <Header />
      {children}
    </>
  );
}

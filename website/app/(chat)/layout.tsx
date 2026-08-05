import { Header } from "@/components/Header";

// The chat route owns the whole viewport below the header: no footer, no page
// scroll — the chat pane scrolls internally.
export default function ChatLayout({ children }: { children: React.ReactNode }) {
  return (
    <>
      <Header />
      {children}
    </>
  );
}

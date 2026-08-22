import type { Metadata } from "next";

import { DocsSidebar } from "@/components/DocsSidebar";
import { ChatPlayground } from "@/components/playground/ChatPlayground";
import { getDocsByCategory } from "@/lib/docs";

export const metadata: Metadata = {
  title: "Playground",
  description:
    "A live agent on the full OpenAPPA loop: bring an OpenRouter key, edit the policy, watch every tool call get mediated.",
};

export default function ChatPage() {
  const categories = getDocsByCategory().map((category) => ({
    name: category.name,
    docs: category.docs.map(({ slug, title, proposal }) => ({ slug, title, proposal })),
  }));

  return (
    <div className="shell chat-shell">
      <DocsSidebar categories={categories} />
      <main className="chat-main">
        <ChatPlayground />
      </main>
    </div>
  );
}

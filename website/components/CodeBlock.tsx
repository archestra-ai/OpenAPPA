"use client";

import { useRef, useState } from "react";

export function CodeBlock(props: React.HTMLAttributes<HTMLPreElement>) {
  const preRef = useRef<HTMLPreElement>(null);
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    const text = preRef.current?.textContent ?? "";
    await navigator.clipboard.writeText(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };

  return (
    <div className="codeblock">
      <pre ref={preRef} {...props} />
      <button type="button" className={copied ? "copy copied" : "copy"} onClick={copy}>
        {copied ? "copied" : "copy"}
      </button>
    </div>
  );
}

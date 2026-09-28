"use client";

import { useEffect, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import { searchDocs, type SearchIndex, type SearchResult } from "@/lib/search";

/* Fetched once per page load, the first time search opens; every later open
   and keystroke matches against the copy held here. */
let indexPromise: Promise<SearchIndex> | null = null;
function loadIndex(): Promise<SearchIndex> {
  indexPromise ??= fetch("/search-index").then((res) => {
    if (!res.ok) throw new Error(`search index: ${res.status}`);
    return res.json() as Promise<SearchIndex>;
  });
  indexPromise.catch(() => {
    indexPromise = null;
  });
  return indexPromise;
}

export function SearchModal({ isOpen, onClose }: { isOpen: boolean; onClose: () => void }) {
  const [index, setIndex] = useState<SearchIndex | null>(null);
  const [failed, setFailed] = useState(false);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<SearchResult[]>([]);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const router = useRouter();

  useEffect(() => {
    if (!isOpen) return;
    setTimeout(() => inputRef.current?.focus(), 50);
    setQuery("");
    setResults([]);
    setSelectedIndex(0);
    if (index) return;
    let cancelled = false;
    setFailed(false);
    loadIndex().then(
      (loaded) => {
        if (!cancelled) setIndex(loaded);
      },
      () => {
        if (!cancelled) setFailed(true);
      },
    );
    return () => {
      cancelled = true;
    };
  }, [isOpen, index]);

  useEffect(() => {
    setResults(index ? searchDocs(index, query) : []);
    setSelectedIndex(0);
  }, [index, query]);

  useEffect(() => {
    if (!isOpen) return;

    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
      } else if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelectedIndex((prev) => (results.length > 0 ? (prev + 1) % results.length : 0));
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelectedIndex((prev) => (results.length > 0 ? (prev - 1 + results.length) % results.length : 0));
      } else if (e.key === "Enter" && results[selectedIndex]) {
        e.preventDefault();
        router.push(results[selectedIndex].url);
        onClose();
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [isOpen, results, selectedIndex, router, onClose]);

  if (!isOpen) return null;

  return (
    <div
      className="search-overlay"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="search-dialog" role="dialog" aria-modal="true" aria-label="Search documentation">
        <div className="search-input-wrapper">
          <svg className="search-icon" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <circle cx="11" cy="11" r="8" />
            <line x1="21" y1="21" x2="16.65" y2="16.65" />
          </svg>
          <input
            ref={inputRef}
            type="text"
            className="search-input"
            placeholder="Search docs or terms..."
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
          {query && (
            <button type="button" className="search-clear" onClick={() => setQuery("")}>
              ✕
            </button>
          )}
          <span className="search-kbd">ESC</span>
        </div>

        {query.trim() !== "" && (
          <div className="search-results">
            {failed ? (
              <div className="search-empty">Search is unavailable right now.</div>
            ) : !index ? (
              <div className="search-empty">Loading…</div>
            ) : results.length === 0 ? (
              /* `ph-mask` is PostHog's default mask-text class: session replay
                 records this element's text as asterisks. Masking the input
                 itself is not enough — this line echoes what was typed back
                 into the DOM, where it is ordinary page text. */
              <div className="search-empty ph-mask">No results found for &ldquo;{query}&rdquo;</div>
            ) : (
              results.map((result, idx) => (
                <a
                  key={result.id}
                  href={result.url}
                  className={`search-item ${idx === selectedIndex ? "active" : ""}`}
                  onClick={(e) => {
                    e.preventDefault();
                    router.push(result.url);
                    onClose();
                  }}
                  onMouseEnter={() => setSelectedIndex(idx)}
                >
                  <div className="search-item-header">
                    <span className="search-item-title">{result.title}</span>
                    <span className={`search-badge badge-${result.type}`}>{result.type}</span>
                  </div>
                  {result.subtitle && <div className="search-item-subtitle">{result.subtitle}</div>}
                  {result.snippet && <div className="search-item-snippet">{result.snippet}</div>}
                </a>
              ))
            )}
          </div>
        )}
      </div>
    </div>
  );
}

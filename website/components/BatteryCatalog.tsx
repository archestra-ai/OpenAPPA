"use client";

import { createContext, useContext } from "react";
import Link from "next/link";

import type { BatteryCard } from "@/lib/docs";

/* Cards come from the battery pages' frontmatter (lib/docs.ts
   getBatteryCards), handed down by the page through DocContent, so a new
   battery page is listed without editing this file. */
export const BatteryCardsContext = createContext<BatteryCard[]>([]);

export function BatteryCatalog() {
  const batteries = useContext(BatteryCardsContext);
  return (
    <section className="battery-catalog" aria-label="Available OpenAPPA batteries">
      <div className="battery-catalog-grid">
        {batteries.map((battery) => (
          <Link className="battery-card" href={battery.url} key={battery.slug}>
            <span className="battery-card-heading">
              <span className="battery-card-title">
                <img
                  alt=""
                  className={`battery-card-logo${battery.slug === "battery-github" ? " battery-card-logo-github" : ""}`}
                  height="22"
                  src={battery.logo}
                  width="22"
                />
                <strong className="battery-card-name">{battery.name}</strong>
              </span>
              <span className="battery-card-arrow" aria-hidden="true">→</span>
            </span>
            <span className="battery-card-description">{battery.description}</span>
          </Link>
        ))}
        <Link className="battery-card battery-card-add" href="/write-a-battery">
          <span className="battery-card-plus" aria-hidden="true">+</span>
          <strong className="battery-card-name">Add your own</strong>
        </Link>
      </div>
    </section>
  );
}

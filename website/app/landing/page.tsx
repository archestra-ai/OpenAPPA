import type { Metadata } from "next";

import { Landing } from "./Landing";

import "./landing.css";

export const metadata: Metadata = {
  title: "Landing",
};

export default function LandingPage() {
  return <Landing />;
}

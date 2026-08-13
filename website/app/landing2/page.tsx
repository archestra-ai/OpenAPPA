import type { Metadata } from "next";

import { Landing } from "./Landing";

import "./landing.css";

export const metadata: Metadata = {
  title: "Landing 2",
};

export default function Landing2Page() {
  return <Landing />;
}

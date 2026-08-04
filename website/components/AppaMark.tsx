"use client";

import { useEffect } from "react";

import { registerPixelMarks } from "@/app/landing/pixel-marks";

declare module "react" {
  namespace JSX {
    interface IntrinsicElements {
      "appa-mark": React.DetailedHTMLProps<React.HTMLAttributes<HTMLElement>, HTMLElement> & {
        size?: number | string;
      };
    }
  }
}

export function AppaMark({ size }: { size: number }) {
  useEffect(() => {
    registerPixelMarks();
  }, []);

  return <appa-mark size={size} />;
}

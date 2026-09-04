"use client";

import { useCallback, useEffect, useRef, useState } from "react";

import { clamp01, readTheme, type Theme } from "@/components/figures/lib";

/* Interactive figure shell: a canvas whose scene is a pure function of one
   master parameter t ∈ [0,1], scrubbed by a slider or advanced by play.
   Off-screen figures idle (IntersectionObserver + rAF). */

export interface FigureProps {
  draw: (ctx: CanvasRenderingContext2D, t: number, theme: Theme) => void;
  designW: number;
  designH: number;
  durationMs?: number;
}

export function Figure({ draw, designW, designH, durationMs = 16000 }: FigureProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const wrapRef = useRef<HTMLDivElement>(null);
  const tRef = useRef(0);
  const playingRef = useRef(false);
  const visibleRef = useRef(true);
  const lastTickRef = useRef<number | null>(null);
  const [playing, setPlaying] = useState(false);
  const [sliderT, setSliderT] = useState(0);

  const setT = useCallback((value: number) => {
    tRef.current = clamp01(value);
    setSliderT(tRef.current);
  }, []);

  useEffect(() => {
    const canvas = canvasRef.current;
    const wrap = wrapRef.current;
    if (!canvas || !wrap) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    let scale = 1;
    let lastDpr = window.devicePixelRatio || 1;
    let lastCssW = 0;

    const resize = () => {
      const cssW = wrap.clientWidth;
      const dpr = window.devicePixelRatio || 1;
      if (cssW === 0) return;
      lastCssW = cssW;
      lastDpr = dpr;
      scale = cssW / designW;
      canvas.width = Math.round(cssW * dpr);
      canvas.height = Math.round(designH * scale * dpr);
      canvas.style.height = `${designH * scale}px`;
    };
    resize();

    const ro = new ResizeObserver(resize);
    ro.observe(wrap);
    window.addEventListener("resize", resize);

    const io = new IntersectionObserver((entries) => {
      visibleRef.current = entries[0]?.isIntersecting ?? true;
    });
    io.observe(canvas);

    let raf = 0;
    const loop = (now: number) => {
      raf = requestAnimationFrame(loop);
      if (!visibleRef.current) {
        lastTickRef.current = now;
        return;
      }
      if (playingRef.current) {
        const last = lastTickRef.current ?? now;
        // clamp the step so a backgrounded tab doesn't jump on resume
        const dt = Math.min(now - last, 100);
        tRef.current = clamp01(tRef.current + dt / durationMs);
        setSliderT(tRef.current);
        if (tRef.current >= 1) {
          playingRef.current = false;
          setPlaying(false);
        }
      }
      lastTickRef.current = now;

      const dpr = window.devicePixelRatio || 1;
      const cssW = wrap.clientWidth;
      if (dpr !== lastDpr || cssW !== lastCssW) {
        resize();
      }

      ctx.setTransform(scale * lastDpr, 0, 0, scale * lastDpr, 0, 0);
      ctx.clearRect(0, 0, designW, designH);
      draw(ctx, tRef.current, readTheme(canvas));
    };
    raf = requestAnimationFrame(loop);

    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
      io.disconnect();
      window.removeEventListener("resize", resize);
    };
  }, [draw, designW, designH, durationMs]);

  const togglePlay = () => {
    if (!playingRef.current && tRef.current >= 1) setT(0);
    playingRef.current = !playingRef.current;
    setPlaying(playingRef.current);
  };

  return (
    <div className="flow-figure" ref={wrapRef}>
      <canvas ref={canvasRef} style={{ width: "100%", display: "block" }} />
      <div className="flow-controls">
        <button type="button" onClick={togglePlay} aria-label={playing ? "Pause" : "Play"}>
          {playing ? "❚❚" : "▶"}
        </button>
        <input
          type="range"
          min={0}
          max={1}
          step={0.001}
          value={sliderT}
          aria-label="Scrub the animation"
          onChange={(event) => {
            playingRef.current = false;
            setPlaying(false);
            setT(Number(event.target.value));
          }}
        />
      </div>
    </div>
  );
}

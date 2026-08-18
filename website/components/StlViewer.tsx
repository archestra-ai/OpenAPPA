"use client";

import { useEffect, useRef, useState } from "react";

/* The printable mascot, turning. It renders the same STL files the page offers
   for download, so the thing you spin is the thing you print — a converted
   mesh would be free to drift from them.

   three.js is imported inside the effect rather than at module scope: it is by
   far the heaviest thing on this site, and only this one section of one page
   should ever pay for it. */

interface Part {
  href: string;
  /** CSS token the part is painted with, so the model follows the theme. */
  tone: string;
}

interface Model {
  key: string;
  label: string;
  weight: string;
  parts: Part[];
  /** Radians about X, applied to the whole group: stands the mascot up. */
  tilt: number;
  /** Half turn about Z, for the model that ships lying head-down. */
  flip: boolean;
}

/* The showpiece leads. All three stand in the file with the bitmap in XZ,
   except the fast one, which ships lying down ready to slice. */
const MODELS: Model[] = [
  // The two-colour pair, in the two tones the mark itself uses. They are cut
  // from one solid and share an origin, so they are loaded into one group and
  // centred together — centring each on its own would pull them apart.
  {
    key: "two-color",
    label: "Multi-color",
    weight: "1 MB",
    tilt: -Math.PI / 2,
    flip: false,
    parts: [
      { href: "/brand/openappa-multi-color-part-1.stl", tone: "--text-strong" },
      { href: "/brand/openappa-multi-color-part-2.stl", tone: "--text-weak" },
    ],
  },
  // A quarter turn about X brings the face round to meet the camera.
  {
    key: "detailed",
    label: "Detailed",
    weight: "969 KB",
    tilt: -Math.PI / 2,
    flip: false,
    parts: [{ href: "/brand/openappa-full-body.stl", tone: "--text-strong" }],
  },
  // Lying face-up in the file with its head toward -Y: a half turn about Z
  // stands it the right way up without touching the face, and the beast is
  // symmetric left to right so the mirrored X costs nothing.
  {
    key: "fast",
    label: "Fast print",
    weight: "57 KB",
    tilt: 0,
    flip: true,
    parts: [{ href: "/brand/openappa-fast-printing.stl", tone: "--text-strong" }],
  },
];

export function StlViewer() {
  const host = useRef<HTMLDivElement>(null);
  const [model, setModel] = useState(MODELS[0]);
  const [state, setState] = useState<"idle" | "loading" | "ready" | "failed">("idle");
  // The default model is a megabyte and three.js is not small either. Nothing
  // is fetched until the viewer is close enough to the viewport to be worth
  // it; a reader who never scrolls this far pays nothing at all.
  const [near, setNear] = useState(false);

  useEffect(() => {
    const mount = host.current;
    if (!mount || near) return;
    const watch = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) setNear(true);
      },
      { rootMargin: "300px" },
    );
    watch.observe(mount);
    return () => watch.disconnect();
  }, [near]);

  useEffect(() => {
    const mount = host.current;
    if (!mount || !near) return;

    let disposed = false;
    let stop = () => {};
    setState("loading");

    void (async () => {
      const THREE = await import("three");
      const { STLLoader } = await import("three/examples/jsm/loaders/STLLoader.js");
      const { OrbitControls } = await import("three/examples/jsm/controls/OrbitControls.js");
      if (disposed) return;

      // The scene borrows the page's own colours, so the viewer flips with the
      // theme toggle like everything else on this page does.
      const tokens = getComputedStyle(document.documentElement);
      const token = (name: string) => tokens.getPropertyValue(name).trim() || "#202124";

      const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true });
      renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
      mount.appendChild(renderer.domElement);

      const scene = new THREE.Scene();
      const camera = new THREE.PerspectiveCamera(38, 1, 0.1, 2000);

      /* Three lights, none of them white-hot: the model is one or two flat
         colours, so all the shape a viewer reads comes from how the facets
         differ from one another. A key from the front-left, a fill from the
         right to keep the shadow side legible, and enough ambient that the
         dark theme's near-black body does not close up. */
      scene.add(new THREE.AmbientLight(0xffffff, 1.35));
      const key = new THREE.DirectionalLight(0xffffff, 2.4);
      key.position.set(-0.6, 1, 1.2);
      scene.add(key);
      const fill = new THREE.DirectionalLight(0xffffff, 0.9);
      fill.position.set(1.2, -0.4, 0.6);
      scene.add(fill);

      const controls = new OrbitControls(camera, renderer.domElement);
      controls.enableDamping = true;
      controls.enablePan = false;

      // It turns on its own until someone takes hold of it, and never again
      // after that — a model that keeps drifting under the pointer is worse
      // than one that sits still.
      const still = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
      let spinning = !still;
      controls.addEventListener("start", () => {
        spinning = false;
      });

      const loader = new STLLoader();
      const geometries = await Promise.all(model.parts.map((part) => loader.loadAsync(part.href).catch(() => null)));
      if (disposed) return;
      if (geometries.some((geometry) => geometry === null)) {
        setState("failed");
        return;
      }

      const group = new THREE.Group();
      const materials: import("three").MeshStandardMaterial[] = [];

      geometries.forEach((geometry, at) => {
        if (!geometry) return;
        geometry.computeVertexNormals();
        const material = new THREE.MeshStandardMaterial({
          color: new THREE.Color(token(model.parts[at].tone)),
          // Flat shading is not a style choice here: every face of this model
          // is planar, and smoothing across the chamfers would invent curves
          // that the printed part does not have.
          flatShading: true,
          metalness: 0.05,
          roughness: 0.55,
        });
        materials.push(material);
        group.add(new THREE.Mesh(geometry, material));
      });

      // Centre the group, not its members: the two-colour parts are registered
      // to each other and have to stay that way.
      const bounds = new THREE.Box3().setFromObject(group);
      const centre = bounds.getCenter(new THREE.Vector3());
      group.children.forEach((child) => child.position.sub(centre));
      group.rotation.x = model.tilt;
      if (model.flip) group.rotation.z = Math.PI;
      scene.add(group);

      const radius = bounds.getSize(new THREE.Vector3()).length() / 2;
      camera.position.set(0, radius * 0.35, radius * 3.1);
      controls.minDistance = radius * 1.4;
      controls.maxDistance = radius * 6;
      controls.update();

      // The theme toggle swaps a class on <html>; the materials have to follow.
      const paint = () => {
        const now = getComputedStyle(document.documentElement);
        materials.forEach((material, at) => {
          material.color.set(now.getPropertyValue(model.parts[at].tone).trim() || "#202124");
        });
      };
      const themeWatch = new MutationObserver(paint);
      themeWatch.observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });

      const resize = () => {
        const { clientWidth: w, clientHeight: h } = mount;
        if (!w || !h) return;
        renderer.setSize(w, h, false);
        camera.aspect = w / h;
        camera.updateProjectionMatrix();
      };
      resize();
      const sizeWatch = new ResizeObserver(resize);
      sizeWatch.observe(mount);

      // Off-screen, it should cost nothing at all.
      let visible = true;
      const seeWatch = new IntersectionObserver((entries) => {
        visible = entries[0]?.isIntersecting ?? true;
      });
      seeWatch.observe(mount);

      let frame = 0;
      const tick = () => {
        frame = requestAnimationFrame(tick);
        if (!visible) return;
        if (spinning) group.rotation.y += 0.004;
        controls.update();
        renderer.render(scene, camera);
      };
      tick();
      setState("ready");

      stop = () => {
        cancelAnimationFrame(frame);
        themeWatch.disconnect();
        sizeWatch.disconnect();
        seeWatch.disconnect();
        controls.dispose();
        geometries.forEach((geometry) => geometry?.dispose());
        materials.forEach((material) => material.dispose());
        renderer.dispose();
        renderer.domElement.remove();
      };
    })();

    return () => {
      disposed = true;
      stop();
    };
  }, [model, near]);

  return (
    <figure className="brand-viewer">
      <div className="brand-viewer-stage" ref={host}>
        {state !== "ready" && (
          <span className="brand-viewer-state">{state === "failed" ? "could not load the model" : "loading…"}</span>
        )}
      </div>
      <figcaption className="brand-viewer-foot">
        <span className="brand-viewer-switch">
          {MODELS.map((option) => (
            <button
              aria-pressed={option.key === model.key}
              className={option.key === model.key ? "is-on" : undefined}
              key={option.key}
              onClick={() => setModel(option)}
              type="button"
            >
              {option.label}
              <span>{option.weight}</span>
            </button>
          ))}
        </span>
        <span className="brand-viewer-hint">Drag to turn · scroll to zoom</span>
      </figcaption>
    </figure>
  );
}

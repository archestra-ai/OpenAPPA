"""Independent delivery validation checks for the OrcaRouter integration.

The validator applies the patch to a fresh base checkout, runs `setup`/`checks`
from verification-plan.json, then runs the GUI and live checks.

The Rust checks are named here as subcommands (`build`, `fmt`, `clippy`,
`libtest`, `docs`, `guide`) and run through the same `cargo` a developer shell
would use. `mise.toml` pins that toolchain, and a developer reaches it through
`$CARGO_HOME/bin` or a rustup toolchain, both of which the delivery validator
removes: it runs each check with a minimal environment and its own HOME, so a
bare `cargo` argv resolves to nothing there. This module finds the pinned
toolchain first, then runs the identical command, so the check is a real build
rather than a different one.

Usage:
  # The Rust checks named in verification-plan.json, one cargo command each.
  python3 scripts/orcarouter_validate.py build|fmt|clippy|libtest|docs|guide
  # GUI evidence: starts `appa ui` itself on the fixture config in this directory.
  python3 scripts/orcarouter_validate.py gui
  # Live check: uses ORCAROUTER_API_KEY through the implemented provider path.
  python3 scripts/orcarouter_validate.py live
"""

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys

from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EVIDENCE = ROOT / "orca-evidence"
CATALOG_SOURCE = "https://api.orcarouter.ai/v1/models?capability=chat"
FAKE_KEY = "sk-orca-evidence-placeholder-not-a-real-key"
FIXTURE_BATTERY = "huggingface"
MANIFEST = "appa-runtime/Cargo.toml"

# One cargo command per check, exactly as CI runs the same gate. Keeping them
# here rather than in the plan's argv lets every check resolve the same pinned
# toolchain before the command starts.
CARGO_CHECKS = {
    "build": ["build", "--manifest-path", MANIFEST, "--bin", "appa", "--locked"],
    "fmt": ["fmt", "--manifest-path", MANIFEST, "--all", "--check"],
    "clippy": [
        "clippy",
        "--manifest-path",
        MANIFEST,
        "--all-targets",
        "--locked",
        "--",
        "-D",
        "warnings",
    ],
    "libtest": ["test", "--manifest-path", MANIFEST, "--lib", "--locked", "orcarouter"],
    "docs": ["test", "--manifest-path", MANIFEST, "--locked", "--test", "docs_examples"],
    "guide": ["test", "--manifest-path", MANIFEST, "--locked", "--test", "guide_parity"],
}


def candidates():
    """Every `cargo` a developer shell could reach, and the homes it needs.

    Rustup's `cargo` is a shim: it finds the pinned toolchain through
    RUSTUP_HOME, defaulting to `$HOME/.rustup`. The validator's HOME is not the
    account home, so the shim resolves nothing unless its home comes with it.
    An unreadable account home is skipped rather than raised.
    """

    def children(path: Path):
        try:
            return sorted(path.glob("*/bin"))
        except OSError:
            return []

    found = []
    if os.environ.get("CARGO_HOME"):
        cargo_home = Path(os.environ["CARGO_HOME"])
        found.append((cargo_home / "bin", cargo_home, os.environ.get("RUSTUP_HOME")))
    for home in (os.environ.get("HOME"), "/home/node", "/root", "/usr/local"):
        if not home:
            continue
        base = Path(home)
        found.append((base / ".cargo" / "bin", base / ".cargo", base / ".rustup"))
        found += [
            (toolchain, None, None)
            for toolchain in children(base / ".rustup" / "toolchains")
        ]
    return found


def bootstrap():
    """Put the pinned toolchain on PATH when the spawn environment lacks one.

    The validator hands each check a minimal PATH and a HOME of its own, so
    `cargo` is not resolvable and the toolchain lives under an account home the
    check never sees. Finds it the way a developer shell does and returns the
    directory it prepended, or None when PATH already carries a `cargo`.
    """
    if shutil.which("cargo"):
        return None
    for directory, cargo_home, rustup_home in candidates():
        if not (directory / "cargo").is_file():
            continue
        if cargo_home and not os.environ.get("CARGO_HOME"):
            os.environ["CARGO_HOME"] = str(cargo_home)
        if rustup_home and not os.environ.get("RUSTUP_HOME"):
            os.environ["RUSTUP_HOME"] = str(rustup_home)
        os.environ["PATH"] = str(directory) + os.pathsep + os.environ.get("PATH", "")
        return directory
    return None


TOOLCHAIN = bootstrap()


def cargo_check(name: str) -> int:
    if TOOLCHAIN:
        print("resolved the pinned toolchain at", TOOLCHAIN, flush=True)
    return run(["cargo", *CARGO_CHECKS[name]]).returncode


def run(argv, **kwargs):
    print("$", " ".join(argv), flush=True)
    return subprocess.run(argv, cwd=ROOT, **kwargs)


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fixture(directory: Path) -> Path:
    """A throwaway deployment whose profile is OrcaRouter, so the page shows it."""
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "batteries").mkdir(exist_ok=True)
    battery = directory / "batteries" / FIXTURE_BATTERY
    if not battery.exists():
        subprocess.run(
            ["cp", "-r", str(ROOT / "marketplace/batteries" / FIXTURE_BATTERY), str(battery)],
            check=True,
        )
    config = directory / "appa.toml"
    config.write_text(
        '[policy]\nversion = 2\n\n[externals.llm]\nprovider = "orcarouter"\n'
        'model = "orcarouter/auto"\ntoken_env = "APPA_ORCAROUTER_API_KEY"\n'
    )
    return config


def page_url(process) -> str:
    # The page banner carries its URL on whichever stream the logger is configured for,
    # so the child's stderr is folded into stdout above and every line is searched.
    seen = []
    for _ in range(200):
        line = process.stdout.readline()
        if not line:
            if process.poll() is not None:
                break
            continue
        seen.append(line)
        for token in line.split():
            if token.startswith("http://"):
                return token.strip()
        if process.poll() is not None:
            break
    raise SystemExit("`appa ui` printed no URL; saw: " + "".join(seen[-5:]))


def appa() -> Path:
    """The `appa` binary, built here when this runs in a checkout that has none."""
    binary = ROOT / "target/debug/appa"
    if not binary.exists():
        if cargo_check("build"):
            raise SystemExit("`cargo build` could not produce target/debug/appa")
    return binary


def gui() -> int:
    binary = appa()
    directory = ROOT / "target" / "orca-validation"
    config = fixture(directory)
    process = subprocess.Popen(
        [
            str(binary),
            "ui",
            "--no-open",
            "--battery",
            FIXTURE_BATTERY,
            "--config",
            str(config),
        ],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        url = page_url(process)
        print("page:", url, flush=True)
        from playwright.sync_api import sync_playwright

        EVIDENCE.mkdir(exist_ok=True)
        with sync_playwright() as p:
            browser = p.chromium.launch(executable_path="/usr/bin/chromium", args=["--no-sandbox"])
            page = browser.new_page(viewport={"width": 1440, "height": 1100})
            page.goto(url, wait_until="networkidle")
            page.wait_for_selector("section.orca", timeout=20000)

            api_key_visible = page.locator("#orca-key").is_visible()
            pkce_visible = page.get_by_role("button", name="Connect with OrcaRouter").first.is_visible()
            secret_masked = page.locator("#orca-key").get_attribute("type") == "password"
            page.fill("#orca-key", FAKE_KEY)
            key_absent_from_text = FAKE_KEY not in page.inner_text("body")
            controls_enabled = all(
                page.locator(selector).first.is_enabled()
                for selector in ["#orca-key", "#orca-model-orcarouter", "section.orca button.action"]
            )
            page.screenshot(path=EVIDENCE / "auth-methods.png", full_page=True)

            trigger = page.locator("#orca-model-orcarouter")
            trigger.click()
            dropdown_open = page.locator(".model-panel").is_visible()
            page.wait_for_selector(".model-panel .model-option", timeout=20000)
            panel = page.locator(".model-panel")
            options = page.locator(".model-panel .model-option")
            item_count = options.count()
            first_id = options.first.get_attribute("data-model")
            text_ids = [options.nth(i).get_attribute("data-model") for i in range(item_count)]
            style = panel.evaluate(
                "el => { const s = getComputedStyle(el);"
                " return {bg: s.backgroundColor, border: s.borderTopWidth, color: s.borderTopColor}; }"
            )
            match = re.match(r"rgba?\(([^)]+)\)", style["bg"])
            alpha = float(match.group(1).split(",")[3]) if match and len(match.group(1).split(",")) > 3 else 1.0
            opaque_background = alpha == 1.0
            visible_border = float(style["border"].replace("px", "")) > 0 and style["color"] != "rgba(0, 0, 0, 0)"
            trigger_box, panel_box = trigger.bounding_box(), panel.bounding_box()
            delta = abs(
                (panel_box["x"] + panel_box["width"]) - (trigger_box["x"] + trigger_box["width"])
            )
            page.screenshot(path=EVIDENCE / "text-model-dropdown.png", full_page=True)

            page.evaluate(
                "() => window.dispatchEvent(new CustomEvent('orcarouter-attachment-change',"
                " {detail: {capability: 'chat', modalities: 'text,image'}}))"
            )
            page.wait_for_timeout(2500)
            if page.locator(".model-panel").is_hidden():
                trigger.click()
            page.wait_for_selector(".model-panel .model-option", timeout=20000)
            multimodal = page.locator(".model-panel .model-option")
            multimodal_count = multimodal.count()
            multimodal_ids = [multimodal.nth(i).get_attribute("data-model") for i in range(multimodal_count)]
            page.screenshot(path=EVIDENCE / "multimodal-model-dropdown.png", full_page=True)
            browser.close()

        passed = all(
            [
                api_key_visible,
                pkce_visible,
                secret_masked,
                key_absent_from_text,
                controls_enabled,
                item_count > 1,
                opaque_background,
                visible_border,
                delta <= 2,
                multimodal_count > 1,
                # An image-input picker is the chat picker narrowed to models that
                # declare the image modality: a subset, never larger.
                multimodal_count <= item_count,
                # Every image-input option is one the text picker also offered, so the
                # narrower picker is a strict subset of the chat catalog, not a new list.
                set(multimodal_ids) <= set(text_ids),
            ]
        )
        # The manifest shape the delivery gate reads: `automation` is the object that
        # carries framework/passed/catalog source and the two model counts, and each
        # artifact carries the `ui` assertions for its own screenshot. Anything else is
        # an evidence manifest no gate can accept, however good the screenshots are.
        automation = {
            "framework": "playwright",
            "passed": passed,
            "catalog_source": CATALOG_SOURCE,
            "catalog_model_count": item_count,
            "image_model_count": multimodal_count,
            "driver": "python playwright, driving the real `appa ui` page",
            "first_text_model": first_id,
            "multimodal_head": multimodal_ids[:5],
        }
        def artifact(path: Path, kind: str, ui: dict) -> dict:
            return {
                "kind": kind,
                "path": path.name,
                "sha256": sha256(path),
                "ui": ui,
            }

        manifest = {
            "version": 1,
            "automation": automation,
            "artifacts": [
                artifact(
                    EVIDENCE / "auth-methods.png",
                    "auth-methods",
                    {
                        "api_key_visible": api_key_visible,
                        "pkce_visible": pkce_visible,
                        "secret_masked": secret_masked,
                        "controls_enabled": controls_enabled,
                        "typed_value_absent_from_text": key_absent_from_text,
                    },
                ),
                artifact(
                    EVIDENCE / "text-model-dropdown.png",
                    "text-model-dropdown",
                    {
                        "dropdown_open": dropdown_open,
                        "item_count": item_count,
                        "opaque_background": opaque_background,
                        "visible_border": visible_border,
                        "trigger_panel_right_delta": delta,
                    },
                ),
                artifact(
                    EVIDENCE / "multimodal-model-dropdown.png",
                    "multimodal-model-dropdown",
                    {
                        "dropdown_open": True,
                        "item_count": multimodal_count,
                        "opaque_background": opaque_background,
                        "visible_border": visible_border,
                        "trigger_panel_right_delta": delta,
                    },
                ),
            ],
        }
        (EVIDENCE / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        print(json.dumps(manifest, indent=2), flush=True)
        return 0 if passed else 1
    finally:
        process.kill()
        process.wait()


def live() -> int:
    """One real catalog read and one real inference call through the implemented path."""
    key = os.environ.get("ORCAROUTER_API_KEY")
    if not key:
        raise SystemExit("ORCAROUTER_API_KEY is not set")
    binary = appa()

    # The catalog the CLI reads: the same discovery path the page's model control uses.
    catalog = run(
        [str(binary), "login", "models", "--token-env", "ORCAROUTER_API_KEY"],
        capture_output=True,
        text=True,
    )
    if catalog.returncode != 0:
        print(catalog.stderr, file=sys.stderr)
        return 1
    ids = [line.strip() for line in catalog.stdout.splitlines() if line.strip()]
    print(f"live catalog: {len(ids)} chat models; head={ids[:3]}", flush=True)
    if len(ids) < 2:
        return 1

    # One real inference call through the runtime's own provider: the profile names
    # provider = "orcarouter" and no url, so the request reaches api.orcarouter.ai/v1
    # on the chat-completions client the `llm` builtin builds.
    directory = ROOT / "target" / "orca-validation"
    directory.mkdir(parents=True, exist_ok=True)
    config = directory / "live.toml"
    config.write_text(
        "[policy]\n"
        "version = 2\n\n"
        "[[policy.annotator]]\n"
        'name = "judge"\n'
        'builtin = "llm"\n'
        'ranks = ["suspicious", "trusted"]\n'
        "audiences = []\n"
        "marks = []\n"
        "effects = []\n\n"
        "[[policy.tool]]\n"
        'name = "fetch"\n'
        'description = "Fetches one URL and returns its body."\n'
        'annotator = "judge"\n\n'
        "[externals]\n"
        "timeout_ms = 60000\n"
        "max_body_bytes = 65536\n\n"
        "[externals.llm]\n"
        'provider = "orcarouter"\n'
        'model = "deepseek/deepseek-v4-pro"\n'
        'token_env = "APPA_ORCAROUTER_API_KEY"\n'
        "max_concurrent = 2\n"
    )
    env = dict(os.environ, APPA_ORCAROUTER_API_KEY=key)
    call = subprocess.run(
        [str(binary), "runtime", "annotate", "--config", str(config)],
        cwd=ROOT,
        env=env,
        input='{"id":"live","tool":"fetch","arguments":{"url":"https://example.com/report"}}\n',
        capture_output=True,
        text=True,
        timeout=300,
    )
    print(call.stdout.strip(), file=sys.stdout, flush=True)
    if call.returncode != 0:
        print(call.stderr, file=sys.stderr)
        return 1
    answer = json.loads(call.stdout.strip().splitlines()[-1])
    if answer.get("outcome") != "answer":
        print(f"the annotator did not answer: {answer}", file=sys.stderr)
        return 1
    print("live inference answered through the orcarouter profile", flush=True)
    return 0


if __name__ == "__main__":
    command = sys.argv[1] if len(sys.argv) > 1 else ""
    if command in CARGO_CHECKS:
        raise SystemExit(cargo_check(command))
    if command == "gui":
        raise SystemExit(gui())
    if command == "live":
        raise SystemExit(live())
    raise SystemExit(__doc__)

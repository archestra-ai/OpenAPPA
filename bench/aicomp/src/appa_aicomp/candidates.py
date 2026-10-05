"""Regenerate the replayable attack candidates from published Kaggle solutions.

No model is called. Every solution below is either a static emitter or an adaptive attack whose
candidate content is a deterministic per-model pool, read by importing the solution's own module
and calling its own builders; its attack logic is never edited.

    uv run appa-aicomp-candidates fetch --into sources
    uv run appa-aicomp-candidates extract --sources sources --out candidates
"""

import argparse
import ast
import hashlib
import importlib.util
import json
import logging
import subprocess
import sys
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from pathlib import Path
from types import ModuleType

logger = logging.getLogger(__name__)

GPT, GEMMA = MODELS = ("openai_gpt-oss-20b", "google_gemma-4-26b-a4b-it")
MAX_CHARS, MAX_MSGS, MAX_FINDINGS = 2000, 32, 2000

# The Kaggle API refuses versioned pulls of other users' kernels, so kernels are pinned by the
# sha256 of the notebook instead; a changed upstream notebook fails the fetch.
KERNELS = {
    "xiaoz259/aas-final-submission-cd": "37727f091999fe1a0a2c8834b4f7db1c4fcc01169e360b034c4d0417521da371",
    "huanligong/jed-attack-public-private": "485ebb3062aaabaee9e867dfe4e24eede9188ba69910b1c08fc456002d536b13",
    "konbu17/jed-attack-exp093": "6ffccae05bb54265ef0ea72b750ccb577b4437cfc6e140a8086671ba713fb306",
    "outliar/agi-deputy-nb1-v11b-gpt2000-gem2000-sub": "3a731824bfd007d1045d9136e7f0efd0db173df214144b4c80c46745c9d66a10",
    "tetsutani/ai-agent-sec-adaptive-uniform-two-probe-recovery": "54b7d943dc717ea9c357d954dd3ac3f1f4d6bf3eae9d43b65092f25913666bb1",
}
REPOS = {
    "p4_tomokazu": ("https://github.com/tomokazu-rikioka/kaggle_ai_agent_security.git", "4c7472a4166bdc32df0aa703aaae33005bbc6ef7"),
    "p9_simon": ("https://github.com/simonrueba/ai-agent-security-9th-place.git", "4ddfb51f086207c631980cfc1b65bba9510f5026"),
    "okpeyemi": ("https://github.com/Okpeyemi/ai-agent-security-attacks.git", "2d8166e9839c07fa7f9599647d542f4c28a3cadc"),
    "larry_decoy": ("https://github.com/LarryLin666666/ai-agent-security-decoy-metric.git", "d69b0bea3a63091aec7f9fa1c4b57f4b5fd6d430"),
    "autosecage": ("https://github.com/The-Adimension/AutoSecAge.git", "3c4d4e639891d95743f7faf3449388e7af5e4f3b"),
}
# Published solutions without replayable candidates, kept here so the survey stays complete:
# codesrepo/kaggle-ai-agent-security (harness only), knightynite/jed-working-note (evidence only),
# xz259/Kaggle-AI-Agent-Security-1st-Place-Solution (GCG search tool; the scored submission is
# the xiaoz259 notebook).


def kernel_dir(root: Path, ref: str) -> Path:
    return root / "kernels" / ref.replace("/", "_")


def written_files(source: str) -> dict[str, str]:
    """Files a notebook cell writes: a `%%writefile` cell, or `path.write_text(text)` with
    `text = "<literal>"` and `path = <dir> / "<name>"` assigned in the same cell."""
    first, _, body = source.partition("\n")
    if first.strip().startswith("%%writefile"):
        return {Path(first.split()[-1]).name: body}
    try:
        tree = ast.parse(source)
    except SyntaxError:
        return {}
    texts: dict[str, str] = {}
    paths: dict[str, str] = {}
    for node in tree.body:
        match node:
            case ast.Assign(targets=[ast.Name(id=name)], value=ast.Constant(value=str() as text)):
                texts[name] = text
            case ast.Assign(targets=[ast.Name(id=name)], value=ast.BinOp(op=ast.Div(), right=ast.Constant(value=str() as filename))):
                paths[name] = filename
    files: dict[str, str] = {}
    for node in ast.walk(tree):
        match node:
            case ast.Call(func=ast.Attribute(value=ast.Name(id=path), attr="write_text"), args=[ast.Name(id=text), *_]) if (
                path in paths and text in texts
            ):
                files[Path(paths[path]).name] = texts[text]
    return files


def fetch(into: Path) -> None:
    for ref, digest in KERNELS.items():
        target = kernel_dir(into, ref)
        subprocess.run(["uvx", "kaggle", "kernels", "pull", ref, "-p", str(target)], check=True, capture_output=True)
        notebook = target / f"{ref.split('/')[1]}.ipynb"
        if (actual := hashlib.sha256(notebook.read_bytes()).hexdigest()) != digest:
            raise SystemExit(f"{ref}: notebook sha256 {actual} != pinned {digest}")
        for cell in json.loads(notebook.read_text())["cells"]:
            source = "".join(cell["source"]) if isinstance(cell["source"], list) else cell["source"]
            for filename, body in written_files(source).items():
                (target / filename).write_text(body)
        logger.info("pulled %s", ref)
    for name, (url, sha) in REPOS.items():
        target = into / "repos" / name
        target.mkdir(parents=True, exist_ok=True)
        for args in (["init", "-q"], ["fetch", "-q", "--depth", "1", url, sha], ["checkout", "-q", "FETCH_HEAD"]):
            subprocess.run(["git", *args], cwd=target, check=True)
        logger.info("fetched %s@%s", url, sha)


def load(path: Path) -> ModuleType:
    name = f"appa_aicomp_solution_{len(sys.modules)}"
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None, path
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def messages(candidates: Sequence) -> list[list[str]]:
    return [[str(m) for m in tuple(c.user_messages)] for c in candidates]


def singles(texts: Sequence[str]) -> list[list[str]]:
    return [[t] for t in texts]


@dataclass(frozen=True)
class Solution:
    name: str
    rank: int | None
    method: str
    build: Callable[[Path], dict[str, list[list[str]]]]


def both(candidates: list[list[str]]) -> dict[str, list[list[str]]]:
    return {GPT: candidates, GEMMA: candidates}


def xiaoz(root: Path) -> dict[str, list[list[str]]]:
    mod = load(kernel_dir(root, "xiaoz259/aas-final-submission-cd") / "attack.py")
    # Shim: the notebook's oss/gemma_cpu_survivors.json banks are not public; p4_tomokazu's verbatim
    # copy of the first-place gemma bank is the only surviving one, so it feeds BOTH models.
    bank = json.loads((root / "repos/p4_tomokazu/benchmarks/scripts/article_speed/first_place_gemma_recipients.json").read_text())
    assert len(bank) == 2000 and bank[0] == "John"
    # run() also does a live KV warm-up that does not alter candidate content; skipped.
    return {
        GPT: messages(mod._build_static_portfolio(mod._make_oss_prompt, bank)),
        GEMMA: messages(mod._build_static_portfolio(mod._make_gemma_prompt, bank)),
    }


def huanligong(root: Path) -> dict[str, list[list[str]]]:
    mod = load(kernel_dir(root, "huanligong/jed-attack-public-private") / "attack.py")

    # The non-online pool: prefix copies, then template+recipient; online mode only reorders/filters it.
    def pool(kind: str) -> list[list[str]]:
        texts = [mod.PREFIX_TEMPLATES[kind]] * mod.PREFIX_COUNTS[kind]
        texts += [mod.TEMPLATES[kind] + t for t in mod.RECIPIENTS[kind]]
        return singles(texts[: mod.MAX_CANDIDATES])

    return {GPT: pool("gpt"), GEMMA: pool("gemma")}


def p4(root: Path) -> dict[str, list[list[str]]]:
    # exp025 is the final submission; run() only detects the model and picks the per-model bank.
    mod = load(root / "repos/p4_tomokazu/experiments/exp025/attack.py")
    return {
        GPT: singles([mod._message_gpt(i) for i in range(mod.GPT_CANDIDATES)]),
        GEMMA: singles([mod._message_gemma(i) for i in range(mod.GEMMA_CANDIDATES)]),
    }


def p9(root: Path) -> dict[str, list[list[str]]]:
    mod = load(root / "repos/p9_simon/deputy_attack.py")
    assert mod.HEDGE_FRAC == 1.0  # default: every candidate is a deputy message
    # The live run auto-sizes N <= N_CAP; we emit the full pool. inject framing only for gpt-oss.
    return {
        GPT: singles([mod._deputy_msg(i, True) for i in range(mod.N_CAP)]),
        GEMMA: singles([mod._deputy_msg(i, False) for i in range(mod.N_CAP)]),
    }


def okpeyemi(root: Path) -> dict[str, list[list[str]]]:
    # Default family "exfil", model-agnostic; the live fill only filters to candidates that fire.
    mod = load(root / "repos/okpeyemi/attack.py")
    return both(messages(mod._emit(mod.HARD_N_CAP, "exfil")))


def larry(root: Path) -> dict[str, list[list[str]]]:
    # Default latency split: slow model (gpt-oss) gets the forged FRAME_TEMPLATE, fast (gemma) TEMPLATE.
    mod = load(root / "repos/larry_decoy/attack.py")
    return {
        GPT: singles([mod._msg(i, mod.FRAME_TEMPLATE) for i in range(mod.HARD_N_CAP)]),
        GEMMA: singles([mod._msg(i, mod.TEMPLATE) for i in range(mod.HARD_N_CAP)]),
    }


def autosecage(root: Path) -> dict[str, list[list[str]]]:
    # Static catalog (env_probe=False): run(None, None) is deterministic and model-agnostic.
    mod = load(root / "repos/autosecage/attack.py")
    return both(messages(mod.AttackAlgorithm().run(None, None)))


def konbu17(root: Path) -> dict[str, list[list[str]]]:
    # The live probe picks one literal prompt per model and repeats it.
    mod = load(kernel_dir(root, "konbu17/jed-attack-exp093") / "attack.py")
    return {GPT: messages(mod._emit_literal(mod.GPTOSS_PROMPT)), GEMMA: messages(mod._emit_literal(mod.GEMMA_PROMPT))}


def outliar(root: Path) -> dict[str, list[list[str]]]:
    mod = load(kernel_dir(root, "outliar/agi-deputy-nb1-v11b-gpt2000-gem2000-sub") / "attack.py")
    return {GPT: messages(mod._build(mod.N_GPT, mod._msg_gpt)), GEMMA: messages(mod._build(mod.N_GEMMA, mod._msg_gem))}


def tetsutani(root: Path) -> dict[str, list[list[str]]]:
    # Shim: the live run races 5 templates and fills with the cheapest-firing one, which depends on
    # the live model; we take the plain fallback template for both models.
    mod = load(kernel_dir(root, "tetsutani/ai-agent-sec-adaptive-uniform-two-probe-recovery") / "attack.py")
    template = mod.TEMPLATES[mod.FALLBACK_INDEX][1]
    return both(messages([mod._candidate(template, i) for i in range(mod.MAX_CANDIDATES)]))


SOLUTIONS = (
    Solution("p1_xiaoz259", 1, "extracted", xiaoz),
    Solution("p2_huanligong", 2, "extracted", huanligong),
    Solution("p4_tomokazu", 4, "extracted", p4),
    Solution("p9_simon", 9, "extracted", p9),
    Solution("okpeyemi", None, "extracted", okpeyemi),
    Solution("larry_decoy", None, "extracted", larry),
    Solution("autosecage", None, "run", autosecage),
    Solution("konbu17", None, "extracted", konbu17),
    Solution("outliar", None, "extracted", outliar),
    Solution("tetsutani", None, "extracted", tetsutani),
)


def valid(candidate: list[str]) -> bool:
    return 0 < len(candidate) <= MAX_MSGS and all(0 < len(m) <= MAX_CHARS for m in candidate)


def extract(sources: Path, out: Path) -> None:
    rows: dict[str, list[dict]] = {m: [] for m in MODELS}
    for solution in SOLUTIONS:
        for model, candidates in solution.build(sources).items():
            kept = [c for c in candidates if valid(c)][:MAX_FINDINGS]
            logger.info("%s %s: kept %d of %d", solution.name, model, len(kept), len(candidates))
            rows[model] += [
                {"solution": solution.name, "rank": solution.rank, "index": i, "user_messages": c, "method": solution.method}
                for i, c in enumerate(kept)
            ]
    out.mkdir(parents=True, exist_ok=True)
    for model, model_rows in rows.items():
        (out / f"{model}.jsonl").write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in model_rows))
        logger.info("wrote %s (%d rows)", out / f"{model}.jsonl", len(model_rows))


def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(message)s")
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("fetch", help="pull the pinned Kaggle notebooks and GitHub repos").add_argument("--into", type=Path, required=True)
    extract_parser = commands.add_parser("extract", help="write <model>.jsonl candidates from fetched sources")
    extract_parser.add_argument("--sources", type=Path, required=True)
    extract_parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    match args.command:
        case "fetch":
            fetch(args.into)
        case "extract":
            extract(args.sources, args.out)


if __name__ == "__main__":
    main()

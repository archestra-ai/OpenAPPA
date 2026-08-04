import os
from pathlib import Path

HARNESS_ROOT = Path(__file__).resolve().parents[1]
os.environ.setdefault("TAU2_DATA_DIR", str(HARNESS_ROOT / ".tau2-bench" / "data"))

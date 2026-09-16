import os
from pathlib import Path

import pytest

HARNESS_ROOT = Path(__file__).resolve().parents[1]
TAU2_DATA_DIR = Path(os.environ.setdefault("TAU2_DATA_DIR", str(HARNESS_ROOT / ".tau2-bench" / "data")))


@pytest.fixture
def tau_checkout() -> None:
    """Skip a test that needs Tau's data when the pinned checkout is absent.

    Tau keeps its benchmark data in a separate repository, so tests that read
    the task inventory or replay reference actions need the checkout the setup
    script clones. The rest — the pinned policy, the native session, the
    reporting — read no Tau data and run anywhere.
    """
    if not TAU2_DATA_DIR.is_dir():
        pytest.skip("the pinned Tau checkout is absent; run ./setup-taubench.sh")

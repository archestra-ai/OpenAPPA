"""corporate-agent-fides: the OpenAPPA corporate demo, defended by Microsoft
Agent Framework's FIDES instead of OpenAPPA's own policy engine.

Same corpus, same planted prompt injection, same tool surface as the sibling
Rust ``corporate-agent`` demo — the only variable is the defense. Use it to
read FIDES's integrity/confidentiality label model against OpenAPPA's
trust/audience algebra on an identical scenario.
"""

from .agent import BuiltAgent, build_agent
from .systems import resolve_corpus_root, resolve_sink_root

__all__ = ["BuiltAgent", "build_agent", "resolve_corpus_root", "resolve_sink_root"]

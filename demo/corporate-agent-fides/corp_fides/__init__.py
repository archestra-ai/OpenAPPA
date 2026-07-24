"""corporate-agent-fides: the OpenAPPA corporate demo, defended by Microsoft
Agent Framework's FIDES instead of OpenAPPA's own policy engine.

Both demos spawn the *same* shared ``corp-systems-mcp`` server (the sibling
``corp-systems`` crate) over the *same* corpus and planted prompt injection —
the only variable is the defense. Use it to read FIDES's
integrity/confidentiality label model against OpenAPPA's trust/audience
algebra on an identical scenario.
"""

from .agent import BuiltAgent, build_agent
from .systems import CorpSystemsClient, resolve_corpus_root, resolve_sink_root

__all__ = ["BuiltAgent", "CorpSystemsClient", "build_agent", "resolve_corpus_root", "resolve_sink_root"]

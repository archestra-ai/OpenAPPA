"""corporate-agent-fides: the OpenAPPA corporate demo, defended by Microsoft
Agent Framework's FIDES instead of OpenAPPA's own policy engine.

This demo spawns the shared ``corp-systems-mcp`` server (the sibling
``corp-systems`` crate); the APPA demo runs that same crate's systems
in-process. Both act over the *same* corpus and planted prompt injection — the
only variable is the defense. Use it to read FIDES's integrity/confidentiality
label model against OpenAPPA's trust/audience algebra on an identical scenario.
"""

from .agent import BuiltAgent, build_agent
from .profile import DEFAULT_PROFILE, Profile, ProfileError, load_profile
from .systems import CorpSystemsClient, resolve_corpus_root, resolve_sink_root

__all__ = [
    "DEFAULT_PROFILE",
    "BuiltAgent",
    "CorpSystemsClient",
    "Profile",
    "ProfileError",
    "build_agent",
    "load_profile",
    "resolve_corpus_root",
    "resolve_sink_root",
]

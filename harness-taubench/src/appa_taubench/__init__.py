"""OpenAPPA evaluation harness for Tau Knowledge."""

SUPPORTED_RETRIEVAL_CONFIGS = ("alltools", "alltools-qwen")
AGENT_PROMPT_PROFILES = {
    "standard": "",
    "verification-recovery-chaos": (
        "Work quickly and propose the requested account action as soon as its target and arguments are known. "
        "If a tool or policy refuses that action and names a prerequisite, use only available tools and factual "
        "returned values to satisfy that prerequisite, then retry the original action once. Never invent identity, "
        "timestamps, account IDs, verification evidence, or unavailable tool names."
    ),
}

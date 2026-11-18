"""Count the words the user reads in simulated transcripts: the text of `### Claude`
turns, without the italic command notes such as *(runs appa describe)*.

    python3 bench/appa-guide/word_count.py runs/*/transcript.md
"""

import re
import sys

COMMAND_NOTE = re.compile(r"^\s*\*\(.*\)\*\s*$")

for path in sys.argv[1:]:
    with open(path, encoding="utf-8") as f:
        turns = re.split(r"^### (User|Claude)\s*$", f.read(), flags=re.M)
    replies = [turns[i + 1] for i in range(1, len(turns) - 1, 2) if turns[i] == "Claude"]
    words = [
        len(" ".join(l for l in reply.splitlines() if not COMMAND_NOTE.match(l)).split())
        for reply in replies
    ]
    first = words[0] if words else 0
    print(f"{path}: total={sum(words)} first_reply={first} replies={len(words)}")

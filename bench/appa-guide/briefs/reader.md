Call ReadAs (the `read_as` writing-review tool) once per S1 transcript and once more for the held-out quiz:

- persona: the exact text of `personas/s1-quiz.txt` (save the response as `reader.md`) or `personas/heldout-quiz.txt` (save it as `heldout.md`).
- draft: the preamble below, followed by the exact transcript.

```text
You are reading a Claude Code chat. You ran clappa, then typed /appa-guide. Lines in italics in parentheses are commands the assistant ran; you see them only as collapsed tool calls.

---

```

Check that the persona and draft you send match the files byte for byte (length and a hash), and save the response verbatim. Backticks inside a JavaScript template literal must reach ReadAs unescaped.

# Test the Hugging Face battery against the Hub

This replay runs the shipped Hugging Face battery against real
repositories. The battery's annotators call the Hub API and mark each
repository's content `suspicious`; a public repository's content keeps a
public audience, a gated repository's metadata too, and a search or a
private repository narrows the audience to the viewer, the token's own
reader.

The folder contains:

- `appa.toml`: the replay configuration, including the battery and two
  tools that make the trust and audience changes observable; and
- `huggingface-battery.appa`: the replay trace.

The trace names public repositories, so any read token replays it. The
last block, commented out, reads a private repository: name one the
token can read and uncomment it. Then run:

```sh
APPA_PROVIDER_HUGGINGFACE_TOKEN=... appa replay \
  --config examples/live-replays/huggingface/appa.toml \
  examples/live-replays/huggingface/huggingface-battery.appa
```

The replay only asks the Hub for visibility; it writes nothing to any
repository.

The battery's unit tests need no token:

```sh
python3 -m unittest discover -s marketplace/batteries/huggingface -p 'test_*.py'
```

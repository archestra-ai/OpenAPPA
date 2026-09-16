# mask-before-sharing

The session starts at `internal`. A command that reads a credential file narrows it to
`self`. The command's output enters the trajectory only after the masker moves it back to
`internal`, so the session keeps its label.

No audience source is bound. `self` and `internal` are levels of the built-in chain, so
the masker applies to the narrowed value without a member list.

- `mask-before-sharing.appa`: a plain command is allowed. A `cat .env` goes through the
  masker. An internal email after it is allowed.

The example uses a stand-in for the masker. It does not change the output text.

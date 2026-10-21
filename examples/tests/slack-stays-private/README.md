# slack-stays-private

Reading a Slack channel limits the content to the channel's members:
`delta = { audience = ["@slack:channel/$channel_id"] }`.

The policy names no tool that sends anything. The trace checks the restriction with
`appa/do_something`, a call that only `appa replay` serves. Its `requires` argument takes
the same fields as a tool contract's `requires`, and it never changes the trajectory.

- `slack-stays-private.appa`: content can reach the public before the read. The read is
  allowed. After it, content cannot reach the public.

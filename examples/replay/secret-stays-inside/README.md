# secret-stays-inside

Reading a file under `/hr/` restricts the trajectory to the organization:
`delta = { audience = ["internal"] }`. `internal` is one of the three built-in audiences,
`self ⊆ internal ⊆ public`. Two sinks read the audience. `Email` leaves the company and
requires `public`. `PostToChannel` reaches the organization and requires `internal`, which
a public trajectory also satisfies.

- `email-before-read.appa`: nothing restricted was read, the trajectory is still public,
  and the email is allowed.
- `leak-after-hr-read.appa`: the read is allowed after the model accepts the narrowing.
  The email is denied: the trajectory no longer reaches `public`. The channel post is
  allowed.
- `public-read-then-email.appa`: a file outside `/hr/` selects the bare `Read` contract,
  whose `delta = {}` restricts nothing, so the email still leaves.

The policy declares no `[audience.internal]` source. `internal` then stays a symbol:
labels compare against it, and no real address is inside it. A deployment that binds a
source, for example `from = ["slack:full-members"]`, gives it members, and a recipient
written as an address can then be found inside it.

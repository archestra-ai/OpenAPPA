# secret-stays-inside

Reading a file under `/hr/` makes the trajectory private: `delta = { audience = ["private"] }`.
Two sinks read the audience. `Email` leaves the company and requires `public`.
`PostToChannel` stays inside and requires only `private`, which a public trajectory
also satisfies.

- `email-before-read.appa`: nothing private was read, the trajectory is still public,
  and the email is allowed.
- `leak-after-hr-read.appa`: the read is allowed after the model accepts the narrowing.
  The email is denied: the trajectory no longer reaches `public`. The channel post is
  allowed.
- `public-read-then-email.appa`: a file outside `/hr/` selects the bare `Read` contract,
  whose `delta = {}` restricts nothing, so the email still leaves.

`private` is a plain reader name here, the spelling the shipped batteries use. The
built-in chain `self ⊆ internal ⊆ public` compares real recipients against a group
source; this example needs none.

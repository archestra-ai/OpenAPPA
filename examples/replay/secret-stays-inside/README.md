# secret-stays-inside

Reading a file under `/hr/` narrows the trajectory's audience to `hr`. From then on an
email may go only to a recipient inside that audience. The `Email` contract reads its
recipient from the call: `requires = { audience = { contains = ["$to"] } }`.

- `email-before-read.appa`: nothing sensitive was read, the audience is still public,
  and `x@other.com` is allowed.
- `leak-after-hr-read.appa`: the read is allowed after the model accepts the narrowing.
  The email to `x@other.com` is denied: that reader is not in `hr`. The email to `hr`
  is allowed.
- `public-read-then-email.appa`: a file outside `/hr/` selects the bare `Read` contract,
  whose `delta = {}` restricts nothing, so the email still goes anywhere.

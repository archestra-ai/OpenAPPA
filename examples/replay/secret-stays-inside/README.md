# secret-stays-inside

Reading a file under `/hr/` limits the data to `hr`. After this read, an email can go
only to a person in `hr`.

`Email` uses the `to` value from the email. It checks that the recipient can read the
data: `requires = { audience = { contains = ["$to"] } }`.

- `secret-stays-inside.appa`: an email to `x@other.com` is allowed before the read. The HR
  read is allowed. An email to `x@other.com` is denied. An email to `hr` is allowed.

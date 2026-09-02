# redact-before-sending

HR data may leave the company only through the redactor. `Read(path:/hr/*)` narrows the
audience to `hr`. `Email` requires the `public` audience. The `redactor` sanitizer is
declared on `tool_input` with `permits.audience = { from = ["hr"], to = ["public"] }`: it
rewrites the email's arguments before dispatch and moves the value to `public`.

- `hr-then-email.appa`: after the HR read, the email is blocked and the offer names the
  redactor, so the step is `expect sanitizer`. The replay stands in for the sanitizer,
  the email runs with its arguments rewritten, and the trajectory stays at `hr`. The
  second email needs the redactor again.
- `email-without-hr.appa`: a read outside `/hr/` restricts nothing, and the email runs
  as is.

The deployment binds the redactor to the shipped `redact-email` builtin. The replay does
not call it: its stand-in returns the arguments unchanged, and the engine records the
pass through the sanitizer the same way.

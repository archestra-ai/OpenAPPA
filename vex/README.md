# VEX

`openvex.json` says which reported vulnerabilities do not affect this
project's release images (`appa-runtime`, `appa-kagent-adk`, `appa-kagent-adk-go`, `golang-adk`, `appa-demo-tools`, `appa-demo-mocks`) as shipped, and why. It is an
[OpenVEX](https://github.com/openvex/spec) document: scanners such as Grype and
Trivy read it and drop the findings it covers.

Each release attests every image's statements, pinned to its digest, next to
its SBOMs (archestra-ai/.github `release-publish.yml`, `vex-document`). The
**VEX Triage** agent, started weekly from archestra-ai/archestra
(`vex-triage.yml`), opens or updates the pull request from
`automation/vex-triage` with its evidence. A person checks the evidence and
merges it: a statement is a claim we sign.

The rules (statuses, justifications, one status per vulnerability, image and
package) and the tool to write a statement by hand are in archestra-ai/archestra
[`vex/README.md`](https://github.com/archestra-ai/archestra/blob/main/vex/README.md).
The **VEX Document** check validates this file on every pull request.

# AI policy

This stack was written by AI agents, for AI-forward developers.

That sentence is a statement of fact, not a disclaimer. Vogt's code,
documentation, and tests were produced by AI coding agents working under the
direction and review of the human maintainer, who is accountable for every
line the repository ships. It is also a statement of intent: Vogt exists to
run product work that people and agents do together, so agent surfaces —
MCP, the operation registry, `docs/AGENT_GUIDE.md`, terminal sessions an
agent can drive — are first-class product, not integrations bolted onto a
human tool.

## What that means for using Vogt

- The documentation is written to be read by people and agents alike.
  `AGENTS.md` is the repository's working contract for agents;
  `docs/AGENT_GUIDE.md` is the guide for agents running work *through* a
  deployed Vogt.
- Transport parity is a design rule: anything the CLI or REST surface can
  do, MCP can do, with tests asserting they agree. An agent is never a
  second-class client.

## What that means for contributing

AI-assisted and AI-authored contributions are welcome and expected — most of
this repository was built that way. The bar does not move either direction
because an agent was involved:

- **You are accountable for what you submit.** Run the checks in
  `docs/CONTRIBUTING.md`, understand the change, and be able to answer
  review questions about it. "The agent wrote it" is not an answer.
- **Unreviewed agent output is not a contribution.** Bulk or speculative
  pull requests, issues generated without reading the code, and changes
  their author cannot explain will be closed without detailed review.
- **The same quality rules apply.** Tests, transport parity, the audited
  write path, and the documented layer order bind agents and people
  equally.
- **You must hold the rights to what you submit.** Contributions are
  accepted under the repository's MIT licence; submitting output you are
  not entitled to license is your responsibility, not the project's.

Disclosure of AI involvement in a contribution is not required. It is the
default assumption here.

## What that means for maintenance

Agents triage, review, and land work in this repository as a matter of
routine. Decisions about scope, releases, and security remain with the human
maintainer, and security reports go to the channel named in `SECURITY.md` —
not to an automated surface.

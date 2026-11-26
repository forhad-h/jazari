# jazari rubrics

The evaluator judges each finished task against its role's rubric. Each criterion is pass or fail,
and the evaluator cites evidence for it:
- the diff (`git diff <base>...HEAD`);
- the files;
- the harness's check output.

A criterion marked `[critical]` must pass. The task passes when:
- every critical criterion passes;
- at least 80% of all criteria pass;
- the overall score is 4 or 5.

Overall score anchors:
- 5: ready to ship, nothing to add;
- 4: ready, small nits only;
- 3: works, but has a real gap;
- 2: important parts are missing or wrong;
- 1: off-task or broken.

## all
- [critical] Done as asked: every acceptance criterion in the task's spec is met. List each one with its evidence.
- [critical] Claims match evidence: what the worker's summary claims (tests pass, a feature works) is backed by the diff or by the harness's check output.
- [critical] Scope: the change does the task, and nothing unrelated was changed.
- [critical] No secrets, credentials or personal data were committed.
- Not a stub: no placeholder, TODO or fake data is passed off as finished work.
- Memory kept: decisions that others need are written where the team reads them (an ADR, the README, `company/`).

## code
- [critical] Tests prove the behavior: new or changed behavior has tests that would fail without the change, and the harness checks pass.
- [critical] Security: input is validated at boundaries, authorization is checked, and queries are parameterized. There is no injection, path traversal, unsafe deserialization or secret in code.
- Correctness: edge cases and error paths are handled, and errors don't leak internals.
- Fits the design: it follows the ADRs, the repo's patterns and the layering. New dependencies are pinned and justified.
- Maintainable: clear names, small functions, and no dead or duplicated code.

## plan
- [critical] The vision names the problem, the users, measurable success and non-goals.
- [critical] The first milestone is small, shippable and testable on its own.
- [critical] The tasks are small and in build order, each with the right role and acceptance criteria in its brief.
- [critical] The threat model lists assets, trust boundaries and STRIDE threats, each with a mitigation.
- Architecture decisions are recorded as ADRs, with the alternatives considered.
- Assumptions are written down, not hidden.

## design
- [critical] Accessible: it meets WCAG 2.2 AA (contrast, labels, focus, keyboard use).
- [critical] Usable: it follows the usability heuristics (status visible, user in control, error prevention and recovery, consistency).
- Consistent with the design system: tokens and components are used, not one-off styles.
- Every state is designed: empty, loading, error and success.
- Responsive from phone width up.

## docs
- [critical] Accurate: it describes the product as it is now, and every command and example works.
- Plain language, for the stated reader.
- Claims from outside sources are cited.
- Findable: it is linked from the README or the relevant `company/` index.

## security
- [critical] Findings are proven or evidenced (an exploit, a test or a code reference), not guessed.
- [critical] Each fix has a regression test that fails before the fix.
- Severity is rated and mapped to OWASP or CWE.
- The threat model and risk register are updated.

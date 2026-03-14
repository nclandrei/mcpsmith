# mcpsmith Next Steps

This is the only active coordination file for follow-up work in this repo.
Keep the tool simple. Do not add systems, agents, or abstractions unless one
of the tasks below clearly needs them.

## Working rules

- One task per agent/change whenever possible.
- Prefer deletion and simplification over new framework code.
- Keep `mcpsmith` non-interactive and easy to chain from another agent.
- If a change does not unblock a real MCP or remove real complexity, do not do it.

## 1. Remove dead legacy flow

Status:
- Done on 2026-03-12.

Goal:
- Delete more of the old v3 `discover/build/contract-test/apply` code and stale references.

Do:
- Remove dead command paths, stale helpers, and outdated docs/tests.
- Keep only the current staged flow and one-shot flow.

Do not do:
- Do not add compatibility shims unless they are tiny and necessary.
- Do not preserve old product language just because it existed before.

Done when:
- The repo has one obvious CLI shape and no stale user-facing references to the old flow.

## 2. Keep deterministic tool location as the default

Status:
- Done on 2026-03-14.

Goal:
- Keep tool evidence extraction simple and predictable.

Do:
- Improve deterministic matching for the repos that matter most.
- Make the confidence signal easier to understand.

Do not do:
- Do not make an agent mandatory for normal tool location.
- Do not add a large scoring or ranking subsystem.

Done when:
- Most supported local/npm/PyPI repos resolve with deterministic logic alone.

## 3. Add a tiny fallback only for low-confidence cases

Goal:
- Add the smallest possible mapper fallback when deterministic matching is weak.

Do:
- Trigger it only below a clear confidence threshold.
- Keep its output narrow: relevant files, why they matter, and confidence.

Do not do:
- Do not introduce multi-agent orchestration.
- Do not send whole repos to the backend by default.

Done when:
- Hard repos can recover with one minimal fallback pass, while normal repos stay fast and simple.

## 4. Expand the surface area only for real blockers

Goal:
- Resist feature creep in artifact resolution and catalog support.

Do:
- Add resolver coverage only for proven blockers in real MCPs.
- Keep official registry + Smithery as the default catalog scope.

Do not do:
- Do not build crawling infrastructure.
- Do not add providers without a clean API or real value.
- Do not add new output artifacts unless they clearly improve conversion quality.

Done when:
- New work is driven by real blocked conversions, not by hypothetical completeness.

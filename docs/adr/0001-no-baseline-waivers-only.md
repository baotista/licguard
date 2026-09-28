# No baseline: pre-existing violations must be fixed or waived

The PRD proposed a baseline mode (`licguard baseline`, `--baseline`, F-19) that silently accepts violations present when the gate is switched on. We dropped it: the gate is blocking from the day it is enabled, and every pre-existing violation must either be fixed or covered by a Waiver, which always carries a reason and an expiry date. This removes a concept and a file to maintain, and prevents undated, unjustified debt from hiding in a snapshot.

## Consequences

- Rollout keeps a non-blocking observation phase in CI to list the violations to fix or waive before the gate is enabled.
- `licguard waive` ships in the MVP to generate Waivers in bulk from current violations; `--reason` and `--expires` are mandatory so it cannot become a disguised, never-expiring baseline.
- Legal's turnaround on Waivers becomes a blocking dependency for onboarding each project.

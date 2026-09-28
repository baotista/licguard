# licguard

A CLI that inventories a project's open source dependencies, normalizes their licenses to SPDX, and evaluates them against a user-defined license policy — locally and as a CI gate.

## Language

### Inventory

**Project**:
The directory being checked, governed by a single `licguard.toml`. A check evaluates one Project and yields one aggregated result.
_Avoid_: Repository, repo, app, manifest

**Inventory source**:
A file from which a Project's Dependencies are extracted: a lockfile (e.g. `package-lock.json`) or an SBOM. A Project has one or more.
_Avoid_: Manifest, dependency file

**Workspace member**:
A local, unpublished package that is part of the Project itself (e.g. an npm workspace). It is a root of Introduction paths, never a Package in the inventory.
_Avoid_: Local package, sub-project

**Package**:
A published artifact identified by ecosystem, name and version (e.g. npm `lodash@4.17.21`). The unit that carries a license and receives a verdict.
_Avoid_: Library, module, component, artifact

**Dependency**:
A Package as used by a given project, with its Scope and Introduction paths. The same Package can be a Dependency of several projects.
_Avoid_: Package (when usage context matters), dep

**Scope**:
Whether a Dependency is needed by what the Project ships (`prod`) or only to build and test it (`dev`). A Dependency reached by at least one `prod` path is `prod`.
_Avoid_: Dev flag, test, provided (ecosystem-specific scopes are mapped to `dev`)

**Introduction path**:
A chain of Packages from the project root to a Dependency (e.g. `app > report-kit > some-pdf-lib`), showing who pulled it in.
_Avoid_: Chain, trail, dependency path

### Licenses

**Declared license**:
The license information exactly as the package author published it (e.g. `"Apache 2"`, `["MIT"]`, `"SEE LICENSE IN LICENSE.md"`), before any interpretation.
_Avoid_: Raw license, license field

**Normalized license**:
The SPDX expression derived from a Declared license or asserted by a License clarification (e.g. `Apache-2.0`). Policy and Waivers operate only on it.
_Avoid_: SPDX license, parsed license

**Elected license**:
The option of an `OR` expression that the verdict is based on — the most favorable one, first in expression order on ties.
_Avoid_: Chosen option, selected license

**License origin**:
Where a Package's license information came from: the installed package, the registry, or a License clarification.
_Avoid_: License source, provenance

**License clarification**:
An assertion, backed by evidence, of a Package's real license when its declared metadata is missing or wrong. It corrects a fact, never expires, and is evaluated by the policy like any other license.
_Avoid_: Override, waiver, fix

**SPDX exception**:
An additional-permission clause attached to a license in an SPDX expression via `WITH` (e.g. `Classpath-exception-2.0`). Published by the package author, never decided by us.
_Avoid_: Exception (unqualified, when the policy meaning is possible)

### Policy

**Policy**:
The license rules a Project declares in its `licguard.toml`: allow, review and deny lists, the `unresolved` and `unlisted` settings, Waivers and License clarifications.
_Avoid_: Rules, config, ruleset

**Distribution model**:
How the software built from a Project reaches its users: `saas`, `distributed` (delivered to a customer) or `internal`. A Project may have several; the strictest applicable verdict wins.
_Avoid_: Distribution context, deployment mode

**Verdict**:
The outcome of evaluating one Package against the Policy: exactly one of `allow`, `review` or `deny`. Depends only on the policy and Waivers.
_Avoid_: Status, classification, "unknown" as a verdict

**Violation**:
A Dependency whose verdict is blocking: `deny`, or `review` when strict mode is on. Any Violation fails the gate.
_Avoid_: Issue, finding, alert

**Verdict reason**:
Why a Package received its verdict: `listed` (its license appears in a policy list), `unresolved`, `unlisted`, or `waived` (a Waiver applies).
_Avoid_: Cause, source

**Unresolved license**:
A Package for which no license was found, or whose declared license cannot be normalized to an SPDX expression. Its verdict is set by the policy's `unresolved` setting.
_Avoid_: Unknown, missing license

**Unlisted license**:
A valid SPDX license that appears in none of the policy's allow, review or deny lists. Its verdict is set by the policy's `unlisted` setting.
_Avoid_: Unknown, unclassified

**Waiver**:
A documented, dated decision by the Project's owners that tolerates a specific package (optionally a specific version) whose license would otherwise violate the policy. Has a reason and an expiry date, and tolerates a verdict without changing the license. Applies only while it matches the Package's Normalized license.
_Avoid_: Exception, dérogation, allowance, override, clarification

**Stale waiver**:
A Waiver that no longer applies because it has expired or no longer matches any Dependency (package removed, version changed, or license changed). It should be removed or renewed.
_Avoid_: Obsolete waiver, dead waiver, unused exception

**Warning**:
A condition reported by a check that needs attention but never fails the gate (e.g. a Stale waiver, a Waiver about to expire, an unmatched License clarification).
_Avoid_: Violation, soft failure

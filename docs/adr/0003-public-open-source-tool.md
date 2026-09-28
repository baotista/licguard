# licguard is a public, organization-neutral open source tool

The PRD framed licguard as an internal tool enforcing one company's policy. We are building it as a public open source project under Apache-2.0, aimed at as many users as possible. The tool therefore makes no assumptions about any organization: it ships no house policy, no internal registry or URL, and no built-in notion of who approves Waivers. Our own company is just one consumer, keeping its central policy in a separate repository.

## Consequences

- `licguard init` generates a neutral template policy whose comments tell users to have it validated by their own legal counsel.
- Distribution targets public channels (GitHub Releases, crates.io, Homebrew, a public container image, a GitHub Marketplace action) rather than internal ones.
- GitHub is the first-class CI platform, but other CI platforms stay in scope.
- The PRD's "not legal advice" non-goal becomes a user-facing disclaimer.

# Build licguard rather than adopt an existing tool

We build our own CLI, modeled on cargo-deny's license checks (policy file, clarifications, dated waivers), instead of adopting an existing tool. The deciding constraints are a single static binary (NF-01), under 30 s for ~1,500 dependencies with a warm cache (NF-02), and no project data sent anywhere but the registries queried (NF-07). Scope stays tractable because we never reimplement Java dependency resolution: Maven and Gradle go through CycloneDX SBOMs.

## Considered Options

- **ORT (OSS Review Toolkit)**: covers npm, Maven and Gradle with a scriptable policy, but is a JVM tool that is heavy to operate and slow to run, far from a single fast binary. Rejected on a documentation review only; no hands-on evaluation was done.
- **FOSSA / Snyk / Mend**: turnkey with curated license data, but paid SaaS that uploads project data, which conflicts with NF-07.
- **license-checker, license_finder**: simple, but each covers one ecosystem, lacks a proper SPDX expression engine, and has no dated waivers.
- **cargo-deny**: the design we follow, but it only supports Cargo.

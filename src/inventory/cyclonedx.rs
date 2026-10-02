//! CycloneDX JSON SBOMs as Inventory sources: their components, named by
//! their purl, are the Packages of the ecosystems licguard supports.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use super::purl::Purl;
use super::{LicenseOrigin, LicensedPackage, Package, Scope, paths};
use crate::ecosystem::Ecosystem;

/// Whether a file named `file_name` is read as a CycloneDX JSON SBOM:
/// `bom.json` or `*.cdx.json`, the names CycloneDX recommends.
pub fn is_sbom(file_name: &str) -> bool {
    file_name == "bom.json" || file_name.ends_with(".cdx.json")
}

/// The CycloneDX specification versions licguard reads.
const SPEC_VERSIONS: [&str; 3] = ["1.4", "1.5", "1.6"];

/// For a file named like an SBOM that is not one.
const NOT_A_BOM_HINT: &str = "hint: licguard reads every bom.json and *.cdx.json file as a CycloneDX SBOM; regenerate it in CycloneDX JSON format, or rename it if it is not an SBOM";

/// What tells a CycloneDX BOM and its version, read before the rest.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Header {
    bom_format: Option<String>,
    spec_version: Option<String>,
}

#[derive(Deserialize)]
struct Bom {
    metadata: Option<Metadata>,
    #[serde(default)]
    components: Vec<Component>,
    #[serde(default)]
    dependencies: Vec<Dependency>,
}

#[derive(Deserialize)]
struct Metadata {
    /// What the SBOM describes: the root of the Introduction paths.
    component: Option<Component>,
}

#[derive(Deserialize)]
struct Component {
    #[serde(rename = "bom-ref")]
    bom_ref: Option<String>,
    #[serde(default)]
    name: String,
    version: Option<String>,
    purl: Option<String>,
    #[serde(default)]
    licenses: Vec<LicenseChoice>,
    /// Components nested in this one, e.g. npm's nested `node_modules`.
    #[serde(default)]
    components: Vec<Component>,
}

/// An entry of a component's `licenses`: a license or an SPDX expression.
#[derive(Deserialize)]
struct LicenseChoice {
    license: Option<License>,
    expression: Option<String>,
}

#[derive(Deserialize)]
struct License {
    id: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
struct Dependency {
    #[serde(rename = "ref")]
    reference: String,
    #[serde(default, rename = "dependsOn")]
    depends_on: Vec<String>,
}

/// Reads the SBOM at `source`, relative to `project`, and returns its
/// Packages with the Declared license of their component. Each Package
/// also gets its shortest Introduction path from the SBOM's root component,
/// following its `dependencies`, when it is reachable from it.
pub fn inventory(project: &Path, source: &str) -> Result<Vec<LicensedPackage>> {
    let path = project.join(source);
    let text =
        fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
    let not_a_bom = |why: String| {
        anyhow!(
            "{} is not a CycloneDX JSON SBOM: {why}\n{NOT_A_BOM_HINT}",
            path.display()
        )
    };
    let header: Header = serde_json::from_str(&text).map_err(|err| not_a_bom(err.to_string()))?;
    if header.bom_format.as_deref() != Some("CycloneDX") {
        return Err(not_a_bom("its bomFormat is not `CycloneDX`".to_string()));
    }
    let version = header.spec_version.unwrap_or_default();
    if !SPEC_VERSIONS.contains(&version.as_str()) {
        bail!(
            "{} uses CycloneDX specVersion {version}, which is not supported\nhint: regenerate it in CycloneDX 1.4, 1.5 or 1.6, the versions the current CycloneDX tools write",
            path.display()
        );
    }
    let bom: Bom = serde_json::from_str(&text).map_err(|err| not_a_bom(err.to_string()))?;

    let root = bom.metadata.and_then(|metadata| metadata.component);
    let root_ref = root.as_ref().and_then(|root| root.bom_ref.clone());
    let mut components = Vec::new();
    flatten(&bom.components, &mut components);
    let by_ref: HashMap<&str, &Component> = components
        .iter()
        .filter_map(|c| Some((c.bom_ref.as_deref()?, *c)))
        .collect();
    let graph: HashMap<&str, &[String]> = bom
        .dependencies
        .iter()
        .map(|d| (d.reference.as_str(), d.depends_on.as_slice()))
        .collect();
    let roots: Vec<(String, String)> = root
        .as_ref()
        .and_then(|root| Some((root.bom_ref.clone()?, root.name.clone())))
        .into_iter()
        .collect();
    let introduction_paths = paths::shortest(&roots, |key: &String, _prod_only| {
        graph
            .get(key.as_str())
            .into_iter()
            .flat_map(|refs| refs.iter())
            .filter_map(|r| {
                let child = by_ref.get(r.as_str())?;
                let name = package(child).map_or_else(|| child.name.clone(), |p| p.name);
                Some((name, r.clone()))
            })
            .collect()
    });

    let mut packages: BTreeMap<Package, LicensedPackage> = BTreeMap::new();
    for component in components {
        if component.bom_ref.is_some() && component.bom_ref == root_ref {
            continue; // the root, listed again
        }
        let Some(package) = package(component) else {
            continue;
        };
        let found = LicensedPackage {
            declared_license: declared_license(&component.licenses),
            license_origin: Some(LicenseOrigin::Sbom),
            from_registry: true,
            scope: Scope::Prod,
            introduction_path: component
                .bom_ref
                .as_ref()
                .and_then(|r| introduction_paths.get(r))
                .map(|(path, _)| path.clone()),
            sources: vec![source.to_string()],
            line: None,
            package: package.clone(),
        };
        // The same Package can be several components, e.g. nested ones.
        match packages.entry(package) {
            Entry::Vacant(vacant) => {
                vacant.insert(found);
            }
            Entry::Occupied(mut occupied) => occupied.get_mut().merge(found, true),
        }
    }
    Ok(packages.into_values().collect())
}

/// Appends `components` and the components nested in them to `out`.
fn flatten<'a>(components: &'a [Component], out: &mut Vec<&'a Component>) {
    for component in components {
        out.push(component);
        flatten(&component.components, out);
    }
}

/// The Package a component is, from its purl; `None` when it has none, or
/// one of an ecosystem licguard does not support.
fn package(component: &Component) -> Option<Package> {
    let purl = Purl::parse(component.purl.as_deref()?)?;
    let ecosystem = Ecosystem::from_purl_type(&purl.kind)?;
    Some(Package {
        ecosystem,
        name: ecosystem.package_name(purl.namespace.as_deref(), &purl.name),
        version: purl.version.or_else(|| component.version.clone())?,
    })
}

/// The Declared license of a component's `licenses`: each entry's
/// `license.id`, else its `license.name`, or its `expression`. Several
/// entries all apply, so they are joined with `AND`. `None` when there is
/// no entry.
fn declared_license(licenses: &[LicenseChoice]) -> Option<String> {
    let terms: Vec<String> = licenses
        .iter()
        .filter_map(|choice| match (&choice.license, &choice.expression) {
            (Some(license), _) => license.id.clone().or_else(|| license.name.clone()),
            (None, Some(expression)) if licenses.len() > 1 => Some(format!("({expression})")),
            (None, Some(expression)) => Some(expression.clone()),
            (None, None) => None,
        })
        .collect();
    (!terms.is_empty()).then(|| terms.join(" AND "))
}

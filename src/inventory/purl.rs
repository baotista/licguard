//! Package URLs (purls), by which CycloneDX components name their package:
//! `pkg:<type>/<namespace>/<name>@<version>?<qualifiers>#<subpath>`.

/// The parts of a purl that identify a Package, percent-decoded.
pub(super) struct Purl {
    /// In lower case, e.g. `maven`.
    pub kind: String,
    /// E.g. the Maven group or the npm scope; `None` when there is none.
    pub namespace: Option<String>,
    pub name: String,
    pub version: Option<String>,
    /// The qualifier keys, in lower case, e.g. `type` or `vcs_url`.
    pub qualifiers: Vec<String>,
}

impl Purl {
    /// Parses `text`; `None` when it is not a purl with a type and a name.
    pub fn parse(text: &str) -> Option<Purl> {
        let (scheme, rest) = text.split_once(':')?;
        if !scheme.eq_ignore_ascii_case("pkg") {
            return None;
        }
        let rest = rest.split('#').next().unwrap_or(rest);
        let (path, qualifiers) = rest.split_once('?').unwrap_or((rest, ""));
        let path = path.trim_matches('/');
        let (kind, rest) = path.split_once('/')?;
        let (namespace, name) = match rest.rsplit_once('/') {
            Some((namespace, name)) => (Some(namespace), name),
            None => (None, rest),
        };
        let (name, version) = match name.split_once('@') {
            Some((name, version)) => (name, Some(decode(version))),
            None => (name, None),
        };
        if kind.is_empty() || name.is_empty() {
            return None;
        }
        Some(Purl {
            kind: kind.to_ascii_lowercase(),
            namespace: namespace.map(|namespace| {
                namespace
                    .split('/')
                    .map(decode)
                    .collect::<Vec<_>>()
                    .join("/")
            }),
            name: decode(name),
            version,
            qualifiers: qualifiers
                .split('&')
                .filter_map(|q| q.split('=').next())
                .filter(|key| !key.is_empty())
                .map(str::to_ascii_lowercase)
                .collect(),
        })
    }
}

/// Decodes the `%XX` escapes of a purl segment; a malformed one is kept.
fn decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .filter(|hex| hex.iter().all(u8::is_ascii_hexdigit))
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

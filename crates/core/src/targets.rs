//! Validated logical target paths.
//!
//! Admission is deliberately narrower than filesystem support: slash-separated
//! portable ASCII with no reserved or malformed components. Native identity,
//! links and length limits are separate checks.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootRole {
    Data,
}

impl RootRole {
    /// Canonical sort identity; keeps ordering out of the public role type.
    pub(crate) fn canonical_key(self) -> u8 {
        match self {
            Self::Data => 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TargetFile {
    pub root: RootRole,
    pub relative: RelativePath,
}

#[derive(Clone, Debug)]
pub struct RelativePath(String);

impl RelativePath {
    /// Validate and preserve the spelling. Reports the first invalid component,
    /// zero-based.
    pub fn parse(spelling: &str) -> Result<Self, PathError> {
        if spelling.is_empty() {
            return Err(PathError::EmptyPath);
        }

        for (index, component) in spelling.split('/').enumerate() {
            if let Some(reason) = component_violation(component) {
                return Err(PathError::InvalidComponent { index, reason });
            }
        }

        Ok(Self(spelling.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn component_violation(component: &str) -> Option<PathViolation> {
    if component.is_empty() {
        return Some(PathViolation::Empty);
    }
    if component == "." || component == ".." {
        return Some(PathViolation::Dot);
    }
    if component.bytes().any(|byte| {
        !(0x20..=0x7e).contains(&byte)
            || matches!(byte, b'<' | b'>' | b':' | b'"' | b'\\' | b'|' | b'?' | b'*')
    }) {
        return Some(PathViolation::ForbiddenCharacter);
    }
    if component.ends_with('.') || component.ends_with(' ') {
        return Some(PathViolation::TrailingDotOrSpace);
    }
    if is_reserved_device_name(component) {
        return Some(PathViolation::ReservedDeviceName);
    }
    None
}

fn is_reserved_device_name(component: &str) -> bool {
    let stem = component
        .split_once('.')
        .map_or(component, |(stem, _)| stem)
        .trim_end_matches(' ')
        .to_ascii_lowercase();

    matches!(
        stem.as_str(),
        "con" | "prn" | "aux" | "nul" | "conin$" | "conout$"
    ) || numbered_device(&stem, "com")
        || numbered_device(&stem, "lpt")
}

fn numbered_device(stem: &str, prefix: &str) -> bool {
    stem.strip_prefix(prefix)
        .is_some_and(|suffix| matches!(suffix.as_bytes(), [b'1'..=b'9']))
}

#[derive(Debug)]
pub enum PathError {
    EmptyPath,
    InvalidComponent { index: usize, reason: PathViolation },
}

#[derive(Debug, PartialEq, Eq)]
pub enum PathViolation {
    Empty,
    Dot,
    ForbiddenCharacter,
    TrailingDotOrSpace,
    ReservedDeviceName,
}

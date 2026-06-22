use serde::{Deserialize, Serialize};

use crate::error::Error;

/// A validated host/tunnel name.
///
/// Rules (mirrors `_valid_host_name` in daemon.py):
/// - Not empty
/// - Does not contain `/`
/// - Does not contain `..`
/// - Is not exactly `.`
/// - Does not start with `.`
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HostName(String);

impl HostName {
    pub fn new(s: &str) -> Result<Self, Error> {
        if s.is_empty() {
            return Err(Error::BadParams("host name must not be empty".into()));
        }
        if s.contains('/') {
            return Err(Error::BadParams(
                format!("host name must not contain '/': {s}"),
            ));
        }
        if s.contains("..") {
            return Err(Error::BadParams(
                format!("host name must not contain '..': {s}"),
            ));
        }
        if s == "." {
            return Err(Error::BadParams("host name must not be '.'".into()));
        }
        if s.starts_with('.') {
            return Err(Error::BadParams(
                format!("host name must not start with '.': {s}"),
            ));
        }
        Ok(HostName(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for HostName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for HostName {
    type Error = Error;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        HostName::new(&s)
    }
}

impl From<HostName> for String {
    fn from(h: HostName) -> String {
        h.0
    }
}

/// A validated TCP port (1024..=65535).
///
/// Ports 0-1023 are privileged / well-known; auto2fa tunnels require
/// unprivileged ports to avoid needing elevated permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Port(u16);

impl Port {
    pub fn new(p: u16) -> Result<Self, Error> {
        if p < 1024 {
            return Err(Error::BadParams(
                format!("port must be 1024..=65535, got {p}"),
            ));
        }
        Ok(Port(p))
    }

    pub fn get(&self) -> u16 {
        self.0
    }
}

impl std::fmt::Display for Port {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostname_rejects_traversal() {
        assert!(HostName::new("k6").is_ok());
        assert!(HostName::new("gpu-node_1").is_ok());
        assert!(HostName::new("a.b.c").is_ok());
        for bad in ["../x", "a/b", "..", ".", "", "a..b", "/etc", ".lead"] {
            assert!(HostName::new(bad).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn port_range() {
        assert!(Port::new(8090).is_ok());
        assert!(Port::new(1024).is_ok());
        assert!(Port::new(65535).is_ok());
        assert!(Port::new(80).is_err());
        assert!(Port::new(1023).is_err());
    }
}

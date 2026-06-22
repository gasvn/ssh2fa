#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrCode {
    InvalidRequest,
    UnknownMethod,
    BadParams,
    NotFound,
    PortInUse,
    Duplicate,
    DiscoveryFailed,
    Internal,
}

impl ErrCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrCode::InvalidRequest => "invalid_request",
            ErrCode::UnknownMethod => "unknown_method",
            ErrCode::BadParams => "bad_params",
            ErrCode::NotFound => "not_found",
            ErrCode::PortInUse => "port_in_use",
            ErrCode::Duplicate => "duplicate",
            ErrCode::DiscoveryFailed => "discovery_failed",
            ErrCode::Internal => "internal",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errcode_strings() {
        assert_eq!(ErrCode::PortInUse.as_str(), "port_in_use");
    }
}

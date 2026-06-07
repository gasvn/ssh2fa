use crate::proto::ErrCode;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad params: {0}")]
    BadParams(String),
    #[error("port in use: {0}")]
    PortInUse(u16),
    #[error("duplicate: {0}")]
    Duplicate(String),
    #[error("discovery failed: {0}")]
    Discovery(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("internal: {0}")]
    Internal(String),
}

impl Error {
    pub fn to_errcode(&self) -> ErrCode {
        match self {
            Error::NotFound(_) => ErrCode::NotFound,
            Error::BadParams(_) => ErrCode::BadParams,
            Error::PortInUse(_) => ErrCode::PortInUse,
            Error::Duplicate(_) => ErrCode::Duplicate,
            Error::Discovery(_) => ErrCode::DiscoveryFailed,
            Error::Io(_) | Error::Internal(_) => ErrCode::Internal,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::ErrCode;

    #[test]
    fn maps_codes() {
        assert_eq!(Error::NotFound("x".into()).to_errcode(), ErrCode::NotFound);
        assert_eq!(Error::PortInUse(8090).to_errcode(), ErrCode::PortInUse);
        assert_eq!(Error::Duplicate("k6".into()).to_errcode(), ErrCode::Duplicate);
        assert_eq!(Error::Discovery("squeue".into()).to_errcode(), ErrCode::DiscoveryFailed);
        assert_eq!(Error::BadParams("x".into()).to_errcode(), ErrCode::BadParams);
    }
}

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ServerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Rate limit exceeded")]
    RateLimitExceeded,

    #[error("Service unavailable")]
    ServiceUnavailable,

    #[error("Invalid packet format")]
    InvalidPacket,

    #[error("Client timeout")]
    ClientTimeout,

    #[error("Maintenance mode active")]
    MaintenanceMode,
}

pub type Result<T> = std::result::Result<T, ServerError>;
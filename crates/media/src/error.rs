#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("unsupported container")]
    Unsupported,
    #[error("storage io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("undecodable media: {0}")]
    Undecodable(String),
    #[error("probe error: {0}")]
    Probe(String),
    #[error("transcode error: {0}")]
    Transcode(String),
    #[error("transcode timed out")]
    TranscodeTimeout,
    #[error("probe timed out")]
    ProbeTimeout,
    #[error("stored object not found: {0}")]
    NotFound(String),
}

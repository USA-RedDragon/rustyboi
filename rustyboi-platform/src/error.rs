//! A small platform-level error type. Replaces the former `pixels::Error` in
//! the entry-point signatures now that `pixels` is gone.

use std::fmt;

#[derive(Debug)]
pub struct PlatformError(String);

impl PlatformError {
    pub(crate) fn new(msg: impl Into<String>) -> Self {
        PlatformError(msg.into())
    }

    /// Wrap any `Display` (winit `OsError`/`EventLoopError`, etc.) as a message.
    pub(crate) fn from_display(e: impl fmt::Display) -> Self {
        PlatformError(e.to_string())
    }
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PlatformError {}

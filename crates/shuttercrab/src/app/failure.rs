//! Failures of a screenshot or a recording, as the user and the log see
//! them.

/// A failure to report: a message for the user, and the details for the log
/// (PRD §24).
#[derive(Debug)]
pub struct Failure {
    pub message: String,
    pub detail: String,
    /// Whether a recording failed, rather than a screenshot.
    pub recording: bool,
}

impl Failure {
    pub(super) fn new(message: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            detail: detail.into(),
            recording: false,
        }
    }

    /// A failure whose message says it all.
    pub(super) fn plain(message: &str) -> Self {
        Self::new(message, message)
    }

    /// The same failure, of a recording.
    pub(super) fn of_recording(self) -> Self {
        Self {
            recording: true,
            ..self
        }
    }

    /// The notification's title.
    pub(super) fn title(&self) -> &'static str {
        if self.recording {
            "Recording failed"
        } else {
            "Screenshot failed"
        }
    }
}

impl From<shuttercrab_capture::CaptureError> for Failure {
    fn from(e: shuttercrab_capture::CaptureError) -> Self {
        // The capture error's code and detail are for the log only.
        Self::new(e.message.clone(), format!("{:?}: {e}", e.code))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_failures_tell_the_user_the_message_and_the_log_the_rest() {
        let failure = Failure::from(shuttercrab_capture::CaptureError {
            code: shuttercrab_capture::CaptureErrorCode::DeviceLost,
            message: "The graphics device was reset; try again".into(),
            detail: Some("CopySubresourceRegion failed: 0x887A0005".into()),
        });
        assert_eq!(failure.message, "The graphics device was reset; try again");
        assert!(failure.detail.contains("DeviceLost"), "{}", failure.detail);
        assert!(failure.detail.contains("0x887A0005"), "{}", failure.detail);
    }
}

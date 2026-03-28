//! Error types for the API client.

/// Errors returned by [`super::NenjoClient`] operations.
#[derive(Debug, thiserror::Error)]
pub enum ApiClientError {
    /// Network / transport error from reqwest.
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// The backend returned a non-success status.
    #[error("API error {status} [{code}]: {message}")]
    Api {
        status: u16,
        code: String,
        message: String,
    },

    /// Failed to deserialise a response body.
    #[error("Failed to parse response: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_error_display() {
        let err = ApiClientError::Api {
            status: 404,
            code: "not_found".into(),
            message: "Task not found".into(),
        };
        let s = err.to_string();
        assert!(s.contains("404"));
        assert!(s.contains("not_found"));
        assert!(s.contains("Task not found"));
    }

    #[test]
    fn test_parse_error_display() {
        let err = ApiClientError::Parse("unexpected token".into());
        assert!(err.to_string().contains("unexpected token"));
    }
}

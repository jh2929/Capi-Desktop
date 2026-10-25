use thiserror::Error;

/// Errores tipados del motor.
///
/// El frontend recibirá `{ code, message, retryable }` en la capa Tauri, en
/// lugar de cadenas de error opacas como hacía el daemon Kotlin.
#[derive(Debug, Error)]
pub enum EngineError {
    #[error("error de red: {0}")]
    Network(String),

    #[error("contenido no disponible: {0}")]
    Unavailable(String),

    #[error("YouTube pidió autenticación o rechazó la petición: {0}")]
    Auth(String),

    #[error("rate limit de YouTube: {0}")]
    RateLimited(String),

    #[error("no se pudo interpretar la respuesta de YouTube: {0}")]
    Decode(String),

    #[error("no encontrado: {0}")]
    NotFound(String),

    #[error("error interno: {0}")]
    Internal(String),
}

impl EngineError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Network(_) => "network",
            Self::Unavailable(_) => "unavailable",
            Self::Auth(_) => "auth",
            Self::RateLimited(_) => "rate_limited",
            Self::Decode(_) => "decode",
            Self::NotFound(_) => "not_found",
            Self::Internal(_) => "internal",
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(self, Self::Network(_) | Self::RateLimited(_))
    }
}

impl From<rustypipe::error::Error> for EngineError {
    fn from(value: rustypipe::error::Error) -> Self {
        // Fase 0: se conserva el mensaje de rustypipe. El mapeo fino a
        // `Unavailable`/`Auth`/`RateLimited` se hará al implementar el player.
        Self::Internal(value.to_string())
    }
}

impl From<reqwest::Error> for EngineError {
    fn from(value: reqwest::Error) -> Self {
        if value.is_timeout() {
            Self::Network("tiempo de espera agotado".into())
        } else if value.is_status() {
            Self::Network(format!(
                "HTTP {}",
                value.status().map(|s| s.as_u16()).unwrap_or(0)
            ))
        } else {
            Self::Network(value.to_string())
        }
    }
}

impl From<url::ParseError> for EngineError {
    fn from(value: url::ParseError) -> Self {
        Self::Internal(format!("URL inválida: {value}"))
    }
}

//! API errors: a status code plus a machine-readable `code` (ADR 0007). The
//! frontend translates codes; the API never returns display sentences.

use axum::Json;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use starr_core::auth::THROTTLE_WINDOW;
use starr_db::DbError;
use utoipa::ToSchema;

/// Error response body.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    /// Machine-readable error code, e.g. `auth.invalid_credentials`.
    #[schema(example = "auth.invalid_credentials")]
    pub code: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiError {
    /// No valid session.
    Unauthenticated,
    /// Valid session, wrong kind of account.
    Forbidden,
    /// Wrong username or password (identical for unknown users).
    InvalidCredentials,
    /// Login throttled.
    TooManyAttempts,
    /// Wrong TOTP or recovery code in the second login step.
    InvalidCode,
    /// The pending login of the second step is unknown, used or expired.
    LoginExpired,
    /// `STARR_ADMIN_TOTP_REQUIRED`: this admin must enroll TOTP first.
    TotpEnrollmentRequired,
    /// Switching back to admin needs a fresh `POST /auth/elevate`.
    ElevationRequired,
    /// Mutating request without a matching `Origin` (CSRF).
    OriginMismatch,
    SetupAlreadyCompleted,
    SetupInvalidToken,
    SetupPublicUrlRequired,
    /// Request input rejected; the code names the field and rule.
    Validation(&'static str),
    /// The addressed resource does not exist; the code names it.
    NotFound(&'static str),
    /// Conflicts with existing data; the code names it.
    Conflict(&'static str),
    /// A dependency is not ready (e.g. the worker has not published the
    /// sealing key yet); the code names it.
    Unavailable(&'static str),
    Internal,
}

impl ApiError {
    #[must_use]
    pub fn status(self) -> StatusCode {
        match self {
            Self::Unauthenticated
            | Self::InvalidCredentials
            | Self::InvalidCode
            | Self::LoginExpired => StatusCode::UNAUTHORIZED,
            Self::Forbidden
            | Self::OriginMismatch
            | Self::SetupInvalidToken
            | Self::TotpEnrollmentRequired
            | Self::ElevationRequired => StatusCode::FORBIDDEN,
            Self::TooManyAttempts => StatusCode::TOO_MANY_REQUESTS,
            Self::SetupAlreadyCompleted | Self::SetupPublicUrlRequired | Self::Conflict(_) => {
                StatusCode::CONFLICT
            }
            Self::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::Unauthenticated => "auth.unauthenticated",
            Self::Forbidden => "auth.forbidden",
            Self::InvalidCredentials => "auth.invalid_credentials",
            Self::TooManyAttempts => "auth.too_many_attempts",
            Self::InvalidCode => "auth.invalid_code",
            Self::LoginExpired => "auth.login_expired",
            Self::TotpEnrollmentRequired => "auth.totp_enrollment_required",
            Self::ElevationRequired => "auth.elevation_required",
            Self::OriginMismatch => "auth.origin_mismatch",
            Self::SetupAlreadyCompleted => "setup.already_completed",
            Self::SetupInvalidToken => "setup.invalid_token",
            Self::SetupPublicUrlRequired => "setup.public_url_required",
            Self::Validation(code)
            | Self::Conflict(code)
            | Self::NotFound(code)
            | Self::Unavailable(code) => code,
            Self::Internal => "internal",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(ErrorBody { code: self.code() });
        if self == Self::TooManyAttempts {
            let retry_after = THROTTLE_WINDOW.num_seconds().to_string();
            return (self.status(), [(header::RETRY_AFTER, retry_after)], body).into_response();
        }
        (self.status(), body).into_response()
    }
}

impl From<DbError> for ApiError {
    fn from(err: DbError) -> Self {
        match err {
            DbError::UsernameTaken => Self::Conflict("validation.username_taken"),
            DbError::ServiceNameTaken => Self::Conflict("services.name_taken"),
            DbError::EmailTaken => Self::Conflict("validation.email_taken"),
            DbError::LastActiveAdmin => Self::Conflict("admin.last_active"),
            DbError::MemberAlreadyLinked => Self::Conflict("member.already_linked"),
            DbError::RoleCapabilityMissing => Self::Validation("categories.capability_missing"),
            DbError::RoleTaken => Self::Conflict("categories.role_taken"),
            DbError::SourceTaken => Self::Conflict("categories.source_taken"),
            DbError::LibraryTaken => Self::Validation("categories.library_overlap"),
            DbError::ServiceMissing => Self::NotFound("services.not_found"),
            other => {
                tracing::error!(error = %other, "database error");
                Self::Internal
            }
        }
    }
}

impl From<tokio::task::JoinError> for ApiError {
    fn from(err: tokio::task::JoinError) -> Self {
        tracing::error!(error = %err, "blocking task failed");
        Self::Internal
    }
}

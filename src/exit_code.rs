#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub(crate) enum ExitCode {
    Success = 0,
    General = 1,
    Auth = 2,
    Connection = 3,
    NotFound = 4,
    Conflict = 5,
}

impl ExitCode {
    pub(crate) fn exit(self) -> ! {
        std::process::exit(self as i32);
    }
}

pub(crate) fn from_http_status(status: u16) -> ExitCode {
    match status {
        200..=299 => ExitCode::Success,
        401 | 403 => ExitCode::Auth,
        404 => ExitCode::NotFound,
        409 => ExitCode::Conflict,
        _ => ExitCode::General,
    }
}

pub(crate) fn from_reqwest_error(err: &reqwest::Error) -> ExitCode {
    if let Some(status) = err.status() {
        return from_http_status(status.as_u16());
    }

    if err.is_connect() || err.is_timeout() || err.is_request() {
        return ExitCode::Connection;
    }

    ExitCode::General
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_2xx_is_success() {
        assert_eq!(from_http_status(200), ExitCode::Success);
        assert_eq!(from_http_status(201), ExitCode::Success);
        assert_eq!(from_http_status(204), ExitCode::Success);
    }

    #[test]
    fn http_401_403_are_auth() {
        assert_eq!(from_http_status(401), ExitCode::Auth);
        assert_eq!(from_http_status(403), ExitCode::Auth);
    }

    #[test]
    fn http_404_is_not_found() {
        assert_eq!(from_http_status(404), ExitCode::NotFound);
    }

    #[test]
    fn http_409_is_conflict() {
        assert_eq!(from_http_status(409), ExitCode::Conflict);
    }

    #[test]
    fn http_500_is_general() {
        assert_eq!(from_http_status(500), ExitCode::General);
    }

    #[test]
    fn exit_code_values_match_doc() {
        assert_eq!(ExitCode::Success as i32, 0);
        assert_eq!(ExitCode::General as i32, 1);
        assert_eq!(ExitCode::Auth as i32, 2);
        assert_eq!(ExitCode::Connection as i32, 3);
        assert_eq!(ExitCode::NotFound as i32, 4);
        assert_eq!(ExitCode::Conflict as i32, 5);
    }
}

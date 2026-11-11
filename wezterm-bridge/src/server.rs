use crate::proto::Inbound;

/// The first line a client sends must be a valid auth message.
///
/// The socket is on loopback, which every process on this machine can reach,
/// so being able to connect proves nothing. Reading the token file does.
pub fn authenticate(first_line: &str, expected_token: &str) -> Result<(), String> {
    let msg: Inbound =
        serde_json::from_str(first_line).map_err(|err| format!("第一行必须是认证消息：{err}"))?;
    match msg {
        Inbound::Auth { token } => {
            if crate::token::verify(expected_token, &token) {
                Ok(())
            } else {
                Err("认证令牌被拒绝".to_string())
            }
        }
        _ => Err("第一行必须是认证消息".to_string()),
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn a_correct_token_authenticates() {
        let line = r#"{"t":"auth","token":"secret"}"#;
        assert_eq!(authenticate(line, "secret"), Ok(()));
    }

    #[test]
    fn a_wrong_token_is_refused() {
        let line = r#"{"t":"auth","token":"guess"}"#;
        assert!(authenticate(line, "secret").is_err());
    }

    #[test]
    fn the_first_line_must_be_auth() {
        let line = r#"{"t":"send","pane":7,"text":"hi"}"#;
        let err = authenticate(line, "secret").expect_err("to refuse");
        assert!(
            err.contains("认证消息"),
            "the client needs to be told what was wrong: {}",
            err
        );
    }

    #[test]
    fn garbage_is_refused_without_panicking() {
        assert!(authenticate("not json at all", "secret").is_err());
    }

    #[test]
    fn an_empty_first_line_is_refused() {
        assert!(authenticate("", "secret").is_err());
    }
}

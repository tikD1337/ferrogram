use tg_core::{Command, Secret};

#[test]
fn phone_command_debug_is_redacted() {
    let command = Command::SubmitPhoneNumber(Secret::new("private input".into()));
    assert!(!format!("{command:?}").contains("private input"));
}

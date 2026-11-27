use super::*;

#[test]
fn parses_slash_commands_locally() {
    assert_eq!(parse_command("/new"), Some(Command::New));
    assert_eq!(parse_command("  /NEW  "), Some(Command::New));
    assert_eq!(parse_command("/reset"), Some(Command::New));
    assert_eq!(parse_command("/help"), Some(Command::Help));
    assert_eq!(
        parse_command("/modelo"),
        Some(Command::Model(String::new()))
    );
    assert_eq!(
        parse_command("/effort high"),
        Some(Command::Effort("high".into()))
    );
    assert_eq!(
        parse_command("/esfuerzo"),
        Some(Command::Effort(String::new()))
    );
    assert_eq!(parse_command("hola /new"), None);
}

#[test]
fn parses_new_commands() {
    assert_eq!(
        parse_command("/rename Mi chat  largo"),
        Some(Command::Rename("Mi chat  largo".into()))
    );
    assert_eq!(parse_command("/rename"), Some(Command::Rename("".into())));
    assert_eq!(parse_command("/agents"), Some(Command::Agents));
    assert_eq!(parse_command("/stop"), Some(Command::Stop));
    assert_eq!(
        parse_command("/model gpt x"),
        Some(Command::Model("gpt x".into()))
    );
    assert_eq!(
        parse_command("/agent claude"),
        Some(Command::Agent("claude".into()))
    );
    let help = help_text();
    for (cmd, _) in COMMAND_HELP {
        assert!(help.contains(cmd));
    }
}

//! YU-RIS adapter state: parameters, capability, suspension, dirty frames.

use yu_emote::EmoteError;
use yu_emote::yuris::{Capability, EmoteArg, YurisEmote};

#[test]
fn capability_gates_animation() {
    let mut emote = YurisEmote::new(Capability::Unavailable);
    assert!(!emote.tick(16));
    assert_eq!(emote.clock_ms(), 0);

    emote.set_capability(Capability::DataOnly);
    assert!(!emote.tick(16));

    emote.set_capability(Capability::Ready);
    assert!(emote.tick(16));
    assert_eq!(emote.clock_ms(), 16);
    assert!(emote.take_dirty());
    assert!(!emote.take_dirty());
}

#[test]
fn emote_parameters_are_recorded_by_name() {
    let mut emote = YurisEmote::new(Capability::Ready);
    emote
        .apply_emote([
            ("BUST", EmoteArg::Int(128)),
            ("HAIR", EmoteArg::Int(128)),
            ("PARTS", EmoteArg::Int(128)),
            ("SMOOTH", EmoteArg::Int(1)),
        ])
        .expect("apply");

    assert_eq!(emote.variables()["BUST"], 128.0);
    assert_eq!(emote.variables()["SMOOTH"], 1.0);
    assert!(emote.unknown_parameters().is_empty());

    let info = emote.query_info(&["BUST", "SMOOTH"]).expect("known fields");
    assert_eq!(info["BUST"], EmoteArg::Int(128));
    assert_eq!(info["SMOOTH"], EmoteArg::Int(1));
}

#[test]
fn unknown_parameters_are_recorded_not_guessed() {
    let mut emote = YurisEmote::new(Capability::DataOnly);
    emote
        .apply_emote([("MYSTERY", EmoteArg::Int(5))])
        .expect("unknown is non-fatal");
    assert!(emote.unknown_parameters().contains("MYSTERY"));
    assert!(!emote.variables().contains_key("MYSTERY"));

    let error = emote.query_info(&["MYSTERY"]).unwrap_err();
    assert!(matches!(error, EmoteError::UnsupportedInfo(field) if field == "MYSTERY"));
}

#[test]
fn known_numeric_name_with_string_value_is_explicit_error() {
    let mut emote = YurisEmote::new(Capability::DataOnly);
    let error = emote
        .apply_emote([("BUST", EmoteArg::Str("128".into()))])
        .unwrap_err();
    assert!(matches!(
        error,
        EmoteError::UnsupportedParameterType { name } if name == "BUST"
    ));
}

#[test]
fn suspend_freezes_clock_and_resume_continues() {
    let mut emote = YurisEmote::new(Capability::Ready);
    assert!(emote.tick(10));
    emote.suspend();
    assert!(emote.is_suspended());
    assert!(!emote.tick(10));
    assert_eq!(emote.clock_ms(), 10);
    emote.resume();
    assert!(emote.tick(10));
    assert_eq!(emote.clock_ms(), 20);
}

#![cfg(test)]

use super::*;

#[test]
fn normalizes_compatible_endpoint_and_rejects_embedded_credentials() {
    assert_eq!(
        base_url("http://127.0.0.1:8080/v1/chat/completions/").unwrap(),
        "http://127.0.0.1:8080/v1",
    );
    for invalid in [
        "https://secret@example.com/v1",
        "https://example.com/v1?key=secret",
        "https://example.com/v1#token",
        "file:///tmp/model",
    ] {
        assert!(base_url(invalid).is_err());
    }
}

#[test]
fn credential_reuse_requires_the_same_origin() {
    let previous = "https://example.com/v1";
    assert!(same_origin(previous, "https://EXAMPLE.com:443/v2"));
    for other in [
        "https://other.example.com/v1",
        "http://example.com/v1",
        "https://example.com:8443/v1",
        "invalid",
    ] {
        assert!(!same_origin(previous, other));
    }
}

#[test]
fn desktop_without_model_options_stays_unconfigured() {
    let settings = ModelSettings::new(Path::new("fixture"), vec![]);
    let options = settings.options();
    assert!(options.is_empty());
    assert!(!DesktopSettings::from_env(&options).model.is_configured());
}

#[test]
fn saved_model_options_keep_environment_and_mock_entries() {
    let mock = Arc::new(DesktopModel::Mock);
    let env = Arc::new(DesktopModel::OpenAi(OpenAiProvider::new("fixture-env-key")));
    let mut settings = ModelSettings::new(Path::new("fixture"), vec![env.clone(), mock.clone()]);
    let stored = Connection {
        base_url: "http://127.0.0.1:8080/v1".to_owned(),
        model_id: "fixture/model".to_owned(),
        api_key: "fixture-secret".to_owned(),
    };
    let json = serde_json::to_vec(&stored).unwrap();
    let saved = Connection::decode(&json)
        .unwrap()
        .into_model(&ConnectionId::OpenAi);
    assert_eq!(saved.model.name(), "fixture/model");
    settings.saved.insert(ConnectionId::OpenAi, saved);
    let second = ConnectionId::new_openai().unwrap();
    settings.saved.insert(
        second.clone(),
        Connection {
            base_url: "http://127.0.0.1:8081/v1".to_owned(),
            ..stored
        }
        .into_model(&second),
    );
    let options = settings.options();
    assert!(Arc::ptr_eq(&options[0], &env));
    assert_eq!(
        settings.saved_entry(&options[1]).unwrap().0,
        &ConnectionId::OpenAi
    );
    assert_eq!(settings.saved_entry(&options[2]).unwrap().0, &second);
    assert_eq!(options[1].name(), options[2].name());
    assert_ne!(settings.source(&options[1]), settings.source(&options[2]));
    assert!(Arc::ptr_eq(&options[3], &mock));
    settings.saved.remove(&ConnectionId::OpenAi);
    assert!(Arc::ptr_eq(&settings.options()[1], &options[2]));
}

#[test]
fn connection_ids_preserve_legacy_selection_and_vault_keys() {
    for (json, id, key) in [
        (r#""deepseek""#, ConnectionId::DeepSeek, "0"),
        (r#""openai""#, ConnectionId::OpenAi, "1"),
    ] {
        assert_eq!(serde_json::from_str::<ConnectionId>(json).unwrap(), id);
        assert_eq!(serde_json::to_string(&id).unwrap(), json);
        assert_eq!(id.key(), key);
    }
    let one = ConnectionId::new_openai().unwrap();
    let two = ConnectionId::new_openai().unwrap();
    assert_ne!(one, two);
    assert_eq!(ConnectionId::from_marker(one.key()), Some(one.clone()));
    assert_eq!(
        serde_json::from_value::<ConnectionId>(serde_json::to_value(&one).unwrap()).unwrap(),
        one
    );
    assert!(ConnectionId::from_marker("../1").is_none());
}

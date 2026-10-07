use dictate_context::{
    ContextConfig, ContextEngine, ContextProvider, NoContext, TestContext, WindowInfo,
};
use dictate_proto::{AppCategory as Cat, ContextInjection, Tone};
use std::sync::Arc;

fn window(class: &str, instance: &str, title: &str) -> WindowInfo {
    WindowInfo {
        class: Some(class.into()),
        instance: Some(instance.into()),
        title: Some(title.into()),
        ..Default::default()
    }
}
fn config(text: &str) -> ContextConfig {
    toml::from_str(text).unwrap()
}

#[test]
fn builtins_cover_all_required_classes_and_instances_with_neutral_tone() {
    let categories = [
        (Cat::Terminal, "com.mitchellh.ghostty ghostty kitty alacritty org.wezfurlong.wezterm xterm urxvt konsole gnome-terminal-server st foot"),
        (Cat::Editor, "code code-oss cursor zed jetbrains-idea emacs neovide gvim"),
        (Cat::Browser, "google-chrome chromium firefox brave-browser"),
        (Cat::Chat, "slack discord signal telegram-desktop element"),
        (Cat::Email, "thunderbird evolution"),
        (Cat::Document, "libreoffice-writer obsidian notion"),
        (Cat::Other, "unknown"),
    ];
    for (category, classes) in categories {
        for class in classes.split_whitespace() {
            for w in [
                window(&class.to_uppercase(), "unrecognized", "synthetic"),
                window("unrecognized", class, "synthetic"),
            ] {
                let p = ContextConfig::default().resolve(Some(&w));
                assert_eq!(p.context.as_ref().unwrap().category, category, "{class}");
                assert_eq!(p.tone, Tone::Neutral);
                assert_eq!(p.inject, None, "even terminals inherit Ctrl+V");
                // No category forces the LLM pass off: terminals get the
                // verbatim category policy inside the pass instead (S21).
                assert_eq!(p.llm_format, None, "{class}");
                if category == Cat::Terminal {
                    assert_eq!(p.spoken_punctuation, Some(false), "{class}");
                    assert_eq!(p.spoken_line_breaks, Some(false), "{class}");
                } else {
                    assert_eq!(p.spoken_punctuation, None, "{class}");
                }
            }
        }
    }
}

#[test]
fn browser_title_rules_override_class_but_do_not_apply_to_other_categories() {
    for browser in ["google-chrome", "firefox", "chromium", "brave-browser"] {
        for title in ["Synthetic inbox - Gmail", "Outlook - synthetic mailbox"] {
            let p = ContextConfig::default().resolve(Some(&window(browser, "x", title)));
            assert_eq!(p.context.unwrap().category, Cat::Email);
        }
    }
    let p = ContextConfig::default().resolve(Some(&window("code", "x", "Outlook - source")));
    assert_eq!(p.context.unwrap().category, Cat::Editor);
}

#[test]
fn first_matching_user_profile_beats_title_and_class_defaults() {
    let c = config(
        r#"
[[profiles]]
name = "browser-chat"
match = { class = "GOOGLE-*", title = "(?i)gmail" }
category = "chat"
tone = "casual"
llm_format = true
inject = "off"
spoken_punctuation = true
spoken_line_breaks = false
[[profiles]]
name = "second"
match = { class = "google-chrome" }
tone = "formal"
"#,
    );
    let p = c.resolve(Some(&window("google-chrome", "chrome", "Synthetic Gmail")));
    let ctx = p.context.unwrap();
    assert_eq!(ctx.category, Cat::Chat);
    assert_eq!(ctx.profile.as_deref(), Some("browser-chat"));
    assert_eq!(p.tone, Tone::Casual);
    assert_eq!(p.inject, Some(ContextInjection::Off));
    assert_eq!(p.llm_format, Some(true));
    assert_eq!(p.spoken_punctuation, Some(true));
    assert_eq!(p.spoken_line_breaks, Some(false));
}

#[test]
fn match_fields_are_conjunctive_and_class_is_distinct_from_instance() {
    let c = config(
        r#"
[[profiles]]
name = "coding"
match = { class = "com.mitchellh.ghostty", instance = "ghostt?", title = "(?i)claude" }
"#,
    );
    for (w, yes) in [
        (
            window("COM.MITCHELLH.GHOSTTY", "GHOSTTY", "Claude synthetic"),
            true,
        ),
        (window("com.mitchellh.ghostty", "ghostty", "other"), false),
        (window("com.mitchellh.ghostty", "wrong", "claude"), false),
        (window("ghostty", "com.mitchellh.ghostty", "claude"), false),
        (
            window("com.mitchellh.ghostty-extra", "ghostty", "claude"),
            false,
        ),
    ] {
        let p = c.resolve(Some(&w));
        assert_eq!(p.context.unwrap().profile.is_some(), yes, "{w:?}");
    }
}

#[test]
fn glob_special_characters_and_exact_matches_are_literal_and_anchored() {
    let c = config(
        r#"
[[profiles]]
name = "literal"
match = { instance = "app.[x]?*" }
"#,
    );
    for (instance, yes) in [
        ("app.[x]1-extra", true),
        ("app.[x]", false),
        ("app.ax1-extra", false),
        ("prefixapp.[x]1", false),
    ] {
        let p = c.resolve(Some(&window("unknown", instance, "synthetic")));
        assert_eq!(p.context.unwrap().profile.is_some(), yes);
    }
}

#[test]
fn invalid_profile_values_report_the_profile_name_at_load() {
    for setting in [
        "match = { title = '[' }",
        "match = { class = '*' }\ncategory = 'typo'",
        "match = { class = '*' }\ntone = 'typo'",
        "match = { class = '*' }\ninject = 'typo'",
        "match = {}",
        "match = { clas = '*' }",
        "match = { class = '*' }\nllm_format = 'false'",
        "match = { class = 123 }",
    ] {
        let text = format!("[[profiles]]\nname = 'broken-profile'\n{setting}");
        let e = toml::from_str::<ContextConfig>(&text)
            .unwrap_err()
            .to_string();
        assert!(e.contains("broken-profile"), "{e}");
    }
}

#[test]
fn an_explicit_terminal_override_wins_and_unset_values_inherit() {
    let c = config(
        r#"
[[profiles]]
name = "terminal"
match = { class = "ghostty" }
llm_format = true
spoken_line_breaks = true
inject = "paste"
"#,
    );
    let p = c.resolve(Some(&window("ghostty", "x", "synthetic")));
    assert_eq!(p.llm_format, Some(true));
    assert_eq!(p.spoken_line_breaks, Some(true));
    assert_eq!(p.spoken_punctuation, Some(false));
    assert_eq!(p.inject, Some(ContextInjection::Paste));
    let p = c.resolve(Some(&window("slack", "x", "synthetic")));
    assert_eq!(p.llm_format, None);
    assert_eq!(p.spoken_punctuation, None);
    assert_eq!(p.spoken_line_breaks, None);
}

#[test]
fn absent_disabled_and_partial_context_have_a_defined_default_path() {
    assert_eq!(NoContext.capture(), None);
    let c = ContextConfig::default();
    assert_eq!(c.resolve(None), Default::default());
    assert_eq!(c.resolve(Some(&WindowInfo::default())), Default::default());
    let p = c.resolve(Some(&WindowInfo {
        instance: Some("Ghostty".into()),
        ..Default::default()
    }));
    assert_eq!(p.context.unwrap().app, "ghostty");
    // Disabling detection drops profiles and host discovery, but an app the
    // caller named explicitly remains the session's identity (dictionary
    // scope and history depend on it).
    assert_eq!(
        ContextEngine::disabled().resolve(Some("Slack"), true),
        dictate_proto::ResolvedProfile {
            context: Some(dictate_proto::AppContext::new("slack")),
            ..Default::default()
        }
    );
    assert_eq!(
        ContextEngine::disabled().resolve(None, true),
        Default::default()
    );
}

#[test]
fn caller_app_and_remote_sessions_never_capture_host_titles() {
    struct MustNotCapture;
    impl ContextProvider for MustNotCapture {
        fn capture(&self) -> Option<WindowInfo> {
            panic!("host focus must not be queried")
        }
    }
    let e = ContextEngine::new(ContextConfig::default(), Arc::new(MustNotCapture));
    let p = e.resolve(Some("Ghostty"), true);
    assert_eq!(p.context.as_ref().unwrap().app, "ghostty");
    assert_eq!(p.context.unwrap().title, None);
    assert_eq!(e.resolve(None, false), Default::default());
    let double = Arc::new(TestContext::new(Some(window(
        "slack",
        "slack",
        "synthetic",
    ))));
    let e = ContextEngine::new(ContextConfig::default(), double.clone());
    let p = e.resolve(None, true);
    double.set(None);
    assert_eq!(
        p.context.unwrap().app,
        "slack",
        "resolved decisions must remain immutable"
    );
}

#[test]
fn unknown_context_keys_warn_once_at_load_with_the_profile_name() {
    use std::io::Write;
    use std::sync::Mutex;
    #[derive(Clone)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let sink = Sink(bytes.clone());
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || sink.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let c = config("enabled=true\nunknown_option=true\n[[profiles]]\nname='named-profile'\nmatch={class='slack'}\nunknown_override=true\ninject='off'");
        for _ in 0..3 {
            assert_eq!(
                c.resolve(Some(&window("slack", "slack", "synthetic")))
                    .inject,
                Some(ContextInjection::Off)
            );
        }
    });
    let log = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert_eq!(log.matches("unknown_option").count(), 1, "{log}");
    assert_eq!(log.matches("unknown_override").count(), 1, "{log}");
    assert!(log.contains("named-profile"), "{log}");
}

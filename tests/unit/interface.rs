use super::*;
use crate::test_fixtures;

#[test]
fn target_label_redacts_url_credentials_path_and_query() {
    for language in [Language::En, Language::Ru] {
        assert_eq!(
            target_label(
                language,
                "https://user:password@example.org/private/token?key=secret"
            ),
            "example.org"
        );
        assert_eq!(
            target_label(language, "private-token"),
            test_fixtures::messages(language)["invalid_target"]
        );
    }
}

#[test]
fn untrusted_names_cannot_control_the_terminal() {
    assert_eq!(display_text("A\r\n\u{1b}\u{202e}B"), "AB");
}

#[test]
fn phone_menu_rows_fit_without_losing_full_detail_text() {
    let label = "🟢 Test node with a long name for a narrow screen";
    let compact = menu_line(label, 28);
    assert!(console::measure_text_width(&compact) <= 24);
    assert!(compact.ends_with('…'));
    assert_eq!(menu_line(label, 100), label);
}

#[test]
fn handover_displays_each_listener_with_its_assigned_protocol() {
    assert_eq!(ports_label(Some(10808), None), "HTTP 127.0.0.1:10808");
    assert_eq!(ports_label(None, Some(10809)), "SOCKS5 127.0.0.1:10809");
    assert_eq!(
        ports_label(Some(10808), Some(10809))
            .lines()
            .collect::<Vec<_>>(),
        ["SOCKS5 127.0.0.1:10809", "HTTP 127.0.0.1:10808"]
    );
}

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::tui::app::App;

use super::*;

#[test]
fn render_create_form_does_not_panic() {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.open_create_form();

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf
        .content()
        .iter()
        .map(|c| c.symbol().to_owned())
        .collect();
    assert!(content.contains("Create VM"), "should contain title");
    assert!(content.contains("Image"), "should contain Image label");
    assert!(content.contains("Memory"), "should contain Memory label");
    assert!(content.contains("Create"), "should show Create button");
    assert!(content.contains("Cancel"), "should show Cancel button");
    assert!(content.contains("Esc"), "should show Esc hint");
}

#[test]
fn render_without_form_does_not_panic() {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let app = App::new("http://127.0.0.1:7800".to_owned());

    terminal.draw(|frame| render(frame, &app)).unwrap();
}

#[test]
fn render_form_shows_preset_defaults() {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.open_create_form();

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf
        .content()
        .iter()
        .map(|c| c.symbol().to_owned())
        .collect();
    assert!(
        content.contains("alpine:latest"),
        "should show default image"
    );
    assert!(content.contains("128 MiB"), "should show default memory");
    assert!(content.contains("vCPUs"), "should show vCPUs label");
}

#[test]
fn render_form_with_error_shows_error() {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.open_create_form();
    if let Some(form) = app.create_form_mut() {
        form.error = Some("Image is required".to_owned());
    }

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf
        .content()
        .iter()
        .map(|c| c.symbol().to_owned())
        .collect();
    assert!(
        content.contains("Image is required"),
        "should show error message"
    );
}

#[test]
fn render_form_narrow_terminal() {
    let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.open_create_form();

    terminal.draw(|frame| render(frame, &app)).unwrap();
}

#[test]
fn render_form_shows_select_arrows() {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.open_create_form();

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf
        .content()
        .iter()
        .map(|c| c.symbol().to_owned())
        .collect();
    assert!(
        content.contains('\u{25C0}'),
        "should show left arrow on select"
    );
    assert!(
        content.contains('\u{25B6}'),
        "should show right arrow/indicator"
    );
}

#[test]
fn render_form_button_row_highlighted() {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.open_create_form();
    if let Some(form) = app.create_form_mut() {
        form.selected_row = 5;
    }

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf
        .content()
        .iter()
        .map(|c| c.symbol().to_owned())
        .collect();
    assert!(content.contains("Create"), "should show Create button");
    assert!(content.contains("Cancel"), "should show Cancel button");
}

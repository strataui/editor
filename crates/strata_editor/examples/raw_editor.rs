//! Minimal standalone raw embeddable `strata-editor` example.
//!
//! Demonstrates embedding `<CodeEditorWidget>` with zero IDE chrome, visible 2px inline caret,
//! `cursor-text` I-beam pointer, double-click/triple-click drag selection, `Alt+Click` multi-cursor,
//! column box selection (`Shift+Alt+Drag`), auto-closing pairs, `LanguageConfig` commenting,
//! and floating top-right inline Find & Replace (`Ctrl+F` / `Ctrl+H`).

use strata::platform::WindowConfig;
use strata::prelude::*;
use strata_editor::{
    render_interactive_editor, CodeEditorHandle, CodeEditorOptions, EditorTheme, LanguageConfig,
    PlainTextSyntaxProvider,
};

const SAMPLE_BUFFER: &str = r#"// Raw strata-editor microkernel demo
fn compute_sum(values: &[i32]) -> i32 {
    let mut total = 0;
    for item in values {
        total += *item;
    }
    total
}

fn main() {
    let nums = [10, 20, 30, 40];
    let result = compute_sum(&nums);
    println!("sum = {}", result);
}
"#;

fn main() -> Result<(), String> {
    let mut cfg = LanguageConfig::plaintext();
    cfg.display_name = "Raw Text (// comments)".to_string();
    cfg.line_comment = Some("//".to_string());
    cfg.block_comment = Some(("/*".to_string(), "*/".to_string()));

    let handle = CodeEditorHandle::with_provider(
        SAMPLE_BUFFER,
        cfg,
        Box::new(PlainTextSyntaxProvider::new()),
    );
    handle.with_editor_mut(|ed| {
        ed.set_auto_closing_pairs(true);
        ed.cursors_mut()
            .set_single(strata_editor::Position::new(2, 8));
    });

    let config = WindowConfig {
        title: "Strata Editor — Raw Embeddable Demo".to_string(),
        width: 960,
        height: 620,
        ..WindowConfig::default()
    };

    let editor_handle = handle.clone();
    strata::launch_with_config(
        move || {
            let options = CodeEditorOptions {
                show_line_numbers: true,
                show_fold_gutter: true,
                show_minimap: true,
                show_status_bar: true,
                show_sticky_scroll: true,
                read_only: false,
                font_size: 13.0,
                line_height: 22.0,
                viewport_height: None,
                theme: Some(EditorTheme::obsidian_dark()),
                class: "flex-1 min-h-0 rounded-none border-0".to_string(),
                ..CodeEditorOptions::default()
            };
            View::new(NodeBlueprint::element(
                Tag::Div,
                vec!["w-full h-full flex flex-col bg-[#1e1e1e] text-[#d4d4d4]".to_string()],
                Vec::new(),
                vec![render_interactive_editor(&editor_handle, &options)],
            ))
        },
        config,
    )
}

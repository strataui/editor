//! Integration tests for the decoupled Slate raw microkernel (`strata-editor`).
//!
//! These tests compile and pass with BOTH default features AND `--no-default-features`
//! (zero built-in grammars / zero `tree-sitter` dependencies).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use strata_dom::{Event, EventType, NodeBlueprint};
use strata_editor::{
    render_interactive_editor, CodeEditor, CodeEditorHandle, CodeEditorOptions, ColorProvider,
    CompletionItem, CompletionItemKind, CompletionPopupState, FoldRange, GutterMarker,
    GutterMarkerKind, HexColorProvider, HoverInfo, HoverPopupState, InlayHint, LanguageConfig,
    LanguageId, LanguageRegistry, LineBuffer, PlainTextSyntaxProvider, Position, Selection,
    SyntaxDiagnostic, SyntaxProvider, SyntaxToken, TokenType,
};

/// Minimal custom syntax provider verifying that external language plugins can drive
/// highlighting, folding, diagnostics, and comments with zero core knowledge of the language.
#[derive(Clone)]
struct CustomDslProvider;

impl SyntaxProvider for CustomDslProvider {
    fn language_id(&self) -> LanguageId {
        LanguageId::new("custom-dsl")
    }

    fn clone_box(&self) -> Box<dyn SyntaxProvider> {
        Box::new(self.clone())
    }

    fn parse_full(&mut self, _buffer: &LineBuffer) {}

    fn tokenize_line(&self, _line_idx: usize, line_text: &str) -> Vec<SyntaxToken> {
        let mut out = Vec::new();
        let mut byte = 0usize;
        for part in line_text.split_inclusive(|c: char| c.is_whitespace()) {
            let trimmed = part.trim_end();
            if !trimmed.is_empty() {
                let tt = if trimmed == "rule" || trimmed == "end" {
                    TokenType::Keyword
                } else if trimmed.starts_with('#') {
                    TokenType::Comment
                } else {
                    TokenType::Identifier
                };
                out.push(SyntaxToken {
                    token_type: tt,
                    start_col: byte,
                    end_col: byte + trimmed.len(),
                    text: trimmed.to_string(),
                });
            }
            byte += part.len();
        }
        out
    }

    fn fold_ranges(&self, buffer: &LineBuffer) -> Vec<FoldRange> {
        let mut ranges = Vec::new();
        let mut stack = Vec::new();
        for i in 0..buffer.line_count() {
            let line = buffer.line(i).unwrap_or("").trim();
            if line.starts_with("rule ") {
                stack.push(i);
            } else if line == "end" {
                if let Some(start) = stack.pop() {
                    if i > start {
                        ranges.push(FoldRange {
                            start_line: start,
                            end_line: i,
                            collapsed: false,
                        });
                    }
                }
            }
        }
        ranges
    }

    fn syntax_diagnostics(&self, buffer: &LineBuffer) -> Vec<SyntaxDiagnostic> {
        let mut diags = Vec::new();
        for i in 0..buffer.line_count() {
            let line = buffer.line(i).unwrap_or("");
            if let Some(idx) = line.find("FORBIDDEN") {
                diags.push(SyntaxDiagnostic {
                    start: Position::new(i, idx),
                    end: Position::new(i, idx + 9),
                    message: "Forbidden keyword in custom-dsl".to_string(),
                    is_warning: false,
                });
            }
        }
        diags
    }
}

fn find_attr_values(node: &NodeBlueprint, attr_name: &str, out: &mut Vec<String>) {
    if let NodeBlueprint::Element {
        attributes,
        children,
        ..
    } = node
    {
        for a in attributes {
            if let strata_dom::Attribute::Property { key, value } = a {
                if key == attr_name {
                    out.push(value.clone());
                }
            }
        }
        for child in children {
            find_attr_values(child, attr_name, out);
        }
    }
}

fn contains_text(node: &NodeBlueprint, needle: &str) -> bool {
    match node {
        NodeBlueprint::Text(s) => s.contains(needle),
        NodeBlueprint::Element { children, .. } => children.iter().any(|c| contains_text(c, needle)),
        _ => false,
    }
}

#[test]
fn test_raw_microkernel_editing_and_incremental_edit_deltas() {
    let mut ed = CodeEditor::new_raw("hello world");
    assert_eq!(ed.language_id(), &LanguageId::plaintext());
    assert_eq!(ed.language_display_name(), "Plain Text");

    ed.cursors_mut().set_single(Position::new(0, 5));
    ed.insert_text(", slate");
    assert_eq!(ed.text(), "hello, slate world");

    let deltas = ed.take_edit_deltas();
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].start_byte, 5);
    assert_eq!(deltas[0].old_end_byte, 5);
    assert_eq!(deltas[0].new_end_byte, 12);
    assert_eq!(deltas[0].inserted_text, ", slate");

    // Draining again returns empty until the next edit.
    assert!(ed.take_edit_deltas().is_empty());

    // Undo also emits pure-Rust TextEditDelta without tree-sitter.
    ed.undo();
    assert_eq!(ed.text(), "hello world");
    let undo_deltas = ed.take_edit_deltas();
    assert_eq!(undo_deltas.len(), 1);
    assert_eq!(undo_deltas[0].start_byte, 5);
    assert_eq!(undo_deltas[0].old_end_byte, 12);
    assert_eq!(undo_deltas[0].new_end_byte, 5);

    ed.redo();
    assert_eq!(ed.text(), "hello, slate world");
}

#[test]
fn test_language_registry_and_pluggable_syntax_provider() {
    let registry = LanguageRegistry::new();
    let cfg = LanguageConfig {
        id: LanguageId::new("custom-dsl"),
        display_name: "Custom DSL".to_string(),
        extensions: vec!["cdsl".to_string()],
        filenames: vec!["Rulesfile".to_string()],
        line_comment: Some("#".to_string()),
        block_comment: None,
        brackets: vec![('(', ')'), ('{', '}')],
        auto_closing_pairs: vec![('(', ')'), ('"', '"')],
        indent_size: 2,
    };

    registry.register_language(
        cfg.clone(),
        Arc::new(|| Box::new(CustomDslProvider)),
    );

    assert_eq!(
        registry.detect_language("src/pipeline.cdsl"),
        LanguageId::new("custom-dsl")
    );
    assert_eq!(
        registry.detect_language("/workspace/Rulesfile"),
        LanguageId::new("custom-dsl")
    );
    assert_eq!(registry.detect_language("notes.unknown"), LanguageId::plaintext());

    let src = "rule alpha\n  FORBIDDEN\nend";
    let provider = registry.create_syntax_provider(&LanguageId::new("custom-dsl"));
    let mut ed = CodeEditor::with_provider(src, cfg, provider);

    assert_eq!(ed.language_display_name(), "Custom DSL");
    assert_eq!(ed.tab_size(), 2);

    let toks = ed.tokenize_line(0, "rule alpha");
    assert_eq!(toks.len(), 2);
    assert_eq!(toks[0].token_type, TokenType::Keyword);
    assert_eq!(toks[0].text, "rule");

    let folds = ed.fold_ranges();
    assert_eq!(folds.len(), 1);
    assert_eq!((folds[0].start_line, folds[0].end_line), (0, 2));

    // Live syntax diagnostic extracted from custom provider.
    assert_eq!(ed.diagnostic_counts().errors, 1);

    // Line comment uses custom LanguageConfig::line_comment ("#").
    ed.cursors_mut().set_single(Position::new(0, 0));
    ed.toggle_line_comment();
    assert!(ed.buffer().line(0).unwrap().starts_with("# rule alpha"));
}

#[test]
fn test_decorations_color_swatches_sticky_scroll_and_popups_render() {
    let src = [
        "struct Palette {",
        "    bg: \"#181818\",",
        "    accent: \"#007acc\",",
        "    fg: \"#cccccc\",",
        "    a: 1,",
        "    b: 2,",
        "    c: 3,",
        "    d: 4,",
        "    e: 5,",
        "    f: 6,",
        "    g: 7,",
        "    h: 8,",
        "    i: 9,",
        "    j: 10,",
        "    k: 11,",
        "    l: 12,",
        "}",
    ]
    .join("\n");

    let handle = CodeEditorHandle::new_raw(&src);

    // Detect hex colors via generic HexColorProvider and attach to DecorationStore.
    let color_provider = HexColorProvider;
    let swatches = handle.with_editor(|ed| {
        let mut all = Vec::new();
        for i in 0..ed.line_count() {
            if let Some(line) = ed.buffer().line(i) {
                all.extend(color_provider.provide_colors(i, line));
            }
        }
        all
    });
    assert_eq!(swatches.len(), 3);
    assert_eq!(swatches[0].css_color, "#181818");
    assert_eq!(swatches[1].css_color, "#007acc");

    handle.with_decorations_mut(|dec| {
        dec.set_color_swatches("hex-colors", swatches);
        dec.set_gutter_markers(
            "git",
            vec![
                GutterMarker {
                    line: 1,
                    kind: GutterMarkerKind::GitAdded,
                    tooltip: Some("Added line".to_string()),
                },
                GutterMarker {
                    line: 2,
                    kind: GutterMarkerKind::GitModified,
                    tooltip: Some("Modified line".to_string()),
                },
            ],
        );
        dec.set_gutter_markers(
            "debug",
            vec![GutterMarker {
                line: 2,
                kind: GutterMarkerKind::Breakpoint,
                tooltip: Some("Breakpoint".to_string()),
            }],
        );
        dec.set_active_debug_line(Some(2));
        dec.set_inlay_hints(
            "lsp",
            vec![InlayHint {
                line: 4,
                col: 6,
                label: ": i32".to_string(),
                is_type: true,
            }],
        );
        dec.completion = CompletionPopupState::open(
            1,
            4,
            vec![
                CompletionItem {
                    label: "background".to_string(),
                    insert_text: "background".to_string(),
                    kind: CompletionItemKind::Property,
                    detail: Some("Color token".to_string()),
                    documentation: None,
                    sort_priority: 0,
                },
                CompletionItem {
                    label: "border".to_string(),
                    insert_text: "border".to_string(),
                    kind: CompletionItemKind::Property,
                    detail: Some("Border token".to_string()),
                    documentation: None,
                    sort_priority: 1,
                },
            ],
        );
        dec.hover = HoverPopupState::show(
            1,
            4,
            HoverInfo {
                title: "Palette::bg".to_string(),
                markdown: "Primary workbench background token".to_string(),
                range: None,
            },
        );
    });

    // Scroll down to line 3 so Sticky Scroll pins `struct Palette {`.
    handle.with_editor_mut(|ed| {
        ed.set_line_height(22.0);
        ed.set_viewport_height(220.0);
        ed.set_scroll_top(66.0);
    });

    let clicked_line = Rc::new(RefCell::new(None::<usize>));
    let clicked_clone = clicked_line.clone();
    let opts = CodeEditorOptions {
        viewport_height: Some(220.0),
        show_sticky_scroll: true,
        on_gutter_click: Some(Rc::new(move |line| {
            *clicked_clone.borrow_mut() = Some(line);
        })),
        ..CodeEditorOptions::default()
    };

    let tree = render_interactive_editor(&handle, &opts);

    let mut swatches_rendered = Vec::new();
    find_attr_values(&tree, "data-color-swatch", &mut swatches_rendered);
    assert!(swatches_rendered.contains(&"#181818".to_string()));
    assert!(swatches_rendered.contains(&"#007acc".to_string()));

    let mut git_markers = Vec::new();
    find_attr_values(&tree, "data-git-gutter", &mut git_markers);
    assert!(git_markers.contains(&"added".to_string()));
    assert!(git_markers.contains(&"modified".to_string()));

    let mut breakpoints = Vec::new();
    find_attr_values(&tree, "data-breakpoint", &mut breakpoints);
    assert_eq!(breakpoints, vec!["active".to_string()]);

    let mut sticky = Vec::new();
    find_attr_values(&tree, "data-editor-sticky-scroll", &mut sticky);
    assert_eq!(sticky, vec!["true".to_string()]);

    let mut inlays = Vec::new();
    find_attr_values(&tree, "data-inlay-hint", &mut inlays);
    assert_eq!(inlays, vec![": i32".to_string()]);

    let mut comp = Vec::new();
    find_attr_values(&tree, "data-editor-completion-popup", &mut comp);
    assert_eq!(comp, vec!["true".to_string()]);

    let mut hover = Vec::new();
    find_attr_values(&tree, "data-editor-hover-popup", &mut hover);
    assert_eq!(hover, vec!["true".to_string()]);
    assert!(contains_text(&tree, "Palette::bg"));

    // Plugin source isolation: clearing "git" keeps "debug" breakpoint intact.
    handle.with_decorations_mut(|dec| dec.clear_source("git"));
    handle.with_editor(|ed| {
        assert_eq!(ed.decorations().gutter_markers_on_line(2).len(), 1);
        assert_eq!(
            ed.decorations().gutter_markers_on_line(2)[0].kind,
            GutterMarkerKind::Breakpoint
        );
    });
}

#[test]
fn test_completion_popup_keyboard_navigation_and_prefix_replacement() {
    let mut ed = CodeEditor::new_raw("let val = my_p");
    ed.cursors_mut().set_single(Position::new(0, 14));
    ed.decorations_mut().completion = CompletionPopupState::open(
        0,
        14,
        vec![
            CompletionItem {
                label: "my_alpha".to_string(),
                insert_text: "my_alpha()".to_string(),
                kind: CompletionItemKind::Function,
                detail: None,
                documentation: None,
                sort_priority: 0,
            },
            CompletionItem {
                label: "my_plugin".to_string(),
                insert_text: "my_plugin_host".to_string(),
                kind: CompletionItemKind::Variable,
                detail: None,
                documentation: None,
                sort_priority: 1,
            },
        ],
    );

    let mut down = Event::new(EventType::KeyDown);
    down.key = "ArrowDown".to_string();
    assert!(ed.handle_key_event(&down));
    assert_eq!(ed.decorations().completion.selected, 1);

    let mut enter = Event::new(EventType::KeyDown);
    enter.key = "Enter".to_string();
    assert!(ed.handle_key_event(&enter));

    assert!(!ed.decorations().completion.visible);
    assert_eq!(ed.text(), "let val = my_plugin_host");
}

#[test]
fn test_visible_2px_caret_cursor_text_and_pointer_events_none_spans() {
    let handle = CodeEditorHandle::new_raw("alpha beta\n\ngamma");
    handle.with_editor_mut(|ed| {
        ed.cursors_mut().set_single(Position::new(0, 3));
        ed.cursors_mut().add_cursor(Position::new(1, 0));
    });
    let tree = handle.render_interactive(&CodeEditorOptions::default());
    let dbg = format!("{tree:?}");
    assert!(
        dbg.contains("width: 2px;") && dbg.contains("margin-right: -2px;"),
        "Carets must render as real 2px painted Divs with -2px zero-jitter margin"
    );
    assert!(
        dbg.contains("cursor-text"),
        "Editor viewport and code rows must carry cursor-text I-beam styling"
    );
    assert!(
        dbg.contains("pointer-events-none"),
        "Static code spans must carry pointer-events-none to preserve viewport coordinate frame"
    );
    let mut carets = Vec::new();
    find_attr_values(&tree, "data-editor-caret-col", &mut carets);
    assert_eq!(carets, vec!["3".to_string(), "0".to_string()]);
}

#[test]
fn test_pointer_drag_modes_double_click_word_triple_click_line_column_and_multicursor_toggle() {
    let mut ed = CodeEditor::new_raw("one two three\nfour five six\nseven eight nine");
    ed.set_char_width(8.0);
    ed.set_line_height(22.0);
    ed.set_viewport_height(66.0);
    let gutter = 80.0;

    // 1. Double-click on "two" (col 5 -> x = gutter + 40) and drag right onto "three" (col 10)
    ed.handle_pointer_down_ex(gutter + 40.0, 5.0, 2, false, false, false, 0, gutter);
    assert_eq!(
        ed.cursors().primary().ordered_range(),
        (Position::new(0, 4), Position::new(0, 7))
    );
    ed.handle_pointer_drag(gutter + 80.0, 5.0, gutter);
    assert_eq!(
        ed.cursors().primary().ordered_range(),
        (Position::new(0, 4), Position::new(0, 13))
    );
    // Drag backwards onto "one" (col 1) -> expands backwards word-by-word while anchoring end of "two"!
    ed.handle_pointer_drag(gutter + 8.0, 5.0, gutter);
    assert_eq!(
        ed.cursors().primary().ordered_range(),
        (Position::new(0, 0), Position::new(0, 7))
    );
    ed.handle_pointer_up();

    // 2. Triple-click line 1 and drag down to line 2, then up to line 0
    ed.handle_pointer_down_ex(gutter + 16.0, 27.0, 3, false, false, false, 0, gutter);
    assert_eq!(
        ed.cursors().primary().ordered_range(),
        (Position::new(1, 0), Position::new(1, 13))
    );
    ed.handle_pointer_drag(gutter + 16.0, 50.0, gutter);
    assert_eq!(
        ed.cursors().primary().ordered_range(),
        (Position::new(1, 0), Position::new(2, 16))
    );
    ed.handle_pointer_drag(gutter + 16.0, 5.0, gutter);
    assert_eq!(
        ed.cursors().primary().ordered_range(),
        (Position::new(0, 0), Position::new(1, 13))
    );
    ed.handle_pointer_up();

    // 3. Alt+Click adds cursor, Alt+Click same spot toggles it off!
    ed.handle_pointer_down_ex(gutter + 16.0, 5.0, 1, false, false, false, 0, gutter);
    ed.handle_pointer_up();
    ed.handle_pointer_down_ex(gutter + 16.0, 27.0, 1, true, false, false, 0, gutter);
    assert_eq!(ed.cursors().count(), 2);
    // Clicking same position with Alt again removes that secondary cursor
    ed.handle_pointer_down_ex(gutter + 16.0, 27.0, 1, true, false, false, 0, gutter);
    assert_eq!(ed.cursors().count(), 1);

    // 4. Switch multi-cursor modifier to CtrlCmd -> Ctrl+Click adds cursor!
    assert_eq!(
        ed.toggle_multi_cursor_modifier(),
        strata_editor::MultiCursorModifier::CtrlCmd
    );
    ed.handle_pointer_down_ex(gutter + 16.0, 48.0, 1, false, false, true, 0, gutter);
    assert_eq!(ed.cursors().count(), 2);

    // 5. Shift+Alt+Drag column box selection across lines 0..=2 cols 0..4
    ed.handle_pointer_down_ex(gutter, 5.0, 1, true, true, false, 0, gutter);
    ed.handle_pointer_drag(gutter + 32.0, 50.0, gutter);
    ed.handle_pointer_up();
    assert_eq!(ed.cursors().count(), 3);
    assert_eq!(
        ed.cursors().all()[0].ordered_range(),
        (Position::new(0, 0), Position::new(0, 4))
    );
    assert_eq!(
        ed.cursors().all()[2].ordered_range(),
        (Position::new(2, 0), Position::new(2, 4))
    );
}

#[test]
fn test_language_config_comments_auto_closing_pairs_and_smart_enter() {
    // 1. HTML-like config with ONLY block_comment -> Ctrl+/ falls back to <!-- ... -->
    let mut html_cfg = LanguageConfig::plaintext();
    html_cfg.id = LanguageId::new("html");
    html_cfg.line_comment = None;
    html_cfg.block_comment = Some(("<!--".to_string(), "-->".to_string()));
    let mut ed = CodeEditor::with_provider(
        "  <div>hi</div>",
        html_cfg,
        Box::new(PlainTextSyntaxProvider::new()),
    );
    ed.cursors_mut().set_single(Position::new(0, 4));
    ed.toggle_line_comment();
    assert_eq!(ed.text(), "  <!-- <div>hi</div> -->");
    ed.toggle_line_comment();
    assert_eq!(ed.text(), "  <div>hi</div>");

    // 2. Python-like `#` comment preserves minimum common indent across multi-line selection
    let mut py_cfg = LanguageConfig::plaintext();
    py_cfg.id = LanguageId::new("python");
    py_cfg.line_comment = Some("#".to_string());
    let mut py = CodeEditor::with_provider(
        "  if True:\n      return 1",
        py_cfg,
        Box::new(PlainTextSyntaxProvider::new()),
    );
    py.cursors_mut().all_mut()[0] =
        Selection::range(Position::new(0, 0), Position::new(1, 10));
    py.toggle_line_comment();
    assert_eq!(py.text(), "  # if True:\n  #     return 1");
    py.toggle_line_comment();
    assert_eq!(py.text(), "  if True:\n      return 1");

    // 3. Auto-closing pairs: insert pair, skip closer, wrap selection, pair-aware backspace, smart enter
    let mut rust_cfg = LanguageConfig::plaintext();
    rust_cfg.line_comment = Some("//".to_string());
    rust_cfg.block_comment = Some(("/*".to_string(), "*/".to_string()));
    let mut re = CodeEditor::with_provider(
        "fn f",
        rust_cfg,
        Box::new(PlainTextSyntaxProvider::new()),
    );
    re.set_auto_closing_pairs(true);
    re.cursors_mut().set_single(Position::new(0, 4));
    re.insert_char('(');
    assert_eq!(re.text(), "fn f()");
    assert_eq!(re.cursors().primary().focus, Position::new(0, 5));
    // Skip over ')'
    re.insert_char(')');
    assert_eq!(re.text(), "fn f()");
    assert_eq!(re.cursors().primary().focus, Position::new(0, 6));
    // Empty pair backspace deletes both
    re.insert_char(' ');
    re.insert_char('{');
    assert_eq!(re.text(), "fn f() {}");
    // Smart Enter between `{` and `}`
    re.insert_newline();
    assert_eq!(re.text(), "fn f() {\n    \n}");
    assert_eq!(re.cursors().primary().focus, Position::new(1, 4));

    // Selection wrap with quotes
    re.set_text("hello");
    re.cursors_mut().all_mut()[0] = Selection::range(Position::new(0, 0), Position::new(0, 5));
    re.insert_char('"');
    assert_eq!(re.text(), "\"hello\"");

    // Block comment toggle via Ctrl+Shift+A
    re.set_text("let a = 1;");
    re.cursors_mut().all_mut()[0] = Selection::range(Position::new(0, 4), Position::new(0, 9));
    let mut blk = Event::new(EventType::KeyDown);
    blk.ctrl_key = true;
    blk.shift_key = true;
    blk.key = "A".to_string();
    assert!(re.handle_key_event(&blk));
    assert_eq!(re.text(), "let /* a = 1 */;");
}

#[test]
fn test_vscode_coding_keybindings_multicursor_paste_emmet_and_whitespace() {
    let mut ed = CodeEditor::new_raw("    foo\n    bar\n    foo");

    // Smart Home toggles between first non-ws (col 4) and col 0
    ed.cursors_mut().set_single(Position::new(0, 7));
    ed.move_to_line_start_smart(false);
    assert_eq!(ed.cursors().primary().focus, Position::new(0, 4));
    ed.move_to_line_start_smart(false);
    assert_eq!(ed.cursors().primary().focus, Position::new(0, 0));

    // Move line down / up
    ed.move_line_down();
    assert_eq!(ed.text(), "    bar\n    foo\n    foo");
    ed.move_line_up();
    assert_eq!(ed.text(), "    foo\n    bar\n    foo");

    // Copy line down & delete line
    ed.copy_line_down();
    assert_eq!(ed.line_count(), 4);
    ed.delete_line();
    assert_eq!(ed.text(), "    foo\n    bar\n    foo");

    // Ctrl+D selects word then adds next occurrence
    ed.cursors_mut().set_single(Position::new(0, 5));
    assert!(ed.add_next_occurrence());
    assert_eq!(ed.cursors().count(), 1);
    assert!(ed.add_next_occurrence());
    assert_eq!(ed.cursors().count(), 2);
    // Multi-cursor line-spread paste!
    assert!(ed.paste_text("ALPHA\nOMEGA"));
    assert_eq!(ed.text(), "    ALPHA\n    bar\n    OMEGA");

    // Expand / shrink selection inside parens
    ed.set_text("call(value)");
    ed.cursors_mut().set_single(Position::new(0, 7));
    ed.expand_selection();
    assert_eq!(
        ed.cursors().primary().ordered_range(),
        (Position::new(0, 5), Position::new(0, 10))
    );
    ed.expand_selection();
    assert_eq!(
        ed.cursors().primary().ordered_range(),
        (Position::new(0, 4), Position::new(0, 11))
    );
    ed.shrink_selection();
    assert_eq!(
        ed.cursors().primary().ordered_range(),
        (Position::new(0, 5), Position::new(0, 10))
    );

    // Emmet expansion
    ed.set_text("div.card#hero>span*2");
    ed.cursors_mut().set_single(Position::new(0, 20));
    assert!(ed.expand_emmet_abbreviation());
    assert_eq!(
        ed.text(),
        "<div id=\"hero\" class=\"card\"><span></span>\n<span></span></div>"
    );

    // Trim trailing whitespace & convert indentation
    ed.set_text("    one   \n    two\t ");
    assert_eq!(ed.trim_trailing_whitespace(), 2);
    assert_eq!(ed.text(), "    one\n    two");
    assert_eq!(ed.convert_indentation(false), 2);
    assert_eq!(ed.text(), "\tone\n\ttwo");

    // Find & Replace Preserve Case (AB)
    ed.set_text("Foo foo FOO");
    ed.open_replace();
    ed.set_find_query("foo");
    ed.set_replace_text("bar");
    ed.search_mut().toggle_preserve_case();
    assert_eq!(ed.replace_all(), 3);
    assert_eq!(ed.text(), "Bar bar BAR");

    // RenderWhitespace::All emits middle-dot `·` glyphs in rendered widget
    let handle = CodeEditorHandle::new_raw("  a b  ");
    handle.with_editor_mut(|e| e.set_render_whitespace(strata_editor::RenderWhitespace::All));
    let rendered = handle.render_interactive(&CodeEditorOptions::default());
    assert!(contains_text(&rendered, "··a·b··"));
}


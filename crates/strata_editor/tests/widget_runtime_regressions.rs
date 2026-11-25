//! End-to-end `strata-ui` `AppRuntime` regression tests for `<CodeEditorWidget>`
//! covering virtualized scrolling (#3), single-line code folding (#4), inline mid-line caret (#5),
//! Find/Replace & clipboard/fold keybindings + exact `on_change` (#6), status bar nowrap (#7),
//! and `ReceivedCharacter` / `Shift+Digit` / `Quote` / `Backslash` / `ImeCommit` (#2).

use std::cell::Cell;
use std::rc::Rc;

use strata::prelude::*;
use strata::platform::{KeyCode, Modifiers, PlatformEvent};
use strata_dom::{Attribute, NodeBlueprint};
use strata_editor::{
    render_interactive_editor, CodeEditorHandle, CodeEditorOptions, EditorTheme, Language,
};
use strata_layout::tree::LayoutNode;
use strata_layout::Rect;

fn flatten_bp<'a>(bp: &'a NodeBlueprint, out: &mut Vec<&'a NodeBlueprint>) {
    match bp {
        NodeBlueprint::Fragment(children) => {
            for c in children {
                flatten_bp(c, out);
            }
        }
        NodeBlueprint::Empty => {}
        other => out.push(other),
    }
}

fn find_box_by_prop(
    bp: &NodeBlueprint,
    layout: &LayoutNode,
    key: &str,
    val: &str,
) -> Option<Rect> {
    if let NodeBlueprint::Element {
        attributes,
        children,
        ..
    } = bp
    {
        let matched = attributes.iter().any(|a| {
            matches!(a, Attribute::Property { key: k, value: v } if k == key && v == val)
        });
        if matched {
            return Some(layout.geometry.border_box);
        }
        let mut flat = Vec::new();
        for c in children {
            flatten_bp(c, &mut flat);
        }
        for (child_bp, child_layout) in flat.iter().zip(layout.children.iter()) {
            if let Some(r) = find_box_by_prop(child_bp, child_layout, key, val) {
                return Some(r);
            }
        }
    } else if let NodeBlueprint::Fragment(children) = bp {
        let mut flat = Vec::new();
        for c in children {
            flatten_bp(c, &mut flat);
        }
        for (child_bp, child_layout) in flat.iter().zip(layout.children.iter()) {
            if let Some(r) = find_box_by_prop(child_bp, child_layout, key, val) {
                return Some(r);
            }
        }
    }
    None
}

fn find_node_pair_by_prop<'a>(
    bp: &'a NodeBlueprint,
    layout: &'a LayoutNode,
    key: &str,
    val: &str,
) -> Option<(&'a NodeBlueprint, &'a LayoutNode)> {
    if let NodeBlueprint::Element {
        attributes,
        children,
        ..
    } = bp
    {
        let matched = attributes.iter().any(|a| {
            matches!(a, Attribute::Property { key: k, value: v } if k == key && v == val)
        });
        if matched {
            return Some((bp, layout));
        }
        let mut flat = Vec::new();
        for c in children {
            flatten_bp(c, &mut flat);
        }
        for (child_bp, child_layout) in flat.iter().zip(layout.children.iter()) {
            if let Some(found) = find_node_pair_by_prop(child_bp, child_layout, key, val) {
                return Some(found);
            }
        }
    } else if let NodeBlueprint::Fragment(children) = bp {
        let mut flat = Vec::new();
        for c in children {
            flatten_bp(c, &mut flat);
        }
        for (child_bp, child_layout) in flat.iter().zip(layout.children.iter()) {
            if let Some(found) = find_node_pair_by_prop(child_bp, child_layout, key, val) {
                return Some(found);
            }
        }
    }
    None
}

fn count_style_substring(bp: &NodeBlueprint, needle: &str) -> usize {
    match bp {
        NodeBlueprint::Element {
            attributes,
            children,
            ..
        } => {
            let mut count = usize::from(attributes.iter().any(|a| {
                matches!(a, Attribute::Property { key, value } if key == "style" && value.contains(needle))
            }));
            for c in children {
                count += count_style_substring(c, needle);
            }
            count
        }
        NodeBlueprint::Fragment(children) => children
            .iter()
            .map(|c| count_style_substring(c, needle))
            .sum(),
        _ => 0,
    }
}

fn build_editor_view(handle: &CodeEditorHandle, options: &CodeEditorOptions) -> View {
    View::new(NodeBlueprint::element(
        Tag::Div,
        vec!["w-full h-full flex flex-col bg-[#09090b]".to_string()],
        Vec::new(),
        vec![render_interactive_editor(handle, options)],
    ))
}

#[test]
fn test_virtualized_scrolling_keeps_rows_visible_and_clicks_aligned_at_160px_and_480px() {
    let content = (0..120)
        .map(|i| format!("let line_{i:03} = {i}; // sample code line"))
        .collect::<Vec<_>>()
        .join("\n");
    let handle = CodeEditorHandle::new(&content, Language::Rust);
    let options = CodeEditorOptions {
        viewport_height: 240.0,
        line_height: 22.0,
        show_minimap: false,
        ..CodeEditorOptions::default()
    };

    let mut runtime = AppRuntime::new(build_editor_view(&handle, &options), 720.0, 320.0, 1.0);
    let vp = find_box_by_prop(
        &runtime.root_blueprint,
        &runtime.layout_root,
        "data-editor-viewport",
        "true",
    )
    .expect("editor viewport must exist");

    // 1. Scroll down 160px via wheel event: row 7 (`160 / 22 = 7`) must sit cleanly at top of viewport
    //    with ZERO blank top gap!
    runtime.dispatch_scroll(vp.x + 120.0, vp.y + 80.0, 0.0, 160.0);
    runtime.update_view(build_editor_view(&handle, &options));
    assert!(
        (handle.with_editor(|e| e.scroll_top()) - 160.0).abs() < 0.5,
        "Expected scroll_top == 160.0"
    );

    let row7 = find_box_by_prop(
        &runtime.root_blueprint,
        &runtime.layout_root,
        "data-editor-row",
        "7",
    )
    .expect("row 7 must be rendered at scroll_top=160");
    let expected_row7_y = vp.y + (7.0 * 22.0 - 160.0);
    assert!(
        (row7.y - expected_row7_y).abs() <= 1.5,
        "row 7 y ({}) must align with vp.y + 7*22 - 160 ({}), not pushed down by a blank top spacer",
        row7.y,
        expected_row7_y
    );

    // Click at vp.y + 50.0 -> (50 + 160) / 22 = 9.54 -> logical line 9!
    runtime.dispatch_pointer_down(vp.x + 120.0, vp.y + 50.0);
    runtime.dispatch_pointer_up(vp.x + 120.0, vp.y + 50.0);
    assert_eq!(
        handle.with_editor(|e| e.cursors().primary().focus.line),
        9,
        "Pointer click at scroll_top=160 must match rendered row 9"
    );

    // 2. Scroll down another 320px (total 480px): previously 100% blank!
    runtime.dispatch_scroll(vp.x + 120.0, vp.y + 80.0, 0.0, 320.0);
    runtime.update_view(build_editor_view(&handle, &options));
    assert!(
        (handle.with_editor(|e| e.scroll_top()) - 480.0).abs() < 0.5,
        "Expected scroll_top == 480.0"
    );

    let row22 = find_box_by_prop(
        &runtime.root_blueprint,
        &runtime.layout_root,
        "data-editor-row",
        "22",
    )
    .expect("row 22 must be rendered at scroll_top=480");
    let expected_row22_y = vp.y + (22.0 * 22.0 - 480.0);
    assert!(
        (row22.y - expected_row22_y).abs() <= 1.5
            && row22.y >= vp.y
            && row22.y < vp.y + vp.height,
        "row 22 ({:?}) must render cleanly inside viewport {:?} at scroll_top=480",
        row22,
        vp
    );
}

#[test]
fn test_folded_row_stays_single_line_horizontal_without_overlapping_next_line() {
    let mut lines = vec!["{".to_string(), "  \"scene\": {".to_string()];
    for i in 0..30 {
        lines.push(format!("    \"item_{i}\": {i},"));
    }
    lines.push("  }".to_string());
    lines.push("}".to_string());
    let content = lines.join("\n");

    let handle = CodeEditorHandle::new(&content, Language::Json);
    let options = CodeEditorOptions {
        viewport_height: 300.0,
        line_height: 22.0,
        show_minimap: false,
        ..CodeEditorOptions::default()
    };

    // Fold `"scene": {` (logical line 1 -> hides lines 2..=32)
    handle.with_editor_mut(|ed| {
        ed.toggle_fold_at(1);
    });
    assert!(
        handle.with_editor(|ed| ed.is_folded(1)),
        "Line 1 must be folded"
    );

    let runtime = AppRuntime::new(build_editor_view(&handle, &options), 720.0, 360.0, 1.0);
    let (row1_bp, row1_layout) = find_node_pair_by_prop(
        &runtime.root_blueprint,
        &runtime.layout_root,
        "data-editor-row",
        "1",
    )
    .expect("folded row 1 must exist");
    let row1_box = row1_layout.geometry.border_box;
    assert!(
        (row1_box.height - 22.0).abs() <= 1.0,
        "Folded row height must remain 22px, got {:?}",
        row1_box
    );

    // Every child span AND the fold pill inside row 1's code cell must sit on ONE horizontal row!
    let (_, code_cell_layout) =
        find_node_pair_by_prop(row1_bp, row1_layout, "data-line-index", "1")
            .expect("code cell for folded row 1");
    let mut prev_right = code_cell_layout.geometry.border_box.x;
    for child in &code_cell_layout.children {
        let cb = child.geometry.border_box;
        assert!(
            cb.y >= row1_box.y - 1.0 && cb.y + cb.height <= row1_box.y + row1_box.height + 1.0,
            "Code span/pill {:?} stacked outside folded row {:?}!",
            cb,
            row1_box
        );
        assert!(
            cb.x >= prev_right - 1.0,
            "Code span/pill {:?} wrapped below/behind previous right edge {}",
            cb,
            prev_right
        );
        prev_right = cb.x + cb.width;
    }

    // Fold chevron must stay aligned inside row 1 (not displaced downward!)
    let chevron_box =
        find_box_by_prop(row1_bp, row1_layout, "data-fold-toggle", "1").expect("fold chevron");
    assert!(
        chevron_box.y >= row1_box.y - 1.0
            && chevron_box.y + chevron_box.height <= row1_box.y + row1_box.height + 1.0,
        "Fold chevron {:?} must stay vertically inside folded row {:?}",
        chevron_box,
        row1_box
    );

    // Next visible row (`}` at logical line 33) must start right below row 1 with ZERO overlap
    let next_row_box = find_box_by_prop(
        &runtime.root_blueprint,
        &runtime.layout_root,
        "data-editor-row",
        "33",
    )
    .expect("closing brace row 33 must be rendered immediately after folded block");
    assert!(
        next_row_box.y >= row1_box.y + row1_box.height - 0.5,
        "Next visible row {:?} must not collide/overlap with folded row {:?}",
        next_row_box,
        row1_box
    );
}

#[test]
fn test_mid_line_caret_renders_inline_at_exact_column_not_at_eol() {
    let content = "  \"name\": \"Model 3\",\n";
    let handle = CodeEditorHandle::new(content, Language::Json);
    let options = CodeEditorOptions {
        viewport_height: 200.0,
        line_height: 22.0,
        show_minimap: false,
        ..CodeEditorOptions::default()
    };

    // Place caret mid-line at Ln 1, Col 7 (between `e` and `"` in `"name"`)
    handle.with_editor_mut(|ed| {
        ed.cursors_mut()
            .set_single(strata_editor::Position::new(0, 7));
    });
    let mut runtime = AppRuntime::new(build_editor_view(&handle, &options), 720.0, 260.0, 1.0);
    let caret_col7 = find_box_by_prop(
        &runtime.root_blueprint,
        &runtime.layout_root,
        "data-editor-caret-col",
        "7",
    )
    .expect("inline caret at col 7 must exist");
    let code_cell = find_box_by_prop(
        &runtime.root_blueprint,
        &runtime.layout_root,
        "data-line-index",
        "0",
    )
    .expect("line 0 code cell");

    // Move caret to EOL (Col 20) and compare x positions
    handle.with_editor_mut(|ed| {
        ed.cursors_mut()
            .set_single(strata_editor::Position::new(0, 20));
    });
    runtime.update_view(build_editor_view(&handle, &options));
    let caret_eol = find_box_by_prop(
        &runtime.root_blueprint,
        &runtime.layout_root,
        "data-editor-caret-col",
        "20",
    )
    .expect("EOL caret at col 20 must exist");

    assert!(
        caret_col7.x > code_cell.x + 20.0,
        "Col 7 caret ({:?}) must render after leading characters past code cell x ({})",
        caret_col7,
        code_cell.x
    );
    assert!(
        caret_eol.x > caret_col7.x + 50.0,
        "Col 7 caret ({:?}) must render strictly BEFORE EOL caret ({:?}), not at end of line",
        caret_col7,
        caret_eol
    );

    // Move caret to empty line 1, Col 0 -> caret still renders cleanly at start of line
    handle.with_editor_mut(|ed| {
        ed.cursors_mut()
            .set_single(strata_editor::Position::new(1, 0));
    });
    runtime.update_view(build_editor_view(&handle, &options));
    let caret_empty = find_box_by_prop(
        &runtime.root_blueprint,
        &runtime.layout_root,
        "data-editor-caret-col",
        "0",
    )
    .expect("empty line caret at col 0 must exist");
    assert!(
        caret_empty.x >= code_cell.x && caret_empty.x < caret_col7.x,
        "Empty-line col 0 caret ({:?}) must sit at the start of the code cell",
        caret_empty
    );
}

#[test]
fn test_find_replace_keybindings_clipboard_false_on_change_and_received_character() {
    let on_change_count = Rc::new(Cell::new(0usize));
    let oc = Rc::clone(&on_change_count);

    let initial = "fn main() {\n    let alpha = \"hello\";\n    let beta = alpha;\n}\n";
    let handle = CodeEditorHandle::new(initial, Language::Rust);
    let options = CodeEditorOptions {
        viewport_height: 240.0,
        line_height: 22.0,
        show_minimap: false,
        on_change: Some(Rc::new(move |_txt: String| {
            oc.set(oc.get() + 1);
        })),
        ..CodeEditorOptions::default()
    };

    let mut runtime = AppRuntime::new(build_editor_view(&handle, &options), 720.0, 320.0, 1.0);
    runtime.freeze_clock_at_epoch();

    // Focus the editor root by clicking inside viewport
    let vp = find_box_by_prop(
        &runtime.root_blueprint,
        &runtime.layout_root,
        "data-editor-viewport",
        "true",
    )
    .expect("viewport");
    runtime.dispatch_pointer_down(vp.x + 100.0, vp.y + 10.0);
    runtime.dispatch_pointer_up(vp.x + 100.0, vp.y + 10.0);
    assert!(runtime.current_focus_path.is_some());

    let ctrl = Modifiers {
        ctrl: true,
        ..Default::default()
    };
    let shift = Modifiers {
        shift: true,
        ..Default::default()
    };
    let ctrl_shift = Modifiers {
        ctrl: true,
        shift: true,
        ..Default::default()
    };

    // 1. Ctrl+F, Ctrl+H, Ctrl+A, Ctrl+C must NOT trigger false on_change callbacks!
    for key in [KeyCode::KeyF, KeyCode::KeyH, KeyCode::KeyA, KeyCode::KeyC] {
        assert!(runtime.step_event(&PlatformEvent::KeyDown {
            key,
            modifiers: ctrl,
        }));
    }
    assert_eq!(
        on_change_count.get(),
        0,
        "Ctrl+F, Ctrl+H, Ctrl+A, and Ctrl+C must NEVER trigger on_change on a clean file"
    );
    assert!(handle.with_editor(|e| e.search().is_open && e.search().show_replace));

    // 2. Search highlight renders when Find is open, and clears completely on Escape inside Find <input>
    handle.with_editor_mut(|ed| ed.set_find_query("alpha"));
    runtime.update_view(build_editor_view(&handle, &options));
    let theme = EditorTheme::default();
    assert!(
        count_style_substring(&runtime.root_blueprint, &theme.find_active_bg) > 0,
        "Open Find bar must highlight active match"
    );

    // Click Find <input> to focus it, then press Escape -> must close Find bar & clear highlights!
    let find_input_box = find_box_by_prop(
        &runtime.root_blueprint,
        &runtime.layout_root,
        "data-find-input",
        "query",
    )
    .expect("Find <input> must be rendered");
    runtime.dispatch_pointer_down(find_input_box.x + 12.0, find_input_box.y + 8.0);
    runtime.dispatch_pointer_up(find_input_box.x + 12.0, find_input_box.y + 8.0);
    assert!(runtime.active_focus.is_some());

    // Typing inside Find <input> must NOT bubble to the code editor!
    let before_text = handle.text();
    runtime.step_event(&PlatformEvent::KeyDown {
        key: KeyCode::KeyX,
        modifiers: Modifiers::default(),
    });
    assert_eq!(handle.text(), before_text);

    // Escape inside Find <input> closes Find bar & clears matches!
    assert!(runtime.step_event(&PlatformEvent::KeyDown {
        key: KeyCode::Escape,
        modifiers: Modifiers::default(),
    }));
    runtime.update_view(build_editor_view(&handle, &options));
    assert!(
        handle.with_editor(|e| !e.search().is_open && e.search().matches.is_empty()),
        "Escape inside Find <input> must close Find bar and clear search matches"
    );
    assert_eq!(
        count_style_substring(&runtime.root_blueprint, &theme.find_active_bg),
        0,
        "No lingering search match highlights may remain after closing Find"
    );
    assert_eq!(
        count_style_substring(&runtime.root_blueprint, &theme.find_match_bg),
        0,
        "No lingering secondary search match highlights may remain after closing Find"
    );

    // 3. Re-focus editor root (click line 0, col 0) and test ReceivedCharacter("\"'\\()!"),
    //    Shift+Digit9 -> '(', Quote, Backslash, ImeCommit, Ctrl+C/X/V, and Ctrl+Shift+[/]!
    runtime.dispatch_pointer_down(vp.x + 80.0, vp.y + 10.0);
    runtime.dispatch_pointer_up(vp.x + 80.0, vp.y + 10.0);
    handle.with_editor_mut(|ed| ed.set_text(""));
    runtime.update_view(build_editor_view(&handle, &options));

    for ch in "\"'\\()!".chars() {
        assert!(runtime.step_event(&PlatformEvent::ReceivedCharacter(ch)));
    }
    assert_eq!(handle.text(), "\"'\\()!");
    assert!(
        on_change_count.get() >= 6,
        "Real text insertion must trigger on_change"
    );

    // Shift+Digit9, Quote, Shift+Quote, Backslash, Shift+Backslash
    assert!(runtime.step_event(&PlatformEvent::KeyDown {
        key: KeyCode::Digit9,
        modifiers: shift,
    }));
    assert!(runtime.step_event(&PlatformEvent::KeyDown {
        key: KeyCode::Quote,
        modifiers: Modifiers::default(),
    }));
    assert!(runtime.step_event(&PlatformEvent::KeyDown {
        key: KeyCode::Quote,
        modifiers: shift,
    }));
    assert!(runtime.step_event(&PlatformEvent::KeyDown {
        key: KeyCode::Backslash,
        modifiers: Modifiers::default(),
    }));
    assert!(runtime.step_event(&PlatformEvent::KeyDown {
        key: KeyCode::Backslash,
        modifiers: shift,
    }));
    assert_eq!(handle.text(), "\"'\\()!('\"\\|");

    // ImeCommit
    assert!(runtime.step_event(&PlatformEvent::ImeCommit("世界".to_string())));
    assert_eq!(handle.text(), "\"'\\()!('\"\\|世界");

    // Ctrl+A, Ctrl+X, Ctrl+V round-trip through unified clipboard
    assert!(runtime.step_event(&PlatformEvent::KeyDown {
        key: KeyCode::KeyA,
        modifiers: ctrl,
    }));
    assert!(runtime.step_event(&PlatformEvent::KeyDown {
        key: KeyCode::KeyX,
        modifiers: ctrl,
    }));
    assert_eq!(handle.text(), "");
    assert!(runtime.step_event(&PlatformEvent::KeyDown {
        key: KeyCode::KeyV,
        modifiers: ctrl,
    }));
    assert_eq!(handle.text(), "\"'\\()!('\"\\|世界");

    // Ctrl+Shift+[ / Ctrl+Shift+] (Strata sends "{" / "}" with shift_key = true)
    handle.with_editor_mut(|ed| {
        ed.set_text("fn demo() {\n    let x = 1;\n    let y = 2;\n}\n");
        ed.cursors_mut()
            .set_single(strata_editor::Position::new(0, 0));
    });
    assert!(runtime.step_event(&PlatformEvent::KeyDown {
        key: KeyCode::BracketLeft,
        modifiers: ctrl_shift,
    }));
    assert!(
        handle.with_editor(|ed| ed.is_folded(0)),
        "Ctrl+Shift+[ must fold block even when Strata emits '{{' with shift_key=true"
    );
    assert!(runtime.step_event(&PlatformEvent::KeyDown {
        key: KeyCode::BracketRight,
        modifiers: ctrl_shift,
    }));
    assert!(
        handle.with_editor(|ed| !ed.is_folded(0)),
        "Ctrl+Shift+] must unfold block even when Strata emits '}}' with shift_key=true"
    );
}

#[test]
fn test_status_bar_spaces_label_stays_single_line_without_wrapping() {
    let handle = CodeEditorHandle::new("let a = 1;\n", Language::Rust);
    let options = CodeEditorOptions {
        viewport_height: 180.0,
        line_height: 22.0,
        show_minimap: false,
        show_status_bar: true,
        ..CodeEditorOptions::default()
    };

    // Render at a compact 420px width (like a floating ScriptEditor panel)
    let runtime = AppRuntime::new(build_editor_view(&handle, &options), 420.0, 240.0, 1.0);
    let spaces_box = find_box_by_prop(
        &runtime.root_blueprint,
        &runtime.layout_root,
        "data-status-spaces",
        "true",
    )
    .expect("status bar Spaces label must exist");
    let status_bar_box = find_box_by_prop(
        &runtime.root_blueprint,
        &runtime.layout_root,
        "data-editor-statusbar",
        "true",
    )
    .expect("status bar must exist");

    assert!(
        spaces_box.height <= 22.0,
        "Status bar 'Spaces: 4' must stay single-line (<=22px), got {:?}",
        spaces_box
    );
    assert!(
        spaces_box.y + spaces_box.height <= status_bar_box.y + status_bar_box.height + 1.0,
        "Status bar 'Spaces: 4' ({:?}) must not clip past status bar {:?}",
        spaces_box,
        status_bar_box
    );
}

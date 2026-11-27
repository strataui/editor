//! Drop-in interactive Monaco-class `<CodeEditorWidget>` component, reactive `CodeEditorHandle`,
//! and `use_code_editor` hook.
//!
//! Renders a virtualized, fold-aware viewport with line numbers, clickable fold chevrons,
//! diagnostic gutter indicators & inline squiggles, Find & Replace overlay bar, matching
//! bracket highlights, multi-cursor carets & selections, Canvas2D minimap with scrub-to-scroll,
//! Monaco status bar, and full `--strata-editor-*` CSS-variable theming.

#![allow(non_snake_case)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use strata_dom::{
    use_ref, use_signal, Attribute, Event, EventType, NodeBlueprint, Signal, Tag, View,
};
use strata_macro::component;
use strata_render::canvas2d::CanvasCommand;

use crate::code_editor::{CodeEditor, RenderWhitespace};
use crate::decorations::{DecorationStore, GutterMarkerKind};
use crate::diagnostics::{DiagnosticCounts, DiagnosticSeverity, EditorDiagnostic};
use crate::language::{
    DetectedColor, InlayHint, LanguageConfig, LanguageId, SyntaxProvider, SyntaxToken, TokenType,
};
use crate::minimap::{render_minimap_canvas_commands, MinimapConfig};
#[cfg(feature = "builtin-syntax")]
use crate::syntax::Language;
use crate::theme::EditorTheme;

/// Visual and behavioral configuration for interactive `<CodeEditorWidget>` rendering.
#[derive(Clone)]
pub struct CodeEditorOptions {
    pub show_line_numbers: bool,
    pub show_fold_gutter: bool,
    pub show_minimap: bool,
    pub show_status_bar: bool,
    pub show_sticky_scroll: bool,
    pub read_only: bool,
    pub font_size: f32,
    pub line_height: f32,
    /// Explicit pixel height for the code viewport, or `None` (default) to automatically
    /// measure the laid-out `flex-1 min-h-0` container height via resize observer.
    pub viewport_height: Option<f32>,
    /// Controls cursor blink visibility (`true` = visible, `false` = blink-off phase).
    pub caret_visible: bool,
    /// Caret geometry style (`"line"`, `"block"`, `"underline"`).
    pub cursor_style: String,
    pub minimap: MinimapConfig,
    pub theme: Option<EditorTheme>,
    pub class: String,
    pub on_change: Option<Rc<dyn Fn(String)>>,
    pub on_gutter_click: Option<Rc<dyn Fn(usize)>>,
    pub on_completion_accept: Option<Rc<dyn Fn(String)>>,
    pub on_context_menu: Option<Rc<dyn Fn(f32, f32)>>,
    pub on_focus: Option<Rc<dyn Fn()>>,
}

impl std::fmt::Debug for CodeEditorOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeEditorOptions")
            .field("show_line_numbers", &self.show_line_numbers)
            .field("show_fold_gutter", &self.show_fold_gutter)
            .field("show_minimap", &self.show_minimap)
            .field("show_status_bar", &self.show_status_bar)
            .field("show_sticky_scroll", &self.show_sticky_scroll)
            .field("read_only", &self.read_only)
            .field("font_size", &self.font_size)
            .field("line_height", &self.line_height)
            .field("viewport_height", &self.viewport_height)
            .field("caret_visible", &self.caret_visible)
            .field("cursor_style", &self.cursor_style)
            .field("minimap", &self.minimap)
            .field("theme", &self.theme)
            .field("class", &self.class)
            .field("on_change", &self.on_change.as_ref().map(|_| "<callback>"))
            .field(
                "on_gutter_click",
                &self.on_gutter_click.as_ref().map(|_| "<callback>"),
            )
            .field(
                "on_completion_accept",
                &self.on_completion_accept.as_ref().map(|_| "<callback>"),
            )
            .field(
                "on_context_menu",
                &self.on_context_menu.as_ref().map(|_| "<callback>"),
            )
            .field("on_focus", &self.on_focus.as_ref().map(|_| "<callback>"))
            .finish()
    }
}

impl Default for CodeEditorOptions {
    fn default() -> Self {
        Self {
            show_line_numbers: true,
            show_fold_gutter: true,
            show_minimap: true,
            show_status_bar: true,
            show_sticky_scroll: true,
            read_only: false,
            font_size: 13.0,
            line_height: 22.0,
            viewport_height: None,
            caret_visible: true,
            cursor_style: "line".to_string(),
            minimap: MinimapConfig::default(),
            theme: None,
            class: String::new(),
            on_change: None,
            on_gutter_click: None,
            on_completion_accept: None,
            on_context_menu: None,
            on_focus: None,
        }
    }
}

/// Reactive shared handle to a [`CodeEditor`] instance.
///
/// Wraps `Rc<RefCell<CodeEditor>>` alongside a reactive `Signal<u64>` revision counter so
/// editing, scrolling, folding, and Find & Replace trigger immediate fine-grained UI updates
/// without cloning the underlying B-Tree text rope.
#[derive(Clone)]
pub struct CodeEditorHandle {
    inner: Rc<RefCell<CodeEditor>>,
    revision: Signal<u64>,
    minimap_dragging: Rc<std::cell::Cell<bool>>,
    measured_viewport_height: Rc<std::cell::Cell<Option<f32>>>,
}

impl std::fmt::Debug for CodeEditorHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ed = self.inner.borrow();
        f.debug_struct("CodeEditorHandle")
            .field("language_id", ed.language_id())
            .field("lines", &ed.line_count())
            .field("revision", &self.revision.get())
            .field("measured_viewport_height", &self.measured_viewport_height.get())
            .finish()
    }
}

impl CodeEditorHandle {
    /// Creates a raw, zero-grammar handle initialized with `initial_text`.
    pub fn new_raw(initial_text: &str) -> Self {
        Self {
            inner: Rc::new(RefCell::new(CodeEditor::new_raw(initial_text))),
            revision: Signal::new(0),
            minimap_dragging: Rc::new(std::cell::Cell::new(false)),
            measured_viewport_height: Rc::new(std::cell::Cell::new(None)),
        }
    }

    /// Creates a standalone handle initialized with a pluggable [`LanguageConfig`] and [`SyntaxProvider`].
    pub fn with_provider(
        initial_text: &str,
        config: LanguageConfig,
        provider: Box<dyn SyntaxProvider>,
    ) -> Self {
        Self {
            inner: Rc::new(RefCell::new(CodeEditor::with_provider(
                initial_text,
                config,
                provider,
            ))),
            revision: Signal::new(0),
            minimap_dragging: Rc::new(std::cell::Cell::new(false)),
            measured_viewport_height: Rc::new(std::cell::Cell::new(None)),
        }
    }

    /// Creates a new standalone handle initialized with `initial_text` and a built-in [`Language`].
    #[cfg(feature = "builtin-syntax")]
    pub fn new(initial_text: &str, language: Language) -> Self {
        Self {
            inner: Rc::new(RefCell::new(CodeEditor::new(initial_text, language))),
            revision: Signal::new(0),
            minimap_dragging: Rc::new(std::cell::Cell::new(false)),
            measured_viewport_height: Rc::new(std::cell::Cell::new(None)),
        }
    }

    fn from_parts(
        inner: Rc<RefCell<CodeEditor>>,
        revision: Signal<u64>,
        minimap_dragging: Rc<std::cell::Cell<bool>>,
        measured_viewport_height: Rc<std::cell::Cell<Option<f32>>>,
    ) -> Self {
        Self {
            inner,
            revision,
            minimap_dragging,
            measured_viewport_height,
        }
    }

    /// Returns the last auto-measured viewport container height (in logical px) when
    /// rendered with `CodeEditorOptions::viewport_height == None`.
    pub fn measured_viewport_height(&self) -> Option<f32> {
        self.measured_viewport_height.get()
    }

    /// Reads the reactive revision signal (subscribes callers to editor updates).
    pub fn version(&self) -> u64 {
        self.revision.get()
    }

    /// Bumps the reactive revision counter to schedule a repaint.
    pub fn notify(&self) {
        let next = self.revision.get().wrapping_add(1);
        self.revision.set(next);
    }

    /// Executes a read-only closure against the inner [`CodeEditor`].
    pub fn with_editor<R>(&self, f: impl FnOnce(&CodeEditor) -> R) -> R {
        let _ = self.revision.get();
        let guard = self.inner.borrow();
        f(&guard)
    }

    /// Executes a mutating closure against the inner [`CodeEditor`] and notifies subscribers.
    pub fn with_editor_mut<R>(&self, f: impl FnOnce(&mut CodeEditor) -> R) -> R {
        let result = {
            let mut guard = self.inner.borrow_mut();
            f(&mut guard)
        };
        self.notify();
        result
    }

    /// Mutates the plugin-isolated [`DecorationStore`] and triggers a reactive repaint.
    pub fn with_decorations_mut<R>(&self, f: impl FnOnce(&mut DecorationStore) -> R) -> R {
        self.with_editor_mut(|ed| f(ed.decorations_mut()))
    }

    /// Replaces the active [`LanguageConfig`] and [`SyntaxProvider`] dynamically.
    pub fn set_syntax_provider(&self, config: LanguageConfig, provider: Box<dyn SyntaxProvider>) {
        self.with_editor_mut(|ed| ed.set_syntax_provider(config, provider));
    }

    /// Returns the active decoupled [`LanguageId`].
    pub fn language_id(&self) -> LanguageId {
        self.with_editor(|ed| ed.language_id().clone())
    }

    /// Returns the display name of the active language.
    pub fn language_display_name(&self) -> String {
        self.with_editor(|ed| ed.language_display_name().to_string())
    }

    /// Returns the full document text.
    pub fn text(&self) -> String {
        self.with_editor(CodeEditor::text)
    }

    /// Replaces document contents and resets history.
    pub fn set_text(&self, text: &str) {
        self.with_editor_mut(|ed| ed.set_text(text));
    }

    /// Returns the active built-in language.
    #[cfg(feature = "builtin-syntax")]
    pub fn language(&self) -> Language {
        self.with_editor(CodeEditor::language)
    }

    /// Changes syntax language using the built-in grammar engine and re-parses the buffer.
    #[cfg(feature = "builtin-syntax")]
    pub fn set_language(&self, language: Language) {
        self.with_editor_mut(|ed| ed.set_language(language));
    }

    /// Updates the active CSS-variable [`EditorTheme`].
    pub fn set_theme(&self, theme: EditorTheme) {
        self.with_editor_mut(|ed| ed.set_theme(theme));
    }

    /// Updates external/LSP diagnostics without clobbering live syntax diagnostics.
    pub fn set_diagnostics(&self, diagnostics: Vec<EditorDiagnostic>) {
        self.with_editor_mut(|ed| ed.set_external_diagnostics(diagnostics));
    }

    /// Returns diagnostic severity counts.
    pub fn diagnostic_counts(&self) -> DiagnosticCounts {
        self.with_editor(CodeEditor::diagnostic_counts)
    }

    /// Opens the Find bar.
    pub fn open_find(&self) {
        self.with_editor_mut(CodeEditor::open_find);
    }

    /// Opens the Find & Replace bar.
    pub fn open_replace(&self) {
        self.with_editor_mut(CodeEditor::open_replace);
    }

    /// Collapses all foldable blocks.
    pub fn fold_all(&self) {
        self.with_editor_mut(CodeEditor::fold_all);
    }

    /// Expands all folded blocks.
    pub fn unfold_all(&self) {
        self.with_editor_mut(CodeEditor::unfold_all);
    }

    /// Renders the complete interactive Monaco-class [`NodeBlueprint`] using `options`.
    pub fn render_interactive(&self, options: &CodeEditorOptions) -> NodeBlueprint {
        render_interactive_editor(self, options)
    }

    /// Renders the interactive editor wrapped in a Strata [`View`].
    pub fn render_view(&self, options: &CodeEditorOptions) -> View {
        View::new(self.render_interactive(options))
    }
}

/// Reactive Strata hook returning a raw, zero-grammar [`CodeEditorHandle`] across re-renders.
pub fn use_code_editor_raw(initial_text: &str) -> CodeEditorHandle {
    type EditorHookState = (
        Rc<RefCell<CodeEditor>>,
        Rc<std::cell::Cell<bool>>,
        Rc<std::cell::Cell<Option<f32>>>,
    );
    let shared_ref = use_ref::<Option<EditorHookState>>(None);
    let revision = use_signal(0u64);
    let (shared, minimap_dragging, measured_vh) = shared_ref.with_mut(|slot| {
        let entry = slot.get_or_insert_with(|| {
            (
                Rc::new(RefCell::new(CodeEditor::new_raw(initial_text))),
                Rc::new(std::cell::Cell::new(false)),
                Rc::new(std::cell::Cell::new(None)),
            )
        });
        (
            Rc::clone(&entry.0),
            Rc::clone(&entry.1),
            Rc::clone(&entry.2),
        )
    });
    CodeEditorHandle::from_parts(shared, revision, minimap_dragging, measured_vh)
}

/// Reactive Strata hook returning a persistent [`CodeEditorHandle`] across component re-renders.
#[cfg(feature = "builtin-syntax")]
pub fn use_code_editor(initial_text: &str, language: Language) -> CodeEditorHandle {
    type EditorHookState = (
        Rc<RefCell<CodeEditor>>,
        Rc<std::cell::Cell<bool>>,
        Rc<std::cell::Cell<Option<f32>>>,
    );
    let shared_ref = use_ref::<Option<EditorHookState>>(None);
    let revision = use_signal(0u64);
    let (shared, minimap_dragging, measured_vh) = shared_ref.with_mut(|slot| {
        let entry = slot.get_or_insert_with(|| {
            (
                Rc::new(RefCell::new(CodeEditor::new(initial_text, language))),
                Rc::new(std::cell::Cell::new(false)),
                Rc::new(std::cell::Cell::new(None)),
            )
        });
        (
            Rc::clone(&entry.0),
            Rc::clone(&entry.1),
            Rc::clone(&entry.2),
        )
    });
    CodeEditorHandle::from_parts(shared, revision, minimap_dragging, measured_vh)
}

/// Drop-in Monaco-class interactive `<CodeEditorWidget>` Strata component.
///
/// Supports virtualized rendering, pluggable incremental syntax highlighting, sticky scroll,
/// gutter folding, breakpoints, Git gutter bars, inline color swatches, inlay hints,
/// completion & hover popups, Find & Replace (`Ctrl+F` / `Ctrl+H`), Canvas2D minimap,
/// LSP/syntax diagnostics, multi-cursor editing, and `--strata-editor-*` CSS-variable theming.
#[cfg(feature = "builtin-syntax")]
#[allow(clippy::too_many_arguments)]
#[component]
pub fn CodeEditorWidget(
    handle: CodeEditorHandle,
    language: Option<Language>,
    theme: Option<EditorTheme>,
    show_minimap: Option<bool>,
    show_line_numbers: Option<bool>,
    show_fold_gutter: Option<bool>,
    show_status_bar: Option<bool>,
    read_only: Option<bool>,
    font_size: Option<f32>,
    line_height: Option<f32>,
    viewport_height: Option<f32>,
    on_change: Option<Rc<dyn Fn(String)>>,
    class: Option<&'static str>,
) -> View {
    if let Some(lang) = language {
        let needs_switch = handle.inner.borrow().language() != lang;
        if needs_switch {
            handle.inner.borrow_mut().set_language(lang);
        }
    }
    if let Some(ro) = read_only {
        handle.inner.borrow_mut().set_read_only(ro);
    }

    let options = CodeEditorOptions {
        show_line_numbers: show_line_numbers.unwrap_or(true),
        show_fold_gutter: show_fold_gutter.unwrap_or(true),
        show_minimap: show_minimap.unwrap_or(true),
        show_status_bar: show_status_bar.unwrap_or(true),
        show_sticky_scroll: true,
        read_only: read_only.unwrap_or(false),
        font_size: font_size.unwrap_or(13.0),
        line_height: line_height.unwrap_or(22.0),
        viewport_height,
        theme,
        class: class.unwrap_or("").to_string(),
        on_change,
        ..CodeEditorOptions::default()
    };

    View::new(render_interactive_editor(&handle, &options))
}

/// Raw, zero-grammar interactive `<CodeEditorWidget>` when `builtin-syntax` is disabled.
#[cfg(not(feature = "builtin-syntax"))]
#[allow(clippy::too_many_arguments)]
#[component]
pub fn CodeEditorWidget(
    handle: CodeEditorHandle,
    theme: Option<EditorTheme>,
    show_minimap: Option<bool>,
    show_line_numbers: Option<bool>,
    show_fold_gutter: Option<bool>,
    show_status_bar: Option<bool>,
    read_only: Option<bool>,
    font_size: Option<f32>,
    line_height: Option<f32>,
    viewport_height: Option<f32>,
    on_change: Option<Rc<dyn Fn(String)>>,
    class: Option<&'static str>,
) -> View {
    if let Some(ro) = read_only {
        handle.inner.borrow_mut().set_read_only(ro);
    }

    let options = CodeEditorOptions {
        show_line_numbers: show_line_numbers.unwrap_or(true),
        show_fold_gutter: show_fold_gutter.unwrap_or(true),
        show_minimap: show_minimap.unwrap_or(true),
        show_status_bar: show_status_bar.unwrap_or(true),
        show_sticky_scroll: true,
        read_only: read_only.unwrap_or(false),
        font_size: font_size.unwrap_or(13.0),
        line_height: line_height.unwrap_or(22.0),
        viewport_height,
        theme,
        class: class.unwrap_or("").to_string(),
        on_change,
        ..CodeEditorOptions::default()
    };

    View::new(render_interactive_editor(&handle, &options))
}

/// Alias matching `<CodeEditorView ... />` naming convention.
pub use CodeEditorWidget as CodeEditorView;

/// Builds the full interactive Monaco-class DOM blueprint (with live event listeners,
/// virtualized fold-aware rows, Find & Replace overlay, Canvas2D minimap, and status bar).
pub fn render_interactive_editor(
    handle: &CodeEditorHandle,
    options: &CodeEditorOptions,
) -> NodeBlueprint {
    // Subscribe to reactive revision changes.
    let _rev = handle.version();

    const DEFAULT_AUTO_VIEWPORT_HEIGHT: f32 = 480.0;
    let auto_measure = options.viewport_height.is_none();
    let effective_vh = options
        .viewport_height
        .or_else(|| handle.measured_viewport_height.get())
        .unwrap_or(DEFAULT_AUTO_VIEWPORT_HEIGHT)
        .max(80.0);

    // Synchronize layout metrics & fold cache once per render pass.
    {
        let mut ed = handle.inner.borrow_mut();
        ed.set_read_only(options.read_only);
        ed.set_line_height(options.line_height);
        ed.set_viewport_height(effective_vh);
        ed.set_char_width((options.font_size * 0.60).max(5.5));
        let _ = ed.ensure_folds_cached();
    }

    let ed = handle.inner.borrow();
    let theme = options.theme.clone().unwrap_or_else(|| ed.theme().clone());
    let effective_lh = options.line_height.max(14.0);
    let scroll_top = ed.scroll_top();
    let gutter_width = compute_gutter_width(options);

    let visible_lines = ed.visible_logical_lines();
    let total_visible = visible_lines.len().max(1);
    let first_visible = (scroll_top / effective_lh).floor() as usize;
    let visible_count = (effective_vh / effective_lh).ceil() as usize;
    let start_idx = first_visible.saturating_sub(2).min(total_visible);
    let end_idx = (first_visible + visible_count + 6).min(total_visible);
    let slice_offset_y = -(scroll_top - (start_idx as f32 * effective_lh));

    let primary_pos = ed.cursors().primary().focus;
    let matching_bracket = ed.matching_bracket(primary_pos);
    let all_folds = ed.fold_ranges();
    let active_search_idx = ed.search().active_match_index;
    let active_debug_line = ed.decorations().active_debug_line();
    let sticky_scopes = if options.show_sticky_scroll {
        ed.enclosing_scopes_at_scroll()
    } else {
        Vec::new()
    };

    let mut row_nodes = Vec::with_capacity(end_idx.saturating_sub(start_idx));

    for &logical_line in &visible_lines[start_idx..end_idx] {
        let Some(line_str) = ed.buffer().line(logical_line) else {
            continue;
        };
        let is_active_line = logical_line == primary_pos.line;
        let is_debug_line = active_debug_line == Some(logical_line);
        let line_diag_sev = ed.diagnostics().max_severity_on_line(logical_line);
        let line_diags = ed.diagnostics().diagnostics_on_line(logical_line);
        let gutter_markers = ed.decorations().gutter_markers_on_line(logical_line);
        let color_swatches = ed.decorations().color_swatches_on_line(logical_line);
        let inlay_hints = ed.decorations().inlay_hints_on_line(logical_line);
        let is_fold_start = all_folds
            .iter()
            .any(|f| f.start_line == logical_line && f.end_line > f.start_line);
        let hidden_count = ed.folded_hidden_count(logical_line);

        let mut row_children = Vec::with_capacity(4);

        // 1. Breakpoint / Diagnostic dot + Line number + Fold chevron + Git gutter diff bar
        if options.show_line_numbers || options.show_fold_gutter {
            let mut gutter_items = Vec::with_capacity(5);

            // Breakpoint / Debug arrow / Diagnostic indicator (clickable when on_gutter_click is set)
            let bp_marker = gutter_markers.iter().find(|m| {
                matches!(
                    m.kind,
                    GutterMarkerKind::Breakpoint
                        | GutterMarkerKind::BreakpointDisabled
                        | GutterMarkerKind::BreakpointConditional
                        | GutterMarkerKind::DebugCurrentLine
                        | GutterMarkerKind::Custom { .. }
                )
            });

            let mut marker_attrs = Vec::new();
            let marker_child: Vec<NodeBlueprint>;
            let marker_class: String;

            if let Some(bm) = bp_marker {
                if let Some(tip) = &bm.tooltip {
                    marker_attrs.push(Attribute::property("title", tip.clone()));
                }
                match &bm.kind {
                    GutterMarkerKind::Breakpoint => {
                        marker_class =
                            "inline-flex items-center justify-center w-3 h-3 mr-0.5 shrink-0 cursor-pointer"
                                .to_string();
                        marker_attrs.push(Attribute::property("data-breakpoint", "active"));
                        marker_child = vec![NodeBlueprint::element(
                            Tag::Span,
                            vec!["inline-block w-2.5 h-2.5 rounded-full".to_string()],
                            vec![Attribute::property("style", "background-color: #e51400;")],
                            Vec::new(),
                        )];
                    }
                    GutterMarkerKind::BreakpointDisabled => {
                        marker_class =
                            "inline-flex items-center justify-center w-3 h-3 mr-0.5 shrink-0 cursor-pointer"
                                .to_string();
                        marker_attrs.push(Attribute::property("data-breakpoint", "disabled"));
                        marker_child = vec![NodeBlueprint::element(
                            Tag::Span,
                            vec!["inline-block w-2.5 h-2.5 rounded-full border".to_string()],
                            vec![Attribute::property(
                                "style",
                                "border-color: #848484; background-color: transparent;",
                            )],
                            Vec::new(),
                        )];
                    }
                    GutterMarkerKind::BreakpointConditional => {
                        marker_class =
                            "inline-flex items-center justify-center w-3 h-3 mr-0.5 shrink-0 cursor-pointer"
                                .to_string();
                        marker_attrs.push(Attribute::property("data-breakpoint", "conditional"));
                        marker_child = vec![NodeBlueprint::element(
                            Tag::Span,
                            vec!["inline-block w-2.5 h-2.5 rounded-full".to_string()],
                            vec![Attribute::property("style", "background-color: #f97316;")],
                            Vec::new(),
                        )];
                    }
                    GutterMarkerKind::DebugCurrentLine => {
                        marker_class =
                            "inline-flex items-center justify-center w-3 h-3 mr-0.5 text-[10px] shrink-0"
                                .to_string();
                        marker_attrs.push(Attribute::property("data-debug-current", "true"));
                        marker_attrs.push(Attribute::property("style", "color: #eab308;"));
                        marker_child = vec![NodeBlueprint::text("▶")];
                    }
                    GutterMarkerKind::Custom { color, glyph } => {
                        marker_class =
                            "inline-flex items-center justify-center w-3 h-3 mr-0.5 text-[10px] shrink-0"
                                .to_string();
                        marker_attrs.push(Attribute::property("style", format!("color: {color};")));
                        marker_child = vec![NodeBlueprint::text(glyph.clone())];
                    }
                    _ => {
                        marker_class = "inline-block w-3 h-3 mr-0.5 shrink-0".to_string();
                        marker_child = Vec::new();
                    }
                }
            } else if let Some(sev) = line_diag_sev {
                let tooltip = line_diags
                    .first()
                    .map(|d| d.message.clone())
                    .unwrap_or_default();
                marker_class =
                    "inline-flex items-center justify-center w-3 h-3 mr-0.5 shrink-0".to_string();
                marker_attrs.push(Attribute::property("data-diag-severity", sev.label()));
                marker_attrs.push(Attribute::property("title", tooltip));
                marker_child = vec![NodeBlueprint::element(
                    Tag::Span,
                    vec!["inline-block w-2 h-2 rounded-full".to_string()],
                    vec![Attribute::property(
                        "style",
                        format!("background-color: {};", theme.diagnostic_css(sev)),
                    )],
                    Vec::new(),
                )];
            } else {
                marker_class =
                    "inline-flex items-center justify-center w-3 h-3 mr-0.5 shrink-0 cursor-pointer"
                        .to_string();
                marker_child = Vec::new();
            }

            if let Some(on_gutter) = options.on_gutter_click.clone() {
                marker_attrs.push(Attribute::property(
                    "data-gutter-click-line",
                    logical_line.to_string(),
                ));
                marker_attrs.push(Attribute::event_listener(EventType::Click, move |e: Event| {
                    e.stop_propagation();
                    on_gutter(logical_line);
                }));
            }

            gutter_items.push(NodeBlueprint::element(
                Tag::Span,
                vec![marker_class],
                marker_attrs,
                marker_child,
            ));

            // Line number
            if options.show_line_numbers {
                let num_color = if is_active_line {
                    &theme.gutter_active_fg
                } else {
                    &theme.gutter_fg
                };
                let mut ln_attrs = vec![Attribute::property("style", format!("color: {};", num_color))];
                if let Some(on_gutter) = options.on_gutter_click.clone() {
                    ln_attrs.push(Attribute::event_listener(EventType::Click, move |e: Event| {
                        e.stop_propagation();
                        on_gutter(logical_line);
                    }));
                }
                gutter_items.push(NodeBlueprint::element(
                    Tag::Span,
                    vec!["w-9 text-right pr-1.5 select-none font-mono text-xs shrink-0 whitespace-nowrap cursor-pointer".to_string()],
                    ln_attrs,
                    vec![NodeBlueprint::text((logical_line + 1).to_string())],
                ));
            }

            // Clickable fold chevron
            if options.show_fold_gutter {
                if is_fold_start {
                    let is_folded = ed.is_folded(logical_line);
                    let chevron = if is_folded { "▸" } else { "▾" };
                    let h_fold = handle.clone();
                    gutter_items.push(NodeBlueprint::element(
                        Tag::Button,
                        vec![
                            "w-4 h-4 flex items-center justify-center text-[11px] select-none cursor-pointer bg-transparent border-0 p-0 shrink-0"
                                .to_string(),
                        ],
                        vec![
                            Attribute::property("data-fold-toggle", logical_line.to_string()),
                            Attribute::property(
                                "title",
                                if is_folded { "Expand fold" } else { "Collapse fold" },
                            ),
                            Attribute::property(
                                "style",
                                format!("color: {};", theme.gutter_active_fg),
                            ),
                            Attribute::event_listener(EventType::Click, move |e: Event| {
                                e.stop_propagation();
                                h_fold.with_editor_mut(|editor| {
                                    editor.toggle_fold_at(logical_line);
                                });
                            }),
                        ],
                        vec![NodeBlueprint::text(chevron)],
                    ));
                } else {
                    gutter_items.push(NodeBlueprint::element(
                        Tag::Span,
                        vec!["w-4 shrink-0 select-none".to_string()],
                        Vec::new(),
                        Vec::new(),
                    ));
                }
            }

            // Git diff vertical indicator bar on right edge of gutter
            let git_marker = gutter_markers.iter().find(|m| {
                matches!(
                    m.kind,
                    GutterMarkerKind::GitAdded
                        | GutterMarkerKind::GitModified
                        | GutterMarkerKind::GitDeleted
                )
            });
            if let Some(gm) = git_marker {
                let (git_attr, git_color) = match gm.kind {
                    GutterMarkerKind::GitAdded => ("added", "#2ea043"),
                    GutterMarkerKind::GitModified => ("modified", "#007acc"),
                    GutterMarkerKind::GitDeleted => ("deleted", "#f85149"),
                    _ => ("", "transparent"),
                };
                gutter_items.push(NodeBlueprint::element(
                    Tag::Span,
                    vec!["inline-block w-[3px] h-full shrink-0 ml-auto".to_string()],
                    vec![
                        Attribute::property("data-git-gutter", git_attr),
                        Attribute::property("style", format!("background-color: {git_color};")),
                    ],
                    Vec::new(),
                ));
            } else {
                gutter_items.push(NodeBlueprint::element(
                    Tag::Span,
                    vec!["inline-block w-[3px] h-full shrink-0 ml-auto".to_string()],
                    Vec::new(),
                    Vec::new(),
                ));
            }

            row_children.push(NodeBlueprint::element(
                Tag::Div,
                vec!["flex items-center h-full pl-1 pr-0.5 select-none shrink-0 border-r".to_string()],
                vec![Attribute::property(
                    "style",
                    format!(
                        "width: {:.0}px; background-color: {}; border-color: {};",
                        gutter_width, theme.gutter_bg, theme.border
                    ),
                )],
                gutter_items,
            ));
        }

        // 2. Syntax-colored code content with search matches, bracket match, carets,
        //    inline color swatches, inlay hints, diagnostics, and fold badge
        let tokens = ed.tokenize_line(logical_line, line_str);
        let search_matches = if ed.search().is_open {
            ed.search().matches_on_line(logical_line)
        } else {
            Vec::new()
        };
        let line_selections: Vec<(usize, usize)> = ed
            .cursors()
            .all()
            .iter()
            .filter_map(|sel| {
                let (s, e) = sel.ordered_range();
                if sel.is_collapsed() || logical_line < s.line || logical_line > e.line {
                    return None;
                }
                let start_c = if logical_line == s.line { s.col } else { 0 };
                let end_c = if logical_line == e.line {
                    e.col
                } else {
                    line_str.chars().count()
                };
                Some((start_c, end_c))
            })
            .collect();
        let line_carets: Vec<usize> = ed
            .cursors()
            .all()
            .iter()
            .filter(|sel| sel.focus.line == logical_line)
            .map(|sel| sel.focus.col)
            .collect();

        let mut code_spans = build_themed_line_spans(
            line_str,
            &tokens,
            &theme,
            &search_matches,
            active_search_idx,
            &line_selections,
            matching_bracket
                .filter(|p| p.line == logical_line)
                .map(|p| p.col),
            &line_carets,
            line_diag_sev,
            &color_swatches,
            &inlay_hints,
            effective_lh,
            ed.render_whitespace(),
            ed.render_control_chars(),
            options.caret_visible,
            &options.cursor_style,
        );

        // Collapsed fold pill `⋯ N lines`
        if let Some(hidden) = hidden_count {
            let h_unfold = handle.clone();
            code_spans.push(NodeBlueprint::element(
                Tag::Button,
                vec![
                    "ml-2 px-1.5 py-0.5 rounded text-[11px] font-mono leading-none whitespace-nowrap shrink-0 cursor-pointer border-0"
                        .to_string(),
                ],
                vec![
                    Attribute::property("data-fold-badge", logical_line.to_string()),
                    Attribute::property(
                        "style",
                        format!(
                            "background-color: {}; color: {};",
                            theme.fold_badge_bg, theme.fold_badge_fg
                        ),
                    ),
                    Attribute::event_listener(EventType::Click, move |e: Event| {
                        e.stop_propagation();
                        h_unfold.with_editor_mut(|editor| {
                            editor.toggle_fold_at(logical_line);
                        });
                    }),
                ],
                vec![NodeBlueprint::text(format!("⋯ {hidden} lines"))],
            ));
        }

        let mut code_attrs = vec![Attribute::property("data-line-index", logical_line.to_string())];
        if line_diag_sev.is_some() {
            code_attrs.push(Attribute::property("data-has-diagnostic", "true"));
        }

        row_children.push(NodeBlueprint::element(
            Tag::Div,
            vec![
                "flex-1 min-w-0 h-full flex items-center pl-2.5 pr-4 whitespace-pre font-mono relative overflow-hidden cursor-text"
                    .to_string(),
            ],
            code_attrs,
            code_spans,
        ));

        let row_bg = if is_debug_line {
            format!(
                "height: {:.1}px; line-height: {:.1}px; background-color: rgba(234, 179, 8, 0.16);",
                effective_lh, effective_lh
            )
        } else if is_active_line {
            format!(
                "height: {:.1}px; line-height: {:.1}px; background-color: {};",
                effective_lh, effective_lh, theme.line_highlight
            )
        } else {
            format!(
                "height: {:.1}px; line-height: {:.1}px;",
                effective_lh, effective_lh
            )
        };

        row_nodes.push(NodeBlueprint::element(
            Tag::Div,
            vec!["flex items-center w-full shrink-0 overflow-hidden".to_string()],
            vec![
                Attribute::property("data-editor-row", logical_line.to_string()),
                Attribute::property("style", row_bg),
            ],
            row_children,
        ));
    }

    let slice_container = NodeBlueprint::element(
        Tag::Div,
        vec!["flex flex-col w-full relative".to_string()],
        vec![
            Attribute::property("data-editor-slice", "true"),
            Attribute::property("style", format!("top: {:.2}px;", slice_offset_y)),
        ],
        row_nodes,
    );

    let mut viewport_children = vec![slice_container];

    // Optional Sticky Scroll header pinned at top of editor viewport
    if !sticky_scopes.is_empty() {
        let mut scope_rows = Vec::with_capacity(sticky_scopes.len());
        for scope in &sticky_scopes {
            let jump_line = scope.line_idx;
            let h_jump = handle.clone();
            scope_rows.push(NodeBlueprint::element(
                Tag::Div,
                vec![
                    "flex items-center w-full px-2 font-mono cursor-pointer whitespace-pre overflow-hidden shrink-0"
                        .to_string(),
                ],
                vec![
                    Attribute::property("data-sticky-line", jump_line.to_string()),
                    Attribute::property(
                        "style",
                        format!(
                            "height: {:.1}px; line-height: {:.1}px; font-size: {:.1}px; color: {};",
                            effective_lh, effective_lh, options.font_size, theme.fg
                        ),
                    ),
                    Attribute::event_listener(EventType::Click, move |e: Event| {
                        e.stop_propagation();
                        h_jump.with_editor_mut(|editor| {
                            editor.set_scroll_top(jump_line as f32 * effective_lh);
                        });
                    }),
                ],
                vec![
                    NodeBlueprint::element(
                        Tag::Span,
                        vec!["w-12 text-right pr-3 select-none text-xs shrink-0".to_string()],
                        vec![Attribute::property(
                            "style",
                            format!("color: {};", theme.gutter_fg),
                        )],
                        vec![NodeBlueprint::text((jump_line + 1).to_string())],
                    ),
                    NodeBlueprint::text(scope.text.clone()),
                ],
            ));
        }
        viewport_children.push(NodeBlueprint::element(
            Tag::Div,
            vec!["absolute top-0 left-0 right-0 flex flex-col border-b z-20".to_string()],
            vec![
                Attribute::property("data-editor-sticky-scroll", "true"),
                Attribute::property(
                    "style",
                    format!(
                        "background-color: {}; border-color: {};",
                        theme.bg, theme.border
                    ),
                ),
            ],
            scope_rows,
        ));
    }

    // Optional Floating Top-Right VS Code Inline Find & Replace Overlay
    if ed.search().is_open {
        viewport_children.push(build_find_replace_bar(
            handle,
            &ed,
            &theme,
            options.on_change.clone(),
        ));
    }

    // Optional IntelliSense Completion Popup
    if ed.decorations().completion.visible && !ed.decorations().completion.items.is_empty() {
        let comp = &ed.decorations().completion;
        let anchor_row = ed
            .logical_to_display_row(comp.anchor_line)
            .unwrap_or(comp.anchor_line);
        let top_px = ((anchor_row + 1) as f32 * effective_lh - scroll_top)
            .clamp(4.0, (effective_vh - 140.0).max(4.0));
        let left_px = (gutter_width + 10.0 + comp.anchor_col as f32 * ed.char_width())
            .clamp(gutter_width + 8.0, 520.0);

        let mut comp_rows = Vec::new();
        for (idx, item) in comp.items.iter().take(12).enumerate() {
            let is_sel = idx == comp.selected;
            let h_comp = handle.clone();
            let on_accept = options.on_completion_accept.clone();
            let on_chg = options.on_change.clone();
            let row_bg = if is_sel {
                theme.selection_bg.as_str()
            } else {
                "transparent"
            };
            let mut row_kids = vec![
                NodeBlueprint::element(
                    Tag::Span,
                    vec!["inline-flex items-center justify-center w-4 h-4 rounded text-[10px] font-bold mr-1.5 shrink-0".to_string()],
                    vec![Attribute::property(
                        "style",
                        format!("background-color: {}; color: {};", theme.border, theme.caret),
                    )],
                    vec![NodeBlueprint::text(item.kind.short_badge())],
                ),
                NodeBlueprint::element(
                    Tag::Span,
                    vec!["text-xs font-mono truncate".to_string()],
                    vec![Attribute::property("style", format!("color: {};", theme.fg))],
                    vec![NodeBlueprint::text(item.label.clone())],
                ),
            ];
            if let Some(detail) = &item.detail {
                row_kids.push(NodeBlueprint::element(
                    Tag::Span,
                    vec!["text-[11px] font-mono ml-auto pl-3 truncate".to_string()],
                    vec![Attribute::property(
                        "style",
                        format!("color: {};", theme.gutter_fg),
                    )],
                    vec![NodeBlueprint::text(detail.clone())],
                ));
            }
            comp_rows.push(NodeBlueprint::element(
                Tag::Div,
                vec!["flex items-center px-2 py-1 cursor-pointer whitespace-nowrap".to_string()],
                vec![
                    Attribute::property("data-completion-item", item.label.clone()),
                    Attribute::property("data-selected", if is_sel { "true" } else { "false" }),
                    Attribute::property("style", format!("background-color: {row_bg};")),
                    Attribute::event_listener(EventType::Click, move |e: Event| {
                        e.stop_propagation();
                        let mut updated_text: Option<String> = None;
                        let accepted = h_comp.with_editor_mut(|editor| {
                            editor.decorations_mut().completion.selected = idx;
                            let res = editor.accept_completion();
                            if res.is_some() && on_chg.is_some() {
                                updated_text = Some(editor.text());
                            }
                            res
                        });
                        if let (Some(cb), Some(lbl)) = (&on_accept, accepted) {
                            cb(lbl);
                        }
                        if let (Some(cb), Some(txt)) = (&on_chg, updated_text) {
                            cb(txt);
                        }
                    }),
                ],
                row_kids,
            ));
        }

        viewport_children.push(NodeBlueprint::element(
            Tag::Div,
            vec!["absolute flex flex-col rounded border shadow-lg z-30 min-w-[240px] max-w-[420px] overflow-hidden".to_string()],
            vec![
                Attribute::property("data-editor-completion-popup", "true"),
                Attribute::property(
                    "style",
                    format!(
                        "top: {:.1}px; left: {:.1}px; background-color: {}; border-color: {};",
                        top_px, left_px, theme.panel_bg, theme.panel_border
                    ),
                ),
            ],
            comp_rows,
        ));
    }

    // Optional Floating Hover Documentation Card
    if ed.decorations().hover.visible {
        if let Some(info) = &ed.decorations().hover.info {
            let hover = &ed.decorations().hover;
            let anchor_row = ed
                .logical_to_display_row(hover.anchor_line)
                .unwrap_or(hover.anchor_line);
            let top_px = ((anchor_row + 1) as f32 * effective_lh - scroll_top)
                .clamp(4.0, (effective_vh - 96.0).max(4.0));
            let left_px = (gutter_width + 10.0 + hover.anchor_col as f32 * ed.char_width())
                .clamp(gutter_width + 8.0, 480.0);

            let mut hover_kids = Vec::new();
            if !info.title.is_empty() {
                hover_kids.push(NodeBlueprint::element(
                    Tag::Div,
                    vec!["font-semibold pb-1 mb-1 border-b".to_string()],
                    vec![Attribute::property(
                        "style",
                        format!("border-color: {}; color: {};", theme.panel_border, theme.caret),
                    )],
                    vec![NodeBlueprint::text(info.title.clone())],
                ));
            }
            if !info.markdown.is_empty() {
                hover_kids.push(NodeBlueprint::element(
                    Tag::Div,
                    vec!["whitespace-pre-wrap".to_string()],
                    Vec::new(),
                    vec![NodeBlueprint::text(info.markdown.clone())],
                ));
            }

            viewport_children.push(NodeBlueprint::element(
                Tag::Div,
                vec!["absolute rounded border px-2.5 py-1.5 text-xs font-mono shadow-lg z-30 max-w-[460px] flex flex-col".to_string()],
                vec![
                    Attribute::property("data-editor-hover-popup", "true"),
                    Attribute::property(
                        "style",
                        format!(
                            "top: {:.1}px; left: {:.1}px; background-color: {}; color: {}; border-color: {};",
                            top_px, left_px, theme.panel_bg, theme.fg, theme.panel_border
                        ),
                    ),
                ],
                hover_kids,
            ));
        }
    }

    // Main virtualized viewport pane with exact coordinate frame, I-beam cursor, pointer capture,
    // double/triple-click drag, Alt+Click / Ctrl+Click multi-cursor, and right-click context hook.
    let code_left_offset = if options.show_line_numbers || options.show_fold_gutter {
        gutter_width + 10.0
    } else {
        10.0
    };
    let h_down = handle.clone();
    let h_move = handle.clone();
    let h_up = handle.clone();
    let h_wheel = handle.clone();
    let on_ctx = options.on_context_menu.clone();
    let on_focus_down = options.on_focus.clone();
    let flex_fill = auto_measure || options.class.contains("flex-1");
    let viewport_style = if flex_fill {
        format!("font-size: {:.1}px;", options.font_size)
    } else {
        format!(
            "height: {:.1}px; font-size: {:.1}px;",
            effective_vh, options.font_size
        )
    };
    let mut viewport_attrs = vec![
        Attribute::property("data-editor-viewport", "true"),
        Attribute::property("style", viewport_style),
        Attribute::event_listener(EventType::MouseDown, move |e: Event| {
            if let Some(cb) = &on_focus_down {
                cb();
            }
            if e.button == 2 {
                h_down.with_editor_mut(|editor| {
                    editor.handle_pointer_down_ex(
                        e.offset_x as f32,
                        e.offset_y as f32,
                        e.detail,
                        e.alt_key,
                        e.shift_key,
                        e.ctrl_key || e.meta_key,
                        e.button,
                        code_left_offset,
                    );
                });
                if let Some(cb) = &on_ctx {
                    cb(e.client_x as f32, e.client_y as f32);
                }
                return;
            }
            e.set_pointer_capture(e.pointer_id);
            h_down.with_editor_mut(|editor| {
                editor.handle_pointer_down_ex(
                    e.offset_x as f32,
                    e.offset_y as f32,
                    e.detail,
                    e.alt_key,
                    e.shift_key,
                    e.ctrl_key || e.meta_key,
                    e.button,
                    code_left_offset,
                );
            });
        }),
        Attribute::event_listener(EventType::MouseMove, move |e: Event| {
            if e.buttons & 1 != 0 {
                h_move.with_editor_mut(|editor| {
                    editor.handle_pointer_drag(
                        e.offset_x as f32,
                        e.offset_y as f32,
                        code_left_offset,
                    );
                });
            }
        }),
        Attribute::event_listener(EventType::MouseUp, move |e: Event| {
            e.release_pointer_capture(e.pointer_id);
            h_up.with_editor_mut(CodeEditor::handle_pointer_up);
        }),
        Attribute::event_listener(EventType::Wheel, move |e: Event| {
            e.prevent_default();
            h_wheel.with_editor_mut(|editor| {
                editor.handle_wheel(e.delta_y as f32);
            });
        }),
    ];
    if auto_measure {
        let h_resize = handle.clone();
        viewport_attrs.push(Attribute::event_listener(EventType::Resize, move |e: Event| {
            let measured_h = e.height as f32;
            if measured_h.is_finite() && measured_h >= 40.0 {
                let prev = h_resize.measured_viewport_height.get();
                if prev.is_none_or(|p| (p - measured_h).abs() > 0.5) {
                    h_resize.measured_viewport_height.set(Some(measured_h));
                    h_resize.with_editor_mut(|editor| {
                        editor.set_viewport_height(measured_h);
                    });
                }
            }
        }));
    }
    let viewport_node = NodeBlueprint::element(
        Tag::Div,
        vec![
            "flex-1 min-w-0 min-h-0 h-full overflow-hidden select-none relative cursor-text"
                .to_string(),
        ],
        viewport_attrs,
        viewport_children,
    );

    // Optional Canvas2D minimap
    let mut body_children = vec![viewport_node];
    if options.show_minimap {
        let minimap_cfg = options.minimap;
        let commands: Arc<Vec<CanvasCommand>> = Arc::new(render_minimap_canvas_commands(
            &ed,
            scroll_top,
            effective_vh,
            effective_lh,
            effective_vh,
            &minimap_cfg,
            &theme,
        ));
        let h_mm_down = handle.clone();
        let h_mm_move = handle.clone();
        let h_mm_up = handle.clone();
        let mm_drag_down = handle.minimap_dragging.clone();
        let mm_drag_move = handle.minimap_dragging.clone();
        let mm_drag_up = handle.minimap_dragging.clone();

        let minimap_style = if flex_fill {
            format!(
                "width: {:.0}px; height: 100%; background-color: {}; border-color: {};",
                minimap_cfg.width, theme.minimap_bg, theme.border
            )
        } else {
            format!(
                "width: {:.0}px; height: {:.0}px; background-color: {}; border-color: {};",
                minimap_cfg.width, effective_vh, theme.minimap_bg, theme.border
            )
        };
        let minimap_node = NodeBlueprint::element(
            Tag::Canvas,
            vec!["shrink-0 border-l cursor-pointer".to_string()],
            vec![
                Attribute::property("data-editor-minimap", "true"),
                Attribute::property("width", format!("{:.0}", minimap_cfg.width)),
                Attribute::property("height", format!("{:.0}", effective_vh)),
                Attribute::property("style", minimap_style),
                Attribute::scene_payload_arc(commands),
                Attribute::event_listener(EventType::MouseDown, move |e: Event| {
                    e.stop_propagation();
                    mm_drag_down.set(true);
                    h_mm_down.with_editor_mut(|editor| {
                        editor.handle_minimap_pointer(
                            e.offset_y as f32,
                            effective_vh,
                            &minimap_cfg,
                        );
                    });
                }),
                Attribute::event_listener(EventType::MouseMove, move |e: Event| {
                    if mm_drag_move.get() && (e.buttons & 1 != 0) {
                        h_mm_move.with_editor_mut(|editor| {
                            editor.handle_minimap_pointer(
                                e.offset_y as f32,
                                effective_vh,
                                &minimap_cfg,
                            );
                        });
                    }
                }),
                Attribute::event_listener(EventType::MouseUp, move |_e: Event| {
                    mm_drag_up.set(false);
                    let _ = &h_mm_up;
                }),
            ],
            Vec::new(),
        );
        body_children.push(minimap_node);
    }

    // Split body (viewport + minimap)
    let mut root_children = Vec::with_capacity(2);
    root_children.push(NodeBlueprint::element(
        Tag::Div,
        vec!["flex flex-1 min-h-0 w-full overflow-hidden".to_string()],
        Vec::new(),
        body_children,
    ));

    // Optional Monaco-style status bar
    if options.show_status_bar {
        root_children.push(build_status_bar(&ed, &theme));
    }

    // Drop read borrow before wiring root keyboard listener
    let lang_name = ed.language_display_name().to_string();
    drop(ed);

    let h_key = handle.clone();
    let on_change_key = options.on_change.clone();
    let h_input = handle.clone();
    let on_change_input = options.on_change.clone();
    let base_root_classes = if auto_measure {
        "strata-code-editor flex flex-col flex-1 min-h-0 font-mono rounded-lg border overflow-hidden relative outline-none"
    } else {
        "strata-code-editor flex flex-col font-mono rounded-lg border overflow-hidden relative outline-none"
    };
    let root_classes = if options.class.is_empty() {
        base_root_classes.to_string()
    } else {
        format!("{base_root_classes} {}", options.class)
    };

    NodeBlueprint::element(
        Tag::Div,
        vec![root_classes],
        vec![
            Attribute::property("role", "textbox"),
            Attribute::property("aria-multiline", "true"),
            Attribute::property("aria-label", format!("Code editor ({lang_name})")),
            Attribute::property("tabindex", "0"),
            Attribute::property("data-strata-code-editor", "true"),
            Attribute::property("data-editor-theme", theme.name.clone()),
            Attribute::property(
                "style",
                format!(
                    "background-color: {}; color: {}; border-color: {};",
                    theme.bg, theme.fg, theme.border
                ),
            ),
            Attribute::event_listener(EventType::KeyDown, move |e: Event| {
                let mut mutated_text: Option<String> = None;
                let handled = h_key.with_editor_mut(|editor| {
                    let before_edit_rev = editor.edit_revision();
                    let handled = editor.handle_key_event(&e);
                    if handled
                        && on_change_key.is_some()
                        && editor.edit_revision() != before_edit_rev
                    {
                        mutated_text = Some(editor.text());
                    }
                    handled
                });
                if handled {
                    e.prevent_default();
                    e.stop_propagation();
                    if let (Some(cb), Some(text)) = (&on_change_key, mutated_text) {
                        cb(text);
                    }
                }
            }),
            Attribute::event_listener(EventType::Input, move |e: Event| {
                let mut mutated_text: Option<String> = None;
                let handled = h_input.with_editor_mut(|editor| {
                    let before_edit_rev = editor.edit_revision();
                    let handled = editor.handle_input_event(&e);
                    if handled
                        && on_change_input.is_some()
                        && editor.edit_revision() != before_edit_rev
                    {
                        mutated_text = Some(editor.text());
                    }
                    handled
                });
                if handled {
                    e.prevent_default();
                    e.stop_propagation();
                    if let (Some(cb), Some(text)) = (&on_change_input, mutated_text) {
                        cb(text);
                    }
                }
            }),
        ],
        root_children,
    )
}

fn compute_gutter_width(options: &CodeEditorOptions) -> f32 {
    let mut w = 18.0; // padding + breakpoint/diagnostic marker + git diff bar
    if options.show_line_numbers {
        w += 42.0;
    }
    if options.show_fold_gutter {
        w += 18.0;
    }
    w
}

#[inline]
fn char_slice_to_str(s: &str, start_char: usize, end_char: usize) -> &str {
    if start_char >= end_char {
        return "";
    }
    let start_byte = s
        .char_indices()
        .nth(start_char)
        .map(|(b, _)| b)
        .unwrap_or(s.len());
    let end_byte = s
        .char_indices()
        .nth(end_char)
        .map(|(b, _)| b)
        .unwrap_or(s.len());
    if start_byte <= end_byte {
        &s[start_byte..end_byte]
    } else {
        ""
    }
}

fn make_code_span(text: String, style: Option<&str>) -> NodeBlueprint {
    let attrs = match style {
        Some(st) if !st.is_empty() => vec![Attribute::property("style", st.to_string())],
        _ => Vec::new(),
    };
    NodeBlueprint::element(
        Tag::Span,
        vec!["whitespace-pre shrink-0 pointer-events-none".to_string()],
        attrs,
        vec![NodeBlueprint::text(text)],
    )
}

fn make_caret_anchor(
    col: usize,
    caret_color: &str,
    line_height: f32,
    caret_visible: bool,
    cursor_style: &str,
) -> NodeBlueprint {
    let caret_h = (line_height - 4.0).max(12.0);
    let opacity = if caret_visible { "1" } else { "0" };
    let style = match cursor_style {
        "block" => format!(
            "width: 8px; height: {:.1}px; margin-right: -8px; background-color: {}; opacity: {};",
            caret_h,
            caret_color,
            if caret_visible { "0.65" } else { "0" }
        ),
        "underline" => format!(
            "width: 8px; height: 2px; margin-top: {:.1}px; margin-right: -8px; background-color: {}; opacity: {};",
            (caret_h - 2.0).max(8.0),
            caret_color,
            opacity
        ),
        _ => format!(
            "width: 2px; height: {:.1}px; margin-right: -2px; background-color: {}; opacity: {};",
            caret_h, caret_color, opacity
        ),
    };
    NodeBlueprint::element(
        Tag::Div,
        vec!["inline-block shrink-0 pointer-events-none z-10".to_string()],
        vec![
            Attribute::property("data-editor-caret-anchor", col.to_string()),
            Attribute::property("data-editor-caret-col", col.to_string()),
            Attribute::property(
                "data-editor-caret-visible",
                if caret_visible { "true" } else { "false" },
            ),
            Attribute::property("style", style),
        ],
        Vec::new(),
    )
}

fn make_color_swatch(color: &DetectedColor) -> NodeBlueprint {
    NodeBlueprint::element(
        Tag::Span,
        vec![
            "inline-block w-[10px] h-[10px] mr-[4px] ml-[1px] rounded-[2px] border align-middle shrink-0 pointer-events-none"
                .to_string(),
        ],
        vec![
            Attribute::property("data-color-swatch", color.css_color.clone()),
            Attribute::property("title", color.css_color.clone()),
            Attribute::property(
                "style",
                format!(
                    "background-color: {}; border-color: rgba(255,255,255,0.45);",
                    color.css_color
                ),
            ),
        ],
        Vec::new(),
    )
}

fn make_inlay_hint(hint: &InlayHint, theme: &EditorTheme) -> NodeBlueprint {
    NodeBlueprint::element(
        Tag::Span,
        vec!["inline-block px-1 mx-0.5 rounded text-[11px] font-mono select-none shrink-0 pointer-events-none".to_string()],
        vec![
            Attribute::property("data-inlay-hint", hint.label.clone()),
            Attribute::property(
                "style",
                format!(
                    "background-color: {}; color: {}; opacity: 0.85;",
                    theme.panel_bg, theme.gutter_fg
                ),
            ),
        ],
        vec![NodeBlueprint::text(hint.label.clone())],
    )
}

#[allow(clippy::too_many_arguments)]
fn transform_display_chars(
    piece: &str,
    piece_char_start: usize,
    line_chars: &[char],
    leading_ws_end: usize,
    trailing_ws_start: usize,
    selections: &[(usize, usize)],
    render_whitespace: RenderWhitespace,
    render_control_chars: bool,
) -> String {
    let needs_ws = render_whitespace != RenderWhitespace::None
        && piece.chars().any(|c| c == ' ' || c == '\t');
    let needs_ctrl = render_control_chars
        && piece
            .chars()
            .any(|c| c.is_control() && c != '\t' && c != '\n' && c != '\r');
    if !needs_ws && !needs_ctrl {
        return piece.to_string();
    }

    let mut out = String::with_capacity(piece.len() + 8);
    for (offset, ch) in piece.chars().enumerate() {
        let col = piece_char_start + offset;
        if ch == ' ' || ch == '\t' {
            let show_ws = match render_whitespace {
                RenderWhitespace::None => false,
                RenderWhitespace::All => true,
                RenderWhitespace::Trailing => col >= trailing_ws_start,
                RenderWhitespace::Selection => {
                    selections.iter().any(|&(s, e)| col >= s && col < e)
                }
                RenderWhitespace::Boundary => {
                    col < leading_ws_end
                        || col >= trailing_ws_start
                        || (col > 0 && line_chars.get(col - 1) == Some(&' '))
                        || line_chars.get(col + 1) == Some(&' ')
                }
            };
            if show_ws {
                out.push(if ch == '\t' { '→' } else { '·' });
            } else {
                out.push(ch);
            }
        } else if render_control_chars && ch.is_control() && ch != '\n' && ch != '\r' {
            let code = ch as u32;
            if code < 32 {
                out.push('^');
                out.push((b'@' + code as u8) as char);
            } else {
                out.push('\u{FFFD}');
            }
        } else {
            out.push(ch);
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn build_themed_line_spans(
    line_text: &str,
    tokens: &[SyntaxToken],
    theme: &EditorTheme,
    search_matches: &[(usize, &crate::search::SearchMatch)],
    active_search_idx: usize,
    selections: &[(usize, usize)],
    matching_bracket_col: Option<usize>,
    carets: &[usize],
    line_diag_sev: Option<DiagnosticSeverity>,
    color_swatches: &[&DetectedColor],
    inlay_hints: &[&InlayHint],
    line_height: f32,
    render_whitespace: RenderWhitespace,
    render_control_chars: bool,
    caret_visible: bool,
    cursor_style: &str,
) -> Vec<NodeBlueprint> {
    let line_char_vec: Vec<char> = line_text.chars().collect();
    let line_chars = line_char_vec.len();
    let leading_ws_end = line_char_vec
        .iter()
        .take_while(|c| **c == ' ' || **c == '\t')
        .count();
    let trailing_ws_start = line_char_vec
        .iter()
        .rposition(|c| *c != ' ' && *c != '\t')
        .map(|i| i + 1)
        .unwrap_or(0);

    let decorate_style = |base: String, char_start: usize, char_end: usize| -> String {
        let mut style = base;
        if let Some((idx, _)) = search_matches
            .iter()
            .find(|(_, m)| char_start < m.end.col && char_end > m.start.col)
        {
            let bg = if *idx == active_search_idx {
                &theme.find_active_bg
            } else {
                &theme.find_match_bg
            };
            style.push_str(&format!(" background-color: {}; border-radius: 2px;", bg));
        } else if selections
            .iter()
            .any(|&(s, e)| char_start < e && char_end > s)
        {
            style.push_str(&format!(" background-color: {};", theme.selection_bg));
        } else if let Some(b_col) = matching_bracket_col {
            if b_col >= char_start && b_col < char_end {
                style.push_str(&format!(
                    " background-color: {}; border-radius: 2px;",
                    theme.bracket_match_bg
                ));
            }
        }
        if let Some(sev) = line_diag_sev {
            style.push_str(&format!(
                " border-bottom: 2px dotted {};",
                theme.diagnostic_css(sev)
            ));
        }
        style
    };

    // 1. Build contiguous base token runs covering `line_text` (un-decorated).
    let mut base_runs: Vec<(usize, usize, String, String)> = Vec::new();
    if tokens.is_empty() {
        if !line_text.is_empty() {
            base_runs.push((0, line_chars, line_text.to_string(), String::new()));
        }
    } else {
        let mut last_byte = 0usize;
        let mut cur_char = 0usize;
        for tok in tokens {
            let start_b = tok.start_col.min(line_text.len());
            let end_b = tok.end_col.min(line_text.len());
            if start_b > last_byte
                && line_text.is_char_boundary(last_byte)
                && line_text.is_char_boundary(start_b)
            {
                let gap = &line_text[last_byte..start_b];
                let gap_len = gap.chars().count();
                base_runs.push((cur_char, cur_char + gap_len, gap.to_string(), String::new()));
                cur_char += gap_len;
            }

            let tok_char_start = if line_text.is_char_boundary(start_b) {
                line_text[..start_b].chars().count()
            } else {
                cur_char
            };
            let tok_char_len = tok.text.chars().count();
            let tok_char_end = tok_char_start + tok_char_len;

            let mut base_style = format!("color: {};", theme.token_css(tok.token_type));
            if matches!(
                tok.token_type,
                TokenType::Keyword | TokenType::ControlFlow | TokenType::Heading
            ) {
                base_style.push_str(" font-weight: 600;");
            } else if matches!(
                tok.token_type,
                TokenType::Comment | TokenType::DocComment
            ) {
                base_style.push_str(" font-style: italic;");
            }
            base_runs.push((tok_char_start, tok_char_end, tok.text.clone(), base_style));
            last_byte = end_b.max(last_byte);
            cur_char = tok_char_end;
        }
        if last_byte < line_text.len() && line_text.is_char_boundary(last_byte) {
            let tail = &line_text[last_byte..];
            let tail_len = tail.chars().count();
            base_runs.push((cur_char, cur_char + tail_len, tail.to_string(), String::new()));
        }
    }

    // 1b. Split base runs at exact selection, search match, and matching-bracket column
    //     boundaries so partial-word and raw-plaintext selections highlight exact characters.
    let mut split_cols: Vec<usize> = Vec::new();
    for &(s, e) in selections {
        split_cols.push(s.min(line_chars));
        split_cols.push(e.min(line_chars));
    }
    for &(_, m) in search_matches {
        split_cols.push(m.start.col.min(line_chars));
        split_cols.push(m.end.col.min(line_chars));
    }
    if let Some(b_col) = matching_bracket_col {
        split_cols.push(b_col.min(line_chars));
        split_cols.push((b_col + 1).min(line_chars));
    }
    split_cols.sort_unstable();
    split_cols.dedup();

    let mut runs: Vec<(usize, usize, String, String)> =
        Vec::with_capacity(base_runs.len() + split_cols.len());
    for (r_start, r_end, text, base_style) in base_runs {
        if split_cols.is_empty() {
            let st = decorate_style(base_style, r_start, r_end);
            runs.push((r_start, r_end, text, st));
            continue;
        }
        let mut cursor = r_start;
        for &cut in &split_cols {
            if cut > cursor && cut < r_end {
                let piece = char_slice_to_str(&text, cursor - r_start, cut - r_start);
                if !piece.is_empty() {
                    let st = decorate_style(base_style.clone(), cursor, cut);
                    runs.push((cursor, cut, piece.to_string(), st));
                }
                cursor = cut;
            }
        }
        if cursor == r_start {
            let st = decorate_style(base_style, r_start, r_end);
            runs.push((r_start, r_end, text, st));
        } else if cursor < r_end {
            let tail = char_slice_to_str(&text, cursor - r_start, r_end - r_start);
            if !tail.is_empty() {
                let st = decorate_style(base_style, cursor, r_end);
                runs.push((cursor, r_end, tail.to_string(), st));
            }
        }
    }

    // 2. Collect inline injection points (carets, color swatches, inlay hints) sorted by char column.
    #[derive(Clone, Copy)]
    enum InlinePoint<'a> {
        Swatch(&'a DetectedColor),
        Inlay(&'a InlayHint),
        Caret(usize),
    }

    let mut points: Vec<(usize, u8, InlinePoint<'_>)> = Vec::with_capacity(
        carets.len() + color_swatches.len() + inlay_hints.len(),
    );
    for sw in color_swatches {
        points.push((sw.start_col.min(line_chars), 0, InlinePoint::Swatch(sw)));
    }
    for hint in inlay_hints {
        points.push((hint.col.min(line_chars), 1, InlinePoint::Inlay(hint)));
    }
    let mut clamped_carets: Vec<usize> = carets.iter().map(|&c| c.min(line_chars)).collect();
    clamped_carets.sort_unstable();
    clamped_carets.dedup();
    for c in clamped_carets {
        points.push((c, 2, InlinePoint::Caret(c)));
    }
    points.sort_by_key(|&(col, order, _)| (col, order));

    let emit_point = |nodes: &mut Vec<NodeBlueprint>, pt: InlinePoint<'_>| match pt {
        InlinePoint::Swatch(sw) => nodes.push(make_color_swatch(sw)),
        InlinePoint::Inlay(hint) => nodes.push(make_inlay_hint(hint, theme)),
        InlinePoint::Caret(col) => nodes.push(make_caret_anchor(
            col,
            &theme.caret,
            line_height,
            caret_visible,
            cursor_style,
        )),
    };

    let mut nodes = Vec::with_capacity(runs.len() + points.len() * 2);
    let mut pt_idx = 0usize;

    for (r_start, r_end, text, style) in runs {
        while pt_idx < points.len() && points[pt_idx].0 <= r_start {
            emit_point(&mut nodes, points[pt_idx].2);
            pt_idx += 1;
        }

        let mut local_start = 0usize;
        let run_len = r_end.saturating_sub(r_start);
        while pt_idx < points.len() && points[pt_idx].0 < r_end {
            let col = points[pt_idx].0;
            let split_at = col.saturating_sub(r_start).min(run_len);
            if split_at > local_start {
                let piece = char_slice_to_str(&text, local_start, split_at);
                if !piece.is_empty() {
                    let rendered = transform_display_chars(
                        piece,
                        r_start + local_start,
                        &line_char_vec,
                        leading_ws_end,
                        trailing_ws_start,
                        selections,
                        render_whitespace,
                        render_control_chars,
                    );
                    nodes.push(make_code_span(rendered, Some(style.as_str())));
                }
            }
            emit_point(&mut nodes, points[pt_idx].2);
            local_start = split_at;
            pt_idx += 1;
        }

        if local_start == 0 {
            if !text.is_empty() {
                let rendered = transform_display_chars(
                    &text,
                    r_start,
                    &line_char_vec,
                    leading_ws_end,
                    trailing_ws_start,
                    selections,
                    render_whitespace,
                    render_control_chars,
                );
                nodes.push(make_code_span(rendered, Some(style.as_str())));
            }
        } else if local_start < run_len {
            let tail = char_slice_to_str(&text, local_start, run_len);
            if !tail.is_empty() {
                let rendered = transform_display_chars(
                    tail,
                    r_start + local_start,
                    &line_char_vec,
                    leading_ws_end,
                    trailing_ws_start,
                    selections,
                    render_whitespace,
                    render_control_chars,
                );
                nodes.push(make_code_span(rendered, Some(style.as_str())));
            }
        }
    }

    while pt_idx < points.len() {
        emit_point(&mut nodes, points[pt_idx].2);
        pt_idx += 1;
    }

    nodes
}

fn build_find_replace_bar(
    handle: &CodeEditorHandle,
    ed: &CodeEditor,
    theme: &EditorTheme,
    on_change: Option<Rc<dyn Fn(String)>>,
) -> NodeBlueprint {
    let search = ed.search();
    let status = if search.query.is_empty() {
        "No results".to_string()
    } else {
        search.status_label()
    };

    let h_toggle_replace = handle.clone();
    let h_query = handle.clone();
    let h_case = handle.clone();
    let h_word = handle.clone();
    let h_regex = handle.clone();
    let h_prev = handle.clone();
    let h_next = handle.clone();
    let h_close = handle.clone();
    let h_find_key = handle.clone();

    let toggle_btn = |label: &str, title: &str, active: bool, attr: &str, on_click: Box<dyn Fn(Event)>| {
        let style = if active {
            format!(
                "background-color: {}; color: {}; border-color: {};",
                theme.selection_bg, theme.fg, theme.caret
            )
        } else {
            format!(
                "background-color: transparent; color: {}; border-color: transparent;",
                theme.gutter_fg
            )
        };
        NodeBlueprint::element(
            Tag::Button,
            vec!["px-1 py-0.5 text-[11px] font-mono rounded-[3px] border cursor-pointer whitespace-nowrap shrink-0 hover:bg-white/10".to_string()],
            vec![
                Attribute::property("data-find-action", attr),
                Attribute::property("title", title),
                Attribute::property("style", style),
                Attribute::event_listener(EventType::MouseDown, |e: Event| {
                    e.stop_propagation();
                }),
                Attribute::event_listener(EventType::Click, move |e: Event| {
                    e.stop_propagation();
                    on_click(e);
                }),
            ],
            vec![NodeBlueprint::text(label)],
        )
    };

    let find_input_box = NodeBlueprint::element(
        Tag::Div,
        vec!["flex items-center flex-1 min-w-[160px] rounded-[2px] border px-1".to_string()],
        vec![Attribute::property(
            "style",
            format!(
                "background-color: {}; border-color: {};",
                theme.bg, theme.panel_border
            ),
        )],
        vec![
            NodeBlueprint::element(
                Tag::Input,
                vec!["flex-1 min-w-[84px] px-1 py-0.5 text-xs font-mono bg-transparent border-0 outline-none".to_string()],
                vec![
                    Attribute::property("type", "text"),
                    Attribute::property("placeholder", "Find"),
                    Attribute::property("value", search.query.clone()),
                    Attribute::property("data-find-input", "query"),
                    Attribute::property("style", format!("color: {};", theme.fg)),
                    Attribute::event_listener(EventType::MouseDown, |e: Event| {
                        e.stop_propagation();
                    }),
                    Attribute::value_listener(EventType::Input, move |val: String| {
                        h_query.with_editor_mut(|editor| editor.set_find_query(val));
                    }),
                    Attribute::event_listener(EventType::KeyDown, move |e: Event| {
                        match e.key.as_str() {
                            "Escape" => {
                                e.prevent_default();
                                e.stop_propagation();
                                h_find_key.with_editor_mut(CodeEditor::close_find);
                            }
                            "Enter" => {
                                e.prevent_default();
                                e.stop_propagation();
                                let shift = e.shift_key;
                                h_find_key.with_editor_mut(|editor| {
                                    if shift {
                                        let _ = editor.find_prev();
                                    } else {
                                        let _ = editor.find_next();
                                    }
                                });
                            }
                            _ => {
                                e.stop_propagation();
                            }
                        }
                    }),
                ],
                Vec::new(),
            ),
            toggle_btn(
                "Aa",
                "Match Case (Alt+C)",
                search.case_sensitive,
                "case",
                Box::new(move |_| {
                    h_case.with_editor_mut(|editor| {
                        let mut s = std::mem::take(editor.search_mut());
                        s.toggle_case_sensitive(editor.buffer());
                        *editor.search_mut() = s;
                    });
                }),
            ),
            toggle_btn(
                "ab",
                "Match Whole Word (Alt+W)",
                search.whole_word,
                "whole-word",
                Box::new(move |_| {
                    h_word.with_editor_mut(|editor| {
                        let mut s = std::mem::take(editor.search_mut());
                        s.toggle_whole_word(editor.buffer());
                        *editor.search_mut() = s;
                    });
                }),
            ),
            toggle_btn(
                ".*",
                "Use Regular Expression (Alt+R)",
                search.use_regex,
                "regex",
                Box::new(move |_| {
                    h_regex.with_editor_mut(|editor| {
                        let mut s = std::mem::take(editor.search_mut());
                        s.toggle_regex(editor.buffer());
                        *editor.search_mut() = s;
                    });
                }),
            ),
        ],
    );

    let find_row = NodeBlueprint::element(
        Tag::Div,
        vec!["flex items-center gap-1 w-full".to_string()],
        Vec::new(),
        vec![
            find_input_box,
            NodeBlueprint::element(
                Tag::Span,
                vec!["text-[11px] font-mono px-1 min-w-[60px] text-center select-none whitespace-nowrap shrink-0".to_string()],
                vec![
                    Attribute::property("data-find-status", "true"),
                    Attribute::property("style", format!("color: {};", theme.gutter_active_fg)),
                ],
                vec![NodeBlueprint::text(status)],
            ),
            toggle_btn(
                "↑",
                "Previous Match (Shift+Enter)",
                false,
                "prev",
                Box::new(move |_| {
                    h_prev.with_editor_mut(|editor| {
                        let _ = editor.find_prev();
                    });
                }),
            ),
            toggle_btn(
                "↓",
                "Next Match (Enter)",
                false,
                "next",
                Box::new(move |_| {
                    h_next.with_editor_mut(|editor| {
                        let _ = editor.find_next();
                    });
                }),
            ),
            toggle_btn(
                "×",
                "Close (Escape)",
                false,
                "close",
                Box::new(move |_| {
                    h_close.with_editor_mut(CodeEditor::close_find);
                }),
            ),
        ],
    );

    let mut rows = vec![find_row];

    if search.show_replace {
        let h_repl_in = handle.clone();
        let h_repl_key = handle.clone();
        let h_preserve = handle.clone();
        let h_repl_one = handle.clone();
        let h_repl_all = handle.clone();
        let cb_repl_enter = on_change.clone();
        let cb_one = on_change.clone();
        let cb_all = on_change;

        let replace_input_box = NodeBlueprint::element(
            Tag::Div,
            vec!["flex items-center flex-1 min-w-[160px] rounded-[2px] border px-1".to_string()],
            vec![Attribute::property(
                "style",
                format!(
                    "background-color: {}; border-color: {};",
                    theme.bg, theme.panel_border
                ),
            )],
            vec![
                NodeBlueprint::element(
                    Tag::Input,
                    vec!["flex-1 min-w-[84px] px-1 py-0.5 text-xs font-mono bg-transparent border-0 outline-none".to_string()],
                    vec![
                        Attribute::property("type", "text"),
                        Attribute::property("placeholder", "Replace"),
                        Attribute::property("value", search.replacement.clone()),
                        Attribute::property("data-find-input", "replace"),
                        Attribute::property("style", format!("color: {};", theme.fg)),
                        Attribute::event_listener(EventType::MouseDown, |e: Event| {
                            e.stop_propagation();
                        }),
                        Attribute::value_listener(EventType::Input, move |val: String| {
                            h_repl_in.with_editor_mut(|editor| editor.set_replace_text(val));
                        }),
                        Attribute::event_listener(EventType::KeyDown, move |e: Event| {
                            match e.key.as_str() {
                                "Escape" => {
                                    e.prevent_default();
                                    e.stop_propagation();
                                    h_repl_key.with_editor_mut(CodeEditor::close_find);
                                }
                                "Enter" => {
                                    e.prevent_default();
                                    e.stop_propagation();
                                    let updated = h_repl_key.with_editor_mut(|editor| {
                                        if editor.replace_current() {
                                            Some(editor.text())
                                        } else {
                                            None
                                        }
                                    });
                                    if let (Some(cb), Some(text)) = (&cb_repl_enter, updated) {
                                        cb(text);
                                    }
                                }
                                _ => {
                                    e.stop_propagation();
                                }
                            }
                        }),
                    ],
                    Vec::new(),
                ),
                toggle_btn(
                    "AB",
                    "Preserve Case (Alt+P)",
                    search.preserve_case,
                    "preserve-case",
                    Box::new(move |_| {
                        h_preserve.with_editor_mut(|editor| {
                            editor.search_mut().toggle_preserve_case();
                        });
                    }),
                ),
            ],
        );

        rows.push(NodeBlueprint::element(
            Tag::Div,
            vec!["flex items-center gap-1 mt-1 w-full".to_string()],
            Vec::new(),
            vec![
                replace_input_box,
                toggle_btn(
                    "Replace",
                    "Replace (Enter)",
                    false,
                    "replace",
                    Box::new(move |_| {
                        let updated = h_repl_one.with_editor_mut(|editor| {
                            if editor.replace_current() {
                                Some(editor.text())
                            } else {
                                None
                            }
                        });
                        if let (Some(cb), Some(text)) = (&cb_one, updated) {
                            cb(text);
                        }
                    }),
                ),
                toggle_btn(
                    "All",
                    "Replace All (Ctrl+Alt+Enter)",
                    false,
                    "replace-all",
                    Box::new(move |_| {
                        let updated = h_repl_all.with_editor_mut(|editor| {
                            if editor.replace_all() > 0 {
                                Some(editor.text())
                            } else {
                                None
                            }
                        });
                        if let (Some(cb), Some(text)) = (&cb_all, updated) {
                            cb(text);
                        }
                    }),
                ),
            ],
        ));
    }

    let replace_chevron = NodeBlueprint::element(
        Tag::Button,
        vec!["w-4 self-stretch flex items-center justify-center text-[11px] font-mono bg-transparent border-0 rounded-[3px] cursor-pointer shrink-0 hover:bg-white/10".to_string()],
        vec![
            Attribute::property("data-find-action", "toggle-replace"),
            Attribute::property("title", "Toggle Replace"),
            Attribute::property("style", format!("color: {};", theme.gutter_active_fg)),
            Attribute::event_listener(EventType::MouseDown, |e: Event| {
                e.stop_propagation();
            }),
            Attribute::event_listener(EventType::Click, move |e: Event| {
                e.stop_propagation();
                h_toggle_replace.with_editor_mut(|editor| {
                    editor.search_mut().show_replace = !editor.search().show_replace;
                });
            }),
        ],
        vec![NodeBlueprint::text(if search.show_replace { "▾" } else { "▸" })],
    );

    NodeBlueprint::element(
        Tag::Div,
        vec!["absolute top-1 right-4 z-30 w-[412px] max-w-[94%] px-1.5 py-1.5 rounded-[4px] border shadow-xl flex items-start gap-1 cursor-default".to_string()],
        vec![
            Attribute::property("data-find-replace-bar", "true"),
            Attribute::property(
                "style",
                format!(
                    "background-color: {}; border-color: {};",
                    theme.panel_bg, theme.panel_border
                ),
            ),
            Attribute::event_listener(EventType::MouseDown, |e: Event| {
                e.stop_propagation();
            }),
        ],
        vec![
            replace_chevron,
            NodeBlueprint::element(
                Tag::Div,
                vec!["flex-1 min-w-0 flex flex-col".to_string()],
                Vec::new(),
                rows,
            ),
        ],
    )
}

fn build_status_bar(ed: &CodeEditor, theme: &EditorTheme) -> NodeBlueprint {
    let primary = ed.cursors().primary().focus;
    let counts = ed.diagnostic_counts();
    let sel_chars = ed.selected_char_count();
    let cursor_badge = if ed.cursors().count() > 1 {
        if sel_chars > 0 {
            format!(
                "Ln {}, Col {} ({} cursors, {} selected)",
                primary.line + 1,
                primary.col + 1,
                ed.cursors().count(),
                sel_chars
            )
        } else {
            format!(
                "Ln {}, Col {} ({} cursors)",
                primary.line + 1,
                primary.col + 1,
                ed.cursors().count()
            )
        }
    } else if sel_chars > 0 {
        format!(
            "Ln {}, Col {} ({} selected)",
            primary.line + 1,
            primary.col + 1,
            sel_chars
        )
    } else {
        format!("Ln {}, Col {}", primary.line + 1, primary.col + 1)
    };
    let indent_label = if ed.insert_spaces() {
        format!("Spaces: {}", ed.tab_size())
    } else {
        format!("Tab Size: {}", ed.tab_size())
    };

    NodeBlueprint::element(
        Tag::Div,
        vec![
            "flex items-center justify-between gap-3 px-3 py-1 text-[11px] font-mono border-t select-none shrink-0 whitespace-nowrap overflow-hidden"
                .to_string(),
        ],
        vec![
            Attribute::property("data-editor-statusbar", "true"),
            Attribute::property(
                "style",
                format!(
                    "background-color: {}; color: {}; border-color: {};",
                    theme.gutter_bg, theme.gutter_fg, theme.border
                ),
            ),
        ],
        vec![
            NodeBlueprint::element(
                Tag::Div,
                vec!["flex items-center gap-3 whitespace-nowrap overflow-hidden shrink-0".to_string()],
                Vec::new(),
                vec![
                    NodeBlueprint::element(
                        Tag::Span,
                        vec!["whitespace-nowrap shrink-0".to_string()],
                        vec![Attribute::property(
                            "style",
                            format!("color: {};", theme.diag_error),
                        )],
                        vec![NodeBlueprint::text(format!("✕ {}", counts.errors))],
                    ),
                    NodeBlueprint::element(
                        Tag::Span,
                        vec!["whitespace-nowrap shrink-0".to_string()],
                        vec![Attribute::property(
                            "style",
                            format!("color: {};", theme.diag_warning),
                        )],
                        vec![NodeBlueprint::text(format!("▲ {}", counts.warnings))],
                    ),
                    NodeBlueprint::element(
                        Tag::Span,
                        vec!["whitespace-nowrap shrink-0".to_string()],
                        vec![Attribute::property("data-status-cursor", "true")],
                        vec![NodeBlueprint::text(cursor_badge)],
                    ),
                ],
            ),
            NodeBlueprint::element(
                Tag::Div,
                vec!["flex items-center gap-3 whitespace-nowrap overflow-hidden shrink-0".to_string()],
                Vec::new(),
                vec![
                    NodeBlueprint::element(
                        Tag::Span,
                        vec!["whitespace-nowrap shrink-0".to_string()],
                        vec![Attribute::property("data-status-spaces", "true")],
                        vec![NodeBlueprint::text(indent_label)],
                    ),
                    NodeBlueprint::element(
                        Tag::Span,
                        vec!["whitespace-nowrap shrink-0".to_string()],
                        Vec::new(),
                        vec![NodeBlueprint::text("UTF-8")],
                    ),
                    NodeBlueprint::element(
                        Tag::Span,
                        vec!["font-semibold whitespace-nowrap shrink-0".to_string()],
                        vec![Attribute::property(
                            "style",
                            format!("color: {};", theme.gutter_active_fg),
                        )],
                        vec![NodeBlueprint::text(ed.language_display_name())],
                    ),
                ],
            ),
        ],
    )
}

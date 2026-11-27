//! High-performance Code Editor controller with virtualized lines, syntax highlighting, and gutter.

use crate::buffer::LineBuffer;
use crate::cursor::{MultiCursor, Position, Selection};
use crate::decorations::DecorationStore;
use crate::diagnostics::{DiagnosticCollection, DiagnosticCounts, EditorDiagnostic};
use crate::history::HistoryStack;
use crate::ime::ImeComposition;
use crate::language::{
    FoldRange, LanguageConfig, LanguageId, PlainTextSyntaxProvider, StickyScope, SyntaxProvider,
    SyntaxToken, TextEditDelta, TokenType,
};
use crate::minimap::{minimap_y_to_scroll_top, MinimapConfig};
use crate::search::{FindReplaceState, SearchMatch};
#[cfg(feature = "builtin-syntax")]
use crate::syntax::{Language, SyntaxHighlighter};
use crate::theme::EditorTheme;
use strata_dom::{Event, NodeBlueprint, Tag};

const MAX_PENDING_DELTAS: usize = 2048;

/// Modifier key used to place additional multi-cursors with mouse clicks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MultiCursorModifier {
    #[default]
    Alt,
    CtrlCmd,
}

/// Whitespace rendering mode matching VS Code's `editor.renderWhitespace`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderWhitespace {
    None,
    Boundary,
    #[default]
    Selection,
    Trailing,
    All,
}

/// Active pointer drag gesture mode tracking character, word-by-word, line-by-line,
/// and rectangular column/box selection drags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerDragMode {
    Char,
    Word {
        origin_start: Position,
        origin_end: Position,
    },
    Line {
        origin_line: usize,
    },
    Column {
        origin: Position,
    },
}

/// High-level code editor managing buffer state, multi-cursors, pluggable incremental syntax,
/// decorations, search/replace, folding, diagnostics, theming, and virtualized rendering.
///
/// The raw microkernel knows nothing about specific languages: callers either use
/// [`CodeEditor::new_raw`] for a zero-grammar plain-text buffer, [`CodeEditor::with_provider`]
/// to attach any language plugin's [`SyntaxProvider`], or (when the default `builtin-syntax`
/// feature is enabled) [`CodeEditor::new`] with built-in grammars.
pub struct CodeEditor {
    buffer: LineBuffer,
    cursors: MultiCursor,
    history: HistoryStack,
    syntax_provider: Box<dyn SyntaxProvider>,
    language_config: LanguageConfig,
    decorations: DecorationStore,
    pending_edit_deltas: Vec<TextEditDelta>,
    folded_lines: Vec<usize>,
    cached_folds: Vec<FoldRange>,
    folds_valid: bool,
    ime: ImeComposition,
    tab_size: usize,
    insert_spaces: bool,
    auto_closing_pairs_enabled: bool,
    wrap_column: Option<usize>,
    search: FindReplaceState,
    diagnostics: DiagnosticCollection,
    theme: EditorTheme,
    scroll_top: f32,
    scroll_left: f32,
    viewport_height: f32,
    line_height: f32,
    char_width: f32,
    read_only: bool,
    is_pointer_dragging: bool,
    drag_mode: PointerDragMode,
    multi_cursor_modifier: MultiCursorModifier,
    column_selection_mode: bool,
    render_whitespace: RenderWhitespace,
    render_control_chars: bool,
    last_pointer_click: Option<(std::time::Instant, f32, f32, i32)>,
    selection_expansion_stack: Vec<Vec<Selection>>,
    edit_revision: u64,
}

impl Clone for CodeEditor {
    fn clone(&self) -> Self {
        Self {
            buffer: self.buffer.clone(),
            cursors: self.cursors.clone(),
            history: self.history.clone(),
            syntax_provider: self.syntax_provider.clone_box(),
            language_config: self.language_config.clone(),
            decorations: self.decorations.clone(),
            pending_edit_deltas: self.pending_edit_deltas.clone(),
            folded_lines: self.folded_lines.clone(),
            cached_folds: self.cached_folds.clone(),
            folds_valid: self.folds_valid,
            ime: self.ime.clone(),
            tab_size: self.tab_size,
            insert_spaces: self.insert_spaces,
            auto_closing_pairs_enabled: self.auto_closing_pairs_enabled,
            wrap_column: self.wrap_column,
            search: self.search.clone(),
            diagnostics: self.diagnostics.clone(),
            theme: self.theme.clone(),
            scroll_top: self.scroll_top,
            scroll_left: self.scroll_left,
            viewport_height: self.viewport_height,
            line_height: self.line_height,
            char_width: self.char_width,
            read_only: self.read_only,
            is_pointer_dragging: self.is_pointer_dragging,
            drag_mode: self.drag_mode,
            multi_cursor_modifier: self.multi_cursor_modifier,
            column_selection_mode: self.column_selection_mode,
            render_whitespace: self.render_whitespace,
            render_control_chars: self.render_control_chars,
            last_pointer_click: self.last_pointer_click,
            selection_expansion_stack: self.selection_expansion_stack.clone(),
            edit_revision: self.edit_revision,
        }
    }
}

impl std::fmt::Debug for CodeEditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeEditor")
            .field("language_id", &self.language_config.id)
            .field("lines", &self.buffer.line_count())
            .field("cursors", &self.cursors.count())
            .field("edit_revision", &self.edit_revision)
            .field("read_only", &self.read_only)
            .finish()
    }
}

impl CodeEditor {
    /// Constructs a raw, zero-grammar microkernel editor backed by [`PlainTextSyntaxProvider`].
    pub fn new_raw(content: &str) -> Self {
        Self::with_provider(
            content,
            LanguageConfig::plaintext(),
            Box::new(PlainTextSyntaxProvider::new()),
        )
    }

    /// Constructs a code editor with a pluggable [`LanguageConfig`] and [`SyntaxProvider`].
    pub fn with_provider(
        content: &str,
        config: LanguageConfig,
        mut syntax_provider: Box<dyn SyntaxProvider>,
    ) -> Self {
        let buffer = LineBuffer::from_str(content);
        syntax_provider.parse_full(&buffer);
        let mut diagnostics = DiagnosticCollection::new();
        diagnostics.set_syntax_diagnostics(syntax_provider.syntax_diagnostics(&buffer));
        let tab_size = config.indent_size.max(1);
        Self {
            buffer,
            cursors: MultiCursor::new(),
            history: HistoryStack::default(),
            syntax_provider,
            language_config: config,
            decorations: DecorationStore::new(),
            pending_edit_deltas: Vec::new(),
            folded_lines: Vec::new(),
            cached_folds: Vec::new(),
            folds_valid: false,
            ime: ImeComposition::new(),
            tab_size,
            insert_spaces: true,
            auto_closing_pairs_enabled: false,
            wrap_column: None,
            search: FindReplaceState::new(),
            diagnostics,
            theme: EditorTheme::default(),
            scroll_top: 0.0,
            scroll_left: 0.0,
            viewport_height: 480.0,
            line_height: 22.0,
            char_width: 7.8,
            read_only: false,
            is_pointer_dragging: false,
            drag_mode: PointerDragMode::Char,
            multi_cursor_modifier: MultiCursorModifier::Alt,
            column_selection_mode: false,
            render_whitespace: RenderWhitespace::Selection,
            render_control_chars: false,
            last_pointer_click: None,
            selection_expansion_stack: Vec::new(),
            edit_revision: 0,
        }
    }

    /// Constructs a new code editor with initial source code and a built-in [`Language`].
    #[cfg(feature = "builtin-syntax")]
    pub fn new(content: &str, language: Language) -> Self {
        let config = language.to_language_config();
        let highlighter = SyntaxHighlighter::with_language(language);
        Self::with_provider(content, config, Box::new(highlighter))
    }

    /// Monotonic counter incremented **only** when document buffer text actually mutates.
    /// Non-mutating shortcuts (`Ctrl+F`, `Ctrl+H`, `Ctrl+A`, `Ctrl+C`, navigation, folding)
    /// leave `edit_revision()` unchanged so `on_change` callbacks never fire falsely.
    pub fn edit_revision(&self) -> u64 {
        self.edit_revision
    }

    /// Synchronizes live syntax diagnostics, invalidates cached folds, and refreshes open search results.
    fn on_buffer_mutated(&mut self) {
        self.folds_valid = false;
        self.diagnostics
            .set_syntax_diagnostics(self.syntax_provider.syntax_diagnostics(&self.buffer));
        if self.search.is_open && !self.search.query.is_empty() {
            self.search.recompute_matches(&self.buffer);
        }
    }

    /// Replaces the active [`LanguageConfig`] and [`SyntaxProvider`] dynamically and re-parses.
    pub fn set_syntax_provider(
        &mut self,
        config: LanguageConfig,
        mut provider: Box<dyn SyntaxProvider>,
    ) {
        provider.parse_full(&self.buffer);
        self.tab_size = config.indent_size.max(1);
        self.language_config = config;
        self.syntax_provider = provider;
        self.folded_lines.clear();
        self.on_buffer_mutated();
    }

    /// Accesses the active pluggable [`SyntaxProvider`].
    pub fn syntax_provider(&self) -> &dyn SyntaxProvider {
        self.syntax_provider.as_ref()
    }

    /// Mutably accesses the active pluggable [`SyntaxProvider`].
    pub fn syntax_provider_mut(&mut self) -> &mut dyn SyntaxProvider {
        self.syntax_provider.as_mut()
    }

    /// Returns the active decoupled [`LanguageId`].
    pub fn language_id(&self) -> &LanguageId {
        &self.language_config.id
    }

    /// Returns the active [`LanguageConfig`].
    pub fn language_config(&self) -> &LanguageConfig {
        &self.language_config
    }

    /// Returns the human-readable display name for the active language (e.g. `"Plain Text"`, `"Rust"`).
    pub fn language_display_name(&self) -> &str {
        &self.language_config.display_name
    }

    /// Tokenizes a single logical line using the active [`SyntaxProvider`].
    pub fn tokenize_line(&self, line_idx: usize, line_text: &str) -> Vec<SyntaxToken> {
        self.syntax_provider.tokenize_line(line_idx, line_text)
    }

    /// Returns the matching bracket position for `pos`, if any.
    pub fn matching_bracket(&self, pos: Position) -> Option<Position> {
        self.syntax_provider
            .matching_bracket(&self.buffer, pos)
            .map(|(_, mate)| mate)
    }

    /// Returns the pinned Sticky Scroll scopes for the current top scroll line.
    ///
    /// Uses plugin-provided sticky scopes if present; otherwise queries the active
    /// [`SyntaxProvider::enclosing_scopes`] automatically.
    pub fn enclosing_scopes_at_scroll(&self) -> Vec<StickyScope> {
        if !self.decorations.sticky_scroll_enabled() {
            return Vec::new();
        }
        let explicit = self.decorations.sticky_scopes();
        if !explicit.is_empty() {
            return explicit.to_vec();
        }
        let effective_lh = self.line_height.max(14.0);
        let first_visible = (self.scroll_top / effective_lh).floor() as usize;
        let logical_top = self
            .display_row_to_logical(first_visible)
            .unwrap_or(first_visible);
        if logical_top == 0 {
            return Vec::new();
        }
        self.syntax_provider
            .enclosing_scopes(&self.buffer, logical_top)
    }

    /// Accesses the plugin-isolated [`DecorationStore`] (gutter markers, breakpoints, colors,
    /// inlay hints, sticky scroll, completion & hover popups).
    pub fn decorations(&self) -> &DecorationStore {
        &self.decorations
    }

    /// Mutably accesses the plugin-isolated [`DecorationStore`].
    pub fn decorations_mut(&mut self) -> &mut DecorationStore {
        &mut self.decorations
    }

    /// Drains accumulated pure-Rust [`TextEditDelta`] records emitted since the last call
    /// (for incremental LSP `textDocument/didChange` and live Git diff listeners).
    pub fn take_edit_deltas(&mut self) -> Vec<TextEditDelta> {
        std::mem::take(&mut self.pending_edit_deltas)
    }

    /// Accepts the currently highlighted item in the IntelliSense completion popup, replacing
    /// any identifier prefix immediately before the primary cursor. Returns the accepted label.
    pub fn accept_completion(&mut self) -> Option<String> {
        if self.read_only || !self.decorations.completion.visible {
            return None;
        }
        let item = self.decorations.completion.active_item()?.clone();
        self.decorations.completion.close();

        let focus = self.cursors.primary().focus;
        let line_str = self.buffer.line(focus.line).unwrap_or("");
        let chars: Vec<char> = line_str.chars().collect();
        let mut prefix_start = focus.col.min(chars.len());
        while prefix_start > 0 && is_word_char(chars[prefix_start - 1]) {
            prefix_start -= 1;
        }
        let replace_start = Position::new(focus.line, prefix_start);
        let insert_text = if item.insert_text.is_empty() {
            item.label.clone()
        } else {
            item.insert_text.clone()
        };
        self.replace_range(replace_start, focus, &insert_text);
        self.ensure_cursor_visible();
        Some(item.label)
    }

    /// Accesses the active built-in [`SyntaxHighlighter`] if this editor was constructed with one.
    #[cfg(feature = "builtin-syntax")]
    pub fn try_highlighter(&self) -> Option<&SyntaxHighlighter> {
        self.syntax_provider
            .as_any()
            .and_then(|a| a.downcast_ref::<SyntaxHighlighter>())
    }

    /// Accesses the active built-in [`SyntaxHighlighter`].
    ///
    /// Panics if the editor was constructed with [`CodeEditor::new_raw`] or a custom non-builtin
    /// [`SyntaxProvider`]; prefer [`CodeEditor::tokenize_line`] or [`CodeEditor::try_highlighter`]
    /// for provider-agnostic code.
    #[cfg(feature = "builtin-syntax")]
    pub fn highlighter(&self) -> &SyntaxHighlighter {
        self.try_highlighter().expect(
            "CodeEditor does not use builtin SyntaxHighlighter; use tokenize_line() or syntax_provider()",
        )
    }

    /// Mutably accesses the active built-in [`SyntaxHighlighter`].
    #[cfg(feature = "builtin-syntax")]
    pub fn highlighter_mut(&mut self) -> &mut SyntaxHighlighter {
        self.syntax_provider
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<SyntaxHighlighter>())
            .expect(
                "CodeEditor does not use builtin SyntaxHighlighter; use syntax_provider_mut()",
            )
    }

    /// Returns the active built-in [`Language`] enum (defaults to `Language::PlainText` for
    /// custom or raw providers).
    #[cfg(feature = "builtin-syntax")]
    pub fn language(&self) -> Language {
        if let Some(h) = self.try_highlighter() {
            return h.language();
        }
        match self.language_config.id.as_str() {
            "rust" => Language::Rust,
            "javascript" => Language::JavaScript,
            "typescript" => Language::TypeScript,
            "python" => Language::Python,
            "cpp" => Language::Cpp,
            "json" => Language::Json,
            "yaml" => Language::Yaml,
            "toml" => Language::Toml,
            "xml" => Language::XmlSdf,
            "markdown" => Language::Markdown,
            "shell" => Language::Shell,
            _ => Language::PlainText,
        }
    }

    /// Changes the syntax language using the built-in grammar engine and re-parses the rope buffer.
    #[cfg(feature = "builtin-syntax")]
    pub fn set_language(&mut self, language: Language) {
        let config = language.to_language_config();
        let highlighter = SyntaxHighlighter::with_language(language);
        self.set_syntax_provider(config, Box::new(highlighter));
    }

    /// Re-parses the document incrementally with the active [`SyntaxProvider`].
    pub fn reparse(&mut self) {
        let total = self.buffer.line_count();
        self.syntax_provider.reparse_range(&self.buffer, 0, total);
        self.on_buffer_mutated();
    }

    /// Feeds one or more chronological [`TextEditDelta`] records into the active [`SyntaxProvider`],
    /// queues them for external listeners (LSP / Git), and runs localized incremental reparsing.
    fn apply_syntax_edits_and_reparse(&mut self, edits: &[TextEditDelta]) {
        if edits.is_empty() {
            return;
        }
        self.edit_revision = self.edit_revision.wrapping_add(1);
        let mut dirty_start = usize::MAX;
        let mut dirty_end = 0usize;
        for edit in edits {
            self.syntax_provider.apply_edit(&self.buffer, edit);
            if self.pending_edit_deltas.len() >= MAX_PENDING_DELTAS {
                self.pending_edit_deltas.drain(..MAX_PENDING_DELTAS / 2);
            }
            self.pending_edit_deltas.push(edit.clone());
            dirty_start = dirty_start.min(edit.start_position.row);
            let row_delta =
                edit.new_end_position.row as isize - edit.old_end_position.row as isize;
            let shifted_end = (dirty_end as isize + row_delta).max(0) as usize;
            dirty_end = shifted_end.max(edit.new_end_position.row.saturating_add(1));
        }
        if dirty_start == usize::MAX {
            dirty_start = 0;
        }
        self.syntax_provider
            .reparse_range(&self.buffer, dirty_start, dirty_end);
        self.on_buffer_mutated();
    }

    /// Accesses the underlying line buffer.
    pub fn buffer(&self) -> &LineBuffer {
        &self.buffer
    }

    /// Accesses the undo/redo history stack.
    pub fn history(&self) -> &HistoryStack {
        &self.history
    }

    /// Accesses the active Find & Replace state.
    pub fn search(&self) -> &FindReplaceState {
        &self.search
    }

    /// Mutably accesses the active Find & Replace state.
    pub fn search_mut(&mut self) -> &mut FindReplaceState {
        &mut self.search
    }

    /// Accesses the diagnostics collection (live syntax + external LSP diagnostics).
    pub fn diagnostics(&self) -> &DiagnosticCollection {
        &self.diagnostics
    }

    /// Replaces external/LSP diagnostics while preserving live syntax diagnostics.
    pub fn set_external_diagnostics(&mut self, diagnostics: Vec<EditorDiagnostic>) {
        self.diagnostics.set_external(diagnostics);
    }

    /// Clears external/LSP diagnostics.
    pub fn clear_external_diagnostics(&mut self) {
        self.diagnostics.clear_external();
    }

    /// Returns diagnostic severity counts for status bars and badges.
    pub fn diagnostic_counts(&self) -> DiagnosticCounts {
        self.diagnostics.counts()
    }

    /// Accesses the active CSS-variable editor theme.
    pub fn theme(&self) -> &EditorTheme {
        &self.theme
    }

    /// Updates the active editor theme.
    pub fn set_theme(&mut self, theme: EditorTheme) {
        self.theme = theme;
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    pub fn scroll_top(&self) -> f32 {
        self.scroll_top
    }

    pub fn set_scroll_top(&mut self, scroll_top: f32) {
        let max_scroll = self.max_scroll_top();
        self.scroll_top = scroll_top.clamp(0.0, max_scroll);
    }

    pub fn scroll_left(&self) -> f32 {
        self.scroll_left
    }

    pub fn set_scroll_left(&mut self, scroll_left: f32) {
        self.scroll_left = scroll_left.max(0.0);
    }

    pub fn viewport_height(&self) -> f32 {
        self.viewport_height
    }

    pub fn set_viewport_height(&mut self, height: f32) {
        self.viewport_height = height.max(40.0);
        let max_scroll = self.max_scroll_top();
        self.scroll_top = self.scroll_top.clamp(0.0, max_scroll);
    }

    pub fn line_height(&self) -> f32 {
        self.line_height
    }

    pub fn set_line_height(&mut self, line_height: f32) {
        self.line_height = line_height.max(12.0);
    }

    pub fn char_width(&self) -> f32 {
        self.char_width
    }

    pub fn set_char_width(&mut self, char_width: f32) {
        self.char_width = char_width.max(4.0);
    }

    pub fn max_scroll_top(&self) -> f32 {
        let visible_rows = self.visible_logical_lines().len().max(1) as f32;
        (visible_rows * self.line_height - self.viewport_height).max(0.0)
    }

    /// Accesses the active cursors.
    pub fn cursors(&self) -> &MultiCursor {
        &self.cursors
    }

    /// Accesses the active cursors mutably.
    pub fn cursors_mut(&mut self) -> &mut MultiCursor {
        &mut self.cursors
    }

    /// Returns the active IME composition state.
    pub fn ime(&self) -> &ImeComposition {
        &self.ime
    }

    /// Mutably accesses the IME composition state.
    pub fn ime_mut(&mut self) -> &mut ImeComposition {
        &mut self.ime
    }

    /// Returns the configured tab size (in spaces).
    pub fn tab_size(&self) -> usize {
        self.tab_size
    }

    /// Configures tab size.
    pub fn set_tab_size(&mut self, size: usize) {
        self.tab_size = size.max(1);
    }

    /// Returns whether indentation inserts spaces (`true`) or literal `\t` tabs (`false`).
    pub fn insert_spaces(&self) -> bool {
        self.insert_spaces
    }

    /// Configures whether indentation inserts spaces (`true`) or literal `\t` tabs (`false`).
    pub fn set_insert_spaces(&mut self, insert_spaces: bool) {
        self.insert_spaces = insert_spaces;
    }

    /// Returns whether `LanguageConfig::auto_closing_pairs` auto-close on single-char typing.
    pub fn auto_closing_pairs_enabled(&self) -> bool {
        self.auto_closing_pairs_enabled
    }

    /// Enables or disables `LanguageConfig`-driven auto-closing brackets and quotes.
    pub fn set_auto_closing_pairs(&mut self, enabled: bool) {
        self.auto_closing_pairs_enabled = enabled;
    }

    /// Returns whether undo history has at least one transaction.
    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    /// Returns whether redo history has at least one transaction.
    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// Returns the active multi-cursor click modifier (`Alt` or `CtrlCmd`).
    pub fn multi_cursor_modifier(&self) -> MultiCursorModifier {
        self.multi_cursor_modifier
    }

    /// Sets the active multi-cursor click modifier.
    pub fn set_multi_cursor_modifier(&mut self, modifier: MultiCursorModifier) {
        self.multi_cursor_modifier = modifier;
    }

    /// Toggles the multi-cursor click modifier between `Alt` and `CtrlCmd` and returns the new mode.
    pub fn toggle_multi_cursor_modifier(&mut self) -> MultiCursorModifier {
        self.multi_cursor_modifier = match self.multi_cursor_modifier {
            MultiCursorModifier::Alt => MultiCursorModifier::CtrlCmd,
            MultiCursorModifier::CtrlCmd => MultiCursorModifier::Alt,
        };
        self.multi_cursor_modifier
    }

    /// Returns whether rectangular Column Selection Mode is active.
    pub fn column_selection_mode(&self) -> bool {
        self.column_selection_mode
    }

    /// Enables or disables rectangular Column Selection Mode.
    pub fn set_column_selection_mode(&mut self, enabled: bool) {
        self.column_selection_mode = enabled;
    }

    /// Returns the active whitespace rendering mode.
    pub fn render_whitespace(&self) -> RenderWhitespace {
        self.render_whitespace
    }

    /// Sets the active whitespace rendering mode.
    pub fn set_render_whitespace(&mut self, mode: RenderWhitespace) {
        self.render_whitespace = mode;
    }

    /// Returns whether control characters are rendered visibly.
    pub fn render_control_chars(&self) -> bool {
        self.render_control_chars
    }

    /// Sets whether control characters are rendered visibly.
    pub fn set_render_control_chars(&mut self, enabled: bool) {
        self.render_control_chars = enabled;
    }

    /// Returns the current active pointer drag gesture mode.
    pub fn drag_mode(&self) -> PointerDragMode {
        self.drag_mode
    }

    /// Counts the total number of selected UTF-8 characters across all active selections
    /// (including line breaks between selected lines) for VS Code's `Ln X, Col Y (N selected)` badge.
    pub fn selected_char_count(&self) -> usize {
        let mut total = 0usize;
        for sel in self.cursors.all() {
            if sel.is_collapsed() {
                continue;
            }
            let (s, e) = sel.ordered_range();
            if s.line == e.line {
                total += e.col.saturating_sub(s.col);
            } else {
                let first_len = self.buffer.line_len(s.line);
                total += first_len.saturating_sub(s.col) + 1;
                for line in (s.line + 1)..e.line {
                    total += self.buffer.line_len(line) + 1;
                }
                total += e.col;
            }
        }
        total
    }

    /// Returns the soft-wrap column limit, if enabled.
    pub fn wrap_column(&self) -> Option<usize> {
        self.wrap_column
    }

    /// Configures soft-wrap column limit.
    pub fn set_wrap_column(&mut self, wrap: Option<usize>) {
        self.wrap_column = wrap;
    }

    /// Total number of lines in the buffer.
    pub fn line_count(&self) -> usize {
        self.buffer.line_count()
    }

    /// Serializes entire buffer content to a String.
    pub fn text(&self) -> String {
        self.buffer.to_string()
    }

    /// Replaces entire buffer content and resets history.
    pub fn set_text(&mut self, text: &str) {
        self.buffer = LineBuffer::from_str(text);
        self.cursors.set_single(Position::ZERO);
        self.history = HistoryStack::default();
        self.folded_lines.clear();
        self.selection_expansion_stack.clear();
        self.scroll_top = 0.0;
        self.scroll_left = 0.0;
        self.syntax_provider.parse_full(&self.buffer);
        self.edit_revision = self.edit_revision.wrapping_add(1);
        self.on_buffer_mutated();
    }

    fn indent_unit(&self) -> String {
        if self.insert_spaces {
            " ".repeat(self.tab_size.max(1))
        } else {
            "\t".to_string()
        }
    }

    fn char_at_position(&self, pos: Position) -> Option<char> {
        self.buffer.line(pos.line)?.chars().nth(pos.col)
    }

    fn char_before_position(&self, pos: Position) -> Option<char> {
        if pos.col == 0 {
            return None;
        }
        self.buffer.line(pos.line)?.chars().nth(pos.col - 1)
    }

    /// Inserts a character at all active cursors (honoring `LanguageConfig::auto_closing_pairs`
    /// when `auto_closing_pairs_enabled()` is active, or Enter with auto-indentation).
    pub fn insert_char(&mut self, ch: char) {
        if ch == '\n' {
            self.insert_newline();
            return;
        }
        if self.auto_closing_pairs_enabled {
            self.insert_char_with_auto_close(ch);
        } else {
            self.insert_text(&ch.to_string());
        }
    }

    /// Inserts a single character applying `LanguageConfig::auto_closing_pairs`:
    /// 1. Wraps non-empty selections when `ch` is an opening pair delimiter.
    /// 2. Skips over an immediately following matching closing delimiter.
    /// 3. Inserts both `open` + `close` and places each caret between them.
    pub fn insert_char_with_auto_close(&mut self, ch: char) {
        if ch == '\n' {
            self.insert_newline();
            return;
        }
        let open_pair = self
            .language_config
            .auto_closing_pairs
            .iter()
            .copied()
            .find(|&(o, _)| o == ch);
        let is_closer = self
            .language_config
            .auto_closing_pairs
            .iter()
            .any(|&(_, c)| c == ch);

        let any_non_collapsed = self.cursors.all().iter().any(|s| !s.is_collapsed());

        // 1. Wrap non-empty selections when typing an opening pair character.
        if any_non_collapsed {
            if let Some((open, close)) = open_pair {
                self.cursors.normalize();
                let before_all = self.cursors.all().to_vec();
                let descending = self.cursors.selections_descending();
                let mut operations = Vec::with_capacity(descending.len() * 2);
                let mut syntax_edits = Vec::with_capacity(descending.len() * 2);
                let mut new_sels: Vec<Selection> = Vec::with_capacity(descending.len());

                for sel in descending {
                    let (start, end) = sel.ordered_range();
                    if start != end {
                        let selected =
                            crate::clipboard::ClipboardBridge::from_selection(&self.buffer, sel)
                                .plain_text;
                        let wrapped = format!("{open}{selected}{close}");
                        let (del_edit, _) = self.buffer.compute_edit_delta(start, end, "");
                        let deleted = self
                            .buffer
                            .delete_range(start.line, start.col, end.line, end.col);
                        syntax_edits.push(del_edit);
                        operations.push(crate::history::EditOperation::Delete {
                            start,
                            end,
                            text: deleted,
                        });
                        let (ins_edit, _) = self.buffer.compute_edit_delta(start, start, &wrapped);
                        self.buffer.insert_str(start.line, start.col, &wrapped);
                        syntax_edits.push(ins_edit);
                        operations.push(crate::history::EditOperation::Insert {
                            pos: start,
                            text: wrapped.clone(),
                        });
                        let lines_vec: Vec<&str> = wrapped.split('\n').collect();
                        for s in &mut new_sels {
                            s.anchor = crate::cursor::shift_position_after_edit(
                                s.anchor, start, end, &lines_vec,
                            );
                            s.focus = crate::cursor::shift_position_after_edit(
                                s.focus, start, end, &lines_vec,
                            );
                        }
                        let inner_start = Position::new(start.line, start.col + 1);
                        let inner_end = if start.line == end.line {
                            Position::new(end.line, end.col + 1)
                        } else {
                            end
                        };
                        new_sels.push(Selection::range(inner_start, inner_end));
                    } else {
                        let pair_str = format!("{open}{close}");
                        let (ins_edit, _) = self.buffer.compute_edit_delta(start, start, &pair_str);
                        self.buffer.insert_str(start.line, start.col, &pair_str);
                        syntax_edits.push(ins_edit);
                        operations.push(crate::history::EditOperation::Insert {
                            pos: start,
                            text: pair_str.clone(),
                        });
                        let lines_vec = [pair_str.as_str()];
                        for s in &mut new_sels {
                            s.anchor = crate::cursor::shift_position_after_edit(
                                s.anchor, start, end, &lines_vec,
                            );
                            s.focus = crate::cursor::shift_position_after_edit(
                                s.focus, start, end, &lines_vec,
                            );
                        }
                        new_sels.push(Selection::point(Position::new(start.line, start.col + 1)));
                    }
                }

                self.cursors = MultiCursor::from_selections(new_sels);
                self.history
                    .push_transaction(crate::history::EditorTransaction::new(
                        operations,
                        before_all,
                        self.cursors.all().to_vec(),
                    ));
                self.apply_syntax_edits_and_reparse(&syntax_edits);
                return;
            }
            self.insert_text(&ch.to_string());
            return;
        }

        // 2. Skip-over closing delimiter when every collapsed cursor sits right before `ch`.
        if is_closer
            && self
                .cursors
                .all()
                .iter()
                .all(|s| self.char_at_position(s.focus) == Some(ch))
        {
            self.move_right(false);
            return;
        }

        // 3. Auto-insert matching pair when next char is boundary/whitespace (and for symmetric quotes
        //    previous char is not alphanumeric/identifier so apostrophes & lifetimes behave naturally).
        if let Some((open, close)) = open_pair {
            let can_auto_close = self.cursors.all().iter().all(|s| {
                let next_ok = match self.char_at_position(s.focus) {
                    None => true,
                    Some(nc) => {
                        nc.is_whitespace()
                            || matches!(nc, ')' | ']' | '}' | ',' | ';' | ':' | '.')
                    }
                };
                let prev_ok = if open == close {
                    !self
                        .char_before_position(s.focus)
                        .is_some_and(|pc| is_word_char(pc) || pc == '\\')
                } else {
                    true
                };
                next_ok && prev_ok
            });

            if can_auto_close {
                let pair_str = format!("{open}{close}");
                self.insert_text(&pair_str);
                self.move_left(false);
                return;
            }
        }

        self.insert_text(&ch.to_string());
    }

    /// Inserts a newline with automatic indentation calculated from the syntax engine.
    /// When the caret sits directly between a matching `LanguageConfig::brackets` pair
    /// (e.g. `{|}`, `[|]`, `(|)`), performs smart VS Code brace splitting across 3 lines.
    pub fn insert_newline(&mut self) {
        self.cursors.normalize();
        let before_all = self.cursors.all().to_vec();
        let descending = self.cursors.selections_descending();

        let mut operations = Vec::with_capacity(descending.len() * 2);
        let mut syntax_edits = Vec::with_capacity(descending.len() * 2);
        let mut processed_carets: Vec<Position> = Vec::with_capacity(descending.len());

        for sel in descending {
            let (start, end) = sel.ordered_range();
            if start != end {
                let (del_edit, _) = self.buffer.compute_edit_delta(start, end, "");
                let deleted = self
                    .buffer
                    .delete_range(start.line, start.col, end.line, end.col);
                syntax_edits.push(del_edit);
                operations.push(crate::history::EditOperation::Delete {
                    start,
                    end,
                    text: deleted,
                });
            }

            let base_line = self.buffer.line(start.line).unwrap_or("");
            let base_indent: String = base_line
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            let prev_ch = self.char_before_position(start);
            let next_ch = self.char_at_position(start);
            let between_brackets = match (prev_ch, next_ch) {
                (Some(o), Some(c)) => self
                    .language_config
                    .brackets
                    .iter()
                    .any(|&(bo, bc)| bo == o && bc == c),
                _ => false,
            };

            if between_brackets {
                let inner_indent = format!("{base_indent}{}", self.indent_unit());
                let inserted_text = format!("\n{inner_indent}\n{base_indent}");
                let (ins_edit, _) = self.buffer.compute_edit_delta(start, start, &inserted_text);

                self.buffer.insert_str(start.line, start.col, &inserted_text);
                syntax_edits.push(ins_edit);
                operations.push(crate::history::EditOperation::Insert {
                    pos: start,
                    text: inserted_text,
                });

                let lines_slice: Vec<&str> = vec!["", &inner_indent, &base_indent];
                let my_new_pos = Position::new(start.line + 1, inner_indent.chars().count());
                for caret in &mut processed_carets {
                    *caret =
                        crate::cursor::shift_position_after_edit(*caret, start, end, &lines_slice);
                }
                processed_carets.push(my_new_pos);
                continue;
            }

            let indent_spaces = self
                .syntax_provider
                .compute_auto_indent(&self.buffer, start.line, self.tab_size);
            let indent_str = if indent_spaces > 0 {
                " ".repeat(indent_spaces)
            } else {
                String::new()
            };
            let inserted_text = format!("\n{indent_str}");
            let (ins_edit, _) = self.buffer.compute_edit_delta(start, start, &inserted_text);

            self.buffer.split_line(start.line, start.col);
            let new_line = start.line + 1;
            if !indent_str.is_empty() {
                self.buffer.insert_str(new_line, 0, &indent_str);
            }
            syntax_edits.push(ins_edit);

            operations.push(crate::history::EditOperation::Insert {
                pos: start,
                text: inserted_text,
            });

            let lines_slice: Vec<&str> = vec!["", &indent_str];
            let my_new_pos = Position::new(new_line, indent_spaces);

            for caret in &mut processed_carets {
                *caret = crate::cursor::shift_position_after_edit(*caret, start, end, &lines_slice);
            }

            processed_carets.push(my_new_pos);
        }

        let new_cursors: Vec<Selection> =
            processed_carets.into_iter().map(Selection::point).collect();

        self.cursors = MultiCursor::from_selections(new_cursors);

        self.history
            .push_transaction(crate::history::EditorTransaction::new(
                operations,
                before_all,
                self.cursors.all().to_vec(),
            ));

        self.apply_syntax_edits_and_reparse(&syntax_edits);
    }

    /// Inserts text at all cursor positions atomically in descending document order.
    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        self.cursors.normalize();
        let before_all = self.cursors.all().to_vec();
        let descending = self.cursors.selections_descending();

        let mut operations = Vec::with_capacity(descending.len());
        let mut syntax_edits = Vec::with_capacity(descending.len() * 2);
        let mut processed_carets: Vec<Position> = Vec::with_capacity(descending.len());

        let text_lines: Vec<&str> = text.split('\n').collect();

        for sel in descending {
            let (start, end) = sel.ordered_range();
            if start != end {
                let (del_edit, _) = self.buffer.compute_edit_delta(start, end, "");
                let deleted = self
                    .buffer
                    .delete_range(start.line, start.col, end.line, end.col);
                syntax_edits.push(del_edit);
                operations.push(crate::history::EditOperation::Delete {
                    start,
                    end,
                    text: deleted,
                });
            }

            let (ins_edit, _) = self.buffer.compute_edit_delta(start, start, text);
            self.buffer.insert_str(start.line, start.col, text);
            syntax_edits.push(ins_edit);
            operations.push(crate::history::EditOperation::Insert {
                pos: start,
                text: text.to_string(),
            });

            let my_new_pos = if text_lines.len() == 1 {
                Position::new(start.line, start.col + text_lines[0].chars().count())
            } else {
                Position::new(
                    start.line + text_lines.len() - 1,
                    text_lines.last().map_or(0, |l| l.chars().count()),
                )
            };

            for caret in &mut processed_carets {
                *caret = crate::cursor::shift_position_after_edit(*caret, start, end, &text_lines);
            }

            processed_carets.push(my_new_pos);
        }

        let new_cursors: Vec<Selection> =
            processed_carets.into_iter().map(Selection::point).collect();

        self.cursors = MultiCursor::from_selections(new_cursors);

        self.history
            .push_transaction(crate::history::EditorTransaction::new(
                operations,
                before_all,
                self.cursors.all().to_vec(),
            ));

        self.apply_syntax_edits_and_reparse(&syntax_edits);
    }

    /// Performs backspace deletion at all active cursors atomically in descending document order.
    /// When `auto_closing_pairs_enabled()` is active and a cursor sits directly inside an empty
    /// matching `LanguageConfig::auto_closing_pairs` pair (e.g. `(|)`), deletes both delimiters.
    pub fn delete_backwards(&mut self) {
        self.cursors.normalize();
        let before_all = self.cursors.all().to_vec();
        let descending = self.cursors.selections_descending();

        let mut operations = Vec::with_capacity(descending.len());
        let mut syntax_edits = Vec::with_capacity(descending.len());
        let mut processed_carets: Vec<Position> = Vec::with_capacity(descending.len());

        for sel in descending {
            let (start, end) = sel.ordered_range();
            if start != end {
                let (del_edit, _) = self.buffer.compute_edit_delta(start, end, "");
                let deleted = self
                    .buffer
                    .delete_range(start.line, start.col, end.line, end.col);
                syntax_edits.push(del_edit);
                operations.push(crate::history::EditOperation::Delete {
                    start,
                    end,
                    text: deleted,
                });

                for caret in &mut processed_carets {
                    *caret = crate::cursor::shift_position_after_edit(*caret, start, end, &[""]);
                }

                processed_carets.push(start);
            } else if start.col > 0 {
                let del_start = Position::new(start.line, start.col - 1);
                let auto_delete_pair = self.auto_closing_pairs_enabled
                    && match (
                        self.char_before_position(start),
                        self.char_at_position(start),
                    ) {
                        (Some(o), Some(c)) => self
                            .language_config
                            .auto_closing_pairs
                            .iter()
                            .any(|&(po, pc)| po == o && pc == c),
                        _ => false,
                    };
                let del_end = if auto_delete_pair {
                    Position::new(start.line, start.col + 1)
                } else {
                    start
                };
                let (del_edit, _) = self.buffer.compute_edit_delta(del_start, del_end, "");
                let deleted = self.buffer.delete_range(
                    del_start.line,
                    del_start.col,
                    del_end.line,
                    del_end.col,
                );
                syntax_edits.push(del_edit);
                operations.push(crate::history::EditOperation::Delete {
                    start: del_start,
                    end: del_end,
                    text: deleted,
                });

                for caret in &mut processed_carets {
                    *caret = crate::cursor::shift_position_after_edit(
                        *caret,
                        del_start,
                        del_end,
                        &[""],
                    );
                }

                processed_carets.push(del_start);
            } else if start.line > 0 {
                let prev_line = start.line - 1;
                let prev_len = self.buffer.line_len(prev_line);
                let del_start = Position::new(prev_line, prev_len);
                let del_end = Position::new(start.line, 0);
                let (del_edit, _) = self.buffer.compute_edit_delta(del_start, del_end, "");

                self.buffer.join_line_with_next(prev_line);
                syntax_edits.push(del_edit);
                operations.push(crate::history::EditOperation::Delete {
                    start: del_start,
                    end: del_end,
                    text: "\n".to_string(),
                });

                for caret in &mut processed_carets {
                    *caret = crate::cursor::shift_position_after_edit(
                        *caret,
                        del_start,
                        del_end,
                        &[""],
                    );
                }

                processed_carets.push(del_start);
            } else {
                processed_carets.push(start);
            }
        }

        let new_cursors: Vec<Selection> =
            processed_carets.into_iter().map(Selection::point).collect();

        self.cursors = MultiCursor::from_selections(new_cursors);

        if !operations.is_empty() {
            self.history
                .push_transaction(crate::history::EditorTransaction::new(
                    operations,
                    before_all,
                    self.cursors.all().to_vec(),
                ));
            self.apply_syntax_edits_and_reparse(&syntax_edits);
        }
    }

    /// Performs forward Delete key deletion at all active cursors atomically.
    pub fn delete_forward(&mut self) {
        self.cursors.normalize();
        let before_all = self.cursors.all().to_vec();
        let descending = self.cursors.selections_descending();

        let mut operations = Vec::with_capacity(descending.len());
        let mut syntax_edits = Vec::with_capacity(descending.len());
        let mut processed_carets: Vec<Position> = Vec::with_capacity(descending.len());

        for sel in descending {
            let (start, end) = sel.ordered_range();
            if start != end {
                let (del_edit, _) = self.buffer.compute_edit_delta(start, end, "");
                let deleted = self
                    .buffer
                    .delete_range(start.line, start.col, end.line, end.col);
                syntax_edits.push(del_edit);
                operations.push(crate::history::EditOperation::Delete {
                    start,
                    end,
                    text: deleted,
                });
                for caret in &mut processed_carets {
                    *caret = crate::cursor::shift_position_after_edit(*caret, start, end, &[""]);
                }
                processed_carets.push(start);
            } else {
                let line_len = self.buffer.line_len(start.line);
                if start.col < line_len {
                    let del_end = Position::new(start.line, start.col + 1);
                    let (del_edit, _) = self.buffer.compute_edit_delta(start, del_end, "");
                    let deleted =
                        self.buffer
                            .delete_range(start.line, start.col, del_end.line, del_end.col);
                    syntax_edits.push(del_edit);
                    operations.push(crate::history::EditOperation::Delete {
                        start,
                        end: del_end,
                        text: deleted,
                    });
                    for caret in &mut processed_carets {
                        *caret = crate::cursor::shift_position_after_edit(
                            *caret,
                            start,
                            del_end,
                            &[""],
                        );
                    }
                    processed_carets.push(start);
                } else if start.line + 1 < self.buffer.line_count() {
                    let del_end = Position::new(start.line + 1, 0);
                    let (del_edit, _) = self.buffer.compute_edit_delta(start, del_end, "");
                    self.buffer.join_line_with_next(start.line);
                    syntax_edits.push(del_edit);
                    operations.push(crate::history::EditOperation::Delete {
                        start,
                        end: del_end,
                        text: "\n".to_string(),
                    });
                    for caret in &mut processed_carets {
                        *caret = crate::cursor::shift_position_after_edit(
                            *caret,
                            start,
                            del_end,
                            &[""],
                        );
                    }
                    processed_carets.push(start);
                } else {
                    processed_carets.push(start);
                }
            }
        }

        let new_cursors: Vec<Selection> =
            processed_carets.into_iter().map(Selection::point).collect();
        self.cursors = MultiCursor::from_selections(new_cursors);

        if !operations.is_empty() {
            self.history
                .push_transaction(crate::history::EditorTransaction::new(
                    operations,
                    before_all,
                    self.cursors.all().to_vec(),
                ));
            self.apply_syntax_edits_and_reparse(&syntax_edits);
        }
    }

    /// Replaces an arbitrary `[start, end)` range with `replacement` as a single undoable
    /// transaction and drives an incremental Tree-Sitter / line-state update.
    /// Returns the resulting end `Position` of `replacement`.
    pub fn replace_range(
        &mut self,
        start: Position,
        end: Position,
        replacement: &str,
    ) -> Position {
        let (s, e) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let before_all = self.cursors.all().to_vec();
        let mut operations = Vec::with_capacity(2);
        let mut syntax_edits = Vec::with_capacity(2);

        if s != e {
            let (del_edit, _) = self.buffer.compute_edit_delta(s, e, "");
            let deleted = self.buffer.delete_range(s.line, s.col, e.line, e.col);
            syntax_edits.push(del_edit);
            operations.push(crate::history::EditOperation::Delete {
                start: s,
                end: e,
                text: deleted,
            });
        }

        let new_end = if !replacement.is_empty() {
            let (ins_edit, end_pos) = self.buffer.compute_edit_delta(s, s, replacement);
            self.buffer.insert_str(s.line, s.col, replacement);
            syntax_edits.push(ins_edit);
            operations.push(crate::history::EditOperation::Insert {
                pos: s,
                text: replacement.to_string(),
            });
            end_pos
        } else {
            s
        };

        self.cursors.set_single(new_end);
        if !operations.is_empty() {
            self.history
                .push_transaction(crate::history::EditorTransaction::new(
                    operations,
                    before_all,
                    self.cursors.all().to_vec(),
                ));
            self.apply_syntax_edits_and_reparse(&syntax_edits);
        }
        new_end
    }

    /// Atomically replaces multiple disjoint `(start, end, replacement)` ranges in descending
    /// document order as a single undo transaction and a single incremental AST reparse.
    pub fn replace_ranges_atomic(&mut self, mut replacements: Vec<(Position, Position, String)>) {
        if replacements.is_empty() {
            return;
        }
        // Normalize and sort descending so earlier offsets remain valid as later ones change.
        for item in &mut replacements {
            if item.0 > item.1 {
                std::mem::swap(&mut item.0, &mut item.1);
            }
        }
        replacements.sort_by_key(|item| std::cmp::Reverse(item.0));

        let before_all = self.cursors.all().to_vec();
        let mut operations = Vec::with_capacity(replacements.len() * 2);
        let mut syntax_edits = Vec::with_capacity(replacements.len() * 2);
        let mut last_pos = Position::ZERO;

        for (s, e, repl) in replacements {
            if s != e {
                let (del_edit, _) = self.buffer.compute_edit_delta(s, e, "");
                let deleted = self.buffer.delete_range(s.line, s.col, e.line, e.col);
                syntax_edits.push(del_edit);
                operations.push(crate::history::EditOperation::Delete {
                    start: s,
                    end: e,
                    text: deleted,
                });
            }
            if !repl.is_empty() {
                let (ins_edit, end_pos) = self.buffer.compute_edit_delta(s, s, &repl);
                self.buffer.insert_str(s.line, s.col, &repl);
                syntax_edits.push(ins_edit);
                operations.push(crate::history::EditOperation::Insert { pos: s, text: repl });
                last_pos = end_pos;
            } else {
                last_pos = s;
            }
        }

        self.cursors.set_single(last_pos);
        if !operations.is_empty() {
            self.history
                .push_transaction(crate::history::EditorTransaction::new(
                    operations,
                    before_all,
                    self.cursors.all().to_vec(),
                ));
            self.apply_syntax_edits_and_reparse(&syntax_edits);
        }
    }

    /// Undoes the last recorded transaction with incremental syntax synchronization.
    pub fn undo(&mut self) {
        if let Some((restored, edits)) = self.history.undo_with_deltas(&mut self.buffer) {
            self.cursors = MultiCursor::from_selections(restored);
            self.apply_syntax_edits_and_reparse(&edits);
        }
    }

    /// Redoes the last undone transaction with incremental syntax synchronization.
    pub fn redo(&mut self) {
        if let Some((restored, edits)) = self.history.redo_with_deltas(&mut self.buffer) {
            self.cursors = MultiCursor::from_selections(restored);
            self.apply_syntax_edits_and_reparse(&edits);
        }
    }

    /// Moves all active cursors left by 1 character.
    pub fn move_left(&mut self, select: bool) {
        let before = self.cursors.all().to_vec();
        let mut new_cursors = Vec::with_capacity(before.len());

        for sel in &before {
            let focus = sel.focus;
            let new_focus = if focus.col > 0 {
                Position::new(focus.line, focus.col - 1)
            } else if focus.line > 0 {
                let prev_line = focus.line - 1;
                let prev_len = self.buffer.line_len(prev_line);
                Position::new(prev_line, prev_len)
            } else {
                Position::ZERO
            };

            let anchor = if select { sel.anchor } else { new_focus };
            new_cursors.push(Selection::range(anchor, new_focus));
        }

        self.cursors = MultiCursor::from_selections(new_cursors);
    }

    /// Moves all active cursors right by 1 character.
    pub fn move_right(&mut self, select: bool) {
        let before = self.cursors.all().to_vec();
        let mut new_cursors = Vec::with_capacity(before.len());

        for sel in &before {
            let focus = sel.focus;
            let cur_line_len = self.buffer.line_len(focus.line);

            let new_focus = if focus.col < cur_line_len {
                Position::new(focus.line, focus.col + 1)
            } else if focus.line + 1 < self.buffer.line_count() {
                Position::new(focus.line + 1, 0)
            } else {
                Position::new(focus.line, cur_line_len)
            };

            let anchor = if select { sel.anchor } else { new_focus };
            new_cursors.push(Selection::range(anchor, new_focus));
        }

        self.cursors = MultiCursor::from_selections(new_cursors);
    }

    /// Moves all active cursors up by 1 line.
    pub fn move_up(&mut self, select: bool) {
        let before = self.cursors.all().to_vec();
        let mut new_cursors = Vec::with_capacity(before.len());

        for sel in &before {
            let focus = sel.focus;
            let new_focus = if focus.line > 0 {
                let prev_line = focus.line - 1;
                let prev_len = self.buffer.line_len(prev_line);
                Position::new(prev_line, focus.col.min(prev_len))
            } else {
                Position::new(0, 0)
            };

            let anchor = if select { sel.anchor } else { new_focus };
            new_cursors.push(Selection::range(anchor, new_focus));
        }

        self.cursors = MultiCursor::from_selections(new_cursors);
    }

    /// Moves all active cursors down by 1 line.
    pub fn move_down(&mut self, select: bool) {
        let before = self.cursors.all().to_vec();
        let mut new_cursors = Vec::with_capacity(before.len());

        for sel in &before {
            let focus = sel.focus;
            let new_focus = if focus.line + 1 < self.buffer.line_count() {
                let next_line = focus.line + 1;
                let next_len = self.buffer.line_len(next_line);
                Position::new(next_line, focus.col.min(next_len))
            } else {
                let last_len = self.buffer.line_len(focus.line);
                Position::new(focus.line, last_len)
            };

            let anchor = if select { sel.anchor } else { new_focus };
            new_cursors.push(Selection::range(anchor, new_focus));
        }

        self.cursors = MultiCursor::from_selections(new_cursors);
    }

    /// Moves all active cursors to the start of their respective line.
    pub fn move_to_line_start(&mut self, select: bool) {
        let before = self.cursors.all().to_vec();
        let mut new_cursors = Vec::with_capacity(before.len());

        for sel in &before {
            let new_focus = Position::new(sel.focus.line, 0);
            let anchor = if select { sel.anchor } else { new_focus };
            new_cursors.push(Selection::range(anchor, new_focus));
        }

        self.cursors = MultiCursor::from_selections(new_cursors);
    }

    /// Moves all active cursors to the end of their respective line.
    pub fn move_to_line_end(&mut self, select: bool) {
        let before = self.cursors.all().to_vec();
        let mut new_cursors = Vec::with_capacity(before.len());

        for sel in &before {
            let line_len = self.buffer.line_len(sel.focus.line);
            let new_focus = Position::new(sel.focus.line, line_len);
            let anchor = if select { sel.anchor } else { new_focus };
            new_cursors.push(Selection::range(anchor, new_focus));
        }

        self.cursors = MultiCursor::from_selections(new_cursors);
    }

    /// Selects all text in the document.
    pub fn select_all(&mut self) {
        let total_lines = self.buffer.line_count();
        let last_line = total_lines.saturating_sub(1);
        let last_col = self.buffer.line_len(last_line);
        self.cursors.set_single(Position::ZERO);
        self.cursors.all_mut()[0] =
            Selection::range(Position::ZERO, Position::new(last_line, last_col));
    }

    /// Ensures structural fold ranges are computed and cached for the current buffer state.
    pub fn ensure_folds_cached(&mut self) -> &[FoldRange] {
        if !self.folds_valid {
            self.cached_folds = self.syntax_provider.fold_ranges(&self.buffer);
            self.folds_valid = true;
            // Prune stale folded line indices that no longer correspond to a fold start.
            self.folded_lines.retain(|&line| {
                self.cached_folds.iter().any(|f| f.start_line == line)
            });
        }
        &self.cached_folds
    }

    /// Returns all structural fold ranges for the current document (using cache when valid).
    pub fn fold_ranges(&self) -> Vec<FoldRange> {
        if self.folds_valid {
            self.cached_folds.clone()
        } else {
            self.syntax_provider.fold_ranges(&self.buffer)
        }
    }

    /// Toggles code folding on the specified line.
    pub fn toggle_fold(&mut self, line: usize) {
        if let Some(pos) = self.folded_lines.iter().position(|&l| l == line) {
            self.folded_lines.remove(pos);
        } else {
            self.folded_lines.push(line);
        }
    }

    /// Toggles folding at `line` only if `line` starts a foldable range; returns `true` if toggled.
    pub fn toggle_fold_at(&mut self, line: usize) -> bool {
        let is_fold_start = self
            .ensure_folds_cached()
            .iter()
            .any(|f| f.start_line == line && f.end_line > f.start_line);
        if !is_fold_start {
            return false;
        }
        self.toggle_fold(line);
        true
    }

    /// Collapses every foldable block in the document.
    pub fn fold_all(&mut self) {
        let starts: Vec<usize> = self
            .ensure_folds_cached()
            .iter()
            .filter(|f| f.end_line > f.start_line)
            .map(|f| f.start_line)
            .collect();
        self.folded_lines = starts;
        self.folded_lines.sort_unstable();
        self.folded_lines.dedup();
    }

    /// Expands all folded regions in the document.
    pub fn unfold_all(&mut self) {
        self.folded_lines.clear();
    }

    /// Checks if the line is currently folded.
    pub fn is_folded(&self, line: usize) -> bool {
        self.folded_lines.contains(&line)
    }

    /// If `line` is currently folded, returns the number of hidden lines beneath it.
    pub fn folded_hidden_count(&self, line: usize) -> Option<usize> {
        if !self.is_folded(line) {
            return None;
        }
        let folds = self.fold_ranges();
        folds
            .iter()
            .filter(|f| f.start_line == line && f.end_line > line)
            .map(|f| f.end_line - f.start_line)
            .max()
    }

    /// Returns the ordered list of visible 0-indexed logical line numbers after skipping
    /// lines concealed inside collapsed fold regions.
    pub fn visible_logical_lines(&self) -> Vec<usize> {
        let total = self.buffer.line_count();
        if self.folded_lines.is_empty() {
            return (0..total).collect();
        }
        let folds = self.fold_ranges();
        let mut collapsed_spans: Vec<(usize, usize)> = folds
            .iter()
            .filter(|f| self.folded_lines.contains(&f.start_line) && f.end_line > f.start_line)
            .map(|f| (f.start_line + 1, f.end_line.min(total.saturating_sub(1))))
            .collect();
        if collapsed_spans.is_empty() {
            return (0..total).collect();
        }
        collapsed_spans.sort_unstable_by_key(|&(s, _)| s);

        let mut visible = Vec::with_capacity(total);
        let mut line = 0usize;
        let mut span_idx = 0usize;
        while line < total {
            while span_idx < collapsed_spans.len() && collapsed_spans[span_idx].1 < line {
                span_idx += 1;
            }
            if span_idx < collapsed_spans.len()
                && line >= collapsed_spans[span_idx].0
                && line <= collapsed_spans[span_idx].1
            {
                line = collapsed_spans[span_idx].1 + 1;
                continue;
            }
            visible.push(line);
            line += 1;
        }
        visible
    }

    /// Maps a 0-indexed logical line number to its visible display row index (or `None` if hidden inside a fold).
    pub fn logical_to_display_row(&self, logical_line: usize) -> Option<usize> {
        if self.folded_lines.is_empty() {
            return (logical_line < self.buffer.line_count()).then_some(logical_line);
        }
        let visible = self.visible_logical_lines();
        visible.binary_search(&logical_line).ok()
    }

    /// Maps a visible display row index back to its 0-indexed logical line number.
    pub fn display_row_to_logical(&self, display_row: usize) -> Option<usize> {
        if self.folded_lines.is_empty() {
            return (display_row < self.buffer.line_count()).then_some(display_row);
        }
        let visible = self.visible_logical_lines();
        visible.get(display_row).copied()
    }

    // -------------------------------------------------------------------------
    // Find & Replace Controller Delegation
    // -------------------------------------------------------------------------

    pub fn open_find(&mut self) {
        self.search.open_find();
        if !self.search.query.is_empty() {
            self.search.recompute_matches(&self.buffer);
        }
    }

    pub fn open_replace(&mut self) {
        self.search.open_replace();
        if !self.search.query.is_empty() {
            self.search.recompute_matches(&self.buffer);
        }
    }

    pub fn close_find(&mut self) {
        self.search.close();
    }

    pub fn set_find_query(&mut self, query: impl Into<String>) {
        self.search.set_query(query, &self.buffer);
        if let Some(m) = self.search.active_match().cloned() {
            self.select_search_match(&m);
        }
    }

    pub fn set_replace_text(&mut self, replacement: impl Into<String>) {
        self.search.set_replacement(replacement);
    }

    pub fn find_next(&mut self) -> Option<SearchMatch> {
        if self.search.matches.is_empty() && !self.search.query.is_empty() {
            self.search.recompute_matches(&self.buffer);
        }
        let m = self.search.find_next()?;
        self.select_search_match(&m);
        Some(m)
    }

    pub fn find_prev(&mut self) -> Option<SearchMatch> {
        if self.search.matches.is_empty() && !self.search.query.is_empty() {
            self.search.recompute_matches(&self.buffer);
        }
        let m = self.search.find_prev()?;
        self.select_search_match(&m);
        Some(m)
    }

    pub fn replace_current(&mut self) -> bool {
        if self.read_only {
            return false;
        }
        let mut search = std::mem::take(&mut self.search);
        let replaced = search.replace_current(self);
        self.search = search;
        if let Some(m) = self.search.active_match().cloned() {
            self.select_search_match(&m);
        }
        replaced
    }

    pub fn replace_all(&mut self) -> usize {
        if self.read_only {
            return 0;
        }
        let mut search = std::mem::take(&mut self.search);
        let count = search.replace_all(self);
        self.search = search;
        count
    }

    fn select_search_match(&mut self, m: &SearchMatch) {
        self.cursors.set_single(m.start);
        self.cursors.all_mut()[0] = Selection::range(m.start, m.end);
        self.ensure_line_visible(m.start.line);
    }

    // -------------------------------------------------------------------------
    // Monaco-Class Editing, Navigation, Scrolling & Input Event Handlers
    // -------------------------------------------------------------------------

    /// Toggles single-line comments across all lines touched by active selections,
    /// driven 100% by `LanguageConfig::line_comment` (falling back to `block_comment` if
    /// `line_comment` is `None`). Uses minimum common indentation across non-empty lines
    /// and preserves active cursor positions.
    pub fn toggle_line_comment(&mut self) {
        if self.read_only {
            return;
        }
        let Some(prefix_owned) = self.language_config.line_comment.clone() else {
            if self.language_config.block_comment.is_some() {
                self.toggle_block_comment();
            }
            return;
        };
        let prefix = prefix_owned.as_str();
        let mut lines: Vec<usize> = Vec::new();
        for sel in self.cursors.all() {
            let (s, e) = sel.ordered_range();
            let last_line = if e.line > s.line && e.col == 0 {
                e.line - 1
            } else {
                e.line
            };
            for line in s.line..=last_line {
                if !lines.contains(&line) {
                    lines.push(line);
                }
            }
        }
        if lines.is_empty() {
            return;
        }
        lines.sort_unstable();

        let non_empty_lines: Vec<usize> = lines
            .iter()
            .copied()
            .filter(|&l| {
                self.buffer
                    .line(l)
                    .map(|txt| !txt.trim().is_empty())
                    .unwrap_or(false)
            })
            .collect();

        let target_lines = if non_empty_lines.is_empty() {
            lines.clone()
        } else {
            non_empty_lines
        };

        let all_commented = target_lines.iter().all(|&l| {
            self.buffer
                .line(l)
                .map(|txt| txt.trim_start().starts_with(prefix))
                .unwrap_or(true)
        });

        let min_indent = target_lines
            .iter()
            .filter_map(|&l| self.buffer.line(l))
            .map(|txt| txt.chars().take_while(|c| *c == ' ' || *c == '\t').count())
            .min()
            .unwrap_or(0);

        let before_sels = self.cursors.all().to_vec();
        let mut edits = Vec::new();
        let mut line_deltas: Vec<(usize, usize, isize)> = Vec::new();
        let prefix_chars = prefix.chars().count();

        for line in target_lines {
            let Some(txt) = self.buffer.line(line) else {
                continue;
            };
            let indent = txt.chars().take_while(|c| *c == ' ' || *c == '\t').count();
            if all_commented {
                let after_indent: String = txt.chars().skip(indent).collect();
                if let Some(rest) = after_indent.strip_prefix(prefix) {
                    let remove_len = if rest.starts_with(' ') {
                        prefix_chars + 1
                    } else {
                        prefix_chars
                    };
                    edits.push((
                        Position::new(line, indent),
                        Position::new(line, indent + remove_len),
                        String::new(),
                    ));
                    line_deltas.push((line, indent, -(remove_len as isize)));
                }
            } else {
                let insert_col = min_indent.min(txt.chars().count());
                let insert_str = format!("{prefix} ");
                let delta = insert_str.chars().count() as isize;
                edits.push((
                    Position::new(line, insert_col),
                    Position::new(line, insert_col),
                    insert_str,
                ));
                line_deltas.push((line, insert_col, delta));
            }
        }
        if edits.is_empty() {
            return;
        }
        self.replace_ranges_atomic(edits);

        // Restore cursor selections shifted cleanly by per-line comment deltas.
        let adjust_pos = |pos: Position| -> Position {
            if let Some(&(_, col_at, delta)) = line_deltas.iter().find(|&&(l, _, _)| l == pos.line)
            {
                let new_col = if pos.col >= col_at {
                    (pos.col as isize + delta).max(col_at as isize) as usize
                } else {
                    pos.col
                };
                Position::new(pos.line, new_col.min(self.buffer.line_len(pos.line)))
            } else {
                pos
            }
        };
        let adjusted: Vec<Selection> = before_sels
            .into_iter()
            .map(|sel| Selection::range(adjust_pos(sel.anchor), adjust_pos(sel.focus)))
            .collect();
        self.cursors = MultiCursor::from_selections(adjusted);
    }

    /// Toggles block comments (`/* ... */`, `<!-- ... -->`, etc.) across active selections,
    /// driven 100% by `LanguageConfig::block_comment`.
    pub fn toggle_block_comment(&mut self) {
        if self.read_only {
            return;
        }
        let Some((open, close)) = self.language_config.block_comment.clone() else {
            return;
        };
        let open_chars = open.chars().count();
        let close_chars = close.chars().count();
        let sels = self.cursors.selections_descending();
        let mut edits = Vec::new();
        let mut final_caret: Option<Position> = None;

        for sel in sels {
            let (s, e) = sel.ordered_range();
            if s != e {
                let selected =
                    crate::clipboard::ClipboardBridge::from_selection(&self.buffer, sel).plain_text;
                let trimmed = selected.trim();
                if trimmed.starts_with(&open)
                    && trimmed.ends_with(&close)
                    && trimmed.len() >= open.len() + close.len()
                {
                    let inner_raw = &trimmed[open.len()..trimmed.len() - close.len()];
                    let unwrapped = inner_raw
                        .strip_prefix(' ')
                        .unwrap_or(inner_raw)
                        .strip_suffix(' ')
                        .unwrap_or_else(|| inner_raw.strip_prefix(' ').unwrap_or(inner_raw));
                    edits.push((s, e, unwrapped.to_string()));
                } else {
                    edits.push((s, e, format!("{open} {selected} {close}")));
                }
            } else if let Some(line_str) = self.buffer.line(s.line) {
                let trimmed = line_str.trim();
                let indent = line_str
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .count();
                let line_len = line_str.chars().count();
                if trimmed.is_empty() {
                    edits.push((s, s, format!("{open}  {close}")));
                    final_caret = Some(Position::new(s.line, s.col + open_chars + 1));
                } else if trimmed.starts_with(&open)
                    && trimmed.ends_with(&close)
                    && trimmed.len() >= open.len() + close.len()
                {
                    let inner_raw = &trimmed[open.len()..trimmed.len() - close.len()];
                    let unwrapped = inner_raw
                        .strip_prefix(' ')
                        .unwrap_or(inner_raw)
                        .strip_suffix(' ')
                        .unwrap_or_else(|| inner_raw.strip_prefix(' ').unwrap_or(inner_raw));
                    edits.push((
                        Position::new(s.line, indent),
                        Position::new(s.line, line_len),
                        unwrapped.to_string(),
                    ));
                } else {
                    let _ = close_chars;
                    edits.push((
                        Position::new(s.line, indent),
                        Position::new(s.line, line_len),
                        format!("{open} {trimmed} {close}"),
                    ));
                }
            }
        }
        if !edits.is_empty() {
            self.replace_ranges_atomic(edits);
            if let Some(caret) = final_caret {
                self.cursors.set_single(caret);
            }
        }
    }

    /// Indents selected lines by one indent unit (`tab_size` spaces or `\t`),
    /// or inserts an indent unit at collapsed carets.
    pub fn indent_selection(&mut self) {
        if self.read_only {
            return;
        }
        let unit = self.indent_unit();
        let has_multiline = self
            .cursors
            .all()
            .iter()
            .any(|s| s.anchor.line != s.focus.line);
        if !has_multiline && self.cursors.all().iter().all(|s| s.is_collapsed()) {
            self.insert_text(&unit);
            return;
        }

        let mut lines = Vec::new();
        for sel in self.cursors.all() {
            let (s, e) = sel.ordered_range();
            let last_line = if e.line > s.line && e.col == 0 {
                e.line - 1
            } else {
                e.line
            };
            for line in s.line..=last_line {
                if !lines.contains(&line) {
                    lines.push(line);
                }
            }
        }
        let edits: Vec<(Position, Position, String)> = lines
            .into_iter()
            .map(|l| (Position::new(l, 0), Position::new(l, 0), unit.clone()))
            .collect();
        self.replace_ranges_atomic(edits);
    }

    /// Outdents selected lines by removing up to `tab_size` leading spaces (or one leading `\t`).
    pub fn outdent_selection(&mut self) {
        if self.read_only {
            return;
        }
        let mut lines = Vec::new();
        for sel in self.cursors.all() {
            let (s, e) = sel.ordered_range();
            let last_line = if e.line > s.line && e.col == 0 {
                e.line - 1
            } else {
                e.line
            };
            for line in s.line..=last_line {
                if !lines.contains(&line) {
                    lines.push(line);
                }
            }
        }
        let mut edits = Vec::new();
        for line in lines {
            let Some(txt) = self.buffer.line(line) else {
                continue;
            };
            if txt.starts_with('\t') {
                edits.push((
                    Position::new(line, 0),
                    Position::new(line, 1),
                    String::new(),
                ));
            } else {
                let leading_spaces = txt.chars().take_while(|&c| c == ' ').count();
                let remove = leading_spaces.min(self.tab_size.max(1));
                if remove > 0 {
                    edits.push((
                        Position::new(line, 0),
                        Position::new(line, remove),
                        String::new(),
                    ));
                }
            }
        }
        self.replace_ranges_atomic(edits);
    }

    /// Smart `Home`: toggles each cursor between the first non-whitespace column on its line
    /// and column 0 (standard VS Code / IDE behavior).
    pub fn move_to_line_start_smart(&mut self, select: bool) {
        let before = self.cursors.all().to_vec();
        let mut next = Vec::with_capacity(before.len());
        for sel in &before {
            let line = sel.focus.line;
            let first_non_ws = self
                .buffer
                .line(line)
                .map(|l| l.chars().take_while(|c| *c == ' ' || *c == '\t').count())
                .unwrap_or(0);
            let target_col = if sel.focus.col == first_non_ws {
                0
            } else {
                first_non_ws
            };
            let new_focus = Position::new(line, target_col);
            let anchor = if select { sel.anchor } else { new_focus };
            next.push(Selection::range(anchor, new_focus));
        }
        self.cursors = MultiCursor::from_selections(next);
    }

    /// Moves selected lines (or current line) up by one line (`Alt+Up`).
    pub fn move_line_up(&mut self) {
        if self.read_only {
            return;
        }
        let sel = *self.cursors.primary();
        let (s, e) = sel.ordered_range();
        let first_line = s.line;
        let last_line = if e.line > s.line && e.col == 0 {
            e.line - 1
        } else {
            e.line
        };
        if first_line == 0 {
            return;
        }
        let above = self
            .buffer
            .line(first_line - 1)
            .unwrap_or("")
            .to_string();
        let mut block = Vec::with_capacity(last_line - first_line + 1);
        for l in first_line..=last_line {
            block.push(self.buffer.line(l).unwrap_or("").to_string());
        }
        block.push(above);
        let replacement = block.join("\n");
        let end_col = self.buffer.line_len(last_line);
        self.replace_range(
            Position::new(first_line - 1, 0),
            Position::new(last_line, end_col),
            &replacement,
        );
        self.cursors.set_single(Position::new(
            sel.focus.line.saturating_sub(1),
            sel.focus.col,
        ));
        if !sel.is_collapsed() {
            self.cursors.all_mut()[0] = Selection::range(
                Position::new(sel.anchor.line.saturating_sub(1), sel.anchor.col),
                Position::new(sel.focus.line.saturating_sub(1), sel.focus.col),
            );
        }
        self.ensure_cursor_visible();
    }

    /// Moves selected lines (or current line) down by one line (`Alt+Down`).
    pub fn move_line_down(&mut self) {
        if self.read_only {
            return;
        }
        let sel = *self.cursors.primary();
        let (s, e) = sel.ordered_range();
        let first_line = s.line;
        let last_line = if e.line > s.line && e.col == 0 {
            e.line - 1
        } else {
            e.line
        };
        if last_line + 1 >= self.buffer.line_count() {
            return;
        }
        let below = self
            .buffer
            .line(last_line + 1)
            .unwrap_or("")
            .to_string();
        let mut block = Vec::with_capacity(last_line - first_line + 2);
        block.push(below);
        for l in first_line..=last_line {
            block.push(self.buffer.line(l).unwrap_or("").to_string());
        }
        let replacement = block.join("\n");
        let end_col = self.buffer.line_len(last_line + 1);
        self.replace_range(
            Position::new(first_line, 0),
            Position::new(last_line + 1, end_col),
            &replacement,
        );
        self.cursors
            .set_single(Position::new(sel.focus.line + 1, sel.focus.col));
        if !sel.is_collapsed() {
            self.cursors.all_mut()[0] = Selection::range(
                Position::new(sel.anchor.line + 1, sel.anchor.col),
                Position::new(sel.focus.line + 1, sel.focus.col),
            );
        }
        self.ensure_cursor_visible();
    }

    /// Copies selected lines (or current line) above (`Ctrl+Shift+Alt+Up`).
    pub fn copy_line_up(&mut self) {
        if self.read_only {
            return;
        }
        let sel = *self.cursors.primary();
        let (s, e) = sel.ordered_range();
        let first_line = s.line;
        let last_line = if e.line > s.line && e.col == 0 {
            e.line - 1
        } else {
            e.line
        };
        let mut block = Vec::with_capacity(last_line - first_line + 1);
        for l in first_line..=last_line {
            block.push(self.buffer.line(l).unwrap_or("").to_string());
        }
        let insert = format!("{}\n", block.join("\n"));
        self.replace_range(
            Position::new(first_line, 0),
            Position::new(first_line, 0),
            &insert,
        );
        self.cursors.set_single(sel.focus);
        if !sel.is_collapsed() {
            self.cursors.all_mut()[0] = sel;
        }
        self.ensure_cursor_visible();
    }

    /// Copies selected lines (or current line) below (`Ctrl+Shift+Alt+Down`).
    pub fn copy_line_down(&mut self) {
        if self.read_only {
            return;
        }
        let sel = *self.cursors.primary();
        let (s, e) = sel.ordered_range();
        let first_line = s.line;
        let last_line = if e.line > s.line && e.col == 0 {
            e.line - 1
        } else {
            e.line
        };
        let span = last_line - first_line + 1;
        let mut block = Vec::with_capacity(span);
        for l in first_line..=last_line {
            block.push(self.buffer.line(l).unwrap_or("").to_string());
        }
        let end_col = self.buffer.line_len(last_line);
        let insert = format!("\n{}", block.join("\n"));
        self.replace_range(
            Position::new(last_line, end_col),
            Position::new(last_line, end_col),
            &insert,
        );
        self.cursors
            .set_single(Position::new(sel.focus.line + span, sel.focus.col));
        if !sel.is_collapsed() {
            self.cursors.all_mut()[0] = Selection::range(
                Position::new(sel.anchor.line + span, sel.anchor.col),
                Position::new(sel.focus.line + span, sel.focus.col),
            );
        }
        self.ensure_cursor_visible();
    }

    /// Duplicates the active selection(s) inline, or duplicates the current line if collapsed.
    pub fn duplicate_selection(&mut self) {
        if self.read_only {
            return;
        }
        if self.cursors.all().iter().all(|s| s.is_collapsed()) {
            self.copy_line_down();
            return;
        }
        let mut edits = Vec::new();
        for &sel in self.cursors.all() {
            if !sel.is_collapsed() {
                let (_, end) = sel.ordered_range();
                let text =
                    crate::clipboard::ClipboardBridge::from_selection(&self.buffer, sel).plain_text;
                edits.push((end, end, text));
            }
        }
        self.replace_ranges_atomic(edits);
        self.ensure_cursor_visible();
    }

    /// Deletes the line(s) touched by active cursors (`Ctrl+Shift+K`).
    pub fn delete_line(&mut self) {
        if self.read_only {
            return;
        }
        let mut lines = Vec::new();
        for sel in self.cursors.all() {
            let (s, e) = sel.ordered_range();
            let last_line = if e.line > s.line && e.col == 0 {
                e.line - 1
            } else {
                e.line
            };
            for line in s.line..=last_line {
                if !lines.contains(&line) {
                    lines.push(line);
                }
            }
        }
        if lines.is_empty() {
            return;
        }
        lines.sort_unstable();
        let line_count = self.buffer.line_count();
        let first = lines[0];
        let last = *lines.last().unwrap_or(&first);
        let focus_col = self.cursors.primary().focus.col;

        if last + 1 < line_count {
            self.replace_range(Position::new(first, 0), Position::new(last + 1, 0), "");
            let target_line = first.min(self.buffer.line_count().saturating_sub(1));
            self.cursors.set_single(Position::new(
                target_line,
                focus_col.min(self.buffer.line_len(target_line)),
            ));
        } else if first > 0 {
            let prev_len = self.buffer.line_len(first - 1);
            let last_len = self.buffer.line_len(last);
            self.replace_range(
                Position::new(first - 1, prev_len),
                Position::new(last, last_len),
                "",
            );
            let target_line = (first - 1).min(self.buffer.line_count().saturating_sub(1));
            self.cursors.set_single(Position::new(
                target_line,
                focus_col.min(self.buffer.line_len(target_line)),
            ));
        } else {
            let last_len = self.buffer.line_len(last);
            self.replace_range(Position::new(0, 0), Position::new(last, last_len), "");
            self.cursors.set_single(Position::ZERO);
        }
        self.ensure_cursor_visible();
    }

    /// Inserts a blank indented line below the current line (`Ctrl+Enter`).
    pub fn insert_line_below(&mut self) {
        if self.read_only {
            return;
        }
        let line = self.cursors.primary().focus.line;
        let len = self.buffer.line_len(line);
        self.cursors.set_single(Position::new(line, len));
        self.insert_newline();
        self.ensure_cursor_visible();
    }

    /// Inserts a blank indented line above the current line (`Ctrl+Shift+Enter`).
    pub fn insert_line_above(&mut self) {
        if self.read_only {
            return;
        }
        let line = self.cursors.primary().focus.line;
        let indent: String = self
            .buffer
            .line(line)
            .unwrap_or("")
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let insert_col = indent.chars().count();
        self.replace_range(
            Position::new(line, 0),
            Position::new(line, 0),
            &format!("{indent}\n"),
        );
        self.cursors.set_single(Position::new(line, insert_col));
        self.ensure_cursor_visible();
    }

    /// Adds a cursor on the line above the topmost active cursor (`Shift+Alt+Up` / `Ctrl+Alt+Up`).
    pub fn add_cursor_above(&mut self) {
        let mut additions = Vec::new();
        for sel in self.cursors.all() {
            if sel.focus.line > 0 {
                let target_line = sel.focus.line - 1;
                let col = sel.focus.col.min(self.buffer.line_len(target_line));
                additions.push(Position::new(target_line, col));
            }
        }
        for pos in additions {
            self.cursors.add_cursor(pos);
        }
        self.ensure_cursor_visible();
    }

    /// Adds a cursor on the line below the bottommost active cursor (`Shift+Alt+Down` / `Ctrl+Alt+Down`).
    pub fn add_cursor_below(&mut self) {
        let max_line = self.buffer.line_count().saturating_sub(1);
        let mut additions = Vec::new();
        for sel in self.cursors.all() {
            if sel.focus.line < max_line {
                let target_line = sel.focus.line + 1;
                let col = sel.focus.col.min(self.buffer.line_len(target_line));
                additions.push(Position::new(target_line, col));
            }
        }
        for pos in additions {
            self.cursors.add_cursor(pos);
        }
        self.ensure_cursor_visible();
    }

    /// Places a collapsed cursor at the end of every line spanned by active selections (`Shift+Alt+I`).
    pub fn add_cursors_to_line_ends(&mut self) {
        let mut lines = Vec::new();
        for sel in self.cursors.all() {
            let (s, e) = sel.ordered_range();
            let last_line = if e.line > s.line && e.col == 0 {
                e.line - 1
            } else {
                e.line
            };
            for line in s.line..=last_line {
                if !lines.contains(&line) {
                    lines.push(line);
                }
            }
        }
        if lines.is_empty() {
            return;
        }
        let sels: Vec<Selection> = lines
            .into_iter()
            .map(|l| Selection::point(Position::new(l, self.buffer.line_len(l))))
            .collect();
        self.cursors = MultiCursor::from_selections(sels);
    }

    /// Selects the word under cursor if collapsed, or adds a cursor on the next matching occurrence (`Ctrl+D`).
    pub fn add_next_occurrence(&mut self) -> bool {
        let primary = *self.cursors.primary();
        if primary.is_collapsed() {
            let (ws, we) = self.word_bounds_at(primary.focus);
            if ws != we {
                self.cursors.set_single(ws);
                self.cursors.all_mut()[0] = Selection::range(ws, we);
                return true;
            }
            return false;
        }
        let needle =
            crate::clipboard::ClipboardBridge::from_selection(&self.buffer, primary).plain_text;
        if needle.is_empty() || needle.contains('\n') {
            return false;
        }
        let needle_chars = needle.chars().count();
        let last_end = self
            .cursors
            .all()
            .iter()
            .map(|s| s.ordered_range().1)
            .max()
            .unwrap_or(Position::ZERO);

        let all_matches = self.find_literal_occurrences(&needle, needle_chars);
        let existing: Vec<(Position, Position)> = self
            .cursors
            .all()
            .iter()
            .map(|s| s.ordered_range())
            .collect();

        // First look after `last_end`, then wrap around from the start of the file.
        let next = all_matches
            .iter()
            .find(|&&(s, e)| s >= last_end && !existing.contains(&(s, e)))
            .or_else(|| {
                all_matches
                    .iter()
                    .find(|&&(s, e)| !existing.contains(&(s, e)))
            })
            .copied();

        if let Some((s, e)) = next {
            self.cursors.add_selection(Selection::range(s, e));
            self.ensure_line_visible(s.line);
            true
        } else {
            false
        }
    }

    /// Adds a cursor on the previous matching occurrence of the selected text (or current word).
    pub fn add_previous_occurrence(&mut self) -> bool {
        let primary = *self.cursors.primary();
        if primary.is_collapsed() {
            let (ws, we) = self.word_bounds_at(primary.focus);
            if ws != we {
                self.cursors.set_single(ws);
                self.cursors.all_mut()[0] = Selection::range(ws, we);
                return true;
            }
            return false;
        }
        let needle =
            crate::clipboard::ClipboardBridge::from_selection(&self.buffer, primary).plain_text;
        if needle.is_empty() || needle.contains('\n') {
            return false;
        }
        let needle_chars = needle.chars().count();
        let first_start = self
            .cursors
            .all()
            .iter()
            .map(|s| s.ordered_range().0)
            .min()
            .unwrap_or(Position::ZERO);

        let all_matches = self.find_literal_occurrences(&needle, needle_chars);
        let existing: Vec<(Position, Position)> = self
            .cursors
            .all()
            .iter()
            .map(|s| s.ordered_range())
            .collect();

        let prev = all_matches
            .iter()
            .rev()
            .find(|&&(s, e)| e <= first_start && !existing.contains(&(s, e)))
            .or_else(|| {
                all_matches
                    .iter()
                    .rev()
                    .find(|&&(s, e)| !existing.contains(&(s, e)))
            })
            .copied();

        if let Some((s, e)) = prev {
            self.cursors.add_selection(Selection::range(s, e));
            self.ensure_line_visible(s.line);
            true
        } else {
            false
        }
    }

    /// Selects every occurrence of the currently selected text (or word under caret) (`Ctrl+Shift+L` / `Ctrl+F2`).
    pub fn select_all_occurrences(&mut self) -> usize {
        let primary = *self.cursors.primary();
        let needle = if primary.is_collapsed() {
            let (ws, we) = self.word_bounds_at(primary.focus);
            if ws == we {
                return 0;
            }
            crate::clipboard::ClipboardBridge::from_selection(
                &self.buffer,
                Selection::range(ws, we),
            )
            .plain_text
        } else {
            crate::clipboard::ClipboardBridge::from_selection(&self.buffer, primary).plain_text
        };
        if needle.is_empty() || needle.contains('\n') {
            return 0;
        }
        let needle_chars = needle.chars().count();
        let matches = self.find_literal_occurrences(&needle, needle_chars);
        if matches.is_empty() {
            return 0;
        }
        let count = matches.len();
        let sels: Vec<Selection> = matches
            .into_iter()
            .map(|(s, e)| Selection::range(s, e))
            .collect();
        self.cursors = MultiCursor::from_selections(sels);
        count
    }

    fn find_literal_occurrences(
        &self,
        needle: &str,
        needle_chars: usize,
    ) -> Vec<(Position, Position)> {
        let mut out = Vec::new();
        for line_idx in 0..self.buffer.line_count() {
            let Some(line) = self.buffer.line(line_idx) else {
                continue;
            };
            let mut search_from = 0usize;
            while let Some(rel) = line[search_from..].find(needle) {
                let start_byte = search_from + rel;
                let start_col = line[..start_byte].chars().count();
                out.push((
                    Position::new(line_idx, start_col),
                    Position::new(line_idx, start_col + needle_chars),
                ));
                search_from = start_byte + needle.len().max(1);
                if search_from >= line.len() {
                    break;
                }
            }
        }
        out
    }

    /// Selects the current line, or extends the selection down by one more line on repeated `Ctrl+L`.
    pub fn select_current_line_extend(&mut self) {
        let sel = *self.cursors.primary();
        let (s, e) = sel.ordered_range();
        let line_count = self.buffer.line_count();
        let start = Position::new(s.line, 0);
        let target_line = if sel.is_collapsed() {
            s.line
        } else if e.col == 0 && e.line > s.line {
            e.line.min(line_count.saturating_sub(1))
        } else {
            e.line
        };
        let end = if target_line + 1 < line_count {
            Position::new(target_line + 1, 0)
        } else {
            Position::new(target_line, self.buffer.line_len(target_line))
        };
        self.cursors.set_single(start);
        self.cursors.all_mut()[0] = Selection::range(start, end);
        self.ensure_cursor_visible();
    }

    /// Smart selection expansion (`Shift+Alt+Right`):
    /// caret -> word -> enclosing quote/bracket interior -> enclosing brackets -> full line -> full document.
    pub fn expand_selection(&mut self) {
        let current = self.cursors.all().to_vec();
        let sel = *self.cursors.primary();
        let (s, e) = sel.ordered_range();
        let total_lines = self.buffer.line_count();
        let doc_end = Position::new(
            total_lines.saturating_sub(1),
            self.buffer.line_len(total_lines.saturating_sub(1)),
        );

        let next_range: Option<(Position, Position)> = if sel.is_collapsed() {
            let (ws, we) = self.word_bounds_at(s);
            if ws != we {
                Some((ws, we))
            } else {
                Some((
                    Position::new(s.line, 0),
                    Position::new(s.line, self.buffer.line_len(s.line)),
                ))
            }
        } else {
            // Try enclosing brackets/quotes on the same line, then full line, then whole document.
            let mut candidate = None;
            if s.line == e.line {
                if let Some(line_str) = self.buffer.line(s.line) {
                    let chars: Vec<char> = line_str.chars().collect();
                    let pairs = [('(', ')'), ('[', ']'), ('{', '}'), ('"', '"'), ('\'', '\'')];
                    'outer: for &(open, close) in &pairs {
                        let mut left = s.col;
                        while left > 0 {
                            left -= 1;
                            if chars[left] == open {
                                if let Some(rel_r) =
                                    chars[e.col..].iter().position(|&c| c == close)
                                {
                                    let right = e.col + rel_r;
                                    let inner =
                                        (Position::new(s.line, left + 1), Position::new(s.line, right));
                                    let outer = (
                                        Position::new(s.line, left),
                                        Position::new(s.line, right + 1),
                                    );
                                    if (s, e) != inner && inner.0 < inner.1 {
                                        candidate = Some(inner);
                                    } else if (s, e) != outer {
                                        candidate = Some(outer);
                                    }
                                    if candidate.is_some() {
                                        break 'outer;
                                    }
                                }
                            }
                        }
                    }
                    if candidate.is_none() {
                        let line_start = Position::new(s.line, 0);
                        let line_end = Position::new(s.line, chars.len());
                        if (s, e) != (line_start, line_end) {
                            candidate = Some((line_start, line_end));
                        }
                    }
                }
            }
            candidate.or_else(|| {
                if (s, e) != (Position::ZERO, doc_end) {
                    Some((Position::ZERO, doc_end))
                } else {
                    None
                }
            })
        };

        if let Some((ns, ne)) = next_range {
            if (ns, ne) != (s, e) {
                self.selection_expansion_stack.push(current);
                self.cursors.set_single(ns);
                self.cursors.all_mut()[0] = Selection::range(ns, ne);
            }
        }
    }

    /// Reverses the last `expand_selection` step (`Shift+Alt+Left`).
    pub fn shrink_selection(&mut self) {
        if let Some(prev) = self.selection_expansion_stack.pop() {
            self.cursors = MultiCursor::from_selections(prev);
        } else {
            let focus = self.cursors.primary().focus;
            self.cursors.set_single(focus);
        }
    }

    /// Deletes one word to the left of every cursor (`Ctrl+Backspace`).
    pub fn delete_word_left(&mut self) {
        if self.read_only {
            return;
        }
        if self.cursors.all().iter().any(|s| !s.is_collapsed()) {
            self.delete_backwards();
            return;
        }
        self.move_word_left(true);
        self.delete_backwards();
    }

    /// Deletes one word to the right of every cursor (`Ctrl+Delete`).
    pub fn delete_word_right(&mut self) {
        if self.read_only {
            return;
        }
        if self.cursors.all().iter().any(|s| !s.is_collapsed()) {
            self.delete_forward();
            return;
        }
        self.move_word_right(true);
        self.delete_forward();
    }

    /// Jumps the primary cursor to its matching bracket (`Ctrl+Shift+\`).
    pub fn goto_matching_bracket(&mut self) -> bool {
        let pos = self.cursors.primary().focus;
        let mate = self.matching_bracket(pos).or_else(|| {
            if pos.col > 0 {
                self.matching_bracket(Position::new(pos.line, pos.col - 1))
            } else {
                None
            }
        });
        if let Some(target) = mate {
            self.cursors.set_single(target);
            self.ensure_cursor_visible();
            true
        } else {
            false
        }
    }

    /// Removes trailing spaces and tabs from every line in the buffer as one undo transaction.
    pub fn trim_trailing_whitespace(&mut self) -> usize {
        if self.read_only {
            return 0;
        }
        let mut edits = Vec::new();
        for line_idx in 0..self.buffer.line_count() {
            if let Some(line) = self.buffer.line(line_idx) {
                let len = line.chars().count();
                let trimmed = line.trim_end_matches([' ', '\t']).chars().count();
                if trimmed < len {
                    edits.push((
                        Position::new(line_idx, trimmed),
                        Position::new(line_idx, len),
                        String::new(),
                    ));
                }
            }
        }
        let count = edits.len();
        if !edits.is_empty() {
            self.replace_ranges_atomic(edits);
        }
        count
    }

    /// Converts leading indentation across the whole document between Spaces and Tabs.
    pub fn convert_indentation(&mut self, to_spaces: bool) -> usize {
        if self.read_only {
            return 0;
        }
        self.insert_spaces = to_spaces;
        let tab_sz = self.tab_size.max(1);
        let mut edits = Vec::new();
        for line_idx in 0..self.buffer.line_count() {
            let Some(line) = self.buffer.line(line_idx) else {
                continue;
            };
            let leading: String = line
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            if leading.is_empty() {
                continue;
            }
            let leading_chars = leading.chars().count();
            let visual_spaces: usize = leading
                .chars()
                .map(|c| if c == '\t' { tab_sz } else { 1 })
                .sum();
            let replacement = if to_spaces {
                " ".repeat(visual_spaces)
            } else {
                let tabs = visual_spaces / tab_sz;
                let rem = visual_spaces % tab_sz;
                format!("{}{}", "\t".repeat(tabs), " ".repeat(rem))
            };
            if replacement != leading {
                edits.push((
                    Position::new(line_idx, 0),
                    Position::new(line_idx, leading_chars),
                    replacement,
                ));
            }
        }
        let count = edits.len();
        if !edits.is_empty() {
            self.replace_ranges_atomic(edits);
        }
        count
    }

    /// Expands a basic Emmet abbreviation immediately before the primary cursor
    /// (e.g. `div.card#hero`, `ul>li*3`, `span.badge`). Returns `true` if expanded.
    pub fn expand_emmet_abbreviation(&mut self) -> bool {
        if self.read_only {
            return false;
        }
        let focus = self.cursors.primary().focus;
        let Some(line) = self.buffer.line(focus.line) else {
            return false;
        };
        let chars: Vec<char> = line.chars().collect();
        let end = focus.col.min(chars.len());
        let mut start = end;
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        if start == end {
            return false;
        }
        let abbr: String = chars[start..end].iter().collect();
        let Some(expanded) = expand_emmet_expr(&abbr) else {
            return false;
        };
        let new_end = self.replace_range(
            Position::new(focus.line, start),
            Position::new(focus.line, end),
            &expanded,
        );
        self.cursors.set_single(new_end);
        self.ensure_cursor_visible();
        true
    }

    /// Moves all cursors left by one word boundary.
    pub fn move_word_left(&mut self, select: bool) {
        let before = self.cursors.all().to_vec();
        let mut next = Vec::with_capacity(before.len());
        for sel in &before {
            let focus = sel.focus;
            let new_focus = if focus.col == 0 {
                if focus.line > 0 {
                    let prev = focus.line - 1;
                    Position::new(prev, self.buffer.line_len(prev))
                } else {
                    Position::ZERO
                }
            } else if let Some(line_str) = self.buffer.line(focus.line) {
                let chars: Vec<char> = line_str.chars().collect();
                let mut col = focus.col.min(chars.len());
                while col > 0 && chars[col - 1].is_whitespace() {
                    col -= 1;
                }
                if col > 0 {
                    let is_word = is_word_char(chars[col - 1]);
                    while col > 0
                        && is_word_char(chars[col - 1]) == is_word
                        && !chars[col - 1].is_whitespace()
                    {
                        col -= 1;
                    }
                }
                Position::new(focus.line, col)
            } else {
                Position::new(focus.line, 0)
            };
            let anchor = if select { sel.anchor } else { new_focus };
            next.push(Selection::range(anchor, new_focus));
        }
        self.cursors = MultiCursor::from_selections(next);
    }

    /// Moves all cursors right by one word boundary.
    pub fn move_word_right(&mut self, select: bool) {
        let before = self.cursors.all().to_vec();
        let mut next = Vec::with_capacity(before.len());
        for sel in &before {
            let focus = sel.focus;
            let cur_len = self.buffer.line_len(focus.line);
            let new_focus = if focus.col >= cur_len {
                if focus.line + 1 < self.buffer.line_count() {
                    Position::new(focus.line + 1, 0)
                } else {
                    Position::new(focus.line, cur_len)
                }
            } else if let Some(line_str) = self.buffer.line(focus.line) {
                let chars: Vec<char> = line_str.chars().collect();
                let mut col = focus.col.min(chars.len());
                while col < chars.len() && chars[col].is_whitespace() {
                    col += 1;
                }
                if col < chars.len() {
                    let is_word = is_word_char(chars[col]);
                    while col < chars.len()
                        && is_word_char(chars[col]) == is_word
                        && !chars[col].is_whitespace()
                    {
                        col += 1;
                    }
                }
                Position::new(focus.line, col)
            } else {
                Position::new(focus.line, cur_len)
            };
            let anchor = if select { sel.anchor } else { new_focus };
            next.push(Selection::range(anchor, new_focus));
        }
        self.cursors = MultiCursor::from_selections(next);
    }

    /// Moves cursors up by one viewport page.
    pub fn page_up(&mut self, select: bool) {
        let step = ((self.viewport_height / self.line_height.max(1.0)).floor() as usize).max(1);
        let before = self.cursors.all().to_vec();
        let mut next = Vec::with_capacity(before.len());
        for sel in &before {
            let target_line = sel.focus.line.saturating_sub(step);
            let target_col = sel.focus.col.min(self.buffer.line_len(target_line));
            let new_focus = Position::new(target_line, target_col);
            let anchor = if select { sel.anchor } else { new_focus };
            next.push(Selection::range(anchor, new_focus));
        }
        self.cursors = MultiCursor::from_selections(next);
        self.ensure_cursor_visible();
    }

    /// Moves cursors down by one viewport page.
    pub fn page_down(&mut self, select: bool) {
        let step = ((self.viewport_height / self.line_height.max(1.0)).floor() as usize).max(1);
        let max_line = self.buffer.line_count().saturating_sub(1);
        let before = self.cursors.all().to_vec();
        let mut next = Vec::with_capacity(before.len());
        for sel in &before {
            let target_line = (sel.focus.line + step).min(max_line);
            let target_col = sel.focus.col.min(self.buffer.line_len(target_line));
            let new_focus = Position::new(target_line, target_col);
            let anchor = if select { sel.anchor } else { new_focus };
            next.push(Selection::range(anchor, new_focus));
        }
        self.cursors = MultiCursor::from_selections(next);
        self.ensure_cursor_visible();
    }

    /// Returns the `(start, end)` word/token boundaries surrounding `pos`.
    pub fn word_bounds_at(&self, pos: Position) -> (Position, Position) {
        let line = pos.line.min(self.buffer.line_count().saturating_sub(1));
        let Some(line_str) = self.buffer.line(line) else {
            return (Position::new(line, 0), Position::new(line, 0));
        };
        let chars: Vec<char> = line_str.chars().collect();
        if chars.is_empty() {
            return (Position::new(line, 0), Position::new(line, 0));
        }
        let idx = pos.col.min(chars.len().saturating_sub(1));
        if chars[idx].is_whitespace() {
            let mut start = idx;
            while start > 0 && chars[start - 1].is_whitespace() {
                start -= 1;
            }
            let mut end = idx + 1;
            while end < chars.len() && chars[end].is_whitespace() {
                end += 1;
            }
            return (Position::new(line, start), Position::new(line, end));
        }
        let target_is_word = is_word_char(chars[idx]);
        let mut start = idx;
        while start > 0
            && is_word_char(chars[start - 1]) == target_is_word
            && !chars[start - 1].is_whitespace()
        {
            start -= 1;
        }
        let mut end = idx + 1;
        while end < chars.len()
            && is_word_char(chars[end]) == target_is_word
            && !chars[end].is_whitespace()
        {
            end += 1;
        }
        (Position::new(line, start), Position::new(line, end))
    }

    /// Selects the identifier/word surrounding `pos` (used on double-click).
    pub fn select_word_at(&mut self, pos: Position) {
        let (start, end) = self.word_bounds_at(pos);
        self.cursors.set_single(start);
        self.cursors.all_mut()[0] = Selection::range(start, end);
    }

    /// Selects the entire logical line (used on triple-click).
    pub fn select_line(&mut self, line: usize) {
        let clamped = line.min(self.buffer.line_count().saturating_sub(1));
        let len = self.buffer.line_len(clamped);
        self.cursors.set_single(Position::new(clamped, 0));
        self.cursors.all_mut()[0] =
            Selection::range(Position::new(clamped, 0), Position::new(clamped, len));
    }

    /// Scrolls the viewport vertically by `delta_y` pixels.
    pub fn scroll_by(&mut self, delta_y: f32) {
        self.set_scroll_top(self.scroll_top + delta_y);
    }

    /// Scrolls so that `logical_line` is positioned near the top of the viewport.
    pub fn scroll_to_line(&mut self, logical_line: usize) {
        if let Some(display_row) = self.logical_to_display_row(logical_line) {
            self.set_scroll_top(display_row as f32 * self.line_height);
        }
    }

    /// Adjusts `scroll_top` minimally so `logical_line` sits inside the visible viewport.
    pub fn ensure_line_visible(&mut self, logical_line: usize) {
        let Some(display_row) = self.logical_to_display_row(logical_line) else {
            return;
        };
        let row_top = display_row as f32 * self.line_height;
        let row_bottom = row_top + self.line_height;
        if row_top < self.scroll_top {
            self.set_scroll_top(row_top);
        } else if row_bottom > self.scroll_top + self.viewport_height {
            self.set_scroll_top(row_bottom - self.viewport_height);
        }
    }

    /// Scrolls minimally to keep the primary caret visible.
    pub fn ensure_cursor_visible(&mut self) {
        let line = self.cursors.primary().focus.line;
        self.ensure_line_visible(line);
    }

    /// Maps an editor-local pointer `(rel_x, rel_y)` to a document [`Position`].
    pub fn position_from_point(&self, rel_x: f32, rel_y: f32, gutter_width: f32) -> Position {
        let display_row =
            ((rel_y + self.scroll_top).max(0.0) / self.line_height.max(1.0)).floor() as usize;
        let visible = self.visible_logical_lines();
        let line = if visible.is_empty() {
            0
        } else {
            visible[display_row.min(visible.len() - 1)]
        };
        let code_x = (rel_x - gutter_width + self.scroll_left).max(0.0);
        let raw_col = (code_x / self.char_width.max(1.0)).round() as usize;
        let max_col = self.buffer.line_len(line);
        Position::new(line, raw_col.min(max_col))
    }

    /// Full pointer press handler supporting single/double/triple-click streaks, Double-Click + Drag,
    /// Triple-Click + Drag, configurable `Alt+Click` / `Ctrl+Click` multi-cursor add & toggle-remove,
    /// `Shift+Alt+Drag` rectangular Column Box selection, and right-click cursor placement.
    #[allow(clippy::too_many_arguments)]
    pub fn handle_pointer_down_ex(
        &mut self,
        rel_x: f32,
        rel_y: f32,
        detail: i32,
        alt_key: bool,
        shift_key: bool,
        ctrl_or_meta: bool,
        button: i16,
        gutter_width: f32,
    ) {
        let pos = self.position_from_point(rel_x, rel_y, gutter_width);

        // Right-click moves caret only if clicking outside existing selections; never starts drag.
        if button == 2 {
            self.is_pointer_dragging = false;
            let inside_existing = self
                .cursors
                .all()
                .iter()
                .any(|s| !s.is_collapsed() && s.contains(pos));
            if !inside_existing {
                self.cursors.set_single(pos);
            }
            return;
        }

        // Defense-in-depth multi-click streak tracking when host supplies detail <= 1.
        let now = std::time::Instant::now();
        let effective_detail = if detail >= 2 {
            self.last_pointer_click = Some((now, rel_x, rel_y, detail));
            detail
        } else {
            let streak = match self.last_pointer_click {
                Some((prev_t, prev_x, prev_y, prev_streak))
                    if now.duration_since(prev_t).as_millis() <= 450
                        && (rel_x - prev_x).abs() <= 6.0
                        && (rel_y - prev_y).abs() <= 6.0 =>
                {
                    if prev_streak >= 3 {
                        1
                    } else {
                        prev_streak + 1
                    }
                }
                _ => 1,
            };
            self.last_pointer_click = Some((now, rel_x, rel_y, streak));
            streak
        };

        let is_multi_cursor_click = match self.multi_cursor_modifier {
            MultiCursorModifier::Alt => alt_key && !shift_key && !ctrl_or_meta,
            MultiCursorModifier::CtrlCmd => ctrl_or_meta && !shift_key && !alt_key,
        };
        let is_column_drag =
            (alt_key && shift_key) || (self.column_selection_mode && !is_multi_cursor_click);

        self.selection_expansion_stack.clear();

        if effective_detail >= 3 {
            self.is_pointer_dragging = true;
            self.drag_mode = PointerDragMode::Line {
                origin_line: pos.line,
            };
            self.select_line(pos.line);
        } else if effective_detail == 2 {
            self.is_pointer_dragging = true;
            let (ws, we) = self.word_bounds_at(pos);
            self.drag_mode = PointerDragMode::Word {
                origin_start: ws,
                origin_end: we,
            };
            self.cursors.set_single(ws);
            self.cursors.all_mut()[0] = Selection::range(ws, we);
        } else if is_column_drag {
            self.is_pointer_dragging = true;
            self.drag_mode = PointerDragMode::Column { origin: pos };
            self.cursors.set_single(pos);
        } else if is_multi_cursor_click {
            self.is_pointer_dragging = false;
            self.drag_mode = PointerDragMode::Char;
            // Toggle off an existing cursor at `pos` when multiple cursors exist; otherwise add one.
            if self.cursors.count() > 1 {
                if let Some(idx) = self
                    .cursors
                    .all()
                    .iter()
                    .position(|s| s.is_collapsed() && s.focus == pos)
                {
                    let mut remaining = self.cursors.all().to_vec();
                    remaining.remove(idx);
                    self.cursors = MultiCursor::from_selections(remaining);
                    return;
                }
            }
            self.cursors.add_cursor(pos);
        } else if shift_key {
            self.is_pointer_dragging = true;
            self.drag_mode = PointerDragMode::Char;
            let anchor = self.cursors.primary().anchor;
            self.cursors.set_single(anchor);
            self.cursors.all_mut()[0] = Selection::range(anchor, pos);
        } else {
            self.is_pointer_dragging = true;
            self.drag_mode = PointerDragMode::Char;
            self.cursors.set_single(pos);
        }
    }

    /// Backwards-compatible pointer press handler inside the code viewport (click, Shift+click,
    /// Alt+click multi-cursor, double-click word selection, triple-click line selection).
    pub fn handle_pointer_down(
        &mut self,
        rel_x: f32,
        rel_y: f32,
        detail: i32,
        alt_key: bool,
        shift_key: bool,
        gutter_width: f32,
    ) {
        self.handle_pointer_down_ex(
            rel_x,
            rel_y,
            detail,
            alt_key,
            shift_key,
            false,
            0,
            gutter_width,
        );
    }

    /// Extends the active selection while dragging the pointer according to `self.drag_mode`
    /// (`Char`, `Word`, `Line`, or rectangular `Column`), with viewport-edge auto-scrolling.
    pub fn handle_pointer_drag(&mut self, rel_x: f32, rel_y: f32, gutter_width: f32) {
        if !self.is_pointer_dragging {
            return;
        }
        // Auto-scroll smoothly when dragging above or below viewport bounds.
        if rel_y < 0.0 {
            self.scroll_by(-(-rel_y).min(self.line_height * 2.0));
        } else if rel_y > self.viewport_height {
            self.scroll_by((rel_y - self.viewport_height).min(self.line_height * 2.0));
        }

        let pos = self.position_from_point(rel_x, rel_y, gutter_width);
        match self.drag_mode {
            PointerDragMode::Char => {
                let anchor = self.cursors.primary().anchor;
                self.cursors.set_single(anchor);
                self.cursors.all_mut()[0] = Selection::range(anchor, pos);
            }
            PointerDragMode::Word {
                origin_start,
                origin_end,
            } => {
                let (ws, we) = self.word_bounds_at(pos);
                let sel = if pos < origin_start {
                    Selection::range(origin_end, ws)
                } else if pos > origin_end {
                    Selection::range(origin_start, we)
                } else {
                    Selection::range(origin_start, origin_end)
                };
                self.cursors.set_single(sel.anchor);
                self.cursors.all_mut()[0] = sel;
            }
            PointerDragMode::Line { origin_line } => {
                let origin_len = self.buffer.line_len(origin_line);
                let target_len = self.buffer.line_len(pos.line);
                let sel = if pos.line >= origin_line {
                    Selection::range(
                        Position::new(origin_line, 0),
                        Position::new(pos.line, target_len),
                    )
                } else {
                    Selection::range(
                        Position::new(origin_line, origin_len),
                        Position::new(pos.line, 0),
                    )
                };
                self.cursors.set_single(sel.anchor);
                self.cursors.all_mut()[0] = sel;
            }
            PointerDragMode::Column { origin } => {
                let code_x = (rel_x - gutter_width + self.scroll_left).max(0.0);
                let target_col = (code_x / self.char_width.max(1.0)).round() as usize;
                let min_line = origin.line.min(pos.line);
                let max_line = origin.line.max(pos.line);
                let mut sels = Vec::with_capacity(max_line - min_line + 1);
                for line in min_line..=max_line {
                    let len = self.buffer.line_len(line);
                    let c_anchor = origin.col.min(len);
                    let c_focus = target_col.min(len);
                    sels.push(Selection::range(
                        Position::new(line, c_anchor),
                        Position::new(line, c_focus),
                    ));
                }
                self.cursors = MultiCursor::from_selections(sels);
            }
        }
    }

    /// Ends a pointer drag gesture.
    pub fn handle_pointer_up(&mut self) {
        self.is_pointer_dragging = false;
    }

    /// Handles wheel scrolling over the editor viewport.
    pub fn handle_wheel(&mut self, delta_y: f32) {
        self.scroll_by(delta_y);
    }

    /// Handles click or drag inside the Canvas2D minimap to jump/scrub `scroll_top`.
    pub fn handle_minimap_pointer(
        &mut self,
        pointer_y: f32,
        minimap_height: f32,
        config: &MinimapConfig,
    ) {
        let target = minimap_y_to_scroll_top(
            pointer_y,
            minimap_height,
            self.visible_logical_lines().len().max(1),
            self.viewport_height,
            self.line_height,
            config,
        );
        self.set_scroll_top(target);
    }

    /// Copies the active selection(s) to Strata's unified clipboard and returns the copied string.
    /// When all cursors are collapsed, copies the primary cursor's full logical line (Monaco/VS Code parity).
    pub fn copy_selection(&self) -> Option<String> {
        let mut parts = Vec::new();
        for &sel in self.cursors.all() {
            if !sel.is_collapsed() {
                let payload = crate::clipboard::ClipboardBridge::from_selection(&self.buffer, sel);
                parts.push(payload.plain_text);
            }
        }
        let copied = if !parts.is_empty() {
            parts.join("\n")
        } else {
            let line = self.cursors.primary().focus.line;
            let text = self.buffer.line(line)?;
            format!("{text}\n")
        };
        strata_dom::clipboard::set_clipboard_text(&copied);
        Some(copied)
    }

    /// Cuts the active selection(s) (or primary logical line when collapsed) to Strata's unified clipboard.
    pub fn cut_selection(&mut self) -> Option<String> {
        if self.read_only {
            return None;
        }
        let copied = self.copy_selection()?;
        let has_selection = self.cursors.all().iter().any(|s| !s.is_collapsed());
        if has_selection {
            let edits: Vec<(Position, Position, String)> = self
                .cursors
                .all()
                .iter()
                .filter(|s| !s.is_collapsed())
                .map(|s| {
                    let (start, end) = s.ordered_range();
                    (start, end, String::new())
                })
                .collect();
            self.replace_ranges_atomic(edits);
        } else {
            let line = self.cursors.primary().focus.line;
            let line_count = self.buffer.line_count();
            let cur_len = self.buffer.line_len(line);
            if line + 1 < line_count {
                self.replace_range(Position::new(line, 0), Position::new(line + 1, 0), "");
            } else if line > 0 {
                let prev_len = self.buffer.line_len(line - 1);
                self.replace_range(
                    Position::new(line - 1, prev_len),
                    Position::new(line, cur_len),
                    "",
                );
            } else if cur_len > 0 {
                self.replace_range(Position::new(0, 0), Position::new(0, cur_len), "");
            }
        }
        Some(copied)
    }

    /// Pastes UTF-8 text (normalizing CRLF/CR line endings) at all active cursors.
    /// When multiple cursors are active and clipboard line count equals cursor count,
    /// spreads each clipboard line into its corresponding cursor (VS Code multi-cursor paste).
    pub fn paste_text(&mut self, text: &str) -> bool {
        if self.read_only || text.is_empty() {
            return false;
        }
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        if normalized.is_empty() {
            return false;
        }
        let cursor_count = self.cursors.count();
        if cursor_count > 1 {
            let lines: Vec<&str> = normalized.split('\n').collect();
            if lines.len() == cursor_count {
                self.cursors.normalize();
                let before_all = self.cursors.all().to_vec();
                let mut pairs: Vec<(Selection, &str)> = before_all
                    .iter()
                    .copied()
                    .zip(lines)
                    .collect();
                pairs.sort_by_key(|(sel, _)| std::cmp::Reverse(sel.ordered_range().0));

                let mut operations = Vec::with_capacity(pairs.len() * 2);
                let mut syntax_edits = Vec::with_capacity(pairs.len() * 2);
                let mut processed_carets: Vec<Position> = Vec::with_capacity(pairs.len());

                for (sel, line_text) in pairs {
                    let (start, end) = sel.ordered_range();
                    if start != end {
                        let (del_edit, _) = self.buffer.compute_edit_delta(start, end, "");
                        let deleted =
                            self.buffer
                                .delete_range(start.line, start.col, end.line, end.col);
                        syntax_edits.push(del_edit);
                        operations.push(crate::history::EditOperation::Delete {
                            start,
                            end,
                            text: deleted,
                        });
                    }
                    if !line_text.is_empty() {
                        let (ins_edit, _) = self.buffer.compute_edit_delta(start, start, line_text);
                        self.buffer.insert_str(start.line, start.col, line_text);
                        syntax_edits.push(ins_edit);
                        operations.push(crate::history::EditOperation::Insert {
                            pos: start,
                            text: line_text.to_string(),
                        });
                    }
                    let my_new_pos =
                        Position::new(start.line, start.col + line_text.chars().count());
                    for caret in &mut processed_carets {
                        *caret = crate::cursor::shift_position_after_edit(
                            *caret,
                            start,
                            end,
                            &[line_text],
                        );
                    }
                    processed_carets.push(my_new_pos);
                }

                let new_cursors: Vec<Selection> =
                    processed_carets.into_iter().map(Selection::point).collect();
                self.cursors = MultiCursor::from_selections(new_cursors);
                if !operations.is_empty() {
                    self.history
                        .push_transaction(crate::history::EditorTransaction::new(
                            operations,
                            before_all,
                            self.cursors.all().to_vec(),
                        ));
                    self.apply_syntax_edits_and_reparse(&syntax_edits);
                }
                return true;
            }
        }
        self.insert_text(&normalized);
        true
    }

    /// Handles a W3C DOM `EventType::Input` event (e.g. atomic multi-character `ImeCommit`).
    pub fn handle_input_event(&mut self, e: &Event) -> bool {
        if self.read_only {
            return false;
        }
        let text = if !e.value.is_empty() {
            e.value.as_str()
        } else if let Some(ref data) = e.data {
            data.as_str()
        } else {
            ""
        };
        if text.is_empty() {
            return false;
        }
        if self.paste_text(text) {
            self.ensure_cursor_visible();
            true
        } else {
            false
        }
    }

    /// Handles a W3C-compatible Strata [`Event`] keydown with full VS Code / Monaco coding keybindings.
    /// Returns `true` if the event was handled and mutated editor state.
    pub fn handle_key_event(&mut self, e: &Event) -> bool {
        let cmd = e.ctrl_key || e.meta_key;
        let key = e.key.as_str();

        if cmd {
            match key {
                "f" | "F" if !e.shift_key && !e.alt_key => {
                    self.open_find();
                    return true;
                }
                "h" | "H" if !e.shift_key && !e.alt_key => {
                    self.open_replace();
                    return true;
                }
                "a" | "A" if e.shift_key && !e.alt_key => {
                    self.toggle_block_comment();
                    return true;
                }
                "a" | "A" if !e.shift_key && !e.alt_key => {
                    self.select_all();
                    return true;
                }
                "c" | "C" if !e.shift_key && !e.alt_key => {
                    let _ = self.copy_selection();
                    return true;
                }
                "x" | "X" if !e.shift_key && !e.alt_key => {
                    if !self.read_only {
                        let _ = self.cut_selection();
                        self.ensure_cursor_visible();
                    }
                    return true;
                }
                "v" | "V" if !e.shift_key && !e.alt_key => {
                    if !self.read_only {
                        if let Some(clip) = strata_dom::clipboard::get_clipboard_text() {
                            if self.paste_text(&clip) {
                                self.ensure_cursor_visible();
                            }
                        }
                    }
                    return true;
                }
                "z" | "Z" if !e.alt_key => {
                    if !self.read_only {
                        if e.shift_key {
                            self.redo();
                        } else {
                            self.undo();
                        }
                        self.ensure_cursor_visible();
                    }
                    return true;
                }
                "y" | "Y" if !e.shift_key && !e.alt_key => {
                    if !self.read_only {
                        self.redo();
                        self.ensure_cursor_visible();
                    }
                    return true;
                }
                "/" | "?" => {
                    self.toggle_line_comment();
                    return true;
                }
                "7" | "&" if e.shift_key && !e.alt_key => {
                    // Non-US keyboard layout alias for Toggle Line Comment (`Ctrl+Shift+7`).
                    self.toggle_line_comment();
                    return true;
                }
                "d" | "D" if !e.shift_key && !e.alt_key => {
                    self.add_next_occurrence();
                    return true;
                }
                "l" | "L" if e.shift_key && !e.alt_key => {
                    self.select_all_occurrences();
                    return true;
                }
                "F2" if !e.shift_key && !e.alt_key => {
                    self.select_all_occurrences();
                    return true;
                }
                "l" | "L" if !e.shift_key && !e.alt_key => {
                    self.select_current_line_extend();
                    return true;
                }
                "k" | "K" if e.shift_key && !e.alt_key => {
                    self.delete_line();
                    return true;
                }
                "Enter" if !e.alt_key => {
                    if e.shift_key {
                        self.insert_line_above();
                    } else {
                        self.insert_line_below();
                    }
                    return true;
                }
                "Backspace" if !e.alt_key => {
                    self.delete_word_left();
                    self.ensure_cursor_visible();
                    return true;
                }
                "Delete" if !e.alt_key => {
                    self.delete_word_right();
                    self.ensure_cursor_visible();
                    return true;
                }
                "\\" | "|" if e.shift_key && !e.alt_key => {
                    self.goto_matching_bracket();
                    return true;
                }
                "[" | "{" if e.shift_key && !e.alt_key => {
                    let line = self.cursors.primary().focus.line;
                    self.toggle_fold_at(line);
                    return true;
                }
                "]" | "}" if e.shift_key && !e.alt_key => {
                    let line = self.cursors.primary().focus.line;
                    if self.is_folded(line) {
                        self.toggle_fold(line);
                    }
                    return true;
                }
                "[" if !e.shift_key && !e.alt_key => {
                    self.outdent_selection();
                    return true;
                }
                "]" if !e.shift_key && !e.alt_key => {
                    self.indent_selection();
                    return true;
                }
                "ArrowUp" if e.alt_key && e.shift_key => {
                    self.copy_line_up();
                    return true;
                }
                "ArrowDown" if e.alt_key && e.shift_key => {
                    self.copy_line_down();
                    return true;
                }
                "ArrowUp" if e.alt_key && !e.shift_key => {
                    self.add_cursor_above();
                    return true;
                }
                "ArrowDown" if e.alt_key && !e.shift_key => {
                    self.add_cursor_below();
                    return true;
                }
                "ArrowLeft" if !e.alt_key => {
                    self.move_word_left(e.shift_key);
                    self.ensure_cursor_visible();
                    return true;
                }
                "ArrowRight" if !e.alt_key => {
                    self.move_word_right(e.shift_key);
                    self.ensure_cursor_visible();
                    return true;
                }
                "Home" if !e.alt_key => {
                    let anchor = if e.shift_key {
                        self.cursors.primary().anchor
                    } else {
                        Position::ZERO
                    };
                    self.cursors.set_single(Position::ZERO);
                    self.cursors.all_mut()[0] = Selection::range(anchor, Position::ZERO);
                    self.ensure_cursor_visible();
                    return true;
                }
                "End" if !e.alt_key => {
                    let last_line = self.buffer.line_count().saturating_sub(1);
                    let end = Position::new(last_line, self.buffer.line_len(last_line));
                    let anchor = if e.shift_key {
                        self.cursors.primary().anchor
                    } else {
                        end
                    };
                    self.cursors.set_single(end);
                    self.cursors.all_mut()[0] = Selection::range(anchor, end);
                    self.ensure_cursor_visible();
                    return true;
                }
                _ => return false,
            }
        }

        // Alt-chorded editing & multi-cursor keybindings (without Ctrl/Meta).
        if e.alt_key {
            match key {
                "a" | "A" if e.shift_key => {
                    self.toggle_block_comment();
                    return true;
                }
                "i" | "I" if e.shift_key => {
                    self.add_cursors_to_line_ends();
                    return true;
                }
                "ArrowUp" if e.shift_key => {
                    self.add_cursor_above();
                    return true;
                }
                "ArrowDown" if e.shift_key => {
                    self.add_cursor_below();
                    return true;
                }
                "ArrowUp" if !e.shift_key => {
                    self.move_line_up();
                    return true;
                }
                "ArrowDown" if !e.shift_key => {
                    self.move_line_down();
                    return true;
                }
                "ArrowRight" if e.shift_key => {
                    self.expand_selection();
                    return true;
                }
                "ArrowLeft" if e.shift_key => {
                    self.shrink_selection();
                    return true;
                }
                _ => return false,
            }
        }

        if !cmd && self.decorations.completion.visible {
            match key {
                "ArrowDown" => {
                    self.decorations.completion.select_next();
                    return true;
                }
                "ArrowUp" => {
                    self.decorations.completion.select_prev();
                    return true;
                }
                "Enter" | "Tab" => {
                    self.accept_completion();
                    return true;
                }
                "Escape" => {
                    self.decorations.completion.close();
                    return true;
                }
                _ => {}
            }
        }

        match key {
            "Escape" => {
                if self.decorations.hover.visible {
                    self.decorations.hover.hide();
                    return true;
                }
                if self.search.is_open {
                    self.close_find();
                    return true;
                }
                if self.cursors.count() > 1 {
                    let p = self.cursors.primary().focus;
                    self.cursors.set_single(p);
                    return true;
                }
                false
            }
            "ArrowLeft" => {
                self.move_left(e.shift_key);
                self.ensure_cursor_visible();
                true
            }
            "ArrowRight" => {
                self.move_right(e.shift_key);
                self.ensure_cursor_visible();
                true
            }
            "ArrowUp" => {
                self.move_up(e.shift_key);
                self.ensure_cursor_visible();
                true
            }
            "ArrowDown" => {
                self.move_down(e.shift_key);
                self.ensure_cursor_visible();
                true
            }
            "Home" => {
                self.move_to_line_start_smart(e.shift_key);
                self.ensure_cursor_visible();
                true
            }
            "End" => {
                self.move_to_line_end(e.shift_key);
                self.ensure_cursor_visible();
                true
            }
            "PageUp" => {
                self.page_up(e.shift_key);
                true
            }
            "PageDown" => {
                self.page_down(e.shift_key);
                true
            }
            "Enter" => {
                if !self.read_only {
                    self.insert_newline();
                    self.ensure_cursor_visible();
                }
                true
            }
            "Backspace" => {
                if !self.read_only {
                    self.delete_backwards();
                    self.ensure_cursor_visible();
                }
                true
            }
            "Delete" => {
                if !self.read_only {
                    self.delete_forward();
                    self.ensure_cursor_visible();
                }
                true
            }
            "Tab" => {
                if !self.read_only {
                    if e.shift_key {
                        self.outdent_selection();
                    } else {
                        self.indent_selection();
                    }
                }
                true
            }
            other if !e.alt_key && other.chars().count() == 1 => {
                if let Some(ch) = other.chars().next() {
                    if !ch.is_control() {
                        if !self.read_only {
                            self.insert_char(ch);
                            self.ensure_cursor_visible();
                        }
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Renders a complete code editor view blueprint with default styling.
    pub fn render(&self) -> NodeBlueprint {
        self.render_blueprint("")
    }

    /// Renders a complete code editor view blueprint with line numbering gutter and syntax tokens.
    pub fn render_blueprint(&self, class_style: &str) -> NodeBlueprint {
        if self.buffer.line_count() > 100 {
            return self.render_virtualized(0.0, 800.0, 24.0, class_style);
        }

        let mut row_nodes = Vec::new();

        let visible_lines = self.buffer.slice_lines(0, self.buffer.line_count());
        let active_line = self.cursors.primary().focus.line;

        for (l_idx, line_str) in visible_lines {
            let is_active = l_idx == active_line;

            let row_class = if is_active {
                "flex items-center font-mono text-sm leading-6 bg-slate-800/40 hover:bg-slate-800/60"
            } else {
                "flex items-center font-mono text-sm leading-6 hover:bg-slate-800/30"
            };

            let visual_sublines = if let Some(cols) = self.wrap_column {
                crate::soft_wrap::wrap_logical_line(line_str, l_idx, cols, true)
            } else {
                vec![crate::soft_wrap::VisualLine {
                    logical_line: l_idx,
                    visual_subline: 0,
                    text: line_str.to_string(),
                    char_start: 0,
                    char_end: line_str.chars().count(),
                    is_continuation: false,
                }]
            };

            for vline in visual_sublines {
                let gutter_text = if vline.is_continuation {
                    "↳".to_string()
                } else {
                    (l_idx + 1).to_string()
                };

                let gutter_node = NodeBlueprint::element(
                    Tag::Span,
                    vec![
                        "w-12 text-right pr-4 select-none text-slate-500 font-mono text-xs"
                            .to_string(),
                    ],
                    Vec::new(),
                    vec![NodeBlueprint::text(gutter_text)],
                );

                let tokens = self.tokenize_line(l_idx, &vline.text);
                let line_token_nodes = Self::build_token_nodes(&vline.text, tokens);

                let code_node = NodeBlueprint::element(
                    Tag::Span,
                    vec!["flex-1 pl-2 whitespace-pre".to_string()],
                    Vec::new(),
                    line_token_nodes,
                );

                row_nodes.push(NodeBlueprint::element(
                    Tag::Div,
                    vec![row_class.to_string()],
                    Vec::new(),
                    vec![gutter_node, code_node],
                ));
            }
        }

        let container_class = format!(
            "bg-slate-950 text-slate-200 border border-slate-800 rounded-lg p-4 font-mono overflow-x-auto {}",
            class_style
        );

        NodeBlueprint::element(Tag::Div, vec![container_class], Vec::new(), row_nodes)
    }

    /// Maps a semantic [`TokenType`] to a Tailwind utility class string.
    pub fn token_type_class(token_type: TokenType) -> &'static str {
        match token_type {
            TokenType::Keyword => "text-purple-400 font-semibold",
            TokenType::ControlFlow => "text-fuchsia-400 font-semibold",
            TokenType::Type => "text-amber-400 font-medium",
            TokenType::Function => "text-blue-400",
            TokenType::Macro => "text-indigo-300 font-medium",
            TokenType::Identifier => "text-slate-200",
            TokenType::Parameter => "text-orange-200",
            TokenType::Property => "text-sky-300",
            TokenType::String => "text-emerald-400",
            TokenType::StringEscape => "text-teal-300 font-semibold",
            TokenType::Number => "text-orange-400",
            TokenType::Boolean => "text-amber-300 font-semibold",
            TokenType::Comment => "text-slate-500 italic",
            TokenType::DocComment => "text-slate-400 italic",
            TokenType::Operator => "text-rose-400",
            TokenType::Punctuation => "text-slate-400",
            TokenType::Tag => "text-cyan-400",
            TokenType::Attribute => "text-sky-300",
            TokenType::Namespace => "text-amber-200",
            TokenType::Heading => "text-cyan-300 font-bold",
            TokenType::Link => "text-blue-300 underline",
            TokenType::Variable => "text-violet-300",
        }
    }

    fn build_token_nodes(line_text: &str, tokens: Vec<SyntaxToken>) -> Vec<NodeBlueprint> {
        let mut line_token_nodes = Vec::new();
        if tokens.is_empty() {
            line_token_nodes.push(NodeBlueprint::text(line_text));
            return line_token_nodes;
        }

        let mut last_col = 0usize;
        for token in tokens {
            let start_col = token.start_col.min(line_text.len());
            let end_col = token.end_col.min(line_text.len());
            if start_col > last_col
                && line_text.is_char_boundary(last_col)
                && line_text.is_char_boundary(start_col)
            {
                line_token_nodes.push(NodeBlueprint::text(&line_text[last_col..start_col]));
            }

            let color_class = Self::token_type_class(token.token_type);
            line_token_nodes.push(NodeBlueprint::element(
                Tag::Span,
                vec![color_class.to_string()],
                Vec::new(),
                vec![NodeBlueprint::text(token.text)],
            ));
            last_col = end_col.max(last_col);
        }
        if last_col < line_text.len() && line_text.is_char_boundary(last_col) {
            line_token_nodes.push(NodeBlueprint::text(&line_text[last_col..]));
        }
        line_token_nodes
    }

    /// Renders a virtualized viewport slice of the code editor.
    ///
    /// Only the lines currently visible in the scroll viewport (plus an overscan margin)
    /// are laid out and styled into DOM blueprint nodes. Spacer containers at the top
    /// and bottom maintain the exact natural scroll height of the entire document.
    pub fn render_virtualized(
        &self,
        scroll_top: f32,
        viewport_height: f32,
        line_height: f32,
        class_style: &str,
    ) -> NodeBlueprint {
        let total_lines = self.buffer.line_count();
        let effective_lh = line_height.max(1.0);

        let first_visible = (scroll_top / effective_lh).floor() as usize;
        let visible_count = (viewport_height / effective_lh).ceil() as usize;

        // Apply a 2-line overscan buffer for smooth scrolling
        let start_line = first_visible.saturating_sub(2).min(total_lines);
        let end_line = (first_visible + visible_count + 2).min(total_lines);

        let top_spacer_height = start_line as f32 * effective_lh;
        let bottom_spacer_height = (total_lines.saturating_sub(end_line)) as f32 * effective_lh;

        let mut row_nodes = Vec::new();

        // Top spacer for virtual scroll height preservation
        if top_spacer_height > 0.0 {
            row_nodes.push(NodeBlueprint::element(
                Tag::Div,
                vec![format!(
                    "h-[{:.1}px] w-full pointer-events-none",
                    top_spacer_height
                )],
                Vec::new(),
                Vec::new(),
            ));
        }

        let visible_lines = self
            .buffer
            .slice_lines(start_line, end_line.saturating_sub(start_line));
        let active_line = self.cursors.primary().focus.line;

        for (l_idx, line_str) in visible_lines {
            let is_active = l_idx == active_line;

            let row_class = if is_active {
                "flex items-center font-mono text-sm leading-6 bg-slate-800/40 hover:bg-slate-800/60"
            } else {
                "flex items-center font-mono text-sm leading-6 hover:bg-slate-800/30"
            };

            let visual_sublines = if let Some(cols) = self.wrap_column {
                crate::soft_wrap::wrap_logical_line(line_str, l_idx, cols, true)
            } else {
                vec![crate::soft_wrap::VisualLine {
                    logical_line: l_idx,
                    visual_subline: 0,
                    text: line_str.to_string(),
                    char_start: 0,
                    char_end: line_str.chars().count(),
                    is_continuation: false,
                }]
            };

            for vline in visual_sublines {
                let gutter_text = if vline.is_continuation {
                    "↳".to_string()
                } else {
                    (l_idx + 1).to_string()
                };

                let gutter_class = if vline.is_continuation {
                    "w-12 text-right pr-4 select-none text-slate-600 font-mono text-xs"
                } else {
                    "w-12 text-right pr-4 select-none text-slate-500 font-mono text-xs"
                };

                let gutter_node = NodeBlueprint::element(
                    Tag::Span,
                    vec![gutter_class.to_string()],
                    Vec::new(),
                    vec![NodeBlueprint::text(gutter_text)],
                );

                // Tokens
                let tokens = self.tokenize_line(l_idx, &vline.text);
                let line_token_nodes = Self::build_token_nodes(&vline.text, tokens);

                let code_node = NodeBlueprint::element(
                    Tag::Span,
                    vec!["flex-1 pl-2 whitespace-pre".to_string()],
                    Vec::new(),
                    line_token_nodes,
                );

                row_nodes.push(NodeBlueprint::element(
                    Tag::Div,
                    vec![row_class.to_string()],
                    Vec::new(),
                    vec![gutter_node, code_node],
                ));
            }
        }

        // Bottom spacer for virtual scroll height preservation
        if bottom_spacer_height > 0.0 {
            row_nodes.push(NodeBlueprint::element(
                Tag::Div,
                vec![format!(
                    "h-[{:.1}px] w-full pointer-events-none",
                    bottom_spacer_height
                )],
                Vec::new(),
                Vec::new(),
            ));
        }

        let container_class = format!(
            "bg-slate-950 text-slate-200 border border-slate-800 rounded-lg p-4 font-mono overflow-y-auto {}",
            class_style
        );

        NodeBlueprint::element(Tag::Div, vec![container_class], Vec::new(), row_nodes)
    }
}

#[inline]
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn expand_emmet_expr(expr: &str) -> Option<String> {
    let trimmed = expr.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "!" {
        return Some(
            "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n    <meta charset=\"UTF-8\">\n    <title>Document</title>\n</head>\n<body>\n    \n</body>\n</html>"
                .to_string(),
        );
    }
    let parts: Vec<&str> = trimmed.split('>').collect();
    if parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    expand_emmet_chain(&parts)
}

fn expand_emmet_chain(parts: &[&str]) -> Option<String> {
    let head = parts.first()?;
    let (atom, repeat) = if let Some((left, count_str)) = head.rsplit_once('*') {
        let count = count_str.parse::<usize>().ok()?.clamp(1, 32);
        (left, count)
    } else {
        (*head, 1)
    };
    let mut tag = String::new();
    let mut id: Option<String> = None;
    let mut classes: Vec<String> = Vec::new();
    let mut mode = 0u8; // 0 = tag, 1 = class, 2 = id
    let mut cur = String::new();
    let flush = |mode: u8,
                 cur: &mut String,
                 tag: &mut String,
                 id: &mut Option<String>,
                 classes: &mut Vec<String>| {
        if cur.is_empty() {
            return;
        }
        let val = std::mem::take(cur);
        match mode {
            0 => *tag = val,
            1 => classes.push(val),
            2 => *id = Some(val),
            _ => {}
        }
    };
    for ch in atom.chars() {
        match ch {
            '.' => {
                flush(mode, &mut cur, &mut tag, &mut id, &mut classes);
                mode = 1;
            }
            '#' => {
                flush(mode, &mut cur, &mut tag, &mut id, &mut classes);
                mode = 2;
            }
            c if c.is_ascii_alphanumeric() || c == '-' || c == '_' => cur.push(c),
            _ => return None,
        }
    }
    flush(mode, &mut cur, &mut tag, &mut id, &mut classes);
    if tag.is_empty() {
        if id.is_some() || !classes.is_empty() {
            tag = "div".to_string();
        } else {
            return None;
        }
    }
    if !tag.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let mut attrs = String::new();
    if let Some(id_val) = id {
        attrs.push_str(&format!(" id=\"{id_val}\""));
    }
    if !classes.is_empty() {
        attrs.push_str(&format!(" class=\"{}\"", classes.join(" ")));
    }
    let inner = if parts.len() > 1 {
        expand_emmet_chain(&parts[1..])?
    } else {
        String::new()
    };
    let single = format!("<{tag}{attrs}>{inner}</{tag}>");
    if repeat == 1 {
        Some(single)
    } else {
        Some(vec![single; repeat].join("\n"))
    }
}

//! Decoupled Language Identity, Syntax Provider, Intelligence Provider, and Registry Traits.
//!
//! This module forms the microkernel extension boundary of the raw **Slate** editor core.
//! The core editor knows nothing about specific programming languages, grammars, or LSPs —
//! all language grammars, folding rules, syntax diagnostics, completions, hovers, definitions,
//! document symbols, color swatches, code actions, inlay hints, and formatters are plugged in
//! dynamically through [`LanguageRegistry`] and [`SyntaxProvider`].

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::buffer::LineBuffer;
use crate::cursor::Position;

/// Open, heap-interned language identifier (e.g. `"plaintext"`, `"rust"`, `"typescript"`, `"go"`).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LanguageId(Arc<str>);

impl Serialize for LanguageId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for LanguageId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::new(s))
    }
}

impl LanguageId {
    /// Built-in raw plain-text language identifier.
    pub const PLAINTEXT_STR: &'static str = "plaintext";

    /// Constructs a new normalized (lowercase) [`LanguageId`].
    pub fn new(id: impl AsRef<str>) -> Self {
        let normalized = id.as_ref().trim().to_ascii_lowercase();
        if normalized.is_empty() {
            Self(Arc::from(Self::PLAINTEXT_STR))
        } else {
            Self(Arc::from(normalized))
        }
    }

    /// Returns the raw plain-text [`LanguageId`].
    pub fn plaintext() -> Self {
        Self(Arc::from(Self::PLAINTEXT_STR))
    }

    /// Returns the underlying string identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for LanguageId {
    fn default() -> Self {
        Self::plaintext()
    }
}

impl fmt::Debug for LanguageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LanguageId({:?})", self.0)
    }
}

impl fmt::Display for LanguageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for LanguageId {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for LanguageId {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

/// Zero-indexed `(row, byte_column)` point matching Tree-Sitter & incremental edit conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TextPoint {
    pub row: usize,
    pub column: usize,
}

impl TextPoint {
    pub const fn new(row: usize, column: usize) -> Self {
        Self { row, column }
    }
}

/// Pure-Rust incremental text edit record emitted on every buffer mutation.
///
/// Consumed by Tree-Sitter syntax plugins, LSP incremental sync clients, and live Git diff trackers
/// without requiring any `tree-sitter` dependency inside the raw editor core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextEditDelta {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start_position: TextPoint,
    pub old_end_position: TextPoint,
    pub new_end_position: TextPoint,
    pub inserted_text: String,
}

#[cfg(feature = "builtin-syntax")]
impl From<&TextEditDelta> for tree_sitter::InputEdit {
    fn from(d: &TextEditDelta) -> Self {
        tree_sitter::InputEdit {
            start_byte: d.start_byte,
            old_end_byte: d.old_end_byte,
            new_end_byte: d.new_end_byte,
            start_position: tree_sitter::Point {
                row: d.start_position.row,
                column: d.start_position.column,
            },
            old_end_position: tree_sitter::Point {
                row: d.old_end_position.row,
                column: d.old_end_position.column,
            },
            new_end_position: tree_sitter::Point {
                row: d.new_end_position.row,
                column: d.new_end_position.column,
            },
        }
    }
}

#[cfg(feature = "builtin-syntax")]
impl From<tree_sitter::InputEdit> for TextEditDelta {
    fn from(e: tree_sitter::InputEdit) -> Self {
        Self {
            start_byte: e.start_byte,
            old_end_byte: e.old_end_byte,
            new_end_byte: e.new_end_byte,
            start_position: TextPoint::new(e.start_position.row, e.start_position.column),
            old_end_position: TextPoint::new(e.old_end_position.row, e.old_end_position.column),
            new_end_position: TextPoint::new(e.new_end_position.row, e.new_end_position.column),
            inserted_text: String::new(),
        }
    }
}

/// Token classification for semantic and lexical syntax highlighting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TokenType {
    Keyword,
    ControlFlow,
    Type,
    Function,
    Macro,
    Identifier,
    Parameter,
    Property,
    String,
    StringEscape,
    Number,
    Boolean,
    Comment,
    DocComment,
    Operator,
    Punctuation,
    Tag,
    Attribute,
    Namespace,
    Heading,
    Link,
    Variable,
}

/// A highlighted syntax token with UTF-8 byte column span offsets within its line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxToken {
    pub token_type: TokenType,
    pub start_col: usize,
    pub end_col: usize,
    pub text: String,
}

/// A collapsible structural fold range spanning `start_line..=end_line` (0-indexed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldRange {
    pub start_line: usize,
    pub end_line: usize,
    pub collapsed: bool,
}

/// Lightweight structural syntax diagnostic emitted directly by a syntax provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntaxDiagnostic {
    pub start: Position,
    pub end: Position,
    pub message: String,
    pub is_warning: bool,
}

/// A single pinned scope line rendered in the Sticky Scroll header above the viewport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StickyScope {
    pub line_idx: usize,
    pub text: String,
    pub depth: usize,
}

/// Declarative language configuration registered by a language plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageConfig {
    pub id: LanguageId,
    pub display_name: String,
    pub extensions: Vec<String>,
    pub filenames: Vec<String>,
    pub line_comment: Option<String>,
    pub block_comment: Option<(String, String)>,
    pub brackets: Vec<(char, char)>,
    pub auto_closing_pairs: Vec<(char, char)>,
    pub indent_size: usize,
}

impl LanguageConfig {
    /// Returns the built-in minimal configuration for raw plain text.
    pub fn plaintext() -> Self {
        Self {
            id: LanguageId::plaintext(),
            display_name: "Plain Text".to_string(),
            extensions: vec!["txt".to_string()],
            filenames: Vec::new(),
            line_comment: None,
            block_comment: None,
            brackets: vec![('(', ')'), ('[', ']'), ('{', '}')],
            auto_closing_pairs: vec![
                ('(', ')'),
                ('[', ']'),
                ('{', '}'),
                ('"', '"'),
                ('\'', '\''),
            ],
            indent_size: 4,
        }
    }
}

/// Pluggable syntax highlighting, folding, sticky-scroll, and bracket-matching provider.
///
/// Raw `CodeEditor` delegates ALL language-specific parsing to an implementation of this trait.
/// Language plugins implement this using Tree-Sitter, TextMate rules, regex state machines,
/// or WebAssembly modules.
pub trait SyntaxProvider: Send {
    /// Language identifier served by this provider.
    fn language_id(&self) -> LanguageId;

    /// Clones this provider into a boxed trait object (preserves `CodeEditor: Clone`).
    fn clone_box(&self) -> Box<dyn SyntaxProvider>;

    /// Performs a full parse/tokenization pass over `buffer`.
    fn parse_full(&mut self, buffer: &LineBuffer);

    /// Applies an incremental [`TextEditDelta`] and updates internal parse/lex caches.
    fn apply_edit(&mut self, buffer: &LineBuffer, edit: &TextEditDelta) {
        let _ = edit;
        self.parse_full(buffer);
    }

    /// Re-parses a dirty line range `[dirty_start..dirty_end)`.
    fn reparse_range(&mut self, buffer: &LineBuffer, dirty_start: usize, dirty_end: usize) {
        let _ = (dirty_start, dirty_end);
        self.parse_full(buffer);
    }

    /// Tokenizes a single logical line for display & minimap rendering.
    fn tokenize_line(&self, line_idx: usize, line_text: &str) -> Vec<SyntaxToken>;

    /// Computes collapsible structural fold ranges across the buffer.
    fn fold_ranges(&self, _buffer: &LineBuffer) -> Vec<FoldRange> {
        Vec::new()
    }

    /// Extracts lightweight local syntax diagnostics (unclosed delimiters, invalid JSON/XML, etc.).
    fn syntax_diagnostics(&self, _buffer: &LineBuffer) -> Vec<SyntaxDiagnostic> {
        Vec::new()
    }

    /// Finds the matching bracket position for the cursor at `pos`, if any.
    fn matching_bracket(&self, buffer: &LineBuffer, pos: Position) -> Option<(Position, Position)> {
        default_matching_bracket(buffer, pos)
    }

    /// Computes auto-indentation spaces for a new line inserted after `line_idx`.
    fn compute_auto_indent(&self, buffer: &LineBuffer, line_idx: usize, tab_size: usize) -> usize {
        let prev_line = buffer.line(line_idx).unwrap_or("");
        let leading = prev_line.chars().take_while(|&c| c == ' ').count();
        let trimmed = prev_line.trim_end();
        if trimmed.ends_with('{') || trimmed.ends_with('[') || trimmed.ends_with('(') {
            leading + tab_size
        } else {
            leading
        }
    }

    /// Computes enclosing scope lines for the Sticky Scroll header when scrolled to `top_line`.
    fn enclosing_scopes(&self, buffer: &LineBuffer, top_line: usize) -> Vec<StickyScope> {
        default_enclosing_scopes(buffer, top_line)
    }

    /// Optional read-only downcast hook for concrete syntax provider inspection.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }

    /// Optional mutable downcast hook for concrete syntax provider inspection.
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }
}

/// Zero-dependency default syntax provider used by the raw microkernel editor.
#[derive(Debug, Clone, Default)]
pub struct PlainTextSyntaxProvider {
    id: LanguageId,
}

impl PlainTextSyntaxProvider {
    pub fn new() -> Self {
        Self {
            id: LanguageId::plaintext(),
        }
    }

    pub fn with_language_id(id: LanguageId) -> Self {
        Self { id }
    }
}

impl SyntaxProvider for PlainTextSyntaxProvider {
    fn language_id(&self) -> LanguageId {
        self.id.clone()
    }

    fn clone_box(&self) -> Box<dyn SyntaxProvider> {
        Box::new(self.clone())
    }

    fn parse_full(&mut self, _buffer: &LineBuffer) {}

    fn apply_edit(&mut self, _buffer: &LineBuffer, _edit: &TextEditDelta) {}

    fn reparse_range(&mut self, _buffer: &LineBuffer, _dirty_start: usize, _dirty_end: usize) {}

    fn tokenize_line(&self, _line_idx: usize, line_text: &str) -> Vec<SyntaxToken> {
        if line_text.is_empty() {
            return Vec::new();
        }
        vec![SyntaxToken {
            token_type: TokenType::Identifier,
            start_col: 0,
            end_col: line_text.len(),
            text: line_text.to_string(),
        }]
    }
}

/// Generic brace/bracket matcher used as the default for any [`SyntaxProvider`].
pub fn default_matching_bracket(
    buffer: &LineBuffer,
    pos: Position,
) -> Option<(Position, Position)> {
    let line = buffer.line(pos.line)?;
    let chars: Vec<char> = line.chars().collect();
    let check_cols = [
        Some(pos.col),
        pos.col.checked_sub(1),
    ];
    for col in check_cols.into_iter().flatten() {
        let Some(&ch) = chars.get(col) else {
            continue;
        };
        let (target, forward) = match ch {
            '(' => (')', true),
            '[' => (']', true),
            '{' => ('}', true),
            ')' => ('(', false),
            ']' => ('[', false),
            '}' => ('{', false),
            _ => continue,
        };
        let origin = Position::new(pos.line, col);
        let mut depth = 0i32;
        if forward {
            for l_idx in pos.line..buffer.line_count() {
                let Some(l) = buffer.line(l_idx) else {
                    break;
                };
                let start_c = if l_idx == pos.line { col } else { 0 };
                for (c_idx, c) in l.chars().enumerate().skip(start_c) {
                    if c == ch {
                        depth += 1;
                    } else if c == target {
                        depth -= 1;
                        if depth == 0 {
                            return Some((origin, Position::new(l_idx, c_idx)));
                        }
                    }
                }
            }
        } else {
            for l_idx in (0..=pos.line).rev() {
                let Some(l) = buffer.line(l_idx) else {
                    break;
                };
                let l_chars: Vec<char> = l.chars().collect();
                let end_c = if l_idx == pos.line {
                    col
                } else if l_chars.is_empty() {
                    continue;
                } else {
                    l_chars.len() - 1
                };
                for c_idx in (0..=end_c).rev() {
                    let c = l_chars[c_idx];
                    if c == ch {
                        depth += 1;
                    } else if c == target {
                        depth -= 1;
                        if depth == 0 {
                            return Some((origin, Position::new(l_idx, c_idx)));
                        }
                    }
                }
            }
        }
    }
    None
}

/// Generic brace/indentation-based sticky-scroll enclosing scope detector.
pub fn default_enclosing_scopes(buffer: &LineBuffer, top_line: usize) -> Vec<StickyScope> {
    if top_line == 0 || buffer.line_count() == 0 {
        return Vec::new();
    }
    let limit = top_line.min(buffer.line_count().saturating_sub(1));
    let mut stack: Vec<(usize, String, usize)> = Vec::new();
    let mut depth = 0usize;

    for idx in 0..=limit {
        let Some(line) = buffer.line(idx) else {
            break;
        };
        let trimmed = line.trim();
        let opens = line.chars().filter(|&c| c == '{').count();
        let closes = line.chars().filter(|&c| c == '}').count();

        if closes > 0 {
            let pop_n = closes.min(depth);
            depth = depth.saturating_sub(pop_n);
            while stack.last().is_some_and(|(_, _, d)| *d > depth) {
                stack.pop();
            }
        }
        if opens > closes && idx < limit && !trimmed.is_empty() {
            let header = trimmed.to_string();
            // Skip trivial brace-only lines for cleaner sticky headers
            if header != "{" && !header.starts_with("//") {
                depth += opens - closes;
                stack.push((idx, line.to_string(), depth));
            } else {
                depth += opens - closes;
            }
        } else if opens > 0 && opens == closes {
            // Single-line block `{ ... }` — does not enclose subsequent lines
        }
    }

    stack
        .into_iter()
        .take(4)
        .enumerate()
        .map(|(d, (line_idx, text, _))| StickyScope {
            line_idx,
            text,
            depth: d,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Separated Language Intelligence Provider Traits (IntelliSense, LSP, Colors)
// ---------------------------------------------------------------------------

/// Kind of completion item shown in the IntelliSense popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CompletionItemKind {
    Function,
    Method,
    Struct,
    Enum,
    Interface,
    Field,
    Variable,
    Constant,
    Module,
    Keyword,
    Snippet,
    Property,
    TypeParameter,
    Text,
}

impl CompletionItemKind {
    pub const fn short_badge(self) -> &'static str {
        match self {
            Self::Function | Self::Method => "fn",
            Self::Struct => "st",
            Self::Enum => "en",
            Self::Interface => "tr",
            Self::Field | Self::Property => "fd",
            Self::Variable => "vr",
            Self::Constant => "cn",
            Self::Module => "md",
            Self::Keyword => "kw",
            Self::Snippet => "sn",
            Self::TypeParameter => "ty",
            Self::Text => "tx",
        }
    }
}

/// One autocomplete suggestion returned by a language plugin or LSP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionItem {
    pub label: String,
    pub insert_text: String,
    pub kind: CompletionItemKind,
    pub detail: Option<String>,
    pub documentation: Option<String>,
    pub sort_priority: u32,
}

/// Rich hover documentation returned for a symbol under the pointer or cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoverInfo {
    pub title: String,
    pub markdown: String,
    pub range: Option<(Position, Position)>,
}

/// Source location returned by Go-to-Definition and Find References.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLocation {
    pub uri: String,
    pub line: usize,
    pub col: usize,
}

/// Document outline / breadcrumb symbol kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SymbolKind {
    File,
    Module,
    Struct,
    Enum,
    Interface,
    Trait,
    Impl,
    Function,
    Method,
    Field,
    Constant,
    Variable,
    Class,
    Property,
}

impl SymbolKind {
    pub const fn badge(self) -> &'static str {
        match self {
            Self::File => "F",
            Self::Module => "M",
            Self::Struct | Self::Class => "S",
            Self::Enum => "E",
            Self::Interface | Self::Trait => "T",
            Self::Impl => "()",
            Self::Function | Self::Method => "fn",
            Self::Field | Self::Property => "f",
            Self::Constant => "C",
            Self::Variable => "v",
        }
    }
}

/// Hierarchical symbol in a document (powers Breadcrumbs, Outline view, and `@` Quick Open).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSymbol {
    pub name: String,
    pub detail: Option<String>,
    pub kind: SymbolKind,
    pub start_line: usize,
    pub end_line: usize,
    pub selection_col: usize,
    pub children: Vec<DocumentSymbol>,
}

/// Detected inline color literal (e.g. `#047857`, `rgb(34, 211, 238)`) for swatch rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedColor {
    pub line: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub css_color: String,
}

/// Single text replacement edit returned by code actions or formatters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextEdit {
    pub start: Position,
    pub end: Position,
    pub new_text: String,
}

/// Quick-fix or refactor action surfaced via the Code Action lightbulb (`Ctrl+.`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeAction {
    pub title: String,
    pub kind: String,
    pub is_preferred: bool,
    pub edits: Vec<TextEdit>,
}

/// Inline type or parameter hint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlayHint {
    pub line: usize,
    pub col: usize,
    pub label: String,
    pub is_type: bool,
}

/// Pluggable IntelliSense completion provider (in-process language plugin, WASM, or LSP bridge).
pub trait CompletionProvider: Send + Sync {
    fn language_id(&self) -> LanguageId;
    fn trigger_characters(&self) -> &[char] {
        &['.', ':', '/']
    }
    fn provide_completions(&self, text: &str, pos: Position) -> Vec<CompletionItem>;
}

/// Pluggable hover documentation provider.
pub trait HoverProvider: Send + Sync {
    fn language_id(&self) -> LanguageId;
    fn provide_hover(&self, text: &str, pos: Position) -> Option<HoverInfo>;
}

/// Pluggable document symbol provider (Outline + Breadcrumbs).
pub trait DocumentSymbolProvider: Send + Sync {
    fn language_id(&self) -> LanguageId;
    fn provide_symbols(&self, text: &str) -> Vec<DocumentSymbol>;
}

/// Pluggable inline color literal detector (renders inline 10x10 color swatches in viewport).
pub trait ColorProvider: Send + Sync {
    fn provide_colors(&self, line_idx: usize, line_text: &str) -> Vec<DetectedColor>;
}

/// Pluggable formatting provider.
pub trait FormattingProvider: Send + Sync {
    fn language_id(&self) -> LanguageId;
    fn format_document(&self, text: &str, tab_size: usize) -> Option<String>;
}

/// Default CSS/Hex inline color detector (`#rgb`, `#rrggbb`, `#rrggbbaa`) usable by any language plugin.
#[derive(Debug, Clone, Default)]
pub struct HexColorProvider;

impl ColorProvider for HexColorProvider {
    fn provide_colors(&self, line_idx: usize, line_text: &str) -> Vec<DetectedColor> {
        let mut out = Vec::new();
        let chars: Vec<char> = line_text.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            if chars[i] == '#' {
                // Only match color literals inside quotes, CSS values, or after whitespace/punctuation
                let prev_ok = i == 0
                    || matches!(
                        chars[i - 1],
                        '"' | '\'' | ' ' | '\t' | ':' | '(' | ',' | '['
                    );
                if prev_ok {
                    let start = i;
                    let mut j = i + 1;
                    while j < chars.len() && chars[j].is_ascii_hexdigit() {
                        j += 1;
                    }
                    let hex_len = j - (start + 1);
                    let next_ok = j == chars.len()
                        || !chars[j].is_ascii_alphanumeric() && chars[j] != '_';
                    if next_ok && matches!(hex_len, 6 | 8) {
                        let css_color: String = chars[start..j].iter().collect();
                        out.push(DetectedColor {
                            line: line_idx,
                            start_col: start,
                            end_col: j,
                            css_color,
                        });
                        i = j;
                        continue;
                    }
                }
            }
            i += 1;
        }
        out
    }
}

/// Factory closure producing a fresh [`SyntaxProvider`] instance per open document buffer.
pub type SyntaxProviderFactory = Arc<dyn Fn() -> Box<dyn SyntaxProvider> + Send + Sync>;

/// Dynamic central registry decoupling raw [`crate::CodeEditor`] from all language plugins.
///
/// Plugins register languages, file-extension mappings, syntax factories, completion/hover/symbol
/// providers, formatters, and color detectors at runtime (including live WASM plugins).
#[derive(Default)]
struct LanguageRegistryInner {
    configs: HashMap<LanguageId, LanguageConfig>,
    extension_map: HashMap<String, LanguageId>,
    filename_map: HashMap<String, LanguageId>,
    syntax_factories: HashMap<LanguageId, SyntaxProviderFactory>,
    completion_providers: HashMap<LanguageId, Vec<Arc<dyn CompletionProvider>>>,
    hover_providers: HashMap<LanguageId, Vec<Arc<dyn HoverProvider>>>,
    symbol_providers: HashMap<LanguageId, Vec<Arc<dyn DocumentSymbolProvider>>>,
    formatting_providers: HashMap<LanguageId, Arc<dyn FormattingProvider>>,
    color_providers: Vec<Arc<dyn ColorProvider>>,
}

#[derive(Clone, Default)]
pub struct LanguageRegistry {
    inner: Arc<RwLock<LanguageRegistryInner>>,
}

impl LanguageRegistry {
    /// Creates an empty microkernel registry containing only `"plaintext"`.
    pub fn new() -> Self {
        let reg = Self::default();
        reg.register_language(
            LanguageConfig::plaintext(),
            Arc::new(|| Box::new(PlainTextSyntaxProvider::new())),
        );
        reg
    }

    /// Registers a language configuration and its [`SyntaxProvider`] factory.
    pub fn register_language(&self, config: LanguageConfig, factory: SyntaxProviderFactory) {
        let mut inner = self.inner.write().unwrap();
        let id = config.id.clone();
        for ext in &config.extensions {
            let clean = ext.trim().trim_start_matches('.').to_ascii_lowercase();
            if !clean.is_empty() {
                inner.extension_map.insert(clean, id.clone());
            }
        }
        for name in &config.filenames {
            inner.filename_map.insert(name.clone(), id.clone());
        }
        inner.configs.insert(id.clone(), config);
        inner.syntax_factories.insert(id, factory);
    }

    /// Unregisters a language (used when a dynamic plugin is disabled or uninstalled).
    pub fn unregister_language(&self, id: &LanguageId) {
        let mut inner = self.inner.write().unwrap();
        if id.as_str() == LanguageId::PLAINTEXT_STR {
            return;
        }
        inner.configs.remove(id);
        inner.syntax_factories.remove(id);
        inner.completion_providers.remove(id);
        inner.hover_providers.remove(id);
        inner.symbol_providers.remove(id);
        inner.formatting_providers.remove(id);
        inner.extension_map.retain(|_, v| v != id);
        inner.filename_map.retain(|_, v| v != id);
    }

    /// Registers a completion (IntelliSense) provider for a language.
    pub fn register_completion_provider(&self, provider: Arc<dyn CompletionProvider>) {
        let mut inner = self.inner.write().unwrap();
        inner
            .completion_providers
            .entry(provider.language_id())
            .or_default()
            .push(provider);
    }

    /// Registers a hover provider for a language.
    pub fn register_hover_provider(&self, provider: Arc<dyn HoverProvider>) {
        let mut inner = self.inner.write().unwrap();
        inner
            .hover_providers
            .entry(provider.language_id())
            .or_default()
            .push(provider);
    }

    /// Registers a document symbol provider (Outline + Breadcrumbs) for a language.
    pub fn register_symbol_provider(&self, provider: Arc<dyn DocumentSymbolProvider>) {
        let mut inner = self.inner.write().unwrap();
        inner
            .symbol_providers
            .entry(provider.language_id())
            .or_default()
            .push(provider);
    }

    /// Registers a formatting provider for a language.
    pub fn register_formatting_provider(&self, provider: Arc<dyn FormattingProvider>) {
        let mut inner = self.inner.write().unwrap();
        inner
            .formatting_providers
            .insert(provider.language_id(), provider);
    }

    /// Registers an inline color detector (e.g., [`HexColorProvider`]).
    pub fn register_color_provider(&self, provider: Arc<dyn ColorProvider>) {
        let mut inner = self.inner.write().unwrap();
        inner.color_providers.push(provider);
    }

    /// Detects the registered [`LanguageId`] for a file path or filename, falling back to `"plaintext"`.
    pub fn detect_language(&self, path: &str) -> LanguageId {
        let inner = self.inner.read().unwrap();
        let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
        if let Some(id) = inner.filename_map.get(base) {
            return id.clone();
        }
        if let Some((_, ext)) = base.rsplit_once('.') {
            let clean = ext.to_ascii_lowercase();
            if let Some(id) = inner.extension_map.get(&clean) {
                return id.clone();
            }
        }
        LanguageId::plaintext()
    }

    /// Returns the registered [`LanguageConfig`] for `id`, falling back to plaintext config.
    pub fn config_for(&self, id: &LanguageId) -> LanguageConfig {
        let inner = self.inner.read().unwrap();
        inner
            .configs
            .get(id)
            .cloned()
            .unwrap_or_else(LanguageConfig::plaintext)
    }

    /// Lists all registered languages sorted by display name.
    pub fn list_languages(&self) -> Vec<LanguageConfig> {
        let inner = self.inner.read().unwrap();
        let mut list: Vec<LanguageConfig> = inner.configs.values().cloned().collect();
        list.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        list
    }

    /// Instantiates a fresh [`SyntaxProvider`] for `id`, falling back to [`PlainTextSyntaxProvider`].
    pub fn create_syntax_provider(&self, id: &LanguageId) -> Box<dyn SyntaxProvider> {
        let inner = self.inner.read().unwrap();
        if let Some(factory) = inner.syntax_factories.get(id) {
            factory()
        } else {
            Box::new(PlainTextSyntaxProvider::with_language_id(id.clone()))
        }
    }

    /// Queries all completion providers registered for `id`.
    pub fn provide_completions(
        &self,
        id: &LanguageId,
        text: &str,
        pos: Position,
    ) -> Vec<CompletionItem> {
        let providers = {
            let inner = self.inner.read().unwrap();
            inner
                .completion_providers
                .get(id)
                .cloned()
                .unwrap_or_default()
        };
        let mut out = Vec::new();
        for p in providers {
            out.extend(p.provide_completions(text, pos));
        }
        out.sort_by(|a, b| {
            a.sort_priority
                .cmp(&b.sort_priority)
                .then_with(|| a.label.cmp(&b.label))
        });
        out
    }

    /// Queries hover providers registered for `id`, returning the first match.
    pub fn provide_hover(&self, id: &LanguageId, text: &str, pos: Position) -> Option<HoverInfo> {
        let providers = {
            let inner = self.inner.read().unwrap();
            inner.hover_providers.get(id).cloned().unwrap_or_default()
        };
        for p in providers {
            if let Some(info) = p.provide_hover(text, pos) {
                return Some(info);
            }
        }
        None
    }

    /// Queries document symbol providers registered for `id`.
    pub fn provide_symbols(&self, id: &LanguageId, text: &str) -> Vec<DocumentSymbol> {
        let providers = {
            let inner = self.inner.read().unwrap();
            inner.symbol_providers.get(id).cloned().unwrap_or_default()
        };
        let mut out = Vec::new();
        for p in providers {
            out.extend(p.provide_symbols(text));
        }
        out
    }

    /// Formats document text via the registered formatter for `id`.
    pub fn format_document(&self, id: &LanguageId, text: &str, tab_size: usize) -> Option<String> {
        let provider = {
            let inner = self.inner.read().unwrap();
            inner.formatting_providers.get(id).cloned()
        }?;
        provider.format_document(text, tab_size)
    }

    /// Detects inline color literals across all registered [`ColorProvider`]s.
    pub fn provide_colors(&self, line_idx: usize, line_text: &str) -> Vec<DetectedColor> {
        let providers = {
            let inner = self.inner.read().unwrap();
            inner.color_providers.clone()
        };
        let mut out = Vec::new();
        for p in providers {
            out.extend(p.provide_colors(line_idx, line_text));
        }
        out
    }
}

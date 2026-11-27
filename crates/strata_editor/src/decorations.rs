//! Generic Editor Decoration & Overlay Layer (`DecorationStore`).
//!
//! Allows any internal or external plugin (Git Source Control, Run & Debug, LSP, Color Decorators,
//! Sticky Scroll, IntelliSense) to attach visual markers, inline color swatches, inlay hints,
//! gutter bars, breakpoints, hover popups, and completion popups to the raw editor surface
//! without coupling the editor core to any plugin's domain logic.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::language::{CompletionItem, DetectedColor, HoverInfo, InlayHint, StickyScope};

/// Visual kind for a gutter decoration marker on a logical line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GutterMarkerKind {
    /// Git added line (vertical green bar).
    GitAdded,
    /// Git modified line (vertical blue bar).
    GitModified,
    /// Git deleted line marker (red triangle/bar).
    GitDeleted,
    /// Active debugger breakpoint (solid red dot).
    Breakpoint,
    /// Disabled debugger breakpoint (muted hollow/gray dot).
    BreakpointDisabled,
    /// Conditional debugger breakpoint (red dot with condition indicator).
    BreakpointConditional,
    /// Active debugger stopped execution line (yellow highlight & arrow).
    DebugCurrentLine,
    /// Custom plugin-defined marker.
    Custom { color: String, glyph: String },
}

/// A single gutter marker contributed by a plugin source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GutterMarker {
    pub line: usize,
    pub kind: GutterMarkerKind,
    pub tooltip: Option<String>,
}

/// Interactive IntelliSense completion popup overlay state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompletionPopupState {
    pub visible: bool,
    pub anchor_line: usize,
    pub anchor_col: usize,
    pub items: Vec<CompletionItem>,
    pub selected: usize,
}

impl CompletionPopupState {
    pub fn open(anchor_line: usize, anchor_col: usize, items: Vec<CompletionItem>) -> Self {
        Self {
            visible: !items.is_empty(),
            anchor_line,
            anchor_col,
            items,
            selected: 0,
        }
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.items.clear();
        self.selected = 0;
    }

    pub fn select_next(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + 1) % self.items.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.items.is_empty() {
            self.selected = if self.selected == 0 {
                self.items.len() - 1
            } else {
                self.selected - 1
            };
        }
    }

    pub fn active_item(&self) -> Option<&CompletionItem> {
        if self.visible {
            self.items.get(self.selected)
        } else {
            None
        }
    }
}

/// Floating hover documentation popup state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HoverPopupState {
    pub visible: bool,
    pub anchor_line: usize,
    pub anchor_col: usize,
    pub info: Option<HoverInfo>,
}

impl HoverPopupState {
    pub fn show(anchor_line: usize, anchor_col: usize, info: HoverInfo) -> Self {
        Self {
            visible: true,
            anchor_line,
            anchor_col,
            info: Some(info),
        }
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.info = None;
    }
}

/// Plugin-isolated store of editor visual decorations and floating overlays.
///
/// Every plugin updates its own namespace (`source`, e.g. `"git"`, `"debug"`, `"lsp"`, `"colors"`)
/// so enabling, disabling, or refreshing one plugin never clobbers another plugin's decorations.
#[derive(Debug, Clone, Default)]
pub struct DecorationStore {
    gutter_by_source: HashMap<String, Vec<GutterMarker>>,
    colors_by_source: HashMap<String, Vec<DetectedColor>>,
    inlay_by_source: HashMap<String, Vec<InlayHint>>,
    sticky_scopes: Vec<StickyScope>,
    sticky_scroll_enabled: bool,
    active_debug_line: Option<usize>,
    pub completion: CompletionPopupState,
    pub hover: HoverPopupState,
}

impl DecorationStore {
    pub fn new() -> Self {
        Self {
            sticky_scroll_enabled: true,
            ..Self::default()
        }
    }

    /// Replaces all gutter markers owned by `source`.
    pub fn set_gutter_markers(&mut self, source: impl Into<String>, markers: Vec<GutterMarker>) {
        let key = source.into();
        if markers.is_empty() {
            self.gutter_by_source.remove(&key);
        } else {
            self.gutter_by_source.insert(key, markers);
        }
    }

    /// Replaces all inline color swatches owned by `source`.
    pub fn set_color_swatches(&mut self, source: impl Into<String>, swatches: Vec<DetectedColor>) {
        let key = source.into();
        if swatches.is_empty() {
            self.colors_by_source.remove(&key);
        } else {
            self.colors_by_source.insert(key, swatches);
        }
    }

    /// Replaces all inlay hints owned by `source`.
    pub fn set_inlay_hints(&mut self, source: impl Into<String>, hints: Vec<InlayHint>) {
        let key = source.into();
        if hints.is_empty() {
            self.inlay_by_source.remove(&key);
        } else {
            self.inlay_by_source.insert(key, hints);
        }
    }

    /// Clears every decoration contributed by `source` (used on plugin unload).
    pub fn clear_source(&mut self, source: &str) {
        self.gutter_by_source.remove(source);
        self.colors_by_source.remove(source);
        self.inlay_by_source.remove(source);
    }

    /// Sets the pinned sticky-scroll scope headers.
    pub fn set_sticky_scopes(&mut self, scopes: Vec<StickyScope>) {
        self.sticky_scopes = scopes;
    }

    pub fn sticky_scopes(&self) -> &[StickyScope] {
        if self.sticky_scroll_enabled {
            &self.sticky_scopes
        } else {
            &[]
        }
    }

    pub fn set_sticky_scroll_enabled(&mut self, enabled: bool) {
        self.sticky_scroll_enabled = enabled;
    }

    pub fn sticky_scroll_enabled(&self) -> bool {
        self.sticky_scroll_enabled
    }

    /// Highlights the active stopped debugger execution line (0-indexed).
    pub fn set_active_debug_line(&mut self, line: Option<usize>) {
        self.active_debug_line = line;
    }

    pub fn active_debug_line(&self) -> Option<usize> {
        self.active_debug_line
    }

    /// Returns all gutter markers on `line`.
    pub fn gutter_markers_on_line(&self, line: usize) -> Vec<&GutterMarker> {
        let mut out = Vec::new();
        for markers in self.gutter_by_source.values() {
            for m in markers {
                if m.line == line {
                    out.push(m);
                }
            }
        }
        out
    }

    /// Returns all inline color swatches on `line` sorted by start column.
    pub fn color_swatches_on_line(&self, line: usize) -> Vec<&DetectedColor> {
        let mut out = Vec::new();
        for list in self.colors_by_source.values() {
            for c in list {
                if c.line == line {
                    out.push(c);
                }
            }
        }
        out.sort_by_key(|c| c.start_col);
        out
    }

    /// Returns all inlay hints on `line` sorted by column.
    pub fn inlay_hints_on_line(&self, line: usize) -> Vec<&InlayHint> {
        let mut out = Vec::new();
        for list in self.inlay_by_source.values() {
            for h in list {
                if h.line == line {
                    out.push(h);
                }
            }
        }
        out.sort_by_key(|h| h.col);
        out
    }
}

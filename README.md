# Strata Editor (`strataui/editor`)

Drop-in, Monaco-class interactive `<CodeEditor>` widget for the [Strata](https://github.com/strataui/strata) pure-Rust frontend framework.

## Features

- Virtualized high-FPS viewport rendering
- Incremental Tree-sitter syntax parsing (`tree.edit(&InputEdit)`) across Rust, Python, JSON, YAML, TOML, XML/SDF, C++, JS/TS, Markdown, and Shell
- Find & replace, code folding, minimap, inline diagnostics, and CSS-variable theming

## Usage

```toml
[dependencies]
strata-ui = "0.1"
strata-editor = "0.1"
```

## License

MIT

# glassterm

A small glass terminal for Windows, in Rust (tao + wry) with xterm.js on top of ConPTY.
Same frosted acrylic backdrop as prism and anion.

## Features

- Real desktop blur behind the window, three glass levels (Solid / Frosted / Clear)
- Tabs: `Ctrl+Shift+T` new, `Ctrl+Shift+W` close, `Ctrl+Tab` / `Ctrl+Shift+Tab` switch, middle-click closes
- Copy / paste: `Ctrl+C` with a selection copies (otherwise it is SIGINT), `Ctrl+V` or `Ctrl+Shift+V` pastes, right-click copies or pastes
- `Ctrl+=` / `Ctrl+-` / `Ctrl+0` font size
- `Ctrl+click` opens links in the default browser
- Remembers window position, size, glass level and font size

## Shell

Uses `pwsh.exe` if it is on `PATH`, otherwise `powershell.exe`. To use something else, set
`shell` and `args` in `%LOCALAPPDATA%\glassterm\state.json`, e.g.

```json
{ "shell": "cmd.exe", "args": [] }
```

## Build

```
cargo build --release
```

The binary is `target\release\glassterm.exe`. xterm.js is vendored in `src/vendor` (MIT, see `LICENSE-xterm`).

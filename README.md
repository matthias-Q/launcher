# launch

A fuzzy searchable application launcher for X11, built with [Freya](https://freyaui.dev).

## Features

- **Calc**: type a calculation (`2^10 / 3`, `sqrt(2)*10`, `2pi`) and copy the result.
- **Apps**: launch desktop applications, ranked by usage history.
- **Windows**: switch to open windows, with their icons.
- **Emoji**: search emoji and copy and/or type them.
- **Power**: lock, suspend, log out, reboot, shut down (destructive ones ask for a second Enter).
- **All tab**: fuzzy search every enabled module at once.
- **dmenu mode**: pick a line from stdin and print it.
- Configurable look (sizes, fonts, colors, icon theme) via a TOML file.

## Installation

```sh
cargo install --path .
```

Requires an X11 session. Optional runtime tools:

- `xclip` (or another clipboard tool, see `copy_command`) to copy emoji
- `xdotool` to type emoji
- a compositor such as picom for rounded window corners

## Usage

```sh
launch                      # open with all enabled modules
launch --mode windows       # open on a specific module
printf 'a\nb\nc' | launch --dmenu
printf 'a\nb\nc' | launch --dmenu -p 'Pick:' --multi --index --no-custom
```

| Key                              | Action                     |
| -------------------------------- | -------------------------- |
| `Enter`                          | Activate selected entry    |
| `Shift+Enter`                    | Activate and keep open (dmenu `--multi`: mark line) |
| `Shift+Delete`                   | Forget from history / close window |
| `Esc`                            | Close                      |
| `↑` / `↓`, `Ctrl+P` / `Ctrl+N`, `Ctrl+K` / `Ctrl+J` | Move selection |
| `PageUp` / `PageDown`            | Move selection by a page   |
| `Tab` / `Shift+Tab`              | Next / previous tab        |
| `Alt+1` … `Alt+9`                | Jump to tab                |

## Configuration

The config is read from `$XDG_CONFIG_HOME/launch/config.toml` (usually
`~/.config/launch/config.toml`), or from the path given with `--config`.
Print a commented example with all options:

```sh
launch --default-config > ~/.config/launch/config.toml
```

Usage history is stored in `$XDG_CACHE_HOME/launch/`.

## License

[MIT](LICENSE)

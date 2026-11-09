# launch

A fuzzy searchable application launcher for X11, built with [Freya](https://freyaui.dev).

## Features

- **Calc**: type a calculation (`2^10 / 3`, `sqrt(2)*10`, `2pi`) and copy the result.
- **Apps**: launch desktop applications, ranked by usage history.
- **Run**: run any executable on `$PATH` (e.g. `~/.cargo/bin`), for programs without a `.desktop` file.
- **Windows**: switch to open windows, with their icons.
- **Emoji**: search emoji and copy and/or type them.
- **Power**: lock, suspend, log out, reboot, shut down (destructive ones ask for a second Enter).
- **Clipboard**: fuzzy search the clipboard history and put an entry back on the clipboard.
  Passwords and tokens (password-manager hint or heuristics) are kept only in
  `$XDG_RUNTIME_DIR` and forgotten after 30 seconds.
- **All tab**: fuzzy search every enabled module at once.
- **dmenu mode**: pick a line from stdin and print it.
- Blurred desktop behind the launcher (X11, `blur` / `blur_opacity` in the config).
- Configurable look (sizes, fonts, colors, icon theme) via a TOML file.

## Installation

```sh
cargo install --path .
```

Requires an X11 session. Optional runtime tools:

- `xclip` (or another clipboard tool, see `copy_command`) to copy emoji
- `xdotool` to type emoji
- `xclip` (X11) or `wl-clipboard` (Wayland) for the clipboard history
- a compositor such as picom for rounded window corners

## Usage

```sh
launch                      # open with all enabled modules
launch --mode windows       # open on a specific module
launch cliphist             # clipboard history (same as --mode clipboard)
printf 'a\nb\nc' | launch --dmenu
printf 'a\nb\nc' | launch --dmenu -p 'Pick:' --multi --index --no-custom
```

| Key                              | Action                     |
| -------------------------------- | -------------------------- |
| `Enter`                          | Activate selected entry    |
| `Shift+Enter`                    | Activate and keep open (dmenu `--multi`: mark line) |
| `Shift+Delete`, `Ctrl+D`         | Forget from history / close window / delete clipboard entry |
| `Esc`                            | Close                      |
| `↑` / `↓`, `Ctrl+P` / `Ctrl+N`, `Ctrl+K` / `Ctrl+J` | Move selection |
| `PageUp` / `PageDown`            | Move selection by a page   |
| `Tab` / `Shift+Tab`              | Next / previous tab        |
| `Alt+1` … `Alt+9`                | Jump to tab                |

## Daemon

`launch daemon` keeps the launcher loaded so it opens faster. Start it with your
session (it must inherit your `PATH` for the Run module); `launch` then asks it to
open a window and starts on its own when no daemon is running. Running the same
command while the launcher is open closes it. dmenu mode never uses the daemon.
Restart the daemon after installing a new version or changing `font_family`.

## Clipboard history

A daemon records every text you copy. Install it once as a systemd user
service (or start `launch cliphist daemon` with your session), then bind
`launch cliphist` to a key. Add `"clipboard"` to `modules` to also get a tab;
the "All" tab only shows clipboard entries once you type.

```sh
launch cliphist install-service    # install and start the daemon
launch cliphist uninstall-service  # stop and remove it
launch cliphist list               # print the history, newest first (secrets masked)
launch cliphist clear              # delete the whole history, including secrets
```

The history is in `$XDG_DATA_HOME/cliphist/history.json`, secrets in
`$XDG_RUNTIME_DIR/cliphist/secrets.json`. Limits and secret handling are set in
the `[clipboard]` section of the config.

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

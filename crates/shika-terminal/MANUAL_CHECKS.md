# shika-terminal manual checks

Things a person has to verify by hand. The unit tests cover the engine and
the input encoder; these cover feel, the IME, and real TUIs.

## Launch

Run from a terminal so `PATH` is inherited (login-shell PATH resolution is
step 2):

```sh
source "$HOME/.cargo/env"
cd ~/hieu/code/shika
cargo run -p shika-terminal --example demo -- --cwd ~/hieu/code/shika -- claude
```

Terminal 1 is `$SHELL -l`. Terminal 2 is the command after `--`.
Keys: `cmd-1` shell, `cmd-2` command, `cmd-g` toggle, `cmd-=` / `cmd--`
font size, `cmd-q` quit. Add `--stats` to print paint counters once a
second, and `--type "TEXT"` to type a line into the shell at startup.

Do not launch it from inside a Claude Code session if you want Claude's
transcripts saved: the child inherits `CLAUDE_CODE_CHILD_SESSION`.

## Claude Code

- [ ] `cmd-2`. The welcome screen, logo, input frame, and status lines draw
      with no gaps between rows and nothing out of column.
- [ ] Type a prompt and press Enter. Tools run, output streams, the spinner
      animates, and the screen does not tear.
- [ ] A permission or multiple-choice question appears. Up and Down move the
      choice, Enter answers it, Escape cancels.
- [ ] Shift-Enter and Option-Enter start a new line in the prompt instead of
      sending it.
- [ ] Ctrl-C clears the prompt; two quick Ctrl-C exit. Ctrl-R, Ctrl-G, and
      Shift-Tab do what Claude's footer says.
- [ ] Mouse wheel over a long answer scrolls (Claude's own scrolling when it
      asks for mouse or alternate scroll, otherwise terminal history).
- [ ] `cmd-1` while Claude is working, wait a minute, `cmd-2`. Claude kept
      running while hidden and the screen is current.
- [ ] Same with Cursor: `-- agent`.

## Typing

- [ ] Typing latency feels like WezTerm: hold a letter, characters repeat
      with no accent popup and no lag; fast typing never drops or reorders.
- [ ] Option-B / Option-F move by word in the shell (Option is Meta).
      Option-Left / Option-Right send `CSI 1;3 D/C`.
- [ ] Ctrl-A, Ctrl-E, Ctrl-U, Ctrl-W, Ctrl-R, Ctrl-L, Ctrl-D work in zsh.
- [ ] Arrows, Home, End, PageUp, PageDown, Delete, F1 to F12 work in `vim`
      or `htop`.
- [ ] Any Cmd shortcut other than Cmd-C and Cmd-V reaches the app (in the
      demo: Cmd-G switches terminals while typing in either one).

## IME

- [ ] Vietnamese (Telex or VNI from System Settings > Keyboard > Input
      Sources): type `tieesng vieejt` in the shell and in Claude's prompt.
      The composing text shows underlined at the cursor; the committed text
      is `tiếng việt` with no stray letters.
- [ ] Japanese Romaji or Pinyin: type, pick a candidate with Space and
      Enter. The candidate window sits at the cursor, not at the window
      corner. Enter during composition commits instead of running the line.
- [ ] Escape during composition cancels it and sends nothing.
- [ ] Ctrl-Cmd-Space emoji picker inserts the emoji.

## Selection and copy

- [ ] Drag selects character by character; double-click selects a word;
      triple-click selects a line. Cmd-C copies exactly that text.
- [ ] Dragging past the top or bottom edge scrolls history and keeps
      extending the selection.
- [ ] With mouse reporting on (Claude, `vim` with `set mouse=a`),
      Shift-drag still selects.
- [ ] Selection over wide characters (`你好`) copies whole characters.

## Paste

- [ ] Cmd-V of several lines into zsh shows them in the buffer without
      running them (bracketed paste); Enter runs them.
- [ ] Cmd-V of several lines into Claude's prompt pastes as one block.
- [ ] Cmd-V into `cat` (no bracketed paste) sends the lines as typed.
- [ ] Drop a PNG from Finder, or a screenshot thumbnail, on Claude's prompt.
      Claude shows it as an attached image. Same in Cursor's prompt.
- [ ] Drop a file whose name has spaces and parentheses on zsh. The path is
      escaped, `ls` of it works, and the terminal has focus afterwards.

## Pager: `seq 1 300 | less`

- [ ] Opens on the alternate screen with `1` at the top and `:` at the
      bottom.
- [ ] `j` / `k`, Up / Down, Space / `b`, `g` / `G` scroll as expected.
- [ ] Mouse wheel scrolls the pager (alternate scroll sends arrow keys).
- [ ] `/150` Enter highlights the match.
- [ ] Resize the window while `less` is open: it redraws to the new size
      with nothing left over.
- [ ] `q` returns to the shell with the earlier output and prompt intact.

## Resize and scrollback

- [ ] Drag the window edge in the shell and in Claude: the grid reflows, the
      program redraws, the prompt stays usable. No flicker of a two-column
      grid during the drag.
- [ ] `cmd-=` / `cmd--` change the font size and the program sees the new
      rows and columns (`stty size`).
- [ ] Wheel or trackpad scrolls history in the shell; typing jumps back to
      the bottom. Shift-PageUp / Shift-PageDown page through history.

## Speed and idle

- [ ] `--stats`: an idle terminal prints `0 frames`.
- [ ] `--type "time seq 1 2000000"`: the window stays responsive (switch
      terminals, resize) and `seq` finishes in about the same time as in
      WezTerm. The busy terminal paints at most once per display frame.
- [ ] `cat` a large file in the hidden terminal (`-- sh -c 'cat big.log;
      exec $SHELL -l'` and stay on `cmd-1`): `--stats` shows 0 frames for
      t2 while it drains; `cmd-2` shows the end of the file.

## Colors and text

- [ ] `msgcat --color=test` or a 24-bit color script shows smooth gradients.
- [ ] Bold, italic, underline, curly underline, strikethrough, inverse, and
      dim all render. Bold uses a real bold face.
- [ ] Emoji and CJK take two cells and the rest of the row stays aligned.
- [ ] Focus: click another app. The block cursor turns hollow, and a
      program that asked for focus events (`vim`, Claude) receives them.

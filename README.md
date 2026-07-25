# zync

`zync` syncs the macOS clipboard over SSH, including images.

The first target workflow is pasting an image into a local app like Codex or
Claude while you are connected to another Mac over SSH.

## Install

Install the binary on both Macs:

```sh
cargo install --path .
```

The remote SSH user must be able to run `zync`. If it is not on the remote
`PATH`, pass the absolute path with `--remote-bin`.

## Paste remote images into local Codex or Claude

Run this on the Mac where Codex or Claude is open:

```sh
zync connect user@other-mac
```

Then copy an image on `other-mac`. For example, from an SSH shell on that Mac:

```sh
osascript -e 'set the clipboard to (read POSIX file "/tmp/screenshot.png" as «class PNGf»)'
```

`zync` will put that image into the local macOS clipboard. You can then paste it
directly into Codex, Claude, ChatGPT, Preview, Messages, or any other local app
that accepts image paste.

Text clipboard changes are synced too.

## Commands

```text
zync connect user@other-mac [--remote-bin zync] [--ssh-bin ssh] [--interval-ms 750]
zync agent
zync serve --peer user@other-mac [--remote-bin zync] [--ssh-bin ssh] [--interval-ms 750]
zync send-once --peer user@other-mac [--remote-bin zync] [--ssh-bin ssh]
zync receive
```

`connect` is the recommended mode. It opens one persistent SSH session and runs
`zync agent` remotely. Clipboard payloads are framed over SSH stdin/stdout, so
the remote machine does not need to SSH back to the local machine.

`serve`, `send-once`, and `receive` are lower-level push commands for setups
where one machine can SSH directly into the other.

## Current limits

- macOS only.
- Supports text, PNG, JPEG, and TIFF clipboard payloads.
- Uses polling instead of native clipboard change notifications.
- Trusts the SSH endpoint. There is no additional encryption beyond SSH.

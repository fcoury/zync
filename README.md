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

## Connect through a custom SSH port

Use an SSH URI when the remote machine listens on a nonstandard port:

```sh
zync connect ssh://user@other-mac:2222
```

You can also configure an SSH host alias in `~/.ssh/config`:

```sshconfig
Host home
    HostName other-mac
    User user
    Port 2222
```

Then connect using the alias:

```sh
zync connect ssh://home
```

## Access the remote clipboard from an SSH session

On macOS, SSH commands can run in the `Background` launchd session instead of
the logged-in user's graphical `Aqua` session. If the remote agent fails with
`pbcopy exited with exit status: 1`, start it in the graphical session using
`launchctl asuser`.

On the remote Mac, find your numeric user ID:

```sh
id -u
```

Then run this on the local Mac, replacing `501` and the binary path with the
remote user's actual ID and installation path:

```sh
zync connect ssh://home \
  --remote-bin 'launchctl asuser 501 /Users/user/.cargo/bin/zync'
```

`zync connect` starts the remote agent automatically; do not run `zync agent`
separately.

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

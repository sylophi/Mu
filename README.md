<img src="assets/icon.png" width="128" alt="Mu's icon">

# Mu

Mu is a browser for your Mac, made to be snappier and more intuitive than Finder. It comes with a terminal, a plain text editor, a simple activity monitor and a disk usage viewer. Everything responds right away, and you can drive it all from the keyboard.

![Mu showing a folder of photos as a grid, with the selected one in the preview](assets/screenshot.png)

## Install

Download `Mu.zip` from the [latest release](https://github.com/sylophi/mu/releases/latest), open it and move Mu to Applications. It keeps itself up to date from there.

Mu needs macOS 27 on a Mac with Apple silicon.

## Features

### Files

- Tabs and windows, which open again where they were
- A details list, or a grid with thumbnails, sorted naturally with folders first
- Copy, move, rename, compress and extract, with undo and redo for every change
- A path bar to type into, which completes folder names and suggests the ones you go to most
- Quick Look, and a preview pane for images, text, documents and folders
- Type-to-select, ⇧-click and ⌘-click selection, and box selection
- The Trash with Put Back
- Folders that merge when copied onto ones with the same name, and a choice for each file that's in the way
- A shelf to set files aside while you find where they go
- Other apps' Show in Finder can show files in Mu instead
- Disk images laid out as Finder shows them, ready to drag the app onto Applications

### The rest of the Mac

- Terminals as tabs, which open where you are
- An editor that opens text files in place of TextEdit, and find in folder
- Activity, for CPU, GPU, memory, network, disks and processes
- Storage, for what takes up the room
- Disks you can eject, and file servers on the network

Its light and dark themes follow the system.

## From the command line

`mu` opens a tab in Mu from a shell or a script. Settings → Command line tool installs it.

```sh
mu                  # this folder
mu ~/Downloads      # a folder, or a file selected in its folder
mu terminal         # a terminal here
mu edit notes.md    # the editor
mu find             # find in this folder
mu storage ~        # what takes up the room
mu activity         # or history, or settings
```

Other apps can do the same with a link, like `mu://folder/Users/me/Downloads`.

## Keys

| Action | Key |
| --- | --- |
| Command palette | ⇧⌘P |
| Open / open in new tab / open in editor | ↩ or ⌘↓ / ⌘↩ / ⌘E |
| Back / forward / enclosing folder | ⌘[ / ⌘] / ⌘↑ |
| Go to folder | ⌘L |
| Rename | ⇧↩ or F2 |
| Quick Look | Space |
| Move to Trash | ⌘⌫ |
| Undo / redo | ⌘Z / ⇧⌘Z |
| Filter / find in folder | ⌘F / ⇧⌘F |
| New tab / close tab / reopen closed tab | ⌘T / ⌘W / ⇧⌘T |
| New terminal | ⌃` |
| Grid or list | ⇥ |
| Sidebar / preview / hidden files | ⌘B / ⌘P / ⇧⌘. |

The rest are in the menus and the command palette.

## Building

```sh
make run        # runs Mu
make app        # builds dist/Mu.app
make bench      # times how soon Mu answers, as CI does for each change
```

It needs Rust and Xcode. `make` also applies Mu's patches to GPUI (`patches/`) before it builds, so a plain `cargo` command needs a `make patched` first.

## License

Mu is under the [MIT License](LICENSE). It's made with [GPUI](https://www.gpui.rs), Zed's UI framework, through [GPUI Kit](https://github.com/longbridge/gpui-kit), and the patches in `patches/` change GPUI crates from [Zed](https://github.com/zed-industries/zed), so they're under the Apache License 2.0, as those crates are.

The colors are from [Zedokai](https://github.com/slymax/zedokai) by slymax. The bundled fonts are [JetBrains Mono](https://github.com/JetBrains/JetBrainsMono) (SIL Open Font License) and the Symbols Nerd Font from [Nerd Fonts](https://github.com/ryanoasis/nerd-fonts) (MIT), with their licenses in `assets/fonts`.

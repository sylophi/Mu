# gpui-pre-macos

GPUI's macOS platform, [gpui-pre-macos 0.3.9](https://crates.io/crates/gpui-pre-macos/0.3.9) as published (a snapshot of [Zed](https://github.com/zed-industries/zed) at `93d7456`), with two changes Mu needs. Mu's `Cargo.toml` patches it in.

- A window's display link pauses once nothing has asked for a frame for 100 ms, so an idle window doesn't wake the main thread on every refresh of the display. GPUI's `frame_waker` and `schedule_frame` start it again (`window.rs`, with `request_frame_now` in `display_link.rs`).
- A drag Mu hands to macOS, like files dragged out to another app, stops the last mouse move being replayed once it begins (`draggingSession:willBeginAtPoint:` in `window.rs`).

Every other file is as published, so diffing it against the crate shows all of it. The crate can go once a gpui-pre snapshot has both, [zed-industries/zed#65472](https://github.com/zed-industries/zed/pull/65472) or its like for the first.

Licensed under the Apache License 2.0, in `LICENSE-APACHE`.

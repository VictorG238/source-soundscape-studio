# Architecture

`crates/app` owns the native window, file selection, drag and drop, selection and panel layout. The Source editor stores a draft per document and applies it to the shared model as one validated history entry. The Manifest Editor uses the same document and draft logic.

`crates/soundscape-core` owns the document bytes, file identity and line index. File loading checks a bounded stream before indexing. Source edits preserve a UTF-8 BOM and untouched legacy bytes. Document logic is independent of egui and can be tested without a display.

`crates/app/src/preview.rs` expands the selected scene and its open-document references into playback layers. It overlays valid inspector drafts before the document changes. Parameter updates reach the active mixer through atomics, and random tracks reschedule on their current clock. A change to the assets or expanded layer layout reloads playback asynchronously. Preview controls and sound directories are independent of document history.

`crates/audio-engine` owns local source resolution, Symphonia decoding, sample-clock scheduling, voice mixing and the CPAL stream. A worker resolves and decodes unique assets once per preview, creates the device stream and releases it on completion or cancellation. The output callback reads decoded frames, uses a preallocated voice pool and publishes counters and recent events through atomics. It performs no file operations or GUI calls. Docker's development and cross-compilation images share their base environment and do not ship with the desktop application.

## Updates

The update worker checks the latest stable GitHub Release for the official Cargo repository, with a bounded response and a cached ETag. The UI builds download links from the configured repository and validated tag. The browser handles manual downloads.

Before enabling automatic installation, publish a signed manifest containing the release version, target and package digest. The app must validate the manifest with an embedded public key, reject downgrades and mismatched targets, and verify the staged package against the signed digest. A trusted local helper would wait for the app to exit, replace the binary from a staging file on the same volume and keep the previous binary until the new version reports a successful launch. On failure it would restore the prior binary. Preferences and project files stay outside the replacement operation.

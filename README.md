# Source Soundscape Studio

**Source Soundscape Studio** is a native Source Engine soundscape editor for Windows and Linux. Edit Source 1 KeyValues scripts, audition audio directly from VPK archives, and hear soundscape changes while you edit. The included audio demo runs without Steam.

[Download for Windows x64](https://github.com/VictorG238/source-soundscape-studio/releases/latest) · [Source 1 audio guide](docs/source-audio.md) · [Changelog](CHANGELOG.md) · [MIT License](LICENSE)

## Features

- **Visual soundscape editor** for `playlooping`, `playrandom` and nested `playsoundscape` layers, with live preview of volume, pitch and random timing.
- **Sound Browser** indexes loose audio and Source 1 VPK v1/v2, with search, waveform audition and multi-select for random variants.
- **Lossless KeyValues editing** preserves comments, duplicate keys and unknown properties, with undo and redo shared between the Source and Visual editors.
- **Manifest Editor** manages the load order of `scripts/soundscapes_manifest.txt` and discovers audio sources through `gameinfo.txt`.

Open a script through **File → Open scripts**, drag it into the app or pass its path at launch. For a first run, use the [included audio demo](#listen-to-the-included-demo).

## Build with Docker

Use Docker Desktop in Linux container mode, or Docker Engine with Compose. Run these commands from the project directory. The first build needs internet access and downloads compilers and dependencies into Docker.

| Operation | Command |
| --- | --- |
| Windows x64 release | `docker compose run --rm --build build-windows` |
| Linux x64 release on an amd64 host | `docker compose run --rm --build build-linux` |
| Linux development build | `docker compose run --rm --build dev` |
| Unit tests | `docker compose run --rm --build test` |
| Clippy | `docker compose run --rm --build clippy` |
| Check formatting | `docker compose run --rm --build format` |
| Clear intermediate compilation artifacts | `docker compose run --rm dev cargo clean` |

The Windows build uses the Microsoft SDK and CRT through cargo-xwin. Review the [Microsoft license terms](https://go.microsoft.com/fwlink/?LinkId=2086102) before using this build. The packaging script inspects the PE headers and DLL imports before copying the executable to `dist/SourceSoundscapeStudio.exe`. Double-click it on Windows to open the workspace.

The toolchain and direct dependency versions are pinned. Cargo uses the checked-in lockfile. Docker retains compiled crates, registry data, Git dependencies and the downloaded SDK in named volumes. The Docker image contains the compiler tools. Changes to Rust source do not invalidate its layers.

## Editing soundscapes

Open `tests/fixtures/harbor.txt` to try two linked soundscapes. Select a soundscape beneath its file in the browser. Search matches either file or soundscape names. Use **+ Soundscape** to add a definition and its context menu to rename, duplicate or remove it. **File → Create script** creates a new document.

In **Visual Editor**, **+ Add layer** inserts a looping, random or nested layer. Select its card to open the Inspector. Adjust a slider or click its number for precise input. Enable **Range** for two endpoints, then apply the change as a single undoable edit. Use **Copy**, **Delete**, **Up** and **Down** on cards to manage layers. A nested card can navigate to a uniquely named soundscape in any open file.

Use **Preview → Audio sources** to add loose sound folders, select the game or mod directory that contains `gameinfo.txt`, or mount one or more VPK files. The first matching source wins. Explicit sources come first in the order shown and persist in `SourceSoundscapeStudio/sound-roots.txt` under the user configuration directory (Roaming AppData on Windows). Scripts under a game's `scripts/` directory also discover sources from the game's `SearchPaths` and nearby content. For scripts copied elsewhere, add their game or mod folder manually. Use **Up** to change priority and **Refresh file status** to recheck the sources.

### Listen to the included demo

1. Open `tests/fixtures/audio/soundscapes_studio.txt` and select **studio.demo**.
2. Add `tests/fixtures/audio/sound` with **Add sound folder**.
3. Press **Play soundscape**. A sustained low tone plays alongside two intermittent, higher chimes from the nested soundscape. Solo the nested card to hear the chimes alone.

Click a sound name on a card to audition it individually. The panel displays decoded duration, codec, channels, sample rate, waveform and progress. **Pause**, **Resume**, **Stop** and **Master** apply to the current preview. Choose **Play soundscape** to return to the mix. Amber sound names indicate missing assets or missing search folders. Hover for the original Source path. Random cards accept additional variants through **+ Sound**. The Inspector supports replacing and removing variants.

### Choose sounds from mounted content

**Choose sound** and **+ Sound** open the Sound Browser. Search by filename, path or extension, narrow the results by folder or source, and click the play icon to audition any available copy. Select several entries for a Random layer and use **Add Selected** to insert them as one undoable edit. A shadowed copy can be auditioned, while an inserted path references the first accessible copy in mount order.

The catalog indexes paths in the background. Selecting a sound decodes its waveform and metadata. Duration sorting and metadata filters trigger background header analysis for the matching files. Use the waveform to seek, or enable **Loop** to audition embedded loop markers or repeat the whole file. Loop compatibility warnings distinguish a Source WAV cue point from a sampler loop. **Import external audio** opens the native picker. Copying an unmounted file into a content folder requires confirmation and a new filename.

See [Source 1 audio resources](docs/source-audio.md) for mount, format and soundscript behavior.

### Audio behavior and limits

CPAL 0.18.2 uses the default native output. Symphonia 0.6.1 decodes mono/stereo WAV PCM, Microsoft and IMA ADPCM WAV, MP3 and Ogg Vorbis. FLAC decoding is also enabled. The sample fixtures contain original synthesized tones. Playback requires neither external converters nor a running game.

The first random event waits half a sampled interval, then subsequent events wait a full sampled interval. Each event selects a variant, volume and pitch. A nested soundscape inherits its parent's gain. Missing clips are skipped with a warning. Mute and Solo affect entire top-level layers, including nested children. While a soundscape is active, valid inspector drafts update volume, pitch and random timing immediately. **Apply** commits the draft to the undoable document. **Save** writes the document to disk. Changing the waves or playback layout reloads the mix in the background and preserves Pause.

Preview approximations include stereo playback without map positioning or DSP, full-file loops in the scene preview, linear resampling and a 10 ms minimum repeat interval. Decoding occurs on a worker with a 256 MiB decoded-sample budget and a maximum of 128 voices. Source 1 VPK versions 1 and 2 support embedded files and numbered chunks. The player reads the requested audio and leaves the archives untouched. Steam app mounts outside the selected game tree and engine-specific language and platform conditions require manual source configuration.

**Source** edits the current document, including comments, repeated keys and extensions. Use **Ctrl+F** to find text and **Apply changes** to validate the draft and update the visual editor. **Undo** and **Redo** update both views. **File → Save** replaces the original using a temporary file. **Save As** requires an unused destination on a filesystem supporting hard links, such as NTFS. External changes detected before saving are reported.

The parser indexes ordered keys and blocks by byte range. Local edits retain comments, duplicate properties and unaffected legacy bytes. Source editing maps legacy bytes individually and restricts new text to the Latin-1 byte range for those files. The editor accepts scripts up to 8 MiB and 250,000 lines. Syntax diagnostics include line and byte column.

**Manifest Editor** opens sibling `scripts/soundscapes_manifest.txt` files and manages ordered `file` entries. Create a script in a game's `scripts/` directory to be offered registration in an eligible manifest. Review the edit and choose **Save manifest** to write it. Registered open scripts appear under their manifest in the browser. The manifest editor resolves loose scripts within that game layout.

## Version and publishing metadata

The workspace Cargo manifest owns the SemVer version. **Help → About Source Soundscape Studio** displays the compiled version, target, build profile and Git commit when supplied. Docker can receive the commit through `docker compose run --rm -e SSS_GIT_COMMIT=<commit> build-windows`.

The project is released under the [MIT License](LICENSE). Third-party dependencies retain their own licenses. Source Engine and Valve game content belong to their respective owners. The test fixtures are synthesized audio and original scripts.

CI runs format, Clippy, tests and a build in Docker. To prepare a stable release, update the workspace version and the matching `## MAJOR.MINOR.PATCH` changelog entry. Push the matching `vMAJOR.MINOR.PATCH` tag after the official repository and license are configured. The release workflow builds the Windows x64 executable in Docker and publishes it with a portable ZIP, release notes and SHA-256 checksums.

**Tools → Settings** controls stable update checks. Once the official repository is configured, the app consults its latest public stable GitHub Release in the background at most once a day on startup. **Check now** runs a manual check. Downloads open in the browser. Automatic installation requires signed releases, a trusted public key and a recovery mechanism.

The layer names and nested reference field follow Valve's SDK 2013 client. [Soundscape implementation](https://github.com/ValveSoftware/source-sdk-2013/blob/master/src/game/client/c_soundscape.cpp)

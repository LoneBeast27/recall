# Contributing to Recall

Build and test steps are in the README ("Build and test (developers)"). This file holds the rules
a change must follow.

## Settings compatibility

People update Recall and sometimes roll it back. Their settings must come with them.

### The rule

A change that is experimental, or that changes how a core part works (recording, audio, the
library, the editor, the overlay, the display handling), must do one of two things:

1. **Keep compatibility.** Settings written by every earlier release load with every value kept.
   If a setting moves or changes shape, the change maps the old value to the new one and the
   compatibility tests prove it.
2. **Ship as a major release (x.0.0)** with a release note. A major release may reset settings,
   but only in the area that changed, and only when the area is declared. Everything else still
   loads cleanly. For example, a major audio change may reset saved audio settings, while video
   settings carry over as they are.

Keep as much as possible even in a major release: a reset is for a setting that cannot be mapped,
not for one that is inconvenient to map.

Rollback must not lose settings either. When an older release reads a file a newer one wrote, it
keeps the values it knows. It also keeps the keys it does not know, so moving forward again finds
them. Clip names and favourites are covered in both directions.

### What the tests cover

The `compatibility` tests run with the normal test run and in the release workflow:

| File | Holds | Tests |
|---|---|---|
| `%APPDATA%\recall-hdr\config.toml` | recording, audio, hotkeys, overlay, storage, display | `recall-hdr`: `config::compatibility` |
| `%APPDATA%\Recall\library.json` | clip names, favourites, last played | `recall-native`: `services::library::compatibility` |
| `%APPDATA%\Recall\folders.json` | recordings folder, watched folders | same |
| `%APPDATA%\Recall\editor.json` | the editor's remembered choices | same |
| `%LOCALAPPDATA%\Recall\updates\settings.json` | automatic update checks | `recall-native`: `update::compatibility` |

Run them alone:

```powershell
cargo test -p recall-hdr compatibility
cargo test --manifest-path recall-native\Cargo.toml compatibility
```

How they work:

- `compat/settings/v<version>/` holds the files each release wrote. Every value differs from that
  release's default (`default.toml`), so a lost value cannot pass unnoticed. A value that must stay
  at its default (an id, a marker) is listed under `"unvaried"` in `compat/rules.json` with the
  reason.
- **Forward:** this build loads each release's files and saves them again. Every old value must
  arrive unchanged at the same key, or at its new key per `"moved"`, or lie in an area a major
  release declared under `"major"`. Anything else fails, and the message names the setting.
- **Rollback (release only):** `scripts/dev/compat-rollback.ps1` builds the previous release and
  lets its own loaders read the files this build writes. Every value it knows must arrive, and its
  save must keep the keys it does not know.
- The checker is `compat/check.rs`; its own unit tests prove that a break outside a declared area
  fails and that a break in a minor release fails.

The fixtures contain no personal data: no user names, real paths, audio device names or monitor
ids. Use example values.

### When you change a setting

- **Add a setting:** nothing to do for old files (they load with the default). Give it a non-default
  value in the next release's fixture.
- **Rename or move a setting:** keep reading the old key and map it in the loader (see the
  migrations in `recall-hdr/src/config.rs`, `load_bytes`). Add a `"moved"` entry:

  ```json
  { "file": "config.toml", "from": "recording.fps", "to": "video.fps" }
  ```

  When the value changes shape too, write a test for the mapping and name it instead of `to`:
  `"checked_by": "record_hotkeys::tests::legacy_save_hotkey_becomes_a_preset"`.
- **Reset a setting in a major release:** add a `"major"` entry and say why in the release notes:

  ```json
  { "version": "2.0.0", "file": "config.toml",
    "areas": ["recording.audio_sources", "recording.app_audio"],
    "note": "The new mixer stores sources per track; saved audio sources start fresh." }
  ```

  The tests refuse an entry whose version is not x.0.0, that names no area, or that has no note.
  The reset is allowed only for files written before that version, and only once the build is that
  version or later. A `2.0.0` entry makes a `1.x` release fail its compatibility step.

### At each release

1. The release workflow runs the compatibility tests with `RECALL_VERSION` set, then the rollback
   check against the previous release.
2. After the release, add `compat/settings/v<version>/`: build the release and let it write its own
   files (`scripts/dev/compat-fixture.ps1 -Tag v<version>`), then commit them.
3. Copy every `"major"` entry for this version into the release notes.

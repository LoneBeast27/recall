# Changelog

Changes to Recall for Windows that you can see or feel. Older releases are described on the
[releases page](https://github.com/LoneBeast27/recall/releases).

## 0.1.5

### New

- **Upscaled export.** A clip recorded below its game's own resolution (for example at 720p
  from a 1440p game) can be exported larger, up to the game's resolution, from **Advanced** on
  the export page. Recall picks the upscaler on your graphics card when you export:
  - **NVIDIA RTX Video Super Resolution** on RTX cards, when it is turned on in the NVIDIA app
    (System › Video › RTX Video Enhancement › Super resolution). Recall never changes that
    switch; the export page tells you where it is when it is off.
  - **AMD FidelityFX Super Resolution 1** otherwise, and for clips smaller than 640×360.
  - A plain resize if neither can run.

  An upscaled export wider or taller than 4096 pixels is saved as HEVC (H.264 stops at 4096);
  the export page says so, because some players and sites do not play HEVC.
- **Report a problem** (Settings › General). Describe what went wrong and Recall saves a zip on
  your PC; if you choose, it also opens a new GitHub issue in your browser with your description,
  and you attach the zip yourself. Recall uploads nothing. The zip can hold:
  - the **action trace**: what Recall did and what failed, in fixed words and numbers;
  - **crash records** Recall keeps when it closes unexpectedly;
  - a **settings summary** of numbers and switches only;
  - up to 6 screenshots or clips you add with **+ Add media** (PNG, JPEG or MP4, 20 MB each).
    Their file metadata, such as location, dates and device names, is removed and they are
    renamed. The picture and sound are kept as they are, so whatever they show or say stays.

  The trace, crash records and summary never hold paths, window titles, or clip, game, program,
  device or user names. A confirm step lists every file and its size before anything is saved.
  The trace and crash records stay in `%LOCALAPPDATA%\Recall\trace\` on your PC.
- **Recording resolution** (Settings › Recording › Resolution). **Native** records each game at
  its own size and shape. A lower choice (1440p, 1080p or 720p) keeps the game's shape, so a
  16:10 or ultrawide game is never squeezed or padded. Choices above your tallest screen are not
  offered.

### Improved

- **Exports use the graphics card.** An export now also decodes and resizes on the graphics card,
  not only encodes there, so it uses far less processor time. Downscaled exports are sharper. A
  clip the graphics card cannot decode is exported the previous way automatically.
- **Export page layout.** The options you change for each export come first; the ones you set
  once are under **Advanced**.
- **Smooth motion with a size limit.** With a size limit and **Smooth motion**, Recall keeps the
  picture at 720p or more and lowers the frame rate first.
- **Export resolution hints.** Export resolutions above the recording's resolution are greyed
  out in Settings, and the editor highlights the resolution in effect when the recording holds
  it down.
- **Minecraft.** Minecraft: Java Edition is recognised by its game window, not only its title,
  and every Minecraft window is named "Minecraft", so all its clips share one folder.

### Fixed

- **No more black bars on windowed games.** A windowed game is recorded at its own size instead
  of being placed on a 1440p canvas.
- **Very wide recordings.** Frames wider than the encoder takes are fitted to its limit, also
  after a fallback to a different codec, instead of failing.
- **Clip frame rate.** Each clip stores the rate it was recorded at, so a clip whose frame rate
  dipped during play no longer reads as about 50 fps and no longer caps its export below the
  recorded rate. Lossless exports keep this rate.
- **Space bar in text boxes.** The space bar types a space again in clip titles, search, the
  report description and audio source names.

### Updating

Run `RecallSetup.exe`, or accept the update an installed Recall offers. Your settings are kept,
and settings from every earlier version load unchanged. During the update, a recording in
progress is stopped and saved.

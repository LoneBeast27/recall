use std::io::Cursor;

use super::file_tag_of;

fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

/// `meta` as FFmpeg writes it with `use_metadata_tags` (`full`: version/flags first).
fn meta(tags: &[(&str, &str)], full: bool) -> Vec<u8> {
    let mut keys = 0u32.to_be_bytes().to_vec();
    keys.extend_from_slice(&(tags.len() as u32).to_be_bytes());
    let mut ilst = Vec::new();
    for (index, (key, value)) in tags.iter().enumerate() {
        let mut entry = ((key.len() + 8) as u32).to_be_bytes().to_vec();
        entry.extend_from_slice(b"mdta");
        entry.extend_from_slice(key.as_bytes());
        keys.extend_from_slice(&entry);
        let mut data = vec![0, 0, 0, 1, 0, 0, 0, 0];
        data.extend_from_slice(value.as_bytes());
        ilst.extend_from_slice(&boxed(&(index as u32 + 1).to_be_bytes(), &boxed(b"data", &data)));
    }
    let mut payload = if full { vec![0; 4] } else { Vec::new() };
    payload.extend_from_slice(&boxed(b"hdlr", &[0, 0, 0, 0, 0, 0, 0, 0, b'm', b'd', b't', b'a', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
    payload.extend_from_slice(&boxed(b"keys", &keys));
    payload.extend_from_slice(&boxed(b"ilst", &ilst));
    boxed(b"meta", &payload)
}

/// A file: ftyp, a stand-in mdat, then `moov` with `inner`.
fn file(inner: &[u8]) -> Cursor<Vec<u8>> {
    let mut out = boxed(b"ftyp", b"isomiso2");
    out.extend_from_slice(&boxed(b"mdat", &[7; 64]));
    let mut moov = boxed(b"mvhd", &[0; 100]);
    moov.extend_from_slice(inner);
    out.extend_from_slice(&boxed(b"moov", &moov));
    Cursor::new(out)
}

const TAGS: [(&str, &str); 3] = [("recall_native_height", "1440"), ("recall_audio_sources", "[\"game\"]"), ("encoder", "Lavf61.7.100")];

#[test]
fn reads_a_tag_from_udta_meta_as_ffmpeg_writes_it() {
    let mut clip = file(&boxed(b"udta", &meta(&TAGS, true)));
    assert_eq!(file_tag_of(&mut clip, "recall_native_height").as_deref(), Some("1440"));
    assert_eq!(file_tag_of(&mut clip, "recall_audio_sources").as_deref(), Some("[\"game\"]"));
    assert_eq!(file_tag_of(&mut clip, "encoder").as_deref(), Some("Lavf61.7.100"));
}

#[test]
fn reads_a_meta_box_straight_under_moov_without_version_bytes() {
    let mut clip = file(&meta(&TAGS, false));
    assert_eq!(file_tag_of(&mut clip, "recall_native_height").as_deref(), Some("1440"));
}

#[test]
fn a_missing_tag_or_index_reads_as_none() {
    let mut clip = file(&boxed(b"udta", &meta(&TAGS[1..], true)));
    assert_eq!(file_tag_of(&mut clip, "recall_native_height"), None);
    assert_eq!(file_tag_of(&mut file(&[]), "recall_native_height"), None);
}

#[test]
fn a_cut_off_file_reads_as_none() {
    let whole = file(&boxed(b"udta", &meta(&TAGS, true))).into_inner();
    for cut in [whole.len() - 40, 120] {
        let mut clip = Cursor::new(whole[..cut].to_vec());
        assert_eq!(file_tag_of(&mut clip, "encoder"), None, "cut at {cut}");
    }
}

#[test]
fn an_oversized_key_entry_ends_the_search() {
    let mut keys = vec![0, 0, 0, 0, 0, 0, 0, 1];
    keys.extend_from_slice(&(64u32 << 20).to_be_bytes()); // a 64 MiB "key"
    keys.extend_from_slice(b"mdta");
    let mut meta = vec![0; 4];
    meta.extend_from_slice(&boxed(b"keys", &keys));
    let mut clip = file(&boxed(b"udta", &boxed(b"meta", &meta)));
    assert_eq!(file_tag_of(&mut clip, "recall_native_height"), None);
}

use super::*;
#[test] fn library_dir_from_uses_the_recordings_folder_and_falls_back_when_blank(){
    if std::env::var_os("RECALL_OUT_DIR").is_some(){return;} // the env var wins over both cases
    let folders=|r:Option<&str>|Folders{recordings:r.map(str::to_owned),..Folders::default()};
    assert_eq!(library_dir_from(&folders(Some(r"E:\Recall\Raw"))),PathBuf::from(r"E:\Recall\Raw"));
    let default=crate::services::library::folders::default_recordings_dir();
    assert!(default.is_absolute()&&default.ends_with("Recall"),"{default:?}");
    assert_eq!(library_dir_from(&folders(Some("  "))),default);
    assert_eq!(library_dir_from(&folders(None)),default);
}
#[test] fn recall_two_track_names_are_only_applied_to_matching_generated_sources(){
    let mut info=ClipInfo{path:"C:/clips/VALORANT__2026-10-17__22-34-31-275__3d260146faca4115835ba29741d71fe3.mp4".into(),duration:1.0,width:1,height:1,video_fps:60.0,source_bytes:1,source_modified_ms:1,audio:vec![crate::edit_model::AudioTrackInfo{stream_index:1,name:"und".into(),channels:2,sample_rate:48_000,preview_path:String::new(),waveform:Vec::new(),source:String::new()},crate::edit_model::AudioTrackInfo{stream_index:2,name:"und".into(),channels:1,sample_rate:48_000,preview_path:String::new(),waveform:Vec::new(),source:String::new()}],keyframes:Vec::new(),..Default::default()};
    normalize_audio_names(&mut info);assert_eq!(info.audio[0].name,"Game audio");assert_eq!(info.audio[1].name,"Microphone");
    info.path="C:/clips/imported.mp4".into();info.audio[0].name="und".into();info.audio[1].name="unknown".into();normalize_audio_names(&mut info);assert_eq!(info.audio[0].name,"Audio track 1");assert_eq!(info.audio[1].name,"Audio track 2");
    assert_eq!(storage::recorder_thumb_stem("my-hyphenated-game__2026-10-17__22-34-31-275__abcdef0123456789-1783463424-thumb.jpg"),Some("my-hyphenated-game__2026-10-17__22-34-31-275__abcdef0123456789"));
    assert_eq!(storage::recorder_thumb_stem("ordinary-name-thumb.jpg"),None);
}
#[test] fn short_track_labels_stay_and_language_codes_do_not(){
    for label in ["Mic","mic","MIC","SFX","Bgm","Sub","Tom"]{assert!(!generic_audio_name(label),"{label}");}
    for code in ["eng","en","fre","jpn","ENG","und"," eng ","","a"]{assert!(generic_audio_name(code),"{code}");}
    let track=|name:&str,stream_index|crate::edit_model::AudioTrackInfo{stream_index,name:name.into(),channels:2,sample_rate:48_000,preview_path:String::new(),waveform:Vec::new(),source:String::new()};
    let mut info=ClipInfo{path:"E:/clips/VALORANT__2025-03-01__20-10-11.mp4".into(),duration:1.0,width:1,height:1,video_fps:60.0,source_bytes:1,source_modified_ms:1,audio:vec![track("eng",1),track("Game",2),track("Mic",3)],keyframes:Vec::new(),..Default::default()};
    normalize_audio_names(&mut info);
    assert_eq!(info.audio.iter().map(|a|a.name.as_str()).collect::<Vec<_>>(),["Audio track 1","Game","Mic"]);
}
#[test] fn reads_marks_sidecar_and_ignores_garbage(){
    let dir=std::env::temp_dir().join(format!("recall-marks-{}",std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let clip=dir.join("VALORANT__2026-10-22__01-02-03.mp4");
    std::fs::write(dir.join("VALORANT__2026-10-22__01-02-03.recall-marks.json"),r#"{"version":1,"marks":[42.5,3.0,"x",-1]}"#).unwrap();
    assert_eq!(read_marks(&clip.to_string_lossy()),vec![3.0,42.5]);
    assert!(read_marks(&dir.join("none.mp4").to_string_lossy()).is_empty());
    let _=std::fs::remove_dir_all(&dir);
}
#[test] fn only_the_stamped_marker_says_what_an_engine_can_do(){
    let dir=std::env::temp_dir().join(format!("recall-engine-{}",std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (old,prepare,new)=(dir.join("old.exe"),dir.join("prepare.exe"),dir.join("new.exe"));
    std::fs::write(&old,b"MZ recorder --prepare-editor --export-edit").unwrap();
    std::fs::write(&prepare,b"MZ recall-engine-caps:prepare-editor; rest").unwrap();
    std::fs::write(&new,b"MZ \0recall-engine-caps:prepare-editor,export-edit;\0 rest").unwrap();
    assert!(!engine_knows(&old,"prepare-editor").unwrap(),"flag names in a program file prove nothing");
    assert!(!engine_knows(&old,"export-edit").unwrap());
    assert!(engine_knows(&prepare,"prepare-editor").unwrap());
    assert!(!engine_knows(&prepare,"export-edit").unwrap());
    assert!(engine_knows(&new,"prepare-editor").unwrap());
    assert!(engine_knows(&new,"export-edit").unwrap());
    assert!(!engine_knows(&new,"prepare").unwrap(),"a whole capability, not a prefix");
    let _=std::fs::remove_dir_all(&dir);
}
#[test] fn a_marker_with_no_end_counts_for_nothing(){
    assert!(stamped_caps(b"recall-engine-caps:prepare-editor").is_empty());
    // A newer engine adds caps after the old ones: an app that needs only the old ones still finds them.
    let caps = stamped_caps(b"MZ recall-engine-caps:prepare-editor,export-edit,upscale-notice;");
    assert_eq!(caps, vec![&b"prepare-editor"[..], &b"export-edit"[..], &b"upscale-notice"[..]]);
}
#[test] fn an_export_has_an_end_that_grows_with_the_work(){
    let range=|start:f64,end:f64,speed:f64|{let mut r=crate::edit_model::TimeRange::new(start,end);r.speed=speed;r};
    let plan=|ranges|EditPlan{ranges,audio:Vec::new(),mix_down:None};
    assert_eq!(export_limit(&plan(vec![]),60),Duration::from_secs(600));
    assert_eq!(export_limit(&plan(vec![range(0.0,60.0,1.0)]),60),Duration::from_secs(1800));
    assert_eq!(export_limit(&plan(vec![range(0.0,60.0,0.25)]),60),Duration::from_secs(5400),"a slowed part is longer");
    assert_eq!(export_limit(&plan(vec![range(0.0,60.0,1.0)]),240),Duration::from_secs(5400),"more frames");
}
#[test] fn a_lost_publish_race_is_retried_and_other_failures_are_not(){
    assert!(lost_publish_race("prepare media: prepare editor: publish cache: The directory is not empty. (os error 145)"));
    assert!(!lost_publish_race("prepare media: open clip.mp4: Invalid data found when processing input"));
}
#[test] fn only_real_edits_count_as_non_default_plans(){
    let info=ClipInfo{path:"C:/clips/a.mp4".into(),duration:10.0,width:1,height:1,video_fps:60.0,source_bytes:1,source_modified_ms:1,audio:vec![crate::edit_model::AudioTrackInfo{stream_index:1,name:"Game audio".into(),channels:2,sample_rate:48_000,preview_path:String::new(),waveform:Vec::new(),source:String::new()}],keyframes:Vec::new(),..Default::default()};
    let untouched=EditPlan{mix_down:None,ranges:vec![crate::edit_model::TimeRange::new(0.0,10.0)],audio:vec![crate::edit_model::AudioSetting{stream_index:1,gain_db:0.0,muted:false,mix:true}]};
    assert!(plan_is_default(&untouched,&info));
    let mut trimmed=untouched.clone();trimmed.ranges[0].start=1.5;assert!(!plan_is_default(&trimmed,&info));
    let mut split=untouched.clone();split.ranges=vec![crate::edit_model::TimeRange::new(0.0,4.0),crate::edit_model::TimeRange::new(4.0,10.0)];assert!(!plan_is_default(&split,&info));
    let mut muted=untouched.clone();muted.audio[0].muted=true;assert!(!plan_is_default(&muted,&info));
}
/// F-editor-export-4: an export or save above the largest preset is refused; saved edits from
/// before the cap still load (the editor caps them).
#[test] fn export_rates_past_the_largest_preset_are_refused(){
    let max=crate::editor_state::MAX_EXPORT_FPS;
    assert!(validate_fps(max).is_ok());
    assert!(validate_fps(max+1).is_err());
    assert!(validate_fps(9_999_999).is_err());
    assert!(validate_fps(0).is_err());
}

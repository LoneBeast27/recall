//! The `--export-edit` project file and the result an edited export reports.

use serde::{Deserialize, Serialize};

use super::ExportResult;
use crate::edit_model::{EditPlan, ExportMode, ExportSettings};

#[derive(Deserialize)]
struct ProjectFile {
    plan: EditPlan,
    #[serde(default)]
    export: ExportSettings,
}

/// Read a project file: a bare [`EditPlan`] (written before export settings
/// existed), or any object with `plan` and optional `export` keys, such as the
/// editor's saved project (`version`, `source_*`, `export_fps` are ignored here).
pub fn parse_project(text: &str) -> Result<(EditPlan, ExportSettings), String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("editor project is not JSON: {e}"))?;
    if value.get("plan").is_some() {
        let project: ProjectFile =
            serde_json::from_value(value).map_err(|e| format!("editor project: {e}"))?;
        return Ok((project.plan, project.export));
    }
    let plan: EditPlan =
        serde_json::from_value(value).map_err(|e| format!("editor project plan: {e}"))?;
    Ok((plan, ExportSettings::default()))
}

/// One range as it landed in the output. For lossless exports `start` is the
/// keyframe the cut snapped to and `end` the end of the last copied frame;
/// ranges whose snapped starts overlap are merged (`plan_ranges` lists them).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ExportedRange {
    pub start: f64,
    pub end: f64,
    pub speed: f64,
    pub output_start: f64,
    pub output_end: f64,
    pub plan_ranges: Vec<usize>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EditedExport {
    /// The re-encode ran on the GPU path (`None` for lossless).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu: Option<bool>,
    /// Why a re-encode ran on the CPU path instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_reason: Option<String>,
    pub mode: ExportMode,
    pub frames: u64,
    pub seconds: f64,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    /// Output frame rate; `None` for lossless (source timing is kept).
    pub fps: Option<u32>,
    /// Average video bitrate requested from the encoder (re-encode only).
    pub video_bitrate: Option<u64>,
    pub max_bytes: Option<u64>,
    /// Encodes it took to land under `max_bytes` (1 without a cap).
    pub attempts: u32,
    pub ranges: Vec<ExportedRange>,
}

impl EditedExport {
    pub fn summary(&self) -> ExportResult {
        ExportResult {
            frames: self.frames,
            seconds: self.seconds,
        }
    }

    /// The `{"type":"export_report",...}` JSON line printed beside `exported`.
    pub fn report_line(&self, source: &str, path: &str) -> String {
        #[derive(Serialize)]
        struct Line<'a> {
            #[serde(rename = "type")]
            kind: &'static str,
            source: &'a str,
            path: &'a str,
            #[serde(flatten)]
            report: &'a EditedExport,
        }
        serde_json::to_string(&Line {
            kind: "export_report",
            source,
            path,
            report: self,
        })
        .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit_model::TimeRange;

    #[test]
    fn bare_plans_and_saved_projects_both_load() {
        let (plan, export) =
            parse_project(r#"{"ranges":[{"start":0.5,"end":2.0}],"audio":[]}"#).unwrap();
        assert_eq!(plan.ranges, vec![TimeRange::new(0.5, 2.0)]);
        assert_eq!(export, ExportSettings::default());

        let saved = r#"{"version":1,"source_path":"C:/a.mp4","source_bytes":1,"source_modified_ms":1,
            "plan":{"ranges":[{"start":0.0,"end":1.0}]},"export_fps":60}"#;
        let (plan, export) = parse_project(saved).unwrap();
        assert_eq!(plan.ranges[0].speed, 1.0);
        assert_eq!(export.mode, ExportMode::Reencode);

        let with_export = r#"{"plan":{"ranges":[{"start":0.0,"end":1.0,"speed":0.5}]},
            "export":{"mode":"lossless","max_bytes":25000000,"height":720}}"#;
        let (plan, export) = parse_project(with_export).unwrap();
        assert_eq!(plan.ranges[0].speed, 0.5);
        assert_eq!(export.mode, ExportMode::Lossless);
        assert_eq!(export.max_bytes, Some(25_000_000));
        assert_eq!(export.height, Some(720));
    }

    #[test]
    fn report_line_is_tagged_and_flat() {
        let report = EditedExport {
            gpu: None,
            cpu_reason: None,
            mode: ExportMode::Lossless,
            frames: 3,
            seconds: 0.05,
            bytes: 10,
            width: 4,
            height: 2,
            fps: None,
            video_bitrate: None,
            max_bytes: None,
            attempts: 1,
            ranges: vec![],
        };
        let value: serde_json::Value =
            serde_json::from_str(&report.report_line("in.mp4", "out.mp4")).unwrap();
        assert_eq!(value["type"], "export_report");
        assert_eq!(value["mode"], "lossless");
        assert_eq!(value["path"], "out.mp4");
        assert!(value["fps"].is_null());
    }
}

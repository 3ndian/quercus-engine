//! SRT subtitle import/export built on the text generator.
//!
//! Phase 1 keeps subtitle cues as ordinary Text clips on a video track: the
//! existing text renderer already previews and burns them in, and the
//! existing clip commands give undo/serialization for free. A dedicated
//! subtitle-block track is a later phase.

use std::path::Path;

use super::prelude::*;
use super::{
	generators::place_text_clip,
	project::{create_text_footage_node, lock, track_behavior},
	sequence::{clip_range, rational_to_ts, sequence_time_base, track_ids},
};
use quercus_codec::subtitles::{parse_srt, write_srt, SubtitleCue};
use quercus_node::block::clip_input;

/// Import every SRT cue as a Text clip on `track_index` (one undo group).
/// Returns the number of cues imported.
pub fn import_srt(
	project: &ProjectRef,
	sequence: NodeId,
	track_index: usize,
	path: &Path,
) -> Result<usize, String> {
	let bytes = std::fs::read(path)
		.map_err(|e| format!("failed to read \"{}\": {e}", path.display()))?;
	let cues = parse_srt(&bytes)?;
	if cues.is_empty() {
		return Ok(0);
	}
	let time_base = {
		let guard = lock(project);
		sequence_time_base(&guard.graph, sequence)
			.ok_or_else(|| "the sequence has no valid frame rate".to_string())?
	};
	quercus_undo::global::group_begin("Import Subtitles")
		.map_err(|e| e.to_string())?;
	let result = (|| -> Result<usize, String> {
		for cue in &cues {
			let in_ts = rational_to_ts(Rational::from_double(cue.in_seconds), time_base).max(0);
			let out_ts =
				rational_to_ts(Rational::from_double(cue.out_seconds), time_base).max(in_ts + 1);
			let text = create_text_footage_node(project)?;
			// The text node's visible input is plain text; set it as one
			// undoable parameter edit inside the import group.
			super::parameters::set_text_value(
				project,
				text,
				"plain_text_in",
				-1,
				&cue.text,
			)?;
			place_text_clip(project, sequence, text, track_index, in_ts, out_ts)?;
		}
		Ok(cues.len())
	})();
	match &result {
		Ok(_) => quercus_undo::global::group_end().map_err(|e| e.to_string())?,
		Err(_) => {
			let _ = quercus_undo::global::group_abort();
		}
	}
	result
}

fn text_value(graph: &Graph, node: NodeId) -> Option<String> {
	let entry = graph.get(node)?;
	for input in [
		"plain_text_in",
		"text_in",
	] {
		if let quercus_node::value::NodeValue::Text(text) =
			&entry.core.standard_value(input, -1)
		{
			if !text.is_empty() {
				return Some(text.clone());
			}
		}
	}
	None
}

/// Export every Text clip on the sequence's video tracks to an SRT file.
/// Returns the number of cues written.
pub fn export_srt(project: &ProjectRef, sequence: NodeId, path: &Path) -> Result<usize, String> {
	let mut cues = Vec::new();
	{
		let guard = lock(project);
		for track in track_ids(&guard.graph, sequence, TrackType::Video) {
			let Some(track_behavior) = track_behavior(&guard.graph, track) else {
				continue;
			};
			for block in &track_behavior.blocks {
				let Some((in_r, out_r, _)) = clip_range(&guard.graph, *block) else {
					continue;
				};
				let Some(source) =
					guard.graph.connected_output(*block, clip_input::TEXTURE_INPUT, -1)
				else {
					continue;
				};
				let Some(text) = text_value(&guard.graph, source) else {
					continue;
				};
				cues.push(SubtitleCue {
					in_seconds: in_r.to_f64(),
					out_seconds: out_r.to_f64(),
					text,
				});
			}
		}
	}
	cues.sort_by(|a, b| {
		a.in_seconds
			.total_cmp(&b.in_seconds)
			.then(a.out_seconds.total_cmp(&b.out_seconds))
	});
	let text = write_srt(&cues);
	std::fs::write(path, text)
		.map_err(|e| format!("failed to write \"{}\": {e}", path.display()))?;
	Ok(cues.len())
}

#[cfg(test)]
mod tests {
	use super::*;
	use quercus_node::project::Project;
	use quercus_node::sequence::SequenceBehavior;

	#[test]
	fn srt_import_and_export_roundtrip_via_text_clips() {
		let project = Project::new();
		project.lock().unwrap().initialize().unwrap();
		let sequence = {
			let (core, behavior) = SequenceBehavior::create();
			project.lock().unwrap().graph.add_node(core, behavior)
		};
		super::super::tracks::add_track(&project, sequence, TrackType::Video)
			.expect("video track");
		let path = std::env::temp_dir().join(format!(
			"quercus_subtitle_import_{}.srt",
			std::process::id()
		));
		std::fs::write(
			&path,
			"1\n00:00:01,000 --> 00:00:02,500\n你好\n第二行\n\n2\n00:00:03,000 --> 00:00:04,000\nSecond\n",
		)
		.unwrap();
		let count = import_srt(&project, sequence, 0, &path).expect("import");
		assert_eq!(count, 2);
		let output = std::env::temp_dir().join(format!(
			"quercus_subtitle_export_{}.srt",
			std::process::id()
		));
		let exported = export_srt(&project, sequence, &output).expect("export");
		assert_eq!(exported, 2);
		let text = std::fs::read_to_string(&output).unwrap();
		assert!(text.contains("你好"));
		assert!(text.contains("Second"));
		let exported_cues = parse_srt(text.as_bytes()).expect("reparse export");
		assert_eq!(exported_cues.len(), 2);
		assert!((exported_cues[0].in_seconds - 1.0).abs() < 0.05);
		assert!((exported_cues[0].out_seconds - 2.5).abs() < 0.05);
		let _ = std::fs::remove_file(path);
		let _ = std::fs::remove_file(output);
	}
}

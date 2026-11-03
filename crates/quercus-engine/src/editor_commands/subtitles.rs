//! SRT subtitle import/export built on the text generator.
//!
//! Phase 1 keeps subtitle cues as ordinary Text clips on a video track: the
//! existing text renderer already previews and burns them in, and the
//! existing clip commands give undo/serialization for free. A dedicated
//! subtitle-block track is a later phase.
//!
//! Styling is applied to the Text clip's direct `text3` generator. The text
//! node only has inputs for font family/size, fill color, outline and
//! vertical alignment; the style's margins and line spacing have no matching
//! input and are carried for API compatibility only. The text node also has
//! no separate outline-enable input, so a style enables the outline whenever
//! [`SubtitleStyle::outline_width`] is greater than zero. `align` is defined
//! as horizontal left/center/right in the C ABI, but because text3 has no
//! horizontal alignment input it is mapped onto its vertical alignment combo
//! (0 = top, 1 = middle, 2 = bottom).

use std::path::Path;

use super::prelude::*;
use super::{
	generators::place_text_clip,
	project::{create_text_footage_node, lock, node_type_id, track_behavior, TEXT_FOOTAGE_TYPE_ID},
	push_multi_command,
	sequence::{clip_range, rational_to_ts, sequence_time_base, track_ids},
};
use quercus_codec::subtitles::{parse_srt, write_srt, SubtitleCue};
use quercus_node::block::clip_input;
use quercus_node::ops::set_value_at_time_command;
use quercus_node::value::NodeValue;

// The text3 input ids are not exported by `quercus_node::nodes::textv3`
// (the module is private), so the generator's stable public ids are repeated
// here. They are part of the serialized project format.
const FONT_FAMILY_INPUT: &str = "font_family_in";
const FONT_SIZE_INPUT: &str = "font_size_in";
const COLOR_INPUT: &str = "color_in";
const OUTLINE_ENABLED_INPUT: &str = "outline_enabled_in";
const OUTLINE_COLOR_INPUT: &str = "outline_color_in";
const OUTLINE_WIDTH_INPUT: &str = "outline_width_in";
const VERTICAL_ALIGNMENT_INPUT: &str = "valign_in";

/// Style applied to subtitle Text clips.
///
/// Values mirror the C ABI `QuercusSubtitleStyle`. The [`Default`] values
/// match the text3 node defaults: no font-family override, 72 pt, opaque
/// white fill, opaque black outline color, and a 2 px outline width. Since
/// the style has no outline-enable flag, a non-zero width enables the
/// outline when the style is applied.
#[derive(Clone, Debug, PartialEq)]
pub struct SubtitleStyle {
	/// Font family name; empty selects the backend default font.
	pub font_family: String,
	/// Font size in points.
	pub font_size: f32,
	/// RGBA fill color.
	pub color: [f64; 4],
	/// RGBA outline color.
	pub outline_color: [f64; 4],
	/// Outline width in points. `0` disables the outline.
	pub outline_width: f32,
	/// Alignment selector (0, 1, 2). Mapped to text3 vertical alignment
	/// because the node has no horizontal alignment input.
	pub align: i32,
	/// Horizontal margin (no matching text3 input; ignored).
	pub margin_h: f32,
	/// Vertical margin (no matching text3 input; ignored).
	pub margin_v: f32,
	/// Line spacing multiplier (no matching text3 input; ignored).
	pub line_spacing: f32,
}

impl Default for SubtitleStyle {
	fn default() -> Self {
		Self {
			font_family: String::new(),
			font_size: 72.0,
			color: [1.0, 1.0, 1.0, 1.0],
			outline_color: [0.0, 0.0, 0.0, 1.0],
			outline_width: 2.0,
			align: 1,
			margin_h: 0.0,
			margin_v: 0.0,
			line_spacing: 1.0,
		}
	}
}

impl SubtitleStyle {
	/// Validates the values that [`apply_subtitle_style`] writes to text3.
	///
	/// Font size must be finite and positive, outline width must be finite and
	/// non-negative, every color channel must be finite, and `align` must be
	/// one of the documented 0/1/2 selectors. The ignored margin and line
	/// spacing values are still required to be finite so a bad style is never
	/// silently accepted.
	pub fn validate(&self) -> Result<(), String> {
		if !self.font_size.is_finite() || self.font_size <= 0.0 {
			return Err(format!(
				"subtitle style: font size must be finite and greater than zero (got {})",
				self.font_size
			));
		}
		if !self.outline_width.is_finite() || self.outline_width < 0.0 {
			return Err(format!(
				"subtitle style: outline width must be finite and non-negative (got {})",
				self.outline_width
			));
		}
		for (name, color) in [
			("color", &self.color),
			("outline color", &self.outline_color),
		] {
			if !color.iter().all(|channel| channel.is_finite()) {
				return Err(format!("subtitle style: {name} must have finite channels"));
			}
		}
		for (name, value) in [
			("margin_h", self.margin_h),
			("margin_v", self.margin_v),
			("line_spacing", self.line_spacing),
		] {
			if !value.is_finite() {
				return Err(format!("subtitle style: {name} must be finite"));
			}
		}
		if !(0..=2).contains(&self.align) {
			return Err(format!(
				"subtitle style: align must be 0, 1 or 2 (got {})",
				self.align
			));
		}
		Ok(())
	}
}

/// The style's `align` selector mapped to text3's vertical alignment combo
/// (C++ `k_v_align_top/middle/bottom` = 0/1/2).
fn vertical_alignment(align: i32) -> i64 {
	match align {
		0 => 0,
		2 => 2,
		_ => 1,
	}
}

/// Builds the one undoable multi command that writes every text3 style input.
///
/// The commands are assembled before any is pushed, so a missing node/input
/// or an unexpected generator type leaves the graph untouched. The caller
/// must have validated `style` already.
fn push_style_commands(
	p: &ProjectRef,
	clip: NodeId,
	style: &SubtitleStyle,
) -> Result<(), String> {
	let commands = {
		let guard = lock(p);
		let source = guard
			.graph
			.connected_output(clip, clip_input::TEXTURE_INPUT, -1)
			.ok_or_else(|| "subtitle style: the clip has no texture source".to_string())?;
		let type_id = node_type_id(&guard.graph, source);
		if type_id != TEXT_FOOTAGE_TYPE_ID {
			return Err(format!(
				"subtitle style: the clip's texture source is not a text generator (found \"{type_id}\")"
			));
		}
		// `set_value_at_time_command` only reads the graph and captures the
		// project; holding the lock while building every command keeps the
		// aggregate consistent. The commands are pushed after the lock drops.
		let time = Rational::new(0, 1);
		let mut commands = Vec::with_capacity(7);
		let values = [
			(
				FONT_FAMILY_INPUT,
				NodeValue::StrCombo(style.font_family.clone()),
			),
			(FONT_SIZE_INPUT, NodeValue::Float(style.font_size as f64)),
			(COLOR_INPUT, NodeValue::Color(style.color)),
			(
				OUTLINE_ENABLED_INPUT,
				NodeValue::Boolean(style.outline_width > 0.0),
			),
			(OUTLINE_COLOR_INPUT, NodeValue::Color(style.outline_color)),
			(
				OUTLINE_WIDTH_INPUT,
				NodeValue::Float(style.outline_width as f64),
			),
			(
				VERTICAL_ALIGNMENT_INPUT,
				NodeValue::Combo(vertical_alignment(style.align)),
			),
		];
		for (input, value) in values {
			commands.push(
				set_value_at_time_command(p, &guard.graph, source, input, -1, time, &value)
					.map_err(|e| format!("subtitle style: cannot set \"{input}\": {e}"))?,
			);
		}
		commands
	};
	push_multi_command(commands, "Subtitle Style")
}

/// Applies `style` to one subtitle Text clip as ONE undo row labelled
/// "Subtitle Style".
///
/// The clip must be a clip whose direct texture source is the text3
/// generator (`org.olivevideoeditor.Olive.text3`); anything else is
/// rejected. If an outer undo group is already open (for example during a
/// styled import) the multi command joins that group instead of creating a
/// second row.
pub fn apply_subtitle_style(
	project: &ProjectRef,
	clip: NodeId,
	style: &SubtitleStyle,
) -> Result<(), String> {
	style.validate()?;
	push_style_commands(project, clip, style)
}

/// Import every SRT cue as a Text clip on `track_index` with the default
/// subtitle style (one undo group). Returns the number of cues imported.
pub fn import_srt(
	project: &ProjectRef,
	sequence: NodeId,
	track_index: usize,
	path: &Path,
) -> Result<usize, String> {
	import_srt_styled(project, sequence, track_index, path, &SubtitleStyle::default())
}

/// Import every SRT cue as a Text clip on `track_index` and apply `style` to
/// each created clip. The whole import is one undo group, so the cue clips
/// and their style edits undo together. Returns the number of cues imported.
///
/// `style` is validated before anything is created; invalid values leave the
/// project and undo stack untouched.
pub fn import_srt_styled(
	project: &ProjectRef,
	sequence: NodeId,
	track_index: usize,
	path: &Path,
	style: &SubtitleStyle,
) -> Result<usize, String> {
	style.validate()?;
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
			let clip = place_text_clip(project, sequence, text, track_index, in_ts, out_ts)?;
			// Joins the open "Import Subtitles" group as a single child.
			push_style_commands(project, clip, style)?;
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
	use std::sync::atomic::{AtomicU64, Ordering};

	static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

	fn test_project() -> (ProjectRef, NodeId) {
		let project = Project::new();
		project.lock().unwrap().initialize().unwrap();
		let sequence = {
			let (core, behavior) = SequenceBehavior::create();
			project.lock().unwrap().graph.add_node(core, behavior)
		};
		super::super::tracks::add_track(&project, sequence, TrackType::Video)
			.expect("video track");
		(project, sequence)
	}

	fn temp_srt(tag: &str, contents: &str) -> std::path::PathBuf {
		let n = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
		let path = std::env::temp_dir().join(format!(
			"quercus_subtitle_{tag}_{}_{n}.srt",
			std::process::id()
		));
		std::fs::write(&path, contents).unwrap();
		path
	}

	fn track_clips(project: &ProjectRef, sequence: NodeId) -> Vec<NodeId> {
		let guard = lock(project);
		let track = track_ids(&guard.graph, sequence, TrackType::Video)[0];
		super::super::sequence::clip_ids(&guard.graph, track)
	}

	fn text_source(project: &ProjectRef, clip: NodeId) -> Option<NodeId> {
		lock(project)
			.graph
			.connected_output(clip, clip_input::TEXTURE_INPUT, -1)
	}

	fn standard(project: &ProjectRef, node: NodeId, input: &str) -> NodeValue {
		lock(project)
			.graph
			.get(node)
			.expect("node")
			.core
			.standard_value(input, -1)
	}

	fn custom_style() -> SubtitleStyle {
		SubtitleStyle {
			font_family: "DejaVu Sans".to_string(),
			font_size: 48.0,
			color: [1.0, 0.25, 0.0, 1.0],
			outline_color: [0.0, 0.0, 1.0, 1.0],
			outline_width: 3.0,
			align: 2,
			..SubtitleStyle::default()
		}
	}

	#[test]
	fn styled_import_applies_font_color_outline_and_alignment() {
		let _g = crate::editor_commands::test_lock();
		quercus_undo::global::clear().unwrap();
		let (project, sequence) = test_project();
		let path = temp_srt("styled", "1\n00:00:01,000 --> 00:00:02,000\nStyled\n");
		let count = import_srt_styled(&project, sequence, 0, &path, &custom_style())
			.expect("styled import");
		assert_eq!(count, 1);
		let clips = track_clips(&project, sequence);
		assert_eq!(clips.len(), 1);
		let source = text_source(&project, clips[0]).expect("text source");
		{
			let guard = lock(&project);
			assert_eq!(node_type_id(&guard.graph, source), TEXT_FOOTAGE_TYPE_ID);
		}
		assert_eq!(
			standard(&project, source, FONT_FAMILY_INPUT),
			NodeValue::StrCombo("DejaVu Sans".to_string())
		);
		assert_eq!(
			standard(&project, source, FONT_SIZE_INPUT),
			NodeValue::Float(48.0)
		);
		assert_eq!(
			standard(&project, source, COLOR_INPUT),
			NodeValue::Color([1.0, 0.25, 0.0, 1.0])
		);
		assert_eq!(
			standard(&project, source, OUTLINE_ENABLED_INPUT),
			NodeValue::Boolean(true)
		);
		assert_eq!(
			standard(&project, source, OUTLINE_COLOR_INPUT),
			NodeValue::Color([0.0, 0.0, 1.0, 1.0])
		);
		assert_eq!(
			standard(&project, source, OUTLINE_WIDTH_INPUT),
			NodeValue::Float(3.0)
		);
		assert_eq!(
			standard(&project, source, VERTICAL_ALIGNMENT_INPUT),
			NodeValue::Combo(2)
		);
		let _ = std::fs::remove_file(path);
	}

	#[test]
	fn set_subtitle_style_is_one_undo_row_and_restores_previous_values() {
		let _g = crate::editor_commands::test_lock();
		quercus_undo::global::clear().unwrap();
		let (project, sequence) = test_project();
		let path = temp_srt("undo", "1\n00:00:00,000 --> 00:00:01,000\nUndo\n");
		import_srt(&project, sequence, 0, &path).expect("import");
		let clip = track_clips(&project, sequence)[0];
		let source = text_source(&project, clip).expect("text source");
		let before = (
			standard(&project, source, FONT_FAMILY_INPUT),
			standard(&project, source, FONT_SIZE_INPUT),
			standard(&project, source, COLOR_INPUT),
			standard(&project, source, OUTLINE_ENABLED_INPUT),
			standard(&project, source, OUTLINE_COLOR_INPUT),
			standard(&project, source, OUTLINE_WIDTH_INPUT),
			standard(&project, source, VERTICAL_ALIGNMENT_INPUT),
		);
		apply_subtitle_style(&project, clip, &custom_style()).expect("apply style");
		assert_eq!(
			standard(&project, source, FONT_SIZE_INPUT),
			NodeValue::Float(48.0)
		);
		assert_eq!(
			standard(&project, source, VERTICAL_ALIGNMENT_INPUT),
			NodeValue::Combo(2)
		);
		let last_row = quercus_undo::global::count().unwrap() - 1;
		assert_eq!(
			quercus_undo::global::command_name(last_row).unwrap(),
			"Subtitle Style",
			"all style fields share one undo row"
		);
		quercus_undo::global::undo().expect("undo style");
		let after = (
			standard(&project, source, FONT_FAMILY_INPUT),
			standard(&project, source, FONT_SIZE_INPUT),
			standard(&project, source, COLOR_INPUT),
			standard(&project, source, OUTLINE_ENABLED_INPUT),
			standard(&project, source, OUTLINE_COLOR_INPUT),
			standard(&project, source, OUTLINE_WIDTH_INPUT),
			standard(&project, source, VERTICAL_ALIGNMENT_INPUT),
		);
		assert_eq!(after, before, "undo restores every style field");
		quercus_undo::global::redo().expect("redo style");
		assert_eq!(
			standard(&project, source, FONT_SIZE_INPUT),
			NodeValue::Float(48.0)
		);
		let _ = std::fs::remove_file(path);
	}

	#[test]
	fn style_application_rejects_non_text_clip_without_panicking() {
		let _g = crate::editor_commands::test_lock();
		quercus_undo::global::clear().unwrap();
		let (project, _sequence) = test_project();
		let clip = {
			let mut guard = lock(&project);
			let (core, behavior) = quercus_node::block::clip_create();
			guard.graph.add_node(core, behavior)
		};
		// A clip with no source is not a subtitle text clip.
		assert!(apply_subtitle_style(&project, clip, &SubtitleStyle::default()).is_err());
		// A clip fed by a non-text generator is rejected too.
		let solid = {
			let (core, behavior) = quercus_node::factory::Factory::global()
				.create_any("org.olivevideoeditor.Olive.solidgenerator")
				.expect("solid generator registered");
			lock(&project).graph.add_node(core, behavior)
		};
		lock(&project)
			.graph
			.connect(solid, clip, clip_input::TEXTURE_INPUT, -1)
			.expect("connect solid");
		assert!(apply_subtitle_style(&project, clip, &SubtitleStyle::default()).is_err());
		// A non-clip node id is rejected, not dereferenced.
		assert!(apply_subtitle_style(&project, solid, &SubtitleStyle::default()).is_err());
	}

	#[test]
	fn invalid_style_values_are_rejected_before_creating_clips() {
		let _g = crate::editor_commands::test_lock();
		quercus_undo::global::clear().unwrap();
		let (project, sequence) = test_project();
		let path = temp_srt("invalid", "1\n00:00:00,000 --> 00:00:01,000\nNope\n");
		let invalid = [
			SubtitleStyle {
				font_size: 0.0,
				..SubtitleStyle::default()
			},
			SubtitleStyle {
				font_size: -4.0,
				..SubtitleStyle::default()
			},
			SubtitleStyle {
				font_size: f32::NAN,
				..SubtitleStyle::default()
			},
			SubtitleStyle {
				outline_width: -1.0,
				..SubtitleStyle::default()
			},
			SubtitleStyle {
				outline_width: f32::INFINITY,
				..SubtitleStyle::default()
			},
			SubtitleStyle {
				color: [f64::NAN, 1.0, 1.0, 1.0],
				..SubtitleStyle::default()
			},
			SubtitleStyle {
				outline_color: [0.0, f64::INFINITY, 0.0, 1.0],
				..SubtitleStyle::default()
			},
			SubtitleStyle {
				align: 3,
				..SubtitleStyle::default()
			},
		];
		for style in &invalid {
			assert!(style.validate().is_err(), "style should be invalid: {style:?}");
			assert!(
				import_srt_styled(&project, sequence, 0, &path, style).is_err(),
				"invalid style must fail before import: {style:?}"
			);
		}
		assert!(
			track_clips(&project, sequence).is_empty(),
			"invalid styles must not create clips"
		);
		let _ = std::fs::remove_file(path);
	}

	#[test]
	fn srt_export_roundtrips_text_and_time_after_styling() {
		let _g = crate::editor_commands::test_lock();
		quercus_undo::global::clear().unwrap();
		let (project, sequence) = test_project();
		let path = temp_srt(
			"roundtrip",
			"1\n00:00:01,000 --> 00:00:02,500\n你好\n第二行\n\n2\n00:00:03,000 --> 00:00:04,000\nStyled export\n",
		);
		assert_eq!(
			import_srt_styled(&project, sequence, 0, &path, &custom_style()).expect("styled import"),
			2
		);
		let output = temp_srt("roundtrip_out", "");
		let exported = export_srt(&project, sequence, &output).expect("export");
		assert_eq!(exported, 2);
		let text = std::fs::read_to_string(&output).unwrap();
		assert!(text.contains("你好"));
		assert!(text.contains("Styled export"));
		assert!(
			!text.contains("DejaVu Sans"),
			"SRT carries text and timing, not style"
		);
		let cues = parse_srt(text.as_bytes()).expect("reparse export");
		assert_eq!(cues.len(), 2);
		assert!(cues[0].text.contains("第二行"));
		assert!((cues[0].in_seconds - 1.0).abs() < 0.05);
		assert!((cues[0].out_seconds - 2.5).abs() < 0.05);
		assert!((cues[1].in_seconds - 3.0).abs() < 0.05);
		assert!((cues[1].out_seconds - 4.0).abs() < 0.05);
		let _ = std::fs::remove_file(path);
		let _ = std::fs::remove_file(output);
	}

	#[test]
	fn existing_unscoped_import_and_export_still_roundtrip() {
		let _g = crate::editor_commands::test_lock();
		quercus_undo::global::clear().unwrap();
		let project = Project::new();
		project.lock().unwrap().initialize().unwrap();
		let sequence = {
			let (core, behavior) = SequenceBehavior::create();
			project.lock().unwrap().graph.add_node(core, behavior)
		};
		super::super::tracks::add_track(&project, sequence, TrackType::Video)
			.expect("video track");
		let path = temp_srt(
			"legacy",
			"1\n00:00:01,000 --> 00:00:02,500\n你好\n第二行\n\n2\n00:00:03,000 --> 00:00:04,000\nSecond\n",
		);
		let count = import_srt(&project, sequence, 0, &path).expect("import");
		assert_eq!(count, 2);
		let output = temp_srt("legacy_out", "");
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

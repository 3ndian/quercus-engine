// Quercus Video Editor - Non-Linear Video Editor
// Copyright (C) 2026 Quercus Team
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! Sequence: the C++ `ViewerOutput`/`Sequence` pair — a node that owns
//! tracks, markers, work area, and playback caches.
//! `// CPP-PARITY: src/node/src/output/viewer/viewer.{h,cpp}`,
//! `// CPP-PARITY: src/node/src/project/sequence/sequence.{h,cpp}`.

use crate::id::NodeId;
use crate::input::Input;
use crate::node::{Category, NodeBehavior, NodeCore};
use crate::value::{AudioParams, NodeValue, ValueType, VideoParams};

/// Sequence texture input id (C++ `ViewerOutput::k_texture_input`).
pub const TEXTURE_INPUT: &str = "tex_in";
/// Sequence audio samples input id (C++ `ViewerOutput::k_samples_input`).
pub const SAMPLES_INPUT: &str = "samples_in";
/// Sequence per-track input id format (C++ `Sequence::k_track_input_format`).
pub const TRACK_INPUT_FORMAT: &str = "track_in_%1";

/// Sequence behavior (viewer node).
pub struct SequenceBehavior {
	/// Track list node ids (video then audio, C++ order).
	pub track_lists: Vec<NodeId>,
	/// Timeline markers handle (quercustimeline, owned; created lazily by
	/// the facade through the C ABI).
	pub markers: crate::handle::CHandle,
	/// Work area handle (quercustimeline, owned; created lazily by the
	/// facade through the C ABI).
	pub workarea: crate::handle::CHandle,
	/// Length cache (C++ last_length_).
	pub last_length: quercus_core::Rational,
	/// Autocache toggles.
	pub autocache_video: bool,
	/// Audio autocache toggle.
	pub autocache_audio: bool,
	/// Playhead position (C++ ViewerOutput::playhead_).
	pub playhead: quercus_core::Rational,
	/// Video parameter streams.
	pub video_params: Vec<VideoParams>,
	/// Audio parameter streams.
	pub audio_params: Vec<AudioParams>,
}

impl SequenceBehavior {
	/// Empty sequence with zero tracks and no parameters.
	pub fn new() -> Self {
		SequenceBehavior {
			track_lists: Vec::new(),
			markers: crate::handle::CHandle::null(),
			workarea: crate::handle::CHandle::null(),
			last_length: quercus_core::Rational::new(0, 1),
			autocache_video: false,
			autocache_audio: false,
			playhead: quercus_core::Rational::new(0, 1),
			video_params: Vec::new(),
			audio_params: Vec::new(),
		}
	}

	/// Constructor: the C++ `Sequence` input surface with default
	/// parameters but no track lists (`// CPP-PARITY: sequence.cpp:36`,
	/// `viewer.cpp:84`). Used by the serializer to rebuild a sequence
	/// from a file — the track lists arrive as separate nodes.
	pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
		let mut core = NodeCore::new();
		// Bin item (C++ `sequence.cpp:37` `set_flag(k_is_item)`): nested
		// sequences are shared, never cloned, by dependency-graph copies.
		core.flags |= crate::node::flags::IS_ITEM;
		// Viewer parameter streams (C++ ViewerOutput::kVideoParamsInput /
		// kAudioParamsInput / kSubtitleParamsInput arrays).
		for (id, ty) in [
			("video_param_in", ValueType::VideoParams),
			("audio_param_in", ValueType::AudioParams),
			("subtitle_param_in", ValueType::None),
		] {
			let mut input = Input::new(id, ty, NodeValue::None);
			input.flags |= crate::input::flags::NOT_CONNECTABLE
				| crate::input::flags::NOT_KEYFRAMABLE
				| crate::input::flags::ARRAY
				| crate::input::flags::HIDDEN;
			core.add_input(input);
		}
		core.add_input(Input::new(
			TEXTURE_INPUT,
			ValueType::Texture,
			NodeValue::None,
		));
		core.add_input(Input::new(
			SAMPLES_INPUT,
			ValueType::Samples,
			NodeValue::None,
		));
		// One array input per track list (`track_in_%1`; the video list
		// owns track_in_0, audio track_in_1, subtitle track_in_2 —
		// `// CPP-PARITY: sequence.h`).
		for base in 0..3 {
			let mut track_input = Input::new(
				&TRACK_INPUT_FORMAT.replace("%1", &base.to_string()),
				ValueType::None,
				NodeValue::None,
			);
			track_input.flags |= crate::input::flags::ARRAY;
			core.add_input(track_input);
		}
		let mut behavior = SequenceBehavior::new();
		behavior.set_default_parameters();
		(core, Box::new(behavior))
	}

	/// Apply the default video/audio parameters (C++
	/// `ViewerOutput::set_default_parameters()`; the config lookups read
	/// the quercus_core config store directly).
	pub fn set_default_parameters(&mut self) {
		let config = quercus_core::configstore::ConfigStore::instance();
		let width = config.get_int(None, "DefaultSequenceWidth", 1920);
		let height = config.get_int(None, "DefaultSequenceHeight", 1080);
		let sample_rate = config.get_int(None, "DefaultSequenceAudioFrequency", 48000);
		// The canonical setting is the "num/den" string
		// `DefaultSequenceFrameRate` (a frame DURATION; the rate is
		// den/num, see quercus-task's footage import). The `...Num`/`...Den`
		// keys this used to read never existed anywhere in the config, so
		// every sequence silently fell back to 30/1 — while the app-side
		// playback clock defaulted to 25, and the audio chunks (sized by
		// the sequence time base) chronically underfed the output device.
		let (tb_num, tb_den) = config
			.get(None, "DefaultSequenceFrameRate")
			.ok()
			.and_then(|s| {
				let (n, d) = s.split_once('/')?;
				Some((n.trim().parse::<i64>().ok()?, d.trim().parse::<i64>().ok()?))
			})
			.filter(|(n, d)| *n > 0 && *d > 0)
			.unwrap_or((1001, 30000));

		self.video_params = vec![VideoParams {
			width,
			height,
			frame_rate: quercus_core::Rational::new(tb_den, tb_num),
			pixel_format: 4, // f32
			channels: 4,
			interlaced: false,
		}];
		self.audio_params = vec![AudioParams {
			sample_rate,
			channel_layout: 0x3, // stereo
			format: 4,           // f32
		}];
	}

	/// Recompute the cached lengths from the track lists (C++
	/// `ViewerOutput::verify_length()`).
	pub fn verify_length(
		&mut self,
		lengths: (
			quercus_core::Rational,
			quercus_core::Rational,
			quercus_core::Rational,
		),
	) {
		let (video, audio, overall) = lengths;
		self.last_length = overall;
		let _ = (video, audio);
	}

	/// Total stream counts.
	pub fn video_stream_count(&self) -> usize {
		self.video_params.len()
	}

	/// Audio stream count.
	pub fn audio_stream_count(&self) -> usize {
		self.audio_params.len()
	}
}

/// Write a rational as the attribute pair `<prefix>_num`/`<prefix>_den`.
fn write_rational_attributes(
	writer: &mut dyn crate::serializer::XmlWrite,
	prefix: &str,
	value: quercus_core::Rational,
) {
	let numerator = format!("{}_num", prefix);
	let denominator = format!("{}_den", prefix);
	writer.attribute(&numerator, &value.numerator().to_string());
	writer.attribute(&denominator, &value.denominator().to_string());
}

/// Read a rational from the attribute pair `<prefix>_num`/`<prefix>_den`
/// (a missing denominator falls back to 1, so the value stays valid).
fn read_rational_attributes(
	reader: &dyn crate::serializer::XmlRead,
	prefix: &str,
) -> quercus_core::Rational {
	let numerator = format!("{}_num", prefix);
	let denominator = format!("{}_den", prefix);
	let num = reader
		.attribute(&numerator)
		.and_then(|v| v.trim().parse::<i64>().ok())
		.unwrap_or(0);
	let den = reader
		.attribute(&denominator)
		.and_then(|v| v.trim().parse::<i64>().ok())
		.unwrap_or(1);
	quercus_core::Rational::new(num, den)
}

/// Read an integer attribute, falling back to `default` when absent or
/// malformed (the load path never fails on a missing field).
fn read_i32_attribute(reader: &dyn crate::serializer::XmlRead, name: &str, default: i32) -> i32 {
	reader
		.attribute(name)
		.and_then(|v| v.trim().parse::<i32>().ok())
		.unwrap_or(default)
}

/// Read an unsigned integer attribute with the same fallback rules.
fn read_u64_attribute(reader: &dyn crate::serializer::XmlRead, name: &str, default: u64) -> u64 {
	reader
		.attribute(name)
		.and_then(|v| v.trim().parse::<u64>().ok())
		.unwrap_or(default)
}

/// Read a boolean attribute written as `1`/`0` (also accepts
/// `true`/`false`); absent means `false`.
fn read_bool_attribute(reader: &dyn crate::serializer::XmlRead, name: &str) -> bool {
	matches!(
		reader.attribute(name).as_deref().map(str::trim),
		Some("1" | "true")
	)
}

impl NodeBehavior for SequenceBehavior {
	fn name(&self) -> &str {
		"Sequence"
	}

	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.sequence"
	}

	fn categories(&self) -> &[Category] {
		&[Category::Output]
	}

	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		// A deep copy must keep the track-list/parameter state, or the
		// sequence reloads with no tracks. The timeline handles are
		// opaque and owned by the source project: the copy starts
		// unbound (the facade creates fresh handles on demand), matching
		// a freshly loaded sequence.
		Some(Box::new(SequenceBehavior {
			track_lists: self.track_lists.clone(),
			markers: crate::handle::CHandle::null(),
			workarea: crate::handle::CHandle::null(),
			last_length: self.last_length,
			autocache_video: self.autocache_video,
			autocache_audio: self.autocache_audio,
			playhead: self.playhead,
			video_params: self.video_params.clone(),
			audio_params: self.audio_params.clone(),
		}))
	}

	/// Custom project save (C++ `Sequence::SaveCustom` writes the
	/// workarea and markers; those live behind opaque quercustimeline
	/// handles in Rust, so only the track-list references and the
	/// behavior-owned parameter streams can be written here).
	///
	/// The parameter streams are persisted as one compact element per
	/// stream (attributes only, so no text escaping is needed):
	///
	/// ```xml
	/// <videoparams width="960" height="540" framerate_num="25"
	///              framerate_den="1" format="4" channels="4" interlaced="0"/>
	/// <audioparams samplerate="48000" layout="3" format="4"/>
	/// ```
	fn save_custom(&self, core: &NodeCore, writer: &mut dyn crate::serializer::XmlWrite) {
		let _ = core;
		if !self.track_lists.is_empty() {
			writer.start_element("tracklists");
			for t in &self.track_lists {
				writer.text_element("tracklist", &t.identity().to_string());
			}
			writer.end_element(); // tracklists
		}
		// Video parameter streams (`ViewerOutput::k_video_params_input`
		// values live in the behavior, not in the node inputs).
		for v in &self.video_params {
			writer.start_element("videoparams");
			writer.attribute("width", &v.width.to_string());
			writer.attribute("height", &v.height.to_string());
			write_rational_attributes(writer, "framerate", v.frame_rate);
			writer.attribute("format", &v.pixel_format.to_string());
			writer.attribute("channels", &v.channels.to_string());
			writer.attribute("interlaced", if v.interlaced { "1" } else { "0" });
			writer.end_element(); // videoparams
		}
		// Audio parameter streams, same convention.
		for a in &self.audio_params {
			writer.start_element("audioparams");
			writer.attribute("samplerate", &a.sample_rate.to_string());
			writer.attribute("layout", &a.channel_layout.to_string());
			writer.attribute("format", &a.format.to_string());
			writer.end_element(); // audioparams
		}
	}

	/// Custom project load (C++ `Sequence::LoadCustom`): the track-list
	/// references are collected here and resolved to live ids by the
	/// serializer's post-load pass; the parameter streams are restored
	/// into the behavior.
	///
	/// Backward compatibility: a file without `<videoparams>` /
	/// `<audioparams>` elements (every pre-existing project, and the C++
	/// writer) leaves the constructor defaults untouched. Unknown
	/// elements are skipped.
	fn load_custom(
		&mut self,
		_core: &mut NodeCore,
		reader: &mut dyn crate::serializer::XmlRead,
	) -> bool {
		let mut video_params: Vec<VideoParams> = Vec::new();
		let mut audio_params: Vec<AudioParams> = Vec::new();
		let mut has_video_params = false;
		let mut has_audio_params = false;
		while reader.next_start_element() {
			match reader.name() {
				"tracklists" => {
					while reader.next_start_element() {
						if reader.name() == "tracklist" {
							if let Some(id) =
								crate::serializer::parse_node_ref(&reader.read_element_text())
							{
								self.track_lists.push(id);
							}
						} else {
							reader.skip_current_element();
						}
					}
				}
				"videoparams" => {
					has_video_params = true;
					video_params.push(VideoParams {
						width: read_i32_attribute(reader, "width", 0),
						height: read_i32_attribute(reader, "height", 0),
						frame_rate: read_rational_attributes(reader, "framerate"),
						pixel_format: read_i32_attribute(reader, "format", 0),
						channels: read_i32_attribute(reader, "channels", 0),
						interlaced: read_bool_attribute(reader, "interlaced"),
					});
					reader.skip_current_element();
				}
				"audioparams" => {
					has_audio_params = true;
					audio_params.push(AudioParams {
						sample_rate: read_i32_attribute(reader, "samplerate", 0),
						channel_layout: read_u64_attribute(reader, "layout", 0),
						format: read_i32_attribute(reader, "format", 0),
					});
					reader.skip_current_element();
				}
				_ => reader.skip_current_element(),
			}
		}
		// Only replace the defaults when the file actually carried the
		// segments: old files keep the constructor's parameters.
		if has_video_params {
			self.video_params = video_params;
		}
		if has_audio_params {
			self.audio_params = audio_params;
		}
		true
	}

	fn as_any(&self) -> Option<&dyn std::any::Any> {
		Some(self)
	}

	fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
		Some(self)
	}
}

impl Default for SequenceBehavior {
	fn default() -> Self {
		SequenceBehavior::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::serializer::{XmlRead, XmlWrite};

	/// `save_custom`/`load_custom` round-trip the parameter streams
	/// through the real XML bridge, and an unknown segment is skipped
	/// without losing the surrounding data.
	#[test]
	fn custom_segment_round_trips_parameter_streams() {
		let mut seq = SequenceBehavior::new();
		seq.video_params = vec![VideoParams {
			width: 960,
			height: 540,
			frame_rate: quercus_core::Rational::new(25, 1),
			pixel_format: 4,
			channels: 4,
			interlaced: false,
		}];
		seq.audio_params = vec![AudioParams {
			sample_rate: 48000,
			channel_layout: 3,
			format: 4,
		}];

		let mut writer = crate::serializer::XmlWriterBridge::new().expect("XML writer");
		writer.start_element("custom");
		seq.save_custom(&NodeCore::new(), &mut writer);
		// A segment written by a newer build that this one does not know.
		writer.start_element("futuresegment");
		writer.attribute("value", "1");
		writer.end_element();
		writer.end_element(); // custom
		let xml = writer.output();
		assert!(
			xml.contains("<videoparams width=\"960\" height=\"540\" framerate_num=\"25\" framerate_den=\"1\" format=\"4\" channels=\"4\" interlaced=\"0\""),
			"compact video params: {xml}"
		);

		let mut reader = crate::serializer::XmlReaderBridge::new(&xml).expect("XML reader");
		assert!(reader.next_start_element(), "positioned on <custom>");
		let mut loaded = SequenceBehavior::new();
		assert!(loaded.load_custom(&mut NodeCore::new(), &mut reader));
		assert_eq!(loaded.video_params, seq.video_params);
		assert_eq!(loaded.audio_params, seq.audio_params);
	}

	/// A legacy `<custom>` carrying only `<tracklists>` leaves the
	/// behavior's parameters untouched (the serializer constructs
	/// sequences through `create()`, whose defaults must survive).
	#[test]
	fn legacy_custom_segment_leaves_parameters_untouched() {
		let xml = "<custom><tracklists><tracklist>7</tracklist></tracklists></custom>";
		let mut reader = crate::serializer::XmlReaderBridge::new(xml).expect("XML reader");
		assert!(reader.next_start_element());
		let mut seq = SequenceBehavior::new();
		seq.video_params = vec![VideoParams {
			width: 1280,
			height: 720,
			frame_rate: quercus_core::Rational::new(30000, 1001),
			pixel_format: 4,
			channels: 4,
			interlaced: true,
		}];
		assert!(seq.load_custom(&mut NodeCore::new(), &mut reader));
		assert_eq!(seq.video_params.len(), 1);
		assert_eq!(seq.video_params[0].width, 1280);
		assert!(seq.video_params[0].interlaced);
	}
}

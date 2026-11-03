//! Subtitle styling C ABI wrappers for editor sessions.

use std::ffi::c_void;
use std::path::Path;

use crate::node::NodeId;

use super::EditorSession;

/// One subtitle style, matching the text3 generator's styling inputs.
///
/// The default values mirror text3: no font-family override, 72 pt, opaque
/// white fill, opaque black outline color and a 2 px outline width. The
/// style has no separate outline-enable flag, so `outline_width > 0` enables
/// the outline when it is applied.
///
/// `margin_h`, `margin_v` and `line_spacing` are accepted for API
/// compatibility but ignored because text3 has no matching inputs. `align`
/// is carried as the 0/1/2 selector from the C ABI; text3 has no horizontal
/// alignment input, so it is mapped to vertical alignment (0 = top,
/// 1 = middle, 2 = bottom).
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
	/// Whether the outline pass is enabled.
	pub outline_enabled: bool,
	/// Outline width in points; `0` draws nothing even when enabled.
	pub outline_width: f32,
	/// Vertical alignment selector (0 = top, 1 = middle, 2 = bottom);
	/// text3 has no horizontal alignment input.
	pub align: i32,
	/// Horizontal margin (ignored by text3).
	pub margin_h: f32,
	/// Vertical margin (ignored by text3).
	pub margin_v: f32,
	/// Line spacing multiplier (ignored by text3).
	pub line_spacing: f32,
}

impl Default for SubtitleStyle {
	fn default() -> Self {
		Self {
			font_family: String::new(),
			font_size: 72.0,
			color: [1.0, 1.0, 1.0, 1.0],
			outline_color: [0.0, 0.0, 0.0, 1.0],
			outline_enabled: true,
			outline_width: 2.0,
			align: 1,
			margin_h: 0.0,
			margin_v: 0.0,
			line_spacing: 1.0,
		}
	}
}

impl SubtitleStyle {
	/// Builds the POD C value, rejecting a font family that does not fit the
	/// 128-byte NUL-terminated field.
	fn to_raw(&self) -> Option<QuercusSubtitleStyle> {
		let bytes = self.font_family.as_bytes();
		if bytes.len() >= 128 || bytes.contains(&0) {
			return None;
		}
		let mut font_family = [0u8; 128];
		font_family[..bytes.len()].copy_from_slice(bytes);
		Some(QuercusSubtitleStyle {
			font_family,
			font_size: self.font_size,
			color: self.color,
			outline_color: self.outline_color,
			outline_enabled: self.outline_enabled,
			outline_width: self.outline_width,
			align: self.align,
			margin_h: self.margin_h,
			margin_v: self.margin_v,
			line_spacing: self.line_spacing,
		})
	}
}

/// POD mirror of the engine's `QuercusSubtitleStyle` C struct.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct QuercusSubtitleStyle {
	font_family: [u8; 128],
	font_size: f32,
	color: [f64; 4],
	outline_color: [f64; 4],
	outline_enabled: bool,
	outline_width: f32,
	align: i32,
	margin_h: f32,
	margin_v: f32,
	line_spacing: f32,
}

#[cfg_attr(
	windows,
	link(name = "quercus_engine.dll.lib", kind = "dylib", modifiers = "+verbatim")
)]
#[cfg_attr(not(windows), link(name = "quercus_engine", kind = "dylib"))]
unsafe extern "C" {
	fn quercus_editor_subtitle_style_default() -> QuercusSubtitleStyle;
	fn quercus_editor_set_subtitle_style(
		session: *const c_void,
		clip: u64,
		style: *const QuercusSubtitleStyle,
	) -> bool;
	fn quercus_editor_import_subtitles_styled(
		session: *const c_void,
		path: *const u8,
		path_len: usize,
		track_index: i32,
		style: *const QuercusSubtitleStyle,
	) -> i32;
}

impl EditorSession {
	/// The engine's default subtitle style (the text3 defaults).
	pub fn default_subtitle_style() -> SubtitleStyle {
		let raw = unsafe { quercus_editor_subtitle_style_default() };
		let end = raw
			.font_family
			.iter()
			.position(|&byte| byte == 0)
			.unwrap_or(raw.font_family.len());
		SubtitleStyle {
			font_family: String::from_utf8_lossy(&raw.font_family[..end]).into_owned(),
			font_size: raw.font_size,
			color: raw.color,
			outline_color: raw.outline_color,
			outline_enabled: raw.outline_enabled,
			outline_width: raw.outline_width,
			align: raw.align,
			margin_h: raw.margin_h,
			margin_v: raw.margin_v,
			line_spacing: raw.line_spacing,
		}
	}

	/// Applies `style` to the Text clip `clip` as one undoable edit.
	/// Returns false for an invalid clip, a non-text clip, invalid style
	/// values, or a font family that does not fit the C field.
	pub fn set_subtitle_style(&self, clip: NodeId, style: &SubtitleStyle) -> bool {
		let Some(raw) = style.to_raw() else {
			return false;
		};
		unsafe { quercus_editor_set_subtitle_style(self.ptr, clip.raw(), &raw) }
	}

	/// Imports an SRT file as styled Text clips on the current sequence and
	/// `track_index`. Returns the cue count.
	pub fn import_subtitles_styled(
		&self,
		path: &Path,
		track_index: usize,
		style: &SubtitleStyle,
	) -> Result<usize, i32> {
		let path = path.to_str().ok_or(-1)?;
		let Some(raw) = style.to_raw() else {
			return Err(-1);
		};
		let count = unsafe {
			quercus_editor_import_subtitles_styled(
				self.ptr,
				path.as_ptr(),
				path.len(),
				track_index as i32,
				&raw,
			)
		};
		(count >= 0).then_some(count as usize).ok_or(count)
	}
}

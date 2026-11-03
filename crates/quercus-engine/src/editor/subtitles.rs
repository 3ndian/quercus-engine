//! SRT import/export ABI for editor sessions.

use super::EditorSessionHandle;
use crate::codec::str_arg;
use crate::editor_commands as ops;
use quercus_node::id::NodeId;

/// One subtitle style, matching the text3 generator's styling inputs.
///
/// `font_family` is a NUL-terminated UTF-8 string of at most 127 bytes
/// (the remaining bytes must be zero). `margin_h`, `margin_v` and
/// `line_spacing` are accepted for API compatibility but are ignored by
/// [`ops::apply_subtitle_style`] because text3 has no matching inputs.
/// `align` is 0/1/2; text3 has no horizontal alignment input, so it is
/// mapped to the vertical alignment combo (0 = top, 1 = middle, 2 = bottom).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct QuercusSubtitleStyle {
	/// NUL-terminated UTF-8 font family (empty selects the default).
	pub font_family: [u8; 128],
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
	/// Horizontal margin (ignored by the text3 application).
	pub margin_h: f32,
	/// Vertical margin (ignored by the text3 application).
	pub margin_v: f32,
	/// Line spacing multiplier (ignored by the text3 application).
	pub line_spacing: f32,
}

impl Default for QuercusSubtitleStyle {
	/// The same values as [`ops::SubtitleStyle::default`]: no font-family
	/// override, 72 pt, opaque white fill, opaque black outline color and a
	/// 2 px outline width.
	fn default() -> Self {
		let native = ops::SubtitleStyle::default();
		let mut font_family = [0u8; 128];
		let bytes = native.font_family.as_bytes();
		let length = bytes.len().min(font_family.len() - 1);
		font_family[..length].copy_from_slice(&bytes[..length]);
		Self {
			font_family,
			font_size: native.font_size,
			color: native.color,
			outline_color: native.outline_color,
			outline_enabled: native.outline_enabled,
			outline_width: native.outline_width,
			align: native.align,
			margin_h: native.margin_h,
			margin_v: native.margin_v,
			line_spacing: native.line_spacing,
		}
	}
}

impl QuercusSubtitleStyle {
	/// Converts the POD style into the engine command style, rejecting a
	/// missing NUL terminator and invalid UTF-8 in `font_family`.
	fn to_native(&self) -> Result<ops::SubtitleStyle, String> {
		let end = self
			.font_family
			.iter()
			.position(|&byte| byte == 0)
			.ok_or_else(|| "subtitle style: font_family is not NUL-terminated".to_string())?;
		let font_family = std::str::from_utf8(&self.font_family[..end])
			.map_err(|_| "subtitle style: font_family is not valid UTF-8".to_string())?
			.to_string();
		Ok(ops::SubtitleStyle {
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

/// Returns the default subtitle style (matching the text3 defaults).
#[unsafe(no_mangle)]
pub extern "C" fn quercus_editor_subtitle_style_default() -> QuercusSubtitleStyle {
	QuercusSubtitleStyle::default()
}

/// Applies `style` to the Text clip `clip` as one undoable "Subtitle Style"
/// edit. Returns false for a null session/style, an invalid clip, a clip
/// whose direct texture source is not a text3 generator, or invalid style
/// values.
///
/// # Safety
/// A non-null `session` must point to a live editor session handle; a
/// non-null `style` must point to a readable `QuercusSubtitleStyle`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_set_subtitle_style(
	session: *const EditorSessionHandle,
	clip: u64,
	style: *const QuercusSubtitleStyle,
) -> bool {
	let (Some(session), Some(style), Some(clip)) = (
		unsafe { session.as_ref() },
		unsafe { style.as_ref() },
		NodeId::from_identity(clip),
	) else {
		return false;
	};
	let Ok(style) = style.to_native() else {
		return false;
	};
	let project = session.project();
	ops::apply_subtitle_style(&project, clip, &style).is_ok()
}

/// Imports an SRT file as Text clips on the current sequence and
/// `track_index`, applying the default subtitle style. Returns the cue
/// count, or -1 on a null/invalid session, path or I/O/parse error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_import_subtitles(
	session: *const EditorSessionHandle,
	path: *const u8,
	path_len: usize,
	track_index: i32,
) -> i32 {
	let (Some(session), Some(path)) = (unsafe { session.as_ref() }, unsafe {
		str_arg(path, path_len)
	}) else {
		return -1;
	};
	if track_index < 0 {
		return -1;
	}
	let (project, sequence) = {
		let state = session.inner.lock().unwrap_or_else(|e| e.into_inner());
		(state.project.clone(), state.current_sequence)
	};
	match ops::import_srt(&project, sequence, track_index as usize, std::path::Path::new(path)) {
		Ok(count) => count as i32,
		Err(_) => -1,
	}
}

/// Imports an SRT file as styled Text clips on the current sequence and
/// `track_index`. A null `style` selects the default style. Returns the cue
/// count, or -1 on a null/invalid session, path, style or I/O/parse error.
///
/// # Safety
/// A non-null `session` must point to a live editor session handle; a
/// non-null `style` must point to a readable `QuercusSubtitleStyle`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_import_subtitles_styled(
	session: *const EditorSessionHandle,
	path: *const u8,
	path_len: usize,
	track_index: i32,
	style: *const QuercusSubtitleStyle,
) -> i32 {
	let (Some(session), Some(path)) = (unsafe { session.as_ref() }, unsafe {
		str_arg(path, path_len)
	}) else {
		return -1;
	};
	if track_index < 0 {
		return -1;
	}
	let style = match unsafe { style.as_ref() } {
		Some(style) => match style.to_native() {
			Ok(style) => style,
			Err(_) => return -1,
		},
		None => ops::SubtitleStyle::default(),
	};
	let (project, sequence) = {
		let state = session.inner.lock().unwrap_or_else(|e| e.into_inner());
		(state.project.clone(), state.current_sequence)
	};
	match ops::import_srt_styled(
		&project,
		sequence,
		track_index as usize,
		std::path::Path::new(path),
		&style,
	) {
		Ok(count) => count as i32,
		Err(_) => -1,
	}
}

/// Exports Text clips on the current sequence's video tracks to an SRT
/// file. Returns the cue count, or -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_export_subtitles(
	session: *const EditorSessionHandle,
	path: *const u8,
	path_len: usize,
) -> i32 {
	let (Some(session), Some(path)) = (unsafe { session.as_ref() }, unsafe {
		str_arg(path, path_len)
	}) else {
		return -1;
	};
	let (project, sequence) = {
		let state = session.inner.lock().unwrap_or_else(|e| e.into_inner());
		(state.project.clone(), state.current_sequence)
	};
	match ops::export_srt(&project, sequence, std::path::Path::new(path)) {
		Ok(count) => count as i32,
		Err(_) => -1,
	}
}

/// Sets a clip's audio gain (0..=4, 1 = unity) as one undo entry.
/// Returns false on a null/invalid session, clip or gain.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_set_clip_gain(
	session: *const EditorSessionHandle,
	clip: u64,
	gain: f64,
) -> bool {
	let (Some(session), Some(clip)) =
		(unsafe { session.as_ref() }, NodeId::from_identity(clip))
	else {
		return false;
	};
	let project = session.project();
	crate::editor_commands::set_clip_gain(&project, clip, gain).is_ok()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn pod_style_default_matches_text3_and_converts() {
		let raw = quercus_editor_subtitle_style_default();
		assert_eq!(raw.font_family[0], 0);
		assert_eq!(raw.font_size, 72.0);
		assert_eq!(raw.color, [1.0, 1.0, 1.0, 1.0]);
		assert_eq!(raw.outline_color, [0.0, 0.0, 0.0, 1.0]);
		assert_eq!(raw.outline_width, 2.0);
		assert_eq!(raw.align, 1);
		assert_eq!(raw.line_spacing, 1.0);
		let native = raw.to_native().expect("default style converts");
		assert_eq!(native, ops::SubtitleStyle::default());
	}

	#[test]
	fn pod_style_rejects_unterminated_and_invalid_font_family() {
		let mut raw = QuercusSubtitleStyle::default();
		raw.font_family = [b'x'; 128];
		assert!(raw.to_native().is_err(), "must require a NUL terminator");

		let mut raw = QuercusSubtitleStyle::default();
		raw.font_family[..2].copy_from_slice(&[0xFF, 0xFE]);
		assert!(raw.to_native().is_err(), "must require valid UTF-8");

		let mut raw = QuercusSubtitleStyle::default();
		raw.font_family[..4].copy_from_slice(b"Sans");
		let native = raw.to_native().expect("valid family");
		assert_eq!(native.font_family, "Sans");
	}

	#[test]
	fn style_abi_null_arguments_are_rejected() {
		assert!(!unsafe {
			quercus_editor_set_subtitle_style(std::ptr::null(), 0, std::ptr::null())
		});
		assert_eq!(
			unsafe {
				quercus_editor_import_subtitles_styled(
					std::ptr::null(),
					std::ptr::null(),
					0,
					0,
					std::ptr::null(),
				)
			},
			-1
		);
	}
}
